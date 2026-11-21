//! Search, replacement planning and worker supervision.

pub mod error;
pub mod folder;
pub mod matcher;
pub mod plan;
pub mod replacement;
pub mod results;
pub mod supervisor;

pub use error::SearchError;
pub use folder::{
    FolderMatch, FolderSearchReport, FolderSearchRequest, SkipReason, SkippedFile,
    WorkspaceFilterPolicy, search_folder,
};
pub use matcher::{
    CompiledSearch, Pattern, SearchHit, SearchOutcome, SearchSpec, compile, find, is_word_char,
    whole_word_boundaries,
};
pub use plan::{PreviewMatch, ReplacePlan, ReplaceRequest, apply_edits, plan_replace_all};
pub use replacement::{ReplacementError, expand_parts, parse_replacement, validate_program};
pub use results::{ResultBudget, ResultCollector, split_batches};
pub use supervisor::{WorkerClient, WorkerRun};
