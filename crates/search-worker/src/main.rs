//! Isolated regex worker.
//!
//! stdout carries only length-prefixed JSON protocol frames. Diagnostics go to
//! stderr. The parent process owns snapshot files, cancellation and the job
//! object; this process only matches UTF-8 snapshots and writes a private
//! replacement artifact.

use std::io::Write;
use std::time::{Duration, Instant};

use notepust_contracts::{
    ErrorCategory, FinishReason, JobId, MatchBatch, PROTOCOL_VERSION, Progress, RegexEngine,
    ReplacementPiece, ReplacementProgram, SearchMatch, SearchPurpose, SearchRequest, SearchStats,
    SnapshotId, WorkerRequest, WorkerResponse, decode_frame, encode_frame, read_frame, write_frame,
};
use regex::bytes::RegexBuilder as StandardRegexBuilder;

const MAX_SNAPSHOT_BYTES: u64 = 384 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CAPTURE_GROUPS: usize = 16;
const BATCH_MATCHES: usize = 500;
const BATCH_BYTES: usize = 512 * 1024;

#[derive(Debug)]
struct WorkerFailure {
    category: ErrorCategory,
    detail: String,
}

impl WorkerFailure {
    fn invalid(detail: impl Into<String>) -> Self {
        Self {
            category: ErrorCategory::InvalidRequest,
            detail: detail.into(),
        }
    }

    fn io(error: std::io::Error) -> Self {
        Self {
            category: ErrorCategory::Io,
            detail: error.to_string(),
        }
    }

    fn resource(detail: impl Into<String>) -> Self {
        Self {
            category: ErrorCategory::ResourceLimit,
            detail: detail.into(),
        }
    }

    fn protocol(detail: impl Into<String>) -> Self {
        Self {
            category: ErrorCategory::Protocol,
            detail: detail.into(),
        }
    }
}

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();

    if let Err(error) = run() {
        eprintln!("search-worker: {}", error.detail);
        std::process::exit(1);
    }
}

fn run() -> Result<(), WorkerFailure> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();

    let hello = read_frame(&mut stdin)
        .map_err(|error| WorkerFailure::protocol(error.to_string()))?
        .ok_or_else(|| WorkerFailure::protocol("parent closed before Hello"))?;
    let request: WorkerRequest =
        decode_frame(&hello).map_err(|error| WorkerFailure::protocol(error.to_string()))?;
    match request {
        WorkerRequest::Hello {
            protocol_version, ..
        } => {
            if protocol_version != PROTOCOL_VERSION {
                send_failed(
                    &mut stdout,
                    None,
                    WorkerFailure::protocol(format!(
                        "protocol {protocol_version} is not supported; expected {PROTOCOL_VERSION}"
                    )),
                )?;
                return Ok(());
            }
        }
        WorkerRequest::Shutdown => return Ok(()),
        _ => {
            send_failed(
                &mut stdout,
                None,
                WorkerFailure::protocol("expected Hello as the first frame"),
            )?;
            return Ok(());
        }
    }

    write_frame(
        &mut stdout,
        &WorkerResponse::Ready {
            protocol_version: PROTOCOL_VERSION,
            build_id: format!("{}-{}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH),
            engines: vec![RegexEngine::Standard, RegexEngine::Advanced],
        },
    )
    .map_err(|error| WorkerFailure::protocol(error.to_string()))?;

    loop {
        let frame = match read_frame(&mut stdin) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(error) => {
                send_failed(
                    &mut stdout,
                    None,
                    WorkerFailure::protocol(error.to_string()),
                )?;
                return Ok(());
            }
        };
        let request: WorkerRequest =
            decode_frame(&frame).map_err(|error| WorkerFailure::protocol(error.to_string()))?;
        match request {
            WorkerRequest::Hello { .. } => {
                send_failed(
                    &mut stdout,
                    None,
                    WorkerFailure::protocol("duplicate Hello"),
                )?;
            }
            WorkerRequest::Shutdown => return Ok(()),
            WorkerRequest::Search(search) => {
                let job_id = search.job_id;
                if let Err(error) = handle_search(search, &mut stdout) {
                    send_failed(&mut stdout, Some(job_id), error)?;
                }
            }
        }
    }
}

