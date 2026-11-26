use crate::error::EditorError;
use crate::{fs_io, prefs, session::Session};
use rune::TextEdit;
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DocId {
    File(PathBuf),
    Session { project: PathBuf, id: String },
}

impl DocId {
    pub(crate) fn tab_key(&self) -> String {
        match self {
            DocId::File(path) => format!("file:{}", path.display()),
            DocId::Session { project, id } => format!("session:{}::{id}", project.display()),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SessionRecovery {
    pub text: String,
    pub notes: String,
}

pub(crate) struct DecorationCache {
    pub generation: u64,
    pub key: String,
    pub spans: Vec<rune::Span>,
    pub description: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CachedJudge {
    pub voice_project: PathBuf,
    pub judge_epoch: u64,
    pub section: String,
    pub report: crate::bridge::JudgeReport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionConflict {
    pub disk_text: String,
    pub session_text: String,
}

pub(crate) struct DocumentBuffer {
    pub id: DocId,
    pub title: String,
    pub path: PathBuf,
    pub edit: TextEdit,
    pub notes: Option<TextEdit>,
    pub original: Option<String>,
    pub before_sha256: Option<String>,
    pub session_id: Option<String>,
    pub voice_project: Option<PathBuf>,
    pub mirror_path: Option<PathBuf>,
    pub finished: bool,
    pub read_only: bool,
    pub user_locked: bool,
    pub lockable: bool,
    pub pinned: bool,
    pub generation: u64,
    pub saved_generation: u64,
    pub autosave_due: Option<f64>,
    pub recovery_due: Option<f64>,
    pub pending_save: bool,
    pub pending_recovery: bool,
    pub close_after_save: bool,
    pub finish_requested: bool,
    pub finish_error: Option<String>,
    pub recovery_error: Option<String>,
    pub last_saved_label: Option<String>,
    pub save_error: Option<String>,
    pub last_disk_sha256: String,
    /// Section chosen in the inspector. `None` follows frontmatter `section:`.
    pub section_override: Option<String>,
    /// Bumped when the judge section or voice guide changes, without dirtying the draft.
    pub judge_epoch: u64,
    /// Request id of the judge call that may still update this tab.
    pub latest_judge_request: Option<u64>,
    pub cached_judge: Option<CachedJudge>,
    pub conflict: Option<SessionConflict>,
    pub force_mirror: bool,
    cached_words: Cell<(u64, usize)>,
    pub(crate) decoration_cache: RefCell<Option<DecorationCache>>,
}

impl DocumentBuffer {
    pub(crate) fn from_workspace_file(
        path: PathBuf,
        title: String,
        content: String,
        read_only: bool,
        lockable: bool,
        pinned: bool,
        voice_project: Option<PathBuf>,
    ) -> Self {
        let sha = fs_io::sha256_text(&content);
        Self {
            id: DocId::File(path.clone()),
            title,
            path,
            edit: TextEdit::new(content),
            notes: None,
            original: None,
            before_sha256: None,
            session_id: None,
            voice_project,
            mirror_path: None,
            finished: false,
            read_only,
            user_locked: false,
            lockable,
            pinned,
            generation: 0,
            saved_generation: 0,
            autosave_due: None,
            recovery_due: None,
            pending_save: false,
            pending_recovery: false,
            close_after_save: false,
            finish_requested: false,
            finish_error: None,
            recovery_error: None,
            last_saved_label: if read_only {
                Some("Read-only".into())
            } else {
                None
            },
            save_error: None,
            last_disk_sha256: sha,
            section_override: None,
            judge_epoch: 0,
            latest_judge_request: None,
            cached_judge: None,
            conflict: None,
            force_mirror: false,
            cached_words: Cell::new((u64::MAX, 0)),
            decoration_cache: RefCell::new(None),
        }
    }

    pub(crate) fn from_session(
        session: Session,
        pinned: bool,
        mirror_path: Option<PathBuf>,
    ) -> Self {
        let path = session
            .project
            .join("sessions")
            .join(&session.id)
            .join("after.md");
        let sha = fs_io::sha256_text(&session.after);
        Self {
            id: DocId::Session {
                project: session.project.clone(),
                id: session.id.clone(),
            },
            title: session.source_name.clone(),
            path,
            edit: TextEdit::new(session.after),
            notes: Some(TextEdit::new(session.notes)),
            original: Some(session.original),
            before_sha256: Some(session.before_sha256),
            session_id: Some(session.id),
            voice_project: Some(session.project),
            mirror_path,
            finished: session.finished,
            read_only: session.finished,
            user_locked: false,
            lockable: false,
            pinned,
            generation: 0,
            saved_generation: 0,
            autosave_due: None,
            recovery_due: None,
            pending_save: false,
            pending_recovery: false,
            close_after_save: false,
            finish_requested: false,
            finish_error: None,
            recovery_error: None,
            last_saved_label: session.finished.then(|| "Finished".into()),
            save_error: None,
            last_disk_sha256: sha,
            section_override: None,
            judge_epoch: 0,
            latest_judge_request: None,
            cached_judge: None,
            conflict: None,
            force_mirror: false,
            cached_words: Cell::new((u64::MAX, 0)),
            decoration_cache: RefCell::new(None),
        }
    }

    pub(crate) fn is_session(&self) -> bool {
        self.session_id.is_some()
    }

    pub(crate) fn matches_session(&self, project: &Path, session_id: &str) -> bool {
        match &self.id {
            DocId::Session {
                project: doc_project,
                id,
            } => id == session_id && same_canonical_path(doc_project, project),
            DocId::File(_) => {
                self.session_id.as_deref() == Some(session_id)
                    && self
                        .voice_project
                        .as_deref()
                        .is_some_and(|doc_project| same_canonical_path(doc_project, project))
            }
        }
    }

    pub(crate) fn matches_workspace_file(&self, canon: &Path) -> bool {
        match &self.id {
            DocId::File(path) => same_canonical_path(path, canon),
            DocId::Session { .. } => {
                !self.finished
                    && self
                        .mirror_path
                        .as_deref()
                        .is_some_and(|path| same_canonical_path(path, canon))
            }
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        !self.finished && self.generation > self.saved_generation
    }

    pub(crate) fn is_editable(&self) -> bool {
        self.can_save() && !self.user_locked
    }

    pub(crate) fn can_save(&self) -> bool {
        !self.read_only && !self.finished && !self.finish_requested
    }

    pub(crate) fn word_counts(&self) -> (usize, Option<usize>) {
        let (cached_gen, cached_total) = self.cached_words.get();
        let total = if cached_gen == self.generation {
            cached_total
        } else {
            let count = count_words(&self.edit.text);
            self.cached_words.set((self.generation, count));
            count
        };
        let lo = self
            .edit
            .anchor
            .min(self.edit.focus)
            .min(self.edit.text.len());
        let hi = self
            .edit
            .anchor
            .max(self.edit.focus)
            .min(self.edit.text.len());
        let selected = if lo < hi
            && self.edit.text.is_char_boundary(lo)
            && self.edit.text.is_char_boundary(hi)
        {
            Some(count_words(&self.edit.text[lo..hi]))
        } else {
            None
        };
        (total, selected)
    }

    pub(crate) fn reading_minutes(&self) -> usize {
        let (words, _) = self.word_counts();
        words.max(1).div_ceil(225)
    }

    pub(crate) fn recovery_path(&self) -> PathBuf {
        if self.is_session() {
            session_recovery_path(&self.path)
        } else {
            recovery_path_for(&self.path)
        }
    }

    pub(crate) fn recovery_contents(&self) -> Result<String, EditorError> {
        match &self.notes {
            Some(notes) => serde_json::to_string(&SessionRecovery {
                text: self.edit.text.clone(),
                notes: notes.text.clone(),
            })
            .map_err(|error| EditorError::Json {
                context: error.to_string(),
            }),
            None => Ok(self.edit.text.clone()),
        }
    }

    pub(crate) fn read_recovery(&self) -> Result<SessionRecovery, EditorError> {
        let text = fs_io::read_utf8(&self.recovery_path())?;
        if self.is_session() {
            serde_json::from_str(&text).map_err(|error| EditorError::Json {
                context: format!("Invalid session recovery copy: {error}"),
            })
        } else {
            Ok(SessionRecovery {
                text,
                notes: String::new(),
            })
        }
    }

    pub(crate) fn format_saved_utc() -> String {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(dur) => {
                let secs_today = (dur.as_secs() % 86_400) as u32;
                let hours = secs_today / 3600;
                let minutes = (secs_today % 3600) / 60;
                format!("Saved {hours:02}:{minutes:02} UTC")
            }
            Err(_) => "Saved locally".into(),
        }
    }
}

pub(crate) fn same_canonical_path(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

pub(crate) fn recovery_path_for(path: &Path) -> PathBuf {
    let key = fs_io::sha256_text(&path.to_string_lossy());
    prefs::recovery_dir().join(format!("{key}.md"))
}

pub(crate) fn session_recovery_path(path: &Path) -> PathBuf {
    recovery_path_for(path).with_extension("json")
}

pub(crate) fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_doc(text: &str) -> DocumentBuffer {
        DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/draft.md"),
            "draft.md".into(),
            text.into(),
            false,
            false,
            true,
            None,
        )
    }

    #[test]
    fn word_counts_and_reading_minutes_handle_unicode_punctuation_and_selection() {
        let empty = test_doc("");
        assert_eq!(empty.word_counts(), (0, None));
        assert_eq!(empty.reading_minutes(), 1);

        let mut doc = test_doc("Café stay — こんにちは ✨ ... --- world!");
        assert_eq!(doc.word_counts(), (4, None));
        assert_eq!(doc.reading_minutes(), 1);

        // Select "Café stay"
        let prefix = "Café stay";
        doc.edit.anchor = 0;
        doc.edit.focus = prefix.len();
        assert_eq!(doc.word_counts(), (4, Some(2)));

        // Memoized cache updates when generation bumps
        doc.edit.text = (0..451)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        doc.edit.anchor = 0;
        doc.edit.focus = 0;
        doc.generation += 1;
        assert_eq!(doc.word_counts(), (451, None));
        assert_eq!(doc.reading_minutes(), 3);
    }

    #[test]
    fn editable_respects_read_only_finished_and_user_locked() {
        let mut doc = test_doc("hello");
        assert!(doc.is_editable());
        doc.user_locked = true;
        assert!(!doc.is_editable());
        doc.user_locked = false;
        doc.read_only = true;
        assert!(!doc.is_editable());
        doc.read_only = false;
        doc.finished = true;
        assert!(!doc.is_editable());
    }
}
