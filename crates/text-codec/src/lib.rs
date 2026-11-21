//! Strict text decoding, encoding, EOL handling and coordinate conversion.
//!
//! Design rules enforced here:
//! - Decoding never inserts U+FFFD; malformed input is an error with a raw byte
//!   offset.
//! - The authoritative editor text keeps its actual CRLF/LF/CR bytes. BOMs are
//!   file metadata.
//! - Raw byte offsets, UTF-8 offsets, UTF-16 positions and display columns are
//!   distinct coordinate spaces.

pub mod coords;
pub mod encoding;
pub mod eol;
pub mod error;
pub mod stream;

use notepust_contracts::{BomPolicy, EncodingId, EncodingSpec};

pub use encoding::{
    RoundTripClass, classify_round_trip, decode_detected, decode_explicit, decode_payload,
    detect_candidate, encode_text, encoding_rs_encoding, sniff_bom, strip_bom,
};
pub use eol::{
    EolKind, EolSummary, EolToken, LineIndex, LogicalLine, convert_eols, default_eol,
    logical_lines, scan_eols, summarize,
};
pub use error::CodecError;
pub use stream::{CharSpan, DecodedChunk, SpanDecoded, StreamingDecoder, decode_with_spans};

/// How the decoder chose the encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionSource {
    /// The user explicitly selected the encoding.
    Explicit,
    /// A BOM was present and authoritative.
    Bom,
    /// The bytes were valid UTF-8, which the product treats as decisive.
    StrictUtf8,
    /// A chardetng candidate was accepted. The UI must show that this was a
    /// guess before destructive operations.
    Guessed,
}

impl DetectionSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Explicit => "selected encoding",
            Self::Bom => "BOM",
            Self::StrictUtf8 => "valid UTF-8",
            Self::Guessed => "detected encoding",
        }
    }
}

/// A completely decoded source file.
#[derive(Clone, Debug)]
pub struct DecodedFile {
    pub text: String,
    pub spec: EncodingSpec,
    pub source: DetectionSource,
    pub round_trip: RoundTripClass,
}

impl DecodedFile {
    pub fn has_bom(&self) -> bool {
        self.spec.bom != BomPolicy::None
    }

    pub fn line_index(&self) -> LineIndex {
        LineIndex::new(&self.text)
    }
}

/// Encodes text for display in an error or diagnostic without changing the
/// authoritative content.
pub fn lossy_preview(bytes: &[u8], limit: usize) -> String {
    let end = limit.min(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The encoding candidates shown to a user when auto-detection cannot be
/// trusted. Strict UTF-8 and BOMs are handled before this list is consulted.
pub fn candidate_encodings() -> &'static [EncodingId] {
    &[
        EncodingId::Utf8,
        EncodingId::Utf16Le,
        EncodingId::Utf16Be,
        EncodingId::Gbk,
        EncodingId::Gb18030,
        EncodingId::Windows1252,
        EncodingId::Big5,
        EncodingId::ShiftJis,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_gbk_is_marked_as_guess() {
        // "中文" in GBK.
        let bytes = [0xD6, 0xD0, 0xCE, 0xC4];
        let decoded = decode_detected(&bytes, true).unwrap();
        assert_eq!(decoded.text, "中文");
        assert!(matches!(
            decoded.source,
            DetectionSource::Guessed | DetectionSource::StrictUtf8
        ));
    }

    #[test]
    fn deterministic_truncated_utf8_reports_offset() {
        let bytes = b"abc\xE4\xB8";
        let decoded = decode_detected(bytes, false);
        assert!(matches!(
            decoded,
            Err(CodecError::Malformed { .. } | CodecError::EncodingChoiceRequired { .. })
        ));
    }
}
