use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::base::input::{
    EditorState, InputEditsEvent, InputEvent, InputState, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, TabSize,
};
use gpui_kit::base::{Editor, Input};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notepust_contracts::{
    BomPolicy, DocRevision, DocumentId, EncodingId, EncodingSpec, FileStamp, JobId,
    LogicalPosition, NativePath, RegexEngine, SearchPurpose, SearchRequest, SnapshotId, Utf8Range,
};
use notepust_file_io::{
    BackupPlan, ExternalChange, OpenError, OpenOptions, OpenedFile, SaveContent, SaveError,
    SaveRequest, SaveResult, detect_external_change, open_file, save_file,
};
use notepust_large_file::{
    IndexProgress, LogError, LogEvent, LogLine, LogReader, LogSearchHit, LogSearchOptions,
};
use notepust_platform::{ForwardedOpen, ProfilePaths};
use notepust_search::{
    FolderMatch, FolderSearchRequest, Pattern, ReplaceRequest, SearchError, SearchSpec,
    WorkerClient, WorkspaceFilterPolicy, apply_edits, find, parse_replacement, plan_replace_all,
    search_folder,
};
use notepust_storage::{
    EditorSettings, Language, RecentFile, RecoveredDocument, RecoveryDb, SessionDocument, Settings,
    StorageSender, StorageWorker, ThemeMode, UiSettings,
};
use notepust_text_codec::{DetectionSource, EolKind, EolSummary, LineIndex, summarize};
use ropey::LineType;

use crate::line_ops;
use crate::syntax;
use crate::theme::{self, Palette};

actions!(
    notepust,
    [
        NewDocument,
        OpenDocument,
        SaveDocument,
        SaveDocumentAs,
        SaveAllDocuments,
        ReloadDocument,
        CloseDocument,
        ToggleFind,
        ToggleReplace,
        GotoLine,
        QuickOpen,
        ToggleWhitespace,
        ToggleEncodingPanel,
        OpenSettings,
        ToggleSplit,
        DuplicateLine,
        DeleteLine,
        MoveLineUp,
        MoveLineDown,
        SortLines,
        TrimTrailingWhitespace,
        ToggleComment,
        IndentLines,
        OutdentLines,
        AddCursorAbove,
        AddCursorBelow,
        FindNextMatch,
        FindPreviousMatch,
        ReplaceAllMatches,
        FolderSearch,
        NextDocument,
        PreviousDocument,
        ToggleThemeMode,
        ToggleUiLanguage,
    ]
);

/// One open document and its view state.
struct DocumentTab {
    id: DocumentId,
    title: String,
    path: Option<PathBuf>,
    editor: Entity<EditorState>,
    encoding: EncodingSpec,
    detection: DetectionSource,
    stamp: Option<FileStamp>,
    raw_bytes: Vec<u8>,
    revision: DocRevision,
    last_saved_revision: DocRevision,
    dirty: bool,
    /// Kept only for small documents; used to clear dirty after undo.
    saved_text: Option<String>,
    eol_summary: EolSummary,
    preferred_eol: EolKind,
    position: LogicalPosition,
    warnings: Vec<String>,
    language: &'static str,
    save_in_flight: bool,
    reload_pending: bool,
    /// Present for read-only large-log tabs; the editor stays empty and is
    /// never rendered or focused.
    log: Option<LogTab>,
    from_recovery: bool,
    external_change: Option<String>,
    search_decorations: RangeDecorationCollection,
    _subscriptions: Vec<Subscription>,
}

impl DocumentTab {
    fn eol_label(&self) -> String {
        if self.eol_summary.total() == 0 {
            "—".to_string()
        } else if self.eol_summary.is_mixed() {
            format!(
                "混合/{}",
                self.eol_summary
                    .dominant()
                    .map(EolKind::label)
                    .unwrap_or("LF")
            )
        } else {
            self.eol_summary
                .dominant()
                .map(|eol| eol.label().to_string())
                .unwrap_or_else(|| "—".to_string())
        }
    }
}

struct SearchUi {
    open: bool,
    replace_mode: bool,
    query: Entity<InputState>,
    replacement: Entity<InputState>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    /// Route regex searches through the PCRE2 worker.
    advanced: bool,
    query_text: String,
    doc_id: Option<DocumentId>,
    revision: Option<DocRevision>,
    ranges: Vec<Utf8Range>,
    active: Option<usize>,
    status: String,
    pending: bool,
    valid: bool,
}

struct TabSeed {
    id: Option<DocumentId>,
    title: String,
    path: Option<PathBuf>,
    text: String,
    encoding: EncodingSpec,
    detection: DetectionSource,
    stamp: Option<FileStamp>,
    raw_bytes: Vec<u8>,
    dirty: bool,
    warnings: Vec<String>,
    from_recovery: bool,
    eol_summary: EolSummary,
    log: Option<LogTab>,
}

const LOG_PAGE_LINES: usize = 200;
const LOG_LINE_BYTES: usize = 16 * 1024;

struct LogTab {
    reader: Arc<Mutex<LogReader>>,
    cancel: Arc<AtomicBool>,
    lines: Vec<LogLine>,
    start_line: u64,
    total_lines: Option<u64>,
    progress: IndexProgress,
    following: bool,
    tail_mode: bool,
    status: String,
    highlight_line: Option<u64>,
}

struct LogSearchUi {
    hits: Vec<LogSearchHit>,
    active: usize,
    pending: bool,
    cancel: Option<Arc<AtomicBool>>,
    doc_id: Option<DocumentId>,
    query: String,
}

struct OpenedLog {
    reader: LogReader,
    lines: Vec<LogLine>,
    start_line: u64,
    total_lines: Option<u64>,
    progress: IndexProgress,
    tail_mode: bool,
}

enum OpenOutcome {
    Editable(OpenedFile),
    Log(OpenedLog),
}

#[derive(Debug)]
enum OpenTabError {
    Edit(OpenError),
    Log(LogError),
}

pub struct Workspace {
    tabs: Vec<DocumentTab>,
    active: Option<usize>,
    focus_handle: FocusHandle,
    settings: Settings,
    profile: ProfilePaths,
    status: String,
    search: SearchUi,
    goto_open: bool,
    goto_input: Entity<InputState>,
    next_untitled: u32,
    last_checkpoint: Instant,
    pending_close: Option<(DocumentId, Instant)>,
    pending_reload: Option<(DocumentId, Instant)>,
    storage_sender: Option<StorageSender>,
    _storage_worker: Option<StorageWorker>,
    initial_goto: Option<(u64, u64)>,
    subscriptions: Vec<Subscription>,
    folder: FolderUi,
    quick: QuickUi,
    encoding_ui: EncodingUi,
    split: Option<usize>,
    log_search: LogSearchUi,
    settings_open: bool,
    settings_ui_font: Entity<InputState>,
    settings_ui_fallbacks: Entity<InputState>,
    settings_ui_size: Entity<InputState>,
    settings_editor_font: Entity<InputState>,
    settings_editor_fallbacks: Entity<InputState>,
    settings_editor_size: Entity<InputState>,
    recent: Vec<RecentFile>,
    pending_opens: std::collections::HashSet<PathBuf>,
    forwarded: Option<Receiver<ForwardedOpen>>,
}

struct FolderUi {
    open: bool,
    pending: bool,
    root: Option<PathBuf>,
    matches: Vec<FolderMatch>,
    files_searched: u64,
    skipped: usize,
    status: String,
    cancel: Option<std::sync::Arc<AtomicBool>>,
}

struct QuickUi {
    open: bool,
    pending: bool,
    input: Entity<InputState>,
    root: Option<PathBuf>,
    files: Vec<PathBuf>,
    matches: Vec<PathBuf>,
    selected: usize,
    status: String,
    generation: u64,
}

struct EncodingUi {
    open: bool,
}