fn send_failed<W: Write>(
    writer: &mut W,
    job_id: Option<JobId>,
    failure: WorkerFailure,
) -> Result<(), WorkerFailure> {
    write_frame(
        writer,
        &WorkerResponse::Failed {
            job_id,
            category: failure.category,
            detail: failure.detail,
        },
    )
    .map_err(|error| WorkerFailure::protocol(error.to_string()))
}

fn handle_search<W: Write>(request: SearchRequest, stdout: &mut W) -> Result<(), WorkerFailure> {
    if request.engine == RegexEngine::Advanced
        && request.replacement.is_some()
        && request.purpose == SearchPurpose::Find
    {
        // A Find request never carries a replacement; reject contradictory
        // requests instead of guessing.
        return Err(WorkerFailure::invalid(
            "Find request must not include a replacement program",
        ));
    }

    let snapshot = read_snapshot(&request)?;
    let text = std::str::from_utf8(&snapshot)
        .map_err(|error| WorkerFailure::invalid(format!("snapshot is not UTF-8: {error}")))?;

    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.clamp(1, 30_000));

    match request.purpose {
        SearchPurpose::Find => run_find(&request, text, deadline, stdout),
        SearchPurpose::ReplacePlan => run_replacement(&request, text, deadline, stdout),
    }
}

fn read_snapshot(request: &SearchRequest) -> Result<Vec<u8>, WorkerFailure> {
    let path = request.path.to_path_buf();
    let metadata = std::fs::metadata(&path).map_err(WorkerFailure::io)?;
    if metadata.len() != request.utf8_length {
        return Err(WorkerFailure::invalid(format!(
            "snapshot length changed: expected {}, found {}",
            request.utf8_length,
            metadata.len()
        )));
    }
    if metadata.len() > MAX_SNAPSHOT_BYTES {
        return Err(WorkerFailure::resource(format!(
            "snapshot is {} bytes; worker limit is {MAX_SNAPSHOT_BYTES}",
            metadata.len()
        )));
    }
    std::fs::read(&path).map_err(WorkerFailure::io)
}

enum Engine {
    Standard(regex::bytes::Regex),
    Advanced(pcre2::bytes::Regex),
}

impl Engine {
    fn captures_len(&self) -> usize {
        match self {
            Self::Standard(regex) => regex.captures_len(),
            Self::Advanced(regex) => regex.captures_len(),
        }
    }

    fn capture_names(&self) -> Vec<String> {
        match self {
            Self::Standard(regex) => regex
                .capture_names()
                .flatten()
                .map(ToString::to_string)
                .collect(),
            Self::Advanced(regex) => regex.capture_names().iter().flatten().cloned().collect(),
        }
    }
}

fn compile_engine(request: &SearchRequest) -> Result<Engine, WorkerFailure> {
    if request.pattern.len() > 64 * 1024 {
        return Err(WorkerFailure::resource(format!(
            "pattern is {} bytes; limit is 65536",
            request.pattern.len()
        )));
    }
    match request.engine {
        RegexEngine::Standard => {
            let mut builder = StandardRegexBuilder::new(&request.pattern);
            builder
                .case_insensitive(!request.options.case_sensitive)
                .dot_matches_new_line(request.options.dot_all)
                .multi_line(request.options.multi_line)
                .crlf(request.options.crlf)
                .unicode(true)
                .size_limit(8 * 1024 * 1024)
                .dfa_size_limit(8 * 1024 * 1024)
                .nest_limit(128);
            let regex = builder
                .build()
                .map_err(|error| WorkerFailure::invalid(error.to_string()))?;
            Ok(Engine::Standard(regex))
        }
        RegexEngine::Advanced => {
            let mut builder = pcre2::bytes::RegexBuilder::new();
            builder
                .caseless(!request.options.case_sensitive)
                .dotall(request.options.dot_all)
                .multi_line(request.options.multi_line)
                .crlf(request.options.crlf)
                .ucp(true)
                .utf(true)
                .jit_if_available(false);
            let regex = builder
                .build(&request.pattern)
                .map_err(|error| WorkerFailure::invalid(error.to_string()))?;
            Ok(Engine::Advanced(regex))
        }
    }
}

#[derive(Clone, Debug)]
struct FoundMatch {
    start: usize,
    end: usize,
    captures: Vec<Option<(usize, usize)>>,
    named: Vec<(String, Option<(usize, usize)>)>,
}

