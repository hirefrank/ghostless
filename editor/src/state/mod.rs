use crate::{
    cli::run_cli,
    documents::{DocId, DocumentBuffer},
    prefs,
    session::{LaunchTarget, Session},
    worker::{CliRunner, InFlight, WakeFn, Worker, WriteRunner, execute_write_op},
    workspace_tree::{self, SessionSummary, WorkspaceEntry},
};
use rune::{TextEdit, Tx};
use rune_kit::{
    FileNode, TreeModel,
    diff::{DiffMode, DiffState, FileDiff},
    shell::PaneState,
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

mod autosave;
mod dialogs;
mod keybindings;
mod search_state;
mod voice;
mod workspace_actions;

#[cfg(test)]
mod tests;

pub(crate) use autosave::poll;
pub(crate) use dialogs::DialogKind;
pub(crate) use keybindings::on_key;
#[cfg(test)]
pub(in crate::state) use voice::judge_target;
pub(crate) use voice::section_for_document;
#[cfg(test)]
pub(in crate::state) use workspace_actions::classify_file_flags;
pub(in crate::state) use workspace_actions::{
    rel_path_from_abs, remap_renamed_path, resolve_mirror_path, tree_files,
};

pub(crate) const SESSION_AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
pub(crate) const FILE_AUTOSAVE_DELAY: Duration = Duration::from_millis(1500);
pub(crate) const JUDGE_DELAY: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Write,
    Preview,
    Compare,
}

impl ViewMode {
    pub(crate) fn as_index(self) -> usize {
        match self {
            ViewMode::Write => 0,
            ViewMode::Preview => 1,
            ViewMode::Compare => 2,
        }
    }

    pub(crate) fn from_index(index: usize) -> Self {
        match index {
            1 => ViewMode::Preview,
            2 => ViewMode::Compare,
            _ => ViewMode::Write,
        }
    }
}

/// Dialogs, focus, and close requests that belong to the open workspace.
/// Replacing this value on a workspace switch drops stale modal state.
#[derive(Default)]
pub(crate) struct WorkspaceChrome {
    pub(crate) focus_mode: bool,
    pub(crate) view_settings_open: bool,
    pub(crate) file_menu_open: bool,
    pub(crate) active_dialog: Option<DialogKind>,
    pub(crate) dialog_input: TextEdit,
    pub(crate) dialog_error: Option<String>,
}

pub(crate) struct Editor {
    pub(crate) documents: Vec<DocumentBuffer>,
    pub(crate) active_doc: Option<usize>,
    pub(crate) fallback_edit: TextEdit,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) voice_project: Option<PathBuf>,
    pub(crate) placeholder_token: Option<String>,
    pub(crate) store: prefs::EditorStateStore,
    pub(crate) sidebar_pane: PaneState,
    pub(crate) inspector_pane: PaneState,
    pub(crate) view_mode: ViewMode,
    pub(crate) reference: crate::ui::reference::ReferencePane,
    pub(crate) find: crate::search::FindState,
    pub(crate) quick: crate::ui::quick_switcher::QuickState,
    pub(crate) folder_search: crate::ui::folder_search::FolderSearchState,
    pub(crate) chrome: WorkspaceChrome,
    fonts_initialized: bool,
    pub(crate) file_tree: TreeModel<FileNode<WorkspaceEntry>>,
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) sessions_collapsed: bool,
    pub(crate) voice_collapsed: bool,
    pub(crate) selected_tree_id: Option<String>,
    pub(crate) diff: DiffState,
    pub(crate) worker_busy: Option<(DocId, InFlight)>,
    pub(crate) last_window_title: Option<String>,
    pub(crate) flushed: bool,
    pub(crate) reopen_window: Option<Box<dyn Fn()>>,
    pub(crate) status: String,
    pub(crate) judge_due: Option<f64>,
    judge_armed: bool,
    judge_inflight: Option<JudgeInFlight>,
    pub(crate) inspector_tab: usize,
    pub(crate) section_menu_open: bool,
    pub(crate) rule_draft: TextEdit,
    pub(crate) toast: Option<String>,
    pub(crate) pending_document_focus: bool,
    pub(crate) compare_unified: bool,
    pub(crate) conflict_compare: bool,
    pub(crate) session_origin: String,
    pub(crate) session_section: TextEdit,
    pub(crate) session_sources: TextEdit,
    pending_track: Option<DocId>,
    session_start_inflight: Option<(DocId, u64)>,
    pub(crate) bridge: crate::bridge::BridgeClient,
    pub(crate) worker: Worker,
}

