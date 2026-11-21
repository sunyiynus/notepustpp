use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use notepust_contracts::{BomPolicy, EncodingId, EncodingSpec, FileIdentity, RawByteRange};
use notepust_platform::read_file_stamp;
use notepust_text_codec::{
    CodecError, EolKind, decode_payload, decode_with_spans, detect_candidate, sniff_bom,
};

use crate::index::LineIndex;
use crate::read_at::FileReader;
use crate::scan::{RawLine, scan_raw_lines};

pub const INDEX_CHUNK_BYTES: usize = 1024 * 1024;
pub const MAX_VIEW_LINE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("encoding error: {0}")]
    Codec(#[from] CodecError),

    #[error("invalid search pattern: {detail}")]
    InvalidPattern { detail: String },

    #[error("the file appears to be binary")]
    Binary,

    #[error("encoding {encoding} cannot be randomly accessed")]
    UnsupportedEncoding { encoding: EncodingId },

    #[error("the requested line is not indexed yet ({scanned_lines} scanned)")]
    NeedsIndex {
        scanned_lines: u64,
        scanned_bytes: u64,
        observed_len: u64,
    },

    #[error("requested line {line} does not exist (file has {lines} lines)")]
    LineOutOfRange { line: u64, lines: u64 },

    #[error("operation cancelled")]
    Cancelled,
}

/// What happened to the file since the last observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogEvent {
    Unchanged,
    Appended { old_len: u64, new_len: u64 },
    Truncated { old_len: u64, new_len: u64 },
    Replaced,
    RewrittenSameSize,
    Removed,
}

#[derive(Clone, Debug)]
pub struct IndexProgress {
    pub generation: u64,
    pub lines_found: u64,
    pub scanned_bytes: u64,
    pub observed_len: u64,
    pub complete: bool,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub line: Option<u64>,
    pub raw_start: u64,
    pub raw_end: u64,
    pub raw_content_len: u64,
    pub text: String,
    pub truncated: bool,
    pub terminator: Option<EolKind>,
}

