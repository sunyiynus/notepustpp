use encoding_rs::DecoderResult;
use notepust_contracts::{EncodingId, EncodingSpec};

use crate::encoding::{decode_payload, encoding_rs_encoding};
use crate::error::CodecError;

/// One decoded chunk with its position in both coordinate spaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedChunk {
    pub text: String,
    pub raw_start: u64,
    pub raw_end: u64,
    pub utf8_start: u64,
    pub utf8_end: u64,
    /// True only when the caller declared the stream finished.
    pub complete: bool,
}

enum Inner {
    EncodingRs {
        decoder: encoding_rs::Decoder,
        output: String,
    },
    Utf16 {
        little_endian: bool,
        /// Trailing bytes (0-3) that did not yet form a complete character.
        /// They have already been counted in `raw_position`.
        pending: Vec<u8>,
    },
}

/// Incremental, strict decoder used for large files and viewport windows.
///
/// The decoder never inserts replacement characters. An incomplete trailing
/// sequence stays buffered until the next chunk; only `finish` reports it as
/// malformed.
pub struct StreamingDecoder {
    spec: EncodingSpec,
    raw_position: u64,
    utf8_position: u64,
    inner: Inner,
}

impl StreamingDecoder {
    pub fn new(spec: &EncodingSpec) -> Result<Self, CodecError> {
        Self::new_at(spec, spec.bom.len() as u64)
    }

    /// Starts at `raw_position` inside the payload (after the BOM, if any).
    ///
    /// Callers may only restart at a codec-safe character boundary.
    pub fn new_at(spec: &EncodingSpec, raw_position: u64) -> Result<Self, CodecError> {
        let inner = match spec.encoding {
            EncodingId::Utf16Le | EncodingId::Utf16Be => Inner::Utf16 {
                little_endian: spec.encoding == EncodingId::Utf16Le,
                pending: Vec::new(),
            },
            encoding => {
                let codec =
                    encoding_rs_encoding(encoding).ok_or_else(|| CodecError::Unsupported {
                        encoding,
                        detail: "no encoding_rs mapping".to_string(),
                    })?;
                Inner::EncodingRs {
                    decoder: codec.new_decoder_without_bom_handling(),
                    output: String::new(),
                }
            }
        };
        Ok(Self {
            spec: spec.clone(),
            raw_position,
            utf8_position: 0,
            inner,
        })
    }

    pub fn spec(&self) -> &EncodingSpec {
        &self.spec
    }

    /// Absolute file offset of the next byte to be fed.
    pub fn raw_position(&self) -> u64 {
        self.raw_position
    }

    pub fn utf8_position(&self) -> u64 {
        self.utf8_position
    }

    /// Decodes one chunk. `last` must only be set at the true end of file.
    pub fn decode(&mut self, bytes: &[u8], last: bool) -> Result<DecodedChunk, CodecError> {
        let raw_start = self.raw_position;
        let utf8_start = self.utf8_position;
        let (text, consumed) = match &mut self.inner {
            Inner::EncodingRs { decoder, output } => {
                output.clear();
                let mut consumed_total = 0usize;
                loop {
                    if output.capacity() - output.len() < 4 {
                        let desired = (output.capacity().max(4096) * 2)
                            .min(bytes.len().saturating_mul(4).saturating_add(4096));
                        output.reserve(desired.max(4));
                    }
                    let (result, read) = decoder.decode_to_string_without_replacement(
                        &bytes[consumed_total..],
                        output,
                        last,
                    );
                    consumed_total += read;
                    match result {
                        DecoderResult::InputEmpty => break,
                        DecoderResult::OutputFull => continue,
                        DecoderResult::Malformed(length, _) => {
                            let offset =
                                raw_start + consumed_total.saturating_sub(length as usize) as u64;
                            return Err(CodecError::malformed(
                                self.spec.encoding,
                                offset,
                                "byte sequence is not valid in this encoding",
                            ));
                        }
                    }
                }
                if consumed_total < bytes.len() && !last {
                    // The decoder must buffer incomplete tails itself; a short
                    // read here would silently drop bytes.
                    return Err(CodecError::malformed(
                        self.spec.encoding,
                        raw_start + consumed_total as u64,
                        "decoder stopped before consuming the chunk",
                    ));
                }
                (std::mem::take(output), consumed_total)
            }
            Inner::Utf16 {
                little_endian,
                pending,
            } => decode_utf16_chunk(
                bytes,
                *little_endian,
                raw_start,
                pending,
                last,
                &self.spec.encoding,
            )?,
        };

        self.raw_position = raw_start + consumed as u64;
        self.utf8_position = utf8_start + text.len() as u64;
        Ok(DecodedChunk {
            text,
            raw_start,
            raw_end: self.raw_position,
            utf8_start,
            utf8_end: self.utf8_position,
            complete: last,
        })
    }

