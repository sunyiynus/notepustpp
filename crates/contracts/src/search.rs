use serde::{Deserialize, Serialize};

use crate::files::NativePath;
use crate::ids::{JobId, SnapshotId};
use crate::offsets::Utf8Range;

/// Regex engine used for a request. The worker never silently switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegexEngine {
    /// Rust `regex`: linear time, Unicode by default, no lookaround/backrefs.
    Standard,
    /// PCRE2: enabled only by an explicit user choice.
    Advanced,
}

impl RegexEngine {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "Regular expression",
            Self::Advanced => "Advanced regex (PCRE2)",
        }
    }
}

/// The worker either only finds matches or builds a complete replacement
/// artifact. A replacement plan is never limited by the preview cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPurpose {
    Find,
    ReplacePlan,
}

/// Engine-independent search options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub dot_all: bool,
    pub multi_line: bool,
    pub crlf: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            whole_word: false,
            dot_all: false,
            multi_line: true,
            crlf: true,
        }
    }
}

/// One parsed replacement fragment. Both engines only report capture ranges;
/// expansion of this program is application logic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplacementPiece {
    Literal { text: String },
    Match,
    Group { index: u32 },
    NamedGroup { name: String },
    Dollar,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementProgram {
    pub pieces: Vec<ReplacementPiece>,
}

impl ReplacementProgram {
    pub fn literal(text: impl Into<String>) -> Self {
        let text = text.into();
        if text.is_empty() {
            return Self::default();
        }
        Self {
            pieces: vec![ReplacementPiece::Literal { text }],
        }
    }
}

/// One match returned by the worker.
///
/// The parent process owns the document revision association. The worker only
/// knows the snapshot identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub start_utf8: u64,
    pub end_utf8: u64,
    /// Exact only for line-oriented log searches; `None` when unknown.
    pub line: Option<u64>,
    /// `0` is the whole match; optional groups are `None`.
    pub captures: Vec<Option<Utf8Range>>,
}

impl SearchMatch {
    pub fn range(&self) -> Utf8Range {
        Utf8Range {
            start: self.start_utf8,
            end: self.end_utf8,
        }
    }
}

/// A regular expression job sent to a persistent worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    pub job_id: JobId,
    pub purpose: SearchPurpose,
    pub engine: RegexEngine,
    pub pattern: String,
    pub options: SearchOptions,
    pub snapshot_id: SnapshotId,
    pub path: NativePath,
    pub utf8_length: u64,
    pub timeout_ms: u64,
    pub result_limit: usize,
    pub replacement: Option<ReplacementProgram>,
    /// Private output path for `ReplacePlan`; never the real target file.
    pub output_artifact_path: Option<NativePath>,
}

/// Why a search or replacement job stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Complete,
    Truncated,
    Cancelled,
    Partial,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_options_have_documented_defaults() {
        let options = SearchOptions::default();
        assert!(!options.case_sensitive);
        assert!(!options.whole_word);
        assert!(options.multi_line);
        assert!(options.crlf);
    }

    #[test]
    fn replacement_program_roundtrips() {
        let program = ReplacementProgram {
            pieces: vec![
                ReplacementPiece::Group { index: 1 },
                ReplacementPiece::Literal {
                    text: " - ".to_string(),
                },
                ReplacementPiece::NamedGroup {
                    name: "name".to_string(),
                },
                ReplacementPiece::Dollar,
            ],
        };
        let json = serde_json::to_string(&program).unwrap();
        let back: ReplacementProgram = serde_json::from_str(&json).unwrap();
        assert_eq!(program, back);
    }
}
