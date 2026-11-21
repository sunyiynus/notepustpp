use std::sync::atomic::AtomicBool;

use notepust_contracts::{TextEdit, Utf8Range};

use crate::error::SearchError;
use crate::matcher::{CompiledSearch, Pattern, SearchSpec};
use crate::replacement::{ReplacementError, parse_replacement, validate_program};

/// A complete, non-overlapping set of edits for one document snapshot.
#[derive(Clone, Debug, Default)]
pub struct ReplacePlan {
    pub edits: Vec<TextEdit>,
    pub match_count: u64,
    pub preview: Vec<PreviewMatch>,
    pub truncated_preview: bool,
}

#[derive(Clone, Debug)]
pub struct PreviewMatch {
    pub range: Utf8Range,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct ReplaceRequest<'a> {
    pub spec: &'a SearchSpec,
    /// The user-typed replacement. For literal searches it is copied verbatim;
    /// for regex searches it is parsed as the replacement language.
    pub replacement: &'a str,
    pub interpret_escapes: bool,
    pub preview_limit: usize,
    /// Hard cap on the number of edits built in memory. The worker's streaming
    /// artifact path exists for larger documents.
    pub max_edits: usize,
}

/// Builds every replacement edit for a document.
///
/// The full scan is independent of the preview limit. `max_edits` protects the
/// in-memory path; the search worker's replacement artifact supports larger
/// jobs without collecting all captures in the UI.
pub fn plan_replace_all(
    text: &str,
    request: &ReplaceRequest<'_>,
    cancel: &AtomicBool,
) -> Result<ReplacePlan, SearchError> {
    let compiled = crate::matcher::compile(request.spec)?;
    let is_regex = matches!(request.spec.pattern, Pattern::Regex { .. });

    let program =
        if is_regex {
            let program = parse_replacement(request.replacement, request.interpret_escapes)
                .map_err(|error| SearchError::InvalidPattern {
                    detail: error.to_string(),
                })?;
            validate_program(&program, compiled.captures_len(), &compiled.capture_names())
                .map_err(|error| SearchError::InvalidPattern {
                    detail: error.to_string(),
                })?;
            Some(program)
        } else {
            None
        };

    let mut plan = ReplacePlan::default();
    match &compiled {
        CompiledSearch::Empty => {
            return Ok(plan);
        }
        CompiledSearch::Literal { needle, whole_word } => {
            let needle = needle.as_slice();
            for found in memchr::memmem::find_iter(text.as_bytes(), needle) {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(SearchError::Cancelled);
                }
                if plan.edits.len() >= request.max_edits {
                    return Err(SearchError::resource(format!(
                        "replacement would create more than {} edits",
                        request.max_edits
                    )));
                }
                let start = found as u64;
                let end = start + needle.len() as u64;
                if *whole_word && !crate::matcher::whole_word_boundaries(text, start, end) {
                    continue;
                }
                plan.edits.push(TextEdit::new(
                    Utf8Range { start, end },
                    request.replacement.to_string(),
                ));
                plan.match_count += 1;
                if plan.preview.len() < request.preview_limit {
                    plan.preview.push(PreviewMatch {
                        range: Utf8Range { start, end },
                        replacement: request.replacement.to_string(),
                    });
                } else {
                    plan.truncated_preview = true;
                }
            }
        }
        CompiledSearch::Regex { regex, whole_word } => {
            for captures in regex.captures_iter(text) {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(SearchError::Cancelled);
                }
                if plan.edits.len() >= request.max_edits {
                    return Err(SearchError::resource(format!(
                        "replacement would create more than {} edits",
                        request.max_edits
                    )));
                }
                let Some(whole) = captures.get(0) else {
                    continue;
                };
                let start = whole.start() as u64;
                let end = whole.end() as u64;
                if *whole_word && !crate::matcher::whole_word_boundaries(text, start, end) {
                    continue;
                }
                let replacement = match &program {
                    Some(program) => compiled.expand(program, &captures),
                    // A literal search may compile to a regex for Unicode case
                    // folding, but its replacement is still copied verbatim.
                    None => request.replacement.to_string(),
                };
                plan.edits
                    .push(TextEdit::new(Utf8Range { start, end }, replacement.clone()));
                plan.match_count += 1;
                if plan.preview.len() < request.preview_limit {
                    plan.preview.push(PreviewMatch {
                        range: Utf8Range { start, end },
                        replacement,
                    });
                } else {
                    plan.truncated_preview = true;
                }
            }
        }
    }

    Ok(plan)
}

