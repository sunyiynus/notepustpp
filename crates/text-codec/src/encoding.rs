use encoding_rs::{DecoderResult, EncoderResult};
use notepust_contracts::{BomPolicy, EncodingId, EncodingSpec};

use crate::error::CodecError;

/// The BOM, if any, at the beginning of a byte slice.
pub fn sniff_bom(bytes: &[u8]) -> BomPolicy {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        BomPolicy::Utf8
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        BomPolicy::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        BomPolicy::Utf16Be
    } else {
        BomPolicy::None
    }
}

/// Decodes bytes after the BOM has already been removed.
pub fn decode_payload(
    bytes: &[u8],
    encoding: EncodingId,
    bom_base_offset: u64,
) -> Result<String, CodecError> {
    match encoding {
        EncodingId::Utf8 => decode_utf8(bytes, bom_base_offset),
        EncodingId::Utf16Le => decode_utf16(bytes, true, bom_base_offset),
        EncodingId::Utf16Be => decode_utf16(bytes, false, bom_base_offset),
        _ => decode_with_encoding_rs(bytes, encoding, bom_base_offset),
    }
}

fn decode_utf8(bytes: &[u8], base: u64) -> Result<String, CodecError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(error) => Err(CodecError::malformed(
            EncodingId::Utf8,
            base + error.valid_up_to() as u64,
            error.to_string(),
        )),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool, base: u64) -> Result<String, CodecError> {
    let encoding = if little_endian {
        EncodingId::Utf16Le
    } else {
        EncodingId::Utf16Be
    };
    if !bytes.len().is_multiple_of(2) {
        return Err(CodecError::malformed(
            encoding,
            base + bytes.len() as u64 - 1,
            "odd number of bytes for UTF-16",
        ));
    }

    let (pairs, _) = bytes.as_chunks::<2>();
    let mut units = pairs.iter().map(|pair| {
        if little_endian {
            u16::from_le_bytes(*pair)
        } else {
            u16::from_be_bytes(*pair)
        }
    });

    let mut text = String::with_capacity(bytes.len() / 2 + 8);
    let mut unit_index = 0u64;
    while let Some(unit) = units.next() {
        if (0xD800..=0xDBFF).contains(&unit) {
            let Some(low) = units.next() else {
                return Err(CodecError::malformed(
                    encoding,
                    base + unit_index * 2,
                    format!("unpaired high surrogate 0x{unit:04X}"),
                ));
            };
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(CodecError::malformed(
                    encoding,
                    base + (unit_index + 1) * 2,
                    format!("high surrogate 0x{unit:04X} followed by 0x{low:04X}"),
                ));
            }
            let scalar = 0x1_0000 + (((unit as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
            if let Some(character) = char::from_u32(scalar) {
                text.push(character);
            }
            unit_index += 2;
            continue;
        }
        if (0xDC00..=0xDFFF).contains(&unit) {
            return Err(CodecError::malformed(
                encoding,
                base + unit_index * 2,
                format!("unpaired low surrogate 0x{unit:04X}"),
            ));
        }
        if let Some(character) = char::from_u32(unit as u32) {
            text.push(character);
        }
        unit_index += 1;
    }
    Ok(text)
}

fn decode_with_encoding_rs(
    bytes: &[u8],
    encoding_id: EncodingId,
    base: u64,
) -> Result<String, CodecError> {
    let encoding = encoding_rs_encoding(encoding_id).ok_or_else(|| CodecError::Unsupported {
        encoding: encoding_id,
        detail: "no encoding_rs mapping".to_string(),
    })?;

    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut output = String::with_capacity(bytes.len().saturating_mul(2).min(64 * 1024 * 1024));
    let mut consumed_total = 0usize;

    loop {
        if output.capacity() - output.len() < 4 {
            let needed = (output.capacity().max(1024) * 2).min(bytes.len() * 4 + 64);
            output.reserve(needed.max(4));
        }

        let (result, consumed) = decoder.decode_to_string_without_replacement(
            &bytes[consumed_total..],
            &mut output,
            true,
        );
        consumed_total += consumed;
        match result {
            DecoderResult::InputEmpty => {
                if consumed_total == bytes.len() {
                    return Ok(output);
                }
                return Err(CodecError::malformed(
                    encoding_id,
                    base + consumed_total as u64,
                    "decoder stopped before input was exhausted",
                ));
            }
            DecoderResult::OutputFull => {
                output.reserve(output.capacity().max(64));
            }
            DecoderResult::Malformed(length, _) => {
                return Err(CodecError::malformed(
                    encoding_id,
                    base + consumed_total.saturating_sub(length as usize) as u64,
                    "byte sequence is not valid in this encoding",
                ));
            }
        }
    }
}

pub fn encoding_rs_encoding(id: EncodingId) -> Option<&'static encoding_rs::Encoding> {
    id.whatwg_label()
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
}

/// Encodes text into the requested format, including a BOM if requested.
///
/// This is a full re-encode. Byte-faithful saves of unchanged documents must
/// use the retained raw source bytes instead.
pub fn encode_text(text: &str, spec: &EncodingSpec) -> Result<Vec<u8>, CodecError> {
    if !spec.bom_is_valid() {
        return Err(CodecError::Unsupported {
            encoding: spec.encoding,
            detail: format!("BOM {} cannot be used", spec.bom.label()),
        });
    }

    let mut output = Vec::with_capacity(text.len() + spec.bom.len() + 8);
    output.extend_from_slice(spec.bom.bytes());

    match spec.encoding {
        EncodingId::Utf8 => output.extend_from_slice(text.as_bytes()),
        EncodingId::Utf16Le => {
            for unit in text.encode_utf16() {
                output.extend_from_slice(&unit.to_le_bytes());
            }
        }
        EncodingId::Utf16Be => {
            for unit in text.encode_utf16() {
                output.extend_from_slice(&unit.to_be_bytes());
            }
        }
        encoding_id => {
            let encoding =
                encoding_rs_encoding(encoding_id).ok_or_else(|| CodecError::Unsupported {
                    encoding: encoding_id,
                    detail: "no encoding_rs mapping".to_string(),
                })?;
            let mut encoder = encoding.new_encoder();
            let mut source_offset = 0usize;
            let mut buffer = vec![0u8; 8192];

            loop {
                let (result, read, written) = encoder.encode_from_utf8_without_replacement(
                    &text[source_offset..],
                    &mut buffer,
                    true,
                );
                output.extend_from_slice(&buffer[..written]);
                source_offset += read;

                match result {
                    EncoderResult::InputEmpty => break,
                    EncoderResult::OutputFull => {}
                    EncoderResult::Unmappable(character) => {
                        return Err(CodecError::Unmappable {
                            encoding: encoding_id,
                            character,
                            utf8_offset: source_offset as u64,
                        });
                    }
                }
            }
        }
    }

    Ok(output)
}

/// Byte-level round-trip classification for one source snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundTripClass {
    /// Decode + encode reproduces the original bytes exactly.
    Exact,
    /// The text decodes, but re-encoding produces different bytes.
    NonCanonical,
    /// The bytes do not decode strictly.
    Malformed,
}

