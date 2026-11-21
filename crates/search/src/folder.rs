use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use notepust_contracts::{EncodingId, EncodingSpec, NativePath, Utf8Range};
use notepust_text_codec::{CodecError, LineIndex, decode_detected, decode_explicit};

use crate::error::SearchError;
use crate::matcher::{SearchSpec, compile};

/// One shared filter policy for the file tree, quick open and search. The
/// callers may present the same rules differently, but they interpret them
/// with this type.
#[derive(Clone, Debug)]
pub struct WorkspaceFilterPolicy {
    pub include_hidden: bool,
    pub respect_ignore_files: bool,
    pub follow_links: bool,
    pub user_excludes: Vec<String>,
    pub includes: Vec<String>,
    pub max_file_bytes: u64,
    /// Explicit fallback for legacy encodings. `None` means only BOM or strict
    /// UTF-8 files are searched automatically.
    pub fallback_encoding: Option<EncodingId>,
    pub allow_guess_encoding: bool,
}

impl Default for WorkspaceFilterPolicy {
    fn default() -> Self {
        Self {
            // .env and other dotfiles are meaningful configuration files.
            include_hidden: true,
            respect_ignore_files: true,
            follow_links: false,
            user_excludes: vec![
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string(),
                "build".to_string(),
            ],
            includes: Vec::new(),
            max_file_bytes: 100 * 1024 * 1024,
            fallback_encoding: None,
            allow_guess_encoding: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    TooLarge,
    Binary,
    EncodingChoiceRequired,
    DecodeError(String),
    PermissionDenied,
    IdentityDuplicate,
    Io(String),
    WalkError(String),
}

#[derive(Clone, Debug)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Clone, Debug)]
pub struct FolderMatch {
    pub path: PathBuf,
    pub native_path: NativePath,
    pub range: Utf8Range,
    /// One-based line number.
    pub line: u64,
    pub snippet: String,
}

#[derive(Clone, Debug, Default)]
pub struct FolderSearchReport {
    pub matches: Vec<FolderMatch>,
    pub skipped: Vec<SkippedFile>,
    pub files_searched: u64,
    pub files_skipped: u64,
    pub truncated: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub struct FolderSearchRequest {
    pub root: PathBuf,
    pub spec: SearchSpec,
    pub policy: WorkspaceFilterPolicy,
    pub max_results: usize,
}

/// Searches a folder tree without blocking the UI.
pub fn search_folder(
    request: &FolderSearchRequest,
    cancel: &AtomicBool,
) -> Result<FolderSearchReport, SearchError> {
    let compiled = compile(&request.spec)?;
    let rules = RuleSet::new(&request.policy)?;
    let mut report = FolderSearchReport::default();
    let mut seen_identities = HashSet::new();

    let root = request.root.clone();
    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(!request.policy.include_hidden)
        .follow_links(request.policy.follow_links)
        .require_git(false)
        .git_global(false)
        .parents(false)
        .ignore(request.policy.respect_ignore_files)
        .git_ignore(request.policy.respect_ignore_files)
        .git_exclude(request.policy.respect_ignore_files);

    let filter_root = root.clone();
    let filter_rules = rules.clone();
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        let relative = relative_display(&filter_root, entry.path());
        !filter_rules.excluded(&relative, is_dir)
    });

    for entry in builder.build() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.skipped.push(SkippedFile {
                    path: root.clone(),
                    reason: SkipReason::WalkError(error.to_string()),
                });
                report.files_skipped += 1;
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let relative = relative_display(&root, entry.path());
        if !rules.included(&relative) {
            continue;
        }
        let path = entry.path().to_path_buf();

        let identity = notepust_platform::identity_of_path(&path).ok().flatten();
        if let Some(identity) = identity
            && !seen_identities.insert(identity)
        {
            report.skipped.push(SkippedFile {
                path,
                reason: SkipReason::IdentityDuplicate,
            });
            report.files_skipped += 1;
            continue;
        }

        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.skipped.push(SkippedFile {
                    path,
                    reason: if error.kind() == std::io::ErrorKind::PermissionDenied {
                        SkipReason::PermissionDenied
                    } else {
                        SkipReason::Io(error.to_string())
                    },
                });
                report.files_skipped += 1;
                continue;
            }
        };
        if metadata.len() > request.policy.max_file_bytes {
            report.skipped.push(SkippedFile {
                path,
                reason: SkipReason::TooLarge,
            });
            report.files_skipped += 1;
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.skipped.push(SkippedFile {
                    path,
                    reason: SkipReason::Io(error.to_string()),
                });
                report.files_skipped += 1;
                continue;
            }
        };
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            report.skipped.push(SkippedFile {
                path,
                reason: SkipReason::Binary,
            });
            report.files_skipped += 1;
            continue;
        }

        let decoded = match decode_folder_file(&bytes, &request.policy) {
            Ok(decoded) => decoded,
            Err(reason) => {
                report.skipped.push(SkippedFile { path, reason });
                report.files_skipped += 1;
                continue;
            }
        };
        report.files_searched += 1;
        let text = decoded.text;
        let index = LineIndex::new(&text);
        let remaining = request
            .max_results
            .saturating_sub(report.matches.len())
            .max(1);
        let outcome = match compiled.find_all(&text, remaining, false, cancel) {
            Ok(outcome) => outcome,
            Err(SearchError::Cancelled) => {
                report.cancelled = true;
                break;
            }
            Err(error) => return Err(error),
        };
        for hit in outcome.hits {
            let line_index = index.line_at_offset(hit.range.start);
            let line = (line_index as u64).saturating_add(1);
            let snippet = snippet_for(&text, &index, hit.range);
            report.matches.push(FolderMatch {
                path: path.clone(),
                native_path: NativePath::from_path(&path),
                range: hit.range,
                line,
                snippet,
            });
        }
        if outcome.truncated || report.matches.len() >= request.max_results {
            report.truncated = true;
            break;
        }
    }

    if report.matches.len() > request.max_results {
        report.matches.truncate(request.max_results);
        report.truncated = true;
    }
    Ok(report)
}

