use super::Editor;
use crate::{
    bridge::{FinishOptions, FinishReceipt},
    documents::{DocId, DocumentBuffer, count_words},
    error::EditorError,
    fs_io, hunks, markdown_decor,
    worker::WriteOutcome,
    workspace_tree,
};
use rune::{FileDialogOptions, TextEdit, Tx};
use serde::Deserialize;
use std::ops::Range;

#[derive(Default, Deserialize)]
struct Provenance {
    origin: Option<String>,
    section: Option<String>,
}

pub(crate) struct FinishModal {
    doc_id: DocId,
    pub(crate) notes: TextEdit,
    pub(crate) check: TextEdit,
    pub(crate) rebuild: bool,
    pub(crate) promote: bool,
    pub(crate) error: Option<EditorError>,
    pub(crate) submitted: bool,
    pub(crate) ai_first: bool,
    writer_first: bool,
    pub(crate) preview: String,
    pub(crate) provenance: String,
    pub(crate) sync_status: String,
    pub(crate) standalone: bool,
    pub(crate) identical: bool,
    pub(crate) placeholders: Vec<Range<usize>>,
}

impl FinishModal {
    fn new(doc: &DocumentBuffer, token: Option<&str>, workspace: Option<&std::path::Path>) -> Self {
        let before = doc.original.as_deref().unwrap_or_default();
        let after = &doc.edit.text;
        let identical = before.trim_end_matches('\n') == after.trim_end_matches('\n');
        let placeholders = markdown_decor::find_placeholder_ranges(after, token);
        let provenance: Provenance = doc
            .voice_project
            .as_ref()
            .zip(doc.session_id.as_ref())
            .and_then(|(project, id)| {
                std::fs::read(project.join("sessions").join(id).join("session.json")).ok()
            })
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let old_words = count_words(before);
        let new_words = count_words(after);
        let hunks = hunks::diff_hunks(before, after).len();
        let linked = doc.mirror_path.as_ref();
        let sync_status = match linked {
            Some(_) if doc.conflict.is_some() => {
                "Workspace file has diverged. The session keeps a local copy.".into()
            }
            Some(path) => {
                let path = workspace
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(path);
                format!("Workspace file {} is up to date.", path.display())
            }
            None => format!(
                "Standalone session (sessions/{}/after.md)",
                doc.session_id.as_deref().unwrap_or_default()
            ),
        };
        Self {
            doc_id: doc.id.clone(),
            notes: doc.notes.clone().unwrap_or_default(),
            check: TextEdit::default(),
            rebuild: true,
            promote: false,
            error: None,
            submitted: false,
            ai_first: provenance.origin.as_deref() == Some("ai-first"),
            writer_first: provenance.origin.as_deref() == Some("writer-first"),
            preview: format!(
                "Words {old_words} → {new_words} ({:+}) · {hunks} changed hunk(s)",
                new_words as i64 - old_words as i64
            ),
            provenance: format!(
                "{} · {}",
                provenance.origin.as_deref().unwrap_or("Unknown origin"),
                provenance.section.as_deref().unwrap_or("All sections")
            ),
            sync_status,
            standalone: linked.is_none(),
            identical,
            placeholders,
        }
    }

    pub(crate) fn can_confirm(&self) -> bool {
        !self.identical && !self.submitted
    }

    pub(crate) fn tier(&self) -> &str {
        if self.ai_first && !self.promote {
            "edited tier"
        } else if self.ai_first || self.writer_first {
            "gold tier"
        } else {
            "edited tier"
        }
    }
}

impl Editor {
    pub(crate) fn can_open_finish(&self) -> bool {
        self.active_is_session()
            && !self.active_is_locked()
            && self
                .active_doc_ref()
                .is_some_and(|doc| doc.external_conflict.is_none())
            && self.chrome.finish.is_none()
            && self.chrome.finish_wait.is_none()
    }

    pub(crate) fn finish(&mut self) {
        if !self.can_open_finish() {
            return;
        }
        let Some(doc) = self.active_doc_ref() else {
            return;
        };
        self.chrome.finish_wait = Some(doc.id.clone());
        self.save_clicked();
    }

