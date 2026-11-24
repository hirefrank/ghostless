use std::{collections::HashSet, path::{Path, PathBuf}, time::Duration};
use rune::{Key, KeyEvent, TextEdit, Tx};
use rune_kit::{
    FileNode, TreeModel,
    diff::{DiffMode, DiffState, FileDiff},
    shell::{PaneState, is_sidebar_toggle},
};
use crate::{
    cli::run_cli,
    fs_io,
    prefs,
    session::{LaunchTarget, Session, load_session},
    worker::{CliRunner, InFlight, WakeFn, Worker, WorkerReply, WorkerRequest},
    workspace_tree::{self, SessionSummary, WorkspaceEntry},
};

pub(crate) const SAVE_DELAY: Duration = Duration::from_millis(800);

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
}

pub(crate) enum TabKind {
    File {
        edit: Box<TextEdit>,
        dirty: bool,
        read_only: bool,
    },
    Session {
        project: PathBuf,
        session_id: String,
    },
}

pub(crate) struct OpenTab {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) path: PathBuf,
    pub(crate) pinned: bool,
    pub(crate) kind: TabKind,
}

pub(crate) struct Editor {
    pub(crate) session: Session,
    pub(crate) session_active: bool,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) voice_project: Option<PathBuf>,
    #[allow(dead_code)]
    pub(crate) initial_file: Option<PathBuf>,
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
    pub(crate) tabs: Vec<OpenTab>,
    pub(crate) active_tab: Option<usize>,
    pub(crate) fallback_edit: TextEdit,
    pub(crate) after: TextEdit,
    pub(crate) notes: TextEdit,
    pub(crate) diff: DiffState,
    pub(crate) generation: u64,
    pub(crate) saved_generation: u64,
    pub(crate) autosave_due: Option<f64>,
    pub(crate) pending_save: bool,
    pub(crate) finish_requested: bool,
    pub(crate) in_flight: Option<InFlight>,
    pub(crate) flushed: bool,
    pub(crate) status: String,
    pub(crate) worker: Worker,
}

impl Editor {
    pub(crate) fn new(target: LaunchTarget, wake: WakeFn) -> Self {
        let store = prefs::load_store();
        let (session, active, workspace, voice_project, initial_file) = match target {
            LaunchTarget::Session(session) => {
                let voice = Some(session.project.clone());
                (session, true, None, voice, None)
            }
            LaunchTarget::Workspace {
                folder,
                voice_project,
                initial_file,
            } => (Self::empty_session(), false, Some(folder), voice_project, initial_file),
            LaunchTarget::Welcome { voice_project } => {
                (Self::empty_session(), false, None, voice_project, None)
            }
        };
        let mut editor = Self::with_worker(session, Box::new(run_cli), wake);
        editor.store = store;
        editor.session_active = active;
        editor.voice_project = voice_project;
        editor.apply_pane_defaults();
        if active {
            editor.open_session_tab_entry();
            if let Some(voice) = &editor.voice_project {
                editor.sessions = workspace_tree::scan_sessions(voice, &editor.store.session_links);
            }
        } else if let Some(folder) = workspace {
            editor.open_workspace(folder, initial_file);
        }
        editor
    }

    fn empty_session() -> Session {
        Session {
            project: PathBuf::new(),
            id: String::new(),
            source_name: String::new(),
            original: String::new(),
            after: String::new(),
            notes: String::new(),
            before_sha256: String::new(),
            finished: false,
        }
    }

