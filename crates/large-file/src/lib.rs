//! On-demand large-file reader, sparse line index, viewport and tail state.
//!
//! A 1 GB log is never loaded into the editor buffer. Raw line boundaries are
//! found without decoding, while visible lines are decoded strictly and
//! bounded. Random-access support is limited to UTF-8, UTF-16 LE/BE, GBK and
//! GB18030.

pub mod index;
pub mod read_at;
pub mod reader;
pub mod scan;
pub mod search;

pub use index::{IndexCheckpoint, LINE_CHECKPOINT_INTERVAL, LineIndex};
pub use read_at::FileReader;
pub use reader::{
    INDEX_CHUNK_BYTES, IndexProgress, LogError, LogEvent, LogLine, LogReader, MAX_VIEW_LINE_BYTES,
    TailView, Viewport, decode_window,
};
pub use scan::{RawLine, RawScan, scan_raw_lines};
pub use search::{
    LogSearchHit, LogSearchOptions, LogSearchResult, MAX_SEARCH_PREVIEW_CHARS, SEARCH_CHUNK_BYTES,
};
