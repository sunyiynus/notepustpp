use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Number of grapheme clusters in a string.
pub fn grapheme_count(text: &str) -> u64 {
    text.graphemes(true).count() as u64
}

/// Byte offset of a zero-based grapheme column, clamped to the end.
pub fn offset_of_grapheme_column(text: &str, column: u64) -> usize {
    if column == 0 {
        return 0;
    }
    for (count, (offset, _grapheme)) in text.grapheme_indices(true).enumerate() {
        if count as u64 == column {
            return offset;
        }
    }
    text.len()
}

/// Converts a validated UTF-8 byte offset to a UTF-16 code unit offset.
pub fn utf8_to_utf16(text: &str, utf8_offset: u64) -> Option<u64> {
    let offset = usize::try_from(utf8_offset).ok()?;
    let prefix = text.get(..offset)?;
    Some(prefix.encode_utf16().count() as u64)
}

/// Converts a UTF-16 code unit offset to a UTF-8 byte offset. Returns `None`
/// if the offset lands inside a surrogate pair.
pub fn utf16_to_utf8(text: &str, utf16_offset: u64) -> Option<u64> {
    let target = usize::try_from(utf16_offset).ok()?;
    let mut units = 0usize;
    for (byte_offset, character) in text.char_indices() {
        if units == target {
            return Some(byte_offset as u64);
        }
        units += character.len_utf16();
        if units > target {
            return None;
        }
    }
    if units == target {
        Some(text.len() as u64)
    } else {
        None
    }
}

pub fn utf16_len(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}

/// Terminal-like width of one grapheme. Tab is handled by the caller.
fn grapheme_width(grapheme: &str, tab_width: u32, current_column: u32) -> u32 {
    if grapheme == "\t" {
        let tab_width = tab_width.max(1);
        return tab_width - (current_column % tab_width);
    }
    let mut width = 0u32;
    for character in grapheme.chars() {
        if character == '\t' {
            let tab_width = tab_width.max(1);
            width += tab_width - ((current_column + width) % tab_width);
        } else {
            width += character.width().unwrap_or(0) as u32;
        }
    }
    width
}

/// Display column of a byte offset inside `text`, expanding tabs.
pub fn display_column_at(text: &str, byte_offset: u64, tab_width: u32) -> u64 {
    let offset = usize::try_from(byte_offset).ok().unwrap_or(text.len());
    let prefix = text.get(..offset).unwrap_or(text);
    let mut column = 0u32;
    for grapheme in prefix.graphemes(true) {
        column += grapheme_width(grapheme, tab_width, column);
    }
    column as u64
}

/// Byte offset and display column for a target display column. When the column
/// falls inside a wide grapheme, the grapheme start is returned.
pub fn offset_at_display_column(text: &str, target_column: u64, tab_width: u32) -> (usize, u64) {
    let target = target_column.min(u32::MAX as u64) as u32;
    let mut column = 0u32;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if column >= target {
            return (offset, column as u64);
        }
        let width = grapheme_width(grapheme, tab_width, column);
        if column + width > target {
            return (offset, column as u64);
        }
        column += width;
    }
    (text.len(), column as u64)
}

/// Width of a text using tab stops, intended for display/layout estimates only.
pub fn display_width(text: &str, tab_width: u32) -> u64 {
    display_column_at(text, text.len() as u64, tab_width)
}

/// Expands tabs according to tab stops, preserving all other characters.
pub fn expand_tabs(text: &str, tab_width: u32) -> String {
    let mut output = String::with_capacity(text.len());
    let mut column = 0u32;
    let tab_width = tab_width.max(1);
    for character in text.chars() {
        if character == '\t' {
            let spaces = tab_width - (column % tab_width);
            for _ in 0..spaces {
                output.push(' ');
            }
            column += spaces;
        } else {
            output.push(character);
            column += character.width().unwrap_or(0) as u32;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_match_platform_units() {
        let text = "a😀中";
        assert_eq!(utf8_to_utf16(text, 1), Some(1));
        assert_eq!(utf8_to_utf16(text, 5), Some(3));
        assert_eq!(utf8_to_utf16(text, 8), Some(4));
        assert_eq!(utf16_to_utf8(text, 2), None);
        assert_eq!(utf16_to_utf8(text, 3), Some(5));
        assert_eq!(utf16_to_utf8(text, 4), Some(8));
    }

    #[test]
    fn grapheme_columns_do_not_split_zwj_sequences() {
        let family = "👨‍👩‍👧‍👦";
        assert_eq!(grapheme_count(family), 1);
        assert_eq!(offset_of_grapheme_column(family, 1), family.len());
        assert_eq!(offset_of_grapheme_column("a中b", 2), 4);
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(display_column_at("\tx", 2, 4), 5);
        assert_eq!(display_column_at("ab\txy", 5, 4), 6);
        assert_eq!(offset_at_display_column("ab\txy", 4, 4), (3, 4));
        assert_eq!(expand_tabs("a\tb\tc", 4), "a   b   c");
    }

    #[test]
    fn chinese_and_emoji_are_wide() {
        assert_eq!(display_width("中", 4), 2);
        assert_eq!(display_width("😀", 4), 2);
        assert_eq!(display_width("e\u{301}", 4), 1);
    }
}
