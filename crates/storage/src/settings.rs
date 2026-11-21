use std::path::Path;

use notepust_contracts::{BomPolicy, EncodingId, EncodingSpec};
use serde::{Deserialize, Serialize};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse settings: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialize settings: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("settings schema {found} is newer than this build supports ({supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
}

/// User settings persisted as TOML.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub files: FileSettings,
    pub recovery: RecoverySettings,
    pub backup: BackupSettings,
    pub editor: EditorSettings,
    pub ui: UiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            files: FileSettings::default(),
            recovery: RecoverySettings::default(),
            backup: BackupSettings::default(),
            editor: EditorSettings::default(),
            ui: UiSettings::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let settings: Self = toml::from_str(&text)?;
        let settings = settings.migrate()?;
        Ok(settings.normalized())
    }

    fn migrate(mut self) -> Result<Self, SettingsError> {
        if self.schema_version > SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedVersion {
                found: self.schema_version,
                supported: SETTINGS_SCHEMA_VERSION,
            });
        }
        if self.schema_version == 0 {
            self.schema_version = 1;
        }
        Ok(self)
    }

    pub fn normalized(mut self) -> Self {
        self.editor.font_family = non_empty_font(self.editor.font_family, "Consolas");
        self.editor.font_fallbacks = clean_font_list(self.editor.font_fallbacks);
        self.editor.tab_size = self.editor.tab_size.clamp(1, 16);
        self.editor.font_size = self.editor.font_size.clamp(6.0, 72.0);
        self.editor.line_height = self.editor.line_height.clamp(1.0, 3.0);
        self.ui.ui_font_size = self.ui.ui_font_size.clamp(6.0, 48.0);
        self.ui.ui_font_family = non_empty_font(self.ui.ui_font_family, "Segoe UI");
        self.ui.ui_font_fallbacks = clean_font_list(self.ui.ui_font_fallbacks);
        self.ui.sidebar_width = self.ui.sidebar_width.clamp(120.0, 800.0);
        self.recovery.flush_interval_ms = self.recovery.flush_interval_ms.clamp(200, 60_000);
        self.recovery.checkpoint_interval_s = self.recovery.checkpoint_interval_s.clamp(1, 600);
        self.backup.versions_per_file = self.backup.versions_per_file.clamp(1, 100);
        self
    }

    pub fn default_encoding(&self) -> EncodingSpec {
        EncodingSpec::new(self.files.default_encoding, self.files.default_bom)
    }

    pub fn to_toml(&self) -> Result<String, SettingsError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Writes settings through a same-directory temp file.
    pub fn save_atomic(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = self.to_toml()?;
        let mut temp = tempfile::NamedTempFile::new_in(
            path.parent()
                .ok_or_else(|| std::io::Error::other("settings path has no parent"))?,
        )?;
        use std::io::Write;
        temp.write_all(text.as_bytes())?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .map_err(|error| SettingsError::Io(error.error))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSettings {
    pub default_encoding: EncodingId,
    pub default_bom: BomPolicy,
    pub new_document_eol: NewDocumentEol,
    pub preserve_pasted_eol: bool,
    pub external_change: ExternalChangePolicy,
    pub legacy_encoding_open: LegacyEncodingPolicy,
    pub detect_encoding_guess: bool,
    pub max_editable_bytes: u64,
}

impl Default for FileSettings {
    fn default() -> Self {
        Self {
            default_encoding: EncodingId::Utf8,
            default_bom: BomPolicy::None,
            new_document_eol: NewDocumentEol::System,
            preserve_pasted_eol: true,
            external_change: ExternalChangePolicy::Prompt,
            legacy_encoding_open: LegacyEncodingPolicy::Ask,
            detect_encoding_guess: true,
            max_editable_bytes: 100 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewDocumentEol {
    Crlf,
    Lf,
    Cr,
    #[default]
    System,
}

impl NewDocumentEol {
    pub fn resolved(self) -> notepust_text_codec::EolKind {
        match self {
            Self::Crlf => notepust_text_codec::EolKind::CrLf,
            Self::Lf => notepust_text_codec::EolKind::Lf,
            Self::Cr => notepust_text_codec::EolKind::Cr,
            Self::System => notepust_text_codec::default_eol(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalChangePolicy {
    #[default]
    Prompt,
    ReloadClean,
    KeepBuffer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyEncodingPolicy {
    #[default]
    Ask,
    AutoDetect,
    KeepReadOnly,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoverySettings {
    pub enabled: bool,
    pub flush_interval_ms: u64,
    pub max_queued_age_ms: u64,
    pub checkpoint_interval_s: u64,
    pub checkpoint_delta_bytes: u64,
    pub checkpoint_delta_count: u64,
    pub closed_document_retention_days: u64,
    pub disk_soft_limit_bytes: u64,
}

impl Default for RecoverySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            flush_interval_ms: 1_000,
            max_queued_age_ms: 5_000,
            checkpoint_interval_s: 15,
            checkpoint_delta_bytes: 1024 * 1024,
            checkpoint_delta_count: 200,
            closed_document_retention_days: 7,
            disk_soft_limit_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSettings {
    pub on_manual_save: bool,
    pub on_batch_replace: bool,
    pub versions_per_file: u32,
    pub retention_days: u32,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            on_manual_save: false,
            on_batch_replace: true,
            versions_per_file: 5,
            retention_days: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_family: String,
    pub font_fallbacks: Vec<String>,
    pub font_size: f32,
    pub line_height: f32,
    pub tab_size: u32,
    pub insert_spaces: bool,
    pub soft_wrap: bool,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub highlight_current_line: bool,
    pub auto_indent: bool,
    pub auto_close_brackets: bool,
    pub show_trim_markers: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_family: "Consolas".to_string(),
            font_fallbacks: vec![
                "Cascadia Mono".to_string(),
                "Microsoft YaHei UI".to_string(),
            ],
            font_size: 14.0,
            line_height: 1.5,
            tab_size: 4,
            insert_spaces: true,
            soft_wrap: true,
            show_line_numbers: true,
            show_whitespace: false,
            highlight_current_line: true,
            auto_indent: true,
            auto_close_brackets: true,
            show_trim_markers: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub theme: ThemeMode,
    pub language: Language,
    pub density: Density,
    pub ui_font_family: String,
    pub ui_font_fallbacks: Vec<String>,
    pub ui_font_size: f32,
    pub sidebar_width: f32,
    pub restore_session: bool,
    pub show_file_tree: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            language: Language::ZhCn,
            density: Density::Compact,
            ui_font_family: "Segoe UI".to_string(),
            ui_font_fallbacks: vec!["Microsoft YaHei UI".to_string()],
            ui_font_size: 13.0,
            sidebar_width: 240.0,
            restore_session: true,
            show_file_tree: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en")]
    En,
}

impl Language {
    pub fn toggle(self) -> Self {
        match self {
            Self::ZhCn => Self::En,
            Self::En => Self::ZhCn,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::ZhCn => "中文",
            Self::En => "English",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

fn non_empty_font(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn clean_font_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_roundtrip() {
        let settings = Settings::default();
        let text = settings.to_toml().unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.files.default_encoding, EncodingId::Utf8);
        assert_eq!(back.editor.tab_size, 4);
        assert!(back.recovery.enabled);
    }

    #[test]
    fn missing_file_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(&dir.path().join("missing.toml")).unwrap();
        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
    }

    #[test]
    fn future_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "schema_version = 99\n").unwrap();
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[editor]\ntab_size = 0\nfont_size = 1000.0\n").unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.editor.tab_size, 1);
        assert_eq!(settings.editor.font_size, 72.0);
    }

    #[test]
    fn atomic_save_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut settings = Settings::default();
        settings.editor.tab_size = 8;
        settings.save_atomic(&path).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.editor.tab_size, 8);

        settings.editor.tab_size = 2;
        settings.save_atomic(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().editor.tab_size, 2);
    }

    #[test]
    fn invalid_toml_is_reported_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "not = [valid").unwrap();
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Parse(_))
        ));
    }

    #[test]
    fn font_settings_are_trimmed_and_get_defaults() {
        let mut settings = Settings::default();
        settings.editor.font_family = "   ".to_string();
        settings.editor.font_fallbacks = vec![
            " Consolas ".to_string(),
            "  ".to_string(),
            "Microsoft YaHei UI".to_string(),
        ];
        settings.ui.ui_font_family = " ".to_string();
        let normalized = settings.normalized();
        assert_eq!(normalized.editor.font_family, "Consolas");
        assert_eq!(
            normalized.editor.font_fallbacks,
            vec!["Consolas".to_string(), "Microsoft YaHei UI".to_string()]
        );
        assert_eq!(normalized.ui.ui_font_family, "Segoe UI");
    }
}
