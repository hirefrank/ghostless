use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use rune::{Key, KeyEvent, TextEdit, Tx};
use rune_kit::{
    FileNode, TreeModel,
    diff::{DiffMode, DiffState, FileDiff},
    shell::{PaneState, is_sidebar_toggle},
};
use crate::{
    cli::run_cli,
    documents::{CachedJudge, DocId, DocumentBuffer},
    fs_io,
    prefs,
    session::{LaunchTarget, Session, load_session},
    worker::{
        CliRunner, InFlight, SearchReply, WakeFn, Worker, WorkerReply, WorkerRequest, WriteRunner,
        WriteOutcome, clear_recovery_copy, execute_write_op,
    },
    workspace_tree::{self, SessionSummary, WorkspaceEntry},
};

fn load_placeholder(project: Option<&Path>) -> Option<String> {
    let raw = fs::read_to_string(project?.join("manifest.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let token = value.get("placeholder")?.as_str()?.trim();
    if token.is_empty() || token.contains(['[', ']', ':']) { None } else { Some(token.to_owned()) }
}

fn tree_files(tree: &TreeModel<FileNode<WorkspaceEntry>>) -> Vec<WorkspaceEntry> {
    fn collect(tree: &TreeModel<FileNode<WorkspaceEntry>>, parent: Option<&str>, out: &mut Vec<WorkspaceEntry>) {
        for id in tree.children(parent) {
            if let Some(node) = tree.get(id) {
                if let Some(entry) = &node.data { out.push(entry.clone()); }
                if node.dir { collect(tree, Some(id), out); }
            }
        }
    }
    let mut entries = Vec::new();
    collect(tree, None, &mut entries);
    entries
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DialogKind {
    NewFile { parent_rel: String },
    NewFolder { parent_rel: String },
    Rename { rel_path: String },
    Delete { rel_path: String, display_name: String, is_dir: bool },
    ConfirmClose { doc_id: DocId, title: String },
    Recover { doc_id: DocId, title: String },
    StartSession,
    ConfirmRevert,
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
    pub(crate) focus_mode: bool,
    pub(crate) view_settings_open: bool,
    fonts_initialized: bool,
    pub(crate) file_tree: TreeModel<FileNode<WorkspaceEntry>>,
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) sessions_collapsed: bool,
    pub(crate) voice_collapsed: bool,
    pub(crate) selected_tree_id: Option<String>,
    pub(crate) file_menu_open: bool,
    pub(crate) active_dialog: Option<DialogKind>,
    pub(crate) dialog_input: TextEdit,
    pub(crate) dialog_error: Option<String>,
    pub(crate) diff: DiffState,
    pub(crate) worker_busy: Option<(DocId, InFlight)>,
    pub(crate) pending_close: Option<DocId>,
    pub(crate) last_window_title: Option<String>,
    pub(crate) flushed: bool,
    quit_recovery_needed: bool,
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
        let session_link = store.session_links.get(&prefs::EditorStateStore::session_link_key(&session.project, &session.id));
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
            focus_mode: false,
            view_settings_open: false,
            fonts_initialized: false,
            file_tree: TreeModel::new().key("workspace-files"),
            sessions: Vec::new(),
            sessions_collapsed: false,
            voice_collapsed: false,
            selected_tree_id: None,
            file_menu_open: false,
            active_dialog: None,
            dialog_input: TextEdit::new(""),
            dialog_error: None,
            diff,
            worker_busy: None,
            pending_close: None,
            last_window_title: None,
            flushed: false,
            quit_recovery_needed: false,
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

    fn empty_with_handlers(
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
            focus_mode: false,
            view_settings_open: false,
            fonts_initialized: false,
            file_tree: TreeModel::new().key("workspace-files"),
            sessions: Vec::new(),
            sessions_collapsed: false,
            voice_collapsed: false,
            selected_tree_id: None,
            file_menu_open: false,
            active_dialog: None,
            dialog_input: TextEdit::new(""),
            dialog_error: None,
            diff,
            worker_busy: None,
            pending_close: None,
            last_window_title: None,
            flushed: false,
            quit_recovery_needed: false,
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

    fn apply_workspace_pane_defaults(&mut self) {
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
        self.active_doc_ref().is_some_and(DocumentBuffer::is_session)
    }

    pub(crate) fn active_has_baseline(&self) -> bool {
        self.active_doc_ref()
            .is_some_and(|d| d.original.is_some())
    }

    pub(crate) fn active_uses_rendered_preview(&self) -> bool {
        match self.view_mode {
            ViewMode::Preview => true,
            ViewMode::Compare if self.active_has_baseline() => false,
            _ => self.active_doc_ref().is_some_and(|doc| {
                doc.read_only && doc.path.file_name().is_some_and(|name| name == "voice-guide.md")
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
                    || self.worker_busy.as_ref().is_some_and(|(id, kind)| {
                        id == &doc.id && *kind == InFlight::Finish
                    })
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

    pub(crate) fn open_workspace(&mut self, folder: PathBuf, initial_file: Option<PathBuf>) {
        self.open_workspace_with_voice(folder, initial_file, None);
    }

    fn open_workspace_with_voice(&mut self, folder: PathBuf, initial_file: Option<PathBuf>, voice_project: Option<PathBuf>) {
        if !self.drain_autosave_work() {
            return;
        }
        if self.documents.iter().any(|doc| doc.is_dirty()) {
            self.status = "Workspace not changed: save or discard unsaved documents first".into();
            return;
        }
        let folder = folder.canonicalize().unwrap_or(folder);
        self.voice_project = voice_project.or_else(|| crate::cli::discover_voice_project(&folder, None));
        self.placeholder_token = load_placeholder(self.voice_project.as_deref());
        self.store.record_workspace(&folder);
        self.workspace = Some(folder.clone());
        self.documents.clear();
        self.reference = Default::default();
        self.find = Default::default();
        self.quick = Default::default();
        self.close_folder_search();
        self.folder_search = Default::default();
        self.active_doc = None;
        self.selected_tree_id = None;
        self.view_mode = ViewMode::Write;
        self.apply_workspace_pane_defaults();
        self.file_tree = workspace_tree::scan_workspace(
            &folder,
            self.voice_project.as_deref(),
            self.store.ui.show_all_files,
        );
        self.sessions = self
            .voice_project
            .as_deref()
            .map(|voice| workspace_tree::scan_sessions(voice, &self.store.session_links))
            .unwrap_or_default();

        if let Some(file) = initial_file {
            self.open_workspace_file(file, true);
        } else {
            let key = folder.display().to_string();
            let recent_files = self.store.last_open_files.get(&key).cloned().unwrap_or_default();
            for rel in recent_files {
                let candidate = folder.join(&rel);
                if candidate.is_file() && fs_io::is_contained_in(&folder, &candidate) {
                    self.open_workspace_file(candidate, true);
                }
            }
        }

        self.judge_due = None;
        self.judge_inflight = None;
        self.conflict_compare = false;
        self.pending_track = None;
        self.session_start_inflight = None;
        self.judge_armed = self.active_doc.is_some();
        if let Err(error) = prefs::save_store(&self.store) {
            self.status = error;
        } else {
            self.status = "Ready".into();
        }
    }

    pub(crate) fn refresh_workspace_tree(&mut self) {
        let Some(folder) = &self.workspace else {
            return;
        };
        let mut fresh = workspace_tree::scan_workspace(
            folder,
            self.voice_project.as_deref(),
            self.store.ui.show_all_files,
        );
        workspace_tree::preserve_open_folders(&self.file_tree, &mut fresh);
        if let Some(sel) = &self.selected_tree_id
            && fresh.contains(sel)
        {
            fresh.expand_to(sel);
            fresh.select(sel);
        }
        self.file_tree = fresh;
        if let Some(voice) = &self.voice_project {
            self.sessions = workspace_tree::scan_sessions(voice, &self.store.session_links);
        }
    }

    pub(crate) fn open_workspace_file(&mut self, path: PathBuf, pin: bool) {
        let canon = path.canonicalize().unwrap_or(path);
        let id = DocId::File(canon.clone());
        if let Some(pos) = self.documents.iter().position(|d| d.id == id) {
            if pin {
                self.documents[pos].pinned = true;
            }
            self.switch_active_doc(pos);
            self.prompt_recovery_if_needed();
            if let Some(rel) = self.rel_from_workspace(&canon) {
                self.selected_tree_id = Some(rel.clone());
                self.file_tree.select(&rel);
            }
            return;
        }
        let text = match fs_io::read_utf8(&canon) {
            Ok(text) => text,
            Err(err) => {
                self.status = err;
                return;
            }
        };
        let title = canon
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        let rel_id = self.rel_from_workspace(&canon);
        let tree_entry = rel_id
            .as_deref()
            .and_then(|rel| self.file_tree.get(rel))
            .and_then(|node| node.data.as_ref());
        let (read_only, lockable) =
            classify_file_flags(tree_entry, &canon, self.voice_project.as_deref());
        let buf = DocumentBuffer::from_workspace_file(
            canon,
            title,
            text,
            read_only,
            lockable,
            pin,
            self.voice_project.clone(),
        );
        self.flush_leaving_doc();
        if !pin
            && let Some(preview_idx) = self
                .documents
                .iter()
                .position(|d| !d.pinned && !d.is_dirty() && !d.is_session())
        {
            self.documents[preview_idx] = buf;
            self.active_doc = Some(preview_idx);
        } else {
            self.documents.push(buf);
            self.active_doc = Some(self.documents.len() - 1);
        }
        self.refresh_find();
        self.dispatch_next();
        if self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        }
        if let Some(rel) = rel_id {
            self.selected_tree_id = Some(rel.clone());
            self.file_tree.select(&rel);
        }
        self.save_open_tabs_pref();
        self.reconcile_linked_file();
        self.prompt_recovery_if_needed();
        self.judge_due = None;
        self.judge_armed = true;
    }

    fn prompt_recovery_if_needed(&mut self) {
        let Some(doc) = self.active_doc_ref() else { return; };
        if doc.read_only || doc.is_dirty() || !doc.recovery_path().is_file() { return; }
        if self.active_dialog.is_none() {
            self.active_dialog = Some(DialogKind::Recover { doc_id: doc.id.clone(), title: doc.title.clone() });
        } else {
            self.status = "A recovery copy is available; reopen the file to review it".into();
        }
    }

    pub(crate) fn resolve_recovery(&mut self, restore: bool) {
        let Some(DialogKind::Recover { doc_id, .. }) = self.active_dialog.clone() else { return; };
        let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) else { self.close_dialog(); return; };
        self.active_doc = Some(pos);
        let doc = &mut self.documents[pos];
        let recovery = doc.recovery_path();
        if restore {
            match doc.read_recovery() {
                Ok(snapshot) => {
                    doc.edit = TextEdit::new(snapshot.text);
                    if doc.is_session() {
                        doc.notes = Some(TextEdit::new(snapshot.notes));
                    }
                    doc.generation += 1;
                    doc.pinned = true;
                    doc.last_saved_label = Some("Unsaved".into());
                    doc.cached_judge = None;
                    doc.latest_judge_request = None;
                    self.refresh_diff();
                    self.judge_armed = true;
                    self.status = "Recovered unsaved changes; save when ready".into();
                }
                Err(error) => { self.dialog_error = Some(error); return; }
            }
        } else if let Err(error) = fs::remove_file(&recovery)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            self.dialog_error = Some(format!("Cannot remove recovery copy: {error}"));
            return;
        }
        self.close_dialog();
        self.refresh_find();
    }

    pub(crate) fn open_session_entry(&mut self, project: PathBuf, session_id: String) {
        let id = DocId::Session {
            project: project.clone(),
            id: session_id.clone(),
        };
        if let Some(pos) = self.documents.iter().position(|d| d.id == id) {
            self.documents[pos].pinned = true;
            self.switch_active_doc(pos);
            self.reconcile_active_linked_doc();
            self.prompt_recovery_if_needed();
            return;
        }
        let loaded = match load_session(project.clone(), session_id.clone()) {
            Ok(session) => session,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let mirror_path = self
            .store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &project,
                &session_id,
            ))
            .and_then(|link| {
                (!link.last_synced_sha256.is_empty() && (self.workspace.is_some() || link.workspace_file.is_absolute()))
                    .then(|| resolve_mirror_path(&link.workspace_file, self.workspace.as_deref()))
            });
        let session_link = self.store.session_links.get(&prefs::EditorStateStore::session_link_key(&project, &session_id));
        let unknown_link = session_link.is_some_and(|link| link.last_synced_sha256.is_empty());
        let unresolved_link = self.workspace.is_none() && session_link.is_some_and(|link| !link.workspace_file.is_absolute());
        let finished = loaded.finished;
        let missing_link = mirror_path.as_ref().is_some_and(|path| !path.is_file());
        let mirror_path = if missing_link { None } else { mirror_path };
        let buf = DocumentBuffer::from_session(loaded, true, mirror_path);
        self.flush_leaving_doc();
        self.documents.push(buf);
        self.active_doc = Some(self.documents.len() - 1);
        self.refresh_find();
        self.refresh_diff();
        self.dispatch_next();
        self.status = if missing_link {
            "The linked workspace file is gone. Editing the session copy.".into()
        } else if unknown_link {
            "Linked draft has no known sync state; session saves locally".into()
        } else if unresolved_link {
            "Linked file unavailable without a workspace; session saves locally".into()
        } else if finished {
            "Finished".into()
        } else {
            "Ready".into()
        };
        self.reconcile_active_linked_doc();
        self.judge_due = None;
        self.judge_armed = true;
        self.prompt_recovery_if_needed();
    }

    pub(crate) fn open_voice_doc(&mut self, filename: &str, read_only: bool) {
        let Some(voice) = self.voice_project.clone() else {
            self.status = "No voice project attached".into();
            return;
        };
        let path = voice.join(filename);
        if !path.is_file() {
            if filename == "voice-guide.md" {
                self.status = "voice-guide.md not found — run `ghostless build`".into();
            } else {
                self.status = format!("{filename} not found in voice project");
            }
            return;
        }
        self.open_workspace_file(path, true);
        if read_only && let Some(doc) = self.active_doc_mut() {
            doc.read_only = true;
            doc.last_saved_label = Some("Read-only".into());
        }
    }

    pub(crate) fn open_quick(&mut self, tx: &mut Tx) {
        use crate::quick_open::{QuickItem, QuickTarget};
        let mut items = Vec::new();
        let files = tree_files(&self.file_tree);
        for doc in self.documents.iter().rev() {
            if let Some(entry) = files.iter().find(|entry| entry.abs_path == doc.path) {
                items.push(QuickItem { id: entry.rel_path.clone(), title: doc.title.clone(), subtitle: entry.rel_path.clone(), target: QuickTarget::File(entry.abs_path.clone()) });
            }
        }
        for session in &self.sessions {
            items.push(QuickItem { id: format!("session:{}", session.id), title: session.source_name.clone(), subtitle: format!("session · {:?}", session.status).to_lowercase(), target: QuickTarget::Session(session.id.clone()) });
        }
        for entry in files {
            if items.iter().any(|item| item.id == entry.rel_path) { continue; }
            let title = entry.abs_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            items.push(QuickItem { id: entry.rel_path.clone(), title, subtitle: entry.rel_path, target: QuickTarget::File(entry.abs_path) });
        }
        self.quick.items = items;
        self.quick.query.replace_text("");
        self.quick.selected = 0;
        self.quick.open = true;
        tx.focus("quick-query", false);
    }

    pub(crate) fn choose_quick(&mut self, index: usize, tx: &mut Tx) {
        use crate::quick_open::QuickTarget;
        let item = crate::ui::quick_switcher::filtered(&self.quick).get(index).cloned();
        if let Some(item) = item {
            match item.target {
                QuickTarget::File(path) => self.open_workspace_file(path, true),
                QuickTarget::Session(id) => if let Some(project) = self.voice_project.clone() { self.open_session_entry(project, id); },
            }
            self.quick.open = false;
            self.focus_document(tx);
        }
    }

    pub(crate) fn open_folder_search(&mut self, tx: &mut Tx) {
        if self.workspace.is_none() {
            return;
        }
        self.close_folder_search();
        self.folder_search = Default::default();
        self.folder_search.open = true;
        tx.focus("folder-query", false);
    }

    pub(crate) fn close_folder_search(&mut self) {
        self.worker.search.cancel();
        self.folder_search.open = false;
        self.folder_search.due = None;
        self.folder_search.request_id = None;
        self.folder_search.matches.clear();
    }

    pub(crate) fn schedule_folder_search(&mut self, tx: &mut Tx) {
        self.worker.search.cancel();
        self.folder_search.request_id = None;
        self.folder_search.matches.clear();
        self.folder_search.selected = 0;
        self.folder_search.message = None;
        self.folder_search.due = (self.folder_search.query.text.chars().count() >= 2)
            .then_some(tx.clock() + 0.2);
        if self.folder_search.due.is_some() {
            tx.wake_after(Duration::from_millis(200));
        }
    }

    fn submit_folder_search(&mut self) {
        use crate::quick_open::MAX_SEARCH_FILE_BYTES;

        let buffers = self.documents.iter().filter_map(|doc| {
            let DocId::File(path) = &doc.id else { return None };
            // An oversized open buffer is excluded, including its disk copy.
            let content = (doc.edit.text.len() <= MAX_SEARCH_FILE_BYTES)
                .then(|| doc.edit.text.clone());
            Some((path.clone(), content))
        }).collect();
        match self.worker.search.submit(
            tree_files(&self.file_tree),
            buffers,
            self.folder_search.query.text.clone(),
        ) {
            Ok(id) => self.folder_search.request_id = Some(id),
            Err(error) => self.folder_search.message = Some(error),
        }
    }

    fn accept_folder_search(&mut self, reply: SearchReply) -> bool {
        if !self.folder_search.open || self.folder_search.request_id != Some(reply.id) {
            return false;
        }
        self.folder_search.request_id = None;
        self.folder_search.matches = reply.matches;
        self.folder_search.selected = 0;
        true
    }

    pub(crate) fn choose_folder_match(&mut self, index: usize, tx: &mut Tx) {
        let Some(hit) = self.folder_search.matches.get(index).cloned() else { return };
        let path = hit.abs_path.canonicalize().unwrap_or_else(|_| hit.abs_path.clone());
        let expected = DocId::File(path.clone());
        self.open_workspace_file(path, true);
        let Some(doc) = self.active_doc.and_then(|index| self.documents.get(index))
            .filter(|doc| doc.id == expected)
        else {
            self.folder_search.message = Some(self.status.clone());
            return;
        };

        let query = &self.folder_search.query.text;
        let matches = crate::search::find_matches(&doc.edit.text, query, false, false);
        let Some((index, same_line)) = hit.locate(&doc.edit.text, &matches) else {
            self.schedule_folder_search(tx);
            self.folder_search.message = Some("That match is no longer in the file.".into());
            return;
        };

        self.find.open = true;
        self.find.show_replace = false;
        self.find.case_sensitive = false;
        self.find.whole_word = false;
        self.find.query.replace_text(query);
        self.find.set_matches(&doc.edit.text, matches, Some(index));
        self.find.notice = (!same_line).then(|| "File changed. Showing the closest match.".into());
        self.view_mode = ViewMode::Write;
        self.reveal_find_position(tx);
        self.close_folder_search();
        self.focus_document(tx);
    }

    pub(crate) fn set_reference(&mut self, source: crate::ui::reference::ReferenceSource) {
        use crate::ui::reference::ReferenceSource;
        let (title, content) = match &source {
            ReferenceSource::SessionOriginal => ("Original (before.md)".to_owned(), self.active_doc_ref().and_then(|d| d.original.clone()).unwrap_or_default()),
            ReferenceSource::File(path) => match fs::read_to_string(path) {
                Ok(content) => (path.file_name().unwrap_or_default().to_string_lossy().into_owned(), content),
                Err(error) => { self.status = format!("Cannot read reference: {error}"); return; }
            },
        };
        self.reference.source = Some(source);
        self.reference.cached_title = title;
        self.reference.cached_content = content;
        self.reference.open = true;
    }

    pub(crate) fn toggle_reference(&mut self) {
        if self.reference.open { self.reference.open = false; return; }
        if self.reference.source.is_none() && let Some(doc) = self.active_doc_ref() {
            let style = self.voice_project.as_ref().or(self.workspace.as_ref()).map(|p| p.join("STYLE.md"));
            let source = crate::ui::reference::default_source(doc.is_session(), &doc.path, style.as_deref());
            if let Some(source) = source { self.set_reference(source); }
        }
        self.reference.open = true;
    }

    pub(crate) fn pin_selected_reference(&mut self) {
        let path = self.selected_tree_id.as_ref().and_then(|id| self.file_tree.get(id))
            .filter(|node| !node.dir).and_then(|node| node.data.as_ref()).map(|entry| entry.abs_path.clone());
        if let Some(path) = path { self.set_reference(crate::ui::reference::ReferenceSource::File(path)); }
    }

    pub(crate) fn on_tree_select(&mut self, id: &str, _tx: &mut Tx) {
        self.selected_tree_id = Some(id.to_owned());
        let Some(node) = self.file_tree.get(id) else {
            return;
        };
        if node.dir {
            return;
        }
        if let Some(entry) = &node.data {
            self.open_workspace_file(entry.abs_path.clone(), false);
        }
    }

    pub(crate) fn on_tree_open(&mut self, id: &str, _tx: &mut Tx) {
        self.selected_tree_id = Some(id.to_owned());
        let Some(node) = self.file_tree.get(id) else {
            return;
        };
        if node.dir {
            return;
        }
        if let Some(entry) = &node.data {
            self.open_workspace_file(entry.abs_path.clone(), true);
        }
    }

    fn flush_leaving_doc(&mut self) {
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
        if let Some(pos) = self.documents.iter().position(|d| d.id.tab_key() == tab_key) {
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
        if !doc.is_session()
            && !self.store.ui.autosave_workspace_files
            && doc.is_dirty()
        {
            self.active_dialog = Some(DialogKind::ConfirmClose {
                doc_id: doc.id.clone(),
                title: doc.title.clone(),
            });
            self.dialog_error = None;
            return false;
        }
        let doc_id = doc.id.clone();
        if doc.is_dirty() {
            if doc.read_only || !self.drain_single_doc_save(&doc_id) {
                return false;
            }
        } else if self.worker_busy.as_ref().is_some_and(|(id, _)| id == &doc_id) {
            self.drain_until_idle();
        }
        if let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) {
            self.remove_doc_at(pos);
        }
        self.sync_window_title(tx);
        true
    }

    fn remove_doc_at(&mut self, pos: usize) {
        let previous = self.active_doc.and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
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
        let current = self.active_doc.and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
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

    pub(crate) fn confirm_close_save(&mut self) {
        let Some(DialogKind::ConfirmClose { doc_id, .. }) = self.active_dialog.clone() else {
            return;
        };
        self.close_dialog();
        if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
            doc.pending_save = true;
            doc.autosave_due = None;
            doc.recovery_due = None;
            doc.pending_recovery = false;
        }
        self.pending_close = Some(doc_id);
        self.dispatch_next();
    }

    pub(crate) fn confirm_close_discard(&mut self, tx: &mut Tx) {
        let Some(DialogKind::ConfirmClose { doc_id, .. }) = self.active_dialog.clone() else {
            return;
        };
        if let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == doc_id) {
            doc.pending_save = false;
            doc.pending_recovery = false;
            doc.autosave_due = None;
            doc.recovery_due = None;
        }
        if self.pending_close.as_ref() == Some(&doc_id) {
            self.pending_close = None;
        }
        if self.worker_busy.as_ref().is_some_and(|(id, _)| id == &doc_id) {
            // Finish any recovery write before removing its copy.
            self.drain_until_idle();
        }
        if let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) {
            if let Err(error) = clear_recovery_copy(&self.documents[pos].recovery_path()) {
                self.dialog_error = Some(error);
                return;
            }
            self.remove_doc_at(pos);
            self.sync_window_title(tx);
        }
        self.close_dialog();
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

    fn rel_from_workspace(&self, abs: &Path) -> Option<String> {
        let root = self.workspace.as_deref()?;
        rel_path_from_abs(root, abs)
    }

    fn save_open_tabs_pref(&mut self) {
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

    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_pane.toggle();
        self.save_ui_prefs_if_workspace();
    }

    pub(crate) fn toggle_inspector(&mut self) {
        self.inspector_pane.toggle();
        self.save_ui_prefs_if_workspace();
    }

    pub(crate) fn toggle_show_all_files(&mut self) {
        self.store.ui.show_all_files = !self.store.ui.show_all_files;
        self.refresh_workspace_tree();
        self.save_ui_prefs_if_workspace();
    }

    pub(crate) fn toggle_autosave(&mut self) {
        self.store.ui.autosave_workspace_files = !self.store.ui.autosave_workspace_files;
        let _ = prefs::save_store(&self.store);
        if self.store.ui.autosave_workspace_files {
            self.flush_autosave_buffers();
        }
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

    fn active_target_parent_rel(&self) -> String {
        if let Some(sel) = &self.selected_tree_id
            && let Some(node) = self.file_tree.get(sel)
        {
            if node.dir {
                return node.path.clone();
            }
            if let Some((parent, _)) = node.path.rsplit_once('/') {
                return parent.to_owned();
            }
        }
        String::new()
    }

    pub(crate) fn can_mutate_selected(&self) -> (bool, bool) {
        let Some(sel) = &self.selected_tree_id else {
            return (false, false);
        };
        let Some(node) = self.file_tree.get(sel) else {
            return (false, false);
        };
        let protected = node.data.as_ref().is_some_and(|e| {
            e.read_only || e.abs_path.file_name().is_some_and(|name| name == "manifest.json")
        });
        let is_file = !node.dir;
        (!protected, is_file)
    }

    pub(crate) fn open_new_file_dialog(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.file_menu_open = false;
        self.dialog_error = None;
        self.dialog_input = TextEdit::new("");
        self.dialog_input.focused = true;
        self.active_dialog = Some(DialogKind::NewFile {
            parent_rel: self.active_target_parent_rel(),
        });
    }

    pub(crate) fn open_new_folder_dialog(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.file_menu_open = false;
        self.dialog_error = None;
        self.dialog_input = TextEdit::new("");
        self.dialog_input.focused = true;
        self.active_dialog = Some(DialogKind::NewFolder {
            parent_rel: self.active_target_parent_rel(),
        });
    }

    pub(crate) fn open_rename_dialog(&mut self) {
        let Some(sel) = self.selected_tree_id.clone() else {
            return;
        };
        let Some(node) = self.file_tree.get(&sel) else {
            return;
        };
        self.file_menu_open = false;
        self.dialog_error = None;
        self.dialog_input = TextEdit::new(node.name.clone());
        self.dialog_input.focused = true;
        self.active_dialog = Some(DialogKind::Rename { rel_path: sel });
    }

    pub(crate) fn open_delete_dialog(&mut self) {
        let Some(sel) = self.selected_tree_id.clone() else {
            return;
        };
        let Some(node) = self.file_tree.get(&sel) else {
            return;
        };
        self.file_menu_open = false;
        self.dialog_error = None;
        self.active_dialog = Some(DialogKind::Delete {
            display_name: node.name.clone(),
            is_dir: node.dir,
            rel_path: sel,
        });
    }

    pub(crate) fn close_dialog(&mut self) {
        self.active_dialog = None;
        self.dialog_error = None;
        self.pending_document_focus = true;
    }

    pub(crate) fn duplicate_selected(&mut self) {
        self.file_menu_open = false;
        let (Some(root), Some(sel)) = (self.workspace.clone(), self.selected_tree_id.clone())
        else {
            return;
        };
        match workspace_tree::duplicate_file(&root, &sel) {
            Ok(created) => {
                if let Some(rel) = rel_path_from_abs(&root, &created) {
                    self.selected_tree_id = Some(rel);
                }
                self.refresh_workspace_tree();
                self.open_workspace_file(created, true);
            }
            Err(err) => self.status = err,
        }
    }

    fn has_pending_writes_under(&self, target: &Path) -> bool {
        self.documents.iter().any(|doc| {
            let writing = self.worker_busy.as_ref().is_some_and(|(id, _)| id == &doc.id);
            let file = !doc.is_session() && doc.path.starts_with(target);
            let mirror = doc.mirror_path.as_ref().is_some_and(|path| path.starts_with(target));
            (file || mirror) && (writing || doc.is_dirty())
        })
    }

    pub(crate) fn submit_dialog(&mut self, tx: &mut Tx) {
        let Some(dialog) = self.active_dialog.clone() else {
            return;
        };
        if matches!(dialog, DialogKind::ConfirmClose { .. }) {
            self.confirm_close_save();
            return;
        }
        if matches!(dialog, DialogKind::StartSession) {
            self.confirm_track_session();
            self.focus_document(tx);
            return;
        }
        if matches!(dialog, DialogKind::ConfirmRevert) {
            self.close_dialog();
            self.revert_to_original(tx);
            return;
        }
        let Some(root) = self.workspace.clone() else {
            return;
        };
        let input = self.dialog_input.text.trim().to_owned();
        match dialog {
            DialogKind::NewFile { parent_rel } => {
                match workspace_tree::create_file(&root, &parent_rel, &input) {
                    Ok(created) => {
                        if let Some(rel) = rel_path_from_abs(&root, &created) {
                            self.selected_tree_id = Some(rel);
                        }
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.open_workspace_file(created, true);
                        self.sync_window_title(tx);
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::NewFolder { parent_rel } => {
                match workspace_tree::create_folder(&root, &parent_rel, &input) {
                    Ok(created) => {
                        if let Some(rel) = rel_path_from_abs(&root, &created) {
                            self.selected_tree_id = Some(rel);
                        }
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        if let Some(sel) = &self.selected_tree_id {
                            self.file_tree.set_expanded(sel, true);
                        }
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::Rename { rel_path } => {
                let old_abs = root.join(&rel_path);
                let old_canon = old_abs.canonicalize().unwrap_or(old_abs);
                if self.has_pending_writes_under(&old_canon) {
                    self.dialog_error = Some("Wait for saves to finish or close unsaved files before renaming them.".into());
                    return;
                }
                match workspace_tree::rename_entry(&root, &rel_path, &input) {
                    Ok(renamed) => {
                        let new_canon = renamed.canonicalize().unwrap_or(renamed);
                        for doc in &mut self.documents {
                            if !doc.is_session() {
                                if doc.path == old_canon {
                                    doc.path = new_canon.clone();
                                    doc.id = DocId::File(new_canon.clone());
                                    if let Some(name) =
                                        new_canon.file_name().and_then(|n| n.to_str())
                                    {
                                        doc.title = name.to_owned();
                                    }
                                } else if let Ok(suffix) = doc.path.strip_prefix(&old_canon) {
                                    let updated = new_canon.join(suffix);
                                    doc.path = updated.clone();
                                    doc.id = DocId::File(updated);
                                }
                            }
                        }
                        if let Some(rel) = rel_path_from_abs(&root, &new_canon) {
                            self.selected_tree_id = Some(rel);
                        }
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                        self.sync_window_title(tx);
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::Delete { rel_path, .. } => {
                let target_abs = root.join(&rel_path);
                let target_canon = target_abs.canonicalize().unwrap_or(target_abs);
                if self.has_pending_writes_under(&target_canon) {
                    self.dialog_error = Some("Save or close unsaved files before deleting them.".into());
                    return;
                }
                match workspace_tree::delete_entry(&root, &rel_path) {
                    Ok(()) => {
                        let active_id = self.active_doc_ref().map(|doc| doc.id.clone());
                        self.documents
                            .retain(|d| d.is_session() || !d.path.starts_with(&target_canon));
                        for doc in &mut self.documents {
                            if doc.session_id.is_some()
                                && doc.mirror_path.as_ref().is_some_and(|path| path.starts_with(&target_canon))
                            {
                                doc.mirror_path = None;
                            }
                        }
                        self.active_doc = active_id.as_ref()
                            .and_then(|id| self.documents.iter().position(|doc| &doc.id == id))
                            .or_else(|| self.active_doc.zip(self.documents.len().checked_sub(1))
                                .map(|(index, last)| index.min(last)));
                        let current = self.active_doc.and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
                        if current != active_id {
                            self.conflict_compare = false;
                            self.judge_due = None;
                            self.judge_armed = self.active_doc.is_some();
                        }
                        self.selected_tree_id = None;
                        self.refresh_find();
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                        self.sync_window_title(tx);
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::ConfirmClose { .. }
            | DialogKind::Recover { .. }
            | DialogKind::StartSession
            | DialogKind::ConfirmRevert => unreachable!(),
        }
    }

    pub(crate) fn changed_active(&mut self, tx: &mut Tx) {
        let Some(idx) = self.active_doc else {
            return;
        };
        let autosave_files = self.store.ui.autosave_workspace_files;
        let busy_same_save = {
            let doc = &self.documents[idx];
            if !doc.is_editable() {
                return;
            }
            self.worker_busy.as_ref().is_some_and(|(id, kind)| {
                id == &doc.id && *kind == InFlight::Save
            })
        };

        let doc = &mut self.documents[idx];
        doc.pinned = true;
        doc.generation += 1;
        doc.save_error = None;
        doc.last_saved_label = Some("Unsaved".into());
        let delay = if doc.is_session() {
            SESSION_AUTOSAVE_DELAY
        } else {
            FILE_AUTOSAVE_DELAY
        };

        if doc.is_session() || autosave_files {
            if busy_same_save {
                doc.pending_save = true;
                doc.autosave_due = None;
            } else {
                doc.autosave_due = Some(tx.clock() + delay.as_secs_f64());
                tx.wake_after(delay);
            }
        } else {
            doc.recovery_due = Some(tx.clock() + FILE_AUTOSAVE_DELAY.as_secs_f64());
            tx.wake_after(FILE_AUTOSAVE_DELAY);
        }
        self.status = "Unsaved changes".into();
        if self.find.open {
            self.find.document_changed(&doc.edit.text, doc.edit.range().0);
        }
        let section_now = section_for_document(doc);
        if doc.cached_judge.as_ref().is_some_and(|cached| cached.section != section_now) {
            doc.cached_judge = None;
        }
        self.request_judge(tx.clock());
        tx.wake_after(JUDGE_DELAY);
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

    pub(crate) fn open_find(&mut self, replace: bool, tx: &mut Tx) {
        let Some(index) = self.active_doc else { return };
        if self.view_mode == ViewMode::Compare
            || (replace && self.view_mode == ViewMode::Preview && !self.active_is_locked())
        {
            self.set_view_mode(ViewMode::Write, tx);
        }
        if !self.find.open {
            let doc = &self.documents[index];
            let selected = doc.edit.selected();
            if !selected.is_empty() && !selected.contains(['\n', '\r']) {
                self.find.query.replace_text(selected);
            }
            self.find.open = true;
            self.refresh_find();
            self.reveal_find_position(tx);
        }
        self.find.show_replace = replace && self.can_replace_find();
        tx.focus("find-query", false);
    }

    pub(crate) fn close_find(&mut self, tx: &mut Tx) {
        self.find.close();
        self.focus_document(tx);
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

    fn refresh_find(&mut self) {
        if !self.find.open {
            return;
        }
        let Some(index) = self.active_doc else {
            self.find.close();
            return;
        };
        let doc = &self.documents[index];
        self.find.refresh(&doc.edit.text, doc.edit.range().0);
    }

    pub(crate) fn update_find_query(&mut self, tx: &mut Tx) {
        let Some(index) = self.active_doc else { return };
        let doc = &self.documents[index];
        let anchor = self.find.navigation_anchor().unwrap_or_else(|| doc.edit.range().0);
        self.find.refresh(&doc.edit.text, anchor);
        self.reveal_find_position(tx);
    }

    fn reveal_find_position(&mut self, tx: &mut Tx) {
        if let Some(range) = self.find.current_range().cloned()
            && let Some(doc) = self.active_doc_mut()
        {
            doc.edit.set_selection(range.start, range.end);
        }

        if self.active_uses_rendered_preview() {
            if let Some(doc) = self.active_doc_ref() {
                crate::ui::reveal_find_position(&doc.edit.text, &self.find, tx);
            }
        } else if self.find.current_range().is_some() {
            tx.scroll_into_view_in("working-body", crate::search::SOURCE_MATCH_KEY);
        } else if self.find.navigation_anchor().is_some() {
            tx.scroll_into_view_in("working-body", crate::search::SOURCE_ANCHOR_KEY);
        }
    }

    pub(crate) fn step_find(&mut self, forward: bool, tx: &mut Tx) {
        if self.find.matches.is_empty() {
            return;
        }
        self.find.current = match self.find.navigation_anchor() {
            Some(anchor) => crate::search::next_match(&self.find.matches, anchor, forward),
            None if forward => Some(0),
            None => Some(self.find.matches.len() - 1),
        };
        self.find.notice = None;
        self.reveal_find_position(tx);
    }

    pub(crate) fn replace_find(&mut self, all: bool, tx: &mut Tx) {
        if !self.find.open || !self.can_replace_find() {
            return;
        }
        let Some(index) = self.active_doc else { return };
        let edit = &mut self.documents[index].edit;
        let replacement = &self.find.replacement.text;
        if all {
            if crate::search::replace_all(edit, &self.find.matches, replacement) > 0 {
                let caret = edit.focus;
                self.changed_active(tx);
                self.find.select_after_replace_all(caret);
                self.reveal_find_position(tx);
            }
            return;
        }
        let Some(range) = self.find.current_range().cloned() else { return };
        let start = range.start;
        if crate::search::replace_one(edit, range, replacement) {
            let inserted = start..edit.range().1;
            self.changed_active(tx);
            self.find.select_after_replacement(inserted);
            self.reveal_find_position(tx);
        }
    }

    pub(crate) fn is_conflict_compare(&self) -> bool {
        self.conflict_compare && self.active_doc_ref().is_some_and(|doc| doc.conflict.is_some())
    }

    /// Left side of Compare. Conflict compare shows the workspace file; every other compare shows the session baseline.
    pub(crate) fn compare_left_text(&self) -> Option<&str> {
        let doc = self.active_doc_ref()?;
        if self.is_conflict_compare() {
            doc.conflict.as_ref().map(|conflict| conflict.disk_text.as_str())
        } else {
            doc.original.as_deref()
        }
    }

    pub(crate) fn refresh_diff(&mut self) {
        let original = self.compare_left_text().map(str::to_owned);
        let Some(doc) = self.active_doc_ref() else {
            self.diff = DiffState::new(Vec::new());
            return;
        };
        let Some(original) = original else {
            self.diff = DiffState::new(Vec::new());
            return;
        };
        let title = doc.title.clone();
        let current = doc.edit.text.clone();
        let mut diff = DiffState::new(vec![FileDiff::from_texts(&title, &original, &current)]);
        diff.set_mode(if self.compare_unified { DiffMode::Unified } else { DiffMode::Split });
        self.diff = diff;
    }

    pub(crate) fn compare_unified(&self) -> bool { self.compare_unified }

    pub(crate) fn set_compare_unified(&mut self, unified: bool) {
        self.compare_unified = unified;
        self.refresh_diff();
    }

    pub(crate) fn can_revert(&self) -> bool {
        self.view_mode == ViewMode::Compare
            && !self.is_conflict_compare()
            && self.active_doc_ref().is_some_and(|doc| doc.is_editable() && doc.original.is_some())
    }

    pub(crate) fn confirm_revert(&mut self) {
        if self.can_revert() {
            self.dialog_error = None;
            self.active_dialog = Some(DialogKind::ConfirmRevert);
        }
    }

    pub(crate) fn revert_to_original(&mut self, tx: &mut Tx) {
        if !self.can_revert() {
            self.focus_document(tx);
            return;
        }
        let Some(original) = self.active_doc_ref().and_then(|doc| doc.original.clone()) else { return };
        let Some(doc) = self.active_doc_mut() else { return };
        doc.edit.undo_group(|edit| {
            edit.set_selection(0, edit.text.len());
            edit.insert(&original);
        });
        self.changed_active(tx);
        self.refresh_diff();
        self.focus_document(tx);
    }

    pub(crate) fn revert_hunk(&mut self, index: usize, tx: &mut Tx) {
        if !self.can_revert() {
            self.focus_document(tx);
            return;
        }
        let Some(original) = self.active_doc_ref().and_then(|doc| doc.original.clone()) else { return };
        let current = self.active_working_edit().text.clone();
        let Some(next) = crate::hunks::revert_hunk_text(&original, &current, index) else {
            self.focus_document(tx);
            return;
        };
        let Some(doc) = self.active_doc_mut() else { return };
        let end = doc.edit.text.len();
        doc.edit.undo_group(|edit| {
            edit.set_selection(0, end);
            edit.insert(&next);
        });
        self.changed_active(tx);
        self.refresh_diff();
        self.focus_document(tx);
    }

    pub(crate) fn active_judge_report(&self) -> Option<&crate::bridge::JudgeReport> {
        let voice = self.voice_project.as_ref()?;
        let doc = self.active_doc_ref()?;
        if !judge_target(doc) {
            return None;
        }
        let cached = doc.cached_judge.as_ref()?;
        if &cached.voice_project != voice
            || cached.judge_epoch != doc.judge_epoch
            || cached.section != section_for_document(doc)
        {
            return None;
        }
        Some(&cached.report)
    }

    pub(crate) fn voice_pill_label(&self) -> Option<String> {
        let project = self.voice_project.as_ref()?;
        let name = project.file_name().and_then(|name| name.to_str()).unwrap_or("voice");
        let Some(doc) = self.active_doc_ref() else {
            return Some(format!("Voice: {name}"));
        };
        if !judge_target(doc) {
            return Some(format!("Voice: {name}"));
        }
        let Some(report) = self.active_judge_report() else {
            return Some(format!("Voice: {name}"));
        };
        if report.needs_build {
            return Some("Build voice".into());
        }
        if report.error.is_some() && report.checks.is_empty() {
            let name = project.file_name().and_then(|name| name.to_str()).unwrap_or("voice");
            return Some(format!("Voice: {name}"));
        }
        let gaps = report.gap_count();
        Some(match gaps {
            0 => format!("Voice: {}", report.score),
            1 => format!("Voice: {} · 1 gap", report.score),
            count => format!("Voice: {} · {count} gaps", report.score),
        })
    }

    pub(crate) fn set_judge_section(&mut self, section: String, clock: f64) {
        let Some(index) = self.active_doc else { return };
        if section_for_document(&self.documents[index]) == section {
            return;
        }
        let doc_id = self.documents[index].id.clone();
        self.documents[index].section_override = Some(section);
        self.documents[index].judge_epoch += 1;
        self.documents[index].latest_judge_request = None;
        self.documents[index].cached_judge = None;
        if self.judge_inflight.as_ref().is_some_and(|inflight| inflight.doc_id == doc_id) {
            self.judge_inflight = None;
        }
        self.request_judge(clock);
    }

    pub(crate) fn choose_judge_section(&mut self, section: String, tx: &mut Tx) {
        self.set_judge_section(section, tx.clock());
        tx.wake_after(JUDGE_DELAY);
    }

    pub(crate) fn toggle_voice_lens(&mut self) {
        self.store.ui.voice_lens = !self.store.ui.voice_lens;
        let _ = prefs::save_store(&self.store);
    }

    pub(crate) fn jump_next_placeholder(&mut self, tx: &mut Tx) {
        let Some(doc) = self.active_doc_ref() else {
            self.status = "No placeholders".into();
            return;
        };
        let caret = doc.edit.range().1;
        let ranges = crate::markdown_decor::find_placeholder_ranges(&doc.edit.text, self.placeholder_token.as_deref());
        if ranges.is_empty() {
            self.status = "No placeholders".into();
            self.focus_document(tx);
            return;
        }
        let index = ranges.iter().position(|range| range.start > caret).unwrap_or(0);
        let range = ranges[index].clone();
        let total = ranges.len();
        if self.jump_to_range(range.start, range.end, tx) {
            self.status = format!("Placeholder {} of {total}", index + 1);
        }
    }

    /// Selects a source range without changing the text. Preview and Compare
    /// return to Write so the selection is on the draft, except a voice guide,
    /// which stays on its rendered page.
    pub(crate) fn jump_to_range(&mut self, start: usize, end: usize, tx: &mut Tx) -> bool {
        if self.active_doc_ref().is_none() {
            return false;
        }
        if self.find.open {
            self.find.close();
        }
        if self.view_mode != ViewMode::Write {
            self.set_view_mode(ViewMode::Write, tx);
        }
        let Some(doc) = self.active_doc_mut() else { return false };
        let len = doc.edit.text.len();
        let start = start.min(len);
        let end = end.min(len);
        if start > end || !doc.edit.text.is_char_boundary(start) || !doc.edit.text.is_char_boundary(end) {
            return false;
        }
        let before = doc.edit.text.clone();
        doc.edit.set_selection(start, end);
        if doc.edit.text != before {
            doc.edit.replace_text(&before);
            return false;
        }
        self.focus_document(tx);
        true
    }

    pub(crate) fn jump_to_query(&mut self, query: &str, tx: &mut Tx) {
        let query = query.trim();
        if query.is_empty() || self.active_doc.is_none() {
            self.status = "Nothing to select".into();
            return;
        }
        let before = self.active_working_edit().text.clone();
        if self.view_mode != ViewMode::Write {
            self.set_view_mode(ViewMode::Write, tx);
        }
        self.find.query.replace_text(query);
        self.find.replacement.replace_text("");
        self.find.case_sensitive = false;
        self.find.whole_word = !query.chars().any(char::is_whitespace);
        self.find.open = true;
        self.find.show_replace = false;
        self.refresh_find();
        if self.find.matches.is_empty() {
            self.find.close();
            self.status = format!("No matches for {query}");
            self.focus_document(tx);
        } else if let Some(range) = self.find.current_range().cloned() {
            if let Some(doc) = self.active_doc_mut() {
                doc.edit.set_selection(range.start, range.end);
            }
            self.focus_document(tx);
        }
        if self.active_working_edit().text != before {
            self.active_working_edit_mut().replace_text(&before);
        }
    }

    pub(crate) fn jump_to_excerpt(&mut self, excerpt: &str, tx: &mut Tx) {
        let excerpt = excerpt.trim();
        let Some(text) = self.active_doc_ref().map(|doc| doc.edit.text.clone()) else {
            return;
        };
        if let Some(start) = text.find(excerpt) {
            self.jump_to_range(start, start + excerpt.len(), tx);
        } else if !excerpt.is_empty() {
            self.jump_to_query(excerpt, tx);
        }
    }

    pub(crate) fn show_live_check(&mut self, query: &str, detail: &str, tx: &mut Tx) {
        let before = self.active_working_edit().text.clone();
        self.jump_to_query(query, tx);
        if self.find.matches.is_empty() {
            self.status = if detail.is_empty() { query.to_owned() } else { detail.to_owned() };
        }
        if self.active_working_edit().text != before {
            self.active_working_edit_mut().replace_text(&before);
        }
    }

    pub(crate) fn request_voice_build(&mut self) {
        let Some(project) = self.voice_project.clone() else {
            self.status = "Open a voice project before building".into();
            return;
        };
        let doc_id = self.active_doc_ref().map(|doc| doc.id.clone());
        let generation = self.active_doc_ref().map(|doc| doc.generation).unwrap_or(0);
        if let Err(error) = self.bridge.request(doc_id, generation, crate::bridge::BridgeOp::Build { project }) {
            self.status = error;
            return;
        }
        self.status = "Building voice guide…".into();
    }

    pub(crate) fn save_durable_rule(&mut self, kind: &str) {
        let text = self.rule_draft.text.trim().to_owned();
        if text.is_empty() {
            self.status = "Write a rule before saving it".into();
            return;
        }
        let Some(project) = self.voice_project.clone() else {
            self.status = "Open a voice project before saving a rule".into();
            return;
        };
        let doc_id = self.active_doc_ref().map(|doc| doc.id.clone());
        let generation = self.active_doc_ref().map(|doc| doc.generation).unwrap_or(0);
        if let Err(error) = self.bridge.request(
            doc_id,
            generation,
            crate::bridge::BridgeOp::SaveNote { project, kind: kind.to_owned(), text },
        ) {
            self.status = error;
            return;
        }
        self.rule_draft = TextEdit::new("");
        self.status = "Saving note…".into();
    }

    pub(crate) fn open_voice_note(&mut self, name: &str) {
        if name != "STYLE.md" && name != "AUDIENCE.md" {
            return;
        }
        let Some(voice) = self.voice_project.clone() else {
            self.status = "Open a voice project first".into();
            return;
        };
        if !voice.is_dir() {
            self.status = "Voice project is not available".into();
            return;
        }
        let path = voice.join(name);
        if !fs_io::is_contained_in(&voice, &path) || workspace_tree::is_protected_voice_path(&path) {
            self.status = format!("Protected voice data is read-only: {}", path.display());
            return;
        }
        if !path.is_file() {
            let stub = format!("# {name}\n\n");
            if let Err(error) = fs_io::atomic_write_text(&path, &stub) {
                self.status = error;
                return;
            }
        }
        self.open_workspace_file(path, true);
    }

    pub(crate) fn can_track_session(&self) -> bool {
        let Some(doc) = self.active_doc_ref() else { return false };
        self.voice_project.is_some()
            && matches!(doc.id, DocId::File(_))
            && doc.session_id.is_none()
            && doc.is_editable()
            && !doc.edit.text.trim().is_empty()
            && !workspace_tree::is_protected_voice_path(&doc.path)
            && self.workspace.as_ref().is_none_or(|root| fs_io::is_contained_in(root, &doc.path))
    }

    pub(crate) fn open_track_session(&mut self) {
        if !self.can_track_session() {
            self.status = "This file cannot start an edit session".into();
            return;
        }
        let section = self.active_doc_ref().map(section_for_document).unwrap_or_else(|| "all".into());
        let section = if section == "all" { String::new() } else { section };
        self.session_origin = "ai-first".into();
        self.session_section = TextEdit::new(section);
        self.session_sources = TextEdit::new("");
        self.dialog_error = None;
        self.active_dialog = Some(DialogKind::StartSession);
    }

    pub(crate) fn confirm_track_session(&mut self) {
        if !self.can_track_session() {
            self.dialog_error = Some("This file cannot start an edit session".into());
            return;
        }
        let dirty = self.active_doc_ref().is_some_and(DocumentBuffer::is_dirty);
        let id = self.active_doc_ref().map(|doc| doc.id.clone());
        self.close_dialog();
        if dirty {
            self.pending_track = id;
            if let Some(doc) = self.active_doc_mut() {
                doc.pending_save = true;
            }
            self.status = "Saving before the session starts…".into();
            return;
        }
        self.request_session_start();
    }

    fn maybe_start_pending_track(&mut self) {
        let Some(id) = self.pending_track.clone() else { return };
        let Some(doc) = self.documents.iter().find(|doc| doc.id == id) else {
            self.pending_track = None;
            return;
        };
        if doc.save_error.is_some() {
            self.pending_track = None;
            self.status = doc.save_error.clone().unwrap_or_else(|| "Save failed".into());
            return;
        }
        if doc.is_dirty() || doc.pending_save || self.worker_busy.is_some() {
            return;
        }
        self.pending_track = None;
        self.request_session_start();
    }

    fn request_session_start(&mut self) {
        if !self.can_track_session() {
            self.status = "This file cannot start an edit session".into();
            return;
        }
        let Some(project) = self.voice_project.clone() else { return };
        let Some(doc) = self.active_doc_ref() else { return };
        let draft = doc.path.clone();
        if !draft.is_file() {
            self.status = "Save the draft to disk before tracking a session".into();
            return;
        }
        if !fs_io::is_contained_in(project.parent().unwrap_or(project.as_path()), &draft)
            && self.workspace.as_ref().is_some_and(|root| !fs_io::is_contained_in(root, &draft))
        {
            self.status = "That draft is outside the workspace".into();
            return;
        }
        let doc_id = doc.id.clone();
        let generation = doc.generation;
        if self.session_start_inflight.as_ref().is_some_and(|(id, _)| id == &doc_id) {
            return;
        }
        let section = self.session_section.text.trim();
        let section = if section.is_empty() || section == "all" { None } else { Some(section.to_owned()) };
        let origin = if self.session_origin == "writer-first" { "writer-first" } else { "ai-first" };
        let sources = self.session_sources.text.split([',', '\n']).map(str::trim).filter(|item| !item.is_empty()).map(str::to_owned).collect::<Vec<_>>();
        if let Err(error) = self.bridge.request(Some(doc_id.clone()), generation, crate::bridge::BridgeOp::SessionStart {
            project,
            draft,
            section,
            origin: origin.into(),
            sources,
        }) {
            self.status = error;
        } else {
            self.session_start_inflight = Some((doc_id, generation));
            self.status = "Starting edit session…".into();
        }
    }

    fn session_start_is_stale(&self, doc_id: &DocId, generation: u64) -> bool {
        self.documents.iter().find(|doc| &doc.id == doc_id).is_some_and(|doc| {
            doc.generation != generation || doc.is_dirty() || doc.session_id.is_some() || !doc.is_editable()
        })
    }

    fn attach_started_session(&mut self, doc_id: Option<DocId>, generation: u64, started: crate::bridge::SessionStartResult) {
        let Some(doc_id) = doc_id else {
            self.status = "Edit session started without a document".into();
            return;
        };
        let Some(project) = self.voice_project.clone() else { return };
        let folder = project.join("sessions").join(&started.id);
        let before = match fs_io::read_utf8(&folder.join("before.md")) {
            Ok(text) => text,
            Err(error) => { self.status = error; return; },
        };
        if fs_io::sha256_text(&before) != started.before_sha256 {
            self.status = "Session baseline does not match before.md".into();
            return;
        }
        let notes = fs_io::read_utf8(&folder.join("notes.md")).unwrap_or_default();
        if self.session_start_is_stale(&doc_id, generation) {
            self.status = "The draft changed before the edit session could attach.".into();
            return;
        }
        let Some(doc) = self.documents.iter().find(|doc| doc.id == doc_id) else {
            self.status = "The draft closed before the session started".into();
            return;
        };
        if workspace_tree::is_protected_voice_path(&doc.path) || !doc.path.is_absolute() {
            self.status = format!("Protected voice data is read-only: {}", doc.path.display());
            return;
        }
        let mirror = doc.path.clone();
        let buffer = doc.edit.text.clone();
        let disk = match fs_io::read_utf8(&mirror) {
            Ok(text) => text,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if fs_io::sha256_text(&disk) != fs_io::sha256_text(&buffer) {
            self.status = "The draft changed on disk before the edit session could attach.".into();
            return;
        }
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == doc_id) else {
            self.status = "The draft closed before the session started".into();
            return;
        };
        doc.session_id = Some(started.id.clone());
        doc.original = Some(before);
        doc.before_sha256 = Some(started.before_sha256);
        doc.notes = Some(TextEdit::new(notes));
        doc.voice_project = Some(project.clone());
        doc.mirror_path = Some(mirror.clone());
        doc.conflict = None;
        let key = prefs::EditorStateStore::session_link_key(&project, &started.id);
        self.store.session_links.insert(key, prefs::SessionLink {
            workspace_file: mirror,
            last_synced_sha256: fs_io::sha256_text(&disk),
        });
        if let Err(error) = prefs::save_store(&self.store) {
            self.status = format!("Session started, but the link was not saved: {error}");
        } else {
            self.status = format!("Tracking edit session {}", started.id);
            self.toast = Some(format!("Edit session {} is tracking this file", started.id));
        }
        self.sessions = workspace_tree::scan_sessions(&project, &self.store.session_links);
        self.refresh_diff();
    }

    pub(crate) fn load_disk_conflict(&mut self, tx: &mut Tx) {
        if self.active_is_locked() {
            self.focus_document(tx);
            return;
        }
        let Some(disk) = self.active_doc_ref().and_then(|doc| doc.conflict.as_ref().map(|conflict| conflict.disk_text.clone())) else {
            return;
        };
        let hash = fs_io::sha256_text(&disk);
        self.remember_link_hash(&hash);
        let Some(doc) = self.active_doc_mut() else { return };
        doc.conflict = None;
        doc.edit.undo_group(|edit| {
            edit.set_selection(0, edit.text.len());
            edit.insert(&disk);
        });
        self.conflict_compare = false;
        self.changed_active(tx);
        self.refresh_diff();
        self.status = "Loaded the file from disk into the session".into();
        self.focus_document(tx);
    }

    pub(crate) fn keep_session_conflict(&mut self, tx: &mut Tx) {
        if self.active_is_locked() {
            self.focus_document(tx);
            return;
        }
        let ready = self.active_doc_mut().is_some_and(|doc| {
            if doc.conflict.is_none() {
                return false;
            }
            doc.conflict = None;
            doc.force_mirror = true;
            doc.pending_save = true;
            true
        });
        if !ready {
            return;
        }
        self.dispatch_next();
        self.conflict_compare = false;
        self.status = "Keeping the session copy".into();
        self.focus_document(tx);
    }

    pub(crate) fn compare_conflict(&mut self, tx: &mut Tx) {
        if self.active_doc_ref().and_then(|doc| doc.conflict.as_ref()).is_none() {
            return;
        }
        self.conflict_compare = true;
        self.set_view_mode(ViewMode::Compare, tx);
        self.focus_document(tx);
    }

    fn remember_link_hash(&mut self, hash: &str) {
        let Some(doc) = self.active_doc_ref() else { return };
        let Some(key) = doc.voice_project.as_ref().zip(doc.session_id.as_ref()).map(|(project, id)| {
            prefs::EditorStateStore::session_link_key(project, id)
        }) else { return };
        if let Some(link) = self.store.session_links.get_mut(&key) {
            link.last_synced_sha256 = hash.to_owned();
            let _ = prefs::save_store(&self.store);
        }
    }

    fn reconcile_active_linked_doc(&mut self) {
        let Some(index) = self.active_doc else { return };
        let doc_id = self.documents[index].id.clone();
        if self.documents[index].finished
            || self.documents[index].pending_save
            || self.documents[index].force_mirror
            || self.worker_busy.as_ref().is_some_and(|(id, kind)| {
                id == &doc_id && matches!(kind, InFlight::Save | InFlight::Finish)
            })
        {
            return;
        }
        if let Some(mirror) = self.documents[index].mirror_path.clone() {
            if !mirror.is_file() {
                self.documents[index].mirror_path = None;
                self.status = "The linked workspace file is gone. Editing the session copy.".into();
                return;
            }
            let disk = match fs_io::read_utf8(&mirror) {
                Ok(text) => text,
                Err(error) => {
                    self.status = error;
                    return;
                }
            };
            let disk_hash = fs_io::sha256_text(&disk);
            let buffer_hash = fs_io::sha256_text(&self.documents[index].edit.text);
            let sync = Self::mirror_sync_hash(&self.store, &self.documents[index]);
            let matches_sync = sync.as_deref() == Some(disk_hash.as_str());
            let matches_buffer = disk_hash == buffer_hash;
            if !matches_sync && !matches_buffer {
                let session_text = self.documents[index].edit.text.clone();
                self.documents[index].conflict = Some(crate::documents::SessionConflict { disk_text: disk, session_text });
                self.status = "This file changed on disk outside the edit session.".into();
                return;
            }
            if self.documents[index].conflict.is_some() {
                self.documents[index].conflict = None;
                self.conflict_compare = false;
            }
            return;
        }
        if self.documents[index].session_id.is_some() || self.documents[index].is_dirty() {
            return;
        }
        let path = self.documents[index].path.clone();
        let Some((project, id, after, before, notes, before_sha)) = self.linked_session_files(&path) else { return };
        let Ok(disk) = fs_io::read_utf8(&path) else { return };
        let buffer = self.documents[index].edit.text.clone();
        let same = fs_io::sha256_text(&disk) == fs_io::sha256_text(&buffer)
            && fs_io::sha256_text(&disk) == fs_io::sha256_text(&after);
        {
            let doc = &mut self.documents[index];
            doc.session_id = Some(id);
            doc.original = Some(before);
            doc.before_sha256 = Some(before_sha);
            doc.notes = Some(TextEdit::new(notes));
            doc.voice_project = Some(project);
            doc.mirror_path = Some(path);
            if same {
                doc.conflict = None;
            } else if buffer == disk {
                doc.edit = TextEdit::new(after.clone());
                doc.conflict = Some(crate::documents::SessionConflict { disk_text: disk, session_text: after });
            } else {
                doc.conflict = Some(crate::documents::SessionConflict { disk_text: disk, session_text: buffer });
            }
        }
        if !same {
            self.status = "This file changed on disk outside the edit session.".into();
        }
    }

    fn reconcile_linked_file(&mut self) {
        let Some(index) = self.active_doc else { return };
        let path = self.documents[index].path.clone();
        let Some((project, id, after, before, notes, before_sha)) = self.linked_session_files(&path) else { return };
        let disk = self.documents[index].edit.text.clone();
        let doc = &mut self.documents[index];
        doc.session_id = Some(id);
        doc.original = Some(before);
        doc.before_sha256 = Some(before_sha);
        doc.notes = Some(TextEdit::new(notes));
        doc.voice_project = Some(project);
        doc.mirror_path = Some(path);
        if fs_io::sha256_text(&disk) == fs_io::sha256_text(&after) {
            doc.conflict = None;
            return;
        }
        doc.edit = TextEdit::new(after.clone());
        doc.conflict = Some(crate::documents::SessionConflict { disk_text: disk, session_text: after });
        self.status = "This file changed on disk outside the edit session.".into();
    }

    fn linked_session_files(&self, path: &Path) -> Option<(PathBuf, String, String, String, String, String)> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        for (key, link) in &self.store.session_links {
            let linked = link.workspace_file.canonicalize().unwrap_or_else(|_| link.workspace_file.clone());
            if linked != canon {
                continue;
            }
            let (project, id) = key.rsplit_once("::")?;
            let project = PathBuf::from(project);
            let folder = project.join("sessions").join(id);
            if !folder.join("session.json").is_file() {
                continue;
            }
            let after = fs_io::read_utf8(&folder.join("after.md")).ok()?;
            let before = fs_io::read_utf8(&folder.join("before.md")).ok()?;
            let notes = fs_io::read_utf8(&folder.join("notes.md")).unwrap_or_default();
            let meta = fs_io::read_utf8(&folder.join("session.json")).ok()?;
            let before_sha = serde_json::from_str::<serde_json::Value>(&meta).ok()?.get("beforeSha256")?.as_str()?.to_owned();
            if fs_io::sha256_text(&before) != before_sha {
                continue;
            }
            return Some((project, id.to_owned(), after, before, notes, before_sha));
        }
        None
    }

    fn request_judge(&mut self, clock: f64) {
        let ready = self.voice_project.is_some()
            && self.active_doc_ref().is_some_and(judge_target);
        if !ready {
            self.judge_due = None;
            return;
        }
        self.judge_due = Some(clock + JUDGE_DELAY.as_secs_f64());
    }

    fn dispatch_judge(&mut self) {
        let Some(project) = self.voice_project.clone() else { return };
        let Some(index) = self.active_doc else { return };
        if !judge_target(&self.documents[index]) {
            return;
        }
        let doc_id = self.documents[index].id.clone();
        let generation = self.documents[index].generation;
        let judge_epoch = self.documents[index].judge_epoch;
        let section = section_for_document(&self.documents[index]);
        if self.judge_inflight.as_ref().is_some_and(|inflight| {
            inflight.doc_id == doc_id
                && inflight.generation == generation
                && inflight.judge_epoch == judge_epoch
                && inflight.section == section
        }) {
            return;
        }
        let text = self.documents[index].edit.text.clone();
        let section_param = (section != "all").then(|| section.clone());
        if let Ok(request_id) = self.bridge.request(
            Some(doc_id.clone()),
            generation,
            crate::bridge::BridgeOp::JudgeText { project, text, section: section_param },
        ) {
            self.documents[index].latest_judge_request = Some(request_id);
            self.judge_inflight = Some(JudgeInFlight {
                doc_id,
                generation,
                judge_epoch,
                section,
                request_id,
            });
        }
    }

    pub(crate) fn apply_bridge_reply(&mut self, reply: crate::bridge::BridgeReply) {
        use crate::bridge::BridgeReply;
        match reply {
            BridgeReply::Judge { request_id, doc_id, generation, section, result } => {
                let mut result = *result;
                if result.section.is_empty() {
                    result.section = section.clone();
                }
                if self.judge_inflight.as_ref().is_some_and(|inflight| inflight.request_id == request_id) {
                    self.judge_inflight = None;
                }
                let voice_project = self.voice_project.clone();
                let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == doc_id) else {
                    return;
                };
                if doc.generation != generation
                    || doc.latest_judge_request != Some(request_id)
                    || section_for_document(doc) != section
                {
                    return;
                }
                let Some(voice_project) = voice_project else { return };
                doc.cached_judge = Some(CachedJudge {
                    voice_project,
                    judge_epoch: doc.judge_epoch,
                    section,
                    report: result,
                });
            }
            BridgeReply::Build { result, .. } => match result {
                Ok(_) => {
                    for doc in &mut self.documents {
                        doc.judge_epoch += 1;
                        doc.latest_judge_request = None;
                        doc.cached_judge = None;
                    }
                    self.judge_inflight = None;
                    self.judge_armed = true;
                }
                Err(error) => self.status = format!("Build failed: {error}"),
            },
            BridgeReply::SaveNote { result, .. } => match result {
                Ok(saved) => {
                    self.status = format!("Saved note to {}", saved.path);
                    self.toast = Some(format!("Saved a note in {}", saved.path));
                }
                Err(error) => {
                    self.status = format!("Could not save note: {error}");
                    self.toast = Some(self.status.clone());
                }
            },
            BridgeReply::SessionStart { doc_id, generation, result } => {
                if let Some(id) = &doc_id
                    && self.session_start_inflight.as_ref().is_some_and(|(inflight, sent)| inflight == id && *sent == generation)
                {
                    self.session_start_inflight = None;
                }
                let stale = doc_id.as_ref().is_some_and(|id| self.session_start_is_stale(id, generation));
                match result {
                    Ok(_started) if stale => {
                        self.status = "The draft changed before the edit session could attach.".into();
                    }
                    Ok(started) => self.attach_started_session(doc_id, generation, started),
                    Err(_) if stale => {}
                    Err(error) => self.status = format!("Could not start session: {error}"),
                }
            }
            BridgeReply::SessionFinish { doc_id, result } => {
                if let Err(error) = result {
                    self.status = if doc_id.is_some() {
                        format!("Could not finish session: {error}")
                    } else {
                        error
                    };
                }
            }
            BridgeReply::RulesList { result }
            | BridgeReply::RulesDecide { result }
            | BridgeReply::ChecksList { result }
            | BridgeReply::ChecksDecide { result }
            | BridgeReply::Promote { result } => {
                if let Err(error) = result {
                    self.status = error;
                }
            }
            BridgeReply::SessionsList { result } => {
                if let Err(error) = result {
                    self.status = error;
                }
            }
        }
    }

    pub(crate) fn save_clicked(&mut self) {
        let Some(idx) = self.active_doc else {
            return;
        };
        let doc = &mut self.documents[idx];
        if !doc.is_editable() {
            return;
        }
        if !doc.is_dirty() && !doc.recovery_path().is_file() {
            if doc.conflict.is_some() {
                self.status = "Resolve the disk conflict (Load Disk or Keep Session) before syncing to disk.".into();
                return;
            }
            let label = DocumentBuffer::format_saved_utc();
            doc.last_saved_label = Some(label);
            self.status = "Saved locally".into();
            return;
        }
        doc.pending_save = true;
        doc.autosave_due = None;
        doc.recovery_due = None;
        doc.pending_recovery = false;
        self.dispatch_next();
    }

    pub(crate) fn finish(&mut self) {
        let Some(idx) = self.active_doc else {
            return;
        };
        if self.active_is_locked() {
            return;
        }
        let doc = &mut self.documents[idx];
        if !doc.is_session() {
            return;
        }
        doc.finish_requested = true;
        doc.pending_save = false;
        doc.autosave_due = None;
        self.status = "Finishing…".into();
        self.dispatch_next();
    }

    pub(crate) fn flush_autosave_buffers(&mut self) {
        let autosave_files = self.store.ui.autosave_workspace_files;
        for doc in &mut self.documents {
            if doc.can_save() && doc.is_dirty() {
                if doc.is_session() || autosave_files {
                    doc.pending_save = true;
                    doc.autosave_due = None;
                } else {
                    doc.pending_recovery = true;
                    doc.recovery_due = None;
                }
            }
        }
        self.dispatch_next();
    }

    fn mirror_sync_hash(store: &prefs::EditorStateStore, doc: &DocumentBuffer) -> Option<String> {
        doc.mirror_path.as_ref()?;
        let (project, id) = doc.voice_project.as_ref().zip(doc.session_id.as_ref())?;
        let key = prefs::EditorStateStore::session_link_key(project, id);
        store.session_links.get(&key).map(|link| link.last_synced_sha256.clone())
    }

    fn saved_snapshot(
        store: &mut prefs::EditorStateStore,
        doc: &mut DocumentBuffer,
        generation: u64,
        sha256: String,
        outcome: WriteOutcome,
    ) -> Option<String> {
        doc.saved_generation = doc.saved_generation.max(generation);
        doc.last_disk_sha256 = sha256.clone();
        let key = doc.voice_project.as_ref().zip(doc.session_id.as_ref())
            .map(|(project, id)| prefs::EditorStateStore::session_link_key(project, id));
        let link = key.as_ref().and_then(|key| store.session_links.get_mut(key));
        match outcome {
            WriteOutcome::Mirrored => {
                if let Some(link) = link {
                    link.last_synced_sha256 = sha256;
                    if let Err(error) = prefs::save_store(store) {
                        return Some(format!("Could not save link state: {error}"));
                    }
                }
                None
            }
            WriteOutcome::MirrorSkipped(error) => Some(error),
            WriteOutcome::Local if doc.mirror_path.is_none() => link.map(|link| {
                if link.last_synced_sha256.is_empty() {
                    "Linked draft has no known sync state".into()
                } else {
                    "Linked file unavailable without a workspace".into()
                }
            }),
            WriteOutcome::Local => None,
        }
    }

    pub(crate) fn dispatch_next(&mut self) {
        if self.worker_busy.is_some() {
            return;
        }

        // 1. Pending finishes
        if let Some(doc) = self
            .documents
            .iter_mut()
            .find(|d| d.finish_requested && !d.finished)
            && let (Some(project), Some(session_id)) =
                (doc.voice_project.clone(), doc.session_id.clone())
        {
            let doc_id = doc.id.clone();
            let snapshot = doc.is_dirty().then(|| {
                let notes = doc
                    .notes
                    .as_ref()
                    .map_or_else(String::new, |n| n.text.clone());
                (doc.edit.text.clone(), notes)
            });
            match self.worker.send(WorkerRequest::Finish {
                doc_id: doc_id.clone(),
                generation: doc.generation,
                project,
                session_id,
                before_sha256: doc.before_sha256.clone(),
                snapshot,
                mirror_path: doc.mirror_path.clone(),
                mirror_expected_sha256: Self::mirror_sync_hash(&self.store, doc),
            }) {
                Ok(()) => {
                    self.worker_busy = Some((doc_id, InFlight::Finish));
                    self.status = "Finishing…".into();
                }
                Err(error) => {
                    doc.finish_requested = false;
                    doc.save_error = Some(error.clone());
                    self.status = format!("Finish stopped: {error}");
                }
            }
            return;
        }

        // 2. Pending atomic saves
        if let Some(doc) = self
            .documents
            .iter_mut()
            .find(|d| d.pending_save && !d.finished)
        {
            doc.pending_save = false;
            doc.autosave_due = None;
            doc.recovery_due = None;
            doc.pending_recovery = false;
            let doc_id = doc.id.clone();
            let mirror_force = std::mem::take(&mut doc.force_mirror);
            let req = if let (Some(project), Some(session_id)) =
                (doc.voice_project.clone(), doc.session_id.clone())
            {
                WorkerRequest::WriteSession {
                    doc_id: doc_id.clone(),
                    generation: doc.generation,
                    project,
                    session_id,
                    before_sha256: doc.before_sha256.clone(),
                    after: doc.edit.text.clone(),
                    notes: doc
                        .notes
                        .as_ref()
                        .map_or_else(String::new, |n| n.text.clone()),
                    mirror_path: doc.mirror_path.clone(),
                    mirror_expected_sha256: Self::mirror_sync_hash(&self.store, doc),
                    mirror_force,
                }
            } else {
                WorkerRequest::WriteFile {
                    doc_id: doc_id.clone(),
                    generation: doc.generation,
                    path: doc.path.clone(),
                    content: doc.edit.text.clone(),
                    clear_recovery: Some(doc.recovery_path()),
                }
            };
            match self.worker.send(req) {
                Ok(()) => {
                    self.worker_busy = Some((doc_id, InFlight::Save));
                    doc.last_saved_label = Some("Saving…".into());
                    self.status = "Saving…".into();
                }
                Err(error) => {
                    doc.save_error = Some(error.clone());
                    self.status = format!("Save failed: {error}");
                }
            }
            return;
        }

        // 3. Pending crash-recovery shadow writes (manual-save mode)
        if let Some(doc) = self
            .documents
            .iter_mut()
            .find(|d| d.pending_recovery && !d.finished && !d.is_session())
        {
            doc.pending_recovery = false;
            doc.recovery_due = None;
            let doc_id = doc.id.clone();
            let req = WorkerRequest::WriteRecovery {
                doc_id: doc_id.clone(),
                recovery_path: doc.recovery_path(),
                content: doc.edit.text.clone(),
            };
            match self.worker.send(req) {
                Ok(()) => self.worker_busy = Some((doc_id, InFlight::Recovery)),
                Err(error) => {
                    doc.save_error = Some(error.clone());
                    self.status = format!("Recovery failed: {error}");
                }
            }
        }
    }

    pub(crate) fn apply_reply(&mut self, reply: WorkerReply, tx: Option<&mut Tx>) -> bool {
        self.worker_busy = None;
        let ok = match reply {
            WorkerReply::Saved {
                doc_id,
                generation,
                sha256,
                result,
            } => {
                let mut saved_ok = false;
                let mut should_close = false;
                if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                    match result {
                        Ok(_) if generation < doc.saved_generation => {
                            // An older save finished after a newer one. Drop it so it cannot rewind the sync hash.
                        }
                        Ok(outcome) => {
                            let warning = Self::saved_snapshot(&mut self.store, doc, generation, sha256, outcome);
                            doc.save_error = None;
                            if doc.is_dirty() {
                                if doc.is_session() || self.store.ui.autosave_workspace_files {
                                    doc.pending_save = true;
                                }
                                doc.last_saved_label = Some("Unsaved".into());
                                self.status = warning.unwrap_or_else(|| "Unsaved changes".into());
                            } else {
                                doc.last_saved_label = Some(DocumentBuffer::format_saved_utc());
                                self.status = warning.map_or_else(
                                    || "Saved locally".into(),
                                    |reason| format!("Saved locally. {reason}"),
                                );
                                if self.pending_close.as_ref() == Some(&doc_id) {
                                    should_close = true;
                                }
                            }
                            saved_ok = true;
                        }
                        Err(_) if generation < doc.saved_generation => {
                            // The failed write is older than the bytes already accepted.
                        }
                        Err(error) => {
                            doc.autosave_due = None;
                            doc.save_error = Some(error.clone());
                            if self.pending_close.as_ref() == Some(&doc_id) {
                                self.pending_close = None;
                            }
                            if doc.finish_requested {
                                doc.finish_requested = false;
                                self.status = format!("Finish stopped: {error}");
                            } else if doc.pending_save && doc.generation > generation {
                                // retry next newer edit
                            } else {
                                doc.pending_save = false;
                                self.status = format!("Save failed: {error}");
                            }
                        }
                    }
                }
                if should_close
                    && let Some(pos) = self.documents.iter().position(|d| d.id == doc_id)
                {
                    self.pending_close = None;
                    self.remove_doc_at(pos);
                    if let Some(tx) = tx {
                        self.sync_window_title(tx);
                    }
                }
                saved_ok
            }
            WorkerReply::RecoveryWritten { doc_id, result } => match result {
                Ok(_) => {
                    if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                        doc.save_error = None;
                    }
                    true
                },
                Err(error) => {
                    if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                        doc.save_error = Some(error.clone());
                    }
                    self.status = format!("Recovery failed: {error}");
                    false
                }
            },
            WorkerReply::Finished {
                doc_id,
                generation,
                snapshot,
                result,
            } => {
                let mut finished_ok = false;
                if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                    let warning = snapshot.and_then(|(sha, outcome)| {
                        Self::saved_snapshot(&mut self.store, doc, generation, sha, outcome)
                    });
                    doc.finish_requested = false;
                    match result {
                        Ok(()) => {
                            doc.finished = true;
                            doc.read_only = true;
                            doc.pending_save = false;
                            doc.autosave_due = None;
                            doc.save_error = None;
                            doc.last_saved_label = Some("Finished".into());
                            self.status = warning.map_or_else(
                                || "Finished. Final and receipt saved locally.".into(),
                                |reason| format!("Finished locally. {reason}"),
                            );
                            if let Some(voice) = &self.voice_project {
                                self.sessions = workspace_tree::scan_sessions(
                                    voice,
                                    &self.store.session_links,
                                );
                            }
                            finished_ok = true;
                        }
                        Err(error) => {
                            doc.save_error = Some(error.clone());
                            self.status = format!("Finish stopped: {error}");
                        }
                    }
                }
                finished_ok
            }
        };
        self.dispatch_next();
        ok
    }

    fn drain_until_idle(&mut self) {
        while self.worker_busy.is_some() {
            match self.worker.recv() {
                Ok(reply) => {
                    self.apply_reply(reply, None);
                }
                Err(_) => {
                    self.worker_busy = None;
                    break;
                }
            }
        }
    }

    fn drain_single_doc_save(&mut self, doc_id: &DocId) -> bool {
        if self.worker_busy.is_some() {
            self.drain_until_idle();
        }
        let mut attempted = false;
        loop {
            if let Some(doc) = self.documents.iter_mut().find(|d| &d.id == doc_id) {
                if !doc.is_dirty() || attempted {
                    return !doc.is_dirty();
                }
                attempted = true;
                doc.pending_save = true;
                doc.autosave_due = None;
            } else {
                return true;
            }
            self.dispatch_next();
            if self.worker_busy.is_none() {
                return false;
            }
            while self.worker_busy.is_some() {
                match self.worker.recv() {
                    Ok(reply) => {
                        let ok = self.apply_reply(reply, None);
                        if !ok && self.worker_busy.is_none() {
                            return false;
                        }
                    }
                    Err(_) => {
                        self.worker_busy = None;
                        return false;
                    }
                }
            }
        }
    }

    fn drain_autosave_work(&mut self) -> bool {
        let mut success = true;
        let autosave = self.store.ui.autosave_workspace_files;
        let mut attempted_finish: HashSet<DocId> = HashSet::new();
        let mut attempted_save: HashSet<DocId> = HashSet::new();
        let mut attempted_recovery: HashSet<DocId> = HashSet::new();

        loop {
            if self.worker_busy.is_some() {
                match self.worker.recv() {
                    Ok(reply) => {
                        let ok = self.apply_reply(reply, None);
                        if !ok {
                            success = false;
                            eprintln!("ghostless-editor: {}", self.status);
                        }
                        continue;
                    }
                    Err(_) => {
                        self.worker_busy = None;
                        self.status = "Workspace not changed: save worker stopped".into();
                        success = false;
                        break;
                    }
                }
            }

            if let Some(doc) = self
                .documents
                .iter_mut()
                .find(|d| d.finish_requested && !d.finished && !attempted_finish.contains(&d.id))
            {
                attempted_finish.insert(doc.id.clone());
                self.dispatch_next();
                if self.worker_busy.is_some() {
                    continue;
                }
            }

            if let Some(doc) = self.documents.iter_mut().find(|d| {
                d.can_save() && d.is_dirty()
                    && (d.is_session() || autosave)
                    && !attempted_save.contains(&d.id)
            }) {
                attempted_save.insert(doc.id.clone());
                doc.pending_save = true;
                doc.autosave_due = None;
                self.dispatch_next();
                if self.worker_busy.is_some() {
                    continue;
                }
            }

            if let Some(doc) = self.documents.iter_mut().find(|d| {
                d.can_save() && d.is_dirty()
                    && !d.is_session()
                    && !autosave
                    && !attempted_recovery.contains(&d.id)
            }) {
                attempted_recovery.insert(doc.id.clone());
                doc.pending_recovery = true;
                doc.recovery_due = None;
                self.dispatch_next();
                if self.worker_busy.is_some() {
                    continue;
                }
            }

            break;
        }
        if self.documents.iter().any(|doc| doc.save_error.is_some() || (doc.read_only && doc.is_dirty())) {
            success = false;
        }
        if !success {
            if let Some(error) = self.documents.iter().find_map(|doc| doc.save_error.as_ref()) {
                self.status = format!("Workspace not changed: {error}");
            } else if !self.status.contains("worker stopped") {
                self.status = "Workspace not changed: a save failed".into();
            }
        }
        success
    }

    fn show_close_error(&mut self) {
        let reason = self.status.strip_prefix("Workspace not changed: ").unwrap_or(&self.status);
        self.status = format!("Could not close: {reason}");
    }

    pub(crate) fn on_application_quit(&mut self) {
        if self.flushed {
            return;
        }
        self.quit_recovery_needed = !self.drain_autosave_work() && !self.write_quit_recovery();
        // The system can still cancel shutdown. Keep the worker available to the
        // live editor, or to the final flush if the editor is dropped.
        if self.workspace.is_some() {
            self.save_open_tabs_pref();
            self.save_ui_prefs_if_workspace();
        }
    }

    fn write_quit_recovery(&self) -> bool {
        let mut recovered_all = true;
        for doc in self.documents.iter().filter(|doc| doc.is_dirty()) {
            let path = doc.recovery_path();
            match doc.recovery_contents().and_then(|text| fs_io::atomic_write_text(&path, &text)) {
                Ok(()) => eprintln!("ghostless-editor: saved a recovery copy for {}", doc.title),
                Err(error) => {
                    recovered_all = false;
                    eprintln!("ghostless-editor: could not recover {}: {error}", doc.title);
                }
            }
        }
        recovered_all
    }

    pub(crate) fn flush_on_exit(&mut self) -> bool {
        if self.flushed {
            return true;
        }
        if !self.drain_autosave_work() {
            return false;
        }
        self.flushed = true;
        if self.workspace.is_some() {
            self.save_open_tabs_pref();
            self.save_ui_prefs_if_workspace();
        }
        self.worker.shutdown();
        self.bridge.shutdown();
        true
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

fn judge_target(doc: &DocumentBuffer) -> bool {
    let name = doc.path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    !matches!(name, "manifest.json" | "receipt.json")
}

pub(crate) fn section_for_document(doc: &DocumentBuffer) -> String {
    if let Some(section) = doc.section_override.as_deref() {
        return if section.is_empty() { "all".into() } else { section.to_owned() };
    }
    crate::markdown_decor::parse_frontmatter_fields(&doc.edit.text)
        .into_iter()
        .find(|(key, _)| key == "section")
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "all".into())
}

pub(crate) fn on_window_close(editor: &mut Editor, tx: &mut Tx) {
    if editor.flush_on_exit() {
        tx.quit();
    } else {
        editor.show_close_error();
        if let Some(reopen) = &editor.reopen_window { reopen(); }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if !self.flush_on_exit() {
            if self.quit_recovery_needed {
                self.write_quit_recovery();
            }
            self.worker.shutdown();
        }
        self.bridge.shutdown();
    }
}

fn resolve_mirror_path(path: &Path, workspace: Option<&Path>) -> PathBuf {
    if path.is_absolute() { return path.to_path_buf(); }
    match workspace {
        Some(root) if fs_io::is_contained_in(root, &root.join(path)) => root.join(path),
        // Leave unresolved paths relative so the worker rejects them instead of writing to cwd.
        _ => path.to_path_buf(),
    }
}

fn classify_file_flags(
    tree_entry: Option<&WorkspaceEntry>,
    path: &Path,
    voice_project: Option<&Path>,
) -> (bool, bool) {
    let name = path.file_name().and_then(|n| n.to_str());
    let read_only = name == Some("voice-guide.md")
        || tree_entry.is_some_and(|entry| entry.read_only)
        || voice_project
            .and_then(|voice| path.strip_prefix(voice).ok())
            .is_some_and(workspace_tree::is_protected_voice_relative)
        || workspace_tree::is_protected_voice_path(path);
    (read_only, !read_only && name == Some("pre-answers.md"))
}

fn rel_path_from_abs(root: &Path, abs: &Path) -> Option<String> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_abs = abs.canonicalize().unwrap_or_else(|_| abs.to_path_buf());
    let rel = canon_abs
        .strip_prefix(&canon_root)
        .or_else(|_| abs.strip_prefix(root))
        .ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

pub(crate) fn poll(editor: &mut Editor, clock: f64, tx: &mut Tx) -> bool {
    if !editor.fonts_initialized {
        let sans = if editor.store.ui.font_family == prefs::FontChoice::Serif { "Georgia, serif" } else { "Inter, sans-serif" };
        tx.set_font_families(sans, "IBM Plex Mono, monospace");
        editor.fonts_initialized = true;
    }
    let mut changed = false;
    while let Ok(reply) = editor.worker.try_recv() {
        editor.apply_reply(reply, Some(tx));
        changed = true;
    }

    while let Ok(reply) = editor.worker.search.try_recv() {
        changed |= editor.accept_folder_search(reply);
    }
    while let Ok(reply) = editor.bridge.try_recv() {
        editor.apply_bridge_reply(reply);
        changed = true;
    }
    if editor.judge_armed {
        editor.judge_armed = false;
        if editor.judge_due.is_none() {
            editor.request_judge(clock);
        }
    }
    if let Some(due) = editor.judge_due {
        if clock >= due {
            editor.judge_due = None;
            editor.dispatch_judge();
        } else {
            tx.wake_after(Duration::from_secs_f64((due - clock).max(0.0)));
        }
    }
    if let Some(due) = editor.folder_search.due {
        if clock >= due {
            editor.folder_search.due = None;
            editor.submit_folder_search();
            changed = true;
        } else {
            tx.wake_after(Duration::from_secs_f64(due - clock));
        }
    }

    let autosave = editor.store.ui.autosave_workspace_files;
    let mut next_wake: Option<f64> = None;
    for doc in &mut editor.documents {
        if !doc.can_save() || !doc.is_dirty() {
            continue;
        }
        if doc.is_session() || autosave {
            if let Some(due) = doc.autosave_due {
                if clock >= due {
                    doc.pending_save = true;
                    doc.autosave_due = None;
                    changed = true;
                } else {
                    next_wake = Some(next_wake.map_or(due, |m| m.min(due)));
                }
            }
        } else if let Some(due) = doc.recovery_due {
            if clock >= due {
                doc.pending_recovery = true;
                doc.recovery_due = None;
                changed = true;
            } else {
                next_wake = Some(next_wake.map_or(due, |m| m.min(due)));
            }
        }
    }

    editor.dispatch_next();
    if editor.worker_busy.is_none()
        && let Some(due) = next_wake
    {
        tx.wake_after(Duration::from_secs_f64((due - clock).max(0.0)));
    }

    if editor.workspace.is_some()
        && !editor.sidebar_pane.is_resizing()
        && !editor.inspector_pane.is_resizing()
    {
        let sb_open = editor.sidebar_pane.is_open();
        let sb_w = editor.sidebar_pane.width();
        let insp_open = editor.inspector_pane.is_open();
        let insp_w = editor.inspector_pane.width();
        if editor.store.ui.sidebar_open != sb_open
            || (editor.store.ui.sidebar_width - sb_w).abs() > 0.5
            || editor.store.ui.inspector_open != insp_open
            || (editor.store.ui.inspector_width - insp_w).abs() > 0.5
        {
            editor.save_ui_prefs_if_workspace();
        }
    }

    editor.maybe_start_pending_track();
    if editor.pending_document_focus {
        editor.pending_document_focus = false;
        editor.focus_document(tx);
        changed = true;
    }

    editor.sync_window_title(tx);
    changed
}

pub(crate) fn on_key(editor: &mut Editor, key: &KeyEvent, tx: &mut Tx) -> bool {
    if key.is_command('q') && !key.mods.shift && !key.mods.alt {
        if editor.flush_on_exit() {
            tx.quit();
        } else {
            editor.show_close_error();
        }
        return true;
    }
    if editor.active_dialog.is_some() {
        return false;
    }
    if key.is_command('o') {
        if key.mods.shift {
            crate::ui_welcome::open_file(tx);
        } else {
            crate::ui_welcome::open_folder(tx);
        }
        return true;
    }
    if editor.is_landing_view() {
        return false;
    }
    if editor.find.open && key.key == Key::Escape { editor.close_find(tx); return true; }
    if key.key == Key::Function(11) || (key.key == Key::Enter && key.mods.shift && (key.mods.cmd || key.mods.ctrl)) || (key.key == Key::Escape && editor.focus_mode && !editor.view_settings_open && !editor.find.open) {
        editor.focus_mode = !editor.focus_mode;
        return true;
    }
    if !key.mods.shift && !key.mods.alt {
        let delta = if key.is_command('=') { Some(1.0) } else if key.is_command('-') { Some(-1.0) } else { None };
        if let Some(delta) = delta {
            editor.store.ui.font_size = (editor.store.ui.font_size + delta).clamp(13.0, 24.0);
            let _ = prefs::save_store(&editor.store);
            return true;
        }
        if key.is_command('0') { editor.store.ui.font_size = 16.5; let _ = prefs::save_store(&editor.store); return true; }
    }
    if key.is_command('p') && !key.mods.shift && !key.mods.alt { editor.open_quick(tx); return true; }
    if key.is_command('f') && key.mods.shift && !key.mods.alt { editor.open_folder_search(tx); return true; }
    if key.is_command('f') && !key.mods.shift && !key.mods.alt { editor.open_find(false, tx); return true; }
    if key.is_command('h') && !key.mods.shift && !key.mods.alt { editor.open_find(true, tx); return true; }
    if key.is_command('v') && key.mods.shift && !key.mods.alt {
        let mode = if editor.view_mode == ViewMode::Preview { ViewMode::Write } else { ViewMode::Preview };
        editor.set_view_mode(mode, tx);
        return true;
    }
    if key.is_command('\\') && !key.mods.shift && !key.mods.alt { editor.toggle_reference(); return true; }
    if is_sidebar_toggle(key) {
        editor.toggle_sidebar();
        return true;
    }
    if key.is_command('j') && !key.mods.shift && !key.mods.alt {
        editor.toggle_inspector();
        return true;
    }
    if editor.section_menu_open && key.key == Key::Escape {
        editor.section_menu_open = false;
        editor.focus_document(tx);
        return true;
    }
    if key.key == Key::Function(8) || (key.is_command('.') && key.mods.shift) {
        editor.jump_next_placeholder(tx);
        return true;
    }
    if key.is_command('l') && !key.mods.shift && !key.mods.alt {
        editor.toggle_voice_lens();
        return true;
    }
    if key.is_command('n') && !key.mods.shift && editor.workspace.is_some() {
        editor.open_new_file_dialog();
        return true;
    }
    if key.is_command('w') && !key.mods.shift && editor.close_active_tab(tx) {
        return true;
    }
    if key.key == Key::Tab && (key.mods.cmd || key.mods.ctrl) {
        editor.cycle_tab(key.mods.shift, tx);
        return true;
    }
    if key.is_command('s') && !key.mods.shift {
        editor.save_clicked();
        return true;
    }
    if key.is_command('d') && !key.mods.shift && editor.active_has_baseline() {
        let mode = if editor.view_mode == ViewMode::Compare { ViewMode::Write } else { ViewMode::Compare };
        editor.set_view_mode(mode, tx);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session::Session,
        worker::{CliRunner, InFlight, WakeFn, WriteOp, WriteRunner},
    };
    use rune::Tx;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, mpsc};
    use std::{thread, time::Duration};

    fn test_session() -> Session {
        Session {
            project: PathBuf::from("/tmp/ghostless-test-voice"),
            id: "2026-01-02-notes".into(),
            source_name: "draft.md".into(),
            original: "Original text.".into(),
            after: "Original text.".into(),
            notes: String::new(),
            before_sha256: "test".into(),
            finished: false,
        }
    }

    #[test]
    fn folder_search_replies_leave_saves_alone_and_ignore_stale_results() {
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        editor.folder_search.open = true;
        editor.folder_search.request_id = Some(2);
        editor.worker_busy = Some((DocId::File("/tmp/a.md".into()), InFlight::Save));
        assert!(!editor.accept_folder_search(SearchReply { id: 1, matches: Vec::new() }));
        assert_eq!(editor.folder_search.request_id, Some(2));
        assert!(editor.accept_folder_search(SearchReply { id: 2, matches: Vec::new() }));
        assert_eq!(editor.folder_search.request_id, None);
        assert!(editor.worker_busy.take().is_some());
    }

    #[test]
    fn find_keeps_the_active_occurrence_and_replace_advances_past_inserted_text() {
        let mut session = test_session();
        session.after = "Café / Café / Café".into();
        let mut editor = Editor::with_handlers(
            session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(true, &mut tx);
        editor.step_find(true, &mut tx);
        let before = editor.find.current_range().cloned().unwrap();

        let edit = editor.active_working_edit_mut();
        edit.set_caret(0);
        edit.insert("Café — ");
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.current, Some(2));
        assert_eq!(editor.find.current_range().unwrap().start, before.start + "Café — ".len());
        assert_eq!(editor.active_working_edit().range(), ("Café — ".len(), "Café — ".len()));

        editor.find.replacement.replace_text("Café again");
        editor.replace_find(false, &mut tx);
        assert_eq!(editor.find.current, Some(3));
        assert_eq!(editor.active_working_edit().selected(), "Café");
        assert!(editor.active_working_edit().text.ends_with("Café again / Café"));

        editor.find.replacement.replace_text("");
        editor.replace_find(false, &mut tx);
        assert_eq!(editor.find.current, Some(0));
    }

    #[test]
    fn editing_inside_a_match_keeps_navigation_anchored_there() {
        let mut session = test_session();
        session.after = "Café / Café / Café".into();
        let mut editor = Editor::with_handlers(
            session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(false, &mut tx);
        editor.step_find(true, &mut tx);
        let middle = editor.find.current_range().unwrap().start;

        let edit = editor.active_working_edit_mut();
        edit.set_selection(middle + 1, middle + 2);
        edit.insert("A");
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.current, Some(1));

        let edit = editor.active_working_edit_mut();
        edit.set_caret(middle + 3);
        edit.insert("✨");
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.matches.len(), 2);
        assert_eq!(editor.find.current, None);
        assert_eq!(editor.find.navigation_anchor(), Some(middle));
        let caret = middle + 3 + "✨".len();
        assert_eq!(editor.active_working_edit().range(), (caret, caret));

        editor.active_working_edit_mut().undo();
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.current, Some(1));
        assert_eq!(editor.find.current_range().unwrap().start, middle);

        editor.active_working_edit_mut().redo();
        editor.changed_active(&mut tx);
        editor.step_find(true, &mut tx);
        assert_eq!(editor.find.current, Some(1));
        assert!(editor.active_working_edit().range().0 > middle);
    }

    #[test]
    fn unsuccessful_replace_preserves_selection_and_document_generation() {
        let mut session = test_session();
        session.after = "Café / Café".into();
        let mut editor = Editor::with_handlers(
            session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(true, &mut tx);
        let selection = editor.active_working_edit().range();
        let generation = editor.active_doc_ref().unwrap().generation;

        editor.find.replacement.replace_text("Café");
        editor.replace_find(false, &mut tx);
        editor.replace_find(true, &mut tx);
        assert_eq!(editor.find.current, Some(0));
        assert_eq!(editor.active_working_edit().range(), selection);
        assert_eq!(editor.active_doc_ref().unwrap().generation, generation);

        // A range that lands inside é is rejected without moving to the next hit.
        editor.find.matches[0] = 4..5;
        editor.find.replacement.replace_text("Tea");
        editor.replace_find(false, &mut tx);
        assert_eq!(editor.find.current, Some(0));
        assert_eq!(editor.active_working_edit().range(), selection);
        assert_eq!(editor.active_doc_ref().unwrap().generation, generation);
        assert_eq!(editor.active_working_edit().text, "Café / Café");
    }

    #[test]
    fn replace_all_reconciles_selection_with_remaining_matches_and_caret() {
        let mut session = test_session();
        session.after = "Café / Café / Café.".into();
        let mut editor = Editor::with_handlers(
            session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(true, &mut tx);
        editor.step_find(true, &mut tx);

        editor.find.replacement.replace_text("✨ CAFÉ");
        editor.replace_find(true, &mut tx);
        let replaced = "✨ CAFÉ / ✨ CAFÉ / ✨ CAFÉ.";
        assert_eq!(editor.active_working_edit().text, replaced);
        assert_eq!(editor.find.current, Some(2));
        let range = editor.find.current_range().unwrap();
        assert_eq!(editor.active_working_edit().range(), (range.start, range.end));
        assert_eq!(editor.active_working_edit().selected(), "CAFÉ");

        editor.find.replacement.replace_text("tea");
        editor.replace_find(true, &mut tx);
        assert_eq!(editor.active_working_edit().text, "✨ tea / ✨ tea / ✨ tea.");
        assert!(editor.find.matches.is_empty());
        assert_eq!(editor.find.current, None);
        let caret = "✨ tea / ✨ tea / ✨ tea".len();
        assert_eq!(editor.find.navigation_anchor(), Some(caret));
        assert_eq!(editor.active_working_edit().range(), (caret, caret));

        editor.active_working_edit_mut().undo();
        editor.changed_active(&mut tx);
        assert_eq!(editor.active_working_edit().text, replaced);
    }

    #[test]
    fn preview_blocks_replacement_and_replace_shortcut_opens_write() {
        let mut editor = Editor::with_handlers(
            test_session(), prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("Original");
        editor.find.replacement.replace_text("Revised");
        editor.open_find(true, &mut tx);
        editor.set_view_mode(ViewMode::Preview, &mut tx);
        assert!(editor.find.open);
        assert!(!editor.find.show_replace);

        editor.replace_find(false, &mut tx);
        editor.replace_find(true, &mut tx);
        assert_eq!(editor.active_working_edit().text, "Original text.");
        assert_eq!(editor.active_doc_ref().unwrap().generation, 0);

        let shortcut = KeyEvent { key: Key::Command('h'), mods: rune::Mods::default() };
        assert!(on_key(&mut editor, &shortcut, &mut tx));
        assert_eq!(editor.view_mode, ViewMode::Write);
        assert!(editor.find.show_replace);
        assert_eq!(editor.active_working_edit().text, "Original text.");

        editor.set_view_mode(ViewMode::Preview, &mut tx);
        editor.active_doc_mut().unwrap().read_only = true;
        assert!(on_key(&mut editor, &shortcut, &mut tx));
        assert_eq!(editor.view_mode, ViewMode::Preview);
        assert!(!editor.find.show_replace);
    }

    #[test]
    fn preview_scrolls_to_orphaned_find_anchor() {
        let prose_padding = "A paragraph above or below the edit.\n\n".repeat(70);
        let yaml_padding = "# A metadata comment above the edit.\n".repeat(120);
        let fixtures = [
            (format!("First Café.\n\n{prose_padding}Middle Café.\n\n{prose_padding}Last Café."), true, 2),
            (format!("---\n{yaml_padding}stage: Café\n---\n# Preview"), false, 0),
        ];
        for (source, step_to_middle, remaining) in fixtures {
            let mut session = test_session();
            session.after = source;
            let mut editor = Editor::with_handlers(
                session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
            );
            let mut tx = Tx::default();
            editor.find.query.replace_text("café");
            editor.open_find(false, &mut tx);
            if step_to_middle {
                editor.step_find(true, &mut tx);
            }
            let start = editor.find.current_range().unwrap().start;
            editor.active_working_edit_mut().insert("tea");
            editor.changed_active(&mut tx);
            assert_eq!(editor.find.navigation_anchor(), Some(start));
            assert_eq!(editor.find.current, None);
            let caret = editor.active_working_edit().range();

            let mut rt = rune::Runtime::new(
                editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0),
            );
            rt.frame(0.0);
            rt.update(|editor, tx| editor.set_view_mode(ViewMode::Preview, tx));
            rt.frame(0.0);

            let elements = rt.elements();
            let element = |key| elements.iter().find(|e| e.key.as_deref() == Some(key)).unwrap();
            let viewport = element("preview-body").rect;
            let mark = element("preview-find-anchor");
            assert_eq!(mark.text.as_deref(), Some("t"));
            assert_eq!(mark.path.contains("/preview-frontmatter/"), !step_to_middle);
            assert!(
                mark.rect[1] >= viewport[1] && mark.rect[1] + mark.rect[3] <= viewport[1] + viewport[3],
                "anchor {:?} is outside preview {viewport:?}", mark.rect,
            );
            assert_eq!(rt.state.find.current, None);
            assert_eq!(rt.state.find.matches.len(), remaining);
            assert_eq!(rt.state.active_working_edit().range(), caret);

            // The writer can move the caret independently of Find. Switching
            // back to Write should still reveal the saved occurrence's position.
            rt.update(|editor, tx| {
                editor.active_working_edit_mut().set_caret(0);
                editor.set_view_mode(ViewMode::Write, tx);
            });
            rt.frame(0.0);
            let elements = rt.elements();
            let element = |key| elements.iter().find(|e| e.key.as_deref() == Some(key)).unwrap();
            let viewport = element("working-body").rect;
            let mark = element(crate::search::SOURCE_ANCHOR_KEY);
            assert_eq!(mark.text.as_deref(), Some("t"));
            assert!(
                mark.rect[1] >= viewport[1] && mark.rect[1] + mark.rect[3] <= viewport[1] + viewport[3],
                "saved anchor {:?} is outside Write viewport {viewport:?}", mark.rect,
            );
            assert_eq!(rt.state.active_working_edit().range(), (0, 0));
        }
    }

    #[test]
    fn preview_suppresses_hidden_comment_find_anchor_and_scrolls_to_visible_fallback() {
        let prose_padding = "A paragraph above or below the edit.\n\n".repeat(70);
        let mut session = test_session();
        session.after = format!(
            "First Café with more text here.\n\n{prose_padding}<!-- Café Café -->\n\n{prose_padding}Last Café."
        );
        let mut editor = Editor::with_handlers(
            session,
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(false, &mut tx);
        editor.step_find(true, &mut tx);
        editor.step_find(true, &mut tx);
        assert_eq!(editor.find.current, Some(2));
        let start = editor.find.current_range().unwrap().start;
        editor.active_working_edit_mut().insert("tea");
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.current, None);
        assert_eq!(editor.find.navigation_anchor(), Some(start));
        assert_eq!(editor.find.matches.len(), 3);

        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        rt.update(|editor, tx| editor.set_view_mode(ViewMode::Preview, tx));
        rt.frame(0.0);

        let elements = rt.elements();
        assert!(
            elements
                .iter()
                .all(|e| e.key.as_deref() != Some("preview-find-anchor"))
        );
        let viewport = elements
            .iter()
            .find(|e| e.key.as_deref() == Some("preview-body"))
            .unwrap()
            .rect;
        let fallback = elements
            .iter()
            .find(|e| e.key.as_deref() == Some("md-mark-2"))
            .unwrap();
        assert_eq!(fallback.text.as_deref(), Some("Café"));
        assert!(
            fallback.rect[1] >= viewport[1]
                && fallback.rect[1] + fallback.rect[3] <= viewport[1] + viewport[3],
            "fallback {:?} is outside preview {viewport:?}",
            fallback.rect,
        );
    }

    #[test]
    fn closing_find_focuses_the_document_surface_that_is_mounted() {
        for (mode, read_only, title, expected) in [
            (ViewMode::Write, false, "draft.md", "prose-input"),
            (ViewMode::Write, true, "draft.md", "working-body"),
            (ViewMode::Preview, false, "draft.md", "preview-body"),
            (ViewMode::Write, true, "voice-guide.md", "preview-body"),
        ] {
            let mut editor = Editor::empty_with_handlers(
                prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
            );
            editor.documents.push(DocumentBuffer::from_workspace_file(
                PathBuf::from(title), title.into(), "Preview words.".into(),
                read_only, false, true, None,
            ));
            editor.active_doc = Some(0);
            editor.view_mode = mode;
            editor.reference.open = true;
            editor.reference.source = Some(crate::ui::reference::ReferenceSource::File("reference.md".into()));
            editor.reference.cached_content = "Reference words.".into();

            let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
            rt.on_key = Some(on_key);
            rt.frame(0.0);
            rt.update(|editor, tx| editor.open_find(false, tx));
            rt.frame(0.0);
            assert!(rt.elements().iter().any(|e| e.focused && e.label.as_deref() == Some("Find")));
            rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
            rt.frame(0.0);

            assert!(!rt.state.find.open);
            let elements = rt.elements();
            let focused = elements.iter().find(|element| element.focused)
                .and_then(|element| element.key.as_deref());
            assert_eq!(focused, Some(expected), "{mode:?}, {title}, read-only={read_only}");
        }
    }

    #[test]
    fn search_dialogs_restore_document_focus_on_all_dismissal_paths() {
        for (mode, read_only, title, expected) in [
            (ViewMode::Preview, false, "draft.md", "preview-body"),
            (ViewMode::Write, true, "voice-guide.md", "preview-body"),
            (ViewMode::Compare, false, "draft.md", "compare-body"),
        ] {
            for folder in [false, true] {
                for dismissal in ["Escape", "Close", "Outside"] {
                    let mut editor = Editor::with_handlers(
                        test_session(), prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
                    );
                    let doc = editor.active_doc_mut().unwrap();
                    doc.title = title.into();
                    doc.path.set_file_name(title);
                    doc.read_only = read_only;
                    editor.view_mode = mode;
                    editor.reference.open = true;
                    editor.reference.source = Some(crate::ui::reference::ReferenceSource::SessionOriginal);

                    let mut rt = rune::Runtime::new(
                        editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0),
                    );
                    rt.on_key = Some(on_key);
                    rt.frame(0.0);
                    assert!(!rt.elements().iter().any(|e| e.focused));
                    rt.update(|editor, tx| {
                        if folder {
                            editor.workspace = Some(PathBuf::from("/tmp"));
                            editor.open_folder_search(tx);
                            editor.workspace = None;
                        } else {
                            editor.open_quick(tx);
                        }
                    });
                    rt.frame(0.0);
                    let label = if folder { "Search in workspace" } else { "Go to file or session" };
                    assert!(rt.elements().iter().any(|e| e.focused && e.label.as_deref() == Some(label)));
                    match dismissal {
                        "Escape" => rt.key(KeyEvent { key: Key::Escape, mods: Default::default() }),
                        "Close" => {
                            let [x, y, w, h] = rt.elements().iter()
                                .find(|e| e.key.as_deref() == Some("close")).unwrap().rect;
                            rt.pointer_down(x + w / 2.0, y + h / 2.0);
                            rt.pointer_up(x + w / 2.0, y + h / 2.0);
                        }
                        _ => {
                            rt.pointer_down(10.0, 80.0);
                            rt.pointer_up(10.0, 80.0);
                        }
                    }
                    rt.frame(0.0);

                    assert!(!rt.state.quick.open && !rt.state.folder_search.open);
                    let elements = rt.elements();
                    let focused = elements.iter().find(|e| e.focused).and_then(|e| e.key.as_deref());
                    assert_eq!(focused, Some(expected), "{mode:?}, {title}, folder={folder}, {dismissal}");
                }
            }
        }
    }

    #[test]
    fn accepting_search_results_focuses_the_rendered_document() {
        use crate::quick_open::{QuickItem, QuickTarget, search_workspace_files};

        for folder in [false, true] {
            let mut editor = Editor::empty_with_handlers(
                prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
            );
            let name = if folder { "voice-guide.md" } else { "draft.md" };
            let path = PathBuf::from(name);
            let source = "A Café note.";
            editor.documents.push(DocumentBuffer::from_workspace_file(
                path.clone(), name.into(), source.into(), folder, false, true, None,
            ));
            editor.active_doc = Some(0);
            editor.view_mode = ViewMode::Preview;

            let mut rt = rune::Runtime::new(
                editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0),
            );
            rt.frame(0.0);
            rt.update(|editor, tx| {
                if folder {
                    editor.workspace = Some(PathBuf::from("/tmp"));
                    editor.open_folder_search(tx);
                    editor.workspace = None;
                    editor.folder_search.query.replace_text("café");
                    editor.folder_search.matches = search_workspace_files(
                        &[WorkspaceEntry {
                            abs_path: path.clone(), rel_path: name.into(),
                            kind: workspace_tree::FileKind::Writing, read_only: true,
                        }],
                        &[(path, Some(source.into()))].into_iter().collect(),
                        "café", 10, || false,
                    );
                } else {
                    editor.open_quick(tx);
                    editor.quick.items.push(QuickItem {
                        id: name.into(), title: name.into(), subtitle: String::new(),
                        target: QuickTarget::File(path),
                    });
                }
            });
            rt.frame(0.0);
            rt.key(KeyEvent { key: Key::Enter, mods: Default::default() });
            rt.frame(0.0);

            assert!(!rt.state.quick.open && !rt.state.folder_search.open);
            assert!(rt.elements().iter().any(|e| e.focused && e.key.as_deref() == Some("preview-body")));
            assert_eq!(rt.state.active_working_edit().text, source);
            if folder {
                assert_eq!(rt.state.active_working_edit().selected(), "Café");
            }
        }
    }

    #[test]
    fn compare_closes_find_and_find_shortcut_returns_to_write() {
        let mut editor = Editor::with_handlers(
            test_session(), prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("Original");
        editor.open_find(false, &mut tx);
        editor.set_view_mode(ViewMode::Compare, &mut tx);
        assert!(!editor.find.open);
        let key = KeyEvent { key: Key::Command('h'), mods: rune::Mods::default() };
        assert!(on_key(&mut editor, &key, &mut tx));
        assert_eq!(editor.view_mode, ViewMode::Write);
        assert!(editor.find.open && editor.find.show_replace);
        assert_eq!(editor.active_working_edit().selected(), "Original");
    }

    #[test]
    fn folder_hit_uses_open_buffer_and_relocates_after_an_edit() {
        use crate::{quick_open::search_workspace_files, workspace_tree::FileKind};
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let path = std::env::temp_dir().join(format!("ghostless-folder-hit-{}.md", std::process::id()));
        let entry = WorkspaceEntry {
            abs_path: path.clone(), rel_path: "draft.md".into(), kind: FileKind::Writing, read_only: false,
        };
        let source = "Opening\nSecond CAFÉtéria\nSeparator\nSecond CAFÉtéria";
        let buffers = [(path.clone(), Some(source.into()))].into_iter().collect();
        editor.documents.push(DocumentBuffer::from_workspace_file(
            path, "draft.md".into(), format!("New ✨ line\n{source}"), false, false, true, None,
        ));
        editor.active_doc = Some(0);
        editor.folder_search.open = true;
        editor.folder_search.query.replace_text("café");
        editor.folder_search.matches = search_workspace_files(&[entry], &buffers, "café", 200, || false);
        editor.find.case_sensitive = true;
        editor.find.whole_word = true;
        editor.view_mode = ViewMode::Preview;

        editor.choose_folder_match(0, &mut Tx::default());

        assert!(!editor.folder_search.open);
        assert!(!editor.find.case_sensitive && !editor.find.whole_word);
        assert_eq!(editor.find.current, Some(0));
        assert_eq!(editor.active_working_edit().selected(), "CAFÉ");
        assert_eq!(editor.active_working_edit().range().0, "New ✨ line\nOpening\nSecond ".len());
        assert_eq!(editor.view_mode, ViewMode::Write);
        assert_eq!(
            editor.find.notice.as_deref(),
            Some("File changed. Showing the closest match."),
        );
    }

    fn noop_wake() -> WakeFn {
        Box::new(|| {})
    }

    fn ok_writer() -> WriteRunner {
        Box::new(|_| Ok(WriteOutcome::Local))
    }

    fn ok_runner() -> CliRunner {
        Box::new(|_, _| Ok("{}".into()))
    }

    fn drain_until_idle(editor: &mut Editor) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while editor.worker_busy.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for background worker"
            );
            let mut tx = Tx::default();
            poll(editor, 10.0, &mut tx);
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn save_and_finish_do_not_block_ui_thread() {
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = writes.clone();

        let writer: WriteRunner = Box::new(move |op| {
            recorded.lock().unwrap().push(op.clone());
            let _ = entered_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
            Ok(WriteOutcome::Local)
        });

        let mut editor = Editor::with_handlers(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            writer,
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "First draft edit.".into();
        editor.changed_active(&mut tx);
        editor.save_clicked();

        assert_eq!(
            editor.worker_busy.as_ref().map(|(_, k)| *k),
            Some(InFlight::Save)
        );
        assert_eq!(editor.status, "Saving…");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started running write");

        editor.active_working_edit_mut().text = "Second draft edit.".into();
        editor.changed_active(&mut tx);
        assert!(editor.documents[0].pending_save);

        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        drain_until_idle(&mut editor);
        assert_eq!(editor.status, "Saved locally");
        assert!(!editor.documents[0].is_dirty());
        assert_eq!(writes.lock().unwrap().len(), 2);
    }

    #[test]
    fn rapid_edits_during_in_flight_save_coalesce_into_one_follow_up() {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = writes.clone();

        let writer: WriteRunner = Box::new(move |op| {
            let count = {
                let mut guard = recorded.lock().unwrap();
                guard.push(op.clone());
                guard.len()
            };
            if count == 1 {
                let _ = started_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            }
            Ok(WriteOutcome::Local)
        });

        let mut editor = Editor::with_handlers(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            writer,
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Edit 0".into();
        editor.changed_active(&mut tx);
        assert!(poll(&mut editor, 1.0, &mut tx));
        assert_eq!(
            editor.worker_busy.as_ref().map(|(_, k)| *k),
            Some(InFlight::Save)
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first save entered worker");

        for idx in 1..=5 {
            editor.active_working_edit_mut().text = format!("Edit {idx}");
            editor.active_notes_edit_mut().text = format!("Note {idx}");
            editor.changed_active(&mut tx);
        }

        release_tx.send(()).unwrap();
        drain_until_idle(&mut editor);

        let guard = writes.lock().unwrap();
        assert_eq!(
            guard.len(),
            2,
            "5 rapid edits must coalesce into one follow-up save"
        );
        match &guard[1] {
            WriteOp::Session { after, notes, .. } => {
                assert_eq!(after, "Edit 5");
                assert_eq!(notes, "Note 5");
            }
            other => panic!("expected WriteOp::Session, got {other:?}"),
        }
        assert_eq!(editor.status, "Saved locally");
        assert!(!editor.documents[0].is_dirty());
    }

    #[test]
    fn recovery_copy_can_be_reviewed_without_overwriting_disk() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-recovery-{}", std::process::id()));
        let home = base.join("home"); let workspace = base.join("workspace");
        fs::create_dir_all(&home).unwrap(); fs::create_dir_all(&workspace).unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let file = workspace.join("draft.md"); fs::write(&file, "disk text").unwrap();
        let file = file.canonicalize().unwrap();
        let recovery = crate::documents::recovery_path_for(&file);
        fs_io::atomic_write_text(&recovery, "recovered text").unwrap();
        let mut editor = Editor::empty_with_handlers(prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.workspace = Some(workspace);
        editor.open_workspace_file(file.clone(), true);
        assert!(matches!(editor.active_dialog, Some(DialogKind::Recover { .. })));
        assert_eq!(editor.active_working_edit().text, "disk text");
        editor.close_dialog();
        assert!(recovery.exists());
        editor.open_workspace_file(file.clone(), true);
        assert!(matches!(editor.active_dialog, Some(DialogKind::Recover { .. })));
        editor.resolve_recovery(true);
        assert_eq!(editor.active_working_edit().text, "recovered text");
        assert!(editor.active_doc_ref().unwrap().is_dirty());
        assert_eq!(fs::read_to_string(&file).unwrap(), "disk text");
        assert!(recovery.exists());
        editor.documents.clear(); editor.workspace = None;
        unsafe { if let Some(v) = old_home { std::env::set_var("GHOSTLESS_HOME", v); } else { std::env::remove_var("GHOSTLESS_HOME"); } }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn dirty_locked_file_saves_on_tab_or_window_close() {
        for action in ["tab", "manual tab", "window"] {
            let mut store = prefs::EditorStateStore::default();
            store.ui.autosave_workspace_files = action != "manual tab";
            let saved = Arc::new(Mutex::new(Vec::new()));
            let recorded = saved.clone();
            let writer: WriteRunner = Box::new(move |op| {
                if let WriteOp::File { content, .. } = op {
                    recorded.lock().unwrap().push(content.clone());
                }
                Ok(WriteOutcome::Local)
            });
            let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
            editor.documents.push(DocumentBuffer::from_workspace_file(
                "/tmp/pre-answers.md".into(), "pre-answers.md".into(), "old".into(),
                false, true, true, None,
            ));
            editor.active_doc = Some(0);
            let mut tx = Tx::default();
            editor.active_working_edit_mut().text = "latest edit".into();
            editor.changed_active(&mut tx);
            editor.toggle_active_user_lock();
            assert!(editor.active_doc_ref().unwrap().user_locked);
            match action {
                "tab" => assert!(editor.close_active_tab(&mut tx)),
                "manual tab" => {
                    assert!(!editor.close_active_tab(&mut tx));
                    assert!(matches!(editor.active_dialog, Some(DialogKind::ConfirmClose { .. })));
                    editor.confirm_close_save();
                    drain_until_idle(&mut editor);
                    assert!(editor.documents.is_empty());
                }
                _ => assert!(editor.flush_on_exit()),
            }
            assert_eq!(*saved.lock().unwrap(), ["latest edit"]);
        }
    }

    fn block_first_write(
        writer: WriteRunner,
    ) -> (WriteRunner, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started, started_rx) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let release = Mutex::new(Some(release));
        let wrapped: WriteRunner = Box::new(move |op| {
            if let Some(release) = release.lock().unwrap().take() {
                started.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            writer(op)
        });
        (wrapped, started_rx, release_tx)
    }

    #[test]
    fn manual_save_does_not_write_edits_made_during_the_save() {
        let mut store = prefs::EditorStateStore::default();
        store.ui.autosave_workspace_files = false;
        let saved = Arc::new(Mutex::new(Vec::new()));
        let recorded = saved.clone();
        let (writer, started, release) = block_first_write(Box::new(move |op| {
            if let WriteOp::File { content, .. } = op {
                recorded.lock().unwrap().push(content.clone());
            }
            Ok(WriteOutcome::Local)
        }));
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
        editor.documents.push(DocumentBuffer::from_workspace_file(
            "/tmp/manual.md".into(), "manual.md".into(), "old".into(), false, false, true, None,
        ));
        editor.active_doc = Some(0);
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "explicit save".into();
        editor.changed_active(&mut tx);
        editor.save_clicked();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        editor.active_working_edit_mut().text = "subsequent edit".into();
        editor.changed_active(&mut tx);
        release.send(()).unwrap();
        drain_until_idle(&mut editor);
        assert_eq!(*saved.lock().unwrap(), ["explicit save"]);
        assert!(editor.active_doc_ref().unwrap().is_dirty());
        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert_eq!(*saved.lock().unwrap(), ["explicit save", "subsequent edit"]);
    }

    #[test]
    fn rename_waits_for_save_and_keeps_latest_contents() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ghostless-rename-busy-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", root.join("home")); }
        let original = root.join("draft.md");
        fs::write(&original, "old").unwrap();
        let (writer, started, release) = block_first_write(Box::new(execute_write_op));
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(), ok_runner(), writer, noop_wake(),
        );
        editor.open_workspace_file(original.clone(), true);
        editor.workspace = Some(root.clone());
        editor.active_working_edit_mut().text = "latest".into();
        let mut tx = Tx::default();
        editor.changed_active(&mut tx);
        editor.save_clicked();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        editor.active_dialog = Some(DialogKind::Rename { rel_path: "draft.md".into() });
        editor.dialog_input.text = "renamed.md".into();
        editor.submit_dialog(&mut tx);
        assert!(editor.dialog_error.is_some());
        assert!(original.is_file());

        release.send(()).unwrap();
        drain_until_idle(&mut editor);
        editor.submit_dialog(&mut tx);
        assert!(!original.exists());
        assert_eq!(fs::read_to_string(root.join("renamed.md")).unwrap(), "latest");
        assert!(editor.flush_on_exit());
        unsafe {
            if let Some(value) = prev_home { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discard_waits_for_inflight_recovery_before_removing_copy() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ghostless-discard-busy-{}", std::process::id()));
        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", root.join("home")); }
        fs::create_dir_all(&root).unwrap();
        let path = root.join("draft.md");
        fs::write(&path, "disk version").unwrap();
        let mut store = prefs::EditorStateStore::default();
        store.ui.autosave_workspace_files = false;
        let (writer, started, release) = block_first_write(Box::new(execute_write_op));
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
        editor.open_workspace_file(path.clone(), true);
        let recovery = editor.active_doc_ref().unwrap().recovery_path();
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "discarded text".into();
        editor.changed_active(&mut tx);
        editor.flush_autosave_buffers();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!editor.close_active_tab(&mut tx));
        release.send(()).unwrap();
        editor.confirm_close_discard(&mut tx);
        assert!(editor.worker_busy.is_none());
        assert!(editor.documents.is_empty());
        assert!(!recovery.exists());
        assert_eq!(fs::read_to_string(path).unwrap(), "disk version");
        unsafe {
            if let Some(value) = prev_home { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn application_quit_preserves_failed_session_save_for_reopen() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ghostless-quit-recovery-{}", std::process::id()));
        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", root.join("home")); }
        let mut session = test_session();
        session.project = root.join("voice");
        session.before_sha256 = fs_io::sha256_text(&session.original);
        let folder = session.project.join("sessions").join(&session.id);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("before.md"), &session.original).unwrap();
        fs::write(folder.join("after.md"), &session.after).unwrap();
        fs::write(folder.join("session.json"), serde_json::json!({
            "kind": "ghostless-edit-session",
            "beforeSha256": session.before_sha256,
        }).to_string()).unwrap();
        let mut editor = Editor::with_handlers(
            session.clone(), prefs::EditorStateStore::default(), ok_runner(),
            Box::new(|_| Err("session directory is read-only".into())), noop_wake(),
        );
        editor.active_working_edit_mut().text = "unsaved body".into();
        editor.active_notes_edit_mut().text = "unsaved notes".into();
        let mut tx = Tx::default();
        editor.changed_active(&mut tx);
        let quit = KeyEvent { key: Key::Command('q'), mods: Default::default() };
        assert!(on_key(&mut editor, &quit, &mut tx));
        assert!(!editor.flushed);
        assert!(editor.active_doc_ref().unwrap().is_dirty());
        assert!(editor.status.contains("Could not close"));
        editor.on_application_quit();
        assert!(!editor.flushed);
        let copy = editor.active_doc_ref().unwrap().recovery_path();
        assert!(copy.is_file());
        drop(editor);

        let mut reopened = Editor::new(LaunchTarget::Session(session), noop_wake());
        assert!(matches!(reopened.active_dialog, Some(DialogKind::Recover { .. })));
        reopened.resolve_recovery(true);
        assert_eq!(reopened.active_working_edit().text, "unsaved body");
        assert_eq!(reopened.active_notes_edit().text, "unsaved notes");
        assert!(reopened.active_doc_ref().unwrap().is_dirty());
        assert!(copy.is_file());
        reopened.save_clicked();
        drain_until_idle(&mut reopened);
        assert!(!copy.exists());
        assert_eq!(fs::read_to_string(folder.join("after.md")).unwrap(), "unsaved body");
        assert_eq!(fs::read_to_string(folder.join("notes.md")).unwrap(), "unsaved notes");
        unsafe {
            if let Some(value) = prev_home { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drop_retries_when_quit_could_not_save_or_recover() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ghostless-quit-retry-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let blocked_home = root.join("recovery-unavailable");
        fs::write(&blocked_home, "").unwrap();
        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &blocked_home); }
        let file = root.join("draft.md");
        fs::write(&file, "disk version").unwrap();
        let save_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready = save_ready.clone();
        let writer: WriteRunner = Box::new(move |op| {
            if ready.load(std::sync::atomic::Ordering::SeqCst) {
                execute_write_op(op)
            } else {
                Err("Save temporarily unavailable".into())
            }
        });
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(), ok_runner(), writer, noop_wake(),
        );
        editor.open_workspace_file(file.clone(), true);
        editor.active_working_edit_mut().text = "latest edit".into();
        editor.changed_active(&mut Tx::default());
        let recovery = editor.active_doc_ref().unwrap().recovery_path();

        editor.on_application_quit();
        assert!(!editor.flushed);
        assert!(!recovery.is_file());
        assert!(editor.active_doc_ref().unwrap().is_dirty());
        assert_eq!(fs::read_to_string(&file).unwrap(), "disk version");

        fs::remove_file(blocked_home).unwrap();
        save_ready.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(editor);
        assert_eq!(fs::read_to_string(file).unwrap(), "latest edit");
        unsafe {
            if let Some(value) = prev_home { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_file_or_folder_with_dirty_tab_is_rejected() {
        let root = std::env::temp_dir().join(format!("ghostless-delete-dirty-{}", std::process::id()));
        fs::create_dir_all(root.join("drafts")).unwrap();
        let file = root.join("drafts/draft.md");
        fs::write(&file, "disk version").unwrap();
        let mut editor = Editor::empty_with_handlers(prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.open_workspace_file(file.clone(), true);
        editor.workspace = Some(root.clone());
        editor.active_working_edit_mut().text = "unsaved work".into();
        editor.changed_active(&mut Tx::default());
        for (rel, is_dir) in [("drafts/draft.md", false), ("drafts", true)] {
            editor.active_dialog = Some(DialogKind::Delete { rel_path: rel.into(), display_name: rel.into(), is_dir });
            editor.submit_dialog(&mut Tx::default());
            assert!(file.exists());
            assert_eq!(editor.active_working_edit().text, "unsaved work");
            assert!(editor.dialog_error.as_deref().unwrap().contains("unsaved"));
        }
        editor.documents.clear();
        editor.workspace = None;
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blank_link_hash_saves_session_locally_without_touching_draft() {
        let base = std::env::temp_dir().join(format!("ghostless-unknown-link-{}", std::process::id()));
        let project = base.join("voice"); let session_id = "2026-04-02-notes";
        let folder = project.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let before = "Original text.\n";
        fs::write(folder.join("before.md"), before).unwrap();
        fs::write(folder.join("after.md"), before).unwrap();
        fs::write(folder.join("session.json"), serde_json::json!({"kind":"ghostless-edit-session", "beforeSha256":fs_io::sha256_text(before)}).to_string()).unwrap();
        let draft = base.join("draft.md"); fs::write(&draft, "External draft\n").unwrap();
        let mut store = prefs::EditorStateStore::default();
        let key = prefs::EditorStateStore::session_link_key(&project, session_id);
        store.session_links.insert(key.clone(), prefs::SessionLink { workspace_file: draft.clone(), last_synced_sha256: String::new() });
        for (sidebar, relative_link) in [(false, false), (true, false), (true, true)] {
            let mut current_store = store.clone();
            if relative_link { current_store.session_links.get_mut(&key).unwrap().workspace_file = "draft.md".into(); }
            let mut editor = if sidebar {
                let mut app = Editor::empty_with_handlers(current_store, ok_runner(), Box::new(execute_write_op), noop_wake());
                if relative_link { app.workspace = Some(base.clone()); }
                app.open_session_entry(project.clone(), session_id.into());
                app
            } else {
                let session = load_session(project.clone(), session_id.into()).unwrap();
                Editor::with_handlers(session, current_store, ok_runner(), Box::new(execute_write_op), noop_wake())
            };
            assert!(editor.status.contains("no known sync state"));
            assert!(editor.active_doc_ref().unwrap().mirror_path.is_none());
            editor.active_working_edit_mut().text = "Locally saved session\n".into();
            editor.active_notes_edit_mut().text = "Notes saved".into();
            editor.changed_active(&mut Tx::default());
            editor.save_clicked(); drain_until_idle(&mut editor);
            assert!(!editor.active_doc_ref().unwrap().is_dirty());
            assert!(editor.status.contains("no known sync state"));
            assert_eq!(fs::read_to_string(folder.join("after.md")).unwrap(), "Locally saved session\n");
            assert_eq!(fs::read_to_string(folder.join("notes.md")).unwrap(), "Notes saved");
            assert_eq!(fs::read_to_string(&draft).unwrap(), "External draft\n");
            assert!(editor.store.session_links[&key].last_synced_sha256.is_empty());
            editor.workspace = None;
            editor.flush_on_exit();
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sidebar_session_with_relative_link_saves_locally_without_workspace() {
        let base = std::env::temp_dir().join(format!("ghostless-sidebar-relative-{}", std::process::id()));
        let project = base.join("voice");
        let session_id = "2026-04-02-notes";
        let folder = project.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let before = "Original text.\n";
        fs::write(folder.join("before.md"), before).unwrap();
        fs::write(folder.join("after.md"), before).unwrap();
        fs::write(folder.join("session.json"), serde_json::json!({
            "kind": "ghostless-edit-session", "beforeSha256": fs_io::sha256_text(before)
        }).to_string()).unwrap();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&project, session_id), prefs::SessionLink {
            workspace_file: "draft.md".into(), last_synced_sha256: fs_io::sha256_text(before)
        });
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), Box::new(execute_write_op), noop_wake());
        editor.open_session_entry(project, session_id.into());
        assert!(editor.status.contains("Linked file unavailable"));
        assert!(editor.active_doc_ref().unwrap().mirror_path.is_none());
        editor.active_working_edit_mut().text = "Saved locally.\n".into();
        editor.active_notes_edit_mut().text = "My notes".into();
        editor.changed_active(&mut Tx::default());
        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert!(!editor.active_doc_ref().unwrap().is_dirty());
        assert!(editor.status.contains("unavailable without a workspace"));
        assert_eq!(fs::read_to_string(folder.join("after.md")).unwrap(), "Saved locally.\n");
        assert_eq!(fs::read_to_string(folder.join("notes.md")).unwrap(), "My notes");
        editor.flush_on_exit();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn standalone_session_saves_when_link_has_no_workspace() {
        let session = test_session();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&session.project, &session.id), prefs::SessionLink { workspace_file: "draft.md".into(), last_synced_sha256: "known".into() });
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = writes.clone();
        let writer: WriteRunner = Box::new(move |op| { captured.lock().unwrap().push(op.clone()); Ok(WriteOutcome::Local) });
        let mut editor = Editor::with_handlers(session, store, ok_runner(), writer, noop_wake());
        assert!(editor.status.contains("unavailable without a workspace") || editor.status.contains("Linked file unavailable"));
        editor.active_working_edit_mut().text = "session edit".into();
        editor.changed_active(&mut Tx::default());
        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert!(matches!(&writes.lock().unwrap()[0], WriteOp::Session { mirror_path: None, .. }));
        assert!(!editor.documents[0].is_dirty());
        assert!(editor.status.contains("unavailable without a workspace"));
    }

    #[test]
    fn workspace_switch_preserves_dirty_documents_when_save_fails() {
        let writer: WriteRunner = Box::new(|_| Err("disk full".into()));
        let mut editor = Editor::with_handlers(test_session(), prefs::EditorStateStore::default(), ok_runner(), writer, noop_wake());
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "unsaved work".into();
        editor.changed_active(&mut tx);
        editor.open_workspace(PathBuf::from("/tmp/other-workspace"), None);
        assert!(editor.workspace.is_none());
        assert_eq!(editor.active_working_edit().text, "unsaved work");
        assert!(editor.documents[0].is_dirty());
        assert!(editor.status.contains("disk full"));
    }

    #[test]
    fn workspace_switch_preserves_manual_buffer_when_recovery_fails() {
        let writer: WriteRunner = Box::new(|_| Err("recovery disk full".into()));
        let mut store = prefs::EditorStateStore::default();
        store.ui.autosave_workspace_files = false;
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
        editor.workspace = Some(PathBuf::from("/tmp/old-workspace"));
        editor.documents.push(DocumentBuffer::from_workspace_file(PathBuf::from("/tmp/draft.md"), "draft.md".into(), "before".into(), false, false, true, None));
        editor.active_doc = Some(0);
        editor.active_working_edit_mut().text = "unsaved manual text".into();
        editor.changed_active(&mut Tx::default());
        editor.open_workspace(PathBuf::from("/tmp/new-workspace"), None);
        assert_eq!(editor.workspace.as_deref(), Some(Path::new("/tmp/old-workspace")));
        assert_eq!(editor.active_working_edit().text, "unsaved manual text");
        assert!(editor.status.contains("recovery disk full"));
    }

    #[test]
    fn workspace_switch_retains_dirty_buffer_when_worker_stops() {
        let mut editor = Editor::with_handlers(test_session(), prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.active_working_edit_mut().text = "still here".into();
        editor.changed_active(&mut Tx::default());
        editor.worker.shutdown();
        editor.open_workspace(PathBuf::from("/tmp/other-workspace"), None);
        assert!(editor.workspace.is_none());
        assert_eq!(editor.active_working_edit().text, "still here");
        assert!(editor.status.contains("Workspace not changed"));
    }

    #[test]
    fn voice_data_files_are_read_only_even_outside_tree() {
        let voice = Path::new("/tmp/voice");
        for relative in [
            "corpus/gold/example.md",
            "gold/example.md",
            "holdouts/example.md",
            "transcripts/example.txt",
            "sessions/2026-11-23/after.md",
            "sessions/2026-11-23/notes.md",
        ] {
            let path = voice.join(relative);
            assert_eq!(classify_file_flags(None, &path, Some(voice)), (true, false));
        }
        assert!(!classify_file_flags(None, &voice.join("drafts/after.md"), Some(voice)).0);
    }

    #[test]
    fn mirror_paths_require_and_stay_within_workspace() {
        let root = std::env::temp_dir();
        assert_eq!(resolve_mirror_path(Path::new("draft.md"), Some(&root)), root.join("draft.md"));
        assert!(!resolve_mirror_path(Path::new("../outside.md"), Some(&root)).is_absolute());
        assert!(!resolve_mirror_path(Path::new("draft.md"), None).is_absolute());
    }

    #[test]
    fn window_close_reopens_when_recovery_write_fails() {
        let mut store = prefs::EditorStateStore::default();
        store.ui.autosave_workspace_files = false;
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), Box::new(|_| Err("recovery unavailable".into())), noop_wake());
        editor.documents.push(DocumentBuffer::from_workspace_file(PathBuf::from("/tmp/recovery-draft.md"), "draft.md".into(), "before".into(), false, false, true, None));
        editor.active_doc = Some(0);
        let reopened = Arc::new(Mutex::new(false));
        let signal = reopened.clone();
        editor.reopen_window = Some(Box::new(move || *signal.lock().unwrap() = true));
        editor.active_working_edit_mut().text = "unsaved".into();
        editor.changed_active(&mut Tx::default());
        on_window_close(&mut editor, &mut Tx::default());
        assert!(*reopened.lock().unwrap());
        assert!(editor.documents[0].is_dirty());
        assert!(!editor.flushed);
        assert!(editor.status.contains("recovery unavailable"));
        editor.documents.clear();
    }

    #[test]
    fn window_close_reopens_on_save_failure_and_retries() {
        let calls = Arc::new(Mutex::new(0));
        let attempts = calls.clone();
        let writer: WriteRunner = Box::new(move |_| {
            let mut count = attempts.lock().unwrap();
            *count += 1;
            if *count == 1 { Err("disk full".into()) } else { Ok(WriteOutcome::Local) }
        });
        let reopened = Arc::new(Mutex::new(0));
        let reopen_count = reopened.clone();
        let mut editor = Editor::with_handlers(test_session(), prefs::EditorStateStore::default(), ok_runner(), writer, noop_wake());
        editor.reopen_window = Some(Box::new(move || *reopen_count.lock().unwrap() += 1));
        editor.active_working_edit_mut().text = "Keep this edit".into();
        editor.changed_active(&mut Tx::default());
        on_window_close(&mut editor, &mut Tx::default());
        assert_eq!(*reopened.lock().unwrap(), 1);
        assert!(!editor.flushed);
        assert!(editor.documents[0].is_dirty());
        assert!(editor.status.contains("disk full"));
        on_window_close(&mut editor, &mut Tx::default());
        assert_eq!(*calls.lock().unwrap(), 2);
        assert!(editor.flushed);
        assert!(!editor.documents[0].is_dirty());
    }

    #[test]
    fn flush_on_exit_saves_dirty_edits_before_autosave_timer_and_is_idempotent() {
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = writes.clone();
        let writer: WriteRunner = Box::new(move |op| {
            recorded.lock().unwrap().push(op.clone());
            Ok(WriteOutcome::Local)
        });

        let mut editor = Editor::with_handlers(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            writer,
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Typed right before closing.".into();
        editor.active_notes_edit_mut().text = "Keep this note.".into();
        editor.changed_active(&mut tx);
        assert!(editor.documents[0].is_dirty());
        assert!(editor.worker_busy.is_none());

        editor.flush_on_exit();
        editor.flush_on_exit();

        let guard = writes.lock().unwrap();
        assert_eq!(guard.len(), 1);
        match &guard[0] {
            WriteOp::Session { after, notes, .. } => {
                assert_eq!(after, "Typed right before closing.");
                assert_eq!(notes, "Keep this note.");
            }
            other => panic!("expected WriteOp::Session, got {other:?}"),
        }
        assert!(!editor.documents[0].is_dirty());
    }

    #[test]
    fn finish_waits_for_in_flight_save_then_finishes_once() {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let write_events = events.clone();
        let cli_events = events.clone();

        let writer: WriteRunner = Box::new(move |_| {
            let count = {
                let mut g = write_events.lock().unwrap();
                g.push("write_session");
                g.len()
            };
            if count == 1 {
                let _ = started_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            }
            Ok(WriteOutcome::Local)
        });
        let runner: CliRunner = Box::new(move |args, _| {
            if args[0] == "finish" {
                cli_events.lock().unwrap().push("finish");
            }
            Ok("{}".into())
        });

        let mut editor = Editor::with_handlers(
            test_session(),
            prefs::EditorStateStore::default(),
            runner,
            writer,
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Final prose.".into();
        editor.changed_active(&mut tx);
        editor.save_clicked();
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("snapshot started");

        editor.finish();
        editor.finish();
        assert!(editor.active_is_locked());

        release_tx.send(()).unwrap();
        editor.flush_on_exit();

        assert_eq!(*events.lock().unwrap(), vec!["write_session", "finish"]);
        assert!(editor.documents[0].finished);
        assert_eq!(editor.status, "Finished. Final and receipt saved locally.");
    }

    #[test]
    fn failed_save_keeps_dirty_without_spinning_and_flush_does_not_loop() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let writer: WriteRunner = Box::new(move |_| {
            *counter.lock().unwrap() += 1;
            Err("disk full".into())
        });

        let mut editor = Editor::with_handlers(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            writer,
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Will fail once.".into();
        editor.changed_active(&mut tx);
        editor.save_clicked();
        drain_until_idle(&mut editor);

        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(editor.documents[0].is_dirty());
        assert!(editor.status.contains("Save failed: disk full"));
        assert!(!poll(&mut editor, 10.0, &mut tx));
        assert_eq!(*calls.lock().unwrap(), 1);

        assert!(!editor.flush_on_exit());
        assert!(!editor.flush_on_exit());
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[test]
    fn per_tab_undo_redo_survives_switching_tabs() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-undo-tabs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = base.join("home");
        let ws = base.join("ws");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(ws.join("drafts")).unwrap();
        fs::write(ws.join("drafts/a.md"), "Alpha").unwrap();
        fs::write(ws.join("drafts/b.md"), "Beta").unwrap();

        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }

        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        editor.open_workspace(ws.clone(), Some(ws.join("drafts/a.md")));
        editor.open_workspace_file(ws.join("drafts/b.md"), true);

        let key_a = DocId::File(ws.join("drafts/a.md").canonicalize().unwrap()).tab_key();
        let key_b = DocId::File(ws.join("drafts/b.md").canonicalize().unwrap()).tab_key();
        let mut tx = Tx::default();

        // Edit Tab A
        editor.activate_tab(&key_a, &mut tx);
        editor.active_working_edit_mut().insert(" one");
        editor.changed_active(&mut tx);
        assert_eq!(editor.active_working_edit().text, "Alpha one");

        // Switch to Tab B and edit Tab B
        editor.activate_tab(&key_b, &mut tx);
        editor.active_working_edit_mut().insert(" two");
        editor.changed_active(&mut tx);
        assert_eq!(editor.active_working_edit().text, "Beta two");

        // Switch back to Tab A: undo/redo must still be intact and independent
        editor.activate_tab(&key_a, &mut tx);
        assert_eq!(editor.active_working_edit().text, "Alpha one");
        editor.active_working_edit_mut().undo();
        assert_eq!(editor.active_working_edit().text, "Alpha");
        editor.active_working_edit_mut().redo();
        assert_eq!(editor.active_working_edit().text, "Alpha one");

        // Tab B stayed untouched
        editor.activate_tab(&key_b, &mut tx);
        assert_eq!(editor.active_working_edit().text, "Beta two");

        drop(editor);
        unsafe {
            if let Some(v) = prev_home {
                std::env::set_var("GHOSTLESS_HOME", v);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn blur_flushes_dirty_autosave_buffers_and_manual_save_respects_mode() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-blur-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = base.join("home");
        let ws = base.join("ws");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(ws.join("drafts")).unwrap();
        let file_a = ws.join("drafts/a.md");
        let file_b = ws.join("drafts/b.md");
        fs::write(&file_a, "Original A").unwrap();
        fs::write(&file_b, "Original B").unwrap();

        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }

        // Auto-save ON: blur flushes file A immediately to disk.
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            ok_runner(),
            Box::new(execute_write_op),
            noop_wake(),
        );
        editor.open_workspace(ws.clone(), Some(file_a.clone()));
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Updated A on blur".into();
        editor.changed_active(&mut tx);
        assert!(editor.documents[0].is_dirty());

        editor.flush_autosave_buffers();
        drain_until_idle(&mut editor);
        assert_eq!(fs_io::read_utf8(&file_a).unwrap(), "Updated A on blur");
        assert!(!editor.documents[0].is_dirty());

        // Auto-save OFF (Manual Save): blur writes only crash-recovery shadow file,
        // NOT file B; explicit Cmd/Ctrl+S writes file B and clears recovery file.
        editor.store.ui.autosave_workspace_files = false;
        editor.open_workspace_file(file_b.clone(), true);
        let b_rec = editor.active_doc_ref().unwrap().recovery_path();
        editor.active_working_edit_mut().text = "Manual B edit".into();
        editor.changed_active(&mut tx);
        editor.flush_autosave_buffers();
        drain_until_idle(&mut editor);
        assert_eq!(fs_io::read_utf8(&file_b).unwrap(), "Original B");
        assert_eq!(fs_io::read_utf8(&b_rec).unwrap(), "Manual B edit");
        assert!(editor.active_doc_ref().unwrap().is_dirty());

        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert_eq!(fs_io::read_utf8(&file_b).unwrap(), "Manual B edit");
        assert!(!b_rec.exists());
        assert!(!editor.active_doc_ref().unwrap().is_dirty());

        drop(editor);
        unsafe {
            if let Some(v) = prev_home {
                std::env::set_var("GHOSTLESS_HOME", v);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn section_inference_uses_frontmatter_until_overridden() {
        let doc = DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/draft.md"),
            "draft.md".into(),
            "---\nsection: notes\n---\nHello.\n".into(),
            false,
            false,
            true,
            None,
        );
        assert_eq!(section_for_document(&doc), "notes");
        let mut picked = doc;
        picked.section_override = Some("all".into());
        assert_eq!(section_for_document(&picked), "all");
        let manifest = DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/manifest.json"),
            "manifest.json".into(),
            "{}".into(),
            true,
            false,
            true,
            None,
        );
        assert!(!judge_target(&manifest));
    }

    #[test]
    fn rapid_edits_coalesce_into_one_judge_and_stale_replies_are_dropped() {
        use crate::bridge::{BridgeBackend, BridgeReply, JudgeReport};
        use serde_json::{Value, json};

        struct RecordBackend {
            calls: Arc<Mutex<Vec<String>>>,
        }
        impl BridgeBackend for RecordBackend {
            fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
                self.calls.lock().unwrap().push(params["text"].as_str().unwrap_or(method).to_owned());
                Ok(json!({
                    "ok": true, "score": 86, "failUnder": 70, "section": "notes",
                    "checks": [{"metric": "medianSentenceLength", "label": "Median sentence", "target": 10.0, "actual": 8.0, "low": 6.0, "high": 14.0, "status": "low", "penalty": 4}, {"metric": "emDashesPer1k", "label": "Em dashes / 1k", "target": 1.0, "actual": 1.0, "low": 0.0, "high": 2.0, "status": "ok", "penalty": 0}],
                    "needsBuild": false
                }))
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut session = test_session();
        session.after = "---\nsection: notes\n---\nFirst.\n".into();
        let mut editor = Editor::with_handlers_backend(
            session,
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
            Some(Box::new(RecordBackend { calls: Arc::clone(&calls) })),
        );
        let mut tx = Tx::default();
        editor.set_judge_section("archive".into(), 0.0);
        assert_eq!(editor.documents[0].section_override.as_deref(), Some("archive"));
        for text in ["---\nsection: notes\n---\nOne.\n", "---\nsection: notes\n---\nTwo.\n", "---\nsection: notes\n---\nThree.\n"] {
            editor.active_working_edit_mut().text = text.into();
            editor.changed_active(&mut tx);
        }
        assert_eq!(editor.bridge.submitted.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(!poll(&mut editor, 1.0, &mut tx) || editor.worker_busy.is_some());
        assert_eq!(editor.bridge.submitted.load(std::sync::atomic::Ordering::Relaxed), 0);
        poll(&mut editor, 1.2, &mut tx);
        assert_eq!(editor.bridge.submitted.load(std::sync::atomic::Ordering::Relaxed), 1);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while editor.active_judge_report().is_none() {
            assert!(std::time::Instant::now() < deadline, "judge reply did not arrive");
            poll(&mut editor, 1.2, &mut tx);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(calls.lock().unwrap().as_slice(), ["---\nsection: notes\n---\nThree.\n"]);
        assert_eq!(editor.active_judge_report().unwrap().score, 86);
        assert_eq!(editor.voice_pill_label().as_deref(), Some("Voice: 86 · 1 gap"));
        let doc_id = editor.documents[0].id.clone();
        editor.apply_bridge_reply(BridgeReply::Judge {
            request_id: 0,
            doc_id: doc_id.clone(),
            generation: 1,
            section: "notes".into(),
            result: Box::new(JudgeReport { score: 1, ..JudgeReport::default() }),
        });
        assert_eq!(editor.active_judge_report().unwrap().score, 86);

        editor.active_working_edit_mut().text = "---\nsection: notes\n---\nFour.\n".into();
        editor.changed_active(&mut tx);
        let superseded = editor.documents[0].generation - 1;
        editor.apply_bridge_reply(BridgeReply::Judge {
            request_id: 0,
            doc_id,
            generation: superseded,
            section: "notes".into(),
            result: Box::new(JudgeReport { score: 2, ..JudgeReport::default() }),
        });
        assert_eq!(editor.active_judge_report().unwrap().score, 86);
    }

    #[test]
    fn edit_during_judge_drops_the_superseded_reply() {
        use crate::bridge::BridgeBackend;
        use serde_json::{Value, json};
        use std::sync::Condvar;

        struct HoldBackend {
            calls: Arc<std::sync::atomic::AtomicUsize>,
            started: mpsc::Sender<()>,
            release: Arc<(Mutex<bool>, Condvar)>,
        }
        impl BridgeBackend for HoldBackend {
            fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, String> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = self.started.send(());
                let (lock, cv) = &*self.release;
                let _guard = cv.wait_timeout_while(
                    lock.lock().unwrap(),
                    Duration::from_secs(2),
                    |ready| !*ready,
                ).unwrap();
                Ok(json!({"ok": true, "score": 40, "failUnder": 70, "needsBuild": false}))
            }
        }

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let mut editor = Editor::with_handlers_backend(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
            Some(Box::new(HoldBackend { calls: Arc::clone(&calls), started: started_tx, release: Arc::clone(&release) })),
        );
        editor.judge_armed = false;
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "Before the reply.\n".into();
        editor.changed_active(&mut tx);
        poll(&mut editor, 1.2, &mut tx);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        editor.active_working_edit_mut().text = "Edited while judging.\n".into();
        editor.changed_active(&mut tx);
        {
            let (lock, cv) = &*release;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while editor.judge_inflight.is_some() {
            assert!(std::time::Instant::now() < deadline, "judge did not settle");
            poll(&mut editor, 1.0, &mut tx);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(editor.active_judge_report().is_none());
        assert!(editor.judge_due.is_some());
    }

    struct ScriptedHold {
        scores: Vec<i32>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        started: mpsc::Sender<usize>,
        release: Arc<(Mutex<usize>, std::sync::Condvar)>,
    }

    impl crate::bridge::BridgeBackend for ScriptedHold {
        fn call(&mut self, _method: &str, params: &serde_json::Value) -> Result<serde_json::Value, String> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = self.started.send(n);
            let (lock, cv) = &*self.release;
            let _guard = cv
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |ready| *ready <= n)
                .unwrap();
            let score = self.scores.get(n).copied().unwrap_or(1);
            let section = params.get("section").and_then(|value| value.as_str()).unwrap_or("all");
            Ok(serde_json::json!({
                "ok": true, "score": score, "failUnder": 70, "section": section, "needsBuild": false
            }))
        }
    }

    fn release_holds(release: &Arc<(Mutex<usize>, std::sync::Condvar)>, ready: usize) {
        let (lock, cv) = &**release;
        *lock.lock().unwrap() = ready;
        cv.notify_all();
    }

    fn wait_for_judge_score(editor: &mut Editor, score: i32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut tx = Tx::default();
        while editor.active_judge_report().map(|report| report.score) != Some(score) {
            assert!(std::time::Instant::now() < deadline, "judge score {score} did not land");
            poll(editor, 10.0, &mut tx);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn section_switch_during_judge_keeps_the_new_section_score() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
        let mut editor = Editor::with_handlers_backend(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
            Some(Box::new(ScriptedHold {
                scores: vec![11, 91],
                calls: Arc::clone(&calls),
                started: started_tx,
                release: Arc::clone(&release),
            })),
        );
        editor.judge_armed = false;
        let mut tx = Tx::default();
        editor.request_judge(0.0);
        poll(&mut editor, JUDGE_DELAY.as_secs_f64(), &mut tx);
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        editor.set_judge_section("essays".into(), JUDGE_DELAY.as_secs_f64());
        assert!(!editor.documents[0].is_dirty());
        assert!(editor.active_judge_report().is_none());
        poll(&mut editor, (JUDGE_DELAY + JUDGE_DELAY).as_secs_f64(), &mut tx);
        assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 2);
        assert_eq!(editor.judge_inflight.as_ref().map(|inflight| inflight.section.as_str()), Some("essays"));
        release_holds(&release, 2);
        wait_for_judge_score(&mut editor, 91);
        assert_eq!(editor.documents[0].cached_judge.as_ref().map(|cached| cached.report.score), Some(91));
        assert_eq!(editor.voice_pill_label().as_deref(), Some("Voice: 91"));
        assert!(!editor.documents[0].is_dirty());
        assert!(editor.active_judge_report().is_some_and(|report| report.score != 11));
    }

    #[test]
    fn aba_section_switch_and_rebuild_drop_stale_judge_replies() {
        use crate::bridge::BuildSummary;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
        let mut editor = Editor::with_handlers_backend(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
            Some(Box::new(ScriptedHold {
                scores: vec![11, 22, 33],
                calls: Arc::clone(&calls),
                started: started_tx,
                release: Arc::clone(&release),
            })),
        );
        editor.judge_armed = false;
        let mut tx = Tx::default();
        editor.request_judge(0.0);
        let delay = JUDGE_DELAY.as_secs_f64();
        poll(&mut editor, delay, &mut tx);
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        editor.set_judge_section("essays".into(), delay);
        poll(&mut editor, delay * 2.0, &mut tx);
        editor.set_judge_section("all".into(), delay * 2.0);
        poll(&mut editor, delay * 3.0, &mut tx);
        assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 3);
        assert!(!editor.documents[0].is_dirty());
        release_holds(&release, 3);
        wait_for_judge_score(&mut editor, 33);
        assert_eq!(editor.active_judge_report().map(|report| report.score), Some(33));
        assert!(!editor.documents[0].is_dirty());

        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
        let mut editor = Editor::with_handlers_backend(
            test_session(),
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
            Some(Box::new(ScriptedHold {
                scores: vec![11, 44],
                calls,
                started: started_tx,
                release: Arc::clone(&release),
            })),
        );
        editor.judge_armed = false;
        editor.request_judge(0.0);
        poll(&mut editor, delay, &mut tx);
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        editor.apply_bridge_reply(crate::bridge::BridgeReply::Build {
            doc_id: Some(editor.documents[0].id.clone()),
            result: Ok(BuildSummary {
                gold_pieces: 1,
                counts: serde_json::Value::Null,
                bronze_format_only: false,
                warnings: Vec::new(),
                generated_at: String::new(),
            }),
        });
        assert_eq!(editor.documents[0].judge_epoch, 1);
        assert!(editor.documents[0].latest_judge_request.is_none());
        assert!(editor.active_judge_report().is_none());
        assert!(editor.judge_inflight.is_none());
        assert!(!editor.documents[0].is_dirty());
        poll(&mut editor, delay, &mut tx);
        poll(&mut editor, delay * 2.0, &mut tx);
        assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 2);
        release_holds(&release, 2);
        wait_for_judge_score(&mut editor, 44);
        assert!(!editor.documents[0].is_dirty());
    }

    fn plant_judge(editor: &mut Editor, section: &str, score: i32) {
        let voice = editor.voice_project.clone().expect("voice project");
        let doc = editor.active_doc_mut().unwrap();
        let epoch = doc.judge_epoch;
        doc.cached_judge = Some(CachedJudge {
            voice_project: voice,
            judge_epoch: epoch,
            section: section.into(),
            report: crate::bridge::JudgeReport {
                ok: true,
                score,
                section: section.into(),
                banned: vec![crate::bridge::BannedHit { tell: "delve".into(), count: 1 }],
                ..crate::bridge::JudgeReport::default()
            },
        });
    }

    fn element_mentions(elements: &[rune::ElementInfo], needle: &str) -> bool {
        elements.iter().filter(|element| !element.exiting).any(|element| {
            element.text.as_deref().is_some_and(|text| text.contains(needle))
                || element.label.as_deref().is_some_and(|text| text.contains(needle))
        })
    }

    #[test]
    fn yaml_section_edit_drops_the_cached_judge_across_tab_switches() {
        let essays = "---\nsection: essays\n---\nPlease delve here.\n";
        let mut session = test_session();
        session.after = essays.into();
        session.original = essays.into();
        let mut editor = Editor::with_handlers(session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.store.ui.voice_lens = true;
        if !editor.inspector_pane.is_open() {
            editor.inspector_pane.toggle();
        }
        plant_judge(&mut editor, "essays", 77);
        assert_eq!(editor.active_judge_report().map(|report| report.score), Some(77));
        let mut rt = rune::Runtime::new(editor, crate::ui::view, (1280.0, 800.0), rune::Env::new(false, 1.0));
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        assert!(element_mentions(&rt.elements(), "Score 77"));
        assert!(element_mentions(&rt.elements(), "Voice: 77"));
        rt.update(|editor, tx| {
            editor.active_working_edit_mut().text = "---\nsection: notes\n---\nPlease delve here.\n".into();
            editor.changed_active(tx);
        });
        assert!(rt.state.active_judge_report().is_none());
        assert!(rt.state.documents[0].cached_judge.is_none());
        rt.frame(1.0 / 60.0);
        assert!(!element_mentions(&rt.elements(), "Score 77"));
        assert!(!element_mentions(&rt.elements(), "Voice: 77"));
        let draft_key = rt.state.documents[0].id.tab_key();
        rt.update(|editor, tx| {
            let other = DocumentBuffer::from_workspace_file(
                PathBuf::from("/tmp/other-judge.md"),
                "other-judge.md".into(),
                "Other note.\n".into(),
                false,
                false,
                true,
                editor.voice_project.clone(),
            );
            let other_key = other.id.tab_key();
            editor.documents.push(other);
            editor.activate_tab(&other_key, tx);
        });
        assert!(rt.state.active_judge_report().is_none());
        rt.update(|editor, tx| editor.activate_tab(&draft_key, tx));
        assert!(rt.state.active_judge_report().is_none());
        assert!(rt.state.documents.iter().find(|doc| doc.id.tab_key() == draft_key).unwrap().cached_judge.is_none());
        rt.frame(0.0);
        assert!(!element_mentions(&rt.elements(), "Score 77"));
        assert!(!element_mentions(&rt.elements(), "Voice: 77"));
    }

    #[test]
    fn judge_report_does_not_leak_across_close_delete_workspace_or_recovery() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-judge-life-home-{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("ghostless-judge-life-{}", std::process::id()));
        let other = std::env::temp_dir().join(format!("ghostless-judge-life-other-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&other).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let mut tx = Tx::default();

        let essays = "---\nsection: essays\n---\nPlease delve here.\n";
        let mut session = test_session();
        session.after = essays.into();
        session.original = essays.into();
        let mut editor = Editor::with_handlers(session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        plant_judge(&mut editor, "essays", 77);
        let key = editor.documents[0].id.tab_key();
        assert!(editor.close_tab_by_id(&key, &mut tx));
        assert!(editor.documents.is_empty());
        assert!(editor.active_judge_report().is_none());

        let draft = root.join("draft.md");
        let keep = root.join("keep.md");
        fs_io::atomic_write_text(&draft, essays).unwrap();
        fs_io::atomic_write_text(&keep, "Keep this.\n").unwrap();
        let mut editor = Editor::empty_with_handlers(prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.open_workspace_with_voice(root.clone(), Some(draft.clone()), Some(root.clone()));
        plant_judge(&mut editor, "essays", 77);
        let mut linked = DocumentBuffer::from_session(test_session(), true, Some(draft.clone()));
        linked.mirror_path = Some(draft.canonicalize().unwrap_or(draft.clone()));
        editor.documents.push(linked);
        editor.active_dialog = Some(DialogKind::Delete {
            rel_path: "draft.md".into(),
            display_name: "draft.md".into(),
            is_dir: false,
        });
        editor.submit_dialog(&mut tx);
        assert!(editor.active_judge_report().is_none());
        assert!(editor.documents.iter().all(|doc| doc.cached_judge.is_none()));
        let linked = editor.documents.iter().find(|doc| doc.is_session()).unwrap();
        assert!(linked.mirror_path.is_none());

        editor.open_workspace_file(keep.clone(), true);
        plant_judge(&mut editor, "all", 77);
        assert_eq!(editor.active_judge_report().map(|report| report.score), Some(77));
        editor.open_workspace_with_voice(other.clone(), None, Some(other.clone()));
        assert!(editor.active_judge_report().is_none());
        assert!(editor.judge_inflight.is_none());
        assert!(editor.judge_due.is_none());
        assert!(!editor.conflict_compare);
        assert!(editor.documents.iter().all(|doc| doc.cached_judge.is_none()));

        let recover = root.join("recover.md");
        fs_io::atomic_write_text(&recover, essays).unwrap();
        editor.open_workspace_with_voice(root.clone(), Some(recover.clone()), Some(root.clone()));
        plant_judge(&mut editor, "essays", 77);
        let recovery = editor.active_doc_ref().unwrap().recovery_path();
        fs_io::atomic_write_text(&recovery, "---\nsection: essays\n---\nRecovered body.\n").unwrap();
        let doc_id = editor.active_doc_ref().unwrap().id.clone();
        let title = editor.active_doc_ref().unwrap().title.clone();
        editor.active_dialog = Some(DialogKind::Recover { doc_id, title });
        editor.resolve_recovery(true);
        assert!(editor.active_doc_ref().unwrap().cached_judge.is_none());
        assert!(editor.active_judge_report().is_none());
        assert_eq!(editor.active_working_edit().text, "---\nsection: essays\n---\nRecovered body.\n");

        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }

    #[test]
    fn voice_navigation_keeps_text_and_returns_focus_in_every_mode() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-lens-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }

        let draft = "# Opening\n\nKeep [PLACEHOLDER: one] then [PLACEHOLDER: two].\n\nPlease delve here.\n";
        let cases = [
            (ViewMode::Write, false, false, false),
            (ViewMode::Preview, false, false, false),
            (ViewMode::Compare, false, false, false),
            (ViewMode::Write, true, false, false),
            (ViewMode::Preview, true, false, false),
            (ViewMode::Compare, true, false, false),
            (ViewMode::Write, false, true, false),
            (ViewMode::Preview, false, true, false),
            (ViewMode::Compare, false, true, false),
            (ViewMode::Write, true, false, true),
            (ViewMode::Preview, true, false, true),
            (ViewMode::Compare, true, false, true),
        ];
        for (mode, read_only, locked, voice_guide) in cases {
            let mut session = test_session();
            session.after = draft.into();
            session.original = draft.into();
            let mut editor = Editor::with_handlers(
                session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
            );
            {
                let doc = editor.active_doc_mut().unwrap();
                doc.read_only = read_only || voice_guide;
                doc.lockable = locked;
                doc.user_locked = locked;
                if voice_guide {
                    doc.path.set_file_name("voice-guide.md");
                    doc.title = "voice-guide.md".into();
                }
            }
            editor.view_mode = mode;
            editor.store.ui.voice_lens = false;
            let generation = editor.active_doc_ref().unwrap().generation;
            let focus = if voice_guide {
                "preview-body"
            } else if read_only || locked {
                "working-body"
            } else {
                "prose-input"
            };

            let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
            rt.on_key = Some(on_key);
            rt.poll = Some(poll);
            rt.frame(0.0);
            if mode == ViewMode::Write && !read_only && !locked && !voice_guide {
                assert!(rt.elements().iter().any(|element| element.key.as_deref() == Some("next-placeholder")));
            }
            rt.key(KeyEvent { key: Key::Function(8), mods: rune::Mods::default() });
            rt.frame(0.0);
            assert_eq!(rt.state.active_working_edit().text, draft, "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}");
            assert_eq!(rt.state.active_doc_ref().unwrap().generation, generation);
            assert_eq!(rt.state.active_working_edit().selected(), "[PLACEHOLDER: one]");
            assert_eq!(rt.state.view_mode, ViewMode::Write);
            assert_surface(&rt, focus);
            rt.key(KeyEvent { key: Key::Command('l'), mods: rune::Mods::default() });
            rt.frame(0.0);
            assert!(rt.state.store.ui.voice_lens);
            assert_eq!(rt.state.active_working_edit().text, draft);
            assert_surface(&rt, focus);
            rt.update(|editor, tx| editor.jump_to_query("delve", tx));
            rt.frame(0.0);
            assert_eq!(rt.state.active_working_edit().selected(), "delve");
            assert_eq!(rt.state.active_working_edit().text, draft);
            assert_eq!(rt.state.active_doc_ref().unwrap().generation, generation);
            let focused = rt.elements().iter().find(|element| element.focused).and_then(|element| element.key.clone());
            assert_eq!(focused.as_deref(), Some(focus), "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}");
        }

        let mut session = test_session();
        session.after = draft.into();
        let editor = Editor::with_handlers(
            session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake(),
        );
        let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
        rt.on_key = Some(on_key);
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt.update(|editor, tx| editor.jump_next_placeholder(tx));
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().selected(), "[PLACEHOLDER: one]");
        rt.key(KeyEvent { key: Key::Command('.'), mods: rune::Mods { shift: true, ..rune::Mods::default() } });
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().selected(), "[PLACEHOLDER: two]");
        rt.key(KeyEvent { key: Key::Function(8), mods: rune::Mods::default() });
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().selected(), "[PLACEHOLDER: one]");
        assert_eq!(rt.state.active_working_edit().text, draft);

        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = std::fs::remove_dir_all(home);
    }

    fn assert_surface(rt: &rune::Runtime<Editor>, focus: &str) {
        let elements = rt.elements();
        let focused = elements.iter().find(|element| element.focused).unwrap_or_else(|| {
            panic!("nothing focused; wanted {focus}");
        });
        assert_eq!(focused.key.as_deref(), Some(focus));
        let rect = focused.rect;
        assert!(rect[2] > 1.0 && rect[3] > 1.0, "focus {focus} has no area {rect:?}");
        assert!(rect[0] >= -1.0 && rect[1] >= -1.0 && rect[0] + rect[2] <= 1121.0 && rect[1] + rect[3] <= 801.0, "{focus} {:?} is outside the window", rect);
        if let Some(parent) = elements.iter().find(|element| element.key.as_deref() == Some("working-body")) {
            assert!(rect[1] >= parent.rect[1] - 1.0 && rect[1] + rect[3] <= parent.rect[1] + parent.rect[3] + 1.0, "{focus} {:?} outside working body {:?}", rect, parent.rect);
        }
        for stray in ["find-source-match", "find-source-anchor", "preview-find-anchor"] {
            assert!(elements.iter().all(|element| element.key.as_deref() != Some(stray)), "{stray} left behind");
        }
    }

    #[test]
    fn linked_reopen_detects_divergence_without_writing_either_copy() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-conflict-home-{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("ghostless-conflict-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let session_id = "2026-04-02-notes";
        let folder = root.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let before = "Session copy.\n";
        let before_sha = fs_io::sha256_text(before);
        fs_io::atomic_write_text(&folder.join("before.md"), before).unwrap();
        fs_io::atomic_write_text(&folder.join("after.md"), before).unwrap();
        fs_io::atomic_write_text(&folder.join("session.json"), &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#)).unwrap();
        let draft = root.join("draft.md");
        fs_io::atomic_write_text(&draft, before).unwrap();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&root, session_id), prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        });
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
        editor.workspace = Some(root.clone());
        editor.voice_project = Some(root.clone());
        editor.open_workspace_file(draft.clone(), true);
        assert!(editor.active_doc_ref().unwrap().conflict.is_none());
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), before);
        assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), before);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), before);

        fs_io::atomic_write_text(&draft, "Disk edit.\n").unwrap();
        let store = prefs::load_store();
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
        editor.workspace = Some(root.clone());
        editor.voice_project = Some(root.clone());
        editor.open_workspace_file(draft.clone(), true);
        let doc = editor.active_doc_ref().unwrap();
        assert!(doc.conflict.is_some());
        assert_eq!(doc.edit.text, before);
        assert_eq!(doc.conflict.as_ref().unwrap().disk_text, "Disk edit.\n");
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), "Disk edit.\n");
        assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), before);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), before);

        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn diverged_linked_open_stays_clean_until_keep_session_mirrors() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-conflict-save-home-{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("ghostless-conflict-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let session_id = "2026-04-02-notes";
        let folder = root.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let session_copy = "Session copy.\n";
        let disk_text = "Disk edit.\n";
        let before_sha = fs_io::sha256_text(session_copy);
        fs_io::atomic_write_text(&folder.join("before.md"), session_copy).unwrap();
        fs_io::atomic_write_text(&folder.join("after.md"), session_copy).unwrap();
        fs_io::atomic_write_text(&folder.join("session.json"), &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#)).unwrap();
        let draft = root.join("draft.md");
        fs_io::atomic_write_text(&draft, disk_text).unwrap();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&root, session_id), prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        });
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), Box::new(crate::worker::execute_write_op), noop_wake());
        editor.workspace = Some(root.clone());
        editor.voice_project = Some(root.clone());
        editor.open_workspace_file(draft.clone(), true);
        {
            let doc = editor.active_doc_ref().unwrap();
            assert!(!doc.is_dirty());
            assert_eq!(doc.edit.text, session_copy);
            assert_eq!(doc.conflict.as_ref().unwrap().disk_text, disk_text);
        }
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), disk_text);

        let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        let elements = rt.elements();
        assert!(elements.iter().any(|element| element.text.as_deref() == Some("Disk conflict — resolve above")));
        assert!(elements.iter().any(|element| element.key.as_deref() == Some("keep-session")));
        rt.key(KeyEvent { key: Key::Command('s'), mods: rune::Mods::default() });
        rt.frame(0.0);
        rt.update(|editor, _| editor.save_clicked());
        assert_eq!(
            rt.state.status,
            "Resolve the disk conflict (Load Disk or Keep Session) before syncing to disk.",
        );
        assert!(!rt.state.status.contains("Saved locally"));
        assert!(!rt.state.active_doc_ref().unwrap().is_dirty());
        assert!(rt.state.worker_busy.is_none());
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), disk_text);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), session_copy);

        let [x, y, w, h] = rt.elements().iter().find(|element| element.key.as_deref() == Some("keep-session")).unwrap().rect;
        rt.pointer_down(x + w / 2.0, y + h / 2.0);
        rt.pointer_up(x + w / 2.0, y + h / 2.0);
        rt.frame(0.0);
        assert!(rt.state.active_doc_ref().unwrap().conflict.is_none());
        assert_eq!(rt.state.worker_busy.as_ref().map(|(_, kind)| *kind), Some(InFlight::Save));
        assert!(!rt.state.active_doc_ref().unwrap().is_dirty());
        rt.update(|editor, _| drain_until_idle(editor));
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), session_copy);
        assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), session_copy);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), session_copy);

        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn compare_reverts_are_undoable_and_refuse_locked_surfaces() {
        let original = "Alpha paragraph.\n\nMiddle paragraph.\n\nOmega paragraph.\n";
        let edited = "Alpha changed.\n\nMiddle edited.\n\nOmega changed.\n";
        let mut session = test_session();
        session.original = original.into();
        session.after = edited.into();
        let mut editor = Editor::with_handlers(session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.view_mode = ViewMode::Compare;
        editor.refresh_diff();
        let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
        rt.on_key = Some(on_key);
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt.update(|editor, tx| editor.revert_hunk(1, tx));
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().text, "Alpha changed.\n\nMiddle paragraph.\n\nOmega changed.\n");
        rt.update(|editor, _| editor.active_working_edit_mut().undo());
        assert_eq!(rt.state.active_working_edit().text, edited);
        rt.update(|editor, tx| editor.revert_to_original(tx));
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().text, original);
        rt.update(|editor, _| editor.active_working_edit_mut().undo());
        assert_eq!(rt.state.active_working_edit().text, edited);
        assert_surface(&rt, "compare-body");

        for (mode, read_only, locked, voice_guide) in [
            (ViewMode::Write, false, false, false),
            (ViewMode::Preview, false, false, false),
            (ViewMode::Compare, true, false, false),
            (ViewMode::Compare, false, true, false),
            (ViewMode::Compare, true, false, true),
        ] {
            let mut session = test_session();
            session.original = original.into();
            session.after = edited.into();
            let mut editor = Editor::with_handlers(session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
            {
                let doc = editor.active_doc_mut().unwrap();
                doc.read_only = read_only || voice_guide;
                doc.lockable = locked;
                doc.user_locked = locked;
                if voice_guide { doc.path.set_file_name("voice-guide.md"); }
            }
            editor.view_mode = mode;
            let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
            rt.on_key = Some(on_key);
            rt.frame(0.0);
            rt.update(|editor, tx| {
                editor.revert_hunk(1, tx);
                editor.revert_to_original(tx);
            });
            rt.frame(0.0);
            assert_eq!(rt.state.active_working_edit().text, edited, "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}");
            let elements = rt.elements();
            for stray in ["find-source-match", "find-source-anchor", "preview-find-anchor"] {
                assert!(elements.iter().all(|element| element.key.as_deref() != Some(stray)));
            }
        }
    }

    #[test]
    fn conflict_compare_uses_the_disk_text_and_refuses_revert() {
        let original = "Baseline only.\n";
        let edited = "one two three\n";
        let disk = "alpha beta gamma delta epsilon\n";
        let mut session = test_session();
        session.original = original.into();
        session.after = edited.into();
        let mut editor = Editor::with_handlers(session, prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.active_doc_mut().unwrap().conflict = Some(crate::documents::SessionConflict {
            disk_text: disk.into(),
            session_text: edited.into(),
        });
        editor.conflict_compare = true;
        editor.view_mode = ViewMode::Compare;
        editor.refresh_diff();
        assert!(!editor.can_revert());
        let mut rt = rune::Runtime::new(editor, crate::ui::view, (1120.0, 800.0), rune::Env::new(false, 1.0));
        rt.on_key = Some(on_key);
        rt.poll = Some(poll);
        rt.frame(0.0);
        let elements = rt.elements();
        let summary = elements.iter().find_map(|element| {
            element.text.as_deref().filter(|text| text.contains("Disk vs session"))
        }).expect("disk compare summary");
        assert!(summary.contains("Words: 5 → 3 (-2)"), "{summary}");
        assert!(summary.contains("Changed hunks: 1"), "{summary}");
        assert!(elements.iter().all(|element| element.key.as_deref() != Some("revert-original")));
        assert!(elements.iter().all(|element| !element.key.as_deref().unwrap_or("").starts_with("revert-hunk")));
        let before = rt.state.active_working_edit().text.clone();
        rt.update(|editor, tx| {
            editor.revert_hunk(0, tx);
            editor.revert_to_original(tx);
        });
        assert_eq!(rt.state.active_working_edit().text, before);
        assert!(!rt.state.can_revert());
        rt.update(|editor, tx| editor.set_view_mode(ViewMode::Write, tx));
        assert!(!rt.state.conflict_compare);
        rt.update(|editor, tx| {
            editor.conflict_compare = true;
            editor.set_view_mode(ViewMode::Compare, tx);
            let other = DocumentBuffer::from_workspace_file(
                PathBuf::from("/tmp/other.md"), "other.md".into(), "Other note.\n".into(), false, false, true, None,
            );
            let key = other.id.tab_key();
            editor.documents.push(other);
            editor.activate_tab(&key, tx);
        });
        assert!(!rt.state.conflict_compare);
    }

    #[test]
    fn reactivating_a_linked_tab_keeps_unsaved_text_and_shows_external_drift() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-reactivate-home-{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("ghostless-reactivate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let session_id = "2026-04-02-notes";
        let folder = root.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let session_text = "Session copy.\n";
        let before_sha = fs_io::sha256_text(session_text);
        fs_io::atomic_write_text(&folder.join("before.md"), session_text).unwrap();
        fs_io::atomic_write_text(&folder.join("after.md"), session_text).unwrap();
        fs_io::atomic_write_text(&folder.join("session.json"), &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#)).unwrap();
        let draft = root.join("draft.md");
        fs_io::atomic_write_text(&draft, session_text).unwrap();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&root, session_id), prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        });
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
        editor.workspace = Some(root.clone());
        editor.voice_project = Some(root.clone());
        editor.open_workspace_file(draft.clone(), true);
        let other = DocumentBuffer::from_workspace_file(
            root.join("other.md"), "other.md".into(), "Other note.\n".into(), false, false, true, None,
        );
        let other_key = other.id.tab_key();
        let draft_key = editor.active_doc_ref().unwrap().id.tab_key();
        editor.documents.push(other);
        fs_io::atomic_write_text(&draft, "External clean.\n").unwrap();
        let mut tx = Tx::default();
        editor.activate_tab(&other_key, &mut tx);
        editor.activate_tab(&draft_key, &mut tx);
        let doc = editor.active_doc_ref().unwrap();
        assert_eq!(doc.edit.text, session_text);
        assert_eq!(doc.conflict.as_ref().map(|conflict| conflict.disk_text.as_str()), Some("External clean.\n"));
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), "External clean.\n");
        assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), session_text);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), session_text);

        let unsaved = "Unsaved words.\n";
        {
            let doc = editor.active_doc_mut().unwrap();
            doc.edit = TextEdit::new(unsaved);
            doc.generation += 1;
        }
        fs_io::atomic_write_text(&draft, "External dirty.\n").unwrap();
        editor.activate_tab(&other_key, &mut tx);
        editor.open_workspace_file(draft.clone(), true);
        let doc = editor.active_doc_ref().unwrap();
        assert_eq!(doc.edit.text, unsaved);
        assert_eq!(doc.conflict.as_ref().map(|conflict| conflict.disk_text.as_str()), Some("External dirty.\n"));
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), "External dirty.\n");
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), session_text);

        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_save_reply_does_not_rewind_the_mirror_hash() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-stale-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let mut editor = Editor::with_handlers(test_session(), prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        let project = editor.voice_project.clone().unwrap();
        let session_id = editor.active_doc_ref().unwrap().session_id.clone().unwrap();
        let key = prefs::EditorStateStore::session_link_key(&project, &session_id);
        editor.store.session_links.insert(key.clone(), prefs::SessionLink {
            workspace_file: PathBuf::from("/tmp/draft.md"),
            last_synced_sha256: "hash-v2".into(),
        });
        let doc_id = {
            let doc = editor.active_doc_mut().unwrap();
            doc.saved_generation = 2;
            doc.generation = 2;
            doc.last_disk_sha256 = "hash-v2".into();
            doc.id.clone()
        };
        editor.status = "Saved locally".into();
        let saved = editor.apply_reply(WorkerReply::Saved {
            doc_id,
            generation: 1,
            sha256: "hash-v1".into(),
            result: Ok(WriteOutcome::Mirrored),
        }, None);
        assert!(!saved);
        let doc = editor.active_doc_ref().unwrap();
        assert_eq!(doc.last_disk_sha256, "hash-v2");
        assert!(doc.save_error.is_none());
        assert_eq!(editor.store.session_links[&key].last_synced_sha256, "hash-v2");
        assert_eq!(editor.status, "Saved locally");
        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn edit_during_save_keeps_the_buffer_dirty_and_the_next_mirror_succeeds() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-edit-save-home-{}", std::process::id()));
        let root = std::env::temp_dir().join(format!("ghostless-edit-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        let session_id = "2026-04-02-notes";
        let folder = root.join("sessions").join(session_id);
        fs::create_dir_all(&folder).unwrap();
        let v0 = "v0\n";
        let v1 = "v1\n";
        let v2 = "v2\n";
        let before_sha = fs_io::sha256_text(v0);
        fs_io::atomic_write_text(&folder.join("before.md"), v0).unwrap();
        fs_io::atomic_write_text(&folder.join("after.md"), v0).unwrap();
        fs_io::atomic_write_text(&folder.join("notes.md"), "").unwrap();
        fs_io::atomic_write_text(&folder.join("session.json"), &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#)).unwrap();
        let draft = root.join("draft.md");
        fs_io::atomic_write_text(&draft, v0).unwrap();
        let mut store = prefs::EditorStateStore::default();
        let key = prefs::EditorStateStore::session_link_key(&root, session_id);
        store.session_links.insert(key.clone(), prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha.clone(),
        });
        let mut session = test_session();
        session.project = root.clone();
        session.id = session_id.into();
        session.original = v0.into();
        session.after = v0.into();
        session.before_sha256 = before_sha.clone();
        let mirrored = crate::worker::write_session_direct(
            &root, session_id, Some(&before_sha), v1, "", Some(&draft), Some(&before_sha), false,
        ).unwrap();
        assert!(matches!(mirrored, WriteOutcome::Mirrored));
        let mut editor = Editor::with_handlers(session, store, ok_runner(), ok_writer(), noop_wake());
        let doc_id = {
            let doc = editor.active_doc_mut().unwrap();
            doc.edit = TextEdit::new(v2);
            doc.generation = 2;
            doc.saved_generation = 0;
            doc.id.clone()
        };
        assert!(editor.apply_reply(WorkerReply::Saved {
            doc_id,
            generation: 1,
            sha256: fs_io::sha256_text(v1),
            result: Ok(WriteOutcome::Mirrored),
        }, None));
        let doc = editor.active_doc_ref().unwrap();
        assert!(doc.is_dirty());
        assert_eq!(doc.edit.text, v2);
        assert_eq!(doc.last_disk_sha256, fs_io::sha256_text(v1));
        assert_eq!(editor.store.session_links[&key].last_synced_sha256, fs_io::sha256_text(v1));
        assert!(!editor.status.contains("Saved locally"));
        let expected = editor.store.session_links[&key].last_synced_sha256.clone();
        let next = crate::worker::write_session_direct(
            &root, session_id, Some(&before_sha), v2, "", Some(&draft), Some(&expected), false,
        ).unwrap();
        assert!(matches!(next, WriteOutcome::Mirrored), "{next:?}");
        assert_eq!(fs_io::read_utf8(&draft).unwrap(), v2);
        assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), v2);
        assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), v0);
        unsafe {
            if let Some(value) = previous { std::env::set_var("GHOSTLESS_HOME", value); }
            else { std::env::remove_var("GHOSTLESS_HOME"); }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_session_start_does_not_attach_or_store_a_link() {
        let mut editor = Editor::empty_with_handlers(prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        let voice = PathBuf::from("/tmp/ghostless-session-start-voice");
        editor.voice_project = Some(voice.clone());
        editor.documents.push(DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/ghostless-session-start-draft.md"),
            "draft.md".into(),
            "Hello\n".into(),
            false,
            false,
            true,
            Some(voice),
        ));
        editor.active_doc = Some(0);
        let doc_id = {
            let doc = editor.active_doc_mut().unwrap();
            doc.edit = TextEdit::new("Hello edited\n");
            doc.generation = 2;
            doc.saved_generation = 1;
            doc.id.clone()
        };
        editor.session_start_inflight = Some((doc_id.clone(), 1));
        editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
            doc_id: Some(doc_id.clone()),
            generation: 1,
            result: Ok(crate::bridge::SessionStartResult {
                id: "2026-04-02-notes".into(),
                path: "sessions/2026-04-02-notes".into(),
                before_sha256: "abc".into(),
                source_draft_name: "draft.md".into(),
                section: None,
                origin: "ai-first".into(),
            }),
        });
        assert!(editor.active_doc_ref().unwrap().session_id.is_none());
        assert!(editor.store.session_links.is_empty());
        assert!(editor.status.contains("changed"));
        assert!(editor.session_start_inflight.is_none());
        editor.status = "Ready".into();
        editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
            doc_id: Some(doc_id),
            generation: 1,
            result: Err("bridge failed".into()),
        });
        assert_eq!(editor.status, "Ready");
        assert!(editor.store.session_links.is_empty());
    }
}