impl LogLine {
    pub fn range(&self) -> RawByteRange {
        RawByteRange {
            start: self.raw_start,
            end: self.raw_end,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Viewport {
    pub generation: u64,
    pub requested_line: u64,
    pub start_line: u64,
    pub lines: Vec<LogLine>,
    pub total_lines: Option<u64>,
    pub index_complete: bool,
    pub scanned_lines: u64,
    pub observed_len: u64,
}

#[derive(Clone, Debug)]
pub struct TailView {
    pub generation: u64,
    pub lines: Vec<LogLine>,
    pub total_lines: Option<u64>,
    pub index_complete: bool,
    pub observed_len: u64,
    pub truncated_search: bool,
}

pub struct LogReader {
    path: PathBuf,
    reader: FileReader,
    spec: EncodingSpec,
    generation: u64,
    index: LineIndex,
    observed_len: u64,
    identity: Option<FileIdentity>,
    last_write_time: u64,
}

impl LogReader {
    /// Opens a large file for bounded, read-only access.
    ///
    /// Only the first 64 KiB are read to detect the encoding; the full file is
    /// never loaded or decoded just to show the first screen.
    pub fn open(path: &Path, explicit: Option<EncodingSpec>) -> Result<Self, LogError> {
        let stamp = read_file_stamp(path)?;
        let observed_len = stamp.stamp.byte_len;
        let reader = FileReader::open(path)?;

        let head_len = observed_len.min(64 * 1024) as usize;
        let mut head = vec![0u8; head_len];
        let read = reader.read_exact_at(0, &mut head)?;
        head.truncate(read);

        let spec = match explicit {
            Some(spec) => spec,
            None => detect_log_encoding(&head, observed_len)?,
        };
        if !spec.encoding.supports_random_access() {
            return Err(LogError::UnsupportedEncoding {
                encoding: spec.encoding,
            });
        }
        if spec.encoding != EncodingId::Utf16Le && spec.encoding != EncodingId::Utf16Be {
            let sample = &head[..head.len().min(8192)];
            if sample.contains(&0) {
                return Err(LogError::Binary);
            }
        }

        let index = LineIndex::new(spec.clone(), 0, observed_len);
        Ok(Self {
            path: path.to_path_buf(),
            reader,
            spec,
            generation: 0,
            index,
            observed_len,
            identity: stamp.stamp.identity,
            last_write_time: stamp.stamp.last_write_time,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn spec(&self) -> &EncodingSpec {
        &self.spec
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn observed_len(&self) -> u64 {
        self.observed_len
    }

    pub fn index(&self) -> &LineIndex {
        &self.index
    }

    pub fn total_lines(&self) -> Option<u64> {
        self.index.total_lines()
    }

    pub fn progress(&self) -> IndexProgress {
        IndexProgress {
            generation: self.generation,
            lines_found: self.index.lines_found(),
            scanned_bytes: self.index.scanned_raw(),
            observed_len: self.observed_len,
            complete: self.index.is_complete(),
        }
    }

    /// Advances the sparse index by at most one chunk.
    pub fn scan_step(&mut self) -> Result<IndexProgress, LogError> {
        let _ = self.index.scan_next(&self.reader, INDEX_CHUNK_BYTES)?;
        Ok(self.progress())
    }

    /// Scans until `line` is indexed or the file ends.
    pub fn ensure_line_indexed(
        &mut self,
        line: u64,
        cancel: &AtomicBool,
    ) -> Result<IndexProgress, LogError> {
        while self.index.lines_found() <= line && !self.index.is_complete() {
            if cancel.load(Ordering::Relaxed) {
                return Err(LogError::Cancelled);
            }
            let (read, _, eof) = self.index.scan_next(&self.reader, INDEX_CHUNK_BYTES)?;
            if read == 0 && !eof {
                break;
            }
        }
        Ok(self.progress())
    }

    /// Returns a viewport without blocking on a missing index.
    pub fn try_viewport(
        &self,
        line: u64,
        count: usize,
        max_line_bytes: usize,
    ) -> Result<Viewport, LogError> {
        if !self.index.is_complete() && self.index.lines_found() <= line {
            return Err(LogError::NeedsIndex {
                scanned_lines: self.index.lines_found(),
                scanned_bytes: self.index.scanned_raw(),
                observed_len: self.observed_len,
            });
        }
        self.viewport(line, count, max_line_bytes)
    }

    /// Reads a viewport from the indexed area.
    pub fn viewport(
        &self,
        line: u64,
        count: usize,
        max_line_bytes: usize,
    ) -> Result<Viewport, LogError> {
        let total = self.index.total_lines();
        if let Some(total) = total
            && line >= total
        {
            return Err(LogError::LineOutOfRange { line, lines: total });
        }
        if !self.index.is_complete() && line >= self.index.lines_found() {
            return Err(LogError::NeedsIndex {
                scanned_lines: self.index.lines_found(),
                scanned_bytes: self.index.scanned_raw(),
                observed_len: self.observed_len,
            });
        }

        let raw_start = self.locate_line_start(line)?;
        let raw_lines = self.collect_lines(raw_start, line, count)?;
        let mut lines = Vec::with_capacity(raw_lines.len());
        for raw_line in raw_lines {
            lines.push(self.decode_line(raw_line, max_line_bytes)?);
        }

        Ok(Viewport {
            generation: self.generation,
            requested_line: line,
            start_line: line,
            lines,
            total_lines: total,
            index_complete: self.index.is_complete(),
            scanned_lines: self.index.lines_found(),
            observed_len: self.observed_len,
        })
    }

    /// Reads the last `count` lines without requiring a complete index.
    pub fn tail(&self, count: usize, max_line_bytes: usize) -> Result<TailView, LogError> {
        let bom = self.spec.bom.len() as u64;
        let mut window = 256 * 1024u64;
        let mut chosen: Option<(u64, Vec<RawLine>)> = None;

        while window <= 64 * 1024 * 1024 || chosen.is_none() {
            let start = self.observed_len.saturating_sub(window).max(bom);
            let start = self.align_to_line_boundary(start, bom)?;
            let mut buffer = vec![0u8; (self.observed_len - start) as usize];
            let read = self.reader.read_exact_at(start, &mut buffer)?;
            buffer.truncate(read);
            let scan = scan_raw_lines(
                &buffer,
                start,
                self.spec.encoding,
                self.index.lines_found(),
                true,
            );
            let mut available = scan.lines;
            // A trailing terminator produces an empty final logical line. It
            // stays in the line index, but "last N lines" should show the
            // last N lines with content.
            if available
                .last()
                .map(|line| line.content_end == self.observed_len && line.content_len() == 0)
                .unwrap_or(false)
            {
                available.pop();
            }
            if available.len() >= count || start == bom {
                chosen = Some((start, available));
                break;
            }
            if start == bom {
                chosen = Some((start, available));
                break;
            }
            window = window.saturating_mul(4);
        }

        let (start, raw_lines) = chosen.unwrap_or((bom, Vec::new()));
        let skip = raw_lines.len().saturating_sub(count);
        let selected = raw_lines.into_iter().skip(skip).collect::<Vec<_>>();
        let total = self.index.total_lines();
        let absolute_base = if self.index.is_complete() {
            total.map(|total| total.saturating_sub(selected.len() as u64))
        } else {
            None
        };

        let mut lines = Vec::with_capacity(selected.len());
        for (index, raw_line) in selected.into_iter().enumerate() {
            let mut line = self.decode_line(raw_line, max_line_bytes)?;
            line.line = absolute_base.map(|base| base + index as u64);
            lines.push(line);
        }

        Ok(TailView {
            generation: self.generation,
            lines,
            total_lines: total,
            index_complete: self.index.is_complete(),
            observed_len: self.observed_len,
            truncated_search: start != bom,
        })
    }

    /// Polls metadata and reports how the log source changed.
    ///
    /// Appended bytes keep the existing index; truncation, replacement or a
    /// same-size rewrite starts a new generation.
    pub fn poll(&mut self) -> Result<LogEvent, LogError> {
        let stamp = match read_file_stamp(&self.path) {
            Ok(stamp) => stamp,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LogEvent::Removed);
            }
            Err(error) => return Err(LogError::Io(error)),
        };

        if stamp.stamp.identity != self.identity {
            self.reset_after_change(
                stamp.stamp.byte_len,
                stamp.stamp.identity,
                stamp.stamp.last_write_time,
            )?;
            return Ok(LogEvent::Replaced);
        }
        if stamp.stamp.byte_len < self.observed_len {
            let old_len = self.observed_len;
            self.reset_after_change(
                stamp.stamp.byte_len,
                stamp.stamp.identity,
                stamp.stamp.last_write_time,
            )?;
            return Ok(LogEvent::Truncated {
                old_len,
                new_len: stamp.stamp.byte_len,
            });
        }
        if stamp.stamp.byte_len > self.observed_len {
            let old_len = self.observed_len;
            self.observed_len = stamp.stamp.byte_len;
            self.last_write_time = stamp.stamp.last_write_time;
            return Ok(LogEvent::Appended {
                old_len,
                new_len: self.observed_len,
            });
        }
        if stamp.stamp.last_write_time != self.last_write_time {
            self.reset_after_change(
                stamp.stamp.byte_len,
                stamp.stamp.identity,
                stamp.stamp.last_write_time,
            )?;
            return Ok(LogEvent::RewrittenSameSize);
        }
        Ok(LogEvent::Unchanged)
    }

    fn reset_after_change(
        &mut self,
        len: u64,
        identity: Option<FileIdentity>,
        write_time: u64,
    ) -> Result<(), LogError> {
        self.generation = self.generation.saturating_add(1);
        self.reader = FileReader::open(&self.path)?;
        self.observed_len = len;
        self.identity = identity;
        self.last_write_time = write_time;
        self.index = LineIndex::new(self.spec.clone(), self.generation, len);
        Ok(())
    }

    fn align_to_line_boundary(&self, offset: u64, bom: u64) -> Result<u64, LogError> {
        let mut start = offset.max(bom);
        if matches!(
            self.spec.encoding,
            EncodingId::Utf16Le | EncodingId::Utf16Be
        ) {
            start = bom + ((start - bom) / 2) * 2;
        }
        if start <= bom {
            return Ok(bom);
        }

        // Read a small prefix and start after the first line break so the
        // first emitted line is complete.
        let mut buffer = vec![0u8; 8192];
        let read = self.reader.read_exact_at(start, &mut buffer)?;
        buffer.truncate(read);
        if read == 0 {
            return Ok(start);
        }
        let scan = scan_raw_lines(&buffer, start, self.spec.encoding, 0, false);
        if let Some(first) = scan.lines.first() {
            // The first scanned line may start before `start`; use the end of
            // the first complete line as the next line boundary.
            return Ok(first.end);
        }
        // No complete line in the prefix: scan forward in larger steps.
        let mut cursor = start;
        while cursor < self.observed_len {
            let mut chunk = vec![0u8; 64 * 1024];
            let read = self.reader.read_exact_at(cursor, &mut chunk)?;
            chunk.truncate(read);
            if read == 0 {
                break;
            }
            let scan = scan_raw_lines(&chunk, cursor, self.spec.encoding, 0, false);
            if let Some(last) = scan.lines.last() {
                return Ok(last.end);
            }
            cursor += read as u64;
        }
        Ok(self.observed_len)
    }

    fn locate_line_start(&self, line: u64) -> Result<u64, LogError> {
        if line == 0 {
            return Ok(self.spec.bom.len() as u64);
        }
        let checkpoint = self.index.checkpoint_for_line(line);
        let mut offset = checkpoint.raw_start;
        let mut current_line = checkpoint.line;
        let mut pending: Vec<u8> = Vec::new();

        while offset < self.observed_len {
            let read_at = offset + pending.len() as u64;
            let mut fresh = vec![0u8; INDEX_CHUNK_BYTES];
            let read = self.reader.read_exact_at(read_at, &mut fresh)?;
            fresh.truncate(read);
            if read == 0 {
                break;
            }
            let mut data = Vec::with_capacity(pending.len() + fresh.len());
            data.extend_from_slice(&pending);
            data.extend_from_slice(&fresh);
            let last = read_at + read as u64 >= self.observed_len;
            let scan = scan_raw_lines(&data, offset, self.spec.encoding, current_line, last);
            for raw_line in &scan.lines {
                if raw_line.number == line {
                    return Ok(raw_line.start);
                }
            }
            if let Some(last_line) = scan.lines.last() {
                current_line = last_line.number + 1;
            }
            let consumed = scan.consumed as usize;
            offset += scan.consumed;
            pending = data[consumed..].to_vec();
            if last {
                break;
            }
        }

        Err(LogError::LineOutOfRange {
            line,
            lines: self.index.lines_found(),
        })
    }

    fn collect_lines(
        &self,
        start: u64,
        start_line: u64,
        count: usize,
    ) -> Result<Vec<RawLine>, LogError> {
        let mut collected = Vec::new();
        let mut offset = start;
        let mut pending: Vec<u8> = Vec::new();
        let mut line_number = start_line;

        while collected.len() < count && offset < self.observed_len {
            let read_at = offset + pending.len() as u64;
            let mut fresh = vec![0u8; INDEX_CHUNK_BYTES];
            let read = self.reader.read_exact_at(read_at, &mut fresh)?;
            fresh.truncate(read);
            if read == 0 {
                break;
            }
            let mut data = Vec::with_capacity(pending.len() + fresh.len());
            data.extend_from_slice(&pending);
            data.extend_from_slice(&fresh);
            let last = read_at + read as u64 >= self.observed_len;
            let scan = scan_raw_lines(&data, offset, self.spec.encoding, line_number, last);
            for raw_line in scan.lines {
                if raw_line.number >= start_line && collected.len() < count {
                    collected.push(raw_line);
                    line_number = raw_line.number + 1;
                }
            }
            let consumed = scan.consumed as usize;
            offset += scan.consumed;
            pending = data[consumed..].to_vec();
            if last {
                break;
            }
        }

        Ok(collected)
    }

    fn decode_line(&self, raw_line: RawLine, max_line_bytes: usize) -> Result<LogLine, LogError> {
        let content_len = raw_line.content_len();
        let limit = max_line_bytes.min(MAX_VIEW_LINE_BYTES) as u64;
        let take = content_len.min(limit);
        let mut buffer = vec![0u8; take as usize + 8];
        let read = self.reader.read_exact_at(raw_line.start, &mut buffer)?;
        buffer.truncate(read.min(take as usize + 4));
        let mut end = take.min(read as u64) as usize;
        if matches!(
            self.spec.encoding,
            EncodingId::Utf16Le | EncodingId::Utf16Be
        ) {
            end -= end % 2;
        }
        let decode_limit = end;
        let mut attempts = 0;
        loop {
            match decode_payload(&buffer[..end], self.spec.encoding, raw_line.start) {
                Ok(text) => {
                    return Ok(LogLine {
                        line: Some(raw_line.number),
                        raw_start: raw_line.start,
                        raw_end: raw_line.end,
                        raw_content_len: content_len,
                        text,
                        truncated: take < content_len || end < decode_limit,
                        terminator: raw_line.terminator,
                    });
                }
                Err(error) => {
                    if take < content_len && attempts < 4 && end > 0 {
                        attempts += 1;
                        end = end.saturating_sub(1);
                        if matches!(
                            self.spec.encoding,
                            EncodingId::Utf16Le | EncodingId::Utf16Be
                        ) {
                            end -= end % 2;
                        }
                        if end == 0 {
                            return Err(LogError::Codec(error));
                        }
                        continue;
                    }
                    return Err(LogError::Codec(error));
                }
            }
        }
    }

    /// Positional reader shared with the streaming search implementation.
    pub(crate) fn file_reader(&self) -> &FileReader {
        &self.reader
    }

    /// Raw bytes for a line, bounded by the caller.
    pub fn read_raw(&self, range: RawByteRange, limit: usize) -> Result<Vec<u8>, LogError> {
        let length = range.len().min(limit as u64) as usize;
        let mut buffer = vec![0u8; length];
        let read = self.reader.read_exact_at(range.start, &mut buffer)?;
        buffer.truncate(read);
        Ok(buffer)
    }
}

fn detect_log_encoding(head: &[u8], observed_len: u64) -> Result<EncodingSpec, LogError> {
    let bom = sniff_bom(head);
    if bom != BomPolicy::None {
        let encoding = match bom {
            BomPolicy::Utf8 => EncodingId::Utf8,
            BomPolicy::Utf16Le => EncodingId::Utf16Le,
            BomPolicy::Utf16Be => EncodingId::Utf16Be,
            BomPolicy::None => unreachable!(),
        };
        return Ok(EncodingSpec::new(encoding, bom));
    }

    // Strict UTF-8, tolerating an incomplete character at the head boundary.
    let mut trim = 0usize;
    while trim <= 3 && trim < head.len() {
        let end = head.len() - trim;
        if std::str::from_utf8(&head[..end]).is_ok() {
            return Ok(EncodingSpec::utf8());
        }
        trim += 1;
    }

    let candidate = detect_candidate(head);
    if !candidate.supports_random_access() {
        return Ok(EncodingSpec::utf8());
    }
    let _ = observed_len;
    Ok(EncodingSpec::new(candidate, BomPolicy::None))
}

/// Decodes a bounded byte window and keeps per-character source lengths.
pub fn decode_window(
    reader: &FileReader,
    range: RawByteRange,
    spec: &EncodingSpec,
    limit: usize,
) -> Result<(String, Vec<notepust_text_codec::CharSpan>), LogError> {
    let length = range.len().min(limit as u64) as usize;
    let mut buffer = vec![0u8; length];
    let read = reader.read_exact_at(range.start, &mut buffer)?;
    buffer.truncate(read);
    let decoded = decode_with_spans(&buffer, spec.encoding, range.start)?;
    Ok((decoded.text, decoded.spans))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn log_file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn viewport_reads_lines_without_full_load() {
        let file = log_file(b"one\r\ntwo\nthree\r\nfour");
        let mut reader = LogReader::open(file.path(), None).unwrap();
        reader
            .ensure_line_indexed(3, &AtomicBool::new(false))
            .unwrap();
        let viewport = reader.viewport(1, 2, 1024).unwrap();
        assert_eq!(viewport.lines.len(), 2);
        assert_eq!(viewport.lines[0].text, "two");
        assert_eq!(viewport.lines[0].terminator, Some(EolKind::Lf));
        assert_eq!(viewport.lines[1].text, "three");
        assert_eq!(viewport.total_lines, Some(4));
    }

    #[test]
    fn try_viewport_reports_index_progress() {
        let file = log_file(b"a\nb\nc\n");
        let reader = LogReader::open(file.path(), None).unwrap();
        assert!(matches!(
            reader.try_viewport(2, 1, 1024),
            Err(LogError::NeedsIndex { .. })
        ));
        let mut reader = reader;
        reader
            .ensure_line_indexed(2, &AtomicBool::new(false))
            .unwrap();
        assert!(reader.try_viewport(2, 1, 1024).is_ok());
    }

    #[test]
    fn tail_returns_last_lines_without_full_index() {
        let file = log_file(b"1\n2\n3\n4\n5\n");
        let reader = LogReader::open(file.path(), None).unwrap();
        let tail = reader.tail(2, 1024).unwrap();
        assert_eq!(tail.lines.len(), 2);
        assert_eq!(tail.lines[0].text, "4");
        assert_eq!(tail.lines[1].text, "5");
        assert!(tail.lines[0].line.is_none());
    }

    #[test]
    fn utf16_viewport_decodes_code_units() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        for unit in "中文\r\n第二行".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let file = log_file(&bytes);
        let mut reader = LogReader::open(file.path(), None).unwrap();
        reader
            .ensure_line_indexed(1, &AtomicBool::new(false))
            .unwrap();
        let viewport = reader.viewport(0, 2, 1024).unwrap();
        assert_eq!(viewport.lines[0].text, "中文");
        assert_eq!(viewport.lines[1].text, "第二行");
    }

    #[test]
    fn detects_append_and_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, b"one\n").unwrap();
        let mut reader = LogReader::open(&path, None).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"two\n")
            .unwrap();
        assert!(matches!(reader.poll().unwrap(), LogEvent::Appended { .. }));

        std::fs::write(&path, b"x").unwrap();
        assert!(matches!(reader.poll().unwrap(), LogEvent::Truncated { .. }));
        assert_eq!(reader.generation(), 1);
    }

    #[test]
    fn long_lines_are_truncated_explicitly() {
        let long = "x".repeat(200_000);
        let bytes = format!("{long}\nshort\n");
        let file = log_file(bytes.as_bytes());
        let mut reader = LogReader::open(file.path(), None).unwrap();
        reader
            .ensure_line_indexed(1, &AtomicBool::new(false))
            .unwrap();
        let viewport = reader.viewport(0, 2, 1024).unwrap();
        assert!(viewport.lines[0].truncated);
        assert_eq!(viewport.lines[0].text.len(), 1024);
        assert_eq!(viewport.lines[1].text, "short");
    }
}
