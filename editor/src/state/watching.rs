use super::{Editor, JUDGE_DELAY, ViewMode, load_placeholder};
use crate::{
    documents::{DocId, DocumentBuffer, ExternalConflict},
    fs_io, prefs,
    state::notifications::NoticeAction,
    ui::reference::ReferenceSource,
    watcher::{WatchBatch, Watcher},
    worker::{InFlight, WakeFn},
    workspace_tree,
};
use rune::{TextEdit, Tx};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
pub(crate) struct WatchState {
    pub(super) wake: Option<Arc<WakeFn>>,
    watcher: Option<Watcher>,
    generation: u64,
    deferred: BTreeSet<PathBuf>,
}

impl Editor {
    pub(in crate::state) fn restart_watcher(&mut self) {
        self.watching.watcher = None;
        self.watching.generation = self.watching.generation.wrapping_add(1);
        self.watching.deferred.clear();
        self.studio = Default::default();
        self.external_compare = None;
        self.notifications = Default::default();
        let Some(wake) = self.watching.wake.clone() else {
            return;
        };
        if self.workspace.is_none() && self.voice_project.is_none() {
            return;
        }
        match Watcher::start(
            self.workspace.clone(),
            self.voice_project.clone(),
            self.watching.generation,
            wake,
        ) {
            Ok(watcher) => self.watching.watcher = Some(watcher),
            Err(error) => eprintln!("ghostless-editor: {error}"),
        }
    }

    pub(in crate::state) fn stop_watcher(&mut self) {
        self.watching.watcher = None;
    }

    pub(in crate::state) fn poll_watcher(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        while let Some(batch) = self
            .watching
            .watcher
            .as_ref()
            .and_then(|watcher| watcher.batches.try_recv().ok())
        {
            changed |= self.apply_watch_batch(batch, tx);
        }
        if self.worker_busy.is_none() && !self.watching.deferred.is_empty() {
            let modified_files = std::mem::take(&mut self.watching.deferred);
            changed |= self.apply_watch_batch(
                WatchBatch {
                    generation: self.watching.generation,
                    modified_files,
                    ..Default::default()
                },
                tx,
            );
        }
        changed
    }

    pub(in crate::state) fn apply_watch_batch(&mut self, batch: WatchBatch, tx: &mut Tx) -> bool {
        if batch.generation != self.watching.generation {
            return false;
        }
        if batch.tree_changed {
            self.refresh_workspace_tree();
            if self
                .selected_tree_id
                .as_deref()
                .is_some_and(|id| !self.file_tree.contains(id))
            {
                self.selected_tree_id = None;
            }
        }
        let mut changed = batch.tree_changed || batch.voice_data_changed;
        if let Some(ReferenceSource::File(path)) = self.reference.source.as_ref()
            && (batch.modified_files.contains(path)
                || (batch.voice_data_changed
                    && self
                        .voice_project
                        .as_ref()
                        .is_some_and(|root| path.starts_with(root))))
        {
            self.reference.cached_content = fs_io::read_utf8(path).unwrap_or_default();
            changed = true;
        }
        if batch.voice_data_changed {
            self.placeholder_token = load_placeholder(self.voice_project.as_deref());
            self.invalidate_voice_analysis();
            if let Some(voice) = &self.voice_project {
                self.sessions = workspace_tree::scan_sessions(voice, &self.store.session_links);
            }
            self.refresh_voice_review();
        }

        for path in batch.modified_files {
            if self.worker_busy.as_ref().is_some_and(|(id, kind)| {
                *kind != InFlight::Recovery
                    && self
                        .documents
                        .iter()
                        .any(|doc| &doc.id == id && watches(doc, &path))
            }) {
                self.watching.deferred.insert(path);
                continue;
            }
            if !self.documents.iter().any(|doc| watches(doc, &path)) {
                continue;
            }
            let Ok(disk_text) = fs_io::read_utf8(&path) else {
                continue;
            };
            let disk_sha256 = fs_io::sha256_text(&disk_text);
            for index in 0..self.documents.len() {
                if !watches(&self.documents[index], &path) {
                    continue;
                }
                changed |= self.observe_external(
                    index,
                    ExternalConflict {
                        path: path.clone(),
                        disk_text: disk_text.clone(),
                        disk_sha256: disk_sha256.clone(),
                    },
                    tx,
                );
            }
        }
        changed
    }

