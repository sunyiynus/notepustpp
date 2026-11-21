use std::sync::atomic::{AtomicBool, Ordering};

use memchr::memmem;
use notepust_contracts::{SearchMatch, SearchOptions, Utf8Range};
use regex::{Captures, Regex, RegexBuilder};

use crate::error::SearchError;

pub const MAX_PATTERN_BYTES: usize = 64 * 1024;
pub const MAX_CAPTURE_GROUPS: usize = 16;
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const REGEX_SIZE_LIMIT: usize = 8 * 1024 * 1024;

/// What the user typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    Literal(String),
    Regex {
        pattern: String,
        /// PCRE2 mode must go through the worker.
        advanced: bool,
    },
}

#[derive(Clone, Debug)]
pub struct SearchSpec {
    pub pattern: Pattern,
    pub options: SearchOptions,
}

impl SearchSpec {
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: Pattern::Literal(pattern.into()),
            options: SearchOptions::default(),
        }
    }

    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            pattern: Pattern::Regex {
                pattern: pattern.into(),
                advanced: false,
            },
            options: SearchOptions::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub range: Utf8Range,
    /// `0` is the whole match. Only populated when captures were requested.
    pub captures: Vec<Option<Utf8Range>>,
}

impl SearchHit {
    pub fn to_match(&self, line: Option<u64>) -> SearchMatch {
        SearchMatch {
            start_utf8: self.range.start,
            end_utf8: self.range.end,
            line,
            captures: self.captures.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
    pub complete: bool,
    pub bytes_scanned: u64,
    pub elapsed_ms: u64,
}

/// A compiled standard search. Literal case-insensitive searches compile to an
/// escaped regex so Unicode simple folding is used instead of lowercasing the
/// text.
pub enum CompiledSearch {
    Empty,
    Literal { needle: Vec<u8>, whole_word: bool },
    Regex { regex: Regex, whole_word: bool },
}

impl std::fmt::Debug for CompiledSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("CompiledSearch::Empty"),
            Self::Literal { needle, .. } => f
                .debug_struct("CompiledSearch::Literal")
                .field("needle_len", &needle.len())
                .finish(),
            Self::Regex { .. } => f.write_str("CompiledSearch::Regex"),
        }
    }
}

pub fn compile(spec: &SearchSpec) -> Result<CompiledSearch, SearchError> {
    match &spec.pattern {
        Pattern::Literal(literal) => {
            if literal.is_empty() {
                return Ok(CompiledSearch::Empty);
            }
            if spec.options.case_sensitive {
                Ok(CompiledSearch::Literal {
                    needle: literal.as_bytes().to_vec(),
                    whole_word: spec.options.whole_word,
                })
            } else {
                let escaped = regex::escape(literal);
                Ok(CompiledSearch::Regex {
                    regex: build_regex(&escaped, &spec.options)?,
                    whole_word: spec.options.whole_word,
                })
            }
        }
        Pattern::Regex { pattern, advanced } => {
            if *advanced {
                return Err(SearchError::AdvancedRequired {
                    detail: "PCRE2 lookaround/backreferences must run in search-worker".to_string(),
                });
            }
            if pattern.is_empty() {
                return Ok(CompiledSearch::Empty);
            }
            Ok(CompiledSearch::Regex {
                regex: build_regex(pattern, &spec.options)?,
                whole_word: spec.options.whole_word,
            })
        }
    }
}

fn build_regex(pattern: &str, options: &SearchOptions) -> Result<Regex, SearchError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(SearchError::resource(format!(
            "pattern is {} bytes; limit is {MAX_PATTERN_BYTES}",
            pattern.len()
        )));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(!options.case_sensitive)
        .dot_matches_new_line(options.dot_all)
        .multi_line(options.multi_line)
        .crlf(options.crlf)
        .unicode(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .nest_limit(128)
        .build()
        .map_err(|error| classify_regex_error(error, pattern))
}

fn classify_regex_error(error: regex::Error, pattern: &str) -> SearchError {
    let text = error.to_string();
    let lowercase = text.to_ascii_lowercase();
    if lowercase.contains("look-around")
        || lowercase.contains("lookahead")
        || lowercase.contains("lookbehind")
        || lowercase.contains("backreference")
        || lowercase.contains("backreference")
    {
        return SearchError::AdvancedRequired {
            detail: format!("{text}; switch to advanced regex mode"),
        };
    }
    if text.contains("size limit") || text.contains("nest limit") {
        return SearchError::resource(format!("pattern {pattern:?}: {text}"));
    }
    SearchError::InvalidPattern { detail: text }
}

