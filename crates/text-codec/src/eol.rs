use notepust_contracts::LogicalPosition;

/// One logical line terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EolKind {
    CrLf,
    Lf,
    Cr,
}

impl EolKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrLf => "\r\n",
            Self::Lf => "\n",
            Self::Cr => "\r",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::CrLf => "CRLF",
            Self::Lf => "LF",
            Self::Cr => "CR",
        }
    }

    pub const fn len(self) -> usize {
        match self {
            Self::CrLf => 2,
            Self::Lf | Self::Cr => 1,
        }
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

/// A terminator located in the authoritative UTF-8 text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EolToken {
    pub kind: EolKind,
    pub start: u64,
    pub end: u64,
}

impl EolToken {
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Counts of each terminator style in a text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EolSummary {
    pub crlf: usize,
    pub lf: usize,
    pub cr: usize,
    pub first: Option<EolKind>,
}

impl EolSummary {
    pub const fn total(self) -> usize {
        self.crlf + self.lf + self.cr
    }

    pub const fn is_mixed(self) -> bool {
        let mut kinds = 0;
        if self.crlf > 0 {
            kinds += 1;
        }
        if self.lf > 0 {
            kinds += 1;
        }
        if self.cr > 0 {
            kinds += 1;
        }
        kinds > 1
    }

    /// Most frequent terminator; ties go to the one seen first.
    pub fn dominant(self) -> Option<EolKind> {
        let first = self.first?;
        let candidates = [
            (EolKind::CrLf, self.crlf),
            (EolKind::Lf, self.lf),
            (EolKind::Cr, self.cr),
        ];
        let max = candidates
            .iter()
            .map(|(_, count)| *count)
            .max()
            .unwrap_or(0);
        if max == 0 {
            return None;
        }
        let tied: Vec<EolKind> = candidates
            .iter()
            .filter(|(_, count)| *count == max)
            .map(|(kind, _)| *kind)
            .collect();
        if tied.contains(&first) {
            Some(first)
        } else {
            Some(tied[0])
        }
    }

    /// The style Enter should use for this document.
    pub fn preferred(self) -> EolKind {
        self.dominant().unwrap_or_else(default_eol)
    }
}

/// Platform default for a brand-new document with no terminators yet.
pub const fn default_eol() -> EolKind {
    if cfg!(windows) {
        EolKind::CrLf
    } else {
        EolKind::Lf
    }
}

/// Iterates terminators, always treating CRLF as one unit.
pub fn scan_eols(text: &str) -> Vec<EolToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    tokens.push(EolToken {
                        kind: EolKind::CrLf,
                        start: index as u64,
                        end: index as u64 + 2,
                    });
                    index += 2;
                } else {
                    tokens.push(EolToken {
                        kind: EolKind::Cr,
                        start: index as u64,
                        end: index as u64 + 1,
                    });
                    index += 1;
                }
            }
            b'\n' => {
                tokens.push(EolToken {
                    kind: EolKind::Lf,
                    start: index as u64,
                    end: index as u64 + 1,
                });
                index += 1;
            }
            _ => index += 1,
        }
    }
    tokens
}

pub fn summarize(text: &str) -> EolSummary {
    let mut summary = EolSummary::default();
    for token in scan_eols(text) {
        match token.kind {
            EolKind::CrLf => summary.crlf += 1,
            EolKind::Lf => summary.lf += 1,
            EolKind::Cr => summary.cr += 1,
        }
        summary.first.get_or_insert(token.kind);
    }
    summary
}

/// One logical line; `content_end` excludes the terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalLine {
    pub start: u64,
    pub content_end: u64,
    pub terminator: Option<EolKind>,
    pub terminator_start: u64,
    pub terminator_end: u64,
}

impl LogicalLine {
    pub const fn content_len(self) -> u64 {
        self.content_end - self.start
    }

    pub const fn end(self) -> u64 {
        self.terminator_end
    }
}

/// Scans a text into logical lines.
///
/// A trailing terminator produces a final empty logical line, matching editor
/// line numbering. Empty text has exactly one empty line.
pub fn logical_lines(text: &str) -> Vec<LogicalLine> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                let (kind, end) = if bytes.get(index + 1) == Some(&b'\n') {
                    (EolKind::CrLf, index + 2)
                } else {
                    (EolKind::Cr, index + 1)
                };
                lines.push(LogicalLine {
                    start: line_start as u64,
                    content_end: index as u64,
                    terminator: Some(kind),
                    terminator_start: index as u64,
                    terminator_end: end as u64,
                });
                index = end;
                line_start = end;
            }
            b'\n' => {
                lines.push(LogicalLine {
                    start: line_start as u64,
                    content_end: index as u64,
                    terminator: Some(EolKind::Lf),
                    terminator_start: index as u64,
                    terminator_end: index as u64 + 1,
                });
                index += 1;
                line_start = index;
            }
            _ => index += 1,
        }
    }

    lines.push(LogicalLine {
        start: line_start as u64,
        content_end: bytes.len() as u64,
        terminator: None,
        terminator_start: bytes.len() as u64,
        terminator_end: bytes.len() as u64,
    });
    lines
}