impl Workspace {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        initial_paths: Vec<PathBuf>,
        forwarded: Option<Receiver<ForwardedOpen>>,
    ) -> Self {
        let profile = notepust_platform::resolve_profile().unwrap_or_else(|error| {
            tracing::warn!(%error, "could not resolve the user profile; using ./.notepust");
            ProfilePaths {
                config_dir: PathBuf::from(".notepust/config"),
                data_dir: PathBuf::from(".notepust/data"),
                cache_dir: PathBuf::from(".notepust/cache"),
                portable: true,
            }
        });
        let mut status = String::new();
        if let Err(error) = profile.ensure() {
            status = format!("配置目录不可写: {error}");
        }
        let settings = Settings::load(&profile.settings_file()).unwrap_or_else(|error| {
            status = format!("设置读取失败，已使用默认值: {error}");
            Settings::default()
        });
        theme::install(cx, settings.ui.theme);
        theme::install_fonts(cx, &settings);

        let mut recovered = Vec::new();
        let mut session_paths = Vec::new();
        let mut recent = Vec::new();
        let mut storage_worker = None;
        if settings.recovery.enabled {
            let db_path = profile.recovery_db();
            if let Ok(mut db) = RecoveryDb::open(&db_path) {
                let _ = db.purge_older_than(settings.recovery.closed_document_retention_days);
                recovered = db.list_recoveries().unwrap_or_default();
                recent = db.list_recent(20).unwrap_or_default();
                if recovered.is_empty() && settings.ui.restore_session {
                    session_paths = db
                        .load_session()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|doc| doc.path.map(|path| path.to_path_buf()))
                        .take(20)
                        .collect();
                }
            }
            match StorageWorker::spawn(db_path) {
                Ok(worker) => storage_worker = Some(worker),
                Err(error) => {
                    status = format!("恢复库不可用: {error}");
                }
            }
        }

        let query = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("查找", window, cx);
            input
        });
        let replacement = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("替换为", window, cx);
            input
        });
        let goto_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("行号", window, cx);
            input
        });
        let quick_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_placeholder("输入文件名筛选…", window, cx);
            input
        });
        let settings_ui_font = new_input(window, cx, "界面字体");
        let settings_ui_fallbacks = new_input(window, cx, "备用字体，逗号分隔");
        let settings_ui_size = new_input(window, cx, "字号");
        let settings_editor_font = new_input(window, cx, "编辑字体");
        let settings_editor_fallbacks = new_input(window, cx, "备用字体，逗号分隔");
        let settings_editor_size = new_input(window, cx, "字号");

        cx.bind_keys([
            KeyBinding::new("ctrl-n", NewDocument, None),
            KeyBinding::new("ctrl-o", OpenDocument, None),
            KeyBinding::new("ctrl-s", SaveDocument, None),
            KeyBinding::new("ctrl-alt-s", SaveDocumentAs, None),
            KeyBinding::new("ctrl-shift-s", SaveAllDocuments, None),
            KeyBinding::new("ctrl-r", ReloadDocument, None),
            KeyBinding::new("ctrl-w", CloseDocument, None),
            KeyBinding::new("ctrl-f", ToggleFind, None),
            KeyBinding::new("ctrl-h", ToggleReplace, None),
            KeyBinding::new("ctrl-g", GotoLine, None),
            KeyBinding::new("ctrl-p", QuickOpen, None),
            KeyBinding::new("ctrl-alt-w", ToggleWhitespace, None),
            KeyBinding::new("ctrl-alt-e", ToggleEncodingPanel, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("ctrl-shift-d", DuplicateLine, None),
            KeyBinding::new("ctrl-shift-k", DeleteLine, None),
            KeyBinding::new("alt-up", MoveLineUp, None),
            KeyBinding::new("alt-down", MoveLineDown, None),
            KeyBinding::new("ctrl-shift-l", SortLines, None),
            KeyBinding::new("ctrl-shift-t", TrimTrailingWhitespace, None),
            KeyBinding::new("ctrl-/", ToggleComment, None),
            KeyBinding::new("ctrl-]", IndentLines, None),
            KeyBinding::new("ctrl-[", OutdentLines, None),
            KeyBinding::new("ctrl-alt-up", AddCursorAbove, None),
            KeyBinding::new("ctrl-alt-down", AddCursorBelow, None),
            KeyBinding::new("ctrl-shift-f", FolderSearch, None),
            KeyBinding::new("f3", FindNextMatch, None),
            KeyBinding::new("shift-f3", FindPreviousMatch, None),
            KeyBinding::new("ctrl-tab", NextDocument, None),
            KeyBinding::new("ctrl-shift-tab", PreviousDocument, None),
        ]);

        let storage_sender = storage_worker.as_ref().map(StorageWorker::sender);
        let mut workspace = Self {
            tabs: Vec::new(),
            active: None,
            focus_handle: cx.focus_handle(),
            settings,
            profile,
            status,
            search: SearchUi {
                open: false,
                replace_mode: false,
                query,
                replacement,
                case_sensitive: false,
                whole_word: false,
                regex: false,
                advanced: false,
                query_text: String::new(),
                doc_id: None,
                revision: None,
                ranges: Vec::new(),
                active: None,
                status: String::new(),
                pending: false,
                valid: false,
            },
            goto_open: false,
            goto_input,
            next_untitled: 1,
            last_checkpoint: Instant::now(),
            pending_close: None,
            pending_reload: None,
            storage_sender,
            _storage_worker: storage_worker,
            initial_goto: None,
            subscriptions: Vec::new(),
            folder: FolderUi {
                open: false,
                pending: false,
                root: None,
                matches: Vec::new(),
                files_searched: 0,
                skipped: 0,
                status: String::new(),
                cancel: None,
            },
            quick: QuickUi {
                open: false,
                pending: false,
                input: quick_input,
                root: None,
                files: Vec::new(),
                matches: Vec::new(),
                selected: 0,
                status: String::new(),
                generation: 0,
            },
            encoding_ui: EncodingUi { open: false },
            split: None,
            log_search: LogSearchUi {
                hits: Vec::new(),
                active: 0,
                pending: false,
                cancel: None,
                doc_id: None,
                query: String::new(),
            },
            settings_open: false,
            settings_ui_font,
            settings_ui_fallbacks,
            settings_ui_size,
            settings_editor_font,
            settings_editor_fallbacks,
            settings_editor_size,
            recent,
            pending_opens: std::collections::HashSet::new(),
            forwarded,
        };

        let query_handle = workspace.search.query.clone();
        let query_events = cx.subscribe(&query_handle, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.search_step(true, cx);
            }
        });
        workspace.subscriptions.push(query_events);
        let replacement_handle = workspace.search.replacement.clone();
        let replacement_events = cx.subscribe(
            &replacement_handle,
            |this, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.replace_all(cx);
                }
            },
        );
        workspace.subscriptions.push(replacement_events);

        let goto_handle = workspace.goto_input.clone();
        let goto_events = cx.subscribe(&goto_handle, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) && this.commit_goto(cx) {
                cx.spawn(async move |this, cx| {
                    this.update_in(cx, |this, window, cx| {
                        this.goto_open = false;
                        this.focus_editor(window, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        });
        workspace.subscriptions.push(goto_events);

        let quick_handle = workspace.quick.input.clone();
        let quick_events =
            cx.subscribe(
                &quick_handle,
                |this, _input, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => this.quick_accept(cx),
                    InputEvent::Change => this.quick_filter(cx),
                    _ => {}
                },
            );
        workspace.subscriptions.push(quick_events);

        let recovered_count = recovered.len();
        for document in recovered.into_iter().take(20) {
            workspace.restore_recovered(document, window, cx);
        }
        if recovered_count > 0 {
            workspace.status = format!(
                "已恢复 {recovered_count} 个未保存文档；它们不会覆盖原文件，保存时请另存为。"
            );
        } else if !initial_paths.is_empty() {
            for path in initial_paths.clone() {
                workspace.start_open(path, window, cx);
            }
        } else {
            for path in session_paths {
                if path.exists() {
                    workspace.start_open(path, window, cx);
                }
            }
        }

        workspace.start_background_loop(cx);
        workspace.start_forward_loop(cx);
        workspace.persist_session(cx);
        workspace
    }

    pub fn set_initial_goto(
        &mut self,
        line: u64,
        column: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.initial_goto = Some((line, column));
        if !self.tabs.is_empty() {
            self.apply_initial_goto(window, cx);
        }
    }

    // ---------------------------------------------------------------------
    // Tab creation and lifecycle
    // ---------------------------------------------------------------------

    fn add_tab(&mut self, seed: TabSeed, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let TabSeed {
            id,
            mut title,
            path,
            text,
            encoding,
            detection,
            stamp,
            raw_bytes,
            dirty,
            warnings,
            from_recovery,
            eol_summary,
            log,
        } = seed;

        let id = id.unwrap_or_default();

        if self.tabs.iter().any(|tab| tab.title == title)
            && let Some(parent) = path
                .as_ref()
                .and_then(|path| path.parent())
                .and_then(Path::file_name)
        {
            title = format!("{}/{}", parent.to_string_lossy(), title);
        }
        let mut counter = 2;
        while self.tabs.iter().any(|tab| tab.title == title) {
            title = format!("{title} ({counter})");
            counter += 1;
        }

        let editor_settings = self.settings.editor.clone();
        let preferred = eol_summary.preferred();
        let approximate_lines = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let large_document = text.len() > 4 * 1024 * 1024 || approximate_lines > 50_000;
        let highlight_language = if large_document {
            "text"
        } else {
            syntax::language_for_path(path.as_deref())
        };
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_value(text.clone(), window, cx);
            state.set_searchable(false, cx);
            state.set_soft_wrap(editor_settings.soft_wrap && !large_document, window, cx);
            state.set_line_number(editor_settings.show_line_numbers, window, cx);
            state.set_show_whitespaces(editor_settings.show_whitespace, window, cx);
            state.set_folding(!large_document, window, cx);
            state.set_tab_size(
                TabSize {
                    tab_size: editor_settings.tab_size as usize,
                    hard_tabs: !editor_settings.insert_spaces,
                },
                cx,
            );
            state.set_preferred_line_ending(
                match preferred {
                    EolKind::CrLf => gpui_kit::base::input::PreferredLineEnding::CrLf,
                    EolKind::Lf => gpui_kit::base::input::PreferredLineEnding::Lf,
                    EolKind::Cr => gpui_kit::base::input::PreferredLineEnding::Cr,
                },
                cx,
            );
            state.set_editor_paddings(Edges {
                top: px(10.),
                right: px(8.),
                bottom: px(10.),
                left: px(4.),
            });
            state
        });
        editor.update(cx, |state, cx| {
            syntax::configure(state, highlight_language, theme::palette(cx), cx);
        });

        let search_decorations = editor.update(cx, |state, cx| {
            state.create_range_decorations_collection(Vec::new(), cx)
        });

        let edits_subscription = cx.subscribe(
            &editor,
            move |this, _editor, _event: &InputEditsEvent, cx| {
                this.on_editor_changed(id, cx);
            },
        );
        let notify_subscription = cx.observe(&editor, move |this, _editor, cx| {
            this.on_editor_notified(id, cx);
        });

        let position = position_of(&editor, cx);
        let tab = DocumentTab {
            id,
            title,
            path,
            editor,
            encoding,
            detection,
            stamp,
            raw_bytes,
            revision: if dirty {
                DocRevision::new(1)
            } else {
                DocRevision::ZERO
            },
            last_saved_revision: DocRevision::ZERO,
            dirty,
            saved_text: if text.len() <= 2 * 1024 * 1024 && !dirty {
                Some(text)
            } else {
                None
            },
            eol_summary,
            preferred_eol: preferred,
            position,
            warnings,
            language: "Plain Text",
            save_in_flight: false,
            reload_pending: false,
            log,
            from_recovery,
            external_change: None,
            search_decorations,
            _subscriptions: vec![edits_subscription, notify_subscription],
        };

        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        self.tabs[index].language = detect_language(self.tabs[index].path.as_deref());
        self.active = Some(index);
        if self.tabs[index].log.is_some() {
            let handle = self.focus_handle.clone();
            window.focus(&handle, cx);
        } else {
            self.focus_editor(window, cx);
        }
        self.update_window_title(window);
        self.persist_session(cx);
        cx.notify();
        index
    }

    fn restore_recovered(
        &mut self,
        document: RecoveredDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display = document
            .display_path
            .clone()
            .or_else(|| document.path.as_ref().and_then(|path| path.file_name()))
            .unwrap_or_else(|| format!("未命名 {}", self.next_untitled));
        let title = format!("恢复: {display}");
        let eol_summary = summarize(&document.text);
        self.add_tab(
            TabSeed {
                id: Some(document.document_id),
                title,
                path: None,
                text: document.text,
                encoding: document.encoding,
                detection: DetectionSource::Explicit,
                stamp: None,
                raw_bytes: Vec::new(),
                dirty: true,
                warnings: vec![
                    "这是恢复副本，不会直接覆盖原文件；保存时会要求另存为。".to_string(),
                ],
                from_recovery: true,
                eol_summary,
                log: None,
            },
            window,
            cx,
        );
    }

    fn new_untitled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = format!("未命名 {}", self.next_untitled);
        self.next_untitled += 1;
        let spec = self.settings.default_encoding();
        let eol = self.settings.files.new_document_eol.resolved();
        let summary = EolSummary {
            crlf: usize::from(eol == EolKind::CrLf),
            lf: usize::from(eol == EolKind::Lf),
            cr: usize::from(eol == EolKind::Cr),
            first: Some(eol),
        };
        let index = self.add_tab(
            TabSeed {
                id: None,
                title,
                path: None,
                text: String::new(),
                encoding: spec,
                detection: DetectionSource::Explicit,
                stamp: None,
                raw_bytes: Vec::new(),
                dirty: false,
                warnings: Vec::new(),
                from_recovery: false,
                eol_summary: summary,
                log: None,
            },
            window,
            cx,
        );
        self.tabs[index].preferred_eol = eol;
        self.status = "新建文档".to_string();
        cx.notify();
    }

    /// Opens without blocking the UI thread. File reading, hashing and
    /// decoding happen on the background executor; the editor entity is
    /// created in the window context when the result arrives.
    fn start_open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.find_tab_by_path(&path) {
            self.activate(index, window, cx);
            return;
        }
        if !self.pending_opens.insert(path.clone()) {
            self.status = format!("{} 正在打开…", path.display());
            cx.notify();
            return;
        }
        self.status = format!("正在打开 {}…", path.display());
        cx.notify();
        let options = OpenOptions {
            allow_guess: true,
            max_bytes: self.settings.files.max_editable_bytes,
            ..Default::default()
        };
        let task = cx
            .background_executor()
            .spawn(async move { (path.clone(), open_tab_source(&path, &options)) });
        cx.spawn(async move |this, cx| {
            let (path, result) = task.await;
            this.update_in(cx, |this, window, cx| {
                this.pending_opens.remove(&path);
                this.on_open_result(path, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_open_result(
        &mut self,
        path: PathBuf,
        result: Result<OpenOutcome, OpenTabError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(outcome) => self.accept_open_outcome(path, outcome, window, cx),
            Err(error) => {
                self.status = format!("打开失败: {}", open_tab_error_label(&error));
                cx.notify();
            }
        }
    }

    fn accept_open_outcome(
        &mut self,
        path: PathBuf,
        outcome: OpenOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            OpenOutcome::Editable(opened) => self.accept_opened(path, opened, window, cx),
            OpenOutcome::Log(opened) => self.accept_log(path, opened, window, cx),
        }
    }

    fn accept_log(
        &mut self,
        path: PathBuf,
        opened: OpenedLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let encoding = opened.reader.spec().clone();
        let following = opened.tail_mode;
        let status = if opened.tail_mode {
            "已从文件尾部打开（未完成全量行索引）".to_string()
        } else {
            "已打开只读日志视图".to_string()
        };
        let log = LogTab {
            progress: opened.progress,
            reader: Arc::new(Mutex::new(opened.reader)),
            cancel: Arc::new(AtomicBool::new(false)),
            lines: opened.lines,
            start_line: opened.start_line,
            total_lines: opened.total_lines,
            following,
            tail_mode: opened.tail_mode,
            status,
            highlight_line: None,
        };
        let index = self.add_tab(
            TabSeed {
                id: None,
                title,
                path: Some(path.clone()),
                text: String::new(),
                encoding,
                detection: DetectionSource::Explicit,
                stamp: None,
                raw_bytes: Vec::new(),
                dirty: false,
                warnings: vec![
                    "只读日志模式：不会把整个文件装入编辑缓冲，也不能直接保存。".to_string(),
                ],
                from_recovery: false,
                eol_summary: summarize(""),
                log: Some(log),
            },
            window,
            cx,
        );
        let document_id = self.tabs[index].id;
        let reader = self.tabs[index]
            .log
            .as_ref()
            .map(|log| log.reader.clone())
            .expect("log tab just created");
        let cancel = self.tabs[index]
            .log
            .as_ref()
            .map(|log| log.cancel.clone())
            .expect("log tab just created");
        self.status = format!(
            "已以只读日志模式打开 {}（{}，已扫描 {} 行）",
            path.display(),
            self.tabs[index].encoding.status_label(),
            self.tabs[index]
                .log
                .as_ref()
                .map(|log| log.progress.lines_found)
                .unwrap_or(0)
        );
        if let Some(sender) = &self.storage_sender {
            let native = NativePath::from_path(&path);
            sender.try_recent(native, path.display().to_string());
        }
        self.spawn_log_indexer(document_id, reader, cancel, cx);
        self.remember_recent(&path);
        self.apply_initial_goto(window, cx);
        cx.notify();
    }

    fn spawn_log_indexer(
        &self,
        document_id: DocumentId,
        reader: Arc<Mutex<LogReader>>,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let weak = Arc::downgrade(&reader);
        cx.spawn(async move |this, cx| {
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let progress = {
                    let Some(reader) = weak.upgrade() else {
                        break;
                    };
                    let Ok(mut guard) = reader.lock() else {
                        break;
                    };
                    if guard.progress().complete {
                        break;
                    }
                    for _ in 0..8 {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        if guard.scan_step().is_err() {
                            break;
                        }
                        if guard.progress().complete {
                            break;
                        }
                    }
                    guard.progress()
                };
                let updated = this
                    .update(cx, |this, cx| {
                        if let Some(index) = this.index_of(document_id)
                            && let Some(log) = this.tabs[index].log.as_mut()
                        {
                            log.progress = progress;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !updated {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            }
        })
        .detach();
    }

    fn open_sync(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.find_tab_by_path(&path) {
            self.activate(index, window, cx);
            return;
        }
        if self.pending_opens.contains(&path) {
            self.status = format!("{} 正在打开…", path.display());
            cx.notify();
            return;
        }
        self.status = format!("正在打开 {}", path.display());
        cx.notify();
        let options = OpenOptions {
            allow_guess: true,
            max_bytes: self.settings.files.max_editable_bytes,
            ..Default::default()
        };
        match open_tab_source(&path, &options) {
            Ok(outcome) => self.accept_open_outcome(path, outcome, window, cx),
            Err(error) => {
                self.status = format!("打开失败: {}", open_tab_error_label(&error));
                cx.notify();
            }
        }
    }

    fn accept_opened(
        &mut self,
        path: PathBuf,
        opened: OpenedFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let eol_summary = summarize(&opened.text);
        let warnings = opened.warnings.clone();
        self.add_tab(
            TabSeed {
                id: None,
                title,
                path: Some(path.clone()),
                text: opened.text,
                encoding: opened.encoding,
                detection: opened.detection,
                stamp: Some(opened.stamp_after),
                raw_bytes: opened.raw_bytes,
                dirty: false,
                warnings,
                from_recovery: false,
                eol_summary,
                log: None,
            },
            window,
            cx,
        );
        self.remember_recent(&path);
        if let Some(sender) = &self.storage_sender {
            let native = NativePath::from_path(&path);
            sender.try_recent(native, path.display().to_string());
        }
        let (path_label, warnings) = self
            .tabs
            .last()
            .map(|tab| (self.status_path(tab), tab.warnings.clone()))
            .unwrap_or_default();
        self.status = if warnings.is_empty() {
            format!("已打开 {path_label}")
        } else {
            format!("已打开 {path_label}：{}", warnings.join(" "))
        };
        self.apply_initial_goto(window, cx);
        cx.notify();
    }

    fn remember_recent(&mut self, path: &Path) {
        let native = NativePath::from_path(path);
        self.recent.retain(|file| file.path != native);
        self.recent.insert(
            0,
            RecentFile {
                path: native,
                display_path: path.display().to_string(),
                opened_at: 0,
            },
        );
        self.recent.truncate(20);
    }

    fn apply_initial_goto(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((line, column)) = self.initial_goto else {
            return;
        };
        self.initial_goto = None;
        if let Some(index) = self.active {
            self.goto_position(index, line, column, cx);
        }
    }

    fn goto_position(&mut self, index: usize, line: u64, column: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.log.is_some() {
            self.log_show(index, line.saturating_sub(1), cx);
            return;
        }
        let text = tab.editor.read(cx).value().to_string();
        let lines = LineIndex::new(&text);
        let position = LogicalPosition::new(line.saturating_sub(1), column.saturating_sub(1));
        let offset = lines.offset_of_position(&text, position);
        let editor = tab.editor.clone();
        editor.update(cx, |state, cx| {
            state.set_selected_range(offset as usize..offset as usize, cx);
        });
        self.refresh_position(index, cx);
    }

    fn find_tab_by_path(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|tab| {
            tab.path
                .as_ref()
                .map(|existing| paths_equal(existing, path))
                .unwrap_or(false)
        })
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if self.split == Some(index) {
            self.split = self.active;
        }
        self.active = Some(index);
        if self.split == self.active {
            self.split = None;
        }
        self.pending_close = None;
        self.pending_reload = None;
        self.check_external_change(index, cx);
        self.focus_editor(window, cx);
        self.update_window_title(window);
        self.persist_session(cx);
        cx.notify();
    }

    fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if self.split == Some(index) {
            self.split = self.active;
        }
        self.active = Some(index);
        if self.split == self.active {
            self.split = None;
        }
        self.pending_close = None;
        self.pending_reload = None;
        self.focus_editor(window, cx);
        self.update_window_title(window);
        cx.notify();
    }

    fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split.take().is_some() {
            self.status = "已关闭分屏".to_string();
            self.focus_editor(window, cx);
            cx.notify();
            return;
        }
        if self.tabs.len() < 2 {
            self.status = "至少需要两个标签才能分屏".to_string();
            cx.notify();
            return;
        }
        let active = self.active.unwrap_or(0);
        self.split = Some((active + 1) % self.tabs.len());
        self.status = "已分屏：点击窗格切换当前文档".to_string();
        cx.notify();
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let id = self.tabs[index].id;
        if self.tabs[index].dirty {
            let now = Instant::now();
            let confirmed = self
                .pending_close
                .map(|(pending_id, at)| {
                    pending_id == id && now.duration_since(at) < Duration::from_secs(4)
                })
                .unwrap_or(false);
            if !confirmed {
                self.pending_close = Some((id, now));
                self.status =
                    "文档有未保存修改：再次按 Ctrl+W 或再次点击 × 将丢弃修改。".to_string();
                cx.notify();
                return;
            }
        }
        self.force_close(index, window, cx);
    }

    fn force_close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if self.log_search.doc_id == Some(tab.id) {
            if let Some(cancel) = self.log_search.cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.log_search.hits.clear();
            self.log_search.pending = false;
            self.log_search.doc_id = None;
        }
        if tab.from_recovery
            && let Some(sender) = &self.storage_sender
        {
            sender.try_remove_recovery(tab.id);
        }
        self.pending_close = None;
        self.split = match self.split {
            Some(split) if split == index => None,
            Some(split) if split > index => Some(split - 1),
            other => other,
        };
        let closing_active = self.active == Some(index);
        self.active = if self.tabs.is_empty() {
            None
        } else if closing_active {
            Some(index.min(self.tabs.len() - 1))
        } else {
            self.active
                .map(|active| if active > index { active - 1 } else { active })
        };
        if self.split == self.active {
            self.split = None;
        }
        self.search.open = false;
        self.search.ranges.clear();
        self.search.valid = false;
        self.focus_editor(window, cx);
        self.update_window_title(window);
        self.persist_session(cx);
        cx.notify();
    }

    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(index);
        self.close_active(window, cx);
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active
            && let Some(tab) = self.tabs.get(index)
        {
            if tab.log.is_some() {
                let handle = self.focus_handle.clone();
                window.focus(&handle, cx);
                return;
            }
            let handle = tab.editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn update_window_title(&self, window: &mut Window) {
        window.set_window_title(&self.window_title());
    }

    fn window_title(&self) -> String {
        match self.active.and_then(|index| self.tabs.get(index)) {
            Some(tab) => format!(
                "{}{} - Notepust",
                tab.title,
                if tab.dirty { " *" } else { "" }
            ),
            None => "Notepust".to_string(),
        }
    }

    // ---------------------------------------------------------------------
    // Editor events and status
    // ---------------------------------------------------------------------

    fn index_of(&self, id: DocumentId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn on_editor_changed(&mut self, id: DocumentId, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        self.tabs[index].revision = self.tabs[index].revision.next();
        self.tabs[index].dirty = true;
        if self.search.doc_id == Some(id) {
            self.search.valid = false;
            self.search.ranges.clear();
            self.search.active = None;
            self.tabs[index].search_decorations.set(Vec::new(), cx);
        }
        self.refresh_position(index, cx);
        self.pending_close = None;
        cx.notify();
    }

    fn on_editor_notified(&mut self, id: DocumentId, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        if self.refresh_position(index, cx) {
            cx.notify();
        }
    }

    fn refresh_position(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let position = position_of(&tab.editor, cx);
        if position != tab.position {
            self.tabs[index].position = position;
            true
        } else {
            false
        }
    }

    /// Clears dirty after an undo returns a small document to its saved text.
    /// Larger documents keep the conservative dirty flag until the next save;
    /// comparing 100 MB on every idle tick is not acceptable.
    fn refresh_clean_state(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for tab in &mut self.tabs {
            if !tab.dirty {
                continue;
            }
            let Some(saved) = &tab.saved_text else {
                continue;
            };
            let text = tab.editor.read(cx).value().to_string();
            if text == *saved {
                tab.dirty = false;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ---------------------------------------------------------------------
    // Search and replacement
    // ---------------------------------------------------------------------

    fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.search.open = true;
        self.search.replace_mode = replace;
        let selected = self.active.and_then(|index| {
            let state = self.tabs.get(index)?.editor.read(cx);
            let range = state.selected_range();
            if range.is_empty() {
                return None;
            }
            let text = state.text().slice(range).to_string();
            (!text.is_empty() && text.len() < 512 && !text.contains(['\n', '\r'])).then_some(text)
        });
        if let Some(text) = selected {
            let query = self.search.query.clone();
            query.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        let handle = self.search.query.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancel) = self.log_search.cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.log_search.hits.clear();
        self.log_search.pending = false;
        self.log_search.doc_id = None;
        self.log_search.query.clear();
        if let Some(index) = self.active
            && let Some(log) = self.tabs[index].log.as_mut()
        {
            log.highlight_line = None;
        }
        self.search.open = false;
        self.search.ranges.clear();
        self.search.valid = false;
        self.search.active = None;
        self.search.status.clear();
        if let Some(index) = self.active {
            self.tabs[index].search_decorations.set(Vec::new(), cx);
        }
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_none() {
            return;
        }
        self.goto_open = true;
        let input = self.goto_input.clone();
        input.update(cx, |state, cx| state.set_value("", window, cx));
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn close_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.goto_open = false;
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn commit_goto(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.goto_input.read(cx).value().trim().to_string();
        let Ok(line) = text.parse::<u64>() else {
            self.status = "请输入有效行号".to_string();
            cx.notify();
            return false;
        };
        let line = line.max(1);
        if let Some(index) = self.active {
            let is_log = self.tabs[index].log.is_some();
            self.goto_position(index, line, 1, cx);
            if !is_log {
                self.status = format!("跳转到第 {line} 行");
            }
        }
        cx.notify();
        true
    }

    fn open_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.path.as_ref())
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| self.profile.data_dir.clone());
        let input = self.quick.input.clone();
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.quick.open = true;
        self.quick.selected = 0;
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        if self.quick.root.as_ref() == Some(&root) && !self.quick.files.is_empty() {
            self.quick_filter(cx);
            return;
        }
        self.quick.root = Some(root.clone());
        self.quick.pending = true;
        self.quick.files.clear();
        self.quick.matches.clear();
        self.quick.status = format!("正在扫描 {}…", root.display());
        self.quick.generation = self.quick.generation.wrapping_add(1);
        let generation = self.quick.generation;
        let scan_root = root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { collect_quick_files(&scan_root) });
        cx.spawn(async move |this, cx| {
            let files = task.await;
            this.update(cx, |this, cx| {
                if this.quick.generation != generation || !this.quick.open {
                    return;
                }
                this.quick.pending = false;
                this.quick.files = files;
                this.quick_filter(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick.open = false;
        self.quick.matches.clear();
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn quick_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.quick.input.read(cx).value().to_string();
        let matches = filter_quick_files(&self.quick.files, &query, 100);
        self.quick.selected = 0;
        self.quick.matches = matches;
        self.quick.status = if self.quick.pending {
            "正在扫描…".to_string()
        } else {
            format!(
                "{} 个匹配 / {} 个文件",
                self.quick.matches.len(),
                self.quick.files.len()
            )
        };
        cx.notify();
    }

    fn quick_accept(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.quick.root.clone() else {
            return;
        };
        let Some(relative) = self
            .quick
            .matches
            .get(self.quick.selected)
            .or_else(|| self.quick.matches.first())
            .cloned()
        else {
            self.quick.status = "没有可打开的文件".to_string();
            cx.notify();
            return;
        };
        let target = root.join(relative);
        self.quick.open = false;
        self.status = format!("正在打开 {}", target.display());
        cx.spawn(async move |this, cx| {
            this.update_in(cx, |this, window, cx| {
                this.focus_editor(window, cx);
                this.start_open(target, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn quick_click(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.quick.root.clone() else {
            return;
        };
        let Some(relative) = self.quick.matches.get(index).cloned() else {
            return;
        };
        let target = root.join(relative);
        self.quick.open = false;
        self.status = format!("正在打开 {}", target.display());
        self.focus_editor(window, cx);
        self.start_open(target, window, cx);
    }

    fn toggle_encoding_panel(&mut self, cx: &mut Context<Self>) {
        self.encoding_ui.open = !self.encoding_ui.open;
        cx.notify();
    }

    fn reinterpret_encoding(&mut self, spec: EncodingSpec, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式暂不支持重新解释编码".to_string();
            cx.notify();
            return;
        }
        if self.tabs[index].dirty {
            self.status = "有未保存修改：先保存或从磁盘重载，再更改编码".to_string();
            cx.notify();
            return;
        }
        let Some(path) = self.tabs[index].path.clone() else {
            self.status = "未命名文档没有可用于重新解释的原始字节".to_string();
            cx.notify();
            return;
        };
        if self.tabs[index].reload_pending || self.tabs[index].save_in_flight {
            self.status = "已有读取或保存操作进行中".to_string();
            cx.notify();
            return;
        }
        let document_id = self.tabs[index].id;
        self.tabs[index].reload_pending = true;
        self.status = format!("正在按 {} 重新读取…", spec.status_label());
        cx.notify();
        let options = OpenOptions {
            explicit_encoding: Some(spec.clone()),
            allow_guess: false,
            max_bytes: self.settings.files.max_editable_bytes,
            ..Default::default()
        };
        let task = cx.background_executor().spawn(async move {
            let result = open_file(&path, &options);
            (spec, result)
        });
        cx.spawn(async move |this, cx| {
            let (spec, result) = task.await;
            this.update_in(cx, |this, window, cx| {
                this.on_reinterpret_result(document_id, spec, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_reinterpret_result(
        &mut self,
        document_id: DocumentId,
        spec: EncodingSpec,
        result: Result<OpenedFile, OpenError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(document_id) else {
            return;
        };
        self.tabs[index].reload_pending = false;
        match result {
            Ok(opened) => {
                let status = format!("已按 {} 重新解释（文档未修改）", spec.status_label());
                self.apply_loaded_document(index, opened, status, window, cx);
            }
            Err(error) => {
                self.status = format!("重新解释失败: {}", open_error_label(&error));
                cx.notify();
            }
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = true;
        let ui_font = self.settings.ui.ui_font_family.clone();
        let ui_fallbacks = self.settings.ui.ui_font_fallbacks.join(", ");
        let ui_size = format!("{:.1}", self.settings.ui.ui_font_size);
        let editor_font = self.settings.editor.font_family.clone();
        let editor_fallbacks = self.settings.editor.font_fallbacks.join(", ");
        let editor_size = format!("{:.1}", self.settings.editor.font_size);
        self.fill_input(&self.settings_ui_font, ui_font, window, cx);
        self.fill_input(&self.settings_ui_fallbacks, ui_fallbacks, window, cx);
        self.fill_input(&self.settings_ui_size, ui_size, window, cx);
        self.fill_input(&self.settings_editor_font, editor_font, window, cx);
        self.fill_input(
            &self.settings_editor_fallbacks,
            editor_fallbacks,
            window,
            cx,
        );
        self.fill_input(&self.settings_editor_size, editor_size, window, cx);
        let handle = self.settings_editor_font.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn fill_input(
        &self,
        input: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let ui_font = self.settings_ui_font.read(cx).value().trim().to_string();
        if !ui_font.is_empty() {
            self.settings.ui.ui_font_family = ui_font;
        }
        let ui_fallbacks = parse_font_list(&self.settings_ui_fallbacks.read(cx).value());
        if !ui_fallbacks.is_empty() {
            self.settings.ui.ui_font_fallbacks = ui_fallbacks;
        }
        if let Ok(size) = self.settings_ui_size.read(cx).value().trim().parse::<f32>() {
            self.settings.ui.ui_font_size = size;
        }

        let editor_font = self
            .settings_editor_font
            .read(cx)
            .value()
            .trim()
            .to_string();
        if !editor_font.is_empty() {
            self.settings.editor.font_family = editor_font;
        }
        let editor_fallbacks = parse_font_list(&self.settings_editor_fallbacks.read(cx).value());
        if !editor_fallbacks.is_empty() {
            self.settings.editor.font_fallbacks = editor_fallbacks;
        }
        if let Ok(size) = self
            .settings_editor_size
            .read(cx)
            .value()
            .trim()
            .parse::<f32>()
        {
            self.settings.editor.font_size = size;
        }

        self.settings = std::mem::take(&mut self.settings).normalized();
        let _ = self.settings.save_atomic(&self.profile.settings_file());
        theme::install_fonts(cx, &self.settings);
        let palette = theme::palette(cx);
        for tab in &self.tabs {
            let editor = tab.editor.clone();
            editor.update(cx, |state, _cx| syntax::refresh_style(state, palette));
        }
        self.status = "字体设置已应用".to_string();
        cx.notify();
    }

    fn reset_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor_defaults = EditorSettings::default();
        self.settings.editor.font_family = editor_defaults.font_family;
        self.settings.editor.font_fallbacks = editor_defaults.font_fallbacks;
        self.settings.editor.font_size = editor_defaults.font_size;
        let ui_defaults = UiSettings::default();
        self.settings.ui.ui_font_family = ui_defaults.ui_font_family;
        self.settings.ui.ui_font_fallbacks = ui_defaults.ui_font_fallbacks;
        self.settings.ui.ui_font_size = ui_defaults.ui_font_size;
        self.open_settings(window, cx);
        self.apply_settings(cx);
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn input_box(&self, input: &Entity<InputState>, width: f32, palette: Palette) -> AnyElement {
        div()
            .h(px(26.))
            .w(px(width))
            .px_2()
            .rounded(px(4.))
            .bg(palette.input_bg)
            .border_1()
            .border_color(palette.border)
            .child(Input::new(input))
            .into_any_element()
    }

    fn render_settings_panel(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let label = |text: &'static str| {
            div()
                .w(px(72.))
                .text_size(px(11.5))
                .text_color(palette.muted)
                .child(text)
                .into_any_element()
        };
        let ui_row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(label(tr(lang, "界面字体", "UI font")))
            .child(self.input_box(&self.settings_ui_font, 190., palette))
            .child(self.input_box(&self.settings_ui_fallbacks, 260., palette))
            .child(self.input_box(&self.settings_ui_size, 64., palette));
        let editor_row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(label(tr(lang, "编辑字体", "Editor font")))
            .child(self.input_box(&self.settings_editor_font, 190., palette))
            .child(self.input_box(&self.settings_editor_fallbacks, 260., palette))
            .child(self.input_box(&self.settings_editor_size, 64., palette));
        let buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.action_button(
                "settings-apply".to_string(),
                tr(lang, "应用", "Apply"),
                palette,
                cx,
                |this, _event, _window, cx| this.apply_settings(cx),
            ))
            .child(self.action_button(
                "settings-reset".to_string(),
                tr(lang, "恢复默认", "Reset"),
                palette,
                cx,
                |this, _event, window, cx| this.reset_settings(window, cx),
            ))
            .child(div().flex_1())
            .child(self.action_button(
                "settings-close".to_string(),
                "×",
                palette,
                cx,
                |this, _event, window, cx| this.close_settings(window, cx),
            ));

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.fg)
                    .child(tr(lang, "设置 · 字体", "Settings · Fonts")),
            )
            .child(ui_row)
            .child(editor_row)
            .child(buttons)
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(palette.muted)
                    .child(tr(
                        lang,
                        "备用字体按顺序生效，用于缺字回退；设置会写入 settings.toml。",
                        "Fallbacks apply in order for missing glyphs; settings are written to settings.toml.",
                    )),
            )
            .into_any_element()
    }

    fn save_with_encoding(&mut self, spec: EncodingSpec, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式不能更改保存编码".to_string();
            cx.notify();
            return;
        }
        if self.tabs[index].encoding == spec {
            self.status = format!("保存编码已经是 {}", spec.status_label());
            cx.notify();
            return;
        }
        let tab = &mut self.tabs[index];
        tab.encoding = spec;
        tab.detection = DetectionSource::Explicit;
        tab.revision = tab.revision.next();
        tab.dirty = true;
        tab.saved_text = None;
        self.status = format!(
            "保存编码已改为 {}；按 Ctrl+S 写入（不可编码字符会报错而不落盘）",
            self.tabs[index].encoding.status_label()
        );
        cx.notify();
    }

    fn search_input_text(&self, cx: &App) -> String {
        self.search.query.read(cx).value().to_string()
    }

    fn search_options(&self) -> notepust_contracts::SearchOptions {
        notepust_contracts::SearchOptions {
            case_sensitive: self.search.case_sensitive,
            whole_word: self.search.whole_word,
            dot_all: false,
            multi_line: true,
            crlf: true,
        }
    }

    fn advanced_mode(&self) -> bool {
        self.search.regex && self.search.advanced
    }

    fn search_spec(&self, pattern: String) -> SearchSpec {
        SearchSpec {
            pattern: if self.search.regex {
                Pattern::Regex {
                    pattern,
                    advanced: false,
                }
            } else {
                Pattern::Literal(pattern)
            },
            options: self.search_options(),
        }
    }

    fn search_step(&mut self, from_cursor: bool, cx: &mut Context<Self>) {
        let query = self.search_input_text(cx);
        let Some(index) = self.active else {
            return;
        };
        if query.is_empty() {
            self.search.ranges.clear();
            self.search.valid = false;
            self.search.status.clear();
            self.tabs[index].search_decorations.set(Vec::new(), cx);
            cx.notify();
            return;
        }
        let doc_id = self.tabs[index].id;
        let revision = self.tabs[index].revision;
        if self.tabs[index].log.is_some() {
            self.log_search_step(query, cx);
            return;
        }
        if self.search.valid
            && self.search.doc_id == Some(doc_id)
            && self.search.revision == Some(revision)
            && self.search.query_text == query
            && !self.search.ranges.is_empty()
        {
            self.advance_match(1, cx);
            return;
        }

        let text = self.tabs[index].editor.read(cx).value().to_string();
        let cursor = self.tabs[index].editor.read(cx).selected_range().end as u64;
        let spec = self.search_spec(query.clone());
        let advanced = self.advanced_mode();
        let options = self.search_options();
        let task_query = query.clone();
        self.search.pending = true;
        self.search.status = "搜索中…".to_string();
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            if advanced {
                advanced_find(&text, &task_query, options, 10_000)
            } else {
                find(&text, &spec, 10_000, false, &AtomicBool::new(false))
                    .map(|outcome| outcome.hits.into_iter().map(|hit| hit.range).collect())
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, _window, cx| {
                this.search.pending = false;
                let Some(index) = this.index_of(doc_id) else {
                    return;
                };
                if this.tabs[index].revision != revision {
                    this.search.valid = false;
                    this.search.status = "文档已修改，请重新搜索。".to_string();
                    cx.notify();
                    return;
                }
                match result {
                    Ok(ranges) => {
                        this.search.ranges = ranges;
                        this.search.active = None;
                        this.search.valid = true;
                        this.search.doc_id = Some(doc_id);
                        this.search.revision = Some(revision);
                        this.search.query_text = query.clone();
                        if this.search.ranges.is_empty() {
                            this.search.status = "0 项".to_string();
                            this.tabs[index].search_decorations.set(Vec::new(), cx);
                        } else {
                            let active = if from_cursor {
                                this.search
                                    .ranges
                                    .iter()
                                    .position(|range| range.start >= cursor)
                                    .unwrap_or(0)
                            } else {
                                0
                            };
                            this.select_search_match(index, active, cx);
                        }
                    }
                    Err(error) => {
                        this.search.valid = false;
                        this.search.status = search_error_label(&error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn advance_match(&mut self, direction: i32, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !self.search.valid || self.search.ranges.is_empty() {
            return;
        }
        let total = self.search.ranges.len();
        let current = self
            .search
            .active
            .unwrap_or(if direction > 0 { total - 1 } else { 0 });
        let next = if direction > 0 {
            (current + 1) % total
        } else if current == 0 {
            total - 1
        } else {
            current - 1
        };
        self.select_search_match(index, next, cx);
    }

    fn select_search_match(&mut self, index: usize, active: usize, cx: &mut Context<Self>) {
        let Some(range) = self.search.ranges.get(active).copied() else {
            return;
        };
        self.search.active = Some(active);
        let editor = self.tabs[index].editor.clone();
        editor.update(cx, |state, cx| {
            state.set_selected_range_with_direction(
                range.start as usize..range.end as usize,
                false,
                cx,
            );
        });
        self.refresh_search_decorations(index, cx);
        self.search.status = format!("{} / {}", active + 1, self.search.ranges.len());
        cx.notify();
    }

    fn refresh_search_decorations(&mut self, index: usize, cx: &mut Context<Self>) {
        let palette = theme::palette(cx);
        let active = self.search.active;
        let decorations: Vec<RangeDecoration> = self
            .search
            .ranges
            .iter()
            .take(2_000)
            .enumerate()
            .map(|(position, range)| {
                RangeDecoration::new(range.start as usize..range.end as usize)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(if Some(position) == active {
                        palette.active_match
                    } else {
                        palette.match_bg
                    })
            })
            .collect();
        self.tabs[index].search_decorations.set(decorations, cx);
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input_text(cx);
        if query.is_empty() {
            self.search.status = "请输入查找内容".to_string();
            cx.notify();
            return;
        }
        let replacement = self.search.replacement.read(cx).value().to_string();
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.search.pending = false;
            self.search.status = "只读日志模式暂不支持替换".to_string();
            cx.notify();
            return;
        }
        let doc_id = self.tabs[index].id;
        let revision = self.tabs[index].revision;
        let text = self.tabs[index].editor.read(cx).value().to_string();
        let spec = self.search_spec(query.clone());
        let interpret_escapes = self.search.regex;
        let advanced = self.advanced_mode();
        let options = self.search_options();
        self.search.pending = true;
        self.search.status = "替换中…".to_string();
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            if advanced {
                let program =
                    parse_replacement(&replacement, interpret_escapes).map_err(|error| {
                        SearchError::InvalidPattern {
                            detail: error.to_string(),
                        }
                    })?;
                advanced_replace(&text, &query, options, program)
            } else {
                let request = ReplaceRequest {
                    spec: &spec,
                    replacement: &replacement,
                    interpret_escapes,
                    preview_limit: 200,
                    max_edits: 1_000_000,
                };
                let plan = plan_replace_all(&text, &request, &AtomicBool::new(false))?;
                let output = apply_edits(&text, &plan.edits)?;
                Ok::<(String, u64), SearchError>((output, plan.match_count))
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.search.pending = false;
                let Some(index) = this.index_of(doc_id) else {
                    return;
                };
                if this.tabs[index].revision != revision {
                    this.search.valid = false;
                    this.search.status = "文档已修改，已取消替换。".to_string();
                    cx.notify();
                    return;
                }
                match result {
                    Ok((output, count)) => {
                        if count == 0 {
                            this.search.status = "没有可替换的匹配".to_string();
                        } else {
                            let editor = this.tabs[index].editor.clone();
                            editor.update(cx, |state, cx| {
                                state.replace_all(output, window, cx);
                                state.set_selected_range(0..0, cx);
                            });
                            this.search.valid = false;
                            this.search.ranges.clear();
                            this.search.active = None;
                            this.tabs[index].search_decorations.set(Vec::new(), cx);
                            this.search.status = format!("已替换 {count} 处");
                        }
                    }
                    Err(error) => {
                        this.search.status = search_error_label(&error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn folder_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input_text(cx);
        if query.is_empty() {
            self.search.open = true;
            self.status = "请先在查找框输入内容，再执行文件夹搜索".to_string();
            cx.notify();
            return;
        }
        let root = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.path.as_ref())
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| self.profile.data_dir.clone());
        if let Some(previous) = self.folder.cancel.take() {
            previous.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        self.folder.cancel = Some(cancel.clone());
        self.folder.open = true;
        self.folder.pending = true;
        self.folder.root = Some(root.clone());
        self.folder.matches.clear();
        self.folder.files_searched = 0;
        self.folder.skipped = 0;
        self.folder.status = format!("正在搜索 {}…", root.display());
        cx.notify();

        let spec = self.search_spec(query);
        let request = FolderSearchRequest {
            root,
            spec,
            policy: WorkspaceFilterPolicy::default(),
            max_results: 500,
        };
        let task_cancel = cancel.clone();
        let task = cx
            .background_executor()
            .spawn(async move { search_folder(&request, &task_cancel) });
        let expected_cancel = cancel;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, _window, cx| {
                let current = this
                    .folder
                    .cancel
                    .as_ref()
                    .map(|current| std::sync::Arc::ptr_eq(current, &expected_cancel))
                    .unwrap_or(false);
                if !current {
                    return;
                }
                this.folder.pending = false;
                match result {
                    Ok(report) => {
                        this.folder.matches = report.matches;
                        this.folder.files_searched = report.files_searched;
                        this.folder.skipped = report.skipped.len();
                        this.folder.status = format!(
                            "{} 项，{} 个文件{}",
                            this.folder.matches.len(),
                            this.folder.files_searched,
                            if report.truncated {
                                "（结果已截断）"
                            } else if report.cancelled {
                                "（已取消）"
                            } else {
                                ""
                            }
                        );
                        if this.folder.skipped > 0 {
                            this.folder
                                .status
                                .push_str(&format!("，跳过 {} 个文件", this.folder.skipped));
                        }
                    }
                    Err(error) => {
                        this.folder.matches.clear();
                        this.folder.status = search_error_label(&error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_folder_match(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.folder.matches.get(index).cloned() else {
            return;
        };
        if self.find_tab_by_path(&hit.path).is_none() {
            self.open_sync(hit.path.clone(), window, cx);
        } else if let Some(tab_index) = self.find_tab_by_path(&hit.path) {
            self.activate(tab_index, window, cx);
        }
        if let Some(tab_index) = self.find_tab_by_path(&hit.path) {
            self.goto_position(tab_index, hit.line, 1, cx);
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    fn toggle_case(&mut self, cx: &mut Context<Self>) {
        self.search.case_sensitive = !self.search.case_sensitive;
        self.search.valid = false;
        self.search_step(true, cx);
    }

    fn toggle_whole_word(&mut self, cx: &mut Context<Self>) {
        self.search.whole_word = !self.search.whole_word;
        self.search.valid = false;
        self.search_step(true, cx);
    }

    fn toggle_regex(&mut self, cx: &mut Context<Self>) {
        self.search.regex = !self.search.regex;
        self.search.valid = false;
        self.search_step(true, cx);
    }

    fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.search.advanced = !self.search.advanced;
        self.search.valid = false;
        self.search_step(true, cx);
    }

    // ---------------------------------------------------------------------
    // Saving
    // ---------------------------------------------------------------------

    fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let Some(path) = self.tabs[index].path.clone() else {
            self.prompt_save_as(window, cx);
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式不能保存".to_string();
            cx.notify();
            return;
        }
        if !self.tabs[index].dirty {
            self.status = "没有需要保存的修改".to_string();
            cx.notify();
            return;
        }
        self.save_tab_to(index, path, false, window, cx);
    }

    fn save_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut targets = Vec::new();
        let mut untitled = 0usize;
        for (index, tab) in self.tabs.iter().enumerate() {
            if !tab.dirty {
                continue;
            }
            if tab.log.is_some() {
                continue;
            }
            let Some(path) = tab.path.clone() else {
                untitled += 1;
                continue;
            };
            if !tab.save_in_flight {
                targets.push((index, path));
            }
        }
        if targets.is_empty() {
            self.status = if untitled > 0 {
                format!("{untitled} 个未命名文档需要用“另存为”保存")
            } else {
                "没有需要保存的修改".to_string()
            };
            cx.notify();
            return;
        }
        let total = targets.len();
        for (index, path) in targets {
            self.save_tab_to(index, path, false, window, cx);
        }
        self.status = if untitled > 0 {
            format!("正在保存 {total} 个文档；{untitled} 个未命名文档需另存为")
        } else {
            format!("正在保存 {total} 个文档…")
        };
        cx.notify();
    }

    fn reload_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        self.reload_tab(index, window, cx);
    }

    fn reload_tab(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(path) = tab.path.clone() else {
            self.status = "未命名文档没有磁盘来源可重新加载".to_string();
            cx.notify();
            return;
        };
        if tab.log.is_some() {
            self.log_tail(index, false, "已重新读取文件尾部".to_string(), cx);
            return;
        }
        if tab.save_in_flight || tab.reload_pending {
            return;
        }
        let id = tab.id;
        if tab.dirty {
            let now = Instant::now();
            let confirmed = self
                .pending_reload
                .map(|(pending_id, at)| {
                    pending_id == id && now.duration_since(at) < Duration::from_secs(4)
                })
                .unwrap_or(false);
            if !confirmed {
                self.pending_reload = Some((id, now));
                self.status =
                    "文档有未保存修改：再次按 Ctrl+R 将丢弃修改并从磁盘重新加载。".to_string();
                cx.notify();
                return;
            }
        }
        self.pending_reload = None;
        self.tabs[index].reload_pending = true;
        self.status = format!("正在重新加载 {}…", path.display());
        cx.notify();
        let options = OpenOptions {
            allow_guess: true,
            max_bytes: self.settings.files.max_editable_bytes,
            ..Default::default()
        };
        let task = cx
            .background_executor()
            .spawn(async move { (path.clone(), open_for_editing(&path, &options)) });
        cx.spawn(async move |this, cx| {
            let (path, result) = task.await;
            this.update_in(cx, |this, window, cx| {
                this.on_reload_result(id, path, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_reload_result(
        &mut self,
        document_id: DocumentId,
        path: PathBuf,
        result: Result<OpenedFile, OpenError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(document_id) else {
            return;
        };
        self.tabs[index].reload_pending = false;
        match result {
            Ok(opened) => {
                let status = format!("已从磁盘重新加载 {}", path.display());
                self.apply_loaded_document(index, opened, status, window, cx);
            }
            Err(error) => {
                self.status = format!("重新加载失败: {}", open_error_label(&error));
            }
        }
        cx.notify();
    }

    fn apply_loaded_document(
        &mut self,
        index: usize,
        opened: OpenedFile,
        status: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let document_id = self.tabs[index].id;
        let language = syntax::language_for_path(self.tabs[index].path.as_deref());
        let palette = theme::palette(cx);
        let summary = summarize(&opened.text);
        let preferred = summary.preferred();
        let small_text = (opened.text.len() <= 2 * 1024 * 1024).then(|| opened.text.clone());
        let editor = self.tabs[index].editor.clone();
        editor.update(cx, |state, cx| {
            state.set_value(opened.text, window, cx);
            syntax::configure(state, language, palette, cx);
            state.set_preferred_line_ending(
                match preferred {
                    EolKind::CrLf => gpui_kit::base::input::PreferredLineEnding::CrLf,
                    EolKind::Lf => gpui_kit::base::input::PreferredLineEnding::Lf,
                    EolKind::Cr => gpui_kit::base::input::PreferredLineEnding::Cr,
                },
                cx,
            );
        });
        let tab = &mut self.tabs[index];
        tab.reload_pending = false;
        tab.revision = tab.revision.next();
        tab.last_saved_revision = tab.revision;
        tab.dirty = false;
        tab.saved_text = small_text;
        tab.encoding = opened.encoding;
        tab.detection = opened.detection;
        tab.stamp = Some(opened.stamp_after);
        tab.raw_bytes = opened.raw_bytes;
        tab.warnings = opened.warnings;
        tab.eol_summary = summary;
        tab.preferred_eol = preferred;
        tab.external_change = None;
        if self.search.doc_id == Some(document_id) {
            self.search.valid = false;
            self.search.ranges.clear();
            self.search.active = None;
            self.search.status.clear();
            self.tabs[index].search_decorations.set(Vec::new(), cx);
        }
        self.pending_close = None;
        self.status = status;
        self.focus_editor(window, cx);
        self.update_window_title(window);
        self.persist_session(cx);
        cx.notify();
    }

    fn save_tab_to(
        &mut self,
        index: usize,
        target: PathBuf,
        is_save_as: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.log.is_some() {
            self.status = "只读日志模式不能保存".to_string();
            cx.notify();
            return;
        }
        if tab.save_in_flight {
            self.status = "已有保存操作进行中".to_string();
            cx.notify();
            return;
        }
        if is_save_as
            && self
                .find_tab_by_path(&target)
                .is_some_and(|existing| existing != index)
        {
            self.status = format!("{} 已在另一个标签中打开，不能覆盖", target.display());
            cx.notify();
            return;
        }
        let document_id = tab.id;
        let revision = tab.revision;
        let text = tab.editor.read(cx).value().to_string();
        let dirty = tab.dirty;
        let raw_bytes = tab.raw_bytes.clone();
        let encoding = tab.encoding.clone();
        let target_is_current = tab
            .path
            .as_ref()
            .is_some_and(|existing| paths_equal(existing, &target));
        // Saving onto the document's own path must still detect external
        // modification; only a save-as to a different path skips the check.
        let expected = if !is_save_as || target_is_current {
            tab.stamp.clone()
        } else {
            None
        };
        let backup = self.settings.backup.on_manual_save.then(|| {
            let stem = target
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".to_string());
            BackupPlan {
                directory: self.profile.backups_dir(),
                file_stem: stem,
            }
        });

        self.tabs[index].save_in_flight = true;
        self.status = format!("正在保存 {}…", target.display());
        cx.notify();

        let task = cx.background_executor().spawn(async move {
            let content = if !dirty && !raw_bytes.is_empty() {
                // Unmodified save-as must reproduce the original bytes, not a
                // re-encoding that happens to look equivalent.
                SaveContent::Bytes(&raw_bytes)
            } else {
                SaveContent::Text(&text)
            };
            let request = SaveRequest {
                document_id,
                revision,
                target: &target,
                encoding: &encoding,
                expected: expected.as_ref(),
                content,
                backup,
            };
            let result = save_file(&request);
            (target, text, result)
        });

        cx.spawn(async move |this, cx| {
            let (target, saved_text, result) = task.await;
            this.update_in(cx, |this, window, cx| {
                this.on_save_result(
                    document_id,
                    revision,
                    target,
                    saved_text,
                    result,
                    window,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn on_save_result(
        &mut self,
        document_id: DocumentId,
        saved_revision: DocRevision,
        target: PathBuf,
        saved_text: String,
        result: Result<SaveResult, SaveError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(document_id) else {
            return;
        };
        self.tabs[index].save_in_flight = false;
        match result {
            Ok(saved) => {
                let tab = &mut self.tabs[index];
                tab.encoding = saved.outcome.serialization.clone();
                tab.stamp = Some(saved.outcome.stamp.clone());
                tab.raw_bytes = saved.bytes;
                tab.path = Some(target.clone());
                tab.title = target
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.display().to_string());
                tab.language = detect_language(tab.path.as_deref());
                let language = syntax::language_for_path(tab.path.as_deref());
                let medium_document = saved.bytes_written <= 2 * 1024 * 1024;
                tab.eol_summary = summarize(&saved_text);
                tab.preferred_eol = tab.eol_summary.preferred();
                if tab.revision == saved_revision {
                    tab.dirty = false;
                    tab.last_saved_revision = saved_revision;
                    tab.saved_text = if saved_text.len() <= 2 * 1024 * 1024 {
                        Some(saved_text.clone())
                    } else {
                        None
                    };
                }
                let bytes_written = saved.bytes_written;
                let backup = saved.outcome.backup_path.clone();
                self.status = format!("已保存 {}（{bytes_written} 字节）", target.display());
                if let Some(backup) = backup {
                    self.status
                        .push_str(&format!("，备份: {}", backup.display_lossy()));
                }
                if self.tabs[index].revision != saved_revision {
                    self.status.push_str("；保存期间的新修改仍未保存");
                }
                if medium_document {
                    let editor = self.tabs[index].editor.clone();
                    editor.update(cx, |state, cx| state.set_highlighter(language, cx));
                }
                if self.tabs[index].from_recovery
                    && !self.tabs[index].dirty
                    && let Some(sender) = &self.storage_sender
                {
                    sender.try_remove_recovery(document_id);
                }
                if let Some(sender) = &self.storage_sender {
                    sender.try_recent(NativePath::from_path(&target), target.display().to_string());
                }
                self.remember_recent(&target);
                self.update_window_title(window);
                self.persist_session(cx);
            }
            Err(error) => {
                self.status = format!("保存失败: {}", save_error_label(&error));
            }
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Read-only log view
    // ---------------------------------------------------------------------

    fn log_search_step(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let Some(log) = self.tabs[index].log.as_ref() else {
            return;
        };
        if query.is_empty() {
            if let Some(cancel) = self.log_search.cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.log_search.hits.clear();
            self.log_search.pending = false;
            self.search.pending = false;
            self.search.valid = false;
            self.search.status.clear();
            if let Some(log) = self.tabs[index].log.as_mut() {
                log.highlight_line = None;
            }
            cx.notify();
            return;
        }
        if self.search.advanced && self.search.regex {
            self.search.status = "日志搜索使用行级常规正则（PCRE2 仅用于编辑文档）".to_string();
        }
        if let Some(previous) = self.log_search.cancel.take() {
            previous.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.log_search.cancel = Some(cancel.clone());
        self.log_search.pending = true;
        self.log_search.hits.clear();
        self.log_search.active = 0;
        self.log_search.doc_id = Some(self.tabs[index].id);
        self.log_search.query = query.clone();
        self.search.pending = true;
        if self.search.status.is_empty() {
            self.search.status = "日志搜索中…".to_string();
        }
        cx.notify();

        let reader = log.reader.clone();
        let options = LogSearchOptions {
            case_sensitive: self.search.case_sensitive,
            whole_word: self.search.whole_word,
            regex: self.search.regex,
            max_results: 5_000,
        };
        let task_cancel = cancel.clone();
        let task = cx.background_executor().spawn(async move {
            let Ok(mut guard) = reader.lock() else {
                return Err(LogError::Io(std::io::Error::other(
                    "log reader lock poisoned",
                )));
            };
            guard.search(&query, &options, &task_cancel, |_, _| {})
        });
        let expected_cancel = cancel;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let current = this
                    .log_search
                    .cancel
                    .as_ref()
                    .map(|current| Arc::ptr_eq(current, &expected_cancel))
                    .unwrap_or(false);
                if !current {
                    return;
                }
                this.log_search.pending = false;
                this.search.pending = false;
                match result {
                    Ok(result) => {
                        this.log_search.hits = result.hits;
                        this.log_search.active = 0;
                        if this.log_search.hits.is_empty() {
                            this.search.valid = false;
                            this.search.status = "日志中 0 项".to_string();
                            if let Some(log) = this.tabs[index].log.as_mut() {
                                log.highlight_line = None;
                                log.status = "日志搜索: 0 项".to_string();
                            }
                        } else {
                            this.search.valid = true;
                            let suffix = if result.truncated {
                                "（已截断）"
                            } else if result.cancelled {
                                "（已取消）"
                            } else {
                                ""
                            };
                            this.search.status = format!(
                                "日志命中 {}/{}{suffix}",
                                this.log_search.active + 1,
                                this.log_search.hits.len()
                            );
                            this.log_jump_to_active_hit(index, window, cx);
                        }
                    }
                    Err(error) => {
                        this.search.valid = false;
                        this.search.status = format!("日志搜索失败: {error}");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn log_advance(&mut self, direction: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_none() || self.log_search.hits.is_empty() {
            self.log_search_step(self.search_input_text(cx), cx);
            return;
        }
        let total = self.log_search.hits.len();
        let current = self.log_search.active;
        self.log_search.active = if direction > 0 {
            (current + 1) % total
        } else if current == 0 {
            total - 1
        } else {
            current - 1
        };
        self.search.status = format!("日志命中 {}/{}", self.log_search.active + 1, total);
        self.log_jump_to_active_hit(index, window, cx);
    }

    fn log_jump_to_active_hit(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hit) = self.log_search.hits.get(self.log_search.active).cloned() else {
            return;
        };
        if let Some(log) = self.tabs[index].log.as_mut() {
            log.status = format!("命中第 {} 行: {}", hit.line, hit.preview);
            log.highlight_line = Some(hit.line);
        }
        self.log_show(index, hit.line.saturating_sub(1), cx);
        if let Some(log) = self.tabs[index].log.as_mut() {
            log.highlight_line = Some(hit.line);
        }
        cx.notify();
    }

    fn log_show(&mut self, index: usize, target: u64, cx: &mut Context<Self>) {
        let Some(log) = self.tabs.get(index).and_then(|tab| tab.log.as_ref()) else {
            return;
        };
        let reader = log.reader.clone();
        let cancel = log.cancel.clone();
        let Ok(mut guard) = reader.lock() else {
            return;
        };
        let total = guard.total_lines();
        let target = total
            .map(|total| target.min(total.saturating_sub(1)))
            .unwrap_or(target);
        let mut chunks = 0u32;
        while !guard.progress().complete && guard.index().lines_found() <= target && chunks < 64 {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if guard.scan_step().is_err() {
                break;
            }
            chunks += 1;
        }
        match guard.try_viewport(target, LOG_PAGE_LINES, LOG_LINE_BYTES) {
            Ok(view) => {
                let progress = guard.progress();
                drop(guard);
                if let Some(log) = self.tabs[index].log.as_mut() {
                    log.lines = view.lines;
                    log.start_line = view.start_line;
                    log.total_lines = view.total_lines;
                    log.progress = progress;
                    log.tail_mode = false;
                    log.highlight_line = None;
                    log.status = format!("定位到第 {} 行", target + 1);
                }
            }
            Err(LogError::NeedsIndex { scanned_lines, .. }) => {
                let progress = guard.progress();
                drop(guard);
                if let Some(log) = self.tabs[index].log.as_mut() {
                    log.progress = progress;
                    log.status = format!("正在建立行索引：已扫描 {scanned_lines} 行，请稍后再试");
                }
            }
            Err(LogError::LineOutOfRange { lines, .. }) => {
                drop(guard);
                if let Some(log) = self.tabs[index].log.as_mut() {
                    log.status = format!("第 {} 行不存在（共 {lines} 行）", target + 1);
                }
            }
            Err(error) => {
                drop(guard);
                if let Some(log) = self.tabs[index].log.as_mut() {
                    log.status = format!("读取日志失败: {error}");
                }
            }
        }
        cx.notify();
    }

    fn log_tail(&mut self, index: usize, follow: bool, status: String, cx: &mut Context<Self>) {
        let Some(log) = self.tabs.get(index).and_then(|tab| tab.log.as_ref()) else {
            return;
        };
        let reader = log.reader.clone();
        let Ok(guard) = reader.lock() else {
            return;
        };
        match guard.tail(LOG_PAGE_LINES, LOG_LINE_BYTES) {
            Ok(view) => {
                let progress = guard.progress();
                drop(guard);
                self.apply_log_tail(index, view, progress, follow, status, cx);
            }
            Err(error) => {
                drop(guard);
                self.status = format!("日志刷新失败: {error}");
                cx.notify();
            }
        }
    }

    fn apply_log_tail(
        &mut self,
        index: usize,
        view: notepust_large_file::TailView,
        progress: IndexProgress,
        following: bool,
        status: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(log) = self.tabs[index].log.as_mut() {
            log.start_line = view
                .lines
                .first()
                .and_then(|line| line.line)
                .unwrap_or(log.start_line);
            log.lines = view.lines;
            log.total_lines = view.total_lines;
            log.progress = progress;
            log.tail_mode = true;
            log.highlight_line = None;
            log.following = following;
            log.status = status;
        }
        cx.notify();
    }

    fn refresh_logs(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for index in 0..self.tabs.len() {
            let Some(log) = self.tabs[index].log.as_ref() else {
                continue;
            };
            let reader = log.reader.clone();
            let following = log.following;
            let Ok(mut guard) = reader.try_lock() else {
                continue;
            };
            let event = match guard.poll() {
                Ok(event) => event,
                Err(error) => {
                    drop(guard);
                    if let Some(log) = self.tabs[index].log.as_mut() {
                        log.status = format!("检查文件变化失败: {error}");
                    }
                    changed = true;
                    continue;
                }
            };
            match event {
                LogEvent::Appended { .. } if following => {
                    if let Ok(view) = guard.tail(LOG_PAGE_LINES, LOG_LINE_BYTES) {
                        let progress = guard.progress();
                        drop(guard);
                        self.apply_log_tail(
                            index,
                            view,
                            progress,
                            true,
                            "已跟随新增内容".to_string(),
                            cx,
                        );
                    } else {
                        drop(guard);
                        changed = true;
                    }
                    continue;
                }
                LogEvent::Appended { .. } => {
                    drop(guard);
                    if let Some(log) = self.tabs[index].log.as_mut() {
                        log.status = "文件有新增内容（跟随已暂停）".to_string();
                    }
                    changed = true;
                }
                LogEvent::Truncated { .. } | LogEvent::Replaced | LogEvent::RewrittenSameSize => {
                    let refreshed = guard.tail(LOG_PAGE_LINES, LOG_LINE_BYTES).ok();
                    let progress = guard.progress();
                    drop(guard);
                    if let Some(view) = refreshed {
                        self.apply_log_tail(
                            index,
                            view,
                            progress,
                            following,
                            "文件已截断或被替换，视图已刷到尾部".to_string(),
                            cx,
                        );
                    } else {
                        changed = true;
                    }
                }
                LogEvent::Removed => {
                    drop(guard);
                    if let Some(log) = self.tabs[index].log.as_mut() {
                        log.status = "文件已从磁盘删除（只读视图保留）".to_string();
                    }
                    changed = true;
                }
                LogEvent::Unchanged => {}
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn log_top_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active {
            self.log_show(index, 0, cx);
        }
    }

    fn log_prev_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active
            && let Some(log) = self.tabs[index].log.as_ref()
        {
            let target = log.start_line.saturating_sub(LOG_PAGE_LINES as u64);
            self.log_show(index, target, cx);
        }
    }

    fn log_next_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active
            && let Some(log) = self.tabs[index].log.as_ref()
        {
            let target = log.start_line + LOG_PAGE_LINES as u64;
            self.log_show(index, target, cx);
        }
    }

    fn log_follow_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let Some(log) = self.tabs[index].log.as_mut() else {
            return;
        };
        let following = !log.following;
        log.following = following;
        if following {
            self.log_tail(index, true, "跟随已开启".to_string(), cx);
        } else {
            if let Some(log) = self.tabs[index].log.as_mut() {
                log.status = "跟随已暂停".to_string();
            }
            cx.notify();
        }
    }

    fn log_reload_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active {
            self.log_tail(index, false, "已重新读取文件尾部".to_string(), cx);
        }
    }

    fn render_editor_panes(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(active) = self.active else {
            return self.render_welcome(palette, cx);
        };
        let mut row = div().flex().flex_row().size_full();
        row = row.child(self.render_pane(active, true, false, palette, cx));
        if let Some(split) = self
            .split
            .filter(|split| *split < self.tabs.len() && *split != active)
        {
            row = row.child(self.render_pane(split, false, true, palette, cx));
        }
        row.into_any_element()
    }

    fn render_pane(
        &self,
        index: usize,
        focused: bool,
        half_width: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = if self.tabs[index].log.is_some() {
            self.render_log_view(index, palette, cx)
        } else {
            div()
                .size_full()
                .font(theme::editor_font(&self.settings))
                .text_size(px(self.settings.editor.font_size))
                .child(Editor::new(&self.tabs[index].editor))
                .into_any_element()
        };
        let mut pane = div()
            .id(format!("editor-pane-{index}"))
            .flex()
            .flex_col()
            .min_w(px(0.))
            .min_h(px(0.));
        pane = if half_width {
            pane.w(relative(0.5)).overflow_hidden()
        } else {
            pane.flex_1()
        };
        if !focused {
            pane = pane
                .border_l_1()
                .border_color(palette.border)
                .bg(palette.bg);
        }
        pane.child(content)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if this.active != Some(index) {
                        this.focus_pane(index, window, cx);
                    }
                }),
            )
            .into_any_element()
    }

    fn render_log_view(
        &self,
        index: usize,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lang = self.settings.ui.language;
        let Some(log) = self.tabs[index].log.as_ref() else {
            return div().into_any_element();
        };
        let total = log
            .total_lines
            .map(|total| total.to_string())
            .unwrap_or_else(|| format!("≥{}", log.progress.lines_found));
        let range = if log.lines.is_empty() {
            "0 行".to_string()
        } else if log.tail_mode && log.lines.first().and_then(|line| line.line).is_none() {
            format!(
                "{} {} 行 / {total}",
                tr(lang, "尾部", "tail"),
                log.lines.len()
            )
        } else {
            format!(
                "行 {}-{} / {total}",
                log.start_line + 1,
                log.start_line + log.lines.len() as u64
            )
        };
        let index_state = if log.progress.complete {
            tr(lang, "索引完成", "Indexed").to_string()
        } else {
            let percent = log
                .progress
                .scanned_bytes
                .saturating_mul(100)
                .checked_div(log.progress.observed_len)
                .unwrap_or(0)
                .min(100);
            format!("{} {percent}%", tr(lang, "建立索引", "Indexing"))
        };
        let follow_label = if log.following {
            tr(lang, "跟随: 开", "Follow: on")
        } else {
            tr(lang, "跟随: 暂停", "Follow: paused")
        };

        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(32.))
            .px_3()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.warning)
                    .child(tr(lang, "只读日志", "Read-only log")),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.muted)
                    .child(format!(
                        "{} · {range} · {index_state}",
                        self.tabs[index].encoding.status_label()
                    )),
            )
            .child(div().flex_1())
            .child(self.tool_button(
                "log-top",
                tr(lang, "顶部", "Top"),
                palette,
                cx,
                Self::log_top_clicked,
            ))
            .child(self.tool_button(
                "log-prev",
                tr(lang, "上一屏", "Page up"),
                palette,
                cx,
                Self::log_prev_clicked,
            ))
            .child(self.tool_button(
                "log-next",
                tr(lang, "下一屏", "Page down"),
                palette,
                cx,
                Self::log_next_clicked,
            ))
            .child(self.toggle_button(
                "log-follow",
                follow_label,
                log.following,
                palette,
                cx,
                Self::log_follow_clicked,
            ))
            .child(self.tool_button(
                "log-reload",
                tr(lang, "刷新", "Refresh"),
                palette,
                cx,
                Self::log_reload_clicked,
            ));

        let mut list = div()
            .id("log-lines")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .bg(palette.bg)
            .font_family("Consolas")
            .text_size(px(12.));
        for (offset, line) in log.lines.iter().enumerate() {
            let number = match line.line {
                Some(line) => (line + 1).to_string(),
                None if log.tail_mode => "—".to_string(),
                None => (log.start_line + offset as u64 + 1).to_string(),
            };
            let highlighted = log.highlight_line.is_some() && log.highlight_line == line.line;
            let text = if line.truncated {
                format!("{}  …〔该行过长，已截断〕", line.text)
            } else {
                line.text.clone()
            };
            list = list.child(
                div()
                    .flex()
                    .items_start()
                    .when(highlighted, |this| {
                        let mut background = palette.active_match;
                        background.a = 0.28;
                        this.bg(background)
                    })
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .px_2()
                            .text_color(palette.muted)
                            .child(number.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_color(palette.fg)
                            .child(text),
                    ),
            );
        }
        if log.lines.is_empty() {
            list = list.child(div().px_3().py_2().text_color(palette.muted).child(tr(
                lang,
                "（文件为空）",
                "(empty file)",
            )));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px_3()
                    .bg(palette.panel)
                    .text_size(px(11.))
                    .text_color(palette.muted)
                    .child(log.status.clone()),
            )
            .child(list)
            .into_any_element()
    }

    fn prompt_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式不能另存为".to_string();
            cx.notify();
            return;
        }
        let directory = self.tabs[index]
            .path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.profile.config_dir.clone());
        let suggested = self.tabs[index]
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.txt", self.tabs[index].title));
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(target))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    if let Some(index) = this.active {
                        this.save_tab_to(index, target, true, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // External changes, recovery, session
    // ---------------------------------------------------------------------

    fn check_external_change(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (Some(path), Some(stamp)) = (tab.path.clone(), tab.stamp.clone()) else {
            return;
        };
        if tab.save_in_flight {
            return;
        }
        let change = detect_external_change(&path, &stamp);
        let message = match change {
            Ok(ExternalChange::Unchanged) => None,
            Ok(ExternalChange::Modified) => {
                Some("文件在磁盘上已被修改；保存会先检查版本并询问冲突。".to_string())
            }
            Ok(ExternalChange::Replaced) => {
                Some("路径现在指向另一个文件；请重新打开。".to_string())
            }
            Ok(ExternalChange::Removed) => Some("文件已从磁盘删除。".to_string()),
            Err(error) => Some(format!("无法检查外部修改: {error}")),
        };
        if self.tabs[index].external_change != message {
            if let Some(message) = &message {
                self.status = message.clone();
            }
            self.tabs[index].external_change = message;
            cx.notify();
        }
    }

    fn checkpoint_dirty(&mut self, cx: &mut Context<Self>) {
        let Some(sender) = self.storage_sender.clone() else {
            return;
        };
        for tab in &self.tabs {
            if !tab.dirty {
                continue;
            }
            let text = tab.editor.read(cx).value().to_string();
            if text.len() > 32 * 1024 * 1024 {
                // Very large buffers are protected by the save path and the
                // final close checkpoint; avoid serializing 100 MB every few
                // seconds while the user is working.
                continue;
            }
            let document = RecoveredDocument {
                document_id: tab.id,
                path: tab.path.as_ref().map(|path| NativePath::from_path(path)),
                display_path: Some(tab.title.clone()),
                lifetime_generation: 1,
                revision: tab.revision,
                encoding: tab.encoding.clone(),
                digest: *blake3::hash(text.as_bytes()).as_bytes(),
                text,
                updated_at: notepust_storage::recovery::unix_now(),
            };
            sender.try_checkpoint(document);
        }
    }

    fn persist_session(&self, cx: &mut Context<Self>) {
        let Some(sender) = &self.storage_sender else {
            return;
        };
        let active = self.active;
        let documents = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| SessionDocument {
                document_id: tab.id,
                path: tab.path.as_ref().map(|path| NativePath::from_path(path)),
                display_path: tab.title.clone(),
                revision: tab.revision,
                caret_utf8: tab.editor.read(cx).selected_range().start as u64,
                scroll_top_line: 0,
                active: Some(index) == active,
            })
            .collect();
        sender.try_session(documents);
    }

    fn start_background_loop(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let alive = this
                    .update(cx, |this, cx| {
                        this.check_all_external_changes(cx);
                        this.refresh_clean_state(cx);
                        this.refresh_logs(cx);
                        if this.settings.recovery.enabled
                            && this.last_checkpoint.elapsed()
                                >= Duration::from_secs(
                                    this.settings.recovery.checkpoint_interval_s.max(1),
                                )
                        {
                            this.checkpoint_dirty(cx);
                            this.last_checkpoint = Instant::now();
                        }
                        if this.last_checkpoint.elapsed() >= Duration::from_secs(5) {
                            this.persist_session(cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn check_all_external_changes(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.tabs.len() {
            self.check_external_change(index, cx);
        }
    }

    fn start_forward_loop(&mut self, cx: &mut Context<Self>) {
        let Some(receiver) = self.forwarded.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            loop {
                let mut requests = Vec::new();
                while let Ok(request) = receiver.try_recv() {
                    requests.push(request);
                }
                if !requests.is_empty() {
                    let alive = this
                        .update_in(cx, |this, window, cx| {
                            window.activate_window();
                            for request in requests {
                                this.handle_forwarded(request, window, cx);
                            }
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            }
        })
        .detach();
    }

    fn handle_forwarded(
        &mut self,
        request: ForwardedOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request.paths.is_empty() {
            self.status = "另一个 Notepust 实例已激活此窗口".to_string();
            cx.notify();
            return;
        }
        if let Some(line) = request.line {
            self.initial_goto = Some((line, request.column.unwrap_or(1)));
        }
        for path in request.paths {
            self.start_open(PathBuf::from(path), window, cx);
        }
    }

    pub fn write_final_checkpoint(&mut self, cx: &mut App) -> bool {
        let Ok(mut db) = RecoveryDb::open(&self.profile.recovery_db()) else {
            return true;
        };
        for tab in &self.tabs {
            if tab.dirty {
                let text = tab.editor.read(cx).value().to_string();
                let document = RecoveredDocument {
                    document_id: tab.id,
                    path: tab.path.as_ref().map(|path| NativePath::from_path(path)),
                    display_path: Some(tab.title.clone()),
                    lifetime_generation: 1,
                    revision: tab.revision,
                    encoding: tab.encoding.clone(),
                    digest: *blake3::hash(text.as_bytes()).as_bytes(),
                    text,
                    updated_at: notepust_storage::recovery::unix_now(),
                };
                if let Err(error) = db.checkpoint(&document) {
                    tracing::error!(%error, "final recovery checkpoint failed");
                }
            } else {
                let _ = db.remove_recovery(tab.id);
            }
        }
        let documents = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| SessionDocument {
                document_id: tab.id,
                path: tab.path.as_ref().map(|path| NativePath::from_path(path)),
                display_path: tab.title.clone(),
                revision: tab.revision,
                caret_utf8: tab.editor.read(&*cx).selected_range().start as u64,
                scroll_top_line: 0,
                active: Some(index) == self.active,
            })
            .collect::<Vec<_>>();
        if let Err(error) = db.save_session(&documents) {
            tracing::error!(%error, "session save failed");
        }
        true
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.settings.ui.theme = match theme::effective_mode(self.settings.ui.theme) {
            ThemeMode::Light => ThemeMode::Dark,
            _ => ThemeMode::Light,
        };
        theme::install(cx, self.settings.ui.theme);
        theme::install_fonts(cx, &self.settings);
        for tab in &self.tabs {
            let editor = tab.editor.clone();
            editor.update(cx, |state, cx| {
                syntax::refresh_style(state, theme::palette(cx));
            });
        }
        let _ = self.settings.save_atomic(&self.profile.settings_file());
        self.status = format!("主题: {:?}", self.settings.ui.theme);
        cx.notify();
    }

    fn toggle_language(&mut self, cx: &mut Context<Self>) {
        self.settings.ui.language = self.settings.ui.language.toggle();
        let _ = self.settings.save_atomic(&self.profile.settings_file());
        cx.notify();
    }

    fn toggle_whitespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show = !self.settings.editor.show_whitespace;
        self.settings.editor.show_whitespace = show;
        for tab in &self.tabs {
            if tab.log.is_some() {
                continue;
            }
            let editor = tab.editor.clone();
            editor.update(cx, |state, cx| state.set_show_whitespaces(show, window, cx));
        }
        let _ = self.settings.save_atomic(&self.profile.settings_file());
        self.status = if show {
            "已显示空白字符".to_string()
        } else {
            "已隐藏空白字符".to_string()
        };
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Action handlers
    // ---------------------------------------------------------------------

    fn on_new_document(&mut self, _: &NewDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.new_untitled(window, cx);
    }

    fn on_open_document(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("打开文件".into()),
        });
        let _ = window;
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                this.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.start_open(path, window, cx);
                    }
                })
                .ok();
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                this.update(cx, |this, cx| {
                    this.status = format!("打开对话框失败: {error}");
                    cx.notify();
                })
                .ok();
            }
            Err(_) => {}
        })
        .detach();
    }

    fn on_save_document(&mut self, _: &SaveDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.save_active(window, cx);
    }

    fn on_save_document_as(
        &mut self,
        _: &SaveDocumentAs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_save_as(window, cx);
    }

    fn on_save_all(&mut self, _: &SaveAllDocuments, window: &mut Window, cx: &mut Context<Self>) {
        self.save_all(window, cx);
    }

    fn on_reload_document(
        &mut self,
        _: &ReloadDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload_active(window, cx);
    }

    fn on_close_document(
        &mut self,
        _: &CloseDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active(window, cx);
    }

    fn on_toggle_find(&mut self, _: &ToggleFind, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }

    fn on_toggle_replace(
        &mut self,
        _: &ToggleReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(true, window, cx);
    }

    fn on_goto_line(&mut self, _: &GotoLine, window: &mut Window, cx: &mut Context<Self>) {
        if self.goto_open {
            self.close_goto(window, cx);
        } else {
            self.open_goto(window, cx);
        }
    }

    fn on_quick_open(&mut self, _: &QuickOpen, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick.open {
            self.close_quick(window, cx);
        } else {
            self.open_quick(window, cx);
        }
    }

    fn on_toggle_whitespace(
        &mut self,
        _: &ToggleWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_whitespace(window, cx);
    }

    fn on_toggle_encoding_panel(
        &mut self,
        _: &ToggleEncodingPanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_encoding_panel(cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.settings_open = false;
            self.focus_editor(window, cx);
            cx.notify();
        } else {
            self.open_settings(window, cx);
        }
    }

    fn on_toggle_split(&mut self, _: &ToggleSplit, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_split(window, cx);
    }

    fn apply_line_operation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(&str, Range<usize>) -> Option<line_ops::LineEdit>,
    ) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式不能修改行".to_string();
            cx.notify();
            return;
        }
        let editor = self.tabs[index].editor.clone();
        let (text, selection) = {
            let state = editor.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let Some(edit) = operation(&text, selection) else {
            self.status = "当前行没有可应用的变化".to_string();
            cx.notify();
            return;
        };
        editor.update(cx, |state, cx| {
            let scroll = state.scroll_offset();
            state.replace_all(edit.text, window, cx);
            state.set_selected_range(edit.selection, cx);
            state.set_scroll_offset(scroll, cx);
        });
        self.refresh_position(index, cx);
        cx.notify();
    }

    fn on_duplicate_line(
        &mut self,
        _: &DuplicateLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_line_operation(window, cx, line_ops::duplicate_lines);
    }

    fn on_delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_line_operation(window, cx, line_ops::delete_lines);
    }

    fn on_move_line_up(&mut self, _: &MoveLineUp, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_line_operation(window, cx, |text, selection| {
            line_ops::move_lines(text, selection, -1)
        });
    }

    fn on_move_line_down(&mut self, _: &MoveLineDown, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_line_operation(window, cx, |text, selection| {
            line_ops::move_lines(text, selection, 1)
        });
    }

    fn on_sort_lines(&mut self, _: &SortLines, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_line_operation(window, cx, line_ops::sort_lines);
    }

    fn on_trim_lines(
        &mut self,
        _: &TrimTrailingWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_line_operation(window, cx, line_ops::trim_trailing_whitespace);
    }

    fn on_toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = line_ops::comment_token(
            self.active
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.path.as_deref()),
        );
        self.apply_line_operation(window, cx, move |text, selection| {
            line_ops::toggle_comments(text, selection, token)
        });
    }

    fn on_indent_lines(&mut self, _: &IndentLines, window: &mut Window, cx: &mut Context<Self>) {
        let indent = if self.settings.editor.insert_spaces {
            " ".repeat(self.settings.editor.tab_size.max(1) as usize)
        } else {
            "\t".to_string()
        };
        self.apply_line_operation(window, cx, move |text, selection| {
            line_ops::indent_lines(text, selection, &indent, false)
        });
    }

    fn on_outdent_lines(&mut self, _: &OutdentLines, window: &mut Window, cx: &mut Context<Self>) {
        let indent = if self.settings.editor.insert_spaces {
            " ".repeat(self.settings.editor.tab_size.max(1) as usize)
        } else {
            "\t".to_string()
        };
        self.apply_line_operation(window, cx, move |text, selection| {
            line_ops::indent_lines(text, selection, &indent, true)
        });
    }

    fn on_add_cursor_above(
        &mut self,
        _: &AddCursorAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_cursor_vertical(true, window, cx);
    }

    fn on_add_cursor_below(
        &mut self,
        _: &AddCursorBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_cursor_vertical(false, window, cx);
    }

    fn add_cursor_vertical(&mut self, above: bool, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].log.is_some() {
            self.status = "只读日志模式没有多光标".to_string();
            cx.notify();
            return;
        }
        let editor = self.tabs[index].editor.clone();
        let (text, selection) = {
            let state = editor.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let tab_width = self.settings.editor.tab_size.max(1);
        let target = if above {
            line_ops::cursor_above(&text, selection.end, tab_width)
        } else {
            line_ops::cursor_below(&text, selection.end, tab_width)
        };
        let Some(target) = target else {
            self.status = "没有可添加光标的相邻行".to_string();
            cx.notify();
            return;
        };
        editor.update(cx, |state, cx| {
            state.add_selection(target..target, cx);
        });
        self.status = "已添加多光标；Alt+点击或 Alt+Shift+拖动可扩展列选区".to_string();
        cx.notify();
    }

    fn on_find_next(&mut self, _: &FindNextMatch, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.log.is_some())
        {
            self.log_advance(1, window, cx);
            return;
        }
        if self.search.valid && !self.search.ranges.is_empty() {
            self.advance_match(1, cx);
            self.focus_editor(window, cx);
        } else {
            self.search_step(true, cx);
        }
    }

    fn on_find_previous(
        &mut self,
        _: &FindPreviousMatch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.log.is_some())
        {
            self.log_advance(-1, window, cx);
            return;
        }
        if self.search.valid && !self.search.ranges.is_empty() {
            self.advance_match(-1, cx);
            self.focus_editor(window, cx);
        } else {
            self.search_step(true, cx);
        }
    }

    fn on_replace_all(
        &mut self,
        _: &ReplaceAllMatches,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_all(cx);
    }

    fn on_folder_search(&mut self, _: &FolderSearch, _window: &mut Window, cx: &mut Context<Self>) {
        self.folder_search(cx);
    }

    fn on_next_document(&mut self, _: &NextDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self
            .active
            .map(|index| (index + 1) % self.tabs.len())
            .unwrap_or(0);
        self.activate(next, window, cx);
    }

    fn on_previous_document(
        &mut self,
        _: &PreviousDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.is_empty() {
            return;
        }
        let previous = self
            .active
            .map(|index| {
                if index == 0 {
                    self.tabs.len() - 1
                } else {
                    index - 1
                }
            })
            .unwrap_or(0);
        self.activate(previous, window, cx);
    }

    fn on_toggle_theme(
        &mut self,
        _: &ToggleThemeMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_theme(cx);
    }

    fn on_toggle_language(
        &mut self,
        _: &ToggleUiLanguage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_language(cx);
    }

    fn on_kit_search(
        &mut self,
        _: &gpui_kit::base::input::Search,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(false, window, cx);
    }

    fn on_kit_replace(
        &mut self,
        _: &gpui_kit::base::input::Replace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(true, window, cx);
    }

    // ---------------------------------------------------------------------
    // Rendering
    // ---------------------------------------------------------------------

    fn status_path(&self, tab: &DocumentTab) -> String {
        tab.path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| tab.title.clone())
    }

    fn render_toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let product = div()
            .flex()
            .items_center()
            .gap_2()
            .mr_2()
            .child(
                div()
                    .size(px(22.))
                    .rounded(px(5.))
                    .bg(palette.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(palette.accent_fg)
                    .child("NP"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(palette.fg)
                    .child("Notepust"),
            );

        let mut row = div()
            .flex()
            .items_center()
            .h(px(40.))
            .px_3()
            .gap_1()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(product);

        row = row
            .child(self.tool_button(
                "tb-new",
                tr(lang, "新建", "New"),
                palette,
                cx,
                Self::new_clicked,
            ))
            .child(self.tool_button(
                "tb-open",
                tr(lang, "打开", "Open"),
                palette,
                cx,
                Self::open_clicked,
            ))
            .child(self.tool_button(
                "tb-save",
                tr(lang, "保存", "Save"),
                palette,
                cx,
                Self::save_clicked,
            ))
            .child(self.tool_button(
                "tb-save-as",
                tr(lang, "另存为", "Save As"),
                palette,
                cx,
                Self::save_as_clicked,
            ))
            .child(self.tool_button(
                "tb-save-all",
                tr(lang, "全部保存", "Save All"),
                palette,
                cx,
                Self::save_all_clicked,
            ))
            .child(self.tool_button(
                "tb-reload",
                tr(lang, "重载", "Reload"),
                palette,
                cx,
                Self::reload_clicked,
            ))
            .child(self.toggle_button(
                "tb-split",
                tr(lang, "分屏", "Split"),
                self.split.is_some(),
                palette,
                cx,
                Self::split_clicked,
            ))
            .child(div().w(px(1.)).h(px(18.)).bg(palette.border).mx_1())
            .child(self.tool_button(
                "tb-find",
                tr(lang, "查找", "Find"),
                palette,
                cx,
                Self::find_clicked,
            ))
            .child(self.tool_button(
                "tb-replace",
                tr(lang, "替换", "Replace"),
                palette,
                cx,
                Self::replace_clicked,
            ))
            .child(self.toggle_button(
                "tb-whitespace",
                tr(lang, "空白", "WS"),
                self.settings.editor.show_whitespace,
                palette,
                cx,
                Self::whitespace_clicked,
            ))
            .child(div().flex_1())
            .child(self.tool_button(
                "tb-theme",
                match theme::effective_mode(self.settings.ui.theme) {
                    ThemeMode::Light => tr(lang, "深色", "Dark"),
                    _ => tr(lang, "浅色", "Light"),
                },
                palette,
                cx,
                Self::theme_clicked,
            ))
            .child(self.tool_button(
                "tb-lang",
                match lang {
                    Language::ZhCn => "EN",
                    Language::En => "中文",
                },
                palette,
                cx,
                Self::language_clicked,
            ))
            .child(self.tool_button(
                "tb-settings",
                tr(lang, "设置", "Settings"),
                palette,
                cx,
                Self::settings_clicked,
            ));

        row.into_any_element()
    }

    fn new_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.new_untitled(window, cx);
    }

    fn open_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("打开文件".into()),
        });
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.start_open(path, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn save_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.save_active(window, cx);
    }

    fn save_as_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_save_as(window, cx);
    }

    fn save_all_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.save_all(window, cx);
    }

    fn reload_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_active(window, cx);
    }

    fn split_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_split(window, cx);
    }

    fn find_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }

    fn replace_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(true, window, cx);
    }

    fn whitespace_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_whitespace(window, cx);
    }

    fn theme_clicked(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_theme(cx);
    }

    fn language_clicked(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_language(cx);
    }

    fn settings_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    fn tool_button(
        &self,
        id: &'static str,
        label: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
        handler: fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>),
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(27.))
            .px_2()
            .rounded(px(4.))
            .cursor_pointer()
            .text_size(px(12.5))
            .text_color(palette.fg)
            .hover(move |style| style.bg(palette.hover))
            .on_click(cx.listener(handler))
            .child(label)
            .into_any_element()
    }

    fn render_tabs(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.tabs.is_empty() {
            return div().h(px(0.)).into_any_element();
        }
        let mut row = div()
            .flex()
            .items_center()
            .h(px(34.))
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border);

        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            let active = self.active == Some(index);
            let title = tab.title.clone();
            let dirty = tab.dirty;
            let tab_id = format!("tab-{index}");
            let close_id = format!("tab-close-{index}");

            let title_element = div()
                .id(tab_id)
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .max_w(px(220.))
                .overflow_hidden()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.activate(index, window, cx);
                }))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(if active { palette.fg } else { palette.muted })
                        .child(title.clone()),
                )
                .when(dirty, |this| {
                    this.child(div().size(px(6.)).rounded(px(3.)).bg(palette.warning))
                });

            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .pl_3()
                    .pr_2()
                    .gap_2()
                    .bg(if active {
                        palette.tab_active
                    } else {
                        palette.tab_inactive
                    })
                    .border_r_1()
                    .border_color(palette.border)
                    .child(title_element)
                    .child(
                        div()
                            .id(close_id)
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(palette.muted)
                            .hover(move |style| style.bg(palette.hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.close_tab_at(index, window, cx);
                            }))
                            .child("×"),
                    ),
            );
        }

        row.into_any_element()
    }

    fn render_search_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let query_box = div()
            .h(px(28.))
            .w(px(280.))
            .px_2()
            .rounded(px(4.))
            .bg(palette.input_bg)
            .border_1()
            .border_color(palette.border)
            .child(Input::new(&self.search.query));

        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(query_box);

        if self.search.replace_mode {
            row = row.child(
                div()
                    .h(px(28.))
                    .w(px(240.))
                    .px_2()
                    .rounded(px(4.))
                    .bg(palette.input_bg)
                    .border_1()
                    .border_color(palette.border)
                    .child(Input::new(&self.search.replacement)),
            );
        }

        row = row
            .child(self.toggle_button(
                "search-case",
                "Aa",
                self.search.case_sensitive,
                palette,
                cx,
                Self::case_clicked,
            ))
            .child(self.toggle_button(
                "search-word",
                "W",
                self.search.whole_word,
                palette,
                cx,
                Self::word_clicked,
            ))
            .child(self.toggle_button(
                "search-regex",
                ".*",
                self.search.regex,
                palette,
                cx,
                Self::regex_clicked,
            ))
            .when_some(
                self.search.regex.then(|| {
                    self.toggle_button(
                        "search-advanced",
                        "PCRE2",
                        self.search.advanced,
                        palette,
                        cx,
                        Self::advanced_clicked,
                    )
                }),
                |row, button| row.child(button),
            )
            .child(self.tool_button("search-prev", "◀", palette, cx, Self::prev_clicked))
            .child(self.tool_button("search-next", "▶", palette, cx, Self::next_clicked));

        if self.search.replace_mode {
            row = row.child(self.tool_button(
                "search-replace-all",
                tr(lang, "全部替换", "Replace All"),
                palette,
                cx,
                Self::replace_all_clicked,
            ));
        }

        row = row.child(
            div()
                .text_size(px(11.5))
                .text_color(palette.muted)
                .max_w(px(340.))
                .overflow_hidden()
                .child(self.search.status.clone()),
        );

        row = row.child(div().flex_1()).child(self.tool_button(
            "search-close",
            "×",
            palette,
            cx,
            Self::close_search_clicked,
        ));

        row.into_any_element()
    }

    fn render_goto_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.muted)
                    .child(tr(lang, "跳转到行", "Go to line")),
            )
            .child(
                div()
                    .h(px(28.))
                    .w(px(120.))
                    .px_2()
                    .rounded(px(4.))
                    .bg(palette.input_bg)
                    .border_1()
                    .border_color(palette.border)
                    .child(Input::new(&self.goto_input)),
            )
            .child(self.tool_button(
                "goto-confirm",
                tr(lang, "跳转", "Go"),
                palette,
                cx,
                Self::goto_confirm_clicked,
            ))
            .child(div().flex_1())
            .child(self.tool_button("goto-close", "×", palette, cx, Self::goto_close_clicked))
            .into_any_element()
    }

    fn goto_confirm_clicked(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.commit_goto(cx) {
            self.close_goto(window, cx);
        }
    }

    fn goto_close_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_goto(window, cx);
    }

    fn quick_close_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_quick(window, cx);
    }

    fn encoding_close_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.encoding_ui.open = false;
        cx.notify();
    }

    fn render_encoding_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let mut reopen_row = div().flex().items_center().gap_1().child(
            div()
                .w(px(96.))
                .text_size(px(11.5))
                .text_color(palette.muted)
                .child(tr(lang, "重新解释为", "Reopen as")),
        );
        let mut save_row = div().flex().items_center().gap_1().child(
            div()
                .w(px(96.))
                .text_size(px(11.5))
                .text_color(palette.muted)
                .child(tr(lang, "保存编码", "Save as")),
        );
        for (key, label, spec) in encoding_choices() {
            let reopen_spec = spec.clone();
            reopen_row = reopen_row.child(self.action_button(
                format!("enc-reopen-{key}"),
                label,
                palette,
                cx,
                move |this, _event, _window, cx| {
                    this.reinterpret_encoding(reopen_spec.clone(), cx);
                },
            ));
            let save_spec = spec;
            save_row = save_row.child(self.action_button(
                format!("enc-save-{key}"),
                label,
                palette,
                cx,
                move |this, _event, _window, cx| {
                    this.save_with_encoding(save_spec.clone(), cx);
                },
            ));
        }
        reopen_row = reopen_row.child(div().flex_1()).child(self.tool_button(
            "enc-close",
            "×",
            palette,
            cx,
            Self::encoding_close_clicked,
        ));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(reopen_row)
            .child(save_row)
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(palette.muted)
                    .child(tr(
                        lang,
                        "重新解释要求文档无未保存修改；保存编码在下次保存时生效。",
                        "Reopen requires a clean document; the save encoding applies on the next save.",
                    )),
            )
            .into_any_element()
    }

    fn action_button(
        &self,
        id: String,
        label: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(24.))
            .px_2()
            .rounded(px(3.))
            .cursor_pointer()
            .text_size(px(11.5))
            .bg(palette.input_bg)
            .text_color(palette.fg)
            .hover(move |style| style.bg(palette.hover))
            .on_click(cx.listener(handler))
            .child(label)
            .into_any_element()
    }

    fn render_quick_open(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let root_label = self
            .quick
            .root
            .as_ref()
            .map(|root| root.display().to_string())
            .unwrap_or_default();
        let mut results = div()
            .id("quick-results")
            .flex()
            .flex_col()
            .max_h(px(260.))
            .overflow_y_scroll()
            .bg(palette.panel);
        for (index, relative) in self.quick.matches.iter().take(12).enumerate() {
            let label = relative.display().to_string();
            let active = index == self.quick.selected;
            results = results.child(
                div()
                    .id(format!("quick-hit-{index}"))
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px_3()
                    .cursor_pointer()
                    .bg(if active { palette.hover } else { palette.panel })
                    .hover(move |style| style.bg(palette.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.quick_click(index, window, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if active { palette.fg } else { palette.muted })
                            .child(label),
                    ),
            );
        }
        if self.quick.matches.is_empty() {
            results = results.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.5))
                    .text_color(palette.muted)
                    .child(if self.quick.pending {
                        tr(lang, "正在扫描…", "Scanning…")
                    } else {
                        tr(lang, "没有匹配的文件", "No matching files")
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .bg(palette.panel)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(palette.muted)
                            .child(tr(lang, "快速打开", "Quick open")),
                    )
                    .child(
                        div()
                            .h(px(28.))
                            .w(px(320.))
                            .px_2()
                            .rounded(px(4.))
                            .bg(palette.input_bg)
                            .border_1()
                            .border_color(palette.border)
                            .child(Input::new(&self.quick.input)),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(palette.muted)
                            .child(self.quick.status.clone()),
                    )
                    .child(div().flex_1())
                    .child(self.tool_button(
                        "quick-close",
                        "×",
                        palette,
                        cx,
                        Self::quick_close_clicked,
                    )),
            )
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(10.5))
                    .text_color(palette.muted)
                    .child(format!(
                        "{}: {root_label} · {}",
                        tr(lang, "根目录", "Root"),
                        tr(lang, "Enter 打开第一个结果", "Enter opens the first match")
                    )),
            )
            .child(results)
            .into_any_element()
    }

    fn toggle_button(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        handler: fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>),
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(24.))
            .min_w(px(24.))
            .px_1()
            .rounded(px(3.))
            .cursor_pointer()
            .text_size(px(11.5))
            .bg(if active {
                palette.accent
            } else {
                palette.input_bg
            })
            .text_color(if active {
                palette.accent_fg
            } else {
                palette.fg
            })
            .hover(move |style| style.bg(palette.hover))
            .on_click(cx.listener(handler))
            .child(label)
            .into_any_element()
    }

    fn case_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_case(cx);
    }

    fn word_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_whole_word(cx);
    }

    fn regex_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_regex(cx);
    }

    fn advanced_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_advanced(cx);
    }

    fn next_clicked(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.search_step(true, cx);
    }

    fn prev_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.search.valid && !self.search.ranges.is_empty() {
            self.advance_match(-1, cx);
        } else {
            self.search_step(true, cx);
        }
    }

    fn replace_all_clicked(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.replace_all(cx);
    }

    fn close_search_clicked(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_find(window, cx);
    }

    fn render_welcome(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let recent = if self.recent.is_empty() {
            None
        } else {
            Some(self.render_recent(palette, cx))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_size(px(40.))
                    .text_color(palette.accent)
                    .child("Notepust"),
            )
            .child(div().text_size(px(14.)).text_color(palette.muted).child(tr(
                lang,
                "专注文本、代码与日志的可靠编辑器",
                "A reliable editor for text, code and logs",
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.tool_button(
                        "welcome-new",
                        tr(lang, "新建文档", "New document"),
                        palette,
                        cx,
                        Self::new_clicked,
                    ))
                    .child(self.tool_button(
                        "welcome-open",
                        tr(lang, "打开文件", "Open file"),
                        palette,
                        cx,
                        Self::open_clicked,
                    )),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.muted)
                    .child(tr(
                        lang,
                        "Ctrl+O 打开 · Ctrl+P 快速打开 · Ctrl+F 查找 · Ctrl+H 替换 · Ctrl+G 跳转行",
                        "Ctrl+O open · Ctrl+P quick open · Ctrl+F find · Ctrl+H replace · Ctrl+G go to line",
                    )),
            )
            .when_some(recent, |this, recent| this.child(recent))
            .into_any_element()
    }

    fn render_recent(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let mut list = div().flex().flex_col().gap_1().w(px(560.));
        for (index, file) in self.recent.iter().take(10).enumerate() {
            let path = file.path.to_path_buf();
            let label = file.display_path.clone();
            list = list.child(
                div()
                    .id(format!("recent-{index}"))
                    .flex()
                    .items_center()
                    .h(px(23.))
                    .px_2()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .overflow_hidden()
                    .hover(move |style| style.bg(palette.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.start_open(path.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(palette.muted)
                            .child(label),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .mt_2()
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.muted)
                    .child(tr(lang, "最近文件", "Recent files")),
            )
            .child(list)
            .into_any_element()
    }

    fn render_folder_results(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("folder-results-list")
            .flex()
            .flex_col()
            .h(px(200.))
            .overflow_y_scroll();

        for (index, hit) in self.folder.matches.iter().enumerate() {
            let location = format!("{}:{}", hit.path.display(), hit.line);
            let snippet = hit.snippet.clone();
            list = list.child(
                div()
                    .id(format!("folder-hit-{index}"))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(22.))
                    .px_3()
                    .cursor_pointer()
                    .hover(move |style| style.bg(palette.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_folder_match(index, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(320.))
                            .flex_none()
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .text_color(palette.muted)
                            .child(location),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .text_color(palette.fg)
                            .child(snippet),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(palette.border)
            .bg(palette.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(div().text_size(px(11.5)).text_color(palette.fg).child(
                        if self.folder.pending {
                            "文件夹搜索中…".to_string()
                        } else {
                            self.folder.status.clone()
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("folder-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_color(palette.muted)
                            .hover(move |style| style.bg(palette.hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                if let Some(cancel) = this.folder.cancel.take() {
                                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                this.folder.open = false;
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn render_status_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let lang = self.settings.ui.language;
        let (left, position, encoding, eol, language, extra) =
            match self.active.and_then(|index| self.tabs.get(index)) {
                Some(tab) => {
                    let left = if !self.status.is_empty() {
                        self.status.clone()
                    } else if let Some(change) = &tab.external_change {
                        change.clone()
                    } else {
                        self.status_path(tab)
                    };
                    let position = tab
                        .log
                        .as_ref()
                        .map(|log| LogicalPosition::new(log.start_line, 1))
                        .unwrap_or(tab.position);
                    (
                        left,
                        position,
                        if tab.detection == DetectionSource::Guessed {
                            format!("{}（检测）", tab.encoding.status_label())
                        } else {
                            tab.encoding.status_label()
                        },
                        tab.eol_label(),
                        tab.language.to_string(),
                        if let Some(log) = tab.log.as_ref() {
                            if log.following {
                                tr(lang, "跟随中", "Following").to_string()
                            } else {
                                tr(lang, "只读日志", "Read-only").to_string()
                            }
                        } else if tab.save_in_flight {
                            tr(lang, "保存中…", "Saving…").to_string()
                        } else if tab.dirty {
                            tr(lang, "未保存", "Modified").to_string()
                        } else {
                            tr(lang, "已保存", "Saved").to_string()
                        },
                    )
                }
                None => (
                    if self.status.is_empty() {
                        tr(lang, "就绪", "Ready").to_string()
                    } else {
                        self.status.clone()
                    },
                    LogicalPosition::default(),
                    "UTF-8".to_string(),
                    "—".to_string(),
                    "Plain Text".to_string(),
                    String::new(),
                ),
            };
        let item = |text: String, palette: Palette| {
            div()
                .px_2()
                .text_size(px(11.5))
                .text_color(palette.status_fg)
                .child(text)
        };

        div()
            .flex()
            .items_center()
            .h(px(24.))
            .px_3()
            .gap_1()
            .bg(palette.status_bg)
            .text_color(palette.status_fg)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(px(11.5))
                    .child(left),
            )
            .child(item(extra, palette))
            .child(item(language, palette))
            .child(item(eol, palette))
            .child(
                div()
                    .id("status-encoding")
                    .px_2()
                    .text_size(px(11.5))
                    .text_color(palette.status_fg)
                    .cursor_pointer()
                    .hover(move |style| style.bg(palette.hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_encoding_panel(cx);
                    }))
                    .child(encoding),
            )
            .child(item(
                format!(
                    "{} {}，{} {}",
                    tr(lang, "行", "Ln"),
                    position.one_based().0,
                    tr(lang, "列", "Col"),
                    position.one_based().1
                ),
                palette,
            ))
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&self.window_title());
        let palette = theme::palette(cx);

        let mut root = div()
            .id("notepust-workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_document))
            .on_action(cx.listener(Self::on_open_document))
            .on_action(cx.listener(Self::on_save_document))
            .on_action(cx.listener(Self::on_save_document_as))
            .on_action(cx.listener(Self::on_save_all))
            .on_action(cx.listener(Self::on_reload_document))
            .on_action(cx.listener(Self::on_close_document))
            .on_action(cx.listener(Self::on_toggle_find))
            .on_action(cx.listener(Self::on_toggle_replace))
            .on_action(cx.listener(Self::on_goto_line))
            .on_action(cx.listener(Self::on_quick_open))
            .on_action(cx.listener(Self::on_toggle_whitespace))
            .on_action(cx.listener(Self::on_toggle_encoding_panel))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_toggle_split))
            .on_action(cx.listener(Self::on_duplicate_line))
            .on_action(cx.listener(Self::on_delete_line))
            .on_action(cx.listener(Self::on_move_line_up))
            .on_action(cx.listener(Self::on_move_line_down))
            .on_action(cx.listener(Self::on_sort_lines))
            .on_action(cx.listener(Self::on_trim_lines))
            .on_action(cx.listener(Self::on_toggle_comment))
            .on_action(cx.listener(Self::on_indent_lines))
            .on_action(cx.listener(Self::on_outdent_lines))
            .on_action(cx.listener(Self::on_add_cursor_above))
            .on_action(cx.listener(Self::on_add_cursor_below))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_previous))
            .on_action(cx.listener(Self::on_replace_all))
            .on_action(cx.listener(Self::on_folder_search))
            .on_action(cx.listener(Self::on_next_document))
            .on_action(cx.listener(Self::on_previous_document))
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_toggle_language))
            .on_action(cx.listener(Self::on_kit_search))
            .on_action(cx.listener(Self::on_kit_replace))
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.bg)
            .text_color(palette.fg)
            .text_size(px(self.settings.ui.ui_font_size))
            .font(theme::ui_font(&self.settings))
            .child(self.render_toolbar(palette, cx))
            .child(self.render_tabs(palette, cx));

        if self.encoding_ui.open {
            root = root.child(self.render_encoding_bar(palette, cx));
        }

        if self.settings_open {
            root = root.child(self.render_settings_panel(palette, cx));
        }

        if self.search.open {
            root = root.child(self.render_search_bar(palette, cx));
        }

        if self.goto_open {
            root = root.child(self.render_goto_bar(palette, cx));
        }

        if self.quick.open {
            root = root.child(self.render_quick_open(palette, cx));
        }

        root = root.child(
            div()
                .flex_1()
                .min_h(px(0.))
                .bg(palette.bg)
                .child(self.render_editor_panes(palette, cx)),
        );

        if self.folder.open {
            root = root.child(self.render_folder_results(palette, cx));
        }

        root.child(self.render_status_bar(palette, cx))
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn tr(lang: Language, zh: &'static str, en: &'static str) -> &'static str {
    match lang {
        Language::ZhCn => zh,
        Language::En => en,
    }
}

fn position_of(editor: &Entity<EditorState>, cx: &App) -> LogicalPosition {
    let state = editor.read(cx);
    let offset = state.selected_range().start;
    let rope = state.text();
    let row = rope.byte_to_line_idx(offset, LineType::LF);
    let line_start = rope.line_to_byte_idx(row, LineType::LF);
    let end = offset.min(line_start.saturating_add(64 * 1024));
    let prefix = rope.slice(line_start..end).to_string();
    let column = notepust_text_codec::coords::grapheme_count(&prefix);
    LogicalPosition::new(row as u64, column)
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Opens a file for editing, accepting a content-detected encoding only when
/// the strict first pass refuses to guess.
fn open_for_editing(path: &Path, options: &OpenOptions) -> Result<OpenedFile, OpenError> {
    match open_file(path, options) {
        Err(OpenError::EncodingChoiceRequired { candidate }) => {
            let retry = OpenOptions {
                explicit_encoding: Some(EncodingSpec::new(candidate, BomPolicy::None)),
                ..options.clone()
            };
            open_file(path, &retry).map(|mut opened| {
                opened.detection = DetectionSource::Guessed;
                opened.warnings.push(format!(
                    "编码 {} 是根据内容检测的，请确认后再保存。",
                    opened.encoding.encoding.label()
                ));
                opened
            })
        }
        other => other,
    }
}

/// Normal editing up to the configured limit; above it (or when one line is
/// too long for the editor's layout) fall back to the read-only log reader.
fn open_tab_source(path: &Path, options: &OpenOptions) -> Result<OpenOutcome, OpenTabError> {
    match open_for_editing(path, options) {
        Ok(opened) => Ok(OpenOutcome::Editable(opened)),
        Err(OpenError::TooLarge { .. } | OpenError::LongLine { .. }) => open_log_source(path)
            .map(OpenOutcome::Log)
            .map_err(OpenTabError::Log),
        Err(error) => Err(OpenTabError::Edit(error)),
    }
}

fn open_log_source(path: &Path) -> Result<OpenedLog, LogError> {
    let mut reader = LogReader::open(path, None)?;
    let mut chunks = 0u32;
    while !reader.progress().complete
        && reader.index().lines_found() <= LOG_PAGE_LINES as u64
        && chunks < 64
    {
        reader.scan_step()?;
        chunks += 1;
    }
    if reader.observed_len() == 0 || reader.total_lines() == Some(0) {
        let progress = reader.progress();
        return Ok(OpenedLog {
            reader,
            lines: Vec::new(),
            start_line: 0,
            total_lines: Some(0),
            progress,
            tail_mode: false,
        });
    }
    match reader
        .try_viewport(0, LOG_PAGE_LINES, LOG_LINE_BYTES)
        .map(|view| (view.lines, view.total_lines))
    {
        Ok((lines, total_lines)) => {
            let progress = reader.progress();
            Ok(OpenedLog {
                reader,
                lines,
                start_line: 0,
                total_lines,
                progress,
                tail_mode: false,
            })
        }
        Err(LogError::NeedsIndex { .. }) => {
            let tail = reader.tail(LOG_PAGE_LINES, LOG_LINE_BYTES)?;
            let start_line = tail.lines.first().and_then(|line| line.line).unwrap_or(0);
            let progress = reader.progress();
            Ok(OpenedLog {
                reader,
                lines: tail.lines,
                start_line,
                total_lines: tail.total_lines,
                progress,
                tail_mode: true,
            })
        }
        Err(error) => Err(error),
    }
}

fn open_tab_error_label(error: &OpenTabError) -> String {
    match error {
        OpenTabError::Edit(error) => open_error_label(error),
        OpenTabError::Log(error) => format!("只读日志视图无法打开: {error}"),
    }
}

const QUICK_SKIP_DIRS: &[&str] = &[
    ".git",
    ".notepust",
    ".venv",
    "node_modules",
    "target",
    "venv",
    "$RECYCLE.BIN",
    "System Volume Information",
];

const QUICK_MAX_FILES: usize = 20_000;

fn collect_quick_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                if QUICK_SKIP_DIRS
                    .iter()
                    .any(|skip| name.to_string_lossy() == *skip)
                {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                files.push(relative.to_path_buf());
                if files.len() >= QUICK_MAX_FILES {
                    files.sort();
                    return files;
                }
            }
        }
    }
    files.sort();
    files
}

fn encoding_choices() -> Vec<(&'static str, &'static str, EncodingSpec)> {
    vec![
        (
            "utf8",
            "UTF-8",
            EncodingSpec::new(EncodingId::Utf8, BomPolicy::None),
        ),
        (
            "utf8bom",
            "UTF-8 BOM",
            EncodingSpec::new(EncodingId::Utf8, BomPolicy::Utf8),
        ),
        (
            "utf16le",
            "UTF-16 LE",
            EncodingSpec::new(EncodingId::Utf16Le, BomPolicy::Utf16Le),
        ),
        (
            "utf16be",
            "UTF-16 BE",
            EncodingSpec::new(EncodingId::Utf16Be, BomPolicy::Utf16Be),
        ),
        (
            "gbk",
            "GBK",
            EncodingSpec::new(EncodingId::Gbk, BomPolicy::None),
        ),
        (
            "gb18030",
            "GB18030",
            EncodingSpec::new(EncodingId::Gb18030, BomPolicy::None),
        ),
    ]
}

fn new_input(
    window: &mut Window,
    cx: &mut Context<Workspace>,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        let mut input = InputState::new(window, cx);
        input.set_placeholder(placeholder, window, cx);
        input
    })
}

fn parse_font_list(value: &str) -> Vec<String> {
    let mut fonts: Vec<String> = Vec::new();
    for part in value.split([',', ';']) {
        let font = part.trim();
        if !font.is_empty() && !fonts.iter().any(|existing| existing.as_str() == font) {
            fonts.push(font.to_string());
        }
    }
    fonts
}

fn filter_quick_files(files: &[PathBuf], query: &str, limit: usize) -> Vec<PathBuf> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return files.iter().take(limit).cloned().collect();
    }
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let mut scored = files
        .iter()
        .filter_map(|relative| {
            let text = relative.to_string_lossy().to_lowercase();
            if !tokens.iter().all(|token| text.contains(token)) {
                return None;
            }
            let name = relative
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let rank = if name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else {
                2
            };
            Some((rank, text.len(), relative.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.truncate(limit);
    scored
        .into_iter()
        .map(|(_, _, relative)| relative)
        .collect()
}

fn detect_language(path: Option<&Path>) -> &'static str {
    let Some(path) = path else {
        return "Plain Text";
    };
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rs" => "Rust",
        "json" => "JSON",
        "jsonc" => "JSONC",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "ini" => "INI",
        "xml" => "XML",
        "md" | "markdown" => "Markdown",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TSX",
        "jsx" => "JSX",
        "py" => "Python",
        "c" => "C",
        "h" | "hpp" | "cpp" | "cc" | "cxx" => "C++",
        "sql" => "SQL",
        "sh" | "bash" => "Shell",
        "log" => "Log",
        _ => "Plain Text",
    }
}

fn open_error_label(error: &OpenError) -> String {
    match error {
        OpenError::TooLarge {
            byte_len, limit, ..
        } => {
            format!("文件为 {byte_len} 字节，超过普通编辑上限 {limit} 字节；只读日志视图也未能打开")
        }
        other => other.to_string(),
    }
}

fn save_error_label(error: &SaveError) -> String {
    match error {
        SaveError::Conflict {
            detail,
            preserved_temp,
        } => {
            let mut text = format!("冲突: {detail}");
            if let Some(path) = preserved_temp {
                text.push_str(&format!("；已保留临时文件 {}", path.display()));
            }
            text
        }
        other => other.to_string(),
    }
}

fn search_error_label(error: &SearchError) -> String {
    match error {
        SearchError::AdvancedRequired { .. } => {
            "该正则包含前后查找/反向引用；请勾选 PCRE2 高级模式".to_string()
        }
        SearchError::AdvancedUnavailable => {
            "找不到 search-worker 程序；请将它与 notepust 放在同一目录".to_string()
        }
        SearchError::Cancelled => "搜索已取消".to_string(),
        other => format!("搜索失败: {other}"),
    }
}

fn worker_binary_path() -> Result<PathBuf, SearchError> {
    let name = if cfg!(windows) {
        "search-worker.exe"
    } else {
        "search-worker"
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(directory) = exe.parent()
    {
        candidates.push(directory.join(name));
    }
    // `cargo run`/`cargo test` place the app and test harness below the same
    // target directory, but not always next to the worker binary.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(profile)
            .join(name),
    );
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or(SearchError::AdvancedUnavailable)
}

/// Runs a PCRE2 find on a private snapshot of the current document. The
/// application keeps owning the text; the worker only sees bytes on disk.
fn advanced_find(
    text: &str,
    pattern: &str,
    options: notepust_contracts::SearchOptions,
    result_limit: usize,
) -> Result<Vec<Utf8Range>, SearchError> {
    let worker = worker_binary_path()?;
    let directory = tempfile::Builder::new()
        .prefix("notepust-regex-")
        .tempdir()
        .map_err(|error| SearchError::Protocol {
            detail: format!("could not create a regex snapshot directory: {error}"),
        })?;
    let snapshot = directory.path().join("snapshot.txt");
    std::fs::write(&snapshot, text.as_bytes()).map_err(|error| SearchError::Io {
        path: snapshot.display().to_string(),
        source: error,
    })?;

    let request = SearchRequest {
        job_id: JobId::new(),
        purpose: SearchPurpose::Find,
        engine: RegexEngine::Advanced,
        pattern: pattern.to_string(),
        options,
        snapshot_id: SnapshotId::new(),
        path: NativePath::from_path(&snapshot),
        utf8_length: text.len() as u64,
        timeout_ms: 10_000,
        result_limit,
        replacement: None,
        output_artifact_path: None,
    };
    let mut client = WorkerClient::spawn(&worker)?;
    let mut ranges = Vec::new();
    let run = client.search(
        request,
        &AtomicBool::new(false),
        |batch| ranges.extend(batch.matches.into_iter().map(|item| item.range())),
        |_| {},
    );
    client.shutdown();
    run.map(|_| ranges)
}

/// Runs a PCRE2 replacement in the worker and reads back its artifact.
fn advanced_replace(
    text: &str,
    pattern: &str,
    options: notepust_contracts::SearchOptions,
    replacement: notepust_contracts::ReplacementProgram,
) -> Result<(String, u64), SearchError> {
    let worker = worker_binary_path()?;
    let directory = tempfile::Builder::new()
        .prefix("notepust-regex-")
        .tempdir()
        .map_err(|error| SearchError::Protocol {
            detail: format!("could not create a regex snapshot directory: {error}"),
        })?;
    let snapshot = directory.path().join("snapshot.txt");
    let artifact = directory.path().join("replacement.txt");
    std::fs::write(&snapshot, text.as_bytes()).map_err(|error| SearchError::Io {
        path: snapshot.display().to_string(),
        source: error,
    })?;

    let request = SearchRequest {
        job_id: JobId::new(),
        purpose: SearchPurpose::ReplacePlan,
        engine: RegexEngine::Advanced,
        pattern: pattern.to_string(),
        options,
        snapshot_id: SnapshotId::new(),
        path: NativePath::from_path(&snapshot),
        utf8_length: text.len() as u64,
        timeout_ms: 30_000,
        result_limit: 200,
        replacement: Some(replacement),
        output_artifact_path: Some(NativePath::from_path(&artifact)),
    };
    let mut client = WorkerClient::spawn(&worker)?;
    let run = client.search(request, &AtomicBool::new(false), |_| {}, |_| {});
    client.shutdown();
    let run = run?;
    let ready = run.replacement.ok_or_else(|| SearchError::Protocol {
        detail: "the worker did not return a replacement artifact".to_string(),
    })?;
    if !ready.complete {
        return Err(SearchError::ResourceLimit {
            detail: "the replacement artifact is incomplete".to_string(),
        });
    }
    let bytes = std::fs::read(&artifact).map_err(|error| SearchError::Io {
        path: artifact.display().to_string(),
        source: error,
    })?;
    if blake3::hash(&bytes).as_bytes() != &ready.digest {
        return Err(SearchError::Protocol {
            detail: "the replacement artifact digest does not match".to_string(),
        });
    }
    let output = String::from_utf8(bytes).map_err(|error| SearchError::Protocol {
        detail: format!("the replacement artifact is not valid UTF-8: {error}"),
    })?;
    Ok((output, ready.match_count))
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod open_routing_tests {
    use std::fs;

    use super::{LOG_LINE_BYTES, OpenOptions, OpenOutcome, open_tab_source};

    #[test]
    fn small_text_opens_in_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        fs::write(&path, "hello\nworld").unwrap();
        let options = OpenOptions::default();
        match open_tab_source(&path, &options).unwrap() {
            OpenOutcome::Editable(opened) => assert_eq!(opened.text, "hello\nworld"),
            OpenOutcome::Log(_) => panic!("small text must use the editable path"),
        }
    }

    #[test]
    fn oversized_file_falls_back_to_the_log_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let options = OpenOptions {
            max_bytes: 8,
            ..OpenOptions::default()
        };
        match open_tab_source(&path, &options).unwrap() {
            OpenOutcome::Log(opened) => {
                assert!(opened.lines.iter().any(|line| line.text == "second"));
            }
            OpenOutcome::Editable(_) => panic!("oversized file must use the log path"),
        }
    }

    #[test]
    fn long_line_falls_back_to_a_truncated_log_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long-line.txt");
        let mut text = "a".repeat(3 * 1024 * 1024);
        text.push('\n');
        fs::write(&path, text).unwrap();
        match open_tab_source(&path, &OpenOptions::default()).unwrap() {
            OpenOutcome::Log(opened) => {
                let line = opened.lines.first().expect("one long line");
                assert!(line.truncated);
                assert_eq!(line.text.len(), LOG_LINE_BYTES);
            }
            OpenOutcome::Editable(_) => panic!("long line must use the log path"),
        }
    }
}