fn run_find<W: Write>(
    request: &SearchRequest,
    text: &str,
    deadline: Instant,
    stdout: &mut W,
) -> Result<(), WorkerFailure> {
    let engine = compile_engine(request)?;
    let limit = if request.result_limit == 0 {
        10_000
    } else {
        request.result_limit
    };
    let mut sender = BatchSender::new(stdout, request.job_id, request.snapshot_id);
    let mut found = 0u64;
    let mut last_scanned = 0u64;
    let mut truncated = false;
    let started = Instant::now();

    {
        let mut visit = |matched: FoundMatch| -> Result<bool, WorkerFailure> {
            if Instant::now() >= deadline {
                return Err(WorkerFailure::resource("search deadline exceeded"));
            }
            last_scanned = matched.end as u64;
            if !whole_word_ok(request, text, matched.start, matched.end) {
                return Ok(true);
            }
            sender.push(to_contract_match(matched), last_scanned)?;
            found += 1;
            if found.is_multiple_of(500) {
                sender.progress(found, last_scanned, Some(text.len() as u64))?;
            }
            if limit != 0 && found >= limit as u64 {
                truncated = true;
                return Ok(false);
            }
            Ok(true)
        };

        match &engine {
            Engine::Standard(regex) => {
                for found_match in regex.find_iter(text.as_bytes()) {
                    let matched = found_match;
                    if !visit(FoundMatch {
                        start: matched.start(),
                        end: matched.end(),
                        captures: Vec::new(),
                        named: Vec::new(),
                    })? {
                        break;
                    }
                }
            }
            Engine::Advanced(regex) => {
                for result in regex.find_iter(text.as_bytes()) {
                    let matched =
                        result.map_err(|error| WorkerFailure::invalid(error.to_string()))?;
                    if !visit(FoundMatch {
                        start: matched.start(),
                        end: matched.end(),
                        captures: Vec::new(),
                        named: Vec::new(),
                    })? {
                        break;
                    }
                }
            }
        }
    }

    sender.finish()?;
    drop(sender);
    let stats = SearchStats {
        bytes_scanned: text.len() as u64,
        matches_found: found,
        elapsed_ms: started.elapsed().as_millis() as u64,
        truncated,
    };
    write_frame(
        stdout,
        &WorkerResponse::Finished {
            job_id: request.job_id,
            reason: if truncated {
                FinishReason::Truncated
            } else {
                FinishReason::Complete
            },
            stats,
        },
    )
    .map_err(|error| WorkerFailure::protocol(error.to_string()))?;
    Ok(())
}

