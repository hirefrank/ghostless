use super::{DialogKind, Editor, ViewMode, WorkspaceChrome, load_placeholder};
use crate::{
    documents::{DocId, DocumentBuffer},
    fs_io, prefs,
    session::load_session,
    workspace_tree::{self, WorkspaceEntry},
};
use rune::{TextEdit, Tx};
use rune_kit::{FileNode, TreeModel};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(in crate::state) fn tree_files(
    tree: &TreeModel<FileNode<WorkspaceEntry>>,
) -> Vec<WorkspaceEntry> {
    fn collect(
        tree: &TreeModel<FileNode<WorkspaceEntry>>,
        parent: Option<&str>,
        out: &mut Vec<WorkspaceEntry>,
    ) {
        for id in tree.children(parent) {
            if let Some(node) = tree.get(id) {
                if let Some(entry) = &node.data {
                    out.push(entry.clone());
                }
                if node.dir {
                    collect(tree, Some(id), out);
                }
            }
        }
    }
    let mut entries = Vec::new();
    collect(tree, None, &mut entries);
    entries
}

impl Editor {
    pub(crate) fn open_workspace(&mut self, folder: PathBuf, initial_file: Option<PathBuf>) {
        self.open_workspace_with_voice(folder, initial_file, None);
    }
    pub(in crate::state) fn open_workspace_with_voice(
        &mut self,
        folder: PathBuf,
        initial_file: Option<PathBuf>,
        voice_project: Option<PathBuf>,
    ) {
        if !self.drain_autosave_work() {
            return;
        }
        if self.documents.iter().any(|doc| doc.is_dirty()) {
            self.status = "Workspace not changed: save or discard unsaved documents first".into();
            return;
        }
        self.stop_watcher();
        let folder = folder.canonicalize().unwrap_or(folder);
        self.voice_project =
            voice_project.or_else(|| crate::cli::discover_voice_project(&folder, None));
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
        self.chrome = WorkspaceChrome::default();
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
            let recent_files = self
                .store
                .last_open_files
                .get(&key)
                .cloned()
                .unwrap_or_default();
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
        self.restart_watcher();
        if let Err(error) = prefs::save_store(&self.store) {
            self.status = error.to_string();
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
        if let Some(pos) = self
            .documents
            .iter()
            .position(|d| d.matches_workspace_file(&canon))
        {
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
                self.status = err.to_string();
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
    pub(in crate::state) fn prompt_recovery_if_needed(&mut self) {
        let Some(doc) = self.active_doc_ref() else {
            return;
        };
        if doc.read_only || doc.is_dirty() || !doc.recovery_path().is_file() {
            return;
        }
        if self.chrome.active_dialog.is_none() {
            self.chrome.active_dialog = Some(DialogKind::Recover {
                doc_id: doc.id.clone(),
                title: doc.title.clone(),
            });
        } else {
            self.status = "A recovery copy is available; reopen the file to review it".into();
        }
    }
    pub(crate) fn resolve_recovery(&mut self, restore: bool) {
        let Some(DialogKind::Recover { doc_id, .. }) = self.chrome.active_dialog.clone() else {
            return;
        };
        let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) else {
            self.close_dialog();
            return;
        };
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
                Err(error) => {
                    self.chrome.dialog_error = Some(error.to_string());
                    return;
                }
            }
        } else if let Err(error) = fs::remove_file(&recovery)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            self.chrome.dialog_error = Some(format!("Cannot remove recovery copy: {error}"));
            return;
        }
        self.close_dialog();
        self.refresh_find();
    }
    pub(crate) fn open_session_entry(&mut self, project: PathBuf, session_id: String) {
        if let Some(pos) = self
            .documents
            .iter()
            .position(|d| d.matches_session(&project, &session_id))
        {
            self.documents[pos].pinned = true;
            self.switch_active_doc(pos);
            self.reconcile_active_linked_doc();
            self.prompt_recovery_if_needed();
            return;
        }
        let loaded = match load_session(project.clone(), session_id.clone()) {
            Ok(session) => session,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        let key = prefs::EditorStateStore::session_link_key(&project, &session_id);
        let session_link = self.store.session_links.get(&key);
        let finished = loaded.finished;
        let mirror_path = (!finished)
            .then_some(session_link)
            .flatten()
            .and_then(|link| {
                (!link.last_synced_sha256.is_empty()
                    && (self.workspace.is_some() || link.workspace_file.is_absolute()))
                .then(|| resolve_mirror_path(&link.workspace_file, self.workspace.as_deref()))
            });
        let unknown_link =
            !finished && session_link.is_some_and(|link| link.last_synced_sha256.is_empty());
        let unresolved_link = !finished
            && self.workspace.is_none()
            && session_link.is_some_and(|link| !link.workspace_file.is_absolute());
        let missing_link = !finished && mirror_path.as_ref().is_some_and(|path| !path.is_file());
        let mut mirror_path = if finished || missing_link {
            None
        } else {
            mirror_path
        };
        let mut open_in_other_tab = false;
        if !finished
            && let Some(mirror) = mirror_path.as_ref()
            && let Some(pos) = self
                .documents
                .iter()
                .position(|d| d.matches_workspace_file(mirror))
        {
            let can_attach_in_place = matches!(self.documents[pos].id, DocId::File(_))
                && self.documents[pos].session_id.is_none()
                && !self.documents[pos].is_dirty()
                && !self.documents[pos].read_only
                && !self.documents[pos].user_locked
                && self
                    .worker_busy
                    .as_ref()
                    .is_none_or(|(id, _)| id != &self.documents[pos].id);
            if can_attach_in_place {
                let mirror = mirror.clone();
                let disk = fs_io::read_utf8(&mirror)
                    .unwrap_or_else(|_| self.documents[pos].edit.text.clone());
                let same = fs_io::sha256_text(&disk) == fs_io::sha256_text(&loaded.after);
                self.flush_leaving_doc();
                {
                    let doc = &mut self.documents[pos];
                    doc.pinned = true;
                    doc.session_id = Some(loaded.id);
                    doc.original = Some(loaded.original);
                    doc.before_sha256 = Some(loaded.before_sha256);
                    doc.notes = Some(TextEdit::new(loaded.notes));
                    doc.voice_project = Some(loaded.project);
                    doc.mirror_path = Some(mirror);
                    if same {
                        doc.conflict = None;
                    } else {
                        doc.edit = TextEdit::new(loaded.after.clone());
                        doc.cached_judge = None;
                        doc.latest_judge_request = None;
                        *doc.decoration_cache.borrow_mut() = None;
                        doc.conflict = Some(crate::documents::SessionConflict {
                            disk_text: disk,
                            session_text: loaded.after,
                        });
                    }
                }
                self.active_doc = Some(pos);
                self.conflict_compare = false;
                self.refresh_find();
                self.refresh_diff();
                self.dispatch_next();
                self.status = if same {
                    "Ready".into()
                } else {
                    "This file changed on disk outside the edit session.".into()
                };
                self.judge_due = None;
                self.judge_armed = true;
                self.prompt_recovery_if_needed();
                return;
            }
            mirror_path = None;
            open_in_other_tab = true;
        }
        let buf = DocumentBuffer::from_session(loaded, true, mirror_path);
        self.flush_leaving_doc();
        self.documents.push(buf);
        self.active_doc = Some(self.documents.len() - 1);
        self.conflict_compare = false;
        self.refresh_find();
        self.refresh_diff();
        self.dispatch_next();
        self.status = if open_in_other_tab {
            "The linked workspace file is open in another tab. Editing the session copy.".into()
        } else if missing_link {
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
    pub(crate) fn set_reference(&mut self, source: crate::ui::reference::ReferenceSource) {
        use crate::ui::reference::ReferenceSource;
        let (title, content) = match &source {
            ReferenceSource::SessionOriginal => (
                "Original (before.md)".to_owned(),
                self.active_doc_ref()
                    .and_then(|d| d.original.clone())
                    .unwrap_or_default(),
            ),
            ReferenceSource::File(path) => match fs::read_to_string(path) {
                Ok(content) => (
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    content,
                ),
                Err(error) => {
                    self.status = format!("Cannot read reference: {error}");
                    return;
                }
            },
        };
        self.reference.source = Some(source);
        self.reference.cached_title = title;
        self.reference.cached_content = content;
        self.reference.open = true;
    }
    pub(crate) fn toggle_reference(&mut self) {
        if self.reference.open {
            self.reference.open = false;
            return;
        }
        if self.reference.source.is_none()
            && let Some(doc) = self.active_doc_ref()
        {
            let style = self
                .voice_project
                .as_ref()
                .or(self.workspace.as_ref())
                .map(|p| p.join("STYLE.md"));
            let source =
                crate::ui::reference::default_source(doc.is_session(), &doc.path, style.as_deref());
            if let Some(source) = source {
                self.set_reference(source);
            }
        }
        self.reference.open = true;
    }
    pub(crate) fn pin_selected_reference(&mut self) {
        let path = self
            .selected_tree_id
            .as_ref()
            .and_then(|id| self.file_tree.get(id))
            .filter(|node| !node.dir)
            .and_then(|node| node.data.as_ref())
            .map(|entry| entry.abs_path.clone());
        if let Some(path) = path {
            self.set_reference(crate::ui::reference::ReferenceSource::File(path));
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
    pub(in crate::state) fn remember_link_hash(&mut self, hash: &str) {
        let Some(doc) = self.active_doc_ref() else {
            return;
        };
        let Some(key) = doc
            .voice_project
            .as_ref()
            .zip(doc.session_id.as_ref())
            .map(|(project, id)| prefs::EditorStateStore::session_link_key(project, id))
        else {
            return;
        };
        if let Some(link) = self.store.session_links.get_mut(&key) {
            link.last_synced_sha256 = hash.to_owned();
            let _ = prefs::save_store(&self.store);
        }
    }
    pub(in crate::state) fn reconcile_linked_file(&mut self) {
        let Some(index) = self.active_doc else { return };
        if self.documents[index].read_only {
            return;
        }
        let path = self.documents[index].path.clone();
        let Some((project, id, after, before, notes, before_sha)) =
            self.linked_session_files(&path)
        else {
            return;
        };
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
        doc.conflict = Some(crate::documents::SessionConflict {
            disk_text: disk,
            session_text: after,
        });
        self.status = "This file changed on disk outside the edit session.".into();
    }
    pub(in crate::state) fn linked_session_files(
        &self,
        path: &Path,
    ) -> Option<(PathBuf, String, String, String, String, String)> {
        struct LinkedCandidate {
            created_at: String,
            key: String,
            project: PathBuf,
            id: String,
            after: String,
            before: String,
            notes: String,
            before_sha: String,
        }

        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let workspace = self
            .workspace
            .as_deref()
            .map(|root| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()));
        let mut valid: Vec<LinkedCandidate> = Vec::new();
        for (key, link) in &self.store.session_links {
            if link.last_synced_sha256.is_empty() {
                continue;
            }
            let resolved = resolve_mirror_path(&link.workspace_file, workspace.as_deref());
            let linked = resolved.canonicalize().unwrap_or(resolved);
            if linked != canon {
                continue;
            }
            let Some((project_str, id)) = key.rsplit_once("::") else {
                continue;
            };
            if !crate::session::valid_session_id(id) {
                continue;
            }
            let project = PathBuf::from(project_str);
            if self
                .documents
                .iter()
                .any(|doc| doc.matches_session(&project, id))
            {
                continue;
            }
            let folder = project.join("sessions").join(id);
            if !folder.join("session.json").is_file() || folder.join("receipt.json").exists() {
                continue;
            }
            let Ok(after) = fs_io::read_utf8(&folder.join("after.md")) else {
                continue;
            };
            let Ok(before) = fs_io::read_utf8(&folder.join("before.md")) else {
                continue;
            };
            let notes = fs_io::read_utf8(&folder.join("notes.md")).unwrap_or_default();
            let Ok(meta) = fs_io::read_utf8(&folder.join("session.json")) else {
                continue;
            };
            let Ok(meta_json) = serde_json::from_str::<serde_json::Value>(&meta) else {
                continue;
            };
            let Some(before_sha) = meta_json
                .get("beforeSha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            if fs_io::sha256_text(&before) != before_sha {
                continue;
            }
            let created_at = meta_json
                .get("createdAt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            valid.push(LinkedCandidate {
                created_at,
                key: key.clone(),
                project,
                id: id.to_owned(),
                after,
                before,
                notes,
                before_sha,
            });
        }
        valid.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
                .then_with(|| b.key.cmp(&a.key))
        });
        valid.into_iter().next().map(|candidate| {
            (
                candidate.project,
                candidate.id,
                candidate.after,
                candidate.before,
                candidate.notes,
                candidate.before_sha,
            )
        })
    }
}

pub(in crate::state) fn remap_renamed_path(
    path: &Path,
    old_canon: &Path,
    new_canon: &Path,
) -> Option<PathBuf> {
    if path == old_canon {
        Some(new_canon.to_path_buf())
    } else {
        path.strip_prefix(old_canon)
            .ok()
            .map(|suffix| new_canon.join(suffix))
    }
}

pub(in crate::state) fn resolve_mirror_path(path: &Path, workspace: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match workspace {
        Some(root) if fs_io::is_contained_in(root, &root.join(path)) => root.join(path),
        // Leave unresolved paths relative so the worker rejects them instead of writing to cwd.
        _ => path.to_path_buf(),
    }
}

pub(in crate::state) fn classify_file_flags(
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

pub(in crate::state) fn rel_path_from_abs(root: &Path, abs: &Path) -> Option<String> {
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
