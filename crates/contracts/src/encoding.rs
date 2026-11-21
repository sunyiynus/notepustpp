use serde::{Deserialize, Serialize};

/// Encodings the application can name explicitly.
///
/// The id is what is persisted. The actual mapping table version is recorded in
/// [`EncodingSpec::mapping_version`] so a library upgrade cannot silently claim
/// byte-for-byte compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncodingId {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gbk,
    Gb18030,
    Windows1252,
    ShiftJis,
    Big5,
    EucJp,
    Iso2022Jp,
}

impl EncodingId {
    pub const ALL: [Self; 10] = [
        Self::Utf8,
        Self::Utf16Le,
        Self::Utf16Be,
        Self::Gbk,
        Self::Gb18030,
        Self::Windows1252,
        Self::ShiftJis,
        Self::Big5,
        Self::EucJp,
        Self::Iso2022Jp,
    ];

    /// Human-readable name shown in the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Gbk => "GBK",
            Self::Gb18030 => "GB18030",
            Self::Windows1252 => "Windows-1252",
            Self::ShiftJis => "Shift-JIS",
            Self::Big5 => "Big5",
            Self::EucJp => "EUC-JP",
            Self::Iso2022Jp => "ISO-2022-JP",
        }
    }

    /// Label understood by `encoding_rs` for non-UTF-16 encodings.
    pub const fn whatwg_label(self) -> Option<&'static str> {
        match self {
            Self::Utf8 => Some("utf-8"),
            Self::Utf16Le | Self::Utf16Be => None,
            Self::Gbk => Some("gbk"),
            Self::Gb18030 => Some("gb18030"),
            Self::Windows1252 => Some("windows-1252"),
            Self::ShiftJis => Some("shift_jis"),
            Self::Big5 => Some("big5"),
            Self::EucJp => Some("euc-jp"),
            Self::Iso2022Jp => Some("iso-2022-jp"),
        }
    }

    pub const fn is_unicode(self) -> bool {
        matches!(self, Self::Utf8 | Self::Utf16Le | Self::Utf16Be)
    }

    /// Encodings that may be restarted from a codec-safe checkpoint without
    /// decoding from the beginning of the file.
    pub const fn supports_random_access(self) -> bool {
        matches!(
            self,
            Self::Utf8 | Self::Utf16Le | Self::Utf16Be | Self::Gbk | Self::Gb18030
        )
    }

    /// The byte order mark that unambiguously belongs to this encoding.
    pub const fn canonical_bom(self) -> BomPolicy {
        match self {
            Self::Utf8 => BomPolicy::Utf8,
            Self::Utf16Le => BomPolicy::Utf16Le,
            Self::Utf16Be => BomPolicy::Utf16Be,
            _ => BomPolicy::None,
        }
    }
}

impl std::fmt::Display for EncodingId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The byte order mark stored at the start of the file.
///
/// A BOM is file metadata, never the first character of the editor text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BomPolicy {
    #[default]
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl BomPolicy {
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => &[],
            Self::Utf8 => &[0xEF, 0xBB, 0xBF],
            Self::Utf16Le => &[0xFF, 0xFE],
            Self::Utf16Be => &[0xFE, 0xFF],
        }
    }

    pub const fn len(self) -> usize {
        self.bytes().len()
    }

    pub const fn is_empty(self) -> bool {
        self.bytes().is_empty()
    }

    pub const fn is_bom(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Utf8 => "UTF-8 BOM",
            Self::Utf16Le => "UTF-16 LE BOM",
            Self::Utf16Be => "UTF-16 BE BOM",
        }
    }
}

/// How unchanged text is turned back into bytes on save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteWritePolicy {
    /// If the text and encoding metadata are unchanged, write the retained
    /// original source bytes so non-canonical sequences round-trip exactly.
    #[default]
    PreserveSourceIfUnchanged,
    /// Always encode the current text, even if it would look like a no-op.
    Reencode,
}

/// Complete serialization description for one document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodingSpec {
    pub encoding: EncodingId,
    pub mapping_version: String,
    pub bom: BomPolicy,
    pub write_policy: ByteWritePolicy,
}

impl EncodingSpec {
    pub const MAPPING_VERSION: &'static str = "whatwg-encoding_rs-0.8";

    pub fn new(encoding: EncodingId, bom: BomPolicy) -> Self {
        Self {
            encoding,
            mapping_version: Self::MAPPING_VERSION.to_string(),
            bom,
            write_policy: ByteWritePolicy::PreserveSourceIfUnchanged,
        }
    }

    pub fn utf8() -> Self {
        Self::new(EncodingId::Utf8, BomPolicy::None)
    }

    pub fn with_write_policy(mut self, policy: ByteWritePolicy) -> Self {
        self.write_policy = policy;
        self
    }

    /// BOMs are only legal for the encoding they encode.
    pub fn bom_is_valid(&self) -> bool {
        match self.bom {
            BomPolicy::None => true,
            BomPolicy::Utf8 => self.encoding == EncodingId::Utf8,
            BomPolicy::Utf16Le => self.encoding == EncodingId::Utf16Le,
            BomPolicy::Utf16Be => self.encoding == EncodingId::Utf16Be,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.bom_is_valid() {
            return Err(format!(
                "BOM {} is not valid for {}",
                self.bom.label(),
                self.encoding.label()
            ));
        }
        if self.mapping_version.trim().is_empty() {
            return Err("mapping_version must describe the codec mapping".to_string());
        }
        Ok(())
    }

    /// A human-readable summary for the status bar.
    pub fn status_label(&self) -> String {
        if self.bom.is_bom() {
            format!("{} ({})", self.encoding.label(), self.bom.label())
        } else {
            self.encoding.label().to_string()
        }
    }
}

impl Default for EncodingSpec {
    fn default() -> Self {
        Self::utf8()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_must_match_encoding() {
        assert!(EncodingSpec::new(EncodingId::Utf8, BomPolicy::Utf8).bom_is_valid());
        assert!(!EncodingSpec::new(EncodingId::Utf8, BomPolicy::Utf16Le).bom_is_valid());
        assert!(EncodingSpec::new(EncodingId::Gbk, BomPolicy::None).bom_is_valid());
        assert!(!EncodingSpec::new(EncodingId::Gbk, BomPolicy::Utf8).bom_is_valid());
    }

    #[test]
    fn bom_policies_roundtrip_through_json() {
        let spec = EncodingSpec::new(EncodingId::Utf16Le, BomPolicy::Utf16Le);
        let json = serde_json::to_string(&spec).unwrap();
        let back: EncodingSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