    /// Flushes buffered state at end of file.
    pub fn finish(&mut self) -> Result<DecodedChunk, CodecError> {
        self.decode(&[], true)
    }
}

fn decode_utf16_chunk(
    bytes: &[u8],
    little_endian: bool,
    raw_start: u64,
    pending: &mut Vec<u8>,
    last: bool,
    encoding: &EncodingId,
) -> Result<(String, usize), CodecError> {
    let pending_len = pending.len();
    let base = raw_start.saturating_sub(pending_len as u64);
    let mut data = Vec::with_capacity(pending_len + bytes.len());
    data.extend_from_slice(pending);
    data.extend_from_slice(bytes);
    pending.clear();

    let mut output = String::with_capacity(data.len() / 2 + 4);
    let mut index = 0usize;

    while index < data.len() {
        if index + 1 >= data.len() {
            if last {
                return Err(CodecError::malformed(
                    *encoding,
                    base + index as u64,
                    "odd number of bytes for UTF-16 at end of file",
                ));
            }
            pending.extend_from_slice(&data[index..]);
            break;
        }

        let unit = if little_endian {
            u16::from_le_bytes([data[index], data[index + 1]])
        } else {
            u16::from_be_bytes([data[index], data[index + 1]])
        };
        let unit_offset = base + index as u64;

        if (0xD800..=0xDBFF).contains(&unit) {
            if index + 3 >= data.len() {
                if last {
                    return Err(CodecError::malformed(
                        *encoding,
                        unit_offset,
                        format!("unpaired high surrogate 0x{unit:04X} at end of file"),
                    ));
                }
                pending.extend_from_slice(&data[index..]);
                break;
            }
            let low = if little_endian {
                u16::from_le_bytes([data[index + 2], data[index + 3]])
            } else {
                u16::from_be_bytes([data[index + 2], data[index + 3]])
            };
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(CodecError::malformed(
                    *encoding,
                    unit_offset + 2,
                    format!("high surrogate 0x{unit:04X} is not followed by a low surrogate"),
                ));
            }
            let scalar = 0x1_0000 + (((unit as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
            let character = char::from_u32(scalar).ok_or_else(|| {
                CodecError::malformed(*encoding, unit_offset, "invalid UTF-16 surrogate pair")
            })?;
            output.push(character);
            index += 4;
            continue;
        }

        if (0xDC00..=0xDFFF).contains(&unit) {
            return Err(CodecError::malformed(
                *encoding,
                unit_offset,
                format!("unpaired low surrogate 0x{unit:04X}"),
            ));
        }

        let character = char::from_u32(unit as u32).ok_or_else(|| {
            CodecError::malformed(*encoding, unit_offset, "invalid UTF-16 scalar")
        })?;
        output.push(character);
        index += 2;
    }

    // All fed bytes are counted as consumed even when a trailing character is
    // buffered; the buffered bytes were part of this call's input.
    Ok((output, bytes.len()))
}

/// One decoded character's source and output lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharSpan {
    pub raw_len: u32,
    pub utf8_len: u32,
}

/// A bounded window decoded with a per-character source map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanDecoded {
    pub text: String,
    pub spans: Vec<CharSpan>,
    pub raw_length: u64,
}

