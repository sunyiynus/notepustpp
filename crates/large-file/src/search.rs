//! Bounded, line-oriented search over a large log.
//!
//! The scan is streaming: the file is read in fixed-size chunks with positional
//! reads, raw line boundaries are found without decoding, and only complete
//! lines are decoded. A line that crosses a chunk boundary (including a CRLF
//! whose CR ends one chunk) is carried into the next chunk, so a match may span
//! the boundary. The whole file is never materialized.
//!
//! Matches are line-level. A regex cannot match across a line terminator.

use std::sync::atomic::{AtomicBool, Ordering};

use memchr::memmem;
use notepust_contracts::EncodingId;
use notepust_text_codec::{decode_payload, decode_with_spans};
use regex::{Regex, RegexBuilder};

use crate::reader::{LogError, LogReader};
use crate::scan::scan_raw_lines;

/// Bytes read from the file per streaming step.
pub const SEARCH_CHUNK_BYTES: usize = 1024 * 1024;

/// Maximum number of characters in [`LogSearchHit::preview`].
pub const MAX_SEARCH_PREVIEW_CHARS: usize = 240;

/// Characters of leading context kept when a preview must be trimmed.
const PREVIEW_CONTEXT_BEFORE: usize = 80;
const REGEX_SIZE_LIMIT: usize = 8 * 1024 * 1024;

/// Options for [`LogReader::search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSearchOptions {
    /// When false, matching uses Unicode simple case folding.
    pub case_sensitive: bool,
    /// Require non-word characters (or line edges) around the whole match.
    pub whole_word: bool,
    /// Interpret `query` as a line-level regular expression.
    pub regex: bool,
    /// Stop after this many hits; `0` means unlimited.
    pub max_results: usize,
}

impl Default for LogSearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            whole_word: false,
            regex: false,
            max_results: 1_000,
        }
    }
}

/// One match inside one raw line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSearchHit {
    /// Exact 1-based line number, counting CR, LF and CRLF as one line each.
    pub line: u64,
    /// Absolute offset of the first raw file byte of the whole match.
    pub raw_start: u64,
    /// Absolute offset one past the last raw file byte of the whole match.
    pub raw_end: u64,
    /// Bounded preview of the whole line, at most [`MAX_SEARCH_PREVIEW_CHARS`]
    /// characters, with `…` where the line was clipped.
    pub preview: String,
    /// True when `preview` omits characters from the line.
    pub preview_truncated: bool,
}

/// Outcome of a streaming search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogSearchResult {
    /// Matches in file order.
    pub hits: Vec<LogSearchHit>,
    /// Bytes of the file up to the read position when the scan stopped,
    /// including the BOM and any tail held for an incomplete line.
    pub scanned_bytes: u64,
    /// Complete lines examined when the scan stopped.
    pub scanned_lines: u64,
    /// File length observed when the search started.
    pub observed_len: u64,
    /// True when `max_results` was reached; more matches may exist.
    pub truncated: bool,
    /// True when cancellation stopped the scan; `hits` are partial.
    pub cancelled: bool,
}