    pub(crate) fn with_worker(session: Session, runner: CliRunner, wake: WakeFn) -> Self {
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &session.source_name,
            &session.original,
            &session.after,
        )]);
        diff.set_mode(DiffMode::Split);
        let worker = Worker::spawn(runner, wake);
        let sidebar_pane = PaneState::new(250.0).range(180.0, 420.0).collapsed();
        let inspector_pane = PaneState::new(310.0).range(240.0, 480.0);
        Self {
            after: TextEdit::new(session.after.clone()),
            notes: TextEdit::new(session.notes.clone()),
            diff,
            view_mode: ViewMode::Write,
            sidebar_pane,
            inspector_pane,
            file_tree: TreeModel::new().key("workspace-files"),
            sessions: Vec::new(),
            sessions_collapsed: false,
            voice_collapsed: false,
            selected_tree_id: None,
            file_menu_open: false,
            active_dialog: None,
            dialog_input: TextEdit::new(""),
            dialog_error: None,
            tabs: Vec::new(),
            active_tab: None,
            fallback_edit: TextEdit::new(""),
            generation: 0,
            saved_generation: 0,
            autosave_due: None,
            pending_save: false,
            finish_requested: false,
            in_flight: None,
            flushed: false,
            status: if session.finished {
                "Finished".into()
            } else {
                "Ready".into()
            },
            session,
            session_active: true,
            workspace: None,
            voice_project: None,
            initial_file: None,
            store: prefs::EditorStateStore::default(),
            worker,
        }
    }

    fn apply_pane_defaults(&mut self) {
        if self.workspace.is_some() || !self.session_active {
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
        } else {
            self.sidebar_pane = PaneState::new(250.0).range(180.0, 420.0).collapsed();
            self.inspector_pane = PaneState::new(310.0).range(240.0, 480.0);
        }
    }

    fn open_session_tab_entry(&mut self) {
        if !self.session_active || self.session.id.is_empty() {
            return;
        }
        let tab_id = format!("session:{}::{}", self.session.project.display(), self.session.id);
        let path = self.session.project.join("sessions").join(&self.session.id);
        if let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) {
            self.tabs[pos].pinned = true;
            self.active_tab = Some(pos);
            return;
        }
        self.tabs.push(OpenTab {
            id: tab_id,
            title: self.session.source_name.clone(),
            path,
            pinned: true,
            kind: TabKind::Session {
                project: self.session.project.clone(),
                session_id: self.session.id.clone(),
            },
        });
        self.active_tab = Some(self.tabs.len() - 1);
    }

    pub(crate) fn open_workspace(&mut self, folder: PathBuf, initial_file: Option<PathBuf>) {
        self.drain_session_writes();
        let folder = folder.canonicalize().unwrap_or(folder);
        self.session_active = false;
        self.voice_project = crate::cli::discover_voice_project(&folder, None);
        self.store.record_workspace(&folder);
        self.workspace = Some(folder.clone());
        self.initial_file = initial_file.clone();
        self.tabs.clear();
        self.active_tab = None;
        self.selected_tree_id = None;
        self.view_mode = ViewMode::Write;
        self.apply_pane_defaults();
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

    pub(crate) fn active_tab_ref(&self) -> Option<&OpenTab> {
        self.active_tab.and_then(|i| self.tabs.get(i))
    }

    pub(crate) fn active_is_session(&self) -> bool {
        matches!(self.active_tab_ref().map(|t| &t.kind), Some(TabKind::Session { .. }))
    }

    pub(crate) fn active_has_baseline(&self) -> bool {
        self.active_is_session()
    }

    pub(crate) fn active_is_read_only(&self) -> bool {
        match self.active_tab_ref().map(|t| &t.kind) {
            Some(TabKind::Session { .. }) => self.is_locked(),
            Some(TabKind::File { read_only, .. }) => *read_only,
            None => true,
        }
    }

    pub(crate) fn active_working_edit(&self) -> &TextEdit {
        match self.active_tab_ref().map(|t| &t.kind) {
            Some(TabKind::Session { .. }) => &self.after,
            Some(TabKind::File { edit, .. }) => edit,
            None => &self.fallback_edit,
        }
    }

    pub(crate) fn active_working_edit_mut(&mut self) -> &mut TextEdit {
        let idx = self.active_tab;
        match idx.and_then(|i| self.tabs.get(i)).map(|t| &t.kind) {
            Some(TabKind::Session { .. }) => &mut self.after,
            Some(TabKind::File { .. }) => {
                if let Some(TabKind::File { edit, .. }) =
                    idx.and_then(|i| self.tabs.get_mut(i)).map(|t| &mut t.kind)
                {
                    edit
                } else {
                    &mut self.fallback_edit
                }
            }
            None => &mut self.fallback_edit,
        }
    }

    pub(crate) fn dirty_file_paths(&self) -> HashSet<PathBuf> {
        let mut out = HashSet::new();
        for tab in &self.tabs {
            if let TabKind::File { dirty: true, .. } = &tab.kind {
                out.insert(tab.path.clone());
            }
        }
        out
    }

    pub(crate) fn tab_is_dirty(&self, tab: &OpenTab) -> bool {
        match &tab.kind {
            TabKind::File { dirty, .. } => *dirty,
            TabKind::Session { project, session_id } => {
                self.session_active
                    && self.session.project == *project
                    && self.session.id == *session_id
                    && self.is_dirty()
            }
        }
    }

    pub(crate) fn open_workspace_file(&mut self, path: PathBuf, pin: bool) {
        let canon = path.canonicalize().unwrap_or(path);
        let tab_id = format!("file:{}", canon.display());
        if let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) {
            if pin {
                self.tabs[pos].pinned = true;
            }
            self.active_tab = Some(pos);
            if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
                self.view_mode = ViewMode::Write;
            }
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
        let read_only = rel_id
            .as_deref()
            .and_then(|rel| self.file_tree.get(rel))
            .and_then(|node| node.data.as_ref())
            .is_some_and(|entry| entry.read_only)
            || canon.file_name().and_then(|n| n.to_str()) == Some("voice-guide.md");
        let new_tab = OpenTab {
            id: tab_id,
            title,
            path: canon.clone(),
            pinned: pin,
            kind: TabKind::File {
                edit: Box::new(TextEdit::new(text)),
                dirty: false,
                read_only,
            },
        };
        if !pin
            && let Some(preview_idx) = self
                .tabs
                .iter()
                .position(|t| !t.pinned && !self.tab_is_dirty(t))
        {
            self.tabs[preview_idx] = new_tab;
            self.active_tab = Some(preview_idx);
        } else {
            self.tabs.push(new_tab);
            self.active_tab = Some(self.tabs.len() - 1);
        }
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
        let tab_id = format!("session:{}::{}", project.display(), session_id);
        if self.session_active && self.session.project == project && self.session.id == session_id {
            if let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) {
                self.tabs[pos].pinned = true;
                self.active_tab = Some(pos);
            } else {
                self.open_session_tab_entry();
            }
            return;
        }
        self.drain_session_writes();
        let loaded = match load_session(project.clone(), session_id.clone()) {
            Ok(session) => session,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        self.tabs.retain(|t| !matches!(t.kind, TabKind::Session { .. }));
        self.session_active = true;
        self.after = TextEdit::new(loaded.after.clone());
        self.notes = TextEdit::new(loaded.notes.clone());
        self.generation = 0;
        self.saved_generation = 0;
        self.autosave_due = None;
        self.pending_save = false;
        self.finish_requested = false;
        self.status = if loaded.finished {
            "Finished".into()
        } else {
            "Ready".into()
        };
        self.session = loaded;
        self.refresh_diff();
        self.open_session_tab_entry();
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
        if read_only
            && let Some(OpenTab {
                kind: TabKind::File { read_only: ro, .. },
                ..
            }) = self.active_tab.and_then(|i| self.tabs.get_mut(i))
        {
            *ro = true;
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

    pub(crate) fn activate_tab(&mut self, id: &str, _tx: &mut Tx) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
            self.active_tab = Some(pos);
            if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
                self.view_mode = ViewMode::Write;
            }
            if let Some(rel) = self.rel_from_workspace(&self.tabs[pos].path) {
                self.selected_tree_id = Some(rel.clone());
                self.file_tree.expand_to(&rel);
                self.file_tree.select(&rel);
            }
        }
    }

    pub(crate) fn pin_tab_by_id(&mut self, id: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.pinned = true;
        }
    }

    pub(crate) fn close_tab_by_id(&mut self, id: &str) -> bool {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else {
            return false;
        };
        match &self.tabs[pos].kind {
            TabKind::Session { .. } => {
                self.drain_session_writes();
                self.session_active = false;
            }
            TabKind::File {
                edit,
                dirty: true,
                read_only: false,
            } => {
                if let Err(err) = fs_io::atomic_write_text(&self.tabs[pos].path, &edit.text) {
                    self.status = format!("Save failed: {err}");
                    return false;
                }
            }
            _ => {}
        }
        self.tabs.remove(pos);
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else {
            let next = match self.active_tab {
                Some(active) if active > pos => active - 1,
                Some(active) if active == pos => pos.min(self.tabs.len() - 1),
                Some(active) => active,
                None => 0,
            };
            self.active_tab = Some(next);
        }
        if !self.active_has_baseline() && self.view_mode == ViewMode::Compare {
            self.view_mode = ViewMode::Write;
        }
        self.save_open_tabs_pref();
        true
    }

    pub(crate) fn close_active_tab(&mut self) -> bool {
        let Some(id) = self.active_tab_ref().map(|t| t.id.clone()) else {
            return false;
        };
        self.close_tab_by_id(&id)
    }

    pub(crate) fn cycle_tab(&mut self, backwards: bool) {
        let len = self.tabs.len();
        if len <= 1 {
            return;
        }
        let cur = self.active_tab.unwrap_or(0);
        let next = if backwards {
            (cur + len - 1) % len
        } else {
            (cur + 1) % len
        };
        let id = self.tabs[next].id.clone();
        self.activate_tab(&id, &mut Tx::default());
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
            .tabs
            .iter()
            .filter_map(|t| match t.kind {
                TabKind::File { .. } => rel_path_from_abs(folder, &t.path),
                TabKind::Session { .. } => None,
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
        let protected = node
            .data
            .as_ref()
            .is_some_and(|e| e.read_only);
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
        let (Some(root), Some(sel)) = (self.workspace.clone(), self.selected_tree_id.clone()) else {
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

    pub(crate) fn submit_dialog(&mut self, _tx: &mut Tx) {
        let (Some(root), Some(dialog)) = (self.workspace.clone(), self.active_dialog.clone()) else {
            return;
        };
        let input = self.dialog_input.text.trim().to_owned();
        match dialog {
            DialogKind::NewFile { parent_rel } => match workspace_tree::create_file(&root, &parent_rel, &input) {
                Ok(created) => {
                    if let Some(rel) = rel_path_from_abs(&root, &created) {
                        self.selected_tree_id = Some(rel);
                    }
                    self.close_dialog();
                    self.refresh_workspace_tree();
                    self.open_workspace_file(created, true);
                }
                Err(err) => self.dialog_error = Some(err),
            },
            DialogKind::NewFolder { parent_rel } => match workspace_tree::create_folder(&root, &parent_rel, &input) {
                Ok(created) => {
                    if let Some(rel) = rel_path_from_abs(&root, &created) {
                        self.selected_tree_id = Some(rel.clone());
                    }
                    self.close_dialog();
                    self.refresh_workspace_tree();
                    if let Some(sel) = &self.selected_tree_id {
                        self.file_tree.set_expanded(sel, true);
                    }
                }
                Err(err) => self.dialog_error = Some(err),
            },
            DialogKind::Rename { rel_path } => {
                let old_abs = root.join(&rel_path);
                let old_canon = old_abs.canonicalize().unwrap_or(old_abs);
                match workspace_tree::rename_entry(&root, &rel_path, &input) {
                    Ok(renamed) => {
                        let new_canon = renamed.canonicalize().unwrap_or(renamed);
                        for tab in &mut self.tabs {
                            if let TabKind::File { .. } = &tab.kind {
                                if tab.path == old_canon {
                                    tab.path = new_canon.clone();
                                    tab.id = format!("file:{}", new_canon.display());
                                    if let Some(name) = new_canon.file_name().and_then(|n| n.to_str()) {
                                        tab.title = name.to_owned();
                                    }
                                } else if let Ok(suffix) = tab.path.strip_prefix(&old_canon) {
                                    let updated = new_canon.join(suffix);
                                    tab.path = updated.clone();
                                    tab.id = format!("file:{}", updated.display());
                                }
                            }
                        }
                        if let Some(rel) = rel_path_from_abs(&root, &new_canon) {
                            self.selected_tree_id = Some(rel);
                        }
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
            DialogKind::Delete { rel_path, .. } => {
                let target_abs = root.join(&rel_path);
                let target_canon = target_abs.canonicalize().unwrap_or(target_abs);
                match workspace_tree::delete_entry(&root, &rel_path) {
                    Ok(()) => {
                        let removed_ids: Vec<String> = self
                            .tabs
                            .iter()
                            .filter(|t| matches!(t.kind, TabKind::File { .. }) && t.path.starts_with(&target_canon))
                            .map(|t| t.id.clone())
                            .collect();
                        for id in removed_ids {
                            self.tabs.retain(|t| t.id != id);
                        }
                        if let Some(idx) = self.active_tab
                            && idx >= self.tabs.len()
                        {
                            self.active_tab = (!self.tabs.is_empty()).then_some(self.tabs.len() - 1);
                        }
                        self.selected_tree_id = None;
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                    }
                    Err(err) => self.dialog_error = Some(err),
                }
            }
        }
    }

    pub(crate) fn is_locked(&self) -> bool {
        self.session.finished
            || self.finish_requested
            || matches!(self.in_flight, Some(InFlight::Finish))
    }

    pub(crate) fn is_dirty(&self) -> bool {
        !self.session.finished && self.generation > self.saved_generation
    }

    pub(crate) fn changed(&mut self, tx: &mut Tx) {
        if self.is_locked() {
            return;
        }
        self.generation += 1;
        if let Some(tab) = self.active_tab.and_then(|i| self.tabs.get_mut(i))
            && matches!(tab.kind, TabKind::Session { .. })
        {
            tab.pinned = true;
        }
        if matches!(self.in_flight, Some(InFlight::Save)) {
            self.pending_save = true;
            self.autosave_due = None;
        } else {
            self.autosave_due = Some(tx.clock() + SAVE_DELAY.as_secs_f64());
            tx.wake_after(SAVE_DELAY);
        }
        self.status = "Unsaved changes".into();
    }

    pub(crate) fn changed_active(&mut self, tx: &mut Tx) {
        if self.active_is_session() {
            self.changed(tx);
            return;
        }
        if let Some(tab) = self.active_tab.and_then(|i| self.tabs.get_mut(i))
            && let TabKind::File {
                dirty,
                read_only: false,
                ..
            } = &mut tab.kind
        {
            *dirty = true;
            tab.pinned = true;
            self.status = "Unsaved changes".into();
        }
    }

    pub(crate) fn refresh_diff(&mut self) {
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &self.session.source_name,
            &self.session.original,
            &self.after.text,
        )]);
        diff.set_mode(DiffMode::Split);
        self.diff = diff;
    }

    pub(crate) fn save_clicked(&mut self) {
        if self.is_locked() {
            return;
        }
        self.pending_save = true;
        self.autosave_due = None;
        self.dispatch_next();
    }

    pub(crate) fn save_active(&mut self) {
        if self.active_is_session() {
            self.save_clicked();
            return;
        }
        if let Some(tab) = self.active_tab.and_then(|i| self.tabs.get_mut(i))
            && let TabKind::File {
                edit,
                dirty,
                read_only: false,
            } = &mut tab.kind
        {
            match fs_io::atomic_write_text(&tab.path, &edit.text) {
                Ok(()) => {
                    *dirty = false;
                    self.status = "Saved locally".into();
                }
                Err(error) => {
                    self.status = format!("Save failed: {error}");
                }
            }
        }
    }

    pub(crate) fn finish(&mut self) {
        if !self.session_active || self.is_locked() {
            return;
        }
        self.finish_requested = true;
        self.pending_save = false;
        self.autosave_due = None;
        self.status = "Finishing…".into();
        self.dispatch_next();
    }

    pub(crate) fn dispatch_next(&mut self) {
        if self.in_flight.is_some() || self.session.finished || !self.session_active {
            return;
        }
        if self.finish_requested {
            let snapshot = self
                .is_dirty()
                .then(|| (self.after.text.clone(), self.notes.text.clone()));
            match self.worker.send(WorkerRequest::Finish {
                project: self.session.project.clone(),
                session_id: self.session.id.clone(),
                generation: self.generation,
                snapshot,
            }) {
                Ok(()) => {
                    self.in_flight = Some(InFlight::Finish);
                    self.status = "Finishing…".into();
                }
                Err(error) => {
                    self.finish_requested = false;
                    self.status = format!("Finish stopped: {error}");
                }
            }
        } else if self.pending_save {
            self.pending_save = false;
            self.autosave_due = None;
            match self.worker.send(WorkerRequest::Snapshot {
                project: self.session.project.clone(),
                session_id: self.session.id.clone(),
                generation: self.generation,
                after: self.after.text.clone(),
                notes: self.notes.text.clone(),
            }) {
                Ok(()) => {
                    self.in_flight = Some(InFlight::Save);
                    self.status = "Saving…".into();
                }
                Err(error) => {
                    self.status = format!("Save failed: {error}");
                }
            }
        }
    }

    pub(crate) fn apply_reply(&mut self, reply: WorkerReply) -> bool {
        self.in_flight = None;
        match reply {
            WorkerReply::Snapshot { generation, result } => match result {
                Ok(()) => {
                    self.saved_generation = self.saved_generation.max(generation);
                    if self.finish_requested {
                        self.dispatch_next();
                    } else if self.generation > self.saved_generation {
                        self.pending_save = true;
                        self.dispatch_next();
                    } else {
                        self.status = "Saved locally".into();
                    }
                    true
                }
                Err(error) => {
                    self.autosave_due = None;
                    if self.finish_requested {
                        self.finish_requested = false;
                        self.status = format!("Finish stopped: {error}");
                    } else if self.pending_save && self.generation > generation {
                        self.dispatch_next();
                    } else {
                        self.pending_save = false;
                        self.status = format!("Save failed: {error}");
                    }
                    false
                }
            },
            WorkerReply::Finish {
                generation,
                saved_snapshot,
                result,
            } => {
                if saved_snapshot {
                    self.saved_generation = self.saved_generation.max(generation);
                }
                self.finish_requested = false;
                match result {
                    Ok(()) => {
                        self.session.finished = true;
                        self.pending_save = false;
                        self.autosave_due = None;
                        self.status = "Finished. Final and receipt saved locally.".into();
                        if let Some(voice) = &self.voice_project {
                            self.sessions =
                                workspace_tree::scan_sessions(voice, &self.store.session_links);
                        }
                        true
                    }
                    Err(error) => {
                        self.status = format!("Finish stopped: {error}");
                        false
                    }
                }
            }
        }
    }

    fn drain_session_writes(&mut self) {
        let mut attempted_flush_save = false;
        let mut attempted_finish = false;
        loop {
            if self.in_flight.is_some() {
                match self.worker.recv() {
                    Ok(reply) => {
                        let ok = self.apply_reply(reply);
                        if !ok && self.in_flight.is_none() {
                            eprintln!("ghostless-editor: {}", self.status);
                            break;
                        }
                        continue;
                    }
                    Err(_) => {
                        self.in_flight = None;
                        break;
                    }
                }
            }
            if self.session_active
                && self.finish_requested
                && !self.session.finished
                && !attempted_finish
            {
                attempted_finish = true;
                self.dispatch_next();
                if self.in_flight.is_some() {
                    continue;
                }
                eprintln!("ghostless-editor: {}", self.status);
                break;
            }
            if self.session_active && self.is_dirty() && !attempted_flush_save {
                attempted_flush_save = true;
                self.pending_save = true;
                self.dispatch_next();
                if self.in_flight.is_some() {
                    continue;
                }
                eprintln!("ghostless-editor: {}", self.status);
                break;
            }
            break;
        }
    }

    pub(crate) fn flush_on_exit(&mut self) {
        if self.flushed {
            return;
        }
        self.flushed = true;
        self.drain_session_writes();
        for tab in &mut self.tabs {
            if let TabKind::File {
                edit,
                dirty,
                read_only: false,
            } = &mut tab.kind
                && *dirty
            {
                match fs_io::atomic_write_text(&tab.path, &edit.text) {
                    Ok(()) => *dirty = false,
                    Err(err) => eprintln!("ghostless-editor: {err}"),
                }
            }
        }
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
        editor.apply_reply(reply);
        changed = true;
    }
    if editor.in_flight.is_none()
        && editor.session_active
        && !editor.session.finished
        && let Some(due) = editor.autosave_due
    {
        if clock >= due {
            editor.pending_save = true;
            editor.autosave_due = None;
            editor.dispatch_next();
            changed = true;
        } else {
            tx.wake_after(Duration::from_secs_f64((due - clock).max(0.0)));
        }
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
    if !editor.session_active && editor.workspace.is_none() {
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
    if key.is_command('w') && !key.mods.shift && editor.close_active_tab() {
        return true;
    }
    if key.key == Key::Tab && (key.mods.cmd || key.mods.ctrl) {
        editor.cycle_tab(key.mods.shift);
        return true;
    }
    if key.is_command('s') && !key.mods.shift {
        editor.save_active();
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
        worker::{CliRunner, InFlight, WakeFn},
    };
    use rune::Tx;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, mpsc};
    use std::{thread, time::Duration};

    type RecordedCall = (Vec<String>, Option<String>);

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

    fn drain_until_idle(editor: &mut Editor) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while editor.in_flight.is_some() {
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
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();

        let runner: CliRunner = Box::new(move |args, input| {
            recorded.lock().unwrap().push((
                args.iter().map(|item| (*item).to_owned()).collect(),
                input.map(str::to_owned),
            ));
            let _ = entered_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
            Ok("{}".into())
        });

        let mut editor = Editor::with_worker(test_session(), runner, noop_wake());
        let mut tx = Tx::default();
        editor.after.text = "First draft edit.".into();
        editor.changed(&mut tx);
        editor.save_clicked();

        assert_eq!(editor.in_flight, Some(InFlight::Save));
        assert_eq!(editor.status, "Saving…");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started running snapshot");

        editor.after.text = "Second draft edit.".into();
        editor.changed(&mut tx);
        assert!(editor.pending_save);

        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        drain_until_idle(&mut editor);
        assert_eq!(editor.status, "Saved locally");
        assert!(!editor.is_dirty());
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn rapid_edits_during_in_flight_save_coalesce_into_one_follow_up() {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();

        let runner: CliRunner = Box::new(move |args, input| {
            let count = {
                let mut guard = recorded.lock().unwrap();
                guard.push((
                    args.iter().map(|item| (*item).to_owned()).collect(),
                    input.map(str::to_owned),
                ));
                guard.len()
            };
            if count == 1 {
                let _ = started_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            }
            Ok("{}".into())
        });

        let mut editor = Editor::with_worker(test_session(), runner, noop_wake());
        let mut tx = Tx::default();
        editor.after.text = "Edit 0".into();
        editor.changed(&mut tx);
        assert!(poll(&mut editor, 1.0, &mut tx));
        assert_eq!(editor.in_flight, Some(InFlight::Save));
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first save entered worker");

        for idx in 1..=5 {
            editor.after.text = format!("Edit {idx}");
            editor.notes.text = format!("Note {idx}");
            editor.changed(&mut tx);
        }

        release_tx.send(()).unwrap();
        drain_until_idle(&mut editor);

        let guard = calls.lock().unwrap();
        assert_eq!(
            guard.len(),
            2,
            "5 rapid edits must coalesce into one follow-up save"
        );
        let last_payload: Value =
            serde_json::from_str(guard[1].1.as_deref().unwrap()).unwrap();
        assert_eq!(last_payload["after"], "Edit 5");
        assert_eq!(last_payload["notes"], "Note 5");
        assert_eq!(editor.status, "Saved locally");
        assert!(!editor.is_dirty());
    }

    #[test]
    fn flush_on_exit_saves_dirty_edits_before_autosave_timer_and_is_idempotent() {
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: CliRunner = Box::new(move |args, input| {
            recorded.lock().unwrap().push((
                args.iter().map(|item| (*item).to_owned()).collect(),
                input.map(str::to_owned),
            ));
            Ok("{}".into())
        });

        let mut editor = Editor::with_worker(test_session(), runner, noop_wake());
        let mut tx = Tx::default();
        editor.after.text = "Typed right before closing.".into();
        editor.notes.text = "Keep this note.".into();
        editor.changed(&mut tx);
        assert!(editor.is_dirty());
        assert_eq!(editor.in_flight, None);

        editor.flush_on_exit();
        editor.flush_on_exit();

        let guard = calls.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].0[0], "snapshot");
        let payload: Value = serde_json::from_str(guard[0].1.as_deref().unwrap()).unwrap();
        assert_eq!(payload["after"], "Typed right before closing.");
        assert_eq!(payload["notes"], "Keep this note.");
        assert!(!editor.is_dirty());
    }

    #[test]
    fn finish_waits_for_in_flight_save_then_finishes_once() {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();

        let runner: CliRunner = Box::new(move |args, input| {
            let cmd = args[0].to_owned();
            recorded.lock().unwrap().push((
                args.iter().map(|item| (*item).to_owned()).collect(),
                input.map(str::to_owned),
            ));
            if cmd == "snapshot" {
                let _ = started_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            }
            Ok("{}".into())
        });

        let mut editor = Editor::with_worker(test_session(), runner, noop_wake());
        let mut tx = Tx::default();
        editor.after.text = "Final prose.".into();
        editor.changed(&mut tx);
        editor.save_clicked();
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("snapshot started");

        editor.finish();
        editor.finish();
        assert!(editor.is_locked());

        release_tx.send(()).unwrap();
        editor.flush_on_exit();

        let guard = calls.lock().unwrap();
        let commands: Vec<&str> = guard.iter().map(|(args, _)| args[0].as_str()).collect();
        assert_eq!(commands, vec!["snapshot", "finish"]);
        assert!(editor.session.finished);
        assert_eq!(editor.status, "Finished. Final and receipt saved locally.");
    }

    #[test]
    fn failed_save_keeps_dirty_without_spinning_and_flush_does_not_loop() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let runner: CliRunner = Box::new(move |_, _| {
            *counter.lock().unwrap() += 1;
            Err("disk full".into())
        });

        let mut editor = Editor::with_worker(test_session(), runner, noop_wake());
        let mut tx = Tx::default();
        editor.after.text = "Will fail once.".into();
        editor.changed(&mut tx);
        editor.save_clicked();
        drain_until_idle(&mut editor);

        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(editor.is_dirty());
        assert!(editor.status.contains("Save failed: disk full"));
        assert!(!poll(&mut editor, 10.0, &mut tx));
        assert_eq!(*calls.lock().unwrap(), 1);

        editor.flush_on_exit();
        editor.flush_on_exit();
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[test]
    fn open_workspace_keeps_worker_alive_for_subsequent_session_open() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-ws-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let workspace = base.join("writings");
        let home = base.join("home");
        std::fs::create_dir_all(workspace.join("drafts")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(workspace.join("drafts/one.md"), "One\n").unwrap();
        std::fs::write(workspace.join("drafts/two.md"), "Two\n").unwrap();

        let prev_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }

        let mut editor = Editor::with_worker(test_session(), Box::new(|_, _| Ok("{}".into())), noop_wake());
        editor.open_workspace(workspace.clone(), Some(workspace.join("drafts/one.md")));
        assert!(!editor.flushed);
        assert!(editor.worker.tx.is_some());
        assert_eq!(editor.tabs.len(), 1);
        assert!(editor.tabs[0].pinned);

        // Single-click preview opens and reuses preview slot
        editor.open_workspace_file(workspace.join("drafts/two.md"), false);
        assert_eq!(editor.tabs.len(), 2);
        assert!(!editor.tabs[1].pinned);

        unsafe {
            if let Some(v) = prev_home {
                std::env::set_var("GHOSTLESS_HOME", v);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
