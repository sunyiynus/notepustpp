use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_id!(
    /// Stable identity of an open document, independent of its path.
    DocumentId
);
uuid_id!(
    /// Identity of one view (selection/scroll/focus owner) of a document.
    ViewId
);
uuid_id!(
    /// Identity of one background job.
    JobId
);
uuid_id!(
    /// Identity of one editable transaction.
    TransactionId
);
uuid_id!(
    /// Identity of one immutable text snapshot.
    SnapshotId
);
uuid_id!(
    /// Identity of one immutable raw-source byte snapshot.
    SourceSnapshotId
);
uuid_id!(
    /// Identity of one search query as seen by the UI.
    QueryId
);

/// Monotonically increasing revision of editable document state.
///
/// It is advanced by committed text changes and by metadata changes that alter
/// the bytes written on save (encoding, BOM, write policy). Undo and redo also
/// advance it. A revision is *not* a dirty flag: undoing back to the saved
/// content must clear dirty even though the revision is newer.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DocRevision(u64);

impl DocRevision {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl From<u64> for DocRevision {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for DocRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_unique_and_serde_transparent() {
        let a = DocumentId::new();
        let b = DocumentId::new();
        assert_ne!(a, b);

        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, format!("\"{a}\""));
        let back: DocumentId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn revision_advances_without_overflowing() {
        assert_eq!(DocRevision::new(1).next(), DocRevision::new(2));
        assert_eq!(DocRevision::new(u64::MAX).next().value(), u64::MAX);
    }
}