/// Classifies whether an unchanged save can preserve the original bytes.
pub fn classify_round_trip(bytes: &[u8], spec: &EncodingSpec) -> RoundTripClass {
    let Some(payload) = strip_bom(bytes, spec.bom) else {
        return RoundTripClass::Malformed;
    };
    let Ok(text) = decode_payload(payload, spec.encoding, spec.bom.len() as u64) else {
        return RoundTripClass::Malformed;
    };
    match encode_text(&text, spec) {
        Ok(encoded) if encoded == bytes => RoundTripClass::Exact,
        Ok(_) => RoundTripClass::NonCanonical,
        Err(_) => RoundTripClass::Malformed,
    }
}

pub fn strip_bom(bytes: &[u8], bom: BomPolicy) -> Option<&[u8]> {
    let expected = bom.bytes();
    if expected.is_empty() {
        Some(bytes)
    } else if bytes.starts_with(expected) {
        Some(&bytes[expected.len()..])
    } else {
        None
    }
}

/// Decodes a complete source file using an automatic policy.
///
/// Order: explicit choice (handled by the caller) > BOM > strict UTF-8 >
/// chardetng candidate. Guesses are only accepted when `allow_guess` is true.
pub fn decode_detected(bytes: &[u8], allow_guess: bool) -> Result<super::DecodedFile, CodecError> {
    use super::{DecodedFile, DetectionSource};

    let bom = sniff_bom(bytes);
    if bom != BomPolicy::None {
        let encoding = match bom {
            BomPolicy::Utf8 => EncodingId::Utf8,
            BomPolicy::Utf16Le => EncodingId::Utf16Le,
            BomPolicy::Utf16Be => EncodingId::Utf16Be,
            BomPolicy::None => unreachable!(),
        };
        let spec = EncodingSpec::new(encoding, bom);
        let payload = &bytes[bom.len()..];
        let text = decode_payload(payload, encoding, bom.len() as u64)?;
        return Ok(DecodedFile {
            text,
            spec,
            source: DetectionSource::Bom,
            round_trip: classify_round_trip(bytes, &EncodingSpec::new(encoding, bom)),
        });
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        let spec = EncodingSpec::utf8();
        return Ok(DecodedFile {
            text: text.to_string(),
            spec: spec.clone(),
            source: DetectionSource::StrictUtf8,
            round_trip: RoundTripClass::Exact,
        });
    }

    let candidate = detect_candidate(bytes);
    if !allow_guess {
        return Err(CodecError::EncodingChoiceRequired { candidate });
    }

    let spec = EncodingSpec::new(candidate, BomPolicy::None);
    let text = decode_payload(bytes, candidate, 0)?;
    Ok(DecodedFile {
        text,
        spec: spec.clone(),
        source: DetectionSource::Guessed,
        round_trip: classify_round_trip(bytes, &spec),
    })
}