#[cfg(test)]
mod advanced_search_tests {
    use notepust_contracts::SearchOptions;

    use super::{advanced_find, advanced_replace, parse_replacement, worker_binary_path};

    fn worker_available() -> bool {
        worker_binary_path().is_ok()
    }

    #[test]
    fn lookahead_runs_in_the_pcre2_worker() {
        if !worker_available() {
            return;
        }
        let text = "foo1 bar2 foo3";
        let ranges = advanced_find(text, r"foo(?=\d)", SearchOptions::default(), 100).unwrap();
        let matches = ranges
            .iter()
            .map(|range| &text[range.start as usize..range.end as usize])
            .collect::<Vec<_>>();
        assert_eq!(matches, vec!["foo", "foo"]);
    }

    #[test]
    fn advanced_replacement_returns_a_verified_artifact() {
        if !worker_available() {
            return;
        }
        let program = parse_replacement("[$0]", false).unwrap();
        let (output, count) =
            advanced_replace("foo1 foo2", r"foo(\d)", SearchOptions::default(), program).unwrap();
        assert_eq!(output, "[foo1] [foo2]");
        assert_eq!(count, 2);
    }
}

#[cfg(test)]
mod quick_open_tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{collect_quick_files, filter_quick_files};

    #[test]
    fn prefers_file_name_matches_and_requires_all_tokens() {
        let files = vec![
            PathBuf::from("README.md"),
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/model.rs"),
        ];
        let matches = filter_quick_files(&files, "mo", 10);
        assert_eq!(matches.first(), Some(&PathBuf::from("src/model.rs")));

        let matches = filter_quick_files(&files, "src rs", 10);
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|path| path.starts_with("src")));
    }

    #[test]
    fn walk_skips_build_and_dependency_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "a").unwrap();
        fs::write(dir.path().join("target/skip.rs"), "b").unwrap();
        fs::write(dir.path().join("node_modules/pkg/skip.js"), "c").unwrap();

        let files = collect_quick_files(dir.path());
        assert!(files.contains(&PathBuf::from("src/a.rs")));
        assert!(
            files
                .iter()
                .all(|path| !path.starts_with("target") && !path.starts_with("node_modules"))
        );
    }
}

#[cfg(test)]
mod settings_font_tests {
    use super::parse_font_list;

    #[test]
    fn splits_deduplicates_and_trims_font_lists() {
        assert_eq!(
            parse_font_list(" Consolas, Microsoft YaHei UI ; Consolas ;; "),
            vec!["Consolas".to_string(), "Microsoft YaHei UI".to_string()]
        );
        assert!(parse_font_list(" , ; ").is_empty());
    }
}