    pub(in crate::state) fn finish_modal_for(&self, id: &DocId) -> bool {
        self.chrome
            .finish
            .as_ref()
            .is_some_and(|modal| &modal.doc_id == id)
    }

    pub(in crate::state) fn poll_finish(&mut self) -> bool {
        let Some(id) = &self.chrome.finish_wait else {
            return false;
        };
        let Some(doc) = self.active_doc_ref().filter(|doc| &doc.id == id) else {
            self.chrome.finish_wait = None;
            return true;
        };
        if self
            .worker_busy
            .as_ref()
            .is_some_and(|(busy, _)| busy == id)
            || doc.pending_save
        {
            return false;
        }
        if !doc.is_dirty() {
            self.chrome.finish = Some(FinishModal::new(
                doc,
                self.placeholder_token.as_deref(),
                self.workspace.as_deref(),
            ));
        }
        self.chrome.finish_wait = None;
        true
    }

    pub(crate) fn dismiss_finish(&mut self) {
        if self
            .chrome
            .finish
            .as_ref()
            .is_some_and(|modal| modal.submitted)
        {
            return;
        }
        if let Some(modal) = self.chrome.finish.take()
            && let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == modal.doc_id)
            && doc
                .notes
                .as_ref()
                .is_some_and(|notes| notes.text != modal.notes.text)
        {
            doc.notes = Some(modal.notes);
            doc.generation += 1;
            doc.pending_save = true;
        }
        self.close_dialog();
        self.dispatch_next();
    }

    pub(crate) fn jump_to_finish_placeholder(&mut self, tx: &mut Tx) {
        let Some(range) = self
            .chrome
            .finish
            .as_ref()
            .and_then(|modal| modal.placeholders.first())
            .cloned()
        else {
            return;
        };
        self.dismiss_finish();
        self.jump_to_range(range.start, range.end, tx);
    }

    pub(crate) fn confirm_finish(&mut self) {
        if self.active_is_locked()
            || self
                .active_doc_ref()
                .is_some_and(|doc| doc.external_conflict.is_some())
        {
            return;
        }
        let Some(id) = self.active_doc_ref().map(|doc| doc.id.clone()) else {
            return;
        };
        let Some(modal) = self
            .chrome
            .finish
            .as_mut()
            .filter(|modal| modal.doc_id == id && modal.can_confirm())
        else {
            return;
        };
        let check = modal.check.text.trim();
        self.finish_options = Some(FinishOptions {
            note: Some(modal.notes.text.clone()),
            check: (!check.is_empty()).then(|| check.to_owned()),
            rebuild: modal.rebuild,
            promote: modal.ai_first && modal.promote,
        });
        modal.submitted = true;
        modal.error = None;
        self.queue_finish();
    }

    pub(crate) fn save_final_copy(&mut self, tx: &mut Tx) {
        let Some(modal) = self
            .chrome
            .finish
            .as_ref()
            .filter(|modal| modal.standalone && !modal.submitted)
        else {
            return;
        };
        let Some(doc) = self.active_doc_ref().filter(|doc| doc.id == modal.doc_id) else {
            return;
        };
        let id = doc.id.clone();
        let generation = doc.generation;
        let contents = doc.edit.text.clone();
        let name = std::path::Path::new(&doc.title)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("draft");
        tx.save_file(
            FileDialogOptions {
                title: "Save Final Copy".into(),
                file_name: Some(format!("{name}-final.md")),
                ..Default::default()
            },
            move |editor: &mut Editor, result, _| {
                if editor
                    .active_doc_ref()
                    .is_none_or(|doc| doc.id != id || doc.generation != generation)
                {
                    return;
                }
                let Some(modal) = editor
                    .chrome
                    .finish
                    .as_mut()
                    .filter(|modal| modal.doc_id == id && !modal.submitted)
                else {
                    return;
                };
                let result = match result {
                    Ok(Some(path)) if workspace_tree::is_protected_voice_path(&path) => Err(
                        EditorError::ProtectedPath(format!("Cannot overwrite {}", path.display())),
                    ),
                    Ok(Some(path)) => fs_io::atomic_write_text(&path, &contents),
                    Ok(None) => return,
                    Err(error) => Err(EditorError::Io {
                        context: error.to_string(),
                    }),
                };
                match result {
                    Ok(()) => editor.status = "Saved final copy".into(),
                    Err(error) => modal.error = Some(error),
                }
            },
        );
    }

    pub(in crate::state) fn apply_finished_reply(
        &mut self,
        doc_id: DocId,
        generation: u64,
        snapshot: Option<(String, WriteOutcome)>,
        result: Result<FinishReceipt, EditorError>,
    ) -> bool {
        let mut finished_ok = false;
        let mut receipt = None;
        let mut error_out = None;
        if let Some(doc) = self
            .documents
            .iter_mut()
            .find(|d| d.id == doc_id && d.generation == generation)
        {
            let warning = snapshot.and_then(|(sha, outcome)| {
                Self::saved_snapshot(&mut self.store, doc, generation, sha, outcome)
            });
            doc.finish_requested = false;
            match result {
                Ok(finished) => {
                    receipt = Some(finished);
                    doc.finished = true;
                    doc.read_only = true;
                    doc.pending_save = false;
                    doc.autosave_due = None;
                    doc.save_error = None;
                    doc.mirror_path = None;
                    doc.force_mirror = false;
                    doc.conflict = None;
                    doc.finish_error = None;
                    if matches!(doc.id, DocId::File(_))
                        && let (Some(project), Some(session_id)) =
                            (doc.voice_project.clone(), doc.session_id.clone())
                    {
                        doc.id = DocId::Session {
                            project: project.clone(),
                            id: session_id.clone(),
                        };
                        doc.path = project.join("sessions").join(&session_id).join("after.md");
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
                    error_out = Some(error);
                }
            }
        }
        if let Some(receipt) = receipt {
            self.finish_succeeded(&doc_id, receipt);
        }
        if let Some(error) = error_out {
            self.finish_failed(&doc_id, error);
        }
        finished_ok
    }

    pub(in crate::state) fn finish_failed(&mut self, id: &DocId, error: EditorError) {
        if let Some(modal) = self
            .chrome
            .finish
            .as_mut()
            .filter(|modal| &modal.doc_id == id)
        {
            modal.error = Some(error);
            modal.submitted = false;
        }
    }

    pub(in crate::state) fn finish_succeeded(&mut self, id: &DocId, receipt: FinishReceipt) {
        if self
            .chrome
            .finish
            .as_ref()
            .is_some_and(|modal| &modal.doc_id == id)
            && let Some(modal) = self.chrome.finish.take()
        {
            if let Some(doc) = self.active_doc_mut() {
                doc.notes = Some(modal.notes);
            }
            self.close_dialog();
        }
        if receipt.rebuilt {
            self.invalidate_voice_analysis();
        }
        if !receipt.warnings.is_empty() {
            self.status = format!("Finished. {}", receipt.warnings.join(" "));
        }
        if let Some(path) = receipt.learned_path {
            self.notify(
                format!("finished-{path}"),
                format!("Finished · Learned to {path}"),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{prefs, session::Session, test_util::TestDir, worker::WriteOutcome};

    fn editor(root: &TestDir, after: &str) -> Editor {
        let id = "2026-01-02-notes";
        let folder = root.join("sessions").join(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("session.json"),
            format!(r#"{{"kind":"ghostless-edit-session","id":"{id}","origin":"ai-first","section":"notes","status":"open","sourceDraftName":"draft.md","createdAt":"2026-01-02T00:00:00Z"}}"#)
        ).unwrap();
        Editor::with_handlers(
            Session {
                project: root.to_path_buf(),
                id: id.into(),
                source_name: "draft.md".into(),
                original: "Café original.".into(),
                after: after.into(),
                notes: "Original note".into(),
                before_sha256: "test".into(),
                finished: false,
            },
            prefs::EditorStateStore::default(),
            Box::new(move |args, input| {
                assert_eq!(args, ["bridge"]);
                let params: serde_json::Value = serde_json::from_str(input.unwrap()).unwrap();
                let params = &params["params"];
                assert_eq!(params["note"], "Keep this modal note");
                if params["checks"] == serde_json::json!(["Bad check"]) {
                    return Ok(r#"{"id":1,"ok":false,"error":"Quote a changed phrase."}"#.into());
                }
                std::fs::write(folder.join("receipt.json"), "{}").unwrap();
                Ok(r#"{"id":1,"ok":true,"result":{"learnedPath":"gold/2026-01-02-notes","rebuilt":true}}"#.into())
            }),
            Box::new(|_| Ok(WriteOutcome::Local)),
            Box::new(|| {}),
        )
    }

    #[test]
    fn preflight_blocks_identical_text_and_selects_first_utf8_placeholder() {
        let root = TestDir::new("finish-preflight");
        let mut editor = editor(&root, "Café original.\n");
        let mut tx = Tx::default();
        editor.finish();
        assert!(editor.poll_finish());
        assert!(!editor.chrome.finish.as_ref().unwrap().can_confirm());
        editor.dismiss_finish();

        editor
            .active_working_edit_mut()
            .replace_text("---\r\nsection: notes\r\n---\r\nCafé [WRITER: add a detail]");
        editor.set_view_mode(super::super::ViewMode::Preview, &mut tx);
        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        rt.update(|editor, _| {
            editor.finish();
            editor.poll_finish();
        });
        rt.frame(0.1);
        assert!(rt.elements().iter().any(|element| element.text.as_deref() == Some("⚠ 1 placeholder(s) still in draft")));
        assert!(rt.state.chrome.finish.as_ref().unwrap().can_confirm());
        rt.update(|editor, tx| {
            editor.jump_to_finish_placeholder(tx);
            super::super::poll(editor, 0.2, tx);
        });
        rt.frame(0.2);
        assert!(rt.state.chrome.finish.is_none());
        assert_eq!(
            rt.state.active_working_edit().selected(),
            "[WRITER: add a detail]"
        );
        assert!(
            rt.elements()
                .iter()
                .any(|element| element.focused && element.key.as_deref() == Some("prose-input"))
        );
    }

    #[test]
    fn rejected_check_keeps_modal_then_success_locks_and_refreshes() {
        let root = TestDir::new("finish-retry");
        let mut editor = editor(&root, "Café final.");
        editor.finish();
        editor.poll_finish();
        let modal = editor.chrome.finish.as_mut().unwrap();
        assert!(modal.rebuild && modal.ai_first && !modal.promote);
        modal.notes.replace_text("Keep this modal note");
        modal.check.replace_text("Bad check");
        editor.confirm_finish();
        editor.drain_until_idle();

        let modal = editor.chrome.finish.as_mut().unwrap();
        assert_eq!(
            modal.error.as_ref().unwrap().to_string(),
            "Quote a changed phrase."
        );
        assert_eq!(modal.notes.text, "Keep this modal note");
        assert_eq!(modal.check.text, "Bad check");
        assert!(modal.can_confirm());
        modal.check.replace_text("");
        assert!(editor.active_doc_ref().unwrap().is_editable());
        assert!(!root.join("sessions/2026-01-02-notes/receipt.json").exists());

        editor.confirm_finish();
        editor.drain_until_idle();
        let doc = editor.active_doc_ref().unwrap();
        assert!(doc.finished && doc.read_only && !doc.pending_save);
        assert_eq!(doc.notes.as_ref().unwrap().text, "Keep this modal note");
        assert!(editor.chrome.finish.is_none());
        assert!(
            editor
                .sessions
                .iter()
                .any(|item| item.status == crate::workspace_tree::SessionStatus::Finished)
        );
        assert_eq!(
            editor.notifications.stack.front().unwrap().options.text,
            "Finished · Learned to gold/2026-01-02-notes"
        );
    }
}