/// Decodes with a user-selected encoding and BOM policy.
pub fn decode_explicit(
    bytes: &[u8],
    spec: &EncodingSpec,
) -> Result<super::DecodedFile, CodecError> {
    use super::{DecodedFile, DetectionSource};

    let actual = sniff_bom(bytes);
    if actual != BomPolicy::None && actual != spec.bom {
        return Err(CodecError::BomMismatch {
            expected: spec.encoding,
            actual,
        });
    }
    let payload = &bytes[spec.bom.len()..];
    let text = decode_payload(payload, spec.encoding, spec.bom.len() as u64)?;
    let round_trip = classify_round_trip(bytes, spec);
    Ok(DecodedFile {
        text,
        spec: spec.clone(),
        source: DetectionSource::Explicit,
        round_trip,
    })
}

/// Returns the best non-UTF-8 candidate. This is a hint, never a promise.
pub fn detect_candidate(bytes: &[u8]) -> EncodingId {
    use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};

    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, Utf8Detection::Allow);
    match encoding.name().to_ascii_lowercase().as_str() {
        "utf-8" => EncodingId::Utf8,
        "utf-16le" => EncodingId::Utf16Le,
        "utf-16be" => EncodingId::Utf16Be,
        "gbk" => EncodingId::Gbk,
        "gb18030" => EncodingId::Gb18030,
        "big5" => EncodingId::Big5,
        "shift_jis" => EncodingId::ShiftJis,
        "euc-jp" => EncodingId::EucJp,
        "iso-2022-jp" => EncodingId::Iso2022Jp,
        _ => EncodingId::Windows1252,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_is_detected_and_stripped() {
        let bytes = b"\xEF\xBB\xBFhello";
        assert_eq!(sniff_bom(bytes), BomPolicy::Utf8);
        let decoded = decode_detected(bytes, false).unwrap();
        assert_eq!(decoded.text, "hello");
        assert_eq!(decoded.spec.bom, BomPolicy::Utf8);
        assert_eq!(
            classify_round_trip(bytes, &decoded.spec),
            RoundTripClass::Exact
        );
    }

    #[test]
    fn utf16le_roundtrips() {
        let text = "中文\nemoji 😀";
        let spec = EncodingSpec::new(EncodingId::Utf16Le, BomPolicy::Utf16Le);
        let bytes = encode_text(text, &spec).unwrap();
        let decoded = decode_explicit(&bytes, &spec).unwrap();
        assert_eq!(decoded.text, text);
        assert_eq!(classify_round_trip(&bytes, &spec), RoundTripClass::Exact);
    }

    #[test]
    fn odd_utf16_is_rejected_with_offset() {
        let bytes = [0xFF, 0xFE, b'a', 0x00, 0x41];
        let error = decode_detected(&bytes, false).unwrap_err();
        assert!(matches!(
            error,
            CodecError::Malformed {
                encoding: EncodingId::Utf16Le,
                ..
            }
        ));
    }

    #[test]
    fn gb18030_four_byte_sequence_is_accepted() {
        // U+1F600 encoded in GB18030.
        let bytes: &[u8] = &[0x94, 0x39, 0xFC, 0x36];
        let spec = EncodingSpec::new(EncodingId::Gb18030, BomPolicy::None);
        let decoded = decode_explicit(bytes, &spec).unwrap();
        assert_eq!(decoded.text, "😀");
        assert_eq!(classify_round_trip(bytes, &spec), RoundTripClass::Exact);
    }

    #[test]
    fn unmappable_character_is_reported() {
        let spec = EncodingSpec::new(EncodingId::Gbk, BomPolicy::None);
        let error = encode_text("😀", &spec).unwrap_err();
        assert!(matches!(error, CodecError::Unmappable { .. }));
    }

    #[test]
    fn strict_utf8_detection_beats_guess() {
        let bytes = "纯文本".as_bytes();
        let decoded = decode_detected(bytes, false).unwrap();
        assert_eq!(decoded.spec.encoding, EncodingId::Utf8);
    }

    #[test]
    fn bom_mismatch_is_reported() {
        let bytes = b"\xEF\xBB\xBFabc";
        let spec = EncodingSpec::new(EncodingId::Utf8, BomPolicy::None);
        assert!(matches!(
            decode_explicit(bytes, &spec),
            Err(CodecError::BomMismatch { .. })
        ));
    }

    #[test]
    fn decoder_result_loop_does_not_lie_about_consumption() {
        // A long valid UTF-8 string decodes in one or more output-full rounds.
        let text = "a".repeat(50_000);
        let bytes = text.as_bytes();
        let decoded = decode_payload(bytes, EncodingId::Utf8, 0).unwrap();
        assert_eq!(decoded, text);
    }
}