fn run_replacement<W: Write>(
    request: &SearchRequest,
    text: &str,
    deadline: Instant,
    stdout: &mut W,
) -> Result<(), WorkerFailure> {
    let program = request
        .replacement
        .as_ref()
        .ok_or_else(|| WorkerFailure::invalid("ReplacePlan requires a replacement program"))?;
    let output_path = request
        .output_artifact_path
        .as_ref()
        .ok_or_else(|| WorkerFailure::invalid("ReplacePlan requires an artifact path"))?;
    let engine = compile_engine(request)?;
    validate_program(program, &engine)?;

    let path = output_path.to_path_buf();
    let file = std::fs::File::create(&path).map_err(WorkerFailure::io)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut written_bytes = 0u64;
    let mut cursor = 0usize;
    let mut count = 0u64;
    let mut sender = BatchSender::new(stdout, request.job_id, request.snapshot_id);
    let preview_limit = if request.result_limit == 0 {
        200
    } else {
        request.result_limit
    };
    let started = Instant::now();

    {
        let mut emit = |matched: FoundMatch| -> Result<bool, WorkerFailure> {
            if Instant::now() >= deadline {
                return Err(WorkerFailure::resource("replacement deadline exceeded"));
            }
            if !whole_word_ok(request, text, matched.start, matched.end) {
                return Ok(true);
            }
            let unchanged = &text[cursor..matched.start];
            write_hashed(&mut writer, &mut hasher, unchanged.as_bytes())?;
            written_bytes += unchanged.len() as u64;
            let replacement = expand_program(program, text, &matched);
            write_hashed(&mut writer, &mut hasher, replacement.as_bytes())?;
            written_bytes += replacement.len() as u64;
            if written_bytes > MAX_ARTIFACT_BYTES {
                return Err(WorkerFailure::resource(format!(
                    "replacement artifact exceeds {MAX_ARTIFACT_BYTES} bytes"
                )));
            }
            cursor = matched.end;
            if count < preview_limit as u64 {
                sender.push(to_contract_match(matched.clone()), cursor as u64)?;
            }
            count += 1;
            if count.is_multiple_of(500) {
                sender.progress(count, cursor as u64, Some(text.len() as u64))?;
            }
            Ok(true)
        };

        match &engine {
            Engine::Standard(regex) => {
                for captures in regex.captures_iter(text.as_bytes()) {
                    let Some(whole) = captures.get(0) else {
                        continue;
                    };
                    let mut ranges = Vec::new();
                    for index in 0..regex.captures_len().min(MAX_CAPTURE_GROUPS) {
                        ranges.push(captures.get(index).map(|m| (m.start(), m.end())));
                    }
                    let named = regex
                        .capture_names()
                        .flatten()
                        .filter_map(|name| {
                            captures
                                .name(name)
                                .map(|m| (name.to_string(), Some((m.start(), m.end()))))
                        })
                        .collect();
                    emit(FoundMatch {
                        start: whole.start(),
                        end: whole.end(),
                        captures: ranges,
                        named,
                    })?;
                }
            }
            Engine::Advanced(regex) => {
                for captures in regex.captures_iter(text.as_bytes()) {
                    let captures =
                        captures.map_err(|error| WorkerFailure::invalid(error.to_string()))?;
                    let Some(whole) = captures.get(0) else {
                        continue;
                    };
                    let mut ranges = Vec::new();
                    for index in 0..regex.captures_len().min(MAX_CAPTURE_GROUPS) {
                        ranges.push(captures.get(index).map(|m| (m.start(), m.end())));
                    }
                    let named = regex
                        .capture_names()
                        .iter()
                        .flatten()
                        .filter_map(|name| {
                            captures
                                .name(name)
                                .map(|m| (name.to_string(), Some((m.start(), m.end()))))
                        })
                        .collect();
                    emit(FoundMatch {
                        start: whole.start(),
                        end: whole.end(),
                        captures: ranges,
                        named,
                    })?;
                }
            }
        }
    }

    let tail = &text[cursor..];
    write_hashed(&mut writer, &mut hasher, tail.as_bytes())?;
    written_bytes += tail.len() as u64;
    if written_bytes > MAX_ARTIFACT_BYTES {
        return Err(WorkerFailure::resource(format!(
            "replacement artifact exceeds {MAX_ARTIFACT_BYTES} bytes"
        )));
    }
    writer.flush().map_err(WorkerFailure::io)?;
    writer.get_ref().sync_all().map_err(WorkerFailure::io)?;
    sender.finish()?;
    drop(sender);

    write_frame(
        stdout,
        &WorkerResponse::ReplacementReady(notepust_contracts::ReplacementReady {
            job_id: request.job_id,
            snapshot_id: request.snapshot_id,
            artifact_path: output_path.clone(),
            utf8_length: written_bytes,
            digest: *hasher.finalize().as_bytes(),
            match_count: count,
            complete: true,
        }),
    )
    .map_err(|error| WorkerFailure::protocol(error.to_string()))?;

    write_frame(
        stdout,
        &WorkerResponse::Finished {
            job_id: request.job_id,
            reason: FinishReason::Complete,
            stats: SearchStats {
                bytes_scanned: text.len() as u64,
                matches_found: count,
                elapsed_ms: started.elapsed().as_millis() as u64,
                truncated: count > preview_limit as u64,
            },
        },
    )
    .map_err(|error| WorkerFailure::protocol(error.to_string()))?;
    Ok(())
}

fn write_hashed<W: Write>(
    writer: &mut W,
    hasher: &mut blake3::Hasher,
    bytes: &[u8],
) -> Result<(), WorkerFailure> {
    writer.write_all(bytes).map_err(WorkerFailure::io)?;
    hasher.update(bytes);
    Ok(())
}

fn to_contract_match(matched: FoundMatch) -> SearchMatch {
    SearchMatch {
        start_utf8: matched.start as u64,
        end_utf8: matched.end as u64,
        line: None,
        captures: matched
            .captures
            .into_iter()
            .map(|range| {
                range.map(|(start, end)| notepust_contracts::Utf8Range {
                    start: start as u64,
                    end: end as u64,
                })
            })
            .collect(),
    }
}

