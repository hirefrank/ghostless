use super::{Editor, ViewMode, tree_files};
use crate::{documents::DocId, worker::SearchReply};
use rune::Tx;
use std::time::Duration;

impl Editor {
    pub(crate) fn open_quick(&mut self, tx: &mut Tx) {
        use crate::quick_open::{QuickItem, QuickTarget};
        let mut items = Vec::new();
        let files = tree_files(&self.file_tree);
        for doc in self.documents.iter().rev() {
            if let Some(entry) = files.iter().find(|entry| entry.abs_path == doc.path) {
                items.push(QuickItem {
                    id: entry.rel_path.clone(),
                    title: doc.title.clone(),
                    subtitle: entry.rel_path.clone(),
                    target: QuickTarget::File(entry.abs_path.clone()),
                });
            }
        }
        for session in &self.sessions {
            items.push(QuickItem {
                id: format!("session:{}", session.id),
                title: session.source_name.clone(),
                subtitle: format!("session · {:?}", session.status).to_lowercase(),
                target: QuickTarget::Session(session.id.clone()),
            });
        }
        for entry in files {
            if items.iter().any(|item| item.id == entry.rel_path) {
                continue;
            }
            let title = entry
                .abs_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            items.push(QuickItem {
                id: entry.rel_path.clone(),
                title,
                subtitle: entry.rel_path,
                target: QuickTarget::File(entry.abs_path),
            });
        }
        self.quick.items = items;
        self.quick.query.replace_text("");
        self.quick.selected = 0;
        self.quick.open = true;
        tx.focus("quick-query", false);
    }
    pub(crate) fn choose_quick(&mut self, index: usize, tx: &mut Tx) {
        use crate::quick_open::QuickTarget;
        let item = crate::ui::quick_switcher::filtered(&self.quick)
            .get(index)
            .cloned();
        if let Some(item) = item {
            match item.target {
                QuickTarget::File(path) => self.open_workspace_file(path, true),
                QuickTarget::Session(id) => {
                    if let Some(project) = self.voice_project.clone() {
                        self.open_session_entry(project, id);
                    }
                }
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
        self.folder_search.due =
            (self.folder_search.query.text.chars().count() >= 2).then_some(tx.clock() + 0.2);
        if self.folder_search.due.is_some() {
            tx.wake_after(Duration::from_millis(200));
        }
    }
    pub(in crate::state) fn submit_folder_search(&mut self) {
        use crate::quick_open::MAX_SEARCH_FILE_BYTES;

        let buffers = self
            .documents
            .iter()
            .filter_map(|doc| {
                let path = match &doc.id {
                    DocId::File(path) => path.clone(),
                    DocId::Session { .. } if !doc.finished => {
                        let mirror = doc.mirror_path.as_ref()?;
                        mirror.canonicalize().unwrap_or_else(|_| mirror.clone())
                    }
                    DocId::Session { .. } => return None,
                };
                // An oversized open buffer is excluded, including its disk copy.
                let content =
                    (doc.edit.text.len() <= MAX_SEARCH_FILE_BYTES).then(|| doc.edit.text.clone());
                Some((path, content))
            })
            .collect();
        match self.worker.search.submit(
            tree_files(&self.file_tree),
            buffers,
            self.folder_search.query.text.clone(),
        ) {
            Ok(id) => self.folder_search.request_id = Some(id),
            Err(error) => self.folder_search.message = Some(error.to_string()),
        }
    }
    pub(in crate::state) fn accept_folder_search(&mut self, reply: SearchReply) -> bool {
        if !self.folder_search.open || self.folder_search.request_id != Some(reply.id) {
            return false;
        }
        self.folder_search.request_id = None;
        self.folder_search.matches = reply.matches;
        self.folder_search.selected = 0;
        true
    }
    pub(crate) fn choose_folder_match(&mut self, index: usize, tx: &mut Tx) {
        let Some(hit) = self.folder_search.matches.get(index).cloned() else {
            return;
        };
        let path = hit
            .abs_path
            .canonicalize()
            .unwrap_or_else(|_| hit.abs_path.clone());
        self.open_workspace_file(path.clone(), true);
        let Some(doc) = self
            .active_doc
            .and_then(|index| self.documents.get(index))
            .filter(|doc| doc.matches_workspace_file(&path))
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
    pub(in crate::state) fn refresh_find(&mut self) {
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
        let anchor = self
            .find
            .navigation_anchor()
            .unwrap_or_else(|| doc.edit.range().0);
        self.find.refresh(&doc.edit.text, anchor);
        self.reveal_find_position(tx);
    }
    pub(in crate::state) fn reveal_find_position(&mut self, tx: &mut Tx) {
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
        let Some(range) = self.find.current_range().cloned() else {
            return;
        };
        let start = range.start;
        if crate::search::replace_one(edit, range, replacement) {
            let inserted = start..edit.range().1;
            self.changed_active(tx);
            self.find.select_after_replacement(inserted);
            self.reveal_find_position(tx);
        }
    }
}