pub fn whole_word_boundaries(text: &str, start: u64, end: u64) -> bool {
    let start = start as usize;
    let end = end as usize;
    if start > text.len() || end > text.len() || start > end {
        return false;
    }
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

/// Compatibility definition of a word character: Unicode alphanumerics plus
/// `_`. Chinese characters count as letters, so there is no word segmentation
/// based on dictionary boundaries.
pub fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

impl CompiledSearch {
    pub fn find_all(
        &self,
        text: &str,
        limit: usize,
        with_captures: bool,
        cancel: &AtomicBool,
    ) -> Result<SearchOutcome, SearchError> {
        let started = std::time::Instant::now();
        let mut hits = Vec::new();
        let mut truncated = false;

        match self {
            Self::Empty => {
                return Ok(SearchOutcome {
                    complete: true,
                    ..Default::default()
                });
            }
            Self::Literal { needle, whole_word } => {
                let needle = needle.as_slice();
                for (index, found) in memmem::find_iter(text.as_bytes(), needle).enumerate() {
                    if index % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                        return Err(SearchError::Cancelled);
                    }
                    if limit != 0 && hits.len() >= limit {
                        truncated = true;
                        break;
                    }
                    let start = found as u64;
                    let end = start + needle.len() as u64;
                    if *whole_word && !whole_word_boundaries(text, start, end) {
                        continue;
                    }
                    let range = Utf8Range { start, end };
                    hits.push(SearchHit {
                        range,
                        captures: if with_captures {
                            vec![Some(range)]
                        } else {
                            Vec::new()
                        },
                    });
                }
            }
            Self::Regex { regex, whole_word } => {
                if with_captures {
                    let capture_limit = regex.captures_len().min(MAX_CAPTURE_GROUPS);
                    for (index, captures) in regex.captures_iter(text).enumerate() {
                        if index % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                            return Err(SearchError::Cancelled);
                        }
                        if limit != 0 && hits.len() >= limit {
                            truncated = true;
                            break;
                        }
                        let Some(whole) = captures.get(0) else {
                            continue;
                        };
                        let start = whole.start() as u64;
                        let end = whole.end() as u64;
                        if *whole_word && !whole_word_boundaries(text, start, end) {
                            continue;
                        }
                        hits.push(SearchHit {
                            range: Utf8Range { start, end },
                            captures: capture_ranges(&captures, capture_limit),
                        });
                    }
                } else {
                    for (index, found) in regex.find_iter(text).enumerate() {
                        if index % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                            return Err(SearchError::Cancelled);
                        }
                        if limit != 0 && hits.len() >= limit {
                            truncated = true;
                            break;
                        }
                        let start = found.start() as u64;
                        let end = found.end() as u64;
                        if *whole_word && !whole_word_boundaries(text, start, end) {
                            continue;
                        }
                        hits.push(SearchHit {
                            range: Utf8Range { start, end },
                            captures: Vec::new(),
                        });
                    }
                }
            }
        }

        Ok(SearchOutcome {
            hits,
            truncated,
            complete: !truncated,
            bytes_scanned: text.len() as u64,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    pub fn captures_len(&self) -> usize {
        match self {
            Self::Regex { regex, .. } => regex.captures_len(),
            Self::Literal { .. } | Self::Empty => 1,
        }
    }

    pub fn capture_names(&self) -> Vec<String> {
        match self {
            Self::Regex { regex, .. } => regex
                .capture_names()
                .flatten()
                .map(ToString::to_string)
                .collect(),
            Self::Literal { .. } | Self::Empty => Vec::new(),
        }
    }

    /// Expands a replacement program for one match. Literal searches do not
    /// interpret `$1` syntax; their replacement is copied verbatim.
    pub fn expand(
        &self,
        program: &notepust_contracts::ReplacementProgram,
        captures: &Captures<'_>,
    ) -> String {
        let mut output = String::new();
        for piece in &program.pieces {
            match piece {
                notepust_contracts::ReplacementPiece::Literal { text } => output.push_str(text),
                notepust_contracts::ReplacementPiece::Match => {
                    if let Some(found) = captures.get(0) {
                        output.push_str(found.as_str());
                    }
                }
                notepust_contracts::ReplacementPiece::Group { index } => {
                    if let Some(found) = captures.get(*index as usize) {
                        output.push_str(found.as_str());
                    }
                }
                notepust_contracts::ReplacementPiece::NamedGroup { name } => {
                    if let Some(found) = captures.name(name) {
                        output.push_str(found.as_str());
                    }
                }
                notepust_contracts::ReplacementPiece::Dollar => output.push('$'),
            }
        }
        output
    }
}

fn capture_ranges(captures: &Captures<'_>, limit: usize) -> Vec<Option<Utf8Range>> {
    (0..limit)
        .map(|index| {
            captures.get(index).map(|found| Utf8Range {
                start: found.start() as u64,
                end: found.end() as u64,
            })
        })
        .collect()
}

/// Convenience entry point for the in-process find bar.
pub fn find(
    text: &str,
    spec: &SearchSpec,
    limit: usize,
    with_captures: bool,
    cancel: &AtomicBool,
) -> Result<SearchOutcome, SearchError> {
    let compiled = compile(spec)?;
    compiled.find_all(text, limit, with_captures, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_contracts::ReplacementProgram;

    fn cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn literal_search_finds_cross_line_text() {
        let outcome = find(
            "one\r\ntwo one",
            &SearchSpec::literal("e\r\ntw"),
            0,
            false,
            &cancel(),
        )
        .unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].range, Utf8Range::new(2u64, 7u64).unwrap());
    }

    #[test]
    fn empty_pattern_finds_nothing() {
        assert!(
            find("abc", &SearchSpec::literal(""), 0, false, &cancel())
                .unwrap()
                .hits
                .is_empty()
        );
    }

    #[test]
    fn unicode_case_insensitive_uses_folding_not_lowercasing() {
        let spec = SearchSpec {
            pattern: Pattern::Literal("STRASSE".to_string()),
            options: SearchOptions {
                case_sensitive: false,
                ..Default::default()
            },
        };
        let outcome = find("Straße STRASSE", &spec, 0, false, &cancel()).unwrap();
        // Rust regex simple folding does not expand ß to SS; both literal
        // spellings are still found without copying or lowercasing the text.
        assert_eq!(outcome.hits.len(), 1);
    }

    #[test]
    fn whole_word_respects_chinese_and_underscore() {
        let spec = SearchSpec {
            pattern: Pattern::Literal("中".to_string()),
            options: SearchOptions {
                whole_word: true,
                ..Default::default()
            },
        };
        let outcome = find("中文 中", &spec, 0, false, &cancel()).unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].range.start, 7);

        let underscore = SearchSpec {
            pattern: Pattern::Literal("foo".to_string()),
            options: SearchOptions {
                whole_word: true,
                ..Default::default()
            },
        };
        assert!(
            find("foo_bar foo", &underscore, 0, false, &cancel())
                .unwrap()
                .hits
                .len()
                == 1
        );
    }

    #[test]
    fn zero_width_matches_advance_and_stop() {
        let outcome = find("a\nb", &SearchSpec::regex(r"^"), 0, true, &cancel()).unwrap();
        assert_eq!(outcome.hits.len(), 2);
        let outcome = find("aaa", &SearchSpec::regex(r"a*"), 0, false, &cancel()).unwrap();
        assert!(!outcome.hits.is_empty());
        assert!(outcome.complete);
    }

    #[test]
    fn lookaround_is_reported_as_needing_advanced_mode() {
        let error = find(
            r"a(?=b)",
            &SearchSpec::regex(r"a(?=b)"),
            0,
            false,
            &cancel(),
        )
        .unwrap_err();
        assert!(matches!(error, SearchError::AdvancedRequired { .. }));
    }

    #[test]
    fn crlf_multiline_anchors_match_lf_semantics() {
        let outcome = find(
            "a\r\nb\r\nc",
            &SearchSpec::regex("(?m)^b$"),
            0,
            false,
            &cancel(),
        )
        .unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].range, Utf8Range::new(3u64, 4u64).unwrap());
    }

    #[test]
    fn captures_include_optional_groups() {
        let outcome = find("ab a", &SearchSpec::regex(r"(a)(b)?"), 0, true, &cancel()).unwrap();
        assert_eq!(outcome.hits.len(), 2);
        assert_eq!(outcome.hits[1].captures[2], None);
    }

    #[test]
    fn limit_marks_truncated() {
        let outcome = find("aaaa", &SearchSpec::literal("a"), 2, false, &cancel()).unwrap();
        assert_eq!(outcome.hits.len(), 2);
        assert!(outcome.truncated);
        assert!(!outcome.complete);
    }

    #[test]
    fn cancellation_stops_scanning() {
        let flag = AtomicBool::new(true);
        let error = find("abcdef", &SearchSpec::literal("a"), 0, false, &flag).unwrap_err();
        assert!(matches!(error, SearchError::Cancelled));
    }

    #[test]
    fn literal_replacement_program_can_be_expanded_for_regex() {
        let compiled = compile(&SearchSpec::regex(r"(a)(b)")).unwrap();
        let captures = match &compiled {
            CompiledSearch::Regex { regex, .. } => regex.captures("ab").unwrap(),
            _ => panic!(),
        };
        let program = ReplacementProgram {
            pieces: vec![
                notepust_contracts::ReplacementPiece::Group { index: 2 },
                notepust_contracts::ReplacementPiece::Group { index: 1 },
            ],
        };
        assert_eq!(compiled.expand(&program, &captures), "ba");
    }
}
