//! Real GPUI input tests for the vendored PreferredLineEnding patch.
//!
//! These run in a headless test window and dispatch real keystrokes through
//! the same focus and binding paths as the application.

use gpui::{Entity, TestAppContext, VisualTestContext};
use gpui_kit::base::input::{EditorState, PreferredLineEnding};

fn editor_with<'a>(
    cx: &'a mut TestAppContext,
    text: &'static str,
    preferred: PreferredLineEnding,
) -> (Entity<EditorState>, &'a mut VisualTestContext) {
    cx.update(gpui_kit::base::init);
    cx.add_window_view(move |window, cx| {
        let mut state = EditorState::new(window, cx);
        state.set_value(text, window, cx);
        state.set_preferred_line_ending(preferred, cx);
        state
    })
}

#[gpui::test]
fn enter_inserts_crlf_when_the_document_uses_crlf(cx: &mut TestAppContext) {
    let (state, cx) = editor_with(cx, "a\r\nb", PreferredLineEnding::CrLf);
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.focus(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_input("X中😀");
    cx.simulate_keystrokes("enter");
    let text = state.read_with(cx, |state, _| state.value().to_string());
    assert_eq!(text, "X中😀\r\na\r\nb");
}

#[gpui::test]
fn enter_inserts_lf_for_an_lf_document(cx: &mut TestAppContext) {
    let (state, cx) = editor_with(cx, "a\nb", PreferredLineEnding::Lf);
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.focus(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_input("X");
    cx.simulate_keystrokes("enter");
    let text = state.read_with(cx, |state, _| state.value().to_string());
    assert_eq!(text, "X\na\nb");
}

#[gpui::test]
fn undo_restores_crlf_document_after_enter(cx: &mut TestAppContext) {
    let (state, cx) = editor_with(cx, "a\r\nb", PreferredLineEnding::CrLf);
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.focus(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_input("X");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("ctrl-z");
    let text = state.read_with(cx, |state, _| state.value().to_string());
    assert_eq!(text, "Xa\r\nb");
}
