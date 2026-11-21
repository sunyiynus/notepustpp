use serde::{Deserialize, Serialize};

use crate::encoding::EncodingSpec;
use crate::errors::OperationError;
use crate::ids::{DocRevision, DocumentId, TransactionId};
use crate::offsets::Utf8Range;

/// One non-overlapping replacement against a validated revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Utf8Range,
    pub inserted_utf8: String,
}

impl TextEdit {
    pub fn new(range: Utf8Range, inserted_utf8: impl Into<String>) -> Self {
        Self {
            range,
            inserted_utf8: inserted_utf8.into(),
        }
    }
}

/// Selection endpoint with direction preserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRange {
    pub anchor: u64,
    pub head: u64,
}

impl SelectionRange {
    pub const fn new(anchor: u64, head: u64) -> Self {
        Self { anchor, head }
    }

    pub const fn caret(offset: u64) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn normalized(self) -> Utf8Range {
        Utf8Range {
            start: self.anchor.min(self.head),
            end: self.anchor.max(self.head),
        }
    }
}

/// A user-visible edit operation committed as one undo transaction.
///
/// All ranges refer to the same `expected_revision`. The adapter validates the
/// entire batch before applying anything, then commits from the highest offset
/// to the lowest. An empty `edits` list with a `format_change` is a pure
/// serialization change and still advances the revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditBatch {
    pub document_id: DocumentId,
    pub expected_revision: DocRevision,
    pub transaction_id: TransactionId,
    pub command_id: String,
    pub edits: Vec<TextEdit>,
    pub format_change: Option<EncodingSpec>,
    pub selection_after: Option<Vec<SelectionRange>>,
    pub undo_label: String,
}

impl EditBatch {
    pub fn new(
        document_id: DocumentId,
        expected_revision: DocRevision,
        command_id: impl Into<String>,
    ) -> Self {
        Self {
            document_id,
            expected_revision,
            transaction_id: TransactionId::new(),
            command_id: command_id.into(),
            edits: Vec::new(),
            format_change: None,
            selection_after: None,
            undo_label: String::new(),
        }
    }

    pub fn push(&mut self, edit: TextEdit) {
        self.edits.push(edit);
    }

    /// Validates byte ordering and overlap. UTF-8 character-boundary checks are
    /// performed by the editor adapter, which owns the text.
    pub fn validate(&self, utf8_len: u64) -> Result<(), OperationError> {
        if let Some(spec) = &self.format_change {
            spec.validate()
                .map_err(|detail| OperationError::InvalidEncoding { detail })?;
        }

        let mut sorted: Vec<&TextEdit> = self.edits.iter().collect();
        sorted.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut previous_end = 0u64;
        for (index, edit) in sorted.iter().enumerate() {
            if edit.range.start > edit.range.end {
                return Err(OperationError::invalid(format!(
                    "edit {index} has reversed range {}",
                    edit.range
                )));
            }
            if edit.range.end > utf8_len {
                return Err(OperationError::invalid(format!(
                    "edit {index} ends at {} beyond document length {utf8_len}",
                    edit.range.end
                )));
            }
            if index > 0 && edit.range.start < previous_end {
                return Err(OperationError::invalid(format!(
                    "edit {index} overlaps the previous edit at {}",
                    edit.range.start
                )));
            }
            previous_end = edit.range.end;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.format_change.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(edits: Vec<TextEdit>) -> EditBatch {
        let mut batch = EditBatch::new(DocumentId::new(), DocRevision::new(3), "test");
        batch.edits = edits;
        batch
    }

    #[test]
    fn rejects_overlapping_edits() {
        let batch = batch(vec![
            TextEdit::new(Utf8Range::new(0u64, 4u64).unwrap(), "a"),
            TextEdit::new(Utf8Range::new(3u64, 6u64).unwrap(), "b"),
        ]);
        assert!(batch.validate(10).is_err());
    }

    #[test]
    fn accepts_adjacent_and_empty_edits() {
        let batch = batch(vec![
            TextEdit::new(Utf8Range::new(0u64, 4u64).unwrap(), "a"),
            TextEdit::new(Utf8Range::new(4u64, 4u64).unwrap(), "b"),
            TextEdit::new(Utf8Range::new(4u64, 8u64).unwrap(), "c"),
        ]);
        assert!(batch.validate(8).is_ok());
    }

    #[test]
    fn rejects_out_of_bounds() {
        let batch = batch(vec![TextEdit::new(
            Utf8Range::new(8u64, 12u64).unwrap(),
            "x",
        )]);
        assert!(batch.validate(10).is_err());
    }
}