/// Decodes a bounded byte window while preserving the raw length of every
/// character. Intended for viewport/search windows, not whole files.
pub fn decode_with_spans(
    bytes: &[u8],
    encoding: EncodingId,
    raw_base: u64,
) -> Result<SpanDecoded, CodecError> {
    let mut text = String::with_capacity(bytes.len());
    let mut spans = Vec::with_capacity(bytes.len());
    let mut index = 0usize;

    match encoding {
        EncodingId::Utf8 => {
            while index < bytes.len() {
                let rest = &bytes[index..];
                let Ok(string) = std::str::from_utf8(rest) else {
                    // Find the valid prefix length for a precise error.
                    let valid = std::str::from_utf8(rest)
                        .err()
                        .map(|error| error.valid_up_to())
                        .unwrap_or(0);
                    if valid == 0 {
                        return Err(CodecError::malformed(
                            encoding,
                            raw_base + index as u64,
                            "invalid UTF-8",
                        ));
                    }
                    let prefix = std::str::from_utf8(&rest[..valid]).map_err(|_| {
                        CodecError::malformed(encoding, raw_base + index as u64, "invalid UTF-8")
                    })?;
                    for character in prefix.chars() {
                        text.push(character);
                        spans.push(CharSpan {
                            raw_len: character.len_utf8() as u32,
                            utf8_len: character.len_utf8() as u32,
                        });
                    }
                    index += valid;
                    continue;
                };
                for character in string.chars() {
                    text.push(character);
                    spans.push(CharSpan {
                        raw_len: character.len_utf8() as u32,
                        utf8_len: character.len_utf8() as u32,
                    });
                    index += character.len_utf8();
                }
            }
        }
        EncodingId::Utf16Le | EncodingId::Utf16Be => {
            let little_endian = encoding == EncodingId::Utf16Le;
            while index < bytes.len() {
                if index + 1 >= bytes.len() {
                    return Err(CodecError::malformed(
                        encoding,
                        raw_base + index as u64,
                        "odd number of bytes for UTF-16",
                    ));
                }
                let unit = if little_endian {
                    u16::from_le_bytes([bytes[index], bytes[index + 1]])
                } else {
                    u16::from_be_bytes([bytes[index], bytes[index + 1]])
                };
                if (0xD800..=0xDBFF).contains(&unit) {
                    if index + 3 >= bytes.len() {
                        return Err(CodecError::malformed(
                            encoding,
                            raw_base + index as u64,
                            "truncated surrogate pair",
                        ));
                    }
                    let low = if little_endian {
                        u16::from_le_bytes([bytes[index + 2], bytes[index + 3]])
                    } else {
                        u16::from_be_bytes([bytes[index + 2], bytes[index + 3]])
                    };
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(CodecError::malformed(
                            encoding,
                            raw_base + index as u64 + 2,
                            "high surrogate not followed by low surrogate",
                        ));
                    }
                    let scalar =
                        0x1_0000 + (((unit as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                    let character = char::from_u32(scalar).ok_or_else(|| {
                        CodecError::malformed(
                            encoding,
                            raw_base + index as u64,
                            "invalid surrogate pair",
                        )
                    })?;
                    text.push(character);
                    spans.push(CharSpan {
                        raw_len: 4,
                        utf8_len: character.len_utf8() as u32,
                    });
                    index += 4;
                    continue;
                }
                if (0xDC00..=0xDFFF).contains(&unit) {
                    return Err(CodecError::malformed(
                        encoding,
                        raw_base + index as u64,
                        "unpaired low surrogate",
                    ));
                }
                let character = char::from_u32(unit as u32).ok_or_else(|| {
                    CodecError::malformed(
                        encoding,
                        raw_base + index as u64,
                        "invalid UTF-16 scalar",
                    )
                })?;
                text.push(character);
                spans.push(CharSpan {
                    raw_len: 2,
                    utf8_len: character.len_utf8() as u32,
                });
                index += 2;
            }
        }
        encoding => {
            while index < bytes.len() {
                let mut matched = None;
                for length in 1..=4.min(bytes.len() - index) {
                    if let Ok(candidate) =
                        decode_payload(&bytes[index..index + length], encoding, 0)
                    {
                        let mut chars = candidate.chars();
                        if let (Some(character), None) = (chars.next(), chars.next()) {
                            matched = Some((character, length));
                            break;
                        }
                    }
                }
                let Some((character, length)) = matched else {
                    return Err(CodecError::malformed(
                        encoding,
                        raw_base + index as u64,
                        "no valid character boundary in this encoding",
                    ));
                };
                text.push(character);
                spans.push(CharSpan {
                    raw_len: length as u32,
                    utf8_len: character.len_utf8() as u32,
                });
                index += length;
            }
        }
    }

    Ok(SpanDecoded {
        text,
        spans,
        raw_length: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::encode_text;
    use notepust_contracts::{BomPolicy, EncodingSpec};

    #[test]
    fn utf8_stream_handles_split_sequences() {
        let spec = EncodingSpec::utf8();
        let mut decoder = StreamingDecoder::new(&spec).unwrap();
        let bytes = "a中😀z".as_bytes();
        let first = decoder.decode(&bytes[..3], false).unwrap();
        assert_eq!(first.text, "a");
        let second = decoder.decode(&bytes[3..], true).unwrap();
        assert_eq!(second.text, "中😀z");
        assert_eq!(first.utf8_end, second.utf8_start);
    }

    #[test]
    fn utf16_stream_handles_split_surrogate() {
        let spec = EncodingSpec::new(EncodingId::Utf16Le, BomPolicy::Utf16Le);
        let bytes = encode_text("a😀b", &spec).unwrap();
        let payload = &bytes[2..];
        let mut decoder = StreamingDecoder::new(&spec).unwrap();
        let first = decoder.decode(&payload[..3], false).unwrap();
        assert_eq!(first.text, "a");
        let second = decoder.decode(&payload[3..], true).unwrap();
        assert_eq!(second.text, "😀b");
    }

    #[test]
    fn truncated_unicode_is_rejected_at_finish() {
        let spec = EncodingSpec::utf8();
        let mut decoder = StreamingDecoder::new(&spec).unwrap();
        let _ = decoder.decode(&[b'a', 0xE4, 0xB8], false).unwrap();
        assert!(decoder.finish().is_err());
    }

    #[test]
    fn span_map_matches_utf8_output() {
        let text = "a中😀b";
        let decoded = decode_with_spans(text.as_bytes(), EncodingId::Utf8, 0).unwrap();
        assert_eq!(decoded.text, text);
        assert_eq!(decoded.spans.len(), 4);
        assert_eq!(
            decoded
                .spans
                .iter()
                .map(|span| span.raw_len as usize)
                .sum::<usize>(),
            text.len()
        );
    }

    #[test]
    fn span_map_handles_gb18030_four_byte_characters() {
        let bytes = &[0x94, 0x39, 0xFC, 0x36];
        let decoded = decode_with_spans(bytes, EncodingId::Gb18030, 100).unwrap();
        assert_eq!(decoded.text, "😀");
        assert_eq!(decoded.spans[0].raw_len, 4);
    }

    #[test]
    fn span_map_handles_utf16_surrogates() {
        let bytes = [0x3D, 0xD8, 0x00, 0xDE, 0x2D, 0x4E];
        let decoded = decode_with_spans(&bytes, EncodingId::Utf16Le, 0).unwrap();
        assert_eq!(decoded.text, "😀中");
        assert_eq!(decoded.spans[0].raw_len, 4);
        assert_eq!(decoded.spans[1].raw_len, 2);
    }
}
