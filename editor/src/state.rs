use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
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
    documents::{DocId, DocumentBuffer},
    fs_io,
    prefs,
    session::{LaunchTarget, Session, load_session},
    worker::{
        CliRunner, InFlight, WakeFn, Worker, WorkerReply, WorkerRequest, WriteRunner,
        execute_write_op,
    },
    workspace_tree::{self, FileKind, SessionSummary, WorkspaceEntry},
};

pub(crate) const SESSION_AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
pub(crate) const FILE_AUTOSAVE_DELAY: Duration = Duration::from_millis(1500);

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
}

pub(crate) struct Editor {
    pub(crate) documents: Vec<DocumentBuffer>,
    pub(crate) active_doc: Option<usize>,
    pub(crate) fallback_edit: TextEdit,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) voice_project: Option<PathBuf>,
    pub(crate) store: prefs::EditorStateStore,
    pub(crate) sidebar_pane: PaneState,
    pub(crate) inspector_pane: PaneState,
    pub(crate) view_mode: ViewMode,
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
    pub(crate) status: String,
    pub(crate) worker: Worker,
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
        let mirror_path = store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &session.project,
                &session.id,
            ))
            .and_then(|link| link.workspace_file.is_absolute().then(|| link.workspace_file.clone()));
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &session.source_name,
            &session.original,
            &session.after,
        )]);
        diff.set_mode(DiffMode::Split);
        let worker = Worker::spawn_with_handlers(runner, writer, wake);
        let voice = Some(session.project.clone());
        let unresolved_link = store.session_links.get(&prefs::EditorStateStore::session_link_key(&session.project, &session.id))
            .is_some_and(|link| !link.workspace_file.is_absolute());
        let status = if unresolved_link {
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
            voice_project: voice,
            store,
            sidebar_pane,
            inspector_pane,
            view_mode: ViewMode::Write,
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
            status,
            worker,
        }
    }

    fn empty_with_handlers(
        store: prefs::EditorStateStore,
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
    ) -> Self {
        let worker = Worker::spawn_with_handlers(runner, writer, wake);
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
            store,
            sidebar_pane: sb,
            inspector_pane: insp,
            view_mode: ViewMode::Write,
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
            status: "Ready".into(),
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
        self.store.record_workspace(&folder);
        self.workspace = Some(folder.clone());
        self.documents.clear();
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
        self.dispatch_next();
        if self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        }
        if let Some(rel) = rel_id {
            self.selected_tree_id = Some(rel.clone());
            self.file_tree.select(&rel);
        }
        self.save_open_tabs_pref();
    }

    pub(crate) fn open_session_entry(&mut self, project: PathBuf, session_id: String) {
        let id = DocId::Session {
            project: project.clone(),
            id: session_id.clone(),
        };
        if let Some(pos) = self.documents.iter().position(|d| d.id == id) {
            self.documents[pos].pinned = true;
            self.switch_active_doc(pos);
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
            .map(|link| resolve_mirror_path(&link.workspace_file, self.workspace.as_deref()));
        let finished = loaded.finished;
        let buf = DocumentBuffer::from_session(loaded, true, mirror_path);
        self.flush_leaving_doc();
        self.documents.push(buf);
        self.active_doc = Some(self.documents.len() - 1);
        self.refresh_diff();
        self.dispatch_next();
        self.status = if finished {
            "Finished".into()
        } else {
            "Ready".into()
        };
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
            self.flush_leaving_doc();
            self.active_doc = Some(pos);
            self.dispatch_next();
        }
        if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        } else if self.view_mode == ViewMode::Compare {
            self.refresh_diff();
        }
    }

    pub(crate) fn activate_tab(&mut self, tab_key: &str, tx: &mut Tx) {
        if let Some(pos) = self.documents.iter().position(|d| d.id.tab_key() == tab_key) {
            self.switch_active_doc(pos);
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
            && doc.is_editable()
        {
            self.active_dialog = Some(DialogKind::ConfirmClose {
                doc_id: doc.id.clone(),
                title: doc.title.clone(),
            });
            self.dialog_error = None;
            return false;
        }
        let doc_id = doc.id.clone();
        if doc.is_editable() && doc.is_dirty() {
            if !self.drain_single_doc_save(&doc_id) {
                return false;
            }
        } else if self.worker_busy.as_ref().is_some_and(|(id, _)| id == &doc_id) {
            self.drain_until_idle();
        }
        self.remove_doc_at(pos);
        self.sync_window_title(tx);
        true
    }

    fn remove_doc_at(&mut self, pos: usize) {
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
        if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        } else if self.view_mode == ViewMode::Compare {
            self.refresh_diff();
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
        self.close_dialog();
        if let Some(pos) = self.documents.iter().position(|d| d.id == doc_id) {
            let _ = fs::remove_file(self.documents[pos].recovery_path());
            if self.pending_close.as_ref() == Some(&doc_id) {
                self.pending_close = None;
            }
            self.remove_doc_at(pos);
            self.sync_window_title(tx);
        }
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

    pub(crate) fn submit_dialog(&mut self, tx: &mut Tx) {
        let Some(dialog) = self.active_dialog.clone() else {
            return;
        };
        if matches!(dialog, DialogKind::ConfirmClose { .. }) {
            self.confirm_close_save();
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
                if self.documents.iter().any(|doc| {
                    !doc.is_session() && doc.path.starts_with(&target_canon)
                        && (doc.is_dirty() || self.worker_busy.as_ref().is_some_and(|(id, _)| id == &doc.id))
                }) {
                    self.dialog_error = Some("Save or close unsaved files before deleting them.".into());
                    return;
                }
                match workspace_tree::delete_entry(&root, &rel_path) {
                    Ok(()) => {
                        self.documents
                            .retain(|d| d.is_session() || !d.path.starts_with(&target_canon));
                        if let Some(idx) = self.active_doc
                            && idx >= self.documents.len()
                        {
                            self.active_doc =
                                (!self.documents.is_empty()).then_some(self.documents.len() - 1);
                        }
                        self.selected_tree_id = None;
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                        self.sync_window_title(tx);
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::ConfirmClose { .. } => unreachable!(),
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
    }

    pub(crate) fn refresh_diff(&mut self) {
        let Some(doc) = self.active_doc_ref() else {
            self.diff = DiffState::new(Vec::new());
            return;
        };
        let Some(original) = &doc.original else {
            self.diff = DiffState::new(Vec::new());
            return;
        };
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &doc.title,
            original,
            &doc.edit.text,
        )]);
        diff.set_mode(DiffMode::Split);
        self.diff = diff;
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
            if doc.is_editable() && doc.is_dirty() {
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
                        Ok(()) => {
                            doc.saved_generation = doc.saved_generation.max(generation);
                            doc.last_disk_sha256 = sha256;
                            doc.save_error = None;
                            if doc.generation > doc.saved_generation {
                                doc.pending_save = true;
                            } else {
                                doc.last_saved_label = Some(DocumentBuffer::format_saved_utc());
                                let missing_mirror = doc.mirror_path.is_none() && doc.voice_project.as_ref().zip(doc.session_id.as_ref()).is_some_and(|(project, id)| {
                                    self.store.session_links.get(&prefs::EditorStateStore::session_link_key(project, id)).is_some_and(|link| !link.workspace_file.is_absolute())
                                });
                                self.status = if missing_mirror { "Saved locally; linked file unavailable without a workspace" } else { "Saved locally" }.into();
                                if self.pending_close.as_ref() == Some(&doc_id) {
                                    should_close = true;
                                }
                            }
                            saved_ok = true;
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
                Ok(()) => {
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
                saved_snapshot,
                after_sha256,
                result,
            } => {
                let mut finished_ok = false;
                if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                    if saved_snapshot {
                        doc.saved_generation = doc.saved_generation.max(generation);
                        if let Some(sha) = after_sha256 {
                            doc.last_disk_sha256 = sha;
                        }
                    }
                    doc.finish_requested = false;
                    match result {
                        Ok(()) => {
                            doc.finished = true;
                            doc.read_only = true;
                            doc.pending_save = false;
                            doc.autosave_due = None;
                            doc.save_error = None;
                            doc.last_saved_label = Some("Finished".into());
                            self.status = "Finished. Final and receipt saved locally.".into();
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
                d.is_editable()
                    && d.is_dirty()
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
                d.is_editable()
                    && d.is_dirty()
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
        if self.documents.iter().any(|doc| doc.save_error.is_some()) {
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

    pub(crate) fn flush_on_exit(&mut self) {
        if self.flushed {
            return;
        }
        self.flushed = true;
        self.drain_autosave_work();
        if self.workspace.is_some() {
            self.save_open_tabs_pref();
            self.save_ui_prefs_if_workspace();
        }
        self.worker.shutdown();
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.flush_on_exit();
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
    let is_pre_answers = path.file_name().and_then(|n| n.to_str()) == Some("pre-answers.md");
    if let Some(entry) = tree_entry {
        let lockable = entry.kind == FileKind::Corpus || is_pre_answers;
        return (entry.read_only, lockable);
    }
    let name = path.file_name().and_then(|n| n.to_str());
    let mut read_only = name == Some("voice-guide.md");
    let mut lockable = is_pre_answers;
    if let Some(voice) = voice_project
        && let Ok(rel) = path.strip_prefix(voice)
    {
        if rel
            .parent()
            .is_some_and(|p| p.starts_with("corpus") || p.starts_with("transcripts"))
        {
            lockable = true;
        }
        if workspace_tree::immutable(rel) {
            read_only = true;
        }
    }
    (read_only, lockable)
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
    let mut changed = false;
    while let Ok(reply) = editor.worker.try_recv() {
        editor.apply_reply(reply, Some(tx));
        changed = true;
    }

    let autosave = editor.store.ui.autosave_workspace_files;
    let mut next_wake: Option<f64> = None;
    for doc in &mut editor.documents {
        if !doc.is_editable() || !doc.is_dirty() {
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

    editor.sync_window_title(tx);
    changed
}

pub(crate) fn on_key(editor: &mut Editor, key: &KeyEvent, tx: &mut Tx) -> bool {
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
    if is_sidebar_toggle(key) {
        editor.toggle_sidebar();
        return true;
    }
    if key.is_command('j') && !key.mods.shift && !key.mods.alt {
        editor.toggle_inspector();
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
        editor.view_mode = if editor.view_mode == ViewMode::Compare {
            ViewMode::Write
        } else {
            editor.refresh_diff();
            ViewMode::Compare
        };
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

    fn noop_wake() -> WakeFn {
        Box::new(|| {})
    }

    fn ok_writer() -> WriteRunner {
        Box::new(|_| Ok(()))
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
            Ok(())
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
            Ok(())
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
    fn deleting_file_or_folder_with_dirty_tab_is_rejected() {
        let root = std::env::temp_dir().join(format!("ghostless-delete-dirty-{}", std::process::id()));
        fs::create_dir_all(root.join("drafts")).unwrap();
        let file = root.join("drafts/draft.md");
        fs::write(&file, "disk version").unwrap();
        let mut editor = Editor::empty_with_handlers(prefs::EditorStateStore::default(), ok_runner(), ok_writer(), noop_wake());
        editor.workspace = Some(root.clone());
        editor.documents.push(DocumentBuffer::from_workspace_file(file.clone(), "draft.md".into(), "disk version".into(), false, false, true, None));
        editor.active_doc = Some(0);
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
    fn standalone_session_saves_when_link_has_no_workspace() {
        let session = test_session();
        let mut store = prefs::EditorStateStore::default();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&session.project, &session.id), prefs::SessionLink { workspace_file: "draft.md".into(), last_synced_sha256: String::new() });
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = writes.clone();
        let writer: WriteRunner = Box::new(move |op| { captured.lock().unwrap().push(op.clone()); Ok(()) });
        let mut editor = Editor::with_handlers(session, store, ok_runner(), writer, noop_wake());
        assert!(editor.status.contains("linked file unavailable") || editor.status.contains("Linked file unavailable"));
        editor.active_working_edit_mut().text = "session edit".into();
        editor.changed_active(&mut Tx::default());
        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert!(matches!(&writes.lock().unwrap()[0], WriteOp::Session { mirror_path: None, .. }));
        assert!(!editor.documents[0].is_dirty());
        assert!(editor.status.contains("linked file unavailable"));
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
    fn session_working_files_are_read_only_even_outside_tree() {
        let voice = Path::new("/tmp/voice");
        for name in ["after.md", "notes.md"] {
            let path = voice.join("sessions/2026-11-23").join(name);
            assert!(classify_file_flags(None, &path, Some(voice)).0);
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
    fn flush_on_exit_saves_dirty_edits_before_autosave_timer_and_is_idempotent() {
        let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = writes.clone();
        let writer: WriteRunner = Box::new(move |op| {
            recorded.lock().unwrap().push(op.clone());
            Ok(())
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
            Ok(())
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

        editor.flush_on_exit();
        editor.flush_on_exit();
        assert_eq!(*calls.lock().unwrap(), 2);
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

        unsafe {
            if let Some(v) = prev_home {
                std::env::set_var("GHOSTLESS_HOME", v);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        let _ = fs::remove_dir_all(&base);
    }
}
