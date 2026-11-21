//! Settings, session, save journal and crash recovery storage.

pub mod recovery;
pub mod settings;
pub mod worker;

pub use recovery::{
    RECOVERY_SCHEMA_VERSION, RecentFile, RecoveredDocument, RecoveryDb, SaveJournalEntry,
    SessionDocument, StorageError,
};
pub use settings::{
    BackupSettings, Density, EditorSettings, ExternalChangePolicy, FileSettings, Language,
    LegacyEncodingPolicy, NewDocumentEol, RecoverySettings, SETTINGS_SCHEMA_VERSION, Settings,
    SettingsError, ThemeMode, UiSettings,
};
pub use worker::{StorageSender, StorageTask, StorageWorker};