    fn observe_external(&mut self, index: usize, disk: ExternalConflict, tx: &mut Tx) -> bool {
        let doc = &self.documents[index];
        let is_mirror = doc.mirror_path.as_ref() == Some(&disk.path);
        let same = doc.last_disk_sha256 == disk.disk_sha256
            || (is_mirror
                && Self::mirror_sync_hash(&self.store, doc)
                    .is_some_and(|sha| sha == disk.disk_sha256));
        if same {
            if doc
                .external_conflict
                .as_ref()
                .is_some_and(|conflict| conflict.path == disk.path)
            {
                let doc = &mut self.documents[index];
                doc.external_conflict = None;
                doc.save_error = None;
                if self.active_doc == Some(index) {
                    self.leave_external_compare(tx);
                }
                return true;
            }
            return false;
        }
        if doc
            .external_conflict
            .as_ref()
            .is_some_and(|conflict| conflict == &disk)
        {
            return false;
        }
        if doc.is_dirty() || self.finish_modal_for(&doc.id) {
            let doc = &mut self.documents[index];
            doc.external_conflict = Some(disk);
            doc.force_mirror = false;
            if !doc.is_session() {
                doc.pending_save = false;
                doc.autosave_due = None;
            }
            if self.active_doc == Some(index) && self.is_external_compare() {
                self.refresh_diff();
            }
        } else {
            let id = doc.id.clone();
            let title = doc.title.clone();
            self.accept_disk(index, disk, tx);
            self.notify(
                format!("disk-{:?}", id),
                format!("{title} updated on disk"),
                Some(NoticeAction::Compare(id)),
            );
        }
        true
    }

    fn accept_disk(&mut self, index: usize, disk: ExternalConflict, tx: &mut Tx) {
        let doc = &mut self.documents[index];
        let mirror = doc.mirror_path.as_ref() == Some(&disk.path);
        doc.external_prev = Some(std::mem::take(&mut doc.edit.text));
        doc.edit = TextEdit::new(disk.disk_text);
        doc.external_conflict = None;
        doc.conflict = None;
        doc.generation += 1;
        doc.saved_generation = doc.generation;
        doc.pending_save = false;
        doc.autosave_due = None;
        doc.recovery_due = None;
        doc.pending_recovery = false;
        doc.save_error = None;
        doc.finish_error = None;
        doc.cached_judge = None;
        doc.latest_judge_request = None;
        doc.decoration_cache.get_mut().take();
        doc.last_disk_sha256 = disk.disk_sha256.clone();
        doc.last_saved_label = Some(DocumentBuffer::format_saved_utc());
        if mirror {
            update_mirror_baseline(&mut self.store, doc, disk.disk_sha256);
            if doc.is_session() && doc.can_save() {
                // The agent changed the workspace file. Persist its new contents
                // to the local session copy under the same guarded mirror hash.
                doc.generation += 1;
                doc.pending_save = true;
            }
        }
        if !doc.is_session()
            && doc.recovery_path().is_file()
            && let Err(error) = crate::worker::clear_recovery_copy(&doc.recovery_path())
        {
            doc.recovery_error = Some(error.to_string());
        }
        if self.active_doc == Some(index) {
            self.find
                .document_changed(&doc.edit.text, doc.edit.range().0);
            self.refresh_diff();
            self.request_judge(tx.clock());
            tx.wake_after(JUDGE_DELAY);
        }
    }

