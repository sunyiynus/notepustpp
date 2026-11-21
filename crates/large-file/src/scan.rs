use notepust_contracts::EncodingId;
use notepust_text_codec::EolKind;

/// A raw line in the original encoded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawLine {
    pub number: u64,
    /// Absolute raw offset of the first content byte.
    pub start: u64,
    /// Absolute raw offset where content ends.
    pub content_end: u64,
    /// Absolute raw offset after the terminator.
    pub end: u64,
    pub terminator: Option<EolKind>,
}

impl RawLine {
    pub const fn content_len(self) -> u64 {
        self.content_end - self.start
    }
}

#[derive(Clone, Debug, Default)]
pub struct RawScan {
    pub lines: Vec<RawLine>,
    /// Bytes consumed from the start of the input (complete lines only).
    pub consumed: u64,
    /// True when the final input line had no terminator and was emitted because
    /// the caller declared the range complete.
    pub flushed_final_line: bool,
}

/// Scans complete lines from a byte range.
///
/// All supported encodings have the property that CR and LF bytes cannot occur
/// inside a multi-byte character, so line boundaries can be found without a
/// full decode. UTF-16 is scanned as code units. A trailing CR is retained
/// unless `last` is true, because it may be the first half of a CRLF that
/// continues in the next chunk.
pub fn scan_raw_lines(
    bytes: &[u8],
    raw_base: u64,
    encoding: EncodingId,
    start_line: u64,
    last: bool,
) -> RawScan {
    match encoding {
        EncodingId::Utf16Le => scan_utf16(bytes, raw_base, start_line, last, true),
        EncodingId::Utf16Be => scan_utf16(bytes, raw_base, start_line, last, false),
        _ => scan_bytes(bytes, raw_base, start_line, last),
    }
}