/// Immutable derived line index for one text revision.
#[derive(Clone, Debug)]
pub struct LineIndex {
    text_len: u64,
    lines: Vec<LogicalLine>,
    summary: EolSummary,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let lines = logical_lines(text);
        let summary = summarize(text);
        Self {
            text_len: text.len() as u64,
            lines,
            summary,
        }
    }

    pub fn from_text(text: &str) -> Self {
        Self::new(text)
    }

    pub fn lines(&self) -> &[LogicalLine] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn text_len(&self) -> u64 {
        self.text_len
    }

    pub fn summary(&self) -> EolSummary {
        self.summary
    }

    pub fn preferred_eol(&self) -> EolKind {
        self.summary.preferred()
    }

    pub fn line_at_offset(&self, offset: u64) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let offset = offset.min(self.text_len);
        match self.lines.binary_search_by(|line| line.start.cmp(&offset)) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    }

    pub fn line(&self, index: usize) -> Option<&LogicalLine> {
        self.lines.get(index)
    }

    /// Zero-based logical position with a grapheme column.
    pub fn position_of_offset(&self, text: &str, offset: u64) -> LogicalPosition {
        let line_index = self.line_at_offset(offset);
        let line = self.lines[line_index];
        let clamped = offset.clamp(line.start, line.content_end);
        let column = crate::coords::grapheme_count(
            text.get(line.start as usize..clamped as usize)
                .unwrap_or(""),
        );
        LogicalPosition::new(line_index as u64, column)
    }

    /// Byte offset for a zero-based logical position. Out-of-range columns
    /// clamp to the end of the line; out-of-range lines clamp to the document.
    pub fn offset_of_position(&self, text: &str, position: LogicalPosition) -> u64 {
        if self.lines.is_empty() {
            return 0;
        }
        let line_index = (position.line as usize).min(self.lines.len() - 1);
        let line = self.lines[line_index];
        let content = text
            .get(line.start as usize..line.content_end as usize)
            .unwrap_or("");
        let offset_in_line = crate::coords::offset_of_grapheme_column(content, position.column);
        line.start + offset_in_line as u64
    }

    pub fn line_start(&self, line: usize) -> Option<u64> {
        self.lines.get(line).map(|line| line.start)
    }

    pub fn line_content<'a>(&self, text: &'a str, line: usize) -> Option<&'a str> {
        let line = self.lines.get(line)?;
        text.get(line.start as usize..line.content_end as usize)
    }
}

/// Converts every terminator to `target` while preserving content bytes.
pub fn convert_eols(text: &str, target: EolKind) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for token in scan_eols(text) {
        output.push_str(&text[cursor..token.start as usize]);
        output.push_str(target.as_str());
        cursor = token.end as usize;
    }
    output.push_str(&text[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_one_terminator() {
        let tokens = scan_eols("a\r\nb\rc\nd");
        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![EolKind::CrLf, EolKind::Cr, EolKind::Lf]
        );
        assert_eq!(tokens[0].len(), 2);
    }

    #[test]
    fn summary_prefers_dominant_and_first_on_tie() {
        let summary = summarize("a\r\nb\nc\r\n");
        assert_eq!(summary.crlf, 2);
        assert_eq!(summary.lf, 1);
        assert!(summary.is_mixed());
        assert_eq!(summary.dominant(), Some(EolKind::CrLf));

        let tie = summarize("a\r\nb\n");
        assert_eq!(tie.dominant(), Some(EolKind::CrLf));
    }

    #[test]
    fn logical_lines_include_trailing_empty_line() {
        let lines = logical_lines("a\r\nb\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines[0].content_end, 1);
        assert_eq!(lines[1].start, 3);
        assert_eq!(lines[2].start, 5);
        assert_eq!(lines[2].content_len(), 0);
    }

    #[test]
    fn empty_text_has_one_line() {
        let lines = logical_lines("");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines[0].content_end, 0);
    }

    #[test]
    fn positions_use_graphemes_not_bytes() {
        let text = "a中😀b\r\nsecond";
        let index = LineIndex::new(text);
        let position = index.position_of_offset(text, 1 + "中".len() as u64 + "😀".len() as u64);
        assert_eq!(position, LogicalPosition::new(0, 3));
        assert_eq!(index.offset_of_position(text, position), 8);
        assert_eq!(
            index.offset_of_position(text, LogicalPosition::new(1, 99)),
            "a中😀b\r\nsecond".len() as u64
        );
    }

    #[test]
    fn offset_inside_crlf_clamps_to_content_end() {
        let text = "abc\r\ndef";
        let index = LineIndex::new(text);
        let position = index.position_of_offset(text, 4);
        assert_eq!(position, LogicalPosition::new(0, 3));
    }

    #[test]
    fn convert_preserves_content() {
        assert_eq!(convert_eols("a\r\nb\rc\n", EolKind::Lf), "a\nb\nc\n");
        assert_eq!(convert_eols("a\nb", EolKind::CrLf), "a\r\nb");
        assert_eq!(convert_eols("", EolKind::CrLf), "");
    }
}
