use serde::{Deserialize, Serialize};

macro_rules! offset_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            pub const ZERO: Self = Self(0);

            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn value(self) -> u64 {
                self.0
            }

            pub const fn saturating_add(self, amount: u64) -> Self {
                Self(self.0.saturating_add(amount))
            }

            pub const fn saturating_sub(self, amount: u64) -> Self {
                Self(self.0.saturating_sub(amount))
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

offset_type!(
    /// Byte offset in the authoritative, un-normalized UTF-8 editor text.
    Utf8Offset
);
offset_type!(
    /// Byte offset in the original encoded file, including any BOM.
    RawByteOffset
);
offset_type!(
    /// UTF-16 code unit position, used only at platform/protocol edges.
    Utf16Position
);
offset_type!(
    /// Column measured in terminal/display cells, never bytes or UTF-16 units.
    DisplayColumn
);

/// Half-open UTF-8 byte range: `start <= end`, `start` is included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Utf8Range {
    pub start: u64,
    pub end: u64,
}

impl Utf8Range {
    pub const EMPTY: Self = Self { start: 0, end: 0 };

    /// Creates a range, rejecting reversed ranges.
    pub fn new(start: impl Into<Utf8Offset>, end: impl Into<Utf8Offset>) -> Option<Self> {
        let start = start.into().0;
        let end = end.into().0;
        (start <= end).then_some(Self { start, end })
    }

    /// Creates a range after clamping `start` and `end` into `0..=len`.
    pub fn clamped(start: u64, end: u64, len: u64) -> Self {
        let start = start.min(len);
        let end = end.min(len).max(start);
        Self { start, end }
    }

    pub const fn start_offset(self) -> Utf8Offset {
        Utf8Offset(self.start)
    }

    pub const fn end_offset(self) -> Utf8Offset {
        Utf8Offset(self.end)
    }

    pub const fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, offset: Utf8Offset) -> bool {
        offset.0 >= self.start && offset.0 < self.end
    }

    pub const fn contains_range(self, other: Self) -> bool {
        other.start >= self.start && other.end <= self.end
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn checked_slice(self, text: &str) -> Option<&str> {
        let start = usize::try_from(self.start).ok()?;
        let end = usize::try_from(self.end).ok()?;
        text.get(start..end)
    }

    pub const fn as_range(self) -> std::ops::Range<u64> {
        self.start..self.end
    }
}

impl From<std::ops::Range<u64>> for Utf8Range {
    fn from(range: std::ops::Range<u64>) -> Self {
        Self {
            start: range.start,
            end: range.end.max(range.start),
        }
    }
}

impl From<std::ops::Range<usize>> for Utf8Range {
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start as u64,
            end: range.end.max(range.start) as u64,
        }
    }
}

impl From<Utf8Range> for std::ops::Range<usize> {
    fn from(range: Utf8Range) -> Self {
        range.start as usize..range.end as usize
    }
}

impl std::fmt::Display for Utf8Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Half-open range of raw, still-encoded file bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawByteRange {
    pub start: u64,
    pub end: u64,
}

impl RawByteRange {
    pub fn new(start: impl Into<RawByteOffset>, end: impl Into<RawByteOffset>) -> Option<Self> {
        let start = start.into().0;
        let end = end.into().0;
        (start <= end).then_some(Self { start, end })
    }

    pub const fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, offset: RawByteOffset) -> bool {
        offset.0 >= self.start && offset.0 < self.end
    }
}

/// Zero-based logical line and grapheme column.
///
/// Status bar displays convert to one-based values. Platform/LSP UTF-16
/// columns are a separate coordinate and must not reuse this type.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct LogicalPosition {
    pub line: u64,
    pub column: u64,
}

impl LogicalPosition {
    pub const fn new(line: u64, column: u64) -> Self {
        Self { line, column }
    }

    pub const fn one_based(self) -> (u64, u64) {
        (self.line.saturating_add(1), self.column.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_validate_and_measure() {
        assert!(Utf8Range::new(5u64, 3u64).is_none());
        let range = Utf8Range::new(3u64, 5u64).unwrap();
        assert_eq!(range.len(), 2);
        assert!(range.contains(Utf8Offset(3)));
        assert!(!range.contains(Utf8Offset(5)));
        assert!(range.overlaps(Utf8Range::new(4u64, 9u64).unwrap()));
        assert!(!range.overlaps(Utf8Range::new(5u64, 9u64).unwrap()));
    }

    #[test]
    fn offsets_do_not_mix_by_accident() {
        let utf8 = Utf8Offset(10);
        let raw = RawByteOffset(20);
        assert_eq!(utf8.value() + raw.value(), 30);
    }
}
