use super::{
    Editor, FILE_AUTOSAVE_DELAY, JUDGE_DELAY, SESSION_AUTOSAVE_DELAY, section_for_document,
};
use crate::{
    documents::{DocId, DocumentBuffer},
    fs_io, prefs,
    worker::{InFlight, WorkerReply, WorkerRequest, WriteOutcome},
    workspace_tree,
};
use rune::Tx;
use std::{collections::HashSet, path::PathBuf, time::Duration};

impl Editor {
    pub(crate) fn toggle_autosave(&mut self) {
        self.store.ui.autosave_workspace_files = !self.store.ui.autosave_workspace_files;
        let _ = prefs::save_store(&self.store);
        if self.store.ui.autosave_workspace_files {
            self.flush_autosave_buffers();
        } else {
            for doc in &mut self.documents {
                if doc.is_session() {
                    continue;
                }
                doc.autosave_due = None;
                if doc.is_dirty() {
                    doc.recovery_due = Some(0.0);
                }
            }
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
            self.worker_busy
                .as_ref()
                .is_some_and(|(id, kind)| id == &doc.id && *kind == InFlight::Save)
        };

        let doc = &mut self.documents[idx];
        doc.pinned = true;
        doc.generation += 1;
        doc.close_after_save = false;
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
            self.find
                .document_changed(&doc.edit.text, doc.edit.range().0);
        }
        let section_now = section_for_document(doc);
        if doc
            .cached_judge
            .as_ref()
            .is_some_and(|cached| cached.section != section_now)
        {
            doc.cached_judge = None;
        }
        self.request_judge(tx.clock());
        tx.wake_after(JUDGE_DELAY);
    }
    pub(crate) fn save_clicked(&mut self) {
        let Some(idx) = self.active_doc else {
            return;
        };
        let doc = &mut self.documents[idx];
        if !doc.can_save() {
            return;
        }
        if !doc.is_dirty() && !doc.recovery_path().is_file() {
            if doc.conflict.is_some() {
                self.status =
                    "Resolve the disk conflict (Load Disk or Keep Session) before syncing to disk."
                        .into();
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
    pub(in crate::state) fn mirror_sync_hash(
        store: &prefs::EditorStateStore,
        doc: &DocumentBuffer,
    ) -> Option<String> {
        doc.mirror_path.as_ref()?;
        let (project, id) = doc.voice_project.as_ref().zip(doc.session_id.as_ref())?;
        let key = prefs::EditorStateStore::session_link_key(project, id);
        store
            .session_links
            .get(&key)
            .map(|link| link.last_synced_sha256.clone())
    }
    pub(in crate::state) fn saved_snapshot(
        store: &mut prefs::EditorStateStore,
        doc: &mut DocumentBuffer,
        generation: u64,
        sha256: String,
        outcome: WriteOutcome,
    ) -> Option<String> {
        doc.saved_generation = doc.saved_generation.max(generation);
        doc.last_disk_sha256 = sha256.clone();
        let key = doc
            .voice_project
            .as_ref()
            .zip(doc.session_id.as_ref())
            .map(|(project, id)| prefs::EditorStateStore::session_link_key(project, id));
        let link = key
            .as_ref()
            .and_then(|key| store.session_links.get_mut(key));
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
            WriteOutcome::MirrorSkipped(error) => Some(error.to_string()),
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
            let mirror_force = std::mem::take(&mut doc.force_mirror);
            doc.pending_save = false;
            let snapshot = (doc.is_dirty() || mirror_force).then(|| {
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
                mirror_force,
            }) {
                Ok(()) => {
                    self.worker_busy = Some((doc_id, InFlight::Finish));
                    self.status = "Finishing…".into();
                }
                Err(error) => {
                    doc.finish_requested = false;
                    doc.save_error = Some(error.to_string());
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
                    expected_sha256: Some(doc.last_disk_sha256.clone()),
                }
            };
            match self.worker.send(req) {
                Ok(()) => {
                    self.worker_busy = Some((doc_id, InFlight::Save));
                    doc.last_saved_label = Some("Saving…".into());
                    self.status = "Saving…".into();
                }
                Err(error) => {
                    doc.save_error = Some(error.to_string());
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
                    doc.save_error = Some(error.to_string());
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
                let mut mirrored: Option<(PathBuf, String)> = None;
                if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                    match result {
                        Ok(_) if generation < doc.saved_generation => {
                            // An older save finished after a newer one. Drop it so it cannot rewind the sync hash.
                        }
                        Ok(outcome) => {
                            if let WriteOutcome::Mirrored = outcome
                                && let Some(path) = doc.mirror_path.clone()
                            {
                                mirrored = Some((path, doc.edit.text.clone()));
                            }
                            let warning = Self::saved_snapshot(
                                &mut self.store,
                                doc,
                                generation,
                                sha256,
                                outcome,
                            );
                            let stale_finish =
                                doc.finish_error.is_some() && generation < doc.generation;
                            if !stale_finish {
                                doc.save_error = None;
                            }
                            if doc.finish_requested {
                                doc.pending_save = false;
                            } else if doc.is_dirty() {
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
                                if doc.close_after_save {
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
                            doc.close_after_save = false;
                            if doc.finish_error.is_none() || generation >= doc.generation {
                                doc.save_error = Some(error.to_string());
                            }
                            if doc.finish_requested {
                                doc.finish_requested = false;
                                doc.pending_save = false;
                                doc.finish_error = Some(error.to_string());
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
                if let Some((path, text)) = mirrored {
                    self.refresh_mirrored_tabs(&doc_id, &path, &text);
                }
                if should_close
                    && let Some(pos) = self.documents.iter().position(|d| d.id == doc_id)
                {
                    self.documents[pos].close_after_save = false;
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
                        doc.recovery_error = None;
                    }
                    true
                }
                Err(error) => {
                    if let Some(doc) = self.documents.iter_mut().find(|d| d.id == doc_id) {
                        doc.recovery_error = Some(error.to_string());
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
                            doc.mirror_path = None;
                            if matches!(doc.id, DocId::File(_))
                                && let (Some(project), Some(session_id)) =
                                    (doc.voice_project.clone(), doc.session_id.clone())
                            {
                                doc.id = DocId::Session {
                                    project: project.clone(),
                                    id: session_id.clone(),
                                };
                                doc.path =
                                    project.join("sessions").join(&session_id).join("after.md");
                                if self
                                    .judge_inflight
                                    .as_ref()
                                    .is_some_and(|inflight| inflight.doc_id == doc_id)
                                {
                                    self.judge_inflight = None;
                                }
                            }
                            doc.last_saved_label = Some("Finished".into());
                            self.status = warning.map_or_else(
                                || "Finished. Final and receipt saved locally.".into(),
                                |reason| format!("Finished locally. {reason}"),
                            );
                            if let Some(voice) = &self.voice_project {
                                self.sessions =
                                    workspace_tree::scan_sessions(voice, &self.store.session_links);
                            }
                            finished_ok = true;
                        }
                        Err(error) => {
                            doc.pending_save = false;
                            doc.finish_error = Some(error.to_string());
                            doc.save_error = Some(error.to_string());
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
    pub(in crate::state) fn drain_until_idle(&mut self) {
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
    pub(in crate::state) fn drain_single_doc_save(&mut self, doc_id: &DocId) -> bool {
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
    pub(in crate::state) fn drain_autosave_work(&mut self) -> bool {
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
                d.can_save()
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
                d.can_save()
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
        if self
            .documents
            .iter()
            .any(|doc| doc.save_error.is_some() || (doc.read_only && doc.is_dirty()))
        {
            success = false;
        }
        if !success {
            if let Some(error) = self
                .documents
                .iter()
                .find_map(|doc| doc.save_error.as_ref().or(doc.recovery_error.as_ref()))
            {
                self.status = format!("Workspace not changed: {error}");
            } else if !self.status.contains("worker stopped") {
                self.status = "Workspace not changed: a save failed".into();
            }
        }
        success
    }
    pub(in crate::state) fn show_close_error(&mut self) {
        let reason = self
            .status
            .strip_prefix("Workspace not changed: ")
            .unwrap_or(&self.status);
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
    pub(in crate::state) fn write_quit_recovery(&self) -> bool {
        let mut recovered_all = true;
        for doc in self.documents.iter().filter(|doc| doc.is_dirty()) {
            let path = doc.recovery_path();
            match doc
                .recovery_contents()
                .and_then(|text| fs_io::atomic_write_text(&path, &text))
            {
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

pub(crate) fn poll(editor: &mut Editor, clock: f64, tx: &mut Tx) -> bool {
    if !editor.fonts_initialized {
        let sans = if editor.store.ui.font_family == prefs::FontChoice::Serif {
            "Georgia, serif"
        } else {
            "Inter, sans-serif"
        };
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