fn decode_folder_file(
    bytes: &[u8],
    policy: &WorkspaceFilterPolicy,
) -> Result<notepust_text_codec::DecodedFile, SkipReason> {
    // BOM and strict UTF-8 are decisive and win over a legacy fallback.
    match decode_detected(bytes, false) {
        Ok(decoded) => return Ok(decoded),
        Err(CodecError::EncodingChoiceRequired { .. }) => {}
        Err(error) => {
            if policy.fallback_encoding.is_none() {
                return Err(SkipReason::DecodeError(error.to_string()));
            }
        }
    }
    if let Some(encoding) = policy.fallback_encoding {
        let spec = EncodingSpec::new(encoding, notepust_contracts::BomPolicy::None);
        return decode_explicit(bytes, &spec)
            .map_err(|error| SkipReason::DecodeError(error.to_string()));
    }
    if policy.allow_guess_encoding {
        return decode_detected(bytes, true)
            .map_err(|error| SkipReason::DecodeError(error.to_string()));
    }
    Err(SkipReason::EncodingChoiceRequired)
}

fn snippet_for(text: &str, index: &LineIndex, range: Utf8Range) -> String {
    let Some(line) = index.line(index.line_at_offset(range.start)) else {
        return String::new();
    };
    let Some(content) = text.get(line.start as usize..line.content_end as usize) else {
        return String::new();
    };
    let line_start = line.start;
    let relative_start = range
        .start
        .saturating_sub(line_start)
        .min(content.len() as u64) as usize;
    let relative_end = range
        .end
        .saturating_sub(line_start)
        .min(content.len() as u64) as usize;
    let mut window_start = relative_start.saturating_sub(80);
    while window_start > 0 && !content.is_char_boundary(window_start) {
        window_start -= 1;
    }
    let mut window_end = (relative_end + 80).min(content.len());
    while window_end < content.len() && !content.is_char_boundary(window_end) {
        window_end += 1;
    }
    let mut snippet = String::new();
    if window_start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&content[window_start..window_end]);
    if window_end < content.len() {
        snippet.push('…');
    }
    if snippet.chars().count() > 240 {
        snippet = snippet.chars().take(240).collect();
        snippet.push('…');
    }
    snippet
}

#[derive(Clone)]
struct RuleSet {
    exclude_globs: GlobSet,
    exclude_names: HashSet<String>,
    include_globs: Option<GlobSet>,
}

impl RuleSet {
    fn new(policy: &WorkspaceFilterPolicy) -> Result<Self, SearchError> {
        let mut exclude_builder = GlobSetBuilder::new();
        let mut exclude_names = HashSet::new();
        for name in [".git", ".hg", ".svn"] {
            exclude_names.insert(name.to_string());
        }
        for pattern in &policy.user_excludes {
            if pattern.contains(['*', '?', '[', ']', '/', '\\']) {
                let glob = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|error| SearchError::invalid(error.to_string()))?;
                exclude_builder.add(glob);
            } else if !pattern.trim().is_empty() {
                exclude_names.insert(pattern.clone());
                let glob = GlobBuilder::new(&format!("**/{pattern}/**"))
                    .literal_separator(true)
                    .build()
                    .map_err(|error| SearchError::invalid(error.to_string()))?;
                exclude_builder.add(glob);
            }
        }
        let exclude_globs = exclude_builder
            .build()
            .map_err(|error| SearchError::invalid(error.to_string()))?;