struct JudgeInFlight {
    doc_id: DocId,
    generation: u64,
    judge_epoch: u64,
    section: String,
    request_id: u64,
}

impl Editor {
    pub(crate) fn new(target: LaunchTarget, wake: WakeFn) -> Self {
        let store = prefs::load_store();
        let runner: CliRunner = Box::new(run_cli);
        let writer: WriteRunner = Box::new(execute_write_op);
        match target {
            LaunchTarget::Session(session) => {
                let mut editor = Self::with_handlers(session, store, runner, writer, wake);
                if let Some(voice) = &editor.voice_project {
                    editor.sessions =
                        workspace_tree::scan_sessions(voice, &editor.store.session_links);
                }
                editor.prompt_recovery_if_needed();
                editor
            }
            LaunchTarget::Workspace {
                folder,
                voice_project,
                initial_file,
            } => {
                let mut editor = Self::empty_with_handlers(store, runner, writer, wake);
                editor.open_workspace_with_voice(folder, initial_file, voice_project);
                editor
            }
            LaunchTarget::Welcome { voice_project } => {
                let mut editor = Self::empty_with_handlers(store, runner, writer, wake);
                editor.placeholder_token = load_placeholder(voice_project.as_deref());
                editor.voice_project = voice_project;
                editor
            }
        }
    }
    pub(crate) fn with_handlers(
        session: Session,
        store: prefs::EditorStateStore,
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
    ) -> Self {
        Self::with_handlers_backend(session, store, runner, writer, wake, None)
    }
    pub(crate) fn with_handlers_backend(
        session: Session,
        store: prefs::EditorStateStore,
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
        backend: Option<Box<dyn crate::bridge::BridgeBackend>>,
    ) -> Self {
        let mirror_path = store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &session.project,
                &session.id,
            ))
            .and_then(|link| {
                (link.workspace_file.is_absolute() && !link.last_synced_sha256.is_empty())
                    .then(|| link.workspace_file.clone())
            });
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &session.source_name,
            &session.original,
            &session.after,
        )]);
        diff.set_mode(DiffMode::Split);
        let (worker, bridge) = boot_io(runner, writer, wake, backend);
        let voice = Some(session.project.clone());
        let session_link = store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &session.project,
                &session.id,
            ));
        let unknown_link = session_link.is_some_and(|link| link.last_synced_sha256.is_empty());
        let unresolved_link = session_link.is_some_and(|link| !link.workspace_file.is_absolute());
        let status = if unknown_link {
            "Linked draft has no known sync state; session saves locally".into()
        } else if unresolved_link {
            "Linked file unavailable without a workspace; session saves locally".into()
        } else if session.finished {
            "Finished".into()
        } else {
            "Ready".into()
        };
        let buf = DocumentBuffer::from_session(session, true, mirror_path);
        let sidebar_pane = PaneState::new(250.0).range(180.0, 420.0).collapsed();
        let inspector_pane = PaneState::new(310.0).range(240.0, 480.0);
        Self {
            documents: vec![buf],
            active_doc: Some(0),
            fallback_edit: TextEdit::new(""),
            workspace: None,
            placeholder_token: load_placeholder(voice.as_deref()),
            voice_project: voice,
            store,
            sidebar_pane,
            inspector_pane,
            view_mode: ViewMode::Write,
            reference: Default::default(),
            find: Default::default(),
            quick: Default::default(),
            folder_search: Default::default(),
            chrome: WorkspaceChrome::default(),
            fonts_initialized: false,
            file_tree: TreeModel::new().key("workspace-files"),
            sessions: Vec::new(),
            sessions_collapsed: false,
            voice_collapsed: false,
            selected_tree_id: None,
            diff,
            worker_busy: None,
            last_window_title: None,
            flushed: false,
            reopen_window: None,
            status,
            judge_due: None,
            judge_armed: true,
            judge_inflight: None,
            inspector_tab: 0,
            section_menu_open: false,
            rule_draft: TextEdit::new(""),
            toast: None,
            pending_document_focus: false,
            compare_unified: false,
            conflict_compare: false,
            session_origin: "ai-first".into(),
            session_section: TextEdit::new(""),
            session_sources: TextEdit::new(""),
            pending_track: None,
            session_start_inflight: None,
            bridge,
            worker,
        }
    }
    pub(in crate::state) fn empty_with_handlers(
        store: prefs::EditorStateStore,
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
    ) -> Self {
        let (worker, bridge) = boot_io(runner, writer, wake, None);
        let diff = DiffState::new(Vec::new());
        let mut sb = PaneState::new(250.0)
            .range(180.0, 420.0)
            .with_width(store.ui.sidebar_width);
        if !store.ui.sidebar_open {
            sb = sb.collapsed();
        }
        let mut insp = PaneState::new(310.0)
            .range(240.0, 480.0)
            .with_width(store.ui.inspector_width);
        if !store.ui.inspector_open {
            insp = insp.collapsed();
        }
        Self {
            documents: Vec::new(),
            active_doc: None,
            fallback_edit: TextEdit::new(""),
            workspace: None,
            voice_project: None,
            placeholder_token: None,
            store,
            sidebar_pane: sb,
            inspector_pane: insp,
            view_mode: ViewMode::Write,
            reference: Default::default(),
            find: Default::default(),
            quick: Default::default(),
            folder_search: Default::default(),
            chrome: WorkspaceChrome::default(),
            fonts_initialized: false,
            file_tree: TreeModel::new().key("workspace-files"),
            sessions: Vec::new(),
            sessions_collapsed: false,
            voice_collapsed: false,
            selected_tree_id: None,
            diff,
            worker_busy: None,
            last_window_title: None,
            flushed: false,
            reopen_window: None,
            status: "Ready".into(),
            judge_due: None,
            judge_armed: false,
            judge_inflight: None,
            inspector_tab: 0,
            section_menu_open: false,
            rule_draft: TextEdit::new(""),
            toast: None,
            pending_document_focus: false,
            compare_unified: false,
            conflict_compare: false,
            session_origin: "ai-first".into(),
            session_section: TextEdit::new(""),
            session_sources: TextEdit::new(""),
            pending_track: None,
            session_start_inflight: None,
            bridge,
            worker,
        }
    }
    pub(in crate::state) fn apply_workspace_pane_defaults(&mut self) {
        let mut sb = PaneState::new(250.0)
            .range(180.0, 420.0)
            .with_width(self.store.ui.sidebar_width);
        if !self.store.ui.sidebar_open {
            sb = sb.collapsed();
        }
        let mut insp = PaneState::new(310.0)
            .range(240.0, 480.0)
            .with_width(self.store.ui.inspector_width);
        if !self.store.ui.inspector_open {
            insp = insp.collapsed();
        }
        self.sidebar_pane = sb;
        self.inspector_pane = insp;
    }
    pub(crate) fn is_landing_view(&self) -> bool {
        self.workspace.is_none() && self.documents.is_empty()
    }
    pub(crate) fn active_doc_ref(&self) -> Option<&DocumentBuffer> {
        self.active_doc.and_then(|i| self.documents.get(i))
    }
    pub(crate) fn active_doc_mut(&mut self) -> Option<&mut DocumentBuffer> {
        self.active_doc.and_then(|i| self.documents.get_mut(i))
    }
    pub(crate) fn active_is_session(&self) -> bool {
        self.active_doc_ref()
            .is_some_and(DocumentBuffer::is_session)
    }
    pub(crate) fn active_has_baseline(&self) -> bool {
        self.active_doc_ref().is_some_and(|d| d.original.is_some())
    }
    pub(crate) fn active_uses_rendered_preview(&self) -> bool {
        match self.view_mode {
            ViewMode::Preview => true,
            ViewMode::Compare if self.active_has_baseline() => false,
            _ => self.active_doc_ref().is_some_and(|doc| {
                doc.read_only
                    && doc
                        .path
                        .file_name()
                        .is_some_and(|name| name == "voice-guide.md")
            }),
        }
    }
    pub(crate) fn can_replace_find(&self) -> bool {
        self.view_mode == ViewMode::Write && !self.active_is_locked()
    }
    pub(crate) fn active_is_locked(&self) -> bool {
        match self.active_doc_ref() {
            Some(doc) => {
                !doc.is_editable()
                    || self
                        .worker_busy
                        .as_ref()
                        .is_some_and(|(id, kind)| id == &doc.id && *kind == InFlight::Finish)
            }
            None => true,
        }
    }
    pub(crate) fn active_working_edit(&self) -> &TextEdit {
        self.active_doc_ref()
            .map_or(&self.fallback_edit, |d| &d.edit)
    }
    pub(crate) fn active_working_edit_mut(&mut self) -> &mut TextEdit {
        let Some(idx) = self.active_doc else {
            return &mut self.fallback_edit;
        };
        self.documents
            .get_mut(idx)
            .map_or(&mut self.fallback_edit, |d| &mut d.edit)
    }
    pub(crate) fn active_notes_edit(&self) -> &TextEdit {
        self.active_doc_ref()
            .and_then(|d| d.notes.as_ref())
            .unwrap_or(&self.fallback_edit)
    }
    pub(crate) fn active_notes_edit_mut(&mut self) -> &mut TextEdit {
        let Some(idx) = self.active_doc else {
            return &mut self.fallback_edit;
        };
        self.documents
            .get_mut(idx)
            .and_then(|d| d.notes.as_mut())
            .unwrap_or(&mut self.fallback_edit)
    }
    pub(crate) fn dirty_file_paths(&self) -> HashSet<PathBuf> {
        self.documents
            .iter()
            .filter(|d| !d.is_session() && d.is_dirty())
            .map(|d| d.path.clone())
            .collect()
    }
    pub(in crate::state) fn flush_leaving_doc(&mut self) {
        let autosave = self.store.ui.autosave_workspace_files;
        if let Some(doc) = self.active_doc_mut()
            && doc.is_editable()
            && doc.is_dirty()
            && (doc.is_session() || autosave)
        {
            doc.pending_save = true;
            doc.autosave_due = None;
        }
    }
    pub(crate) fn switch_active_doc(&mut self, pos: usize) {
        if pos >= self.documents.len() {
            return;
        }
        if self.active_doc != Some(pos) {
            self.reconcile_active_linked_doc();
            self.flush_leaving_doc();
            self.active_doc = Some(pos);
            self.conflict_compare = false;
            self.refresh_find();
            self.dispatch_next();
        }
        self.reconcile_active_linked_doc();
        if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
            self.conflict_compare = false;
        } else if self.view_mode == ViewMode::Compare {
            self.refresh_diff();
        }
        self.judge_due = None;
        self.judge_armed = true;
    }
    pub(crate) fn activate_tab(&mut self, tab_key: &str, tx: &mut Tx) {
        if let Some(pos) = self
            .documents
            .iter()
            .position(|d| d.id.tab_key() == tab_key)
        {
            self.switch_active_doc(pos);
            self.prompt_recovery_if_needed();
            if let Some(doc) = self.documents.get(pos)
                && !doc.is_session()
                && let Some(rel) = self.rel_from_workspace(&doc.path)
            {
                self.selected_tree_id = Some(rel.clone());
                self.file_tree.expand_to(&rel);
                self.file_tree.select(&rel);
            }
            self.sync_window_title(tx);
        }
    }
    pub(crate) fn pin_tab_by_id(&mut self, tab_key: &str) {
        if let Some(doc) = self
            .documents
            .iter_mut()
            .find(|d| d.id.tab_key() == tab_key)
        {
            doc.pinned = true;
        }
    }
    pub(crate) fn close_tab_by_id(&mut self, tab_key: &str, tx: &mut Tx) -> bool {
        let Some(pos) = self
            .documents
            .iter()
            .position(|d| d.id.tab_key() == tab_key)
        else {
            return false;
        };
        let doc = &self.documents[pos];
        if !doc.is_session() && !self.store.ui.autosave_workspace_files && doc.is_dirty() {
            self.chrome.active_dialog = Some(DialogKind::ConfirmClose {
                doc_id: doc.id.clone(),
                title: doc.title.clone(),
            });
            self.chrome.dialog_error = None;
            return false;
        }
        let doc_id = doc.id.clone();
        if doc.is_dirty() {
            if doc.read_only || !self.drain_single_doc_save(&doc_id) {
                return false;
            }
        } else if self
            .worker_busy
            .as_ref()
            .is_some_and(|(id, _)| id == &doc_id)
        {
            self.drain_until_idle();
        }
        if let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) {
            self.remove_doc_at(pos);
        }
        self.sync_window_title(tx);
        true
    }
    pub(in crate::state) fn remove_doc_at(&mut self, pos: usize) {
        let previous = self
            .active_doc
            .and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
        self.documents.remove(pos);
        if self.documents.is_empty() {
            self.active_doc = None;
        } else {
            let next = match self.active_doc {
                Some(active) if active > pos => active - 1,
                Some(active) if active == pos => pos.min(self.documents.len() - 1),
                Some(active) => active,
                None => 0,
            };
            self.active_doc = Some(next);
        }
        self.refresh_find();
        if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        } else if self.view_mode == ViewMode::Compare {
            self.refresh_diff();
        }
        let current = self
            .active_doc
            .and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
        if previous != current {
            self.conflict_compare = false;
            self.judge_due = None;
            self.judge_armed = self.active_doc.is_some();
        }
        self.save_open_tabs_pref();
    }
    pub(crate) fn close_active_tab(&mut self, tx: &mut Tx) -> bool {
        let Some(key) = self.active_doc_ref().map(|d| d.id.tab_key()) else {
            return false;
        };
        self.close_tab_by_id(&key, tx)
    }
    pub(crate) fn cycle_tab(&mut self, backwards: bool, tx: &mut Tx) {
        let len = self.documents.len();
        if len <= 1 {
            return;
        }
        let cur = self.active_doc.unwrap_or(0);
        let next = if backwards {
            (cur + len - 1) % len
        } else {
            (cur + 1) % len
        };
        let key = self.documents[next].id.tab_key();
        self.activate_tab(&key, tx);
    }
    pub(crate) fn desired_window_title(&self) -> String {
        if let Some(doc) = self.active_doc_ref() {
            format!("{} — ghostless", doc.title)
        } else if let Some(ws) = &self.workspace {
            let name = ws
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("ghostless");
            format!("{name} — ghostless")
        } else {
            "ghostless editor".into()
        }
    }
    pub(crate) fn sync_window_title(&mut self, tx: &mut Tx) {
        let desired = self.desired_window_title();
        if self.last_window_title.as_deref() != Some(&desired) {
            self.last_window_title = Some(desired.clone());
            tx.set_title(desired);
        }
    }
    pub(in crate::state) fn rel_from_workspace(&self, abs: &Path) -> Option<String> {
        let root = self.workspace.as_deref()?;
        rel_path_from_abs(root, abs)
    }
    pub(in crate::state) fn save_open_tabs_pref(&mut self) {
        let Some(folder) = &self.workspace else {
            return;
        };
        let rels: Vec<String> = self
            .documents
            .iter()
            .filter_map(|d| {
                if d.is_session() {
                    None
                } else {
                    rel_path_from_abs(folder, &d.path)
                }
            })
            .collect();
        self.store
            .last_open_files
            .insert(folder.display().to_string(), rels);
        let _ = prefs::save_store(&self.store);
    }
    pub(crate) fn save_ui_prefs_if_workspace(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.store.ui.sidebar_open = self.sidebar_pane.is_open();
        self.store.ui.sidebar_width = self.sidebar_pane.width();
        self.store.ui.inspector_open = self.inspector_pane.is_open();
        self.store.ui.inspector_width = self.inspector_pane.width();
        let _ = prefs::save_store(&self.store);
    }
    pub(crate) fn toggle_active_user_lock(&mut self) {
        if let Some(doc) = self.active_doc_mut()
            && doc.lockable
            && !doc.read_only
            && !doc.finished
        {
            doc.user_locked = !doc.user_locked;
        }
    }
    pub(crate) fn set_view_mode(&mut self, mode: ViewMode, tx: &mut Tx) {
        if mode != ViewMode::Compare {
            self.conflict_compare = false;
        }
        if mode == ViewMode::Compare {
            if !self.active_has_baseline() {
                return;
            }
            self.find.close();
            self.refresh_diff();
        }
        self.view_mode = mode;
        if !self.can_replace_find() {
            self.find.show_replace = false;
        }
        if self.find.open {
            self.reveal_find_position(tx);
        }
    }
    pub(crate) fn document_focus_key(&self) -> Option<&'static str> {
        self.active_doc_ref()?;
        Some(if self.active_uses_rendered_preview() {
            crate::ui::PREVIEW_BODY
        } else if self.view_mode == ViewMode::Compare && self.active_has_baseline() {
            "compare-body"
        } else if self.active_is_locked() {
            "working-body"
        } else {
            "prose-input"
        })
    }
    pub(crate) fn focus_document(&self, tx: &mut Tx) {
        if let Some(key) = self.document_focus_key() {
            tx.focus(key, false);
        }
    }
}

