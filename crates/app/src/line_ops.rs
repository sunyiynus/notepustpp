//! Pure line-oriented editing operations.
//!
//! Every operation takes the authoritative text plus byte selection offsets and
//! returns the replacement text with a selection. The app applies the result
//! through one undoable editor transaction.

use std::ops::Range;
use std::path::Path;

use notepust_text_codec::coords::{display_column_at, offset_at_display_column};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEdit {
    pub text: String,
    pub selection: Range<usize>,
}

type Line = (String, String);

/// Splits text into `(content, terminator)` pairs. A trailing terminator does
/// not create an extra empty line. Empty text has no lines.
fn split_keep_ends(text: &str) -> Vec<Line> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            let terminator = if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                index += 2;
                "\r\n"
            } else {
                index += 1;
                "\r"
            };
            lines.push((
                text[start..index - terminator.len()].to_string(),
                terminator.to_string(),
            ));
            start = index;
        } else if bytes[index] == b'\n' {
            lines.push((text[start..index].to_string(), "\n".to_string()));
            index += 1;
            start = index;
        } else {
            index += 1;
        }
    }
    if start < text.len() {
        lines.push((text[start..].to_string(), String::new()));
    }
    lines
}

fn line_starts(lines: &[Line]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for (content, terminator) in lines {
        starts.push(offset);
        offset += content.len() + terminator.len();
    }
    starts
}

fn line_of_offset(starts: &[usize], offset: usize) -> Option<usize> {
    (!starts.is_empty()).then(|| {
        starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    })
}

fn selected_lines(
    lines: &[Line],
    starts: &[usize],
    selection: Range<usize>,
) -> Option<(usize, usize)> {
    let first = line_of_offset(starts, selection.start)?;
    let last = if selection.end > selection.start {
        line_of_offset(starts, selection.end.saturating_sub(1))?
    } else {
        first
    };
    Some((first.min(lines.len() - 1), last.min(lines.len() - 1)))
}

fn join_lines(lines: &[Line]) -> String {
    let mut text = String::new();
    for (content, terminator) in lines {
        text.push_str(content);
        text.push_str(terminator);
    }
    text
}

fn block_end(starts: &[usize], lines: &[Line], first: usize, last: usize) -> usize {
    let mut end = starts[first];
    for (content, terminator) in &lines[first..=last] {
        end += content.len() + terminator.len();
    }
    end
}

pub fn duplicate_lines(text: &str, selection: Range<usize>) -> Option<LineEdit> {
    let lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let mut output = Vec::with_capacity(lines.len() + last - first + 1);
    output.extend_from_slice(&lines[..=last]);
    output.extend_from_slice(&lines[first..=last]);
    output.extend_from_slice(&lines[last + 1..]);
    let count = last - first + 1;
    let duplicate_first = last + 1;
    let duplicate_last = last + count;
    let new_starts = line_starts(&output);
    let selection = new_starts[duplicate_first]
        ..block_end(&new_starts, &output, duplicate_first, duplicate_last);
    Some(LineEdit {
        text: join_lines(&output),
        selection,
    })
}

pub fn delete_lines(text: &str, selection: Range<usize>) -> Option<LineEdit> {
    let lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let start = starts[first];
    let mut output = Vec::with_capacity(lines.len());
    output.extend_from_slice(&lines[..first]);
    output.extend_from_slice(&lines[last + 1..]);
    let new_text = join_lines(&output);
    let caret = start.min(new_text.len());
    Some(LineEdit {
        text: new_text,
        selection: caret..caret,
    })
}

pub fn move_lines(text: &str, selection: Range<usize>, direction: i32) -> Option<LineEdit> {
    let mut lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    if direction < 0 {
        if first == 0 {
            return None;
        }
        lines[first - 1..=last].rotate_left(1);
        let new_starts = line_starts(&lines);
        let new_start = new_starts[first - 1];
        let new_end = block_end(&new_starts, &lines, first - 1, last - 1);
        Some(LineEdit {
            text: join_lines(&lines),
            selection: new_start..new_end,
        })
    } else if direction > 0 {
        if last + 1 >= lines.len() {
            return None;
        }
        lines[first..=last + 1].rotate_right(1);
        let new_starts = line_starts(&lines);
        let new_start = new_starts[first + 1];
        let new_end = block_end(&new_starts, &lines, first + 1, last + 1);
        Some(LineEdit {
            text: join_lines(&lines),
            selection: new_start..new_end,
        })
    } else {
        None
    }
}

pub fn sort_lines(text: &str, selection: Range<usize>) -> Option<LineEdit> {
    let mut lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let mut contents = lines[first..=last]
        .iter()
        .map(|(content, _)| content.clone())
        .collect::<Vec<_>>();
    contents.sort();
    for (slot, content) in lines[first..=last].iter_mut().zip(contents) {
        slot.0 = content;
    }
    let start = starts[first];
    let end = block_end(&starts, &lines, first, last);
    Some(LineEdit {
        text: join_lines(&lines),
        selection: start..end,
    })
}