    pub(crate) fn keep_external_mine(&mut self, tx: &mut Tx) {
        if self.active_is_locked() {
            return;
        }
        let Some(index) = self.active_doc else { return };
        let doc = &mut self.documents[index];
        let Some(conflict) = doc.external_conflict.take() else {
            return;
        };
        if !doc.is_dirty() {
            doc.generation += 1;
        }
        doc.last_disk_sha256 = conflict.disk_sha256.clone();
        if doc.mirror_path.as_ref() == Some(&conflict.path) {
            update_mirror_baseline(&mut self.store, doc, conflict.disk_sha256);
            doc.conflict = None;
        }
        doc.force_mirror = false;
        doc.save_error = None;
        self.leave_external_compare(tx);
        self.status = "Kept your edits. The next save will replace the disk copy.".into();
    }

    pub(crate) fn reload_external_disk(&mut self, tx: &mut Tx) {
        if self.active_is_locked() {
            return;
        }
        let Some(index) = self.active_doc else { return };
        let Some(conflict) = self.documents[index].external_conflict.take() else {
            return;
        };
        self.accept_disk(index, conflict, tx);
        self.leave_external_compare(tx);
        self.status = "Reloaded the disk copy.".into();
    }

    fn leave_external_compare(&mut self, tx: &mut Tx) {
        if self.external_compare.is_some() {
            self.set_view_mode(ViewMode::Write, tx);
        }
        self.focus_document(tx);
    }

    pub(crate) fn compare_active_external(&mut self, tx: &mut Tx) {
        if let Some(id) = self.active_doc_ref().map(|doc| doc.id.clone()) {
            self.compare_external(&id, tx);
        }
    }

    pub(in crate::state) fn compare_external(&mut self, id: &DocId, tx: &mut Tx) {
        if !self
            .documents
            .iter()
            .any(|doc| &doc.id == id && external_sides(doc).is_some())
        {
            return;
        }
        self.activate_tab(&id.tab_key(), tx);
        self.external_compare = Some(id.clone());
        self.conflict_compare = false;
        self.set_view_mode(ViewMode::Compare, tx);
        self.focus_document(tx);
    }

    pub(crate) fn is_external_compare(&self) -> bool {
        self.external_compare_sides().is_some()
    }

    pub(in crate::state) fn external_compare_sides(&self) -> Option<(&str, &str)> {
        let doc = self
            .active_doc_ref()
            .filter(|doc| self.external_compare.as_ref() == Some(&doc.id))?;
        external_sides(doc)
    }

    pub(in crate::state) fn workspace_generation(&self) -> u64 {
        self.watching.generation
    }
}

fn watches(doc: &DocumentBuffer, path: &Path) -> bool {
    doc.path == path || doc.mirror_path.as_deref() == Some(path)
}

fn external_sides(doc: &DocumentBuffer) -> Option<(&str, &str)> {
    if let Some(conflict) = &doc.external_conflict {
        Some((&doc.edit.text, &conflict.disk_text))
    } else {
        doc.external_prev
            .as_deref()
            .map(|previous| (previous, doc.edit.text.as_str()))
    }
}