/// Applies already validated, non-overlapping edits.
pub fn apply_edits(text: &str, edits: &[TextEdit]) -> Result<String, SearchError> {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| edit.range.start);
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for edit in sorted {
        let start = edit.range.start as usize;
        let end = edit.range.end as usize;
        if start < cursor || end > text.len() || start > end {
            return Err(SearchError::invalid(format!(
                "edit {} overlaps or is out of bounds",
                edit.range
            )));
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return Err(SearchError::invalid(format!(
                "edit {} does not fall on UTF-8 boundaries",
                edit.range
            )));
        }
        output.push_str(&text[cursor..start]);
        output.push_str(&edit.inserted_utf8);
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

impl From<ReplacementError> for SearchError {
    fn from(error: ReplacementError) -> Self {
        SearchError::InvalidPattern {
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::{Pattern, SearchSpec};
    use notepust_contracts::SearchOptions;

    fn cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn literal_replacement_copies_text_verbatim() {
        let spec = SearchSpec::literal("ab");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "$1 \\n",
            interpret_escapes: true,
            preview_limit: 10,
            max_edits: 100,
        };
        let plan = plan_replace_all("ab ab", &request, &cancel()).unwrap();
        assert_eq!(plan.edits.len(), 2);
        assert_eq!(plan.edits[0].inserted_utf8, "$1 \\n");
    }

    #[test]
    fn regex_replacement_expands_groups() {
        let spec = SearchSpec::regex(r"(\w+), (\w+)");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "$2 $1",
            interpret_escapes: false,
            preview_limit: 10,
            max_edits: 100,
        };
        let plan = plan_replace_all("Doe, John", &request, &cancel()).unwrap();
        assert_eq!(plan.match_count, 1);
        let output = apply_edits("Doe, John", &plan.edits).unwrap();
        assert_eq!(output, "John Doe");
    }

    #[test]
    fn replacement_newline_escape_is_explicit() {
        let spec = SearchSpec::regex(r",");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: r",\n",
            interpret_escapes: true,
            preview_limit: 10,
            max_edits: 100,
        };
        let plan = plan_replace_all("a,b", &request, &cancel()).unwrap();
        assert_eq!(plan.edits[0].inserted_utf8, ",\n");
    }

    #[test]
    fn unknown_capture_group_fails_before_editing() {
        let spec = SearchSpec::regex(r"(a)");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "$2",
            interpret_escapes: false,
            preview_limit: 10,
            max_edits: 100,
        };
        assert!(matches!(
            plan_replace_all("a", &request, &cancel()),
            Err(SearchError::InvalidPattern { .. })
        ));
    }

    #[test]
    fn zero_width_replacements_terminate() {
        let spec = SearchSpec {
            pattern: Pattern::Regex {
                pattern: "^".to_string(),
                advanced: false,
            },
            options: SearchOptions::default(),
        };
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "-",
            interpret_escapes: false,
            preview_limit: 10,
            max_edits: 100,
        };
        let plan = plan_replace_all("a\nb\n", &request, &cancel()).unwrap();
        assert_eq!(plan.edits.len(), 3);
        let output = apply_edits("a\nb\n", &plan.edits).unwrap();
        assert_eq!(output, "-a\n-b\n-");
    }

    #[test]
    fn replacement_can_delete_text() {
        let spec = SearchSpec::literal("x");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "",
            interpret_escapes: false,
            preview_limit: 10,
            max_edits: 100,
        };
        let plan = plan_replace_all("axbxc", &request, &cancel()).unwrap();
        assert_eq!(apply_edits("axbxc", &plan.edits).unwrap(), "abc");
    }

    #[test]
    fn preview_limit_does_not_limit_replacement() {
        let spec = SearchSpec::literal("a");
        let request = ReplaceRequest {
            spec: &spec,
            replacement: "b",
            interpret_escapes: false,
            preview_limit: 2,
            max_edits: 100,
        };
        let plan = plan_replace_all("aaaa", &request, &cancel()).unwrap();
        assert_eq!(plan.match_count, 4);
        assert_eq!(plan.preview.len(), 2);
        assert!(plan.truncated_preview);
        assert_eq!(apply_edits("aaaa", &plan.edits).unwrap(), "bbbb");
    }

    #[test]
    fn apply_rejects_non_boundary_edits() {
        let edit = TextEdit::new(Utf8Range::new(1u64, 2u64).unwrap(), "x");
        assert!(apply_edits("中", &[edit]).is_err());
    }
}
