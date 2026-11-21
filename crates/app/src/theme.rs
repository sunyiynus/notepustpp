use gpui_kit::base::{ColorTokens, Theme, ThemeAppearance};
use gpui_kit::*;
use notepust_storage::{Settings, ThemeMode};

/// Application chrome palette. Semantic roles, not scattered hex values.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Hsla,
    pub panel: Hsla,
    pub tab_active: Hsla,
    pub tab_inactive: Hsla,
    pub border: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_fg: Hsla,
    pub status_bg: Hsla,
    pub status_fg: Hsla,
    pub hover: Hsla,
    pub input_bg: Hsla,
    pub match_bg: Hsla,
    pub active_match: Hsla,
    pub warning: Hsla,
    pub selection: Hsla,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: rgb(0x1e1e1e).into(),
            panel: rgb(0x252526).into(),
            tab_active: rgb(0x1e1e1e).into(),
            tab_inactive: rgb(0x2d2d2d).into(),
            border: rgb(0x3c3c3c).into(),
            fg: rgb(0xd4d4d4).into(),
            muted: rgb(0x8c8c8c).into(),
            accent: rgb(0x0e639c).into(),
            accent_fg: rgb(0xffffff).into(),
            status_bg: rgb(0x007acc).into(),
            status_fg: rgb(0xffffff).into(),
            hover: rgb(0x2a2d2e).into(),
            input_bg: rgb(0x3c3c3c).into(),
            match_bg: rgba(0xd7ba7d, 0.35),
            active_match: rgba(0xff9632, 0.55),
            warning: rgb(0xcca700).into(),
            selection: rgba(0x264f78, 0.85),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            bg: rgb(0xffffff).into(),
            panel: rgb(0xf3f3f3).into(),
            tab_active: rgb(0xffffff).into(),
            tab_inactive: rgb(0xe8e8e8).into(),
            border: rgb(0xd4d4d4).into(),
            fg: rgb(0x1f1f1f).into(),
            muted: rgb(0x717171).into(),
            accent: rgb(0x005fb8).into(),
            accent_fg: rgb(0xffffff).into(),
            status_bg: rgb(0x007acc).into(),
            status_fg: rgb(0xffffff).into(),
            hover: rgb(0xe8e8e8).into(),
            input_bg: rgb(0xffffff).into(),
            match_bg: rgba(0xffd166, 0.45),
            active_match: rgba(0xff9632, 0.55),
            warning: rgb(0x9d5d00).into(),
            selection: rgba(0xadd6ff, 0.9),
        }
    }
}

impl Global for Palette {}

fn rgba(hex: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(hex).into();
    color.a = alpha;
    color
}

/// Resolves a saved preference to a concrete palette.
pub fn effective_mode(mode: ThemeMode) -> ThemeMode {
    match mode {
        // System appearance is not exposed portably yet; dark is the product
        // default and is reported as such rather than silently guessed.
        ThemeMode::System => ThemeMode::Dark,
        other => other,
    }
}

/// Installs the application palette and the matching Base editor tokens.
pub fn install(cx: &mut App, mode: ThemeMode) {
    let mode = effective_mode(mode);
    let palette = match mode {
        ThemeMode::Light => Palette::light(),
        _ => Palette::dark(),
    };
    cx.set_global(palette);
    let theme = Theme::global_mut(cx);
    theme.appearance = if matches!(mode, ThemeMode::Light) {
        ThemeAppearance::Light
    } else {
        ThemeAppearance::Dark
    };
    theme.tokens.colors = if matches!(mode, ThemeMode::Light) {
        ColorTokens::light()
    } else {
        ColorTokens::dark()
    };
    theme.tokens.colors.selection = palette.selection;
    theme.tokens.typography.mono = "Consolas".into();
    theme.tokens.typography.sans = "Segoe UI".into();
    cx.refresh_windows();
}

pub fn palette(cx: &App) -> Palette {
    cx.try_global::<Palette>()
        .cloned()
        .unwrap_or_else(Palette::dark)
}

/// Font used by editor text, including the user's fallback chain.
pub fn editor_font(settings: &Settings) -> Font {
    let mut font = font(settings.editor.font_family.clone());
    font.fallbacks = Some(FontFallbacks::from_fonts(
        settings.editor.font_fallbacks.clone(),
    ));
    font
}

/// Font used by chrome and labels.
pub fn ui_font(settings: &Settings) -> Font {
    let mut font = font(settings.ui.ui_font_family.clone());
    font.fallbacks = Some(FontFallbacks::from_fonts(
        settings.ui.ui_font_fallbacks.clone(),
    ));
    font
}

/// Publishes the configured font families to Base theme consumers.
pub fn install_fonts(cx: &mut App, settings: &Settings) {
    let theme = Theme::global_mut(cx);
    theme.tokens.typography.mono = settings.editor.font_family.clone().into();
    theme.tokens.typography.sans = settings.ui.ui_font_family.clone().into();
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use notepust_storage::Settings;

    use super::{editor_font, ui_font};

    #[test]
    fn configured_fonts_carry_their_fallback_chain() {
        let settings = Settings::default();
        let editor = editor_font(&settings);
        assert_eq!(editor.family.as_ref(), "Consolas");
        let fallbacks = editor.fallbacks.expect("editor fallbacks");
        assert!(
            fallbacks
                .fallback_list()
                .iter()
                .any(|font| font == "Microsoft YaHei UI")
        );
        let ui = ui_font(&settings);
        assert_eq!(ui.family.as_ref(), "Segoe UI");
    }
}