impl LogReader {
    /// Searches the log line by line without loading it into memory.
    ///
    /// Hits are reported in file order with 1-based line numbers and absolute
    /// raw byte offsets. The progress callback receives
    /// `(bytes_scanned, total_bytes)` after every chunk. Cancellation is polled
    /// per chunk and returns `Ok` with `cancelled: true` and the hits collected
    /// so far. An empty query returns no hits and does not scan.
    pub fn search(
        &mut self,
        query: &str,
        options: &LogSearchOptions,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<LogSearchResult, LogError> {
        let observed_len = self.observed_len();
        let encoding = self.spec().encoding;
        let bom = self.spec().bom.len() as u64;
        let reader = self.file_reader();

        let mut result = LogSearchResult {
            observed_len,
            ..LogSearchResult::default()
        };
        let Some(matcher) = compile_matcher(query, options)? else {
            return Ok(result);
        };

        let mut offset = bom.min(observed_len);
        let mut pending: Vec<u8> = Vec::new();
        let mut next_line = 0u64;

        loop {
            if cancel.load(Ordering::Relaxed) {
                result.cancelled = true;
                break;
            }

            let read_at = offset + pending.len() as u64;
            if read_at >= observed_len {
                break;
            }

            let to_read = SEARCH_CHUNK_BYTES.min((observed_len - read_at) as usize);
            let mut fresh = vec![0u8; to_read];
            let read = reader.read_exact_at(read_at, &mut fresh)?;
            fresh.truncate(read);
            // A short read before the advertised length means the file shrank
            // under us; finish with what is available instead of spinning.
            let eof = read == 0 || read_at + read as u64 >= observed_len;

            let mut data = Vec::with_capacity(pending.len() + fresh.len());
            data.extend_from_slice(&pending);
            data.extend_from_slice(&fresh);
            let scan = scan_raw_lines(&data, offset, encoding, next_line, eof);

            let mut stopped = false;
            for raw_line in &scan.lines {
                next_line = raw_line.number + 1;
                let content_start = (raw_line.start - offset) as usize;
                let content_end = (raw_line.content_end - offset) as usize;
                let content = &data[content_start..content_end];
                if content.is_empty() {
                    continue;
                }

                let text = decode_payload(content, encoding, raw_line.start)?;
                let remaining = if options.max_results == 0 {
                    usize::MAX
                } else {
                    options.max_results.saturating_sub(result.hits.len())
                };
                let line_matches = find_line_matches(&matcher, &text, remaining);
                if line_matches.is_empty() {
                    continue;
                }

                let boundaries = if matches!(
                    encoding,
                    EncodingId::Utf8 | EncodingId::Utf16Le | EncodingId::Utf16Be
                ) {
                    Vec::new()
                } else {
                    raw_boundaries(content, encoding, raw_line.start)?
                };

                for (match_start, match_end) in line_matches {
                    let (raw_start, raw_end) = raw_span(
                        &text,
                        encoding,
                        raw_line.start,
                        &boundaries,
                        match_start,
                        match_end,
                    );
                    let (preview, preview_truncated) = build_preview(&text, match_start, match_end);
                    result.hits.push(LogSearchHit {
                        line: raw_line.number + 1,
                        raw_start,
                        raw_end,
                        preview,
                        preview_truncated,
                    });
                    if options.max_results != 0 && result.hits.len() >= options.max_results {
                        result.truncated = true;
                        stopped = true;
                        break;
                    }
                }
                if stopped {
                    break;
                }
            }

            let consumed = scan.consumed as usize;
            offset += scan.consumed;
            pending.clear();
            if !eof {
                pending.extend_from_slice(&data[consumed..]);
            }
            result.scanned_bytes = (offset + pending.len() as u64).min(observed_len);
            result.scanned_lines = next_line;
            on_progress(result.scanned_bytes, observed_len);

            if stopped || eof {
                break;
            }
        }

        Ok(result)
    }
}

enum LineMatcher {
    Literal { needle: Vec<u8>, whole_word: bool },
    Regex { regex: Regex, whole_word: bool },
}

/// Compiles the query. `None` means the query is empty and nothing is scanned.
fn compile_matcher(
    query: &str,
    options: &LogSearchOptions,
) -> Result<Option<LineMatcher>, LogError> {
    if query.is_empty() {
        return Ok(None);
    }
    if options.regex {
        return Ok(Some(LineMatcher::Regex {
            regex: build_regex(query, options.case_sensitive)?,
            whole_word: options.whole_word,
        }));
    }
    if options.case_sensitive {
        return Ok(Some(LineMatcher::Literal {
            needle: query.as_bytes().to_vec(),
            whole_word: options.whole_word,
        }));
    }
    // Escape the literal and let the regex engine fold case. This keeps match
    // offsets in the original text, so no lowercased copy is needed.
    let escaped = regex::escape(query);
    Ok(Some(LineMatcher::Regex {
        regex: build_regex(&escaped, false)?,
        whole_word: options.whole_word,
    }))
}

fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex, LogError> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .unicode(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| LogError::InvalidPattern {
            detail: error.to_string(),
        })
}