pub fn trim_trailing_whitespace(text: &str, selection: Range<usize>) -> Option<LineEdit> {
    let mut lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let mut changed = false;
    for (content, _) in &mut lines[first..=last] {
        let trimmed = content.trim_end_matches([' ', '\t']);
        if trimmed.len() != content.len() {
            *content = trimmed.to_string();
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    let start = starts[first];
    let end = block_end(&starts, &lines, first, last);
    Some(LineEdit {
        text: join_lines(&lines),
        selection: start..end,
    })
}

pub fn comment_token(path: Option<&Path>) -> &'static str {
    let extension = path
        .and_then(|path| path.extension())
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "py" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "ini" => "#",
        "sql" => "--",
        _ => "//",
    }
}

pub fn toggle_comments(text: &str, selection: Range<usize>, token: &str) -> Option<LineEdit> {
    let mut lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let non_empty = lines[first..=last]
        .iter()
        .filter(|(content, _)| !content.trim().is_empty())
        .collect::<Vec<_>>();
    if non_empty.is_empty() {
        return None;
    }
    let all_commented = non_empty.iter().all(|(content, _)| {
        let indent = content.len() - content.trim_start().len();
        content[indent..].starts_with(token)
    });
    for (content, _) in &mut lines[first..=last] {
        if content.trim().is_empty() {
            continue;
        }
        let indent = content.len() - content.trim_start().len();
        if all_commented {
            let after = &content[indent + token.len()..];
            let after = after.strip_prefix(' ').unwrap_or(after);
            *content = format!("{}{}", &content[..indent], after);
        } else {
            *content = format!("{}{} {}", &content[..indent], token, &content[indent..]);
        }
    }
    let start = starts[first];
    let end = block_end(&starts, &lines, first, last);
    Some(LineEdit {
        text: join_lines(&lines),
        selection: start..end,
    })
}

pub fn indent_lines(
    text: &str,
    selection: Range<usize>,
    indent: &str,
    outdent: bool,
) -> Option<LineEdit> {
    let mut lines = split_keep_ends(text);
    let starts = line_starts(&lines);
    let (first, last) = selected_lines(&lines, &starts, selection)?;
    let mut changed = false;
    if outdent {
        let spaces = indent.len();
        for (content, _) in &mut lines[first..=last] {
            if content.starts_with(indent) {
                *content = content[indent.len()..].to_string();
                changed = true;
            } else {
                let removable = content
                    .chars()
                    .take_while(|character| *character == ' ' || *character == '\t')
                    .take(spaces)
                    .map(char::len_utf8)
                    .sum::<usize>();
                if removable > 0 {
                    *content = content[removable..].to_string();
                    changed = true;
                }
            }
        }
        if !changed {
            return None;
        }
    } else {
        for (content, _) in &mut lines[first..=last] {
            if content.trim().is_empty() {
                continue;
            }
            *content = format!("{indent}{content}");
            changed = true;
        }
        if !changed {
            return None;
        }
    }
    let start = starts[first];
    let end = block_end(&starts, &lines, first, last);
    Some(LineEdit {
        text: join_lines(&lines),
        selection: start..end,
    })
}

fn line_bounds(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .rfind(['\n', '\r'])
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = text[offset..]
        .find(['\n', '\r'])
        .map(|index| offset + index)
        .unwrap_or(text.len());
    (start, end)
}

fn vertical_offset(text: &str, offset: usize, direction: i32, tab_width: u32) -> Option<usize> {
    let (line_start, line_end) = line_bounds(text, offset);
    let column = display_column_at(
        &text[line_start..offset],
        (offset - line_start) as u64,
        tab_width,
    );
    if direction < 0 {
        if line_start == 0 {
            return None;
        }
        let prefix = &text[..line_start];
        let previous_end = line_start - if prefix.ends_with("\r\n") { 2 } else { 1 };
        let previous_start = text[..previous_end]
            .rfind(['\n', '\r'])
            .map(|index| index + 1)
            .unwrap_or(0);
        let target = &text[previous_start..previous_end];
        let (local, _) = offset_at_display_column(target, column, tab_width);
        Some(previous_start + local)
    } else {
        if line_end >= text.len() {
            return None;
        }
        let next_start = line_end
            + if text[line_end..].starts_with("\r\n") {
                2
            } else {
                1
            };
        let next_end = text[next_start..]
            .find(['\n', '\r'])
            .map(|index| next_start + index)
            .unwrap_or(text.len());
        let target = &text[next_start..next_end];
        let (local, _) = offset_at_display_column(target, column, tab_width);
        Some(next_start + local)
    }
}

