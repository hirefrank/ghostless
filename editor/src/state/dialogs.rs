use super::{
    Editor, rel_path_from_abs, remap_renamed_path, resolve_mirror_path, section_for_document,
};
use crate::{
    documents::{DocId, DocumentBuffer},
    fs_io, prefs,
    worker::clear_recovery_copy,
    workspace_tree,
};
use rune::{TextEdit, Tx};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DialogKind {
    NewFile {
        parent_rel: String,
    },
    NewFolder {
        parent_rel: String,
    },
    Rename {
        rel_path: String,
    },
    Delete {
        rel_path: String,
        display_name: String,
        is_dir: bool,
    },
    ConfirmClose {
        doc_id: DocId,
        title: String,
    },
    Recover {
        doc_id: DocId,
        title: String,
    },
    StartSession,
    ConfirmRevert,
}

impl Editor {
    pub(crate) fn confirm_close_save(&mut self) {
        let Some(DialogKind::ConfirmClose { doc_id, .. }) = self.chrome.active_dialog.clone()
        else {
            return;
        };
        self.close_dialog();
        if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
            doc.pending_save = true;
            doc.autosave_due = None;
            doc.recovery_due = None;
            doc.pending_recovery = false;
            doc.close_after_save = true;
        }
        self.dispatch_next();
    }
    pub(crate) fn confirm_close_discard(&mut self, tx: &mut Tx) {
        let Some(DialogKind::ConfirmClose { doc_id, .. }) = self.chrome.active_dialog.clone()
        else {
            return;
        };
        if let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == doc_id) {
            doc.pending_save = false;
            doc.pending_recovery = false;
            doc.autosave_due = None;
            doc.recovery_due = None;
            doc.close_after_save = false;
        }
        if self
            .worker_busy
            .as_ref()
            .is_some_and(|(id, _)| id == &doc_id)
        {
            // Finish any recovery write before removing its copy.
            self.drain_until_idle();
        }
        if let Some(pos) = self.documents.iter().position(|doc| doc.id == doc_id) {
            if let Err(error) = clear_recovery_copy(&self.documents[pos].recovery_path()) {
                self.chrome.dialog_error = Some(error.to_string());
                return;
            }
            self.remove_doc_at(pos);
            self.sync_window_title(tx);
        }
        self.close_dialog();
    }
    pub(in crate::state) fn active_target_parent_rel(&self) -> String {
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
            e.read_only
                || e.abs_path
                    .file_name()
                    .is_some_and(|name| name == "manifest.json")
        });
        let is_file = !node.dir;
        (!protected, is_file)
    }
    pub(crate) fn open_new_file_dialog(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.chrome.file_menu_open = false;
        self.chrome.dialog_error = None;
        self.chrome.dialog_input = TextEdit::new("");
        self.chrome.dialog_input.focused = true;
        self.chrome.active_dialog = Some(DialogKind::NewFile {
            parent_rel: self.active_target_parent_rel(),
        });
    }
    pub(crate) fn open_new_folder_dialog(&mut self) {
        if self.workspace.is_none() {
            return;
        }
        self.chrome.file_menu_open = false;
        self.chrome.dialog_error = None;
        self.chrome.dialog_input = TextEdit::new("");
        self.chrome.dialog_input.focused = true;
        self.chrome.active_dialog = Some(DialogKind::NewFolder {
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
        self.chrome.file_menu_open = false;
        self.chrome.dialog_error = None;
        self.chrome.dialog_input = TextEdit::new(node.name.clone());
        self.chrome.dialog_input.focused = true;
        self.chrome.active_dialog = Some(DialogKind::Rename { rel_path: sel });
    }
    pub(crate) fn open_delete_dialog(&mut self) {
        let Some(sel) = self.selected_tree_id.clone() else {
            return;
        };
        let Some(node) = self.file_tree.get(&sel) else {
            return;
        };
        self.chrome.file_menu_open = false;
        self.chrome.dialog_error = None;
        self.chrome.active_dialog = Some(DialogKind::Delete {
            display_name: node.name.clone(),
            is_dir: node.dir,
            rel_path: sel,
        });
    }
    pub(crate) fn close_dialog(&mut self) {
        self.chrome.active_dialog = None;
        self.chrome.dialog_error = None;
        self.pending_document_focus = true;
    }
    pub(crate) fn duplicate_selected(&mut self) {
        self.chrome.file_menu_open = false;
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
    pub(in crate::state) fn has_pending_writes_under(&self, target: &Path) -> bool {
        self.documents.iter().any(|doc| {
            let writing = self
                .worker_busy
                .as_ref()
                .is_some_and(|(id, _)| id == &doc.id)
                || self.pending_track.as_ref() == Some(&doc.id)
                || self
                    .session_start_inflight
                    .as_ref()
                    .is_some_and(|(id, _)| id == &doc.id);
            let file = matches!(doc.id, DocId::File(_)) && doc.path.starts_with(target);
            let mirror = doc
                .mirror_path
                .as_ref()
                .is_some_and(|path| path.starts_with(target));
            (file || mirror) && (writing || doc.is_dirty())
        })
    }
    pub(crate) fn submit_dialog(&mut self, tx: &mut Tx) {
        let Some(dialog) = self.chrome.active_dialog.clone() else {
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
        let input = self.chrome.dialog_input.text.trim().to_owned();
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
                    Err(err) => self.chrome.dialog_error = Some(err),
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
                    Err(err) => self.chrome.dialog_error = Some(err),
                }
            }
            DialogKind::Rename { rel_path } => {
                let old_abs = root.join(&rel_path);
                let old_canon = old_abs.canonicalize().unwrap_or(old_abs);
                if self.has_pending_writes_under(&old_canon) {
                    self.chrome.dialog_error = Some(
                        "Wait for saves to finish or close unsaved files before renaming them."
                            .into(),
                    );
                    return;
                }
                let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
                let link_targets: Vec<(String, PathBuf, bool)> = self
                    .store
                    .session_links
                    .iter()
                    .map(|(key, link)| {
                        let resolved = resolve_mirror_path(&link.workspace_file, Some(&root_canon));
                        let canon = resolved.canonicalize().unwrap_or(resolved);
                        (key.clone(), canon, link.workspace_file.is_relative())
                    })
                    .collect();
                match workspace_tree::rename_entry(&root, &rel_path, &input) {
                    Ok(renamed) => {
                        let new_canon = renamed.canonicalize().unwrap_or(renamed);
                        for doc in &mut self.documents {
                            if matches!(doc.id, DocId::File(_))
                                && let Some(updated) =
                                    remap_renamed_path(&doc.path, &old_canon, &new_canon)
                            {
                                let old_id = doc.id.clone();
                                doc.path = updated.clone();
                                doc.id = DocId::File(updated.clone());
                                if updated == new_canon
                                    && let Some(name) =
                                        new_canon.file_name().and_then(|n| n.to_str())
                                {
                                    doc.title = name.to_owned();
                                }
                                if self
                                    .judge_inflight
                                    .as_ref()
                                    .is_some_and(|inflight| inflight.doc_id == old_id)
                                {
                                    self.judge_inflight = None;
                                    self.judge_armed = true;
                                }
                            }
                            if let Some(mirror) = doc.mirror_path.as_deref()
                                && let Some(updated) =
                                    remap_renamed_path(mirror, &old_canon, &new_canon)
                            {
                                doc.mirror_path = Some(updated);
                            }
                        }
                        let mut links_changed = false;
                        for (key, canon, was_relative) in link_targets {
                            if let Some(updated) =
                                remap_renamed_path(&canon, &old_canon, &new_canon)
                                && let Some(link) = self.store.session_links.get_mut(&key)
                            {
                                link.workspace_file = if was_relative {
                                    rel_path_from_abs(&root_canon, &updated)
                                        .map(PathBuf::from)
                                        .unwrap_or(updated)
                                } else {
                                    updated
                                };
                                links_changed = true;
                            }
                        }
                        if links_changed {
                            let _ = prefs::save_store(&self.store);
                        }
                        if let Some(rel) = rel_path_from_abs(&root, &new_canon) {
                            self.selected_tree_id = Some(rel);
                        }
                        self.close_dialog();
                        self.refresh_workspace_tree();
                        self.save_open_tabs_pref();
                        self.sync_window_title(tx);
                    }
                    Err(err) => self.chrome.dialog_error = Some(err),
                }
            }
            DialogKind::Delete { rel_path, .. } => {
                let target_abs = root.join(&rel_path);
                let target_canon = target_abs.canonicalize().unwrap_or(target_abs);
                if self.has_pending_writes_under(&target_canon) {
                    self.chrome.dialog_error =
                        Some("Save or close unsaved files before deleting them.".into());
                    return;
                }
                let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
                let deleted_link_keys: Vec<String> = self
                    .store
                    .session_links
                    .iter()
                    .filter_map(|(key, link)| {
                        let resolved = resolve_mirror_path(&link.workspace_file, Some(&root_canon));
                        let canon = resolved.canonicalize().unwrap_or(resolved);
                        canon.starts_with(&target_canon).then(|| key.clone())
                    })
                    .collect();
                match workspace_tree::delete_entry(&root, &rel_path) {
                    Ok(()) => {
                        let mut active_id = self.active_doc_ref().map(|doc| doc.id.clone());
                        self.documents
                            .retain(|d| d.is_session() || !d.path.starts_with(&target_canon));
                        for doc in &mut self.documents {
                            let hit_mirror = doc
                                .mirror_path
                                .as_ref()
                                .is_some_and(|path| path.starts_with(&target_canon));
                            let hit_file_session = matches!(doc.id, DocId::File(_))
                                && doc.path.starts_with(&target_canon);
                            if doc.session_id.is_some() && (hit_mirror || hit_file_session) {
                                doc.mirror_path = None;
                                if matches!(doc.id, DocId::File(_))
                                    && let (Some(project), Some(session_id)) =
                                        (doc.voice_project.clone(), doc.session_id.clone())
                                {
                                    let old_id = doc.id.clone();
                                    let new_id = DocId::Session {
                                        project: project.clone(),
                                        id: session_id.clone(),
                                    };
                                    doc.path =
                                        project.join("sessions").join(&session_id).join("after.md");
                                    doc.id = new_id.clone();
                                    if active_id.as_ref() == Some(&old_id) {
                                        active_id = Some(new_id);
                                    }
                                    if self
                                        .judge_inflight
                                        .as_ref()
                                        .is_some_and(|inflight| inflight.doc_id == old_id)
                                    {
                                        self.judge_inflight = None;
                                        self.judge_armed = true;
                                    }
                                }
                            }
                        }
                        let mut links_removed = false;
                        for key in deleted_link_keys {
                            links_removed |= self.store.session_links.remove(&key).is_some();
                        }
                        if links_removed {
                            let _ = prefs::save_store(&self.store);
                        }
                        self.active_doc = active_id
                            .as_ref()
                            .and_then(|id| self.documents.iter().position(|doc| &doc.id == id))
                            .or_else(|| {
                                self.active_doc
                                    .zip(self.documents.len().checked_sub(1))
                                    .map(|(index, last)| index.min(last))
                            });
                        let current = self
                            .active_doc
                            .and_then(|index| self.documents.get(index).map(|doc| doc.id.clone()));
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
                    Err(err) => self.chrome.dialog_error = Some(err),
                }
            }
            DialogKind::ConfirmClose { .. }
            | DialogKind::Recover { .. }
            | DialogKind::StartSession
            | DialogKind::ConfirmRevert => unreachable!(),
        }
    }
    pub(crate) fn confirm_revert(&mut self) {
        if self.can_revert() {
            self.chrome.dialog_error = None;
            self.chrome.active_dialog = Some(DialogKind::ConfirmRevert);
        }
    }
    pub(crate) fn can_track_session(&self) -> bool {
        let Some(doc) = self.active_doc_ref() else {
            return false;
        };
        self.voice_project.is_some()
            && matches!(doc.id, DocId::File(_))
            && doc.session_id.is_none()
            && doc.is_editable()
            && !doc.edit.text.trim().is_empty()
            && !workspace_tree::is_protected_voice_path(&doc.path)
            && self
                .workspace
                .as_ref()
                .is_none_or(|root| fs_io::is_contained_in(root, &doc.path))
    }
    pub(crate) fn open_track_session(&mut self) {
        if !self.can_track_session() {
            self.status = "This file cannot start an edit session".into();
            return;
        }
        let section = self
            .active_doc_ref()
            .map(section_for_document)
            .unwrap_or_else(|| "all".into());
        let section = if section == "all" {
            String::new()
        } else {
            section
        };
        self.session_origin = "ai-first".into();
        self.session_section = TextEdit::new(section);
        self.session_sources = TextEdit::new("");
        self.chrome.dialog_error = None;
        self.chrome.active_dialog = Some(DialogKind::StartSession);
    }
    pub(crate) fn confirm_track_session(&mut self) {
        if !self.can_track_session() {
            self.chrome.dialog_error = Some("This file cannot start an edit session".into());
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
    pub(in crate::state) fn maybe_start_pending_track(&mut self) {
        let Some(id) = self.pending_track.clone() else {
            return;
        };
        let Some(doc) = self.documents.iter().find(|doc| doc.id == id) else {
            self.pending_track = None;
            return;
        };
        if doc.save_error.is_some() {
            self.pending_track = None;
            self.status = doc
                .save_error
                .clone()
                .unwrap_or_else(|| "Save failed".into());
            return;
        }
        if doc.is_dirty() || doc.pending_save || self.worker_busy.is_some() {
            return;
        }
        self.pending_track = None;
        self.request_session_start();
    }
    pub(in crate::state) fn request_session_start(&mut self) {
        if !self.can_track_session() {
            self.status = "This file cannot start an edit session".into();
            return;
        }
        let Some(project) = self.voice_project.clone() else {
            return;
        };
        let Some(doc) = self.active_doc_ref() else {
            return;
        };
        let draft = doc.path.clone();
        if !draft.is_file() {
            self.status = "Save the draft to disk before tracking a session".into();
            return;
        }
        if !fs_io::is_contained_in(project.parent().unwrap_or(project.as_path()), &draft)
            && self
                .workspace
                .as_ref()
                .is_some_and(|root| !fs_io::is_contained_in(root, &draft))
        {
            self.status = "That draft is outside the workspace".into();
            return;
        }
        let doc_id = doc.id.clone();
        let generation = doc.generation;
        if self
            .session_start_inflight
            .as_ref()
            .is_some_and(|(id, _)| id == &doc_id)
        {
            return;
        }
        let section = self.session_section.text.trim();
        let section = if section.is_empty() || section == "all" {
            None
        } else {
            Some(section.to_owned())
        };
        let origin = if self.session_origin == "writer-first" {
            "writer-first"
        } else {
            "ai-first"
        };
        let sources = self
            .session_sources
            .text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Err(error) = self.bridge.request(
            Some(doc_id.clone()),
            generation,
            crate::bridge::BridgeOp::SessionStart {
                project,
                draft,
                section,
                origin: origin.into(),
                sources,
            },
        ) {
            self.status = error.to_string();
        } else {
            self.session_start_inflight = Some((doc_id, generation));
            self.status = "Starting edit session…".into();
        }
    }
    pub(in crate::state) fn session_start_is_stale(&self, doc_id: &DocId, generation: u64) -> bool {
        self.documents
            .iter()
            .find(|doc| &doc.id == doc_id)
            .is_some_and(|doc| {
                doc.generation != generation
                    || doc.is_dirty()
                    || doc.session_id.is_some()
                    || !doc.is_editable()
            })
    }
    pub(in crate::state) fn attach_started_session(
        &mut self,
        doc_id: Option<DocId>,
        generation: u64,
        started: crate::bridge::SessionStartResult,
    ) {
        let Some(doc_id) = doc_id else {
            self.status = "Edit session started without a document".into();
            return;
        };
        let Some(project) = self.voice_project.clone() else {
            return;
        };
        let folder = project.join("sessions").join(&started.id);
        let before = match fs_io::read_utf8(&folder.join("before.md")) {
            Ok(text) => text,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
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
        if self
            .documents
            .iter()
            .any(|doc| doc.matches_session(&project, &started.id))
        {
            self.status = format!("Edit session {} is already open", started.id);
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
                self.status = error.to_string();
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
        self.store.session_links.insert(
            key,
            prefs::SessionLink {
                workspace_file: mirror,
                last_synced_sha256: fs_io::sha256_text(&disk),
            },
        );
        if let Err(error) = prefs::save_store(&self.store) {
            self.status = format!("Session started, but the link was not saved: {error}");
        } else {
            self.status = format!("Tracking edit session {}", started.id);
            self.notify(
                "track-session".into(),
                format!("Edit session {} is tracking this file", started.id),
                None,
            );
        }
        self.sessions = workspace_tree::scan_sessions(&project, &self.store.session_links);
        self.refresh_diff();
    }
}
