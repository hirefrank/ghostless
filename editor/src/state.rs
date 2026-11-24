use std::{time::Duration, path::PathBuf};
use rune::{KeyEvent, TextEdit, Tx};
use rune_kit::diff::{DiffMode, DiffState, FileDiff};
use crate::{cli::run_cli, session::{Session, LaunchTarget}, worker::{CliRunner, WakeFn, Worker, WorkerRequest, WorkerReply, InFlight}};

pub(crate) const SAVE_DELAY: Duration = Duration::from_millis(800);
pub(crate) struct Editor {
    pub(crate) session: Session,
    pub(crate) session_active: bool,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) voice_project: Option<PathBuf>,
    pub(crate) initial_file: Option<PathBuf>,
    pub(crate) store: crate::prefs::EditorStateStore,
    pub(crate) after: TextEdit,
    pub(crate) notes: TextEdit,
    pub(crate) diff: DiffState,
    pub(crate) show_diff: bool,
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
        let (session, active, workspace, voice_project, initial_file) = match target {
            LaunchTarget::Session(session) => (session, true, None, None, None),
            LaunchTarget::Workspace { folder, voice_project, initial_file } => (Self::empty_session(), false, Some(folder), voice_project, initial_file),
            LaunchTarget::Welcome { voice_project } => (Self::empty_session(), false, None, voice_project, None),
        };
        let mut editor = Self::with_worker(session, Box::new(run_cli), wake);
        editor.store = crate::prefs::load_store();
        editor.session_active = active;
        editor.workspace = workspace;
        editor.voice_project = voice_project;
        editor.initial_file = initial_file;
        if let Some(folder) = &editor.workspace { editor.store.record_workspace(folder); let _ = crate::prefs::save_store(&editor.store); }
        editor
    }

    fn empty_session() -> Session {
        Session { project: PathBuf::new(), id: String::new(), source_name: String::new(), original: String::new(), after: String::new(), notes: String::new(), finished: false }
    }

    pub(crate) fn open_workspace(&mut self, folder: PathBuf, initial_file: Option<PathBuf>) {
        self.flush_on_exit();
        self.session_active = false;
        self.voice_project = crate::cli::discover_voice_project(&folder, None);
        self.store.record_workspace(&folder);
        if let Err(error) = crate::prefs::save_store(&self.store) { self.status = error; }
        self.workspace = Some(folder);
        self.initial_file = initial_file;
    }

    pub(crate) fn with_worker(session: Session, runner: CliRunner, wake: WakeFn) -> Self {
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &session.source_name,
            &session.original,
            &session.after,
        )]);
        diff.set_mode(DiffMode::Split);
        let worker = Worker::spawn(session.project.clone(), session.id.clone(), runner, wake);
        Self {
            after: TextEdit::new(session.after.clone()),
            notes: TextEdit::new(session.notes.clone()),
            diff,
            show_diff: false,
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
            store: crate::prefs::EditorStateStore::default(),
            worker,
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
        if matches!(self.in_flight, Some(InFlight::Save)) {
            self.pending_save = true;
            self.autosave_due = None;
        } else {
            self.autosave_due = Some(tx.clock() + SAVE_DELAY.as_secs_f64());
            tx.wake_after(SAVE_DELAY);
        }
        self.status = "Unsaved changes".into();
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

    pub(crate) fn finish(&mut self) {
        if self.is_locked() {
            return;
        }
        self.finish_requested = true;
        self.pending_save = false;
        self.autosave_due = None;
        self.status = "Finishing…".into();
        self.dispatch_next();
    }

    pub(crate) fn dispatch_next(&mut self) {
        if self.in_flight.is_some() || self.session.finished {
            return;
        }
        if self.finish_requested {
            let snapshot = self
                .is_dirty()
                .then(|| (self.after.text.clone(), self.notes.text.clone()));
            match self.worker.send(WorkerRequest::Finish {
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

    pub(crate) fn flush_on_exit(&mut self) {
        if self.flushed {
            return;
        }
        self.flushed = true;
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
            if self.finish_requested && !self.session.finished && !attempted_finish {
                attempted_finish = true;
                self.dispatch_next();
                if self.in_flight.is_some() {
                    continue;
                }
                eprintln!("ghostless-editor: {}", self.status);
                break;
            }
            if self.is_dirty() && !attempted_flush_save {
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
        self.worker.shutdown();
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.flush_on_exit();
    }
}

pub(crate) fn poll(editor: &mut Editor, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(reply) = editor.worker.try_recv() {
        editor.apply_reply(reply);
        changed = true;
    }
    if editor.in_flight.is_none()
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
    changed
}

pub(crate) fn on_key(editor: &mut Editor, key: &KeyEvent, tx: &mut Tx) -> bool {
    if key.is_command('o') {
        if key.mods.shift { crate::ui_welcome::open_file(tx); } else { crate::ui_welcome::open_folder(tx); }
        return true;
    }
    if !editor.session_active { return false; }
    if key.is_command('s') {
        editor.save_clicked();
        return true;
    }
    if key.is_command('d') {
        editor.show_diff = !editor.show_diff;
        if editor.show_diff {
            editor.refresh_diff();
        }
        return true;
    }
    false
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{session::Session, worker::{CliRunner, WakeFn, InFlight}};
    use std::{sync::mpsc, thread, time::Duration};
    use rune::Tx;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    type RecordedCall = (Vec<String>, Option<String>);

    fn test_session() -> Session {
        Session {
            project: PathBuf::from("/tmp/ghostless-test-voice"),
            id: "2026-01-02-notes".into(),
            source_name: "draft.md".into(),
            original: "Original text.".into(),
            after: "Original text.".into(),
            notes: String::new(),
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

        // save_clicked returns immediately while worker is blocked on release_rx.
        assert_eq!(editor.in_flight, Some(InFlight::Save));
        assert_eq!(editor.status, "Saving…");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker started running snapshot");

        // Typing remains non-blocking while save is in flight.
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
        // Trigger autosave once clock passes SAVE_DELAY.
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
        assert_eq!(guard.len(), 2, "5 rapid edits must coalesce into one follow-up save");
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
        editor.finish(); // duplicate request ignored while locked
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

        // Flush tries once and exits cleanly without spinning.
        editor.flush_on_exit();
        editor.flush_on_exit();
        assert_eq!(*calls.lock().unwrap(), 2);
    }

}