/// Non-overlapping matches as UTF-8 byte ranges in `text`, capped at `limit`.
fn find_line_matches(matcher: &LineMatcher, text: &str, limit: usize) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    match matcher {
        LineMatcher::Literal { needle, whole_word } => {
            for start in memmem::find_iter(text.as_bytes(), needle) {
                let end = start + needle.len();
                if *whole_word && !whole_word_boundaries(text, start, end) {
                    continue;
                }
                matches.push((start, end));
                if matches.len() >= limit {
                    break;
                }
            }
        }
        LineMatcher::Regex { regex, whole_word } => {
            for found in regex.find_iter(text) {
                let (start, end) = (found.start(), found.end());
                if *whole_word && !whole_word_boundaries(text, start, end) {
                    continue;
                }
                matches.push((start, end));
                if matches.len() >= limit {
                    break;
                }
            }
        }
    }
    matches
}

fn whole_word_boundaries(text: &str, start: usize, end: usize) -> bool {
    let left_ok = text[..start]
        .chars()
        .next_back()
        .map(|character| !is_word_char(character))
        .unwrap_or(true);
    let right_ok = text[end..]
        .chars()
        .next()
        .map(|character| !is_word_char(character))
        .unwrap_or(true);
    left_ok && right_ok
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Maps decoded UTF-8 offsets back to absolute raw file offsets.
///
/// UTF-8 and UTF-16 can be mapped arithmetically. GBK and GB18030 need
/// per-character raw lengths, supplied by `boundaries` from
/// [`decode_with_spans`].
fn raw_span(
    text: &str,
    encoding: EncodingId,
    raw_start: u64,
    boundaries: &[(u64, u64)],
    match_start: usize,
    match_end: usize,
) -> (u64, u64) {
    match encoding {
        EncodingId::Utf8 => (raw_start + match_start as u64, raw_start + match_end as u64),
        EncodingId::Utf16Le | EncodingId::Utf16Be => {
            let start_units = text[..match_start].encode_utf16().count() as u64;
            let end_units = text[..match_end].encode_utf16().count() as u64;
            (raw_start + start_units * 2, raw_start + end_units * 2)
        }
        _ => (
            boundary_offset(boundaries, match_start as u64, false),
            boundary_offset(boundaries, match_end as u64, true),
        ),
    }
}

/// `(utf8_offset, raw_offset)` at every character boundary of a decoded line.
fn raw_boundaries(
    content: &[u8],
    encoding: EncodingId,
    raw_start: u64,
) -> Result<Vec<(u64, u64)>, LogError> {
    let decoded = decode_with_spans(content, encoding, raw_start)?;
    let mut boundaries = Vec::with_capacity(decoded.spans.len() + 1);
    let mut utf8 = 0u64;
    let mut raw = raw_start;
    boundaries.push((0, raw));
    for span in decoded.spans {
        utf8 += span.utf8_len as u64;
        raw += span.raw_len as u64;
        boundaries.push((utf8, raw));
    }
    Ok(boundaries)
}

/// Finds the raw offset for a UTF-8 offset. `round_up` resolves an offset that
/// falls inside a character to the end of that character.
fn boundary_offset(boundaries: &[(u64, u64)], offset: u64, round_up: bool) -> u64 {
    match boundaries.binary_search_by_key(&offset, |boundary| boundary.0) {
        Ok(index) => boundaries[index].1,
        Err(0) => boundaries[0].1,
        Err(index) if round_up => boundaries[index].1,
        Err(index) => boundaries[index - 1].1,
    }
}

/// Builds a bounded preview around the match.
fn build_preview(text: &str, match_start: usize, match_end: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= MAX_SEARCH_PREVIEW_CHARS {
        return (text.to_string(), false);
    }

    let match_char_start = text[..match_start].chars().count();
    let match_char_end = text[..match_end].chars().count();
    let match_chars = match_char_end - match_char_start;

    // A match that nearly fills the window is shown from its own start.
    if match_chars + 2 >= MAX_SEARCH_PREVIEW_CHARS {
        let take = MAX_SEARCH_PREVIEW_CHARS - 2;
        let mut preview = String::new();
        preview.push('…');
        preview.extend(text.chars().skip(match_char_start).take(take));
        preview.push('…');
        return (preview, true);
    }

    let mut before = match_char_start.min(PREVIEW_CONTEXT_BEFORE);
    let after_available = total - match_char_end;
    let mut after =
        after_available.min(MAX_SEARCH_PREVIEW_CHARS.saturating_sub(match_chars + before));
    let mut truncated_before = before < match_char_start;
    let mut truncated_after = after < after_available;
    loop {
        let used = before
            + match_chars
            + after
            + usize::from(truncated_before)
            + usize::from(truncated_after);
        if used <= MAX_SEARCH_PREVIEW_CHARS {
            break;
        }
        if after > 0 {
            after -= 1;
            truncated_after = true;
        } else if before > 0 {
            before -= 1;
            truncated_before = true;
        } else {
            break;
        }
    }

    let mut preview = String::new();
    if truncated_before {
        preview.push('…');
    }
    preview.extend(
        text.chars()
            .skip(match_char_start - before)
            .take(before + match_chars + after),
    );
    if truncated_after {
        preview.push('…');
    }
    (preview, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use notepust_contracts::{BomPolicy, EncodingSpec};
    use notepust_text_codec::encode_text;

    fn write_file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        file
    }

    fn run_search(
        reader: &mut LogReader,
        query: &str,
        options: &LogSearchOptions,
    ) -> LogSearchResult {
        reader
            .search(query, options, &AtomicBool::new(false), |_, _| {})
            .unwrap()
    }

    fn hit_lines(result: &LogSearchResult) -> Vec<u64> {
        result.hits.iter().map(|hit| hit.line).collect()
    }

    #[test]
    fn literal_match_spans_chunk_boundary() {
        // The needle starts four bytes before the first chunk ends.
        let mut bytes = vec![b'A'; SEARCH_CHUNK_BYTES - 4];
        bytes.extend_from_slice(b"needle\nsecond line\n");
        let file = write_file(&bytes);
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let result = run_search(&mut reader, "needle", &LogSearchOptions::default());
        assert!(!result.cancelled);
        assert!(!result.truncated);
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(hit.line, 1);
        assert_eq!(hit.raw_start, (SEARCH_CHUNK_BYTES - 4) as u64);
        assert_eq!(hit.raw_end, (SEARCH_CHUNK_BYTES + 2) as u64);
        assert!(hit.preview_truncated);
        assert!(hit.preview.contains("needle"));
        assert!(hit.preview.chars().count() <= MAX_SEARCH_PREVIEW_CHARS);

        let second = run_search(&mut reader, "second", &LogSearchOptions::default());
        assert_eq!(second.hits.len(), 1);
        assert_eq!(second.hits[0].line, 2);
        assert_eq!(second.hits[0].raw_start, (SEARCH_CHUNK_BYTES + 3) as u64);
        assert_eq!(second.hits[0].raw_end, (SEARCH_CHUNK_BYTES + 9) as u64);
    }

    #[test]
    fn case_insensitive_and_whole_word() {
        let file = write_file(b"Hello world\nhello there\nshelloworld\nHELLO\n");
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let sensitive = run_search(
            &mut reader,
            "Hello",
            &LogSearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert_eq!(hit_lines(&sensitive), vec![1]);

        let insensitive = run_search(&mut reader, "hello", &LogSearchOptions::default());
        assert_eq!(hit_lines(&insensitive), vec![1, 2, 3, 4]);

        let whole = run_search(
            &mut reader,
            "hello",
            &LogSearchOptions {
                whole_word: true,
                ..Default::default()
            },
        );
        assert_eq!(hit_lines(&whole), vec![1, 2, 4]);
    }

    #[test]
    fn utf16le_search_reports_lines_and_raw_offsets() {
        let spec = EncodingSpec::new(EncodingId::Utf16Le, BomPolicy::Utf16Le);
        let bytes = encode_text("first\nsecond 中文 needle\nthird\n", &spec).unwrap();
        let file = write_file(&bytes);
        let mut reader = LogReader::open(file.path(), Some(spec)).unwrap();

        let result = run_search(&mut reader, "中文", &LogSearchOptions::default());
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(hit.line, 2);
        // BOM + "first" + CR/LF + "second " = 2 + 10 + 2 + 14.
        assert_eq!(hit.raw_start, 28);
        assert_eq!(hit.raw_end, 32);
        assert_eq!(hit.preview, "second 中文 needle");

        let needle = run_search(&mut reader, "needle", &LogSearchOptions::default());
        assert_eq!(needle.hits.len(), 1);
        assert_eq!(needle.hits[0].line, 2);
        assert_eq!(needle.hits[0].raw_start, 34);
        // Six UTF-16 code units, two bytes each.
        assert_eq!(needle.hits[0].raw_end, 46);
    }

    #[test]
    fn gbk_search_finds_chinese_literal() {
        let spec = EncodingSpec::new(EncodingId::Gbk, BomPolicy::None);
        let bytes = encode_text("第一行\n中文关键字\n第三行\n", &spec).unwrap();
        let file = write_file(&bytes);
        let mut reader = LogReader::open(file.path(), Some(spec)).unwrap();

        let result = run_search(&mut reader, "中文", &LogSearchOptions::default());
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(hit.line, 2);
        // "第一行" is 6 raw bytes plus one LF.
        assert_eq!(hit.raw_start, 7);
        assert_eq!(hit.raw_end, 11);
        assert_eq!(hit.preview, "中文关键字");
    }

    #[test]
    fn regex_search_and_invalid_pattern() {
        let file = write_file(b"error 404\nok\nerror 500\n");
        let mut reader = LogReader::open(file.path(), None).unwrap();
        let options = LogSearchOptions {
            regex: true,
            ..Default::default()
        };

        let result = run_search(&mut reader, r"error \d+", &options);
        assert_eq!(hit_lines(&result), vec![1, 3]);
        assert_eq!(result.hits[0].raw_start, 0);
        assert_eq!(result.hits[0].raw_end, 9);
        assert_eq!(result.hits[1].raw_start, 13);
        assert_eq!(result.hits[1].raw_end, 22);

        let error = reader
            .search("(", &options, &AtomicBool::new(false), |_, _| {})
            .unwrap_err();
        assert!(matches!(error, LogError::InvalidPattern { .. }));
    }

    #[test]
    fn max_results_truncates() {
        let file = write_file(b"hit one\nhit two\nhit three\nhit four\n");
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let result = run_search(
            &mut reader,
            "hit",
            &LogSearchOptions {
                max_results: 2,
                ..Default::default()
            },
        );
        assert_eq!(hit_lines(&result), vec![1, 2]);
        assert!(result.truncated);
        assert!(!result.cancelled);
    }

    #[test]
    fn pre_set_cancel_returns_partial_hits_without_error() {
        let file = write_file(b"needle\nneedle\n");
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let cancel = AtomicBool::new(true);
        let result = reader
            .search("needle", &LogSearchOptions::default(), &cancel, |_, _| {})
            .unwrap();
        assert!(result.cancelled);
        assert!(!result.truncated);
        assert!(result.hits.is_empty());
        assert_eq!(result.scanned_bytes, 0);
        assert_eq!(result.scanned_lines, 0);
    }

    #[test]
    fn mixed_line_terminators_keep_line_numbers() {
        let file = write_file(b"alpha\r\nbeta\rgamma\nneedle\n");
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let result = run_search(&mut reader, "needle", &LogSearchOptions::default());
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].line, 4);
        assert_eq!(result.hits[0].raw_start, 18);
        assert_eq!(result.hits[0].raw_end, 24);
    }

    #[test]
    fn cancellation_after_first_chunk_keeps_partial_hits() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"needle first\n");
        let filler_len = bytes.len() + SEARCH_CHUNK_BYTES + 16;
        bytes.resize(filler_len, b'x');
        bytes.extend_from_slice(b"\nneedle second\n");
        let file = write_file(&bytes);
        let mut reader = LogReader::open(file.path(), None).unwrap();

        let cancel = AtomicBool::new(false);
        let result = reader
            .search("needle", &LogSearchOptions::default(), &cancel, |_, _| {
                cancel.store(true, Ordering::Relaxed);
            })
            .unwrap();
        assert!(result.cancelled);
        assert!(!result.truncated);
        assert_eq!(hit_lines(&result), vec![1]);
    }
}