fn update_mirror_baseline(store: &mut prefs::EditorStateStore, doc: &DocumentBuffer, sha: String) {
    if let Some((project, id)) = doc.voice_project.as_ref().zip(doc.session_id.as_ref())
        && let Some(link) = store
            .session_links
            .get_mut(&prefs::EditorStateStore::session_link_key(project, id))
    {
        link.last_synced_sha256 = sha;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::ViewMode, test_util::TestDir, worker::execute_write_op};

    fn open_file(root: &TestDir) -> (Editor, PathBuf) {
        let path = root.join("draft.md");
        std::fs::write(&path, "Café original.").unwrap();
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            Box::new(|_, _| Ok(String::new())),
            Box::new(execute_write_op),
            Box::new(|| {}),
        );
        editor.documents.push(DocumentBuffer::from_workspace_file(
            path.clone(),
            "draft.md".into(),
            "Café original.".into(),
            false,
            true,
            true,
            None,
        ));
        editor.active_doc = Some(0);
        (editor, path)
    }

    fn observe(editor: &mut Editor, path: &Path, tx: &mut Tx) -> bool {
        editor.apply_watch_batch(
            WatchBatch {
                generation: editor.watching.generation,
                modified_files: BTreeSet::from([path.to_owned()]),
                ..Default::default()
            },
            tx,
        )
    }

    #[test]
    fn defers_in_flight_write_then_suppresses_its_own_hash() {
        let root = TestDir::new("watch-self");
        let (mut editor, path) = open_file(&root);
        let mut tx = Tx::default();
        editor
            .active_working_edit_mut()
            .replace_text("Saved by the editor.");
        editor.changed_active(&mut tx);
        editor.save_clicked();
        assert!(editor.worker_busy.is_some());
        assert!(!observe(&mut editor, &path, &mut tx));
        assert!(editor.watching.deferred.contains(&path));
        editor.drain_until_idle();
        assert!(!editor.poll_watcher(&mut tx));
        assert!(editor.notifications.stack.entries().is_empty());
        assert!(editor.active_doc_ref().unwrap().external_conflict.is_none());
    }

    #[test]
    fn clean_external_reload_notifies_and_compares_previous_text() {
        let root = TestDir::new("watch-clean");
        let (mut editor, path) = open_file(&root);
        let mut tx = Tx::default();
        std::fs::write(&path, "Café external.").unwrap();
        assert!(!editor.apply_watch_batch(
            WatchBatch {
                generation: editor.watching.generation.wrapping_add(1),
                modified_files: BTreeSet::from([path.clone()]),
                ..Default::default()
            },
            &mut tx
        ));
        assert!(observe(&mut editor, &path, &mut tx));
        let doc = editor.active_doc_ref().unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(doc.edit.text, "Café external.");
        assert_eq!(doc.external_prev.as_deref(), Some("Café original."));
        assert_eq!(doc.last_disk_sha256, fs_io::sha256_text("Café external."));
        let entry = editor.notifications.stack.front().unwrap();
        assert_eq!(entry.options.text, "draft.md updated on disk");
        let id = entry.id;
        let action = entry.options.action.clone().unwrap();
        editor.run_notice_action(id, &action, &mut tx);
        assert_eq!(editor.view_mode, ViewMode::Compare);
        assert_eq!(editor.compare_left_text(), Some("Café original."));
        assert_eq!(editor.compare_right_text(), Some("Café external."));
        assert!(!editor.can_revert());
    }

    #[test]
    fn dirty_external_conflict_can_keep_mine_or_reload() {
        let root = TestDir::new("watch-dirty");
        let (mut editor, path) = open_file(&root);
        let mut tx = Tx::default();
        editor.active_working_edit_mut().replace_text("My draft.");
        editor.changed_active(&mut tx);
        std::fs::write(&path, "First external edit.").unwrap();
        assert!(observe(&mut editor, &path, &mut tx));
        assert_eq!(editor.active_working_edit().text, "My draft.");
        assert!(editor.active_doc_ref().unwrap().is_dirty());
        editor.compare_active_external(&mut tx);
        assert_eq!(editor.compare_left_text(), Some("My draft."));
        assert_eq!(editor.compare_right_text(), Some("First external edit."));
        editor.keep_external_mine(&mut tx);
        assert_eq!(
            editor.active_doc_ref().unwrap().last_disk_sha256,
            fs_io::sha256_text("First external edit.")
        );
        editor.save_clicked();
        editor.drain_until_idle();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "My draft.");

        editor
            .active_working_edit_mut()
            .replace_text("Unsaved edit.");
        editor.changed_active(&mut tx);
        std::fs::write(&path, "Second external edit.").unwrap();
        assert!(observe(&mut editor, &path, &mut tx));
        editor.reload_external_disk(&mut tx);
        let doc = editor.active_doc_ref().unwrap();
        assert_eq!(doc.edit.text, "Second external edit.");
        assert!(doc.external_conflict.is_none() && !doc.is_dirty());
        assert_eq!(
            doc.last_disk_sha256,
            fs_io::sha256_text("Second external edit.")
        );
    }
}
