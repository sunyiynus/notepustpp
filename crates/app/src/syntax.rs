//! Tree-sitter syntax highlighting for the editor.
//!
//! The application owns the editor's authoritative text and undo history; this
//! module only adapts the GPUI-Kit highlighter seam. Parsing is incremental and
//! bounded: a synchronous parse gets a small budget, and a debounced catch-up
//! parse runs after the user pauses. Files above the highlight ceiling keep
//! plain styling rather than blocking the UI.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit as BaseInputEdit, InputEditorStyle,
    InputHighlighter, InputHighlighterFactory,
};
use gpui_kit::component::highlighter::SyntaxHighlighter;
use gpui_kit::*;
use ropey::Rope;
use tree_sitter::{InputEdit, Point};

use crate::theme::Palette;

const SYNC_PARSE_BUDGET: Duration = Duration::from_millis(8);
const MAX_HIGHLIGHT_BYTES: usize = 2 * 1024 * 1024;
const CATCH_UP_DELAY: Duration = Duration::from_millis(180);

/// Language name understood by the GPUI-Component language registry.
pub fn language_for_path(path: Option<&Path>) -> &'static str {
    let Some(path) = path else {
        return "text";
    };
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rs" => "rust",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "html" | "htm" | "xml" => "html",
        "css" => "css",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "py" => "python",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "bash",
        _ => "text",
    }
}

pub fn highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language: &str| {
        (!language.is_empty() && language != "text")
            .then(|| Box::new(TreeSitterHighlighter::new(language)) as Box<dyn InputHighlighter>)
    })
}

/// Installs the grammar factory, highlight resolver and language on an editor.
pub fn configure(
    state: &mut EditorState,
    language: &str,
    palette: Palette,
    cx: &mut Context<EditorState>,
) {
    state.set_highlighter_factory(highlighter_factory(), cx);
    state.set_editor_style(editor_style(palette));
    state.set_highlighter(language, cx);
}

/// Updates syntax colors without resetting the parsed tree.
pub fn refresh_style(state: &mut EditorState, palette: Palette) {
    state.set_editor_style(editor_style(palette));
}

pub fn editor_style(palette: Palette) -> InputEditorStyle {
    InputEditorStyle {
        highlight_styles: Arc::new(SyntaxResolver { palette }),
        ..Default::default()
    }
}

struct SyntaxResolver {
    palette: Palette,
}

fn rgb_color(hex: u32) -> Hsla {
    rgb(hex).into()
}

impl HighlightStyleResolver for SyntaxResolver {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        let light = !self.palette.dark;
        let color = match name {
            "keyword" | "keyword.control" | "preproc" => {
                if light {
                    rgb_color(0xaf00db)
                } else {
                    rgb_color(0xc586c0)
                }
            }
            "string"
            | "string.escape"
            | "string.regex"
            | "string.special"
            | "string.special.symbol"
            | "text.literal" => {
                if light {
                    rgb_color(0xa31515)
                } else {
                    rgb_color(0xce9178)
                }
            }
            "comment" | "comment.doc" => {
                if light {
                    rgb_color(0x008000)
                } else {
                    rgb_color(0x6a9955)
                }
            }
            "number" | "boolean" | "constant" => {
                if light {
                    rgb_color(0x098658)
                } else {
                    rgb_color(0xb5cea8)
                }
            }
            "function" | "constructor" => {
                if light {
                    rgb_color(0x795e26)
                } else {
                    rgb_color(0xdcdcaa)
                }
            }
            "type" | "enum" | "variant" => {
                if light {
                    rgb_color(0x267f99)
                } else {
                    rgb_color(0x4ec9b0)
                }
            }
            "variable" | "variable.special" | "property" | "attribute" => {
                if light {
                    rgb_color(0x001080)
                } else {
                    rgb_color(0x9cdcfe)
                }
            }
            "tag" | "tag.doctype" => {
                if light {
                    rgb_color(0x800000)
                } else {
                    rgb_color(0x569cd6)
                }
            }
            "operator" => self.palette.fg,
            "punctuation"
            | "punctuation.bracket"
            | "punctuation.delimiter"
            | "punctuation.list_marker"
            | "punctuation.special" => self.palette.muted,
            "label" | "title" | "primary" | "link_text" | "link_uri" => self.palette.accent,
            "hint" | "predictive" => self.palette.muted,
            "emphasis" | "emphasis.strong" => self.palette.fg,
            _ => return None,
        };
        let font_style =
            matches!(name, "comment" | "comment.doc" | "emphasis").then_some(FontStyle::Italic);
        Some(HighlightStyle {
            color: Some(color),
            font_style,
            ..Default::default()
        })
    }
}