pub(in crate::state) fn load_placeholder(project: Option<&Path>) -> Option<String> {
    let raw = fs::read_to_string(project?.join("manifest.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let token = value.get("placeholder")?.as_str()?.trim();
    if token.is_empty() || token.contains(['[', ']', ':']) {
        None
    } else {
        Some(token.to_owned())
    }
}

fn boot_io(
    runner: CliRunner,
    writer: WriteRunner,
    wake: WakeFn,
    backend: Option<Box<dyn crate::bridge::BridgeBackend>>,
) -> (Worker, crate::bridge::BridgeClient) {
    let wake = Arc::new(wake);
    let bridge = crate::bridge::BridgeClient::spawn(Arc::clone(&wake), backend);
    let worker = Worker::spawn_with_shared_wake(runner, writer, wake);
    (worker, bridge)
}

pub(crate) fn on_window_close(editor: &mut Editor, tx: &mut Tx) {
    if editor.flush_on_exit() {
        tx.quit();
    } else {
        editor.show_close_error();
        if let Some(reopen) = &editor.reopen_window {
            reopen();
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if !self.flush_on_exit() {
            // Always recover every dirty buffer on drop, even after a cancelled quit and later edits.
            self.write_quit_recovery();
            self.worker.shutdown();
        }
        self.bridge.shutdown();
    }
}
