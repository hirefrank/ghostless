use super::{Editor, JUDGE_DELAY, JudgeInFlight, ViewMode};
use crate::{
    documents::{CachedJudge, DocId, DocumentBuffer},
    fs_io, prefs,
    worker::InFlight,
    workspace_tree,
};
use rune::{TextEdit, Tx};
use rune_kit::diff::{DiffMode, DiffState, FileDiff};

impl Editor {
    pub(crate) fn is_conflict_compare(&self) -> bool {
        self.conflict_compare
            && self
                .active_doc_ref()
                .is_some_and(|doc| doc.conflict.is_some())
    }

    /// Left side of Compare. Conflict compare shows the workspace file; every other compare shows the session baseline.
    pub(crate) fn compare_left_text(&self) -> Option<&str> {
        let doc = self.active_doc_ref()?;
        if let Some((left, _)) = self.external_compare_sides() {
            Some(left)
        } else if self.is_conflict_compare() {
            doc.conflict
                .as_ref()
                .map(|conflict| conflict.disk_text.as_str())
        } else {
            doc.original.as_deref()
        }
    }
    pub(crate) fn compare_right_text(&self) -> Option<&str> {
        let doc = self.active_doc_ref()?;
        Some(
            self.external_compare_sides()
                .map_or(doc.edit.text.as_str(), |(_, right)| right),
        )
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
        let current = self.compare_right_text().unwrap_or_default();
        let mut diff = DiffState::new(vec![FileDiff::from_texts(&title, &original, current)]);
        diff.set_mode(if self.compare_unified {
            DiffMode::Unified
        } else {
            DiffMode::Split
        });
        self.diff = diff;
    }
    pub(crate) fn compare_unified(&self) -> bool {
        self.compare_unified
    }
    pub(crate) fn set_compare_unified(&mut self, unified: bool) {
        self.compare_unified = unified;
        self.refresh_diff();
    }
    pub(crate) fn can_revert(&self) -> bool {
        self.view_mode == ViewMode::Compare
            && !self.is_conflict_compare()
            && !self.is_external_compare()
            && self
                .active_doc_ref()
                .is_some_and(|doc| doc.is_editable() && doc.original.is_some())
    }
    pub(crate) fn revert_to_original(&mut self, tx: &mut Tx) {
        if !self.can_revert() {
            self.focus_document(tx);
            return;
        }
        let Some(original) = self.active_doc_ref().and_then(|doc| doc.original.clone()) else {
            return;
        };
        let Some(doc) = self.active_doc_mut() else {
            return;
        };
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
        let Some(original) = self.active_doc_ref().and_then(|doc| doc.original.clone()) else {
            return;
        };
        let current = self.active_working_edit().text.clone();
        let Some(next) = crate::hunks::revert_hunk_text(&original, &current, index) else {
            self.focus_document(tx);
            return;
        };
        let Some(doc) = self.active_doc_mut() else {
            return;
        };
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
        let name = project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("voice");
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
            let name = project
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("voice");
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
        if self
            .judge_inflight
            .as_ref()
            .is_some_and(|inflight| inflight.doc_id == doc_id)
        {
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
        let ranges = crate::markdown_decor::find_placeholder_ranges(
            &doc.edit.text,
            self.placeholder_token.as_deref(),
        );
        if ranges.is_empty() {
            self.status = "No placeholders".into();
            self.focus_document(tx);
            return;
        }
        let index = ranges
            .iter()
            .position(|range| range.start > caret)
            .unwrap_or(0);
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
        let Some(doc) = self.active_doc_mut() else {
            return false;
        };
        let len = doc.edit.text.len();
        let start = start.min(len);
        let end = end.min(len);
        if start > end
            || !doc.edit.text.is_char_boundary(start)
            || !doc.edit.text.is_char_boundary(end)
        {
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
            self.status = if detail.is_empty() {
                query.to_owned()
            } else {
                detail.to_owned()
            };
        }
        if self.active_working_edit().text != before {
            self.active_working_edit_mut().replace_text(&before);
        }
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
            crate::bridge::BridgeOp::SaveNote {
                project,
                kind: kind.to_owned(),
                text,
            },
        ) {
            self.status = error.to_string();
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
        if !fs_io::is_contained_in(&voice, &path) || workspace_tree::is_protected_voice_path(&path)
        {
            self.status = format!("Protected voice data is read-only: {}", path.display());
            return;
        }
        if !path.is_file() {
            let stub = format!("# {name}\n\n");
            if let Err(error) = fs_io::atomic_write_text(&path, &stub) {
                self.status = error.to_string();
                return;
            }
        }
        self.open_workspace_file(path, true);
    }
    pub(crate) fn load_disk_conflict(&mut self, tx: &mut Tx) {
        if self.active_is_locked() {
            self.focus_document(tx);
            return;
        }
        let Some(disk) = self.active_doc_ref().and_then(|doc| {
            doc.conflict
                .as_ref()
                .map(|conflict| conflict.disk_text.clone())
        }) else {
            return;
        };
        let hash = fs_io::sha256_text(&disk);
        self.remember_link_hash(&hash);
        let Some(doc) = self.active_doc_mut() else {
            return;
        };
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
        if self
            .active_doc_ref()
            .and_then(|doc| doc.conflict.as_ref())
            .is_none()
        {
            return;
        }
        self.external_compare = None;
        self.conflict_compare = true;
        self.set_view_mode(ViewMode::Compare, tx);
        self.focus_document(tx);
    }
    pub(in crate::state) fn refresh_mirrored_tabs(
        &mut self,
        saved_id: &DocId,
        mirror: &std::path::Path,
        text: &str,
    ) {
        let hash = fs_io::sha256_text(text);
        let mut flagged = false;
        for doc in &mut self.documents {
            if &doc.id == saved_id || !doc.matches_workspace_file(mirror) {
                continue;
            }
            if doc.is_dirty() {
                doc.conflict = Some(crate::documents::SessionConflict {
                    disk_text: text.to_owned(),
                    session_text: doc.edit.text.clone(),
                });
                flagged = true;
            } else if !doc.finished {
                doc.edit = TextEdit::new(text);
                doc.last_disk_sha256 = hash.clone();
            }
        }
        if flagged {
            self.status = "A linked workspace file changed on disk.".into();
        }
    }
    pub(in crate::state) fn reconcile_active_linked_doc(&mut self) {
        let Some(index) = self.active_doc else { return };
        let doc_id = self.documents[index].id.clone();
        if self.documents[index].finished
            || self.documents[index].external_conflict.is_some()
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
                    self.status = error.to_string();
                    return;
                }
            };
            let disk_hash = fs_io::sha256_text(&disk);
            let buffer_hash = fs_io::sha256_text(&self.documents[index].edit.text);
            let sync = Self::mirror_sync_hash(&self.store, &self.documents[index]);
            let matches_sync = sync.as_deref() == Some(disk_hash.as_str());
            let matches_buffer = disk_hash == buffer_hash;
            if matches_buffer && !matches_sync && sync.is_some() {
                self.remember_link_hash(&disk_hash);
            }
            if !matches_sync && !matches_buffer {
                let session_text = self.documents[index].edit.text.clone();
                self.documents[index].conflict = Some(crate::documents::SessionConflict {
                    disk_text: disk,
                    session_text,
                });
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
        let Some((project, id, after, before, notes, before_sha)) =
            self.linked_session_files(&path)
        else {
            return;
        };
        let Ok(disk) = fs_io::read_utf8(&path) else {
            return;
        };
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
                doc.conflict = Some(crate::documents::SessionConflict {
                    disk_text: disk,
                    session_text: after,
                });
            } else {
                doc.conflict = Some(crate::documents::SessionConflict {
                    disk_text: disk,
                    session_text: buffer,
                });
            }
        }
        if !same {
            self.status = "This file changed on disk outside the edit session.".into();
        }
    }
    pub(in crate::state) fn invalidate_voice_analysis(&mut self) {
        for doc in &mut self.documents {
            doc.judge_epoch += 1;
            doc.latest_judge_request = None;
            doc.cached_judge = None;
        }
        self.judge_inflight = None;
        self.judge_armed = true;
    }

    pub(in crate::state) fn request_judge(&mut self, clock: f64) {
        let ready = self.voice_project.is_some() && self.active_doc_ref().is_some_and(judge_target);
        if !ready {
            self.judge_due = None;
            return;
        }
        self.judge_due = Some(clock + JUDGE_DELAY.as_secs_f64());
    }
    pub(in crate::state) fn dispatch_judge(&mut self) {
        let Some(project) = self.voice_project.clone() else {
            return;
        };
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
            crate::bridge::BridgeOp::JudgeText {
                project,
                text,
                section: section_param,
            },
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
            BridgeReply::Judge {
                request_id,
                doc_id,
                generation,
                section,
                result,
            } => {
                let mut result = *result;
                if result.section.is_empty() {
                    result.section = section.clone();
                }
                if self
                    .judge_inflight
                    .as_ref()
                    .is_some_and(|inflight| inflight.request_id == request_id)
                {
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
                let Some(voice_project) = voice_project else {
                    return;
                };
                doc.cached_judge = Some(CachedJudge {
                    voice_project,
                    judge_epoch: doc.judge_epoch,
                    section,
                    report: result,
                });
            }
            BridgeReply::Build { request_id, result } => {
                self.apply_voice_build(request_id, result);
            }
            BridgeReply::SaveNote { result, .. } => match result {
                Ok(saved) => {
                    self.status = format!("Saved note to {}", saved.path);
                    self.notify(
                        "voice-note".into(),
                        format!("Saved a note in {}", saved.path),
                        None,
                    );
                }
                Err(error) => {
                    self.status = format!("Could not save note: {error}");
                    self.notify("voice-note-error".into(), self.status.clone(), None);
                }
            },
            BridgeReply::SessionStart {
                doc_id,
                generation,
                result,
            } => {
                if let Some(id) = &doc_id
                    && self
                        .session_start_inflight
                        .as_ref()
                        .is_some_and(|(inflight, sent)| inflight == id && *sent == generation)
                {
                    self.session_start_inflight = None;
                }
                let stale = doc_id
                    .as_ref()
                    .is_some_and(|id| self.session_start_is_stale(id, generation));
                match result {
                    Ok(_started) if stale => {
                        self.status =
                            "The draft changed before the edit session could attach.".into();
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
                        error.to_string()
                    };
                }
            }
            BridgeReply::RulesList { request_id, result } => {
                self.apply_rules_list(request_id, result);
            }
            BridgeReply::ChecksList { request_id, result } => {
                self.apply_checks_list(request_id, result);
            }
            BridgeReply::RulesDecide { request_id, result } => {
                self.apply_voice_decision(request_id, result.map(|_| ()));
            }
            BridgeReply::ChecksDecide { request_id, result } => {
                self.apply_voice_decision(request_id, result.map(|_| ()));
            }
            BridgeReply::Promote { result } => {
                if let Err(error) = result {
                    self.status = error.to_string();
                }
            }
            BridgeReply::SessionsList { result } => {
                if let Err(error) = result {
                    self.status = error.to_string();
                }
            }
        }
    }
}

pub(in crate::state) fn judge_target(doc: &DocumentBuffer) -> bool {
    let name = doc
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    !matches!(name, "manifest.json" | "receipt.json")
}

pub(crate) fn section_for_document(doc: &DocumentBuffer) -> String {
    if let Some(section) = doc.section_override.as_deref() {
        return if section.is_empty() {
            "all".into()
        } else {
            section.to_owned()
        };
    }
    crate::markdown_decor::parse_frontmatter_fields(&doc.edit.text)
        .into_iter()
        .find(|(key, _)| key == "section")
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "all".into())
}