fn validate_program(program: &ReplacementProgram, engine: &Engine) -> Result<(), WorkerFailure> {
    let groups = engine.captures_len();
    let names = engine.capture_names();
    for piece in &program.pieces {
        match piece {
            ReplacementPiece::Group { index } => {
                if *index as usize >= groups {
                    return Err(WorkerFailure::invalid(format!(
                        "replacement references group ${index}, but the pattern has {groups} groups"
                    )));
                }
            }
            ReplacementPiece::NamedGroup { name }
                if !names.iter().any(|candidate| candidate == name) =>
            {
                return Err(WorkerFailure::invalid(format!(
                    "replacement references unknown group {name:?}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn expand_program(program: &ReplacementProgram, text: &str, matched: &FoundMatch) -> String {
    let mut output = String::new();
    for piece in &program.pieces {
        match piece {
            ReplacementPiece::Literal { text } => output.push_str(text),
            ReplacementPiece::Match => output.push_str(&text[matched.start..matched.end]),
            ReplacementPiece::Group { index } => {
                if let Some(Some((start, end))) = matched.captures.get(*index as usize) {
                    output.push_str(&text[*start..*end]);
                }
            }
            ReplacementPiece::NamedGroup { name } => {
                if let Some((_, Some((start, end)))) = matched
                    .named
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                {
                    output.push_str(&text[*start..*end]);
                }
            }
            ReplacementPiece::Dollar => output.push('$'),
        }
    }
    output
}

fn whole_word_ok(request: &SearchRequest, text: &str, start: usize, end: usize) -> bool {
    if !request.options.whole_word {
        return true;
    }
    let left_ok = text[..start]
        .chars()
        .next_back()
        .map(|character| !(character.is_alphanumeric() || character == '_'))
        .unwrap_or(true);
    let right_ok = text[end..]
        .chars()
        .next()
        .map(|character| !(character.is_alphanumeric() || character == '_'))
        .unwrap_or(true);
    left_ok && right_ok
}

struct BatchSender<'a, W: Write> {
    stdout: &'a mut W,
    job_id: JobId,
    snapshot_id: SnapshotId,
    sequence: u32,
    pending: Vec<SearchMatch>,
    bytes_scanned: u64,
}

impl<'a, W: Write> BatchSender<'a, W> {
    fn new(stdout: &'a mut W, job_id: JobId, snapshot_id: SnapshotId) -> Self {
        Self {
            stdout,
            job_id,
            snapshot_id,
            sequence: 0,
            pending: Vec::new(),
            bytes_scanned: 0,
        }
    }

    fn push(&mut self, matched: SearchMatch, scanned: u64) -> Result<(), WorkerFailure> {
        self.bytes_scanned = scanned;
        self.pending.push(matched);
        if self.pending.len() >= BATCH_MATCHES {
            self.flush()?;
        }
        Ok(())
    }

    fn progress(
        &mut self,
        matches_found: u64,
        bytes_scanned: u64,
        total_bytes: Option<u64>,
    ) -> Result<(), WorkerFailure> {
        write_frame(
            self.stdout,
            &WorkerResponse::Progress(Progress {
                job_id: self.job_id,
                bytes_scanned,
                matches_found,
                total_bytes,
            }),
        )
        .map_err(|error| WorkerFailure::protocol(error.to_string()))
    }

    fn flush(&mut self) -> Result<(), WorkerFailure> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let matches = std::mem::take(&mut self.pending);
        let mut start = 0usize;
        while start < matches.len() {
            let mut end = matches.len();
            while end > start + 1 {
                let candidate = MatchBatch {
                    job_id: self.job_id,
                    snapshot_id: self.snapshot_id,
                    sequence: self.sequence,
                    matches: matches[start..end].to_vec(),
                    bytes_scanned: self.bytes_scanned,
                };
                let size = encode_frame(&candidate)
                    .map(|frame| frame.len())
                    .unwrap_or(usize::MAX);
                if size <= BATCH_BYTES {
                    break;
                }
                end = start + (end - start).div_ceil(2);
            }
            let batch = MatchBatch {
                job_id: self.job_id,
                snapshot_id: self.snapshot_id,
                sequence: self.sequence,
                matches: matches[start..end].to_vec(),
                bytes_scanned: self.bytes_scanned,
            };
            write_frame(self.stdout, &WorkerResponse::MatchBatch(batch))
                .map_err(|error| WorkerFailure::protocol(error.to_string()))?;
            self.sequence = self.sequence.wrapping_add(1);
            start = end;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), WorkerFailure> {
        self.flush()
    }
}