        let include_globs = if policy.includes.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &policy.includes {
                let glob = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|error| SearchError::invalid(error.to_string()))?;
                builder.add(glob);
            }
            Some(
                builder
                    .build()
                    .map_err(|error| SearchError::invalid(error.to_string()))?,
            )
        };

        Ok(Self {
            exclude_globs,
            exclude_names,
            include_globs,
        })
    }

    fn excluded(&self, relative: &str, _is_dir: bool) -> bool {
        if relative.is_empty() {
            return false;
        }
        for component in relative.split('/') {
            if self.exclude_names.contains(component) {
                return true;
            }
        }
        self.exclude_globs.is_match(relative)
    }

    fn included(&self, relative: &str) -> bool {
        if self.excluded(relative, false) {
            return false;
        }
        match &self.include_globs {
            Some(globs) => globs.is_match(relative),
            None => true,
        }
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::SearchSpec;
    use notepust_contracts::SearchOptions;
    use std::sync::atomic::AtomicBool;

    fn cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn finds_files_respects_gitignore_and_hidden_policy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=needle\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules").join("x.js"), "needle\n").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "one\nneedle two\n").unwrap();

        let report = search_folder(
            &FolderSearchRequest {
                root: dir.path().to_path_buf(),
                spec: SearchSpec {
                    pattern: crate::matcher::Pattern::Literal("needle".to_string()),
                    options: SearchOptions::default(),
                },
                policy: WorkspaceFilterPolicy::default(),
                max_results: 100,
            },
            &cancel(),
        )
        .unwrap();

        let paths: Vec<String> = report
            .matches
            .iter()
            .map(|hit| hit.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(paths.contains(&".env".to_string()));
        assert!(paths.contains(&"keep.txt".to_string()));
        assert!(!paths.contains(&"ignored.txt".to_string()));
        assert!(!paths.contains(&"x.js".to_string()));
        assert!(
            report
                .matches
                .iter()
                .any(|hit| hit.line == 2 && hit.snippet.contains("needle"))
        );
    }

    #[test]
    fn skips_binary_and_undecodable_files_with_reasons() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("binary.bin"), [0u8, 1, 2, 3]).unwrap();
        std::fs::write(dir.path().join("gbk.txt"), [0xD6, 0xD0, 0xCE, 0xC4]).unwrap();
        let report = search_folder(
            &FolderSearchRequest {
                root: dir.path().to_path_buf(),
                spec: SearchSpec::literal("中"),
                policy: WorkspaceFilterPolicy::default(),
                max_results: 10,
            },
            &cancel(),
        )
        .unwrap();
        assert!(report.matches.is_empty());
        assert!(
            report
                .skipped
                .iter()
                .any(|skip| skip.reason == SkipReason::Binary)
        );
        assert!(
            report
                .skipped
                .iter()
                .any(|skip| skip.reason == SkipReason::EncodingChoiceRequired)
        );
    }

    #[test]
    fn explicit_fallback_searches_legacy_encoding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gbk.txt"), [0xD6, 0xD0, 0xCE, 0xC4]).unwrap();
        let policy = WorkspaceFilterPolicy {
            fallback_encoding: Some(EncodingId::Gbk),
            ..Default::default()
        };
        let report = search_folder(
            &FolderSearchRequest {
                root: dir.path().to_path_buf(),
                spec: SearchSpec::literal("中"),
                policy,
                max_results: 10,
            },
            &cancel(),
        )
        .unwrap();
        assert_eq!(report.matches.len(), 1);
        assert_eq!(report.files_searched, 1);
    }

    #[test]
    fn result_limit_is_explicitly_truncated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("many.txt"), "x\n".repeat(50)).unwrap();
        let report = search_folder(
            &FolderSearchRequest {
                root: dir.path().to_path_buf(),
                spec: SearchSpec::literal("x"),
                policy: WorkspaceFilterPolicy::default(),
                max_results: 3,
            },
            &cancel(),
        )
        .unwrap();
        assert_eq!(report.matches.len(), 3);
        assert!(report.truncated);
    }

    #[test]
    fn invalid_glob_is_rejected_before_walking() {
        let dir = tempfile::tempdir().unwrap();
        let policy = WorkspaceFilterPolicy {
            includes: vec!["[".to_string()],
            ..Default::default()
        };
        let error = search_folder(
            &FolderSearchRequest {
                root: dir.path().to_path_buf(),
                spec: SearchSpec::literal("x"),
                policy,
                max_results: 10,
            },
            &cancel(),
        )
        .unwrap_err();
        assert!(matches!(error, SearchError::InvalidPattern { .. }));
    }
}