fn scan_bytes(bytes: &[u8], raw_base: u64, start_line: u64, last: bool) -> RawScan {
    let mut result = RawScan::default();
    let mut line_start = 0usize;
    let mut line_number = start_line;
    let mut index = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if index + 1 >= bytes.len() {
                    if !last {
                        break;
                    }
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + 1,
                        Some(EolKind::Cr),
                        line_number,
                    );
                    line_number += 1;
                    index += 1;
                    line_start = index;
                    break;
                }
                if bytes[index + 1] == b'\n' {
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + 2,
                        Some(EolKind::CrLf),
                        line_number,
                    );
                    line_number += 1;
                    index += 2;
                } else {
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + 1,
                        Some(EolKind::Cr),
                        line_number,
                    );
                    line_number += 1;
                    index += 1;
                }
                line_start = index;
            }
            b'\n' => {
                push_line(
                    &mut result,
                    raw_base,
                    line_start,
                    index,
                    index + 1,
                    Some(EolKind::Lf),
                    line_number,
                );
                line_number += 1;
                index += 1;
                line_start = index;
            }
            _ => index += 1,
        }
    }

    result.consumed = line_start as u64;
    if last {
        if line_start < bytes.len() {
            push_line(
                &mut result,
                raw_base,
                line_start,
                bytes.len(),
                bytes.len(),
                None,
                line_number,
            );
            result.consumed = bytes.len() as u64;
            result.flushed_final_line = true;
        } else if line_start == bytes.len() && !result.lines.is_empty() {
            // The final terminator creates a trailing empty logical line.
            result.lines.push(RawLine {
                number: line_number,
                start: raw_base + bytes.len() as u64,
                content_end: raw_base + bytes.len() as u64,
                end: raw_base + bytes.len() as u64,
                terminator: None,
            });
            result.flushed_final_line = true;
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn push_line(
    result: &mut RawScan,
    raw_base: u64,
    start: usize,
    content_end: usize,
    end: usize,
    terminator: Option<EolKind>,
    number: u64,
) {
    result.lines.push(RawLine {
        number,
        start: raw_base + start as u64,
        content_end: raw_base + content_end as u64,
        end: raw_base + end as u64,
        terminator,
    });
}

fn scan_utf16(
    bytes: &[u8],
    raw_base: u64,
    start_line: u64,
    last: bool,
    little_endian: bool,
) -> RawScan {
    let mut result = RawScan::default();
    let unit_len = 2usize;
    let mut line_start = 0usize;
    let mut line_number = start_line;
    let mut index = 0usize;

    let unit_at = |index: usize| -> Option<u16> {
        if index + 1 >= bytes.len() {
            return None;
        }
        Some(if little_endian {
            u16::from_le_bytes([bytes[index], bytes[index + 1]])
        } else {
            u16::from_be_bytes([bytes[index], bytes[index + 1]])
        })
    };

    while index + unit_len <= bytes.len() {
        let Some(unit) = unit_at(index) else {
            break;
        };
        match unit {
            0x000D => {
                let Some(next) = unit_at(index + unit_len) else {
                    if !last {
                        break;
                    }
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + unit_len,
                        Some(EolKind::Cr),
                        line_number,
                    );
                    line_number += 1;
                    index += unit_len;
                    line_start = index;
                    break;
                };
                if next == 0x000A {
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + 2 * unit_len,
                        Some(EolKind::CrLf),
                        line_number,
                    );
                    line_number += 1;
                    index += 2 * unit_len;
                } else {
                    push_line(
                        &mut result,
                        raw_base,
                        line_start,
                        index,
                        index + unit_len,
                        Some(EolKind::Cr),
                        line_number,
                    );
                    line_number += 1;
                    index += unit_len;
                }
                line_start = index;
            }
            0x000A => {
                push_line(
                    &mut result,
                    raw_base,
                    line_start,
                    index,
                    index + unit_len,
                    Some(EolKind::Lf),
                    line_number,
                );
                line_number += 1;
                index += unit_len;
                line_start = index;
            }
            _ => index += unit_len,
        }
    }

    result.consumed = line_start as u64;
    if last {
        // Odd trailing byte is not a complete line and is reported by the
        // decoder when the range is actually displayed.
        let remainder_start = line_start;
        let remainder_end = bytes.len() - (bytes.len() - line_start) % unit_len;
        if remainder_start < remainder_end {
            push_line(
                &mut result,
                raw_base,
                remainder_start,
                remainder_end,
                remainder_end,
                None,
                line_number,
            );
            result.consumed = remainder_end as u64;
            result.flushed_final_line = true;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_lone_terminators_are_counted_once() {
        let scan = scan_raw_lines(b"a\r\nb\rc\n", 0, EncodingId::Utf8, 0, true);
        assert_eq!(scan.lines.len(), 4);
        assert_eq!(scan.lines[0].terminator, Some(EolKind::CrLf));
        assert_eq!(scan.lines[1].terminator, Some(EolKind::Cr));
        assert_eq!(scan.lines[2].terminator, Some(EolKind::Lf));
        assert_eq!(scan.lines[3].start, 7);
    }

    #[test]
    fn incomplete_crlf_is_retained_until_next_chunk() {
        let first = scan_raw_lines(b"a\r", 0, EncodingId::Utf8, 0, false);
        assert!(first.lines.is_empty());
        assert_eq!(first.consumed, 0);

        // The caller carries the pending CR into the next chunk.
        let second = scan_raw_lines(b"\r\nb", 0, EncodingId::Utf8, 0, true);
        assert_eq!(second.lines.len(), 2);
        assert_eq!(second.lines[0].terminator, Some(EolKind::CrLf));
        assert_eq!(second.lines[1].start, 2);
    }

    #[test]
    fn unterminated_final_line_is_emitted_only_at_end() {
        let scan = scan_raw_lines(b"a\nb", 0, EncodingId::Utf8, 0, false);
        assert_eq!(scan.lines.len(), 1);
        assert_eq!(scan.consumed, 2);

        let scan = scan_raw_lines(b"b", 2, EncodingId::Utf8, 0, true);
        assert_eq!(scan.lines.len(), 1);
        assert_eq!(scan.lines[0].start, 2);
        assert_eq!(scan.lines[0].terminator, None);
    }

    #[test]
    fn utf16_lines_are_scanned_by_code_unit() {
        let mut bytes = Vec::new();
        for unit in "a\r\nb".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let scan = scan_raw_lines(&bytes, 0, EncodingId::Utf16Le, 0, true);
        assert_eq!(scan.lines.len(), 2);
        assert_eq!(scan.lines[0].terminator, Some(EolKind::CrLf));
        assert_eq!(scan.lines[1].start, 6);
    }
}