pub fn cursor_above(text: &str, offset: usize, tab_width: u32) -> Option<usize> {
    vertical_offset(text, offset, -1, tab_width)
}

pub fn cursor_below(text: &str, offset: usize, tab_width: u32) -> Option<usize> {
    vertical_offset(text, offset, 1, tab_width)
}

/// Returns one selection per line between the anchor and caret at the same
/// display columns. This is the programmatic block-selection intent; the editor
/// also supports Alt+click and Alt+Shift+drag natively.
#[allow(dead_code)]
pub fn column_selection(
    text: &str,
    anchor_offset: usize,
    caret_offset: usize,
    tab_width: u32,
) -> Vec<Range<usize>> {
    let (anchor_start, _) = line_bounds(text, anchor_offset);
    let (caret_start, _) = line_bounds(text, caret_offset);
    let anchor_column = display_column_at(
        &text[anchor_start..anchor_offset],
        (anchor_offset - anchor_start) as u64,
        tab_width,
    );
    let caret_column = display_column_at(
        &text[caret_start..caret_offset],
        (caret_offset - caret_start) as u64,
        tab_width,
    );
    let low = anchor_column.min(caret_column);
    let high = anchor_column.max(caret_column);
    let first = anchor_start.min(caret_start);
    let last = anchor_start.max(caret_start);
    let mut ranges = Vec::new();
    let mut line_start = first;
    while line_start <= last && line_start <= text.len() {
        let line_end = text[line_start..]
            .find(['\n', '\r'])
            .map(|index| line_start + index)
            .unwrap_or(text.len());
        let content = &text[line_start..line_end];
        let (a, _) = offset_at_display_column(content, low, tab_width);
        let (b, _) = offset_at_display_column(content, high, tab_width);
        ranges.push(line_start + a..(line_start + b));
        if line_end >= text.len() {
            break;
        }
        line_start = line_end
            + if text[line_end..].starts_with("\r\n") {
                2
            } else {
                1
            };
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_and_deletes_multiple_lines() {
        let text = "a\r\nb\r\nc\r\n";
        let edit = duplicate_lines(text, 0..4).unwrap();
        assert_eq!(edit.text, "a\r\nb\r\na\r\nb\r\nc\r\n");
        assert_eq!(&edit.text[edit.selection.clone()], "a\r\nb\r\n");
        let edit = delete_lines(text, 0..4).unwrap();
        assert_eq!(edit.text, "c\r\n");
        assert_eq!(edit.selection, 0..0);
    }

    #[test]
    fn moves_lines_up_and_down_with_crlf() {
        let up = move_lines("a\nb\nc", 2..3, -1).unwrap();
        assert_eq!(up.text, "b\na\nc");
        assert_eq!(&up.text[up.selection.clone()], "b\n");
        let down = move_lines("a\nb\nc", 0..1, 1).unwrap();
        assert_eq!(down.text, "b\na\nc");
        assert_eq!(&down.text[down.selection.clone()], "a\n");
    }

    #[test]
    fn sorts_and_trims_selected_lines() {
        let text = "b  \na\t\nc\n";
        let sorted = sort_lines(text, 0..text.len()).unwrap();
        assert_eq!(sorted.text, "a\t\nb  \nc\n");
        let trimmed = trim_trailing_whitespace(text, 0..text.len()).unwrap();
        assert_eq!(trimmed.text, "b\na\nc\n");
    }

    #[test]
    fn toggles_comments_with_indent() {
        let text = "  a\n  b\n";
        let commented = toggle_comments(text, 0..text.len(), "//").unwrap();
        assert_eq!(commented.text, "  // a\n  // b\n");
        let uncommented = toggle_comments(&commented.text, 0..commented.text.len(), "//").unwrap();
        assert_eq!(uncommented.text, text);
    }

    #[test]
    fn indents_and_outdents() {
        let text = "a\nb\n";
        let indented = indent_lines(text, 0..text.len(), "    ", false).unwrap();
        assert_eq!(indented.text, "    a\n    b\n");
        let outdented = indent_lines(&indented.text, 0..indented.text.len(), "    ", true).unwrap();
        assert_eq!(outdented.text, text);
    }

    #[test]
    fn cursor_moves_vertically_by_display_column() {
        let text = "abc\n中文x\ndef";
        let below = cursor_below(text, 11, 4).unwrap();
        assert_eq!(below, 15);
        let above = cursor_above(text, below, 4).unwrap();
        assert_eq!(above, 7);
    }

    #[test]
    fn column_selection_covers_each_line() {
        let text = "abc\n中文x\nz";
        let ranges = column_selection(text, 2, text.len(), 4);
        assert_eq!(ranges.len(), 3);
        assert_eq!(&text[ranges[0].clone()], "b");
    }
}