struct TreeSitterHighlighter {
    language: SharedString,
    inner: Rc<RefCell<SyntaxHighlighter>>,
    pending_full: Rc<Cell<bool>>,
    parse_task: Rc<RefCell<Option<Task<()>>>>,
}

impl TreeSitterHighlighter {
    fn new(language: &str) -> Self {
        Self {
            language: language.to_string().into(),
            inner: Rc::new(RefCell::new(SyntaxHighlighter::new(language))),
            pending_full: Rc::new(Cell::new(false)),
            parse_task: Rc::new(RefCell::new(None)),
        }
    }
}

fn to_tree_sitter_edit(edit: BaseInputEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: Point::new(edit.start_position.row, edit.start_position.column),
        old_end_position: Point::new(edit.old_end_position.row, edit.old_end_position.column),
        new_end_position: Point::new(edit.new_end_position.row, edit.new_end_position.column),
    }
}

impl InputHighlighter for TreeSitterHighlighter {
    fn language(&self) -> SharedString {
        self.language.clone()
    }

    fn update(
        &mut self,
        edit: Option<BaseInputEdit>,
        text: &Rope,
        _folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        if text.len() > MAX_HIGHLIGHT_BYTES {
            if let Some(edit) = edit {
                self.inner
                    .borrow_mut()
                    .edit_tree(Some(to_tree_sitter_edit(edit)), text);
            }
            return;
        }

        // A previous update timed out. Rebuild once with the current text after
        // the user pauses; this also clears any stale tree.
        if self.pending_full.replace(false) {
            *self.inner.borrow_mut() = SyntaxHighlighter::new(self.language.as_ref());
            let _ = self.inner.borrow_mut().update(None, text, None);
            self.parse_task.borrow_mut().take();
            return;
        }

        let completed = self.inner.borrow_mut().update(
            edit.map(to_tree_sitter_edit),
            text,
            Some(SYNC_PARSE_BUDGET),
        );
        if completed {
            self.parse_task.borrow_mut().take();
            return;
        }

        self.pending_full.set(true);
        let entity = cx.entity();
        let task = cx.spawn_in(window, async move |_this, cx| {
            cx.background_executor().timer(CATCH_UP_DELAY).await;
            entity.update(cx, |state, cx| state.refresh_highlighter(cx));
        });
        self.parse_task.borrow_mut().replace(task);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.inner.borrow().styles(range, resolver)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        self.inner
            .borrow()
            .tree()
            .map(extract_fold_ranges)
            .unwrap_or_default()
    }
}

fn extract_fold_ranges(tree: &tree_sitter::Tree) -> Vec<FoldRange> {
    fn collect(node: tree_sitter::Node, ranges: &mut Vec<FoldRange>) {
        let start = node.start_position().row;
        let end = node.end_position().row;
        if end.saturating_sub(start) >= 2 {
            ranges.push(FoldRange::new(start, end));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(child, ranges);
        }
    }

    let root = tree.root_node();
    let mut ranges = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        collect(child, &mut ranges);
    }
    ranges.sort_by_key(|range| range.start_line);
    ranges.dedup_by_key(|range| range.start_line);
    ranges
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::language_for_path;

    #[test]
    fn maps_supported_extensions_and_falls_back_to_text() {
        assert_eq!(language_for_path(Some(Path::new("main.RS"))), "rust");
        assert_eq!(language_for_path(Some(Path::new("config.toml"))), "toml");
        assert_eq!(language_for_path(Some(Path::new("page.tsx"))), "tsx");
        assert_eq!(language_for_path(Some(Path::new("unknown.xyz"))), "text");
        assert_eq!(language_for_path(None), "text");
    }
}
