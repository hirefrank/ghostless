use crate::error::EditorError;
use crate::{
    documents::DocId,
    fs_io,
    quick_open::{FolderMatch, search_workspace_files},
    session::valid_session_id,
    workspace_tree::{self, WorkspaceEntry},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

pub(crate) type CliFn = dyn Fn(&[&str], Option<&str>) -> Result<String, EditorError> + Send;
pub(crate) type CliRunner = Box<CliFn>;
pub(crate) type WriteFn = dyn Fn(&WriteOp) -> Result<WriteOutcome, EditorError> + Send;
pub(crate) type WriteRunner = Box<WriteFn>;
pub(crate) type WakeFn = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InFlight {
    Save,
    Recovery,
    Finish,
}

#[derive(Clone, Debug)]
pub(crate) enum WriteOp {
    File {
        path: PathBuf,
        content: String,
        clear_recovery: Option<PathBuf>,
        expected_sha256: Option<String>,
    },
    Recovery {
        recovery_path: PathBuf,
        content: String,
    },
    Session {
        project: PathBuf,
        session_id: String,
        before_sha256: Option<String>,
        after: String,
        notes: String,
        mirror_path: Option<PathBuf>,
        mirror_expected_sha256: Option<String>,
        mirror_force: bool,
    },
}

pub(crate) enum WorkerRequest {
    WriteFile {
        doc_id: DocId,
        generation: u64,
        path: PathBuf,
        content: String,
        clear_recovery: Option<PathBuf>,
        expected_sha256: Option<String>,
    },
    WriteRecovery {
        doc_id: DocId,
        recovery_path: PathBuf,
        content: String,
    },
    WriteSession {
        doc_id: DocId,
        generation: u64,
        project: PathBuf,
        session_id: String,
        before_sha256: Option<String>,
        after: String,
        notes: String,
        mirror_path: Option<PathBuf>,
        mirror_expected_sha256: Option<String>,
        mirror_force: bool,
    },
    Finish {
        doc_id: DocId,
        generation: u64,
        project: PathBuf,
        session_id: String,
        before_sha256: Option<String>,
        snapshot: Option<(String, String)>,
        mirror_path: Option<PathBuf>,
        mirror_expected_sha256: Option<String>,
        mirror_force: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    Local,
    Mirrored,
    MirrorSkipped(EditorError),
}

pub(crate) enum WorkerReply {
    Saved {
        doc_id: DocId,
        generation: u64,
        sha256: String,
        result: Result<WriteOutcome, EditorError>,
    },
    RecoveryWritten {
        doc_id: DocId,
        result: Result<WriteOutcome, EditorError>,
    },
    Finished {
        doc_id: DocId,
        generation: u64,
        snapshot: Option<(String, WriteOutcome)>,
        result: Result<(), EditorError>,
    },
}

struct SearchRequest {
    id: u64,
    files: Vec<WorkspaceEntry>,
    buffers: HashMap<PathBuf, Option<String>>,
    query: String,
}

pub(crate) struct SearchReply {
    pub(crate) id: u64,
    pub(crate) matches: Vec<FolderMatch>,
}

#[derive(Default)]
struct SearchQueue {
    pending: Option<SearchRequest>,
    closed: bool,
}

#[derive(Default)]
struct SearchMailbox {
    queue: Mutex<SearchQueue>,
    ready: Condvar,
    generation: AtomicU64,
}

impl SearchMailbox {
    fn receive(&self) -> Option<SearchRequest> {
        let mut queue = self
            .ready
            .wait_while(self.queue.lock().unwrap(), |queue| {
                queue.pending.is_none() && !queue.closed
            })
            .unwrap();
        queue.pending.take()
    }
}

pub(crate) struct SearchWorker {
    mailbox: Arc<SearchMailbox>,
    rx: mpsc::Receiver<SearchReply>,
}

impl SearchWorker {
    fn spawn(wake: Arc<WakeFn>) -> Self {
        Self::spawn_with_search(wake, |request, cancelled| {
            search_workspace_files(
                &request.files,
                &request.buffers,
                &request.query,
                200,
                cancelled,
            )
        })
    }

    fn spawn_with_search(
        wake: Arc<WakeFn>,
        search: impl Fn(&SearchRequest, &dyn Fn() -> bool) -> Vec<FolderMatch> + Send + 'static,
    ) -> Self {
        let (replies, rx) = mpsc::channel();
        let mailbox = Arc::new(SearchMailbox::default());
        let reader = Arc::clone(&mailbox);
        thread::spawn(move || {
            while let Some(request) = reader.receive() {
                let cancelled = || request.id != reader.generation.load(Ordering::Relaxed);
                if cancelled() {
                    continue;
                }
                let matches = search(&request, &cancelled);
                if !cancelled()
                    && replies
                        .send(SearchReply {
                            id: request.id,
                            matches,
                        })
                        .is_ok()
                {
                    wake();
                }
            }
        });
        Self { mailbox, rx }
    }

    pub(crate) fn submit(
        &self,
        files: Vec<WorkspaceEntry>,
        buffers: HashMap<PathBuf, Option<String>>,
        query: String,
    ) -> Result<u64, EditorError> {
        let mut queue = self.mailbox.queue.lock().unwrap();
        if queue.closed {
            return Err("Workspace search is unavailable".into());
        }
        let id = self
            .mailbox
            .generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        queue.pending = Some(SearchRequest {
            id,
            files,
            buffers,
            query,
        });
        self.mailbox.ready.notify_one();
        Ok(id)
    }

    pub(crate) fn cancel(&self) {
        let mut queue = self.mailbox.queue.lock().unwrap();
        self.mailbox.generation.fetch_add(1, Ordering::Relaxed);
        queue.pending = None;
    }

    pub(crate) fn try_recv(&self) -> Result<SearchReply, mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    fn shutdown(&self) {
        // Closing also wakes an idle reader. A reader already in filesystem I/O
        // can exit once the read returns, independently of the save worker.
        let mut queue = self.mailbox.queue.lock().unwrap();
        queue.closed = true;
        self.mailbox.generation.fetch_add(1, Ordering::Relaxed);
        queue.pending = None;
        self.mailbox.ready.notify_one();
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) struct Worker {
    pub(crate) tx: Option<mpsc::Sender<WorkerRequest>>,
    pub(crate) rx: mpsc::Receiver<WorkerReply>,
    pub(crate) join: Option<thread::JoinHandle<()>>,
    pub(crate) search: SearchWorker,
}

impl Worker {
    #[cfg(test)]
    pub(crate) fn spawn_with_handlers(
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
    ) -> Self {
        Self::spawn_with_shared_wake(runner, writer, Arc::new(wake))
    }

    pub(crate) fn spawn_with_shared_wake(
        runner: CliRunner,
        writer: WriteRunner,
        wake: Arc<WakeFn>,
    ) -> Self {
        let search = SearchWorker::spawn(Arc::clone(&wake));
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (rep_tx, rep_rx) = mpsc::channel::<WorkerReply>();
        let join = thread::spawn(move || {
            while let Ok(request) = req_rx.recv() {
                let reply = match request {
                    WorkerRequest::WriteFile {
                        doc_id,
                        generation,
                        path,
                        content,
                        clear_recovery,
                        expected_sha256,
                    } => {
                        let sha256 = fs_io::sha256_text(&content);
                        let result = writer(&WriteOp::File {
                            path,
                            content,
                            clear_recovery,
                            expected_sha256,
                        });
                        WorkerReply::Saved {
                            doc_id,
                            generation,
                            sha256,
                            result,
                        }
                    }
                    WorkerRequest::WriteRecovery {
                        doc_id,
                        recovery_path,
                        content,
                    } => {
                        let result = writer(&WriteOp::Recovery {
                            recovery_path,
                            content,
                        });
                        WorkerReply::RecoveryWritten { doc_id, result }
                    }
                    WorkerRequest::WriteSession {
                        doc_id,
                        generation,
                        project,
                        session_id,
                        before_sha256,
                        after,
                        notes,
                        mirror_path,
                        mirror_expected_sha256,
                        mirror_force,
                    } => {
                        let sha256 = fs_io::sha256_text(&after);
                        let result = writer(&WriteOp::Session {
                            project,
                            session_id,
                            before_sha256,
                            after,
                            notes,
                            mirror_path,
                            mirror_expected_sha256,
                            mirror_force,
                        });
                        WorkerReply::Saved {
                            doc_id,
                            generation,
                            sha256,
                            result,
                        }
                    }
                    WorkerRequest::Finish {
                        doc_id,
                        generation,
                        project,
                        session_id,
                        before_sha256,
                        snapshot,
                        mirror_path,
                        mirror_expected_sha256,
                        mirror_force,
                    } => {
                        let mut saved_snapshot = None;
                        let result = if let Some((after, notes)) = snapshot {
                            let sha = fs_io::sha256_text(&after);
                            match writer(&WriteOp::Session {
                                project: project.clone(),
                                session_id: session_id.clone(),
                                before_sha256,
                                after,
                                notes,
                                mirror_path,
                                mirror_expected_sha256,
                                mirror_force,
                            }) {
                                Ok(outcome) => {
                                    saved_snapshot = Some((sha, outcome));
                                    run_finish(&project, &session_id, &*runner)
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            run_finish(&project, &session_id, &*runner)
                        };
                        WorkerReply::Finished {
                            doc_id,
                            generation,
                            snapshot: saved_snapshot,
                            result,
                        }
                    }
                };
                let _ = rep_tx.send(reply);
                wake();
            }
        });
        Self {
            tx: Some(req_tx),
            rx: rep_rx,
            join: Some(join),
            search,
        }
    }

    pub(crate) fn send(&self, request: WorkerRequest) -> Result<(), EditorError> {
        let Some(tx) = &self.tx else {
            return Err("Background save worker stopped".into());
        };
        tx.send(request)
            .map_err(|_| "Background save worker stopped".into())
    }

    pub(crate) fn try_recv(&self) -> Result<WorkerReply, mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    pub(crate) fn recv(&self) -> Result<WorkerReply, mpsc::RecvError> {
        self.rx.recv()
    }

    pub(crate) fn shutdown(&mut self) {
        self.search.shutdown();
        self.tx.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn execute_write_op(op: &WriteOp) -> Result<WriteOutcome, EditorError> {
    match op {
        WriteOp::File {
            path,
            content,
            clear_recovery,
            expected_sha256,
        } => {
            if let Some(expected) = expected_sha256.as_deref().filter(|hash| !hash.is_empty()) {
                let current = if path.is_file() {
                    fs_io::sha256_text(&fs_io::read_utf8(path)?)
                } else {
                    String::new()
                };
                if current != expected {
                    return Err(EditorError::MirrorConflict);
                }
            }
            write_workspace_file(path, content)?;
            if let Some(recovery) = clear_recovery {
                clear_recovery_copy(recovery)?;
            }
            Ok(WriteOutcome::Local)
        }
        WriteOp::Recovery {
            recovery_path,
            content,
        } => fs_io::atomic_write_text(recovery_path, content).map(|()| WriteOutcome::Local),
        WriteOp::Session {
            project,
            session_id,
            before_sha256,
            after,
            notes,
            mirror_path,
            mirror_expected_sha256,
            mirror_force,
        } => write_session_direct(
            project,
            session_id,
            before_sha256.as_deref(),
            after,
            notes,
            mirror_path.as_deref(),
            mirror_expected_sha256.as_deref(),
            *mirror_force,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_session_direct(
    project: &Path,
    session_id: &str,
    cached_before_sha256: Option<&str>,
    after: &str,
    notes: &str,
    mirror_path: Option<&Path>,
    mirror_expected_sha256: Option<&str>,
    mirror_force: bool,
) -> Result<WriteOutcome, EditorError> {
    if !valid_session_id(session_id) {
        return Err(EditorError::InvalidSessionId(session_id.to_owned()));
    }
    let folder = project.join("sessions").join(session_id);
    if !folder.join("session.json").is_file() {
        return Err(EditorError::message(format!(
            "Session {session_id} not found in {}.",
            project.display()
        )));
    }
    if folder.join("receipt.json").exists() {
        return Err(EditorError::AlreadyFinished(
            "This session is already finished. Start a new session to edit again.".into(),
        ));
    }
    let meta_raw = fs_io::read_utf8(&folder.join("session.json"))?;
    let meta: Value = serde_json::from_str(&meta_raw).map_err(|e| EditorError::Json {
        context: format!("Invalid session metadata in {}: {e}", folder.display()),
    })?;
    if meta.get("kind").and_then(Value::as_str) != Some("ghostless-edit-session") {
        return Err("Unsupported session format".into());
    }
    let expected_sha = meta
        .get("beforeSha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "Session metadata missing beforeSha256 in {}",
                folder.display()
            )
        })?;
    if cached_before_sha256.is_some_and(|cached| cached != expected_sha) {
        return Err(EditorError::BaselineMismatch(format!(
            "The immutable baseline metadata changed in {}.",
            folder.display()
        )));
    }
    let before = fs_io::read_utf8(&folder.join("before.md"))?;
    if fs_io::sha256_text(&before) != expected_sha {
        return Err(EditorError::BaselineMismatch(format!(
            "The immutable baseline has changed in {}.",
            folder.display()
        )));
    }
    let after_path = folder.join("after.md");
    fs_io::atomic_write_text(&after_path, after)?;
    fs_io::atomic_write_text(&folder.join("notes.md"), notes)?;
    clear_recovery_copy(&crate::documents::session_recovery_path(&after_path))?;

    let Some(mirror) = mirror_path else {
        return Ok(WriteOutcome::Local);
    };
    match write_mirror(mirror, mirror_expected_sha256, after, mirror_force) {
        Ok(()) => Ok(WriteOutcome::Mirrored),
        Err(error) => Ok(WriteOutcome::MirrorSkipped(error)),
    }
}

pub(crate) fn clear_recovery_copy(path: &Path) -> Result<(), EditorError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EditorError::Io {
            context: format!("Cannot remove recovery copy {}: {error}", path.display()),
        }),
    }
}

fn write_mirror(
    path: &Path,
    expected_sha256: Option<&str>,
    after: &str,
    force: bool,
) -> Result<(), EditorError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(EditorError::ProtectedPath(
            "Linked path is outside the workspace".into(),
        ));
    }
    if !force {
        let expected = expected_sha256
            .filter(|hash| !hash.is_empty())
            .ok_or("Linked draft has no known sync state")?;
        let current = fs_io::read_utf8(path)?;
        if fs_io::sha256_text(&current) != expected {
            return Err(EditorError::MirrorConflict);
        }
    } else if path.exists() && workspace_tree::is_protected_voice_path(path) {
        return Err(EditorError::ProtectedPath(format!(
            "Protected voice data is read-only: {}",
            path.display()
        )));
    }
    write_workspace_file(path, after)
}

fn write_workspace_file(path: &Path, content: &str) -> Result<(), EditorError> {
    if workspace_tree::is_protected_voice_path(path) {
        return Err(EditorError::ProtectedPath(format!(
            "Protected voice data is read-only: {}",
            path.display()
        )));
    }
    fs_io::atomic_write_text(path, content)
}

pub(crate) fn run_finish(
    project: &Path,
    session_id: &str,
    runner: &CliFn,
) -> Result<(), EditorError> {
    let project = project.to_str().ok_or("Project path is not UTF-8")?;
    runner(
        &[
            "finish",
            "--session",
            session_id,
            "--project",
            project,
            "--json",
        ],
        None,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blocked_search_does_not_delay_save_or_finish_and_skips_superseded_queries() {
        use std::time::Duration;

        let wait = Duration::from_secs(5);
        let (started, searches) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut worker = Worker::spawn_with_handlers(
            Box::new(|_, _| Ok(String::new())),
            Box::new(|_| Ok(WriteOutcome::Local)),
            Box::new(|| {}),
        );
        worker.search.shutdown();
        worker.search = SearchWorker::spawn_with_search(
            Arc::new(Box::new(|| {})),
            move |request, cancelled| {
                started.send(request.query.clone()).unwrap();
                if request.query == "old" {
                    blocked.recv_timeout(wait).unwrap();
                    assert!(cancelled());
                }
                Vec::new()
            },
        );
        worker
            .search
            .submit(Vec::new(), HashMap::new(), "old".into())
            .unwrap();
        assert_eq!(searches.recv_timeout(wait).unwrap(), "old");
        worker
            .search
            .submit(Vec::new(), HashMap::new(), "skipped".into())
            .unwrap();
        let latest = worker
            .search
            .submit(Vec::new(), HashMap::new(), "latest".into())
            .unwrap();

        let path = PathBuf::from("worker-search-test.md");
        let doc_id = DocId::File(path.clone());
        worker
            .send(WorkerRequest::WriteFile {
                doc_id: doc_id.clone(),
                generation: 1,
                path,
                content: "draft".into(),
                clear_recovery: None,
                expected_sha256: None,
            })
            .unwrap();
        worker
            .send(WorkerRequest::Finish {
                doc_id,
                generation: 1,
                project: std::env::temp_dir(),
                session_id: "session".into(),
                before_sha256: None,
                snapshot: None,
                mirror_path: None,
                mirror_expected_sha256: None,
                mirror_force: false,
            })
            .unwrap();
        let save = worker.rx.recv_timeout(wait);
        let finish = worker.rx.recv_timeout(wait);
        release.send(()).unwrap();

        assert!(matches!(
            save,
            Ok(WorkerReply::Saved {
                result: Ok(WriteOutcome::Local),
                ..
            })
        ));
        assert!(matches!(
            finish,
            Ok(WorkerReply::Finished { result: Ok(()), .. })
        ));
        assert_eq!(searches.recv_timeout(wait).unwrap(), "latest");
        assert_eq!(worker.search.rx.recv_timeout(wait).unwrap().id, latest);

        // The same worker can go idle after delivering a reply and accept more work.
        for _ in 0..32 {
            worker.search.cancel();
            let id = worker
                .search
                .submit(Vec::new(), HashMap::new(), "again".into())
                .unwrap();
            assert_eq!(worker.search.rx.recv_timeout(wait).unwrap().id, id);
        }
        worker.shutdown();
        assert!(
            worker
                .search
                .submit(Vec::new(), HashMap::new(), "closed".into())
                .is_err()
        );
    }

    #[test]
    fn write_session_direct_writes_atomically_and_enforces_receipt_and_baseline_guards() {
        let base = std::env::temp_dir().join(format!("ghostless-direct-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let session_id = "2026-04-02-notes";
        let session_dir = base.join("sessions").join(session_id);
        fs::create_dir_all(&session_dir).unwrap();
        let before = "Original text.\n";
        let before_sha = fs_io::sha256_text(before);
        fs_io::atomic_write_text(&session_dir.join("before.md"), before).unwrap();
        fs_io::atomic_write_text(&session_dir.join("after.md"), before).unwrap();
        fs_io::atomic_write_text(
            &session_dir.join("session.json"),
            &json!({
                "kind": "ghostless-edit-session",
                "version": 1,
                "id": session_id,
                "beforeSha256": before_sha,
            })
            .to_string(),
        )
        .unwrap();

        let mirror = base.join("mirror/draft.md");
        fs_io::atomic_write_text(&mirror, before).unwrap();
        write_session_direct(
            &base,
            session_id,
            Some(&before_sha),
            "Revised prose.\n",
            "Tightened lead.",
            Some(&mirror),
            Some(&before_sha),
            false,
        )
        .unwrap();
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("after.md")).unwrap(),
            "Revised prose.\n"
        );
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("notes.md")).unwrap(),
            "Tightened lead."
        );
        assert_eq!(fs_io::read_utf8(&mirror).unwrap(), "Revised prose.\n");

        fs_io::atomic_write_text(&mirror, "external edit").unwrap();
        let outcome = write_session_direct(
            &base,
            session_id,
            Some(&before_sha),
            "Local edits",
            "Local notes",
            Some(&mirror),
            Some(&fs_io::sha256_text("Revised prose.\n")),
            false,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            WriteOutcome::MirrorSkipped(EditorError::MirrorConflict)
        ));
        assert_eq!(fs_io::read_utf8(&mirror).unwrap(), "external edit");
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("before.md")).unwrap(),
            before
        );
        let forced = write_session_direct(
            &base,
            session_id,
            Some(&before_sha),
            "Kept session.\n",
            "Kept notes",
            Some(&mirror),
            Some(&before_sha),
            true,
        )
        .unwrap();
        assert!(matches!(forced, WriteOutcome::Mirrored));
        assert_eq!(fs_io::read_utf8(&mirror).unwrap(), "Kept session.\n");
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("after.md")).unwrap(),
            "Kept session.\n"
        );
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("before.md")).unwrap(),
            before
        );

        let missing = base.join("missing.md");
        for invalid_mirror in [missing.as_path(), Path::new("../outside.md")] {
            let outcome = write_session_direct(
                &base,
                session_id,
                Some(&before_sha),
                "Local edits",
                "Local notes",
                Some(invalid_mirror),
                Some(&before_sha),
                false,
            )
            .unwrap();
            assert!(matches!(outcome, WriteOutcome::MirrorSkipped(_)));
            assert_eq!(
                fs_io::read_utf8(&session_dir.join("after.md")).unwrap(),
                "Local edits"
            );
            assert_eq!(
                fs_io::read_utf8(&session_dir.join("notes.md")).unwrap(),
                "Local notes"
            );
        }
        assert!(!missing.exists());

        // A valid snapshot can still be saved when its mirror points at voice data.
        fs::write(base.join("manifest.json"), r#"{"kind":"ghostless"}"#).unwrap();
        for directory in ["corpus", "transcripts", "holdouts", "gold"] {
            let protected = base.join(directory).join("example.md");
            fs_io::atomic_write_text(&protected, before).unwrap();
            let error = execute_write_op(&WriteOp::File {
                path: protected.clone(),
                content: "Attempted overwrite".into(),
                clear_recovery: None,
                expected_sha256: None,
            })
            .unwrap_err();
            assert!(error.to_string().contains("Protected voice data"));
            let outcome = write_session_direct(
                &base,
                session_id,
                Some(&before_sha),
                "Local edits",
                "Local notes",
                Some(&protected),
                Some(&before_sha),
                false,
            )
            .unwrap();
            assert!(matches!(
                outcome,
                WriteOutcome::MirrorSkipped(EditorError::ProtectedPath(_))
            ));
            assert_eq!(
                fs_io::read_utf8(&session_dir.join("after.md")).unwrap(),
                "Local edits"
            );
            let forced = write_session_direct(
                &base,
                session_id,
                Some(&before_sha),
                "Forced protected",
                "notes",
                Some(&protected),
                Some(&before_sha),
                true,
            )
            .unwrap();
            assert!(matches!(
                forced,
                WriteOutcome::MirrorSkipped(EditorError::ProtectedPath(error))
                    if error.contains("Protected voice data")
            ));
            assert_eq!(fs_io::read_utf8(&protected).unwrap(), before);
        }

        // Tampered before.md must be refused without overwriting after.md.
        fs_io::atomic_write_text(&session_dir.join("before.md"), "Tampered!\n").unwrap();
        let err = write_session_direct(
            &base,
            session_id,
            Some(&before_sha),
            "Bad write",
            "",
            None,
            None,
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("immutable baseline has changed"),
            "{err}"
        );
        assert_eq!(
            fs_io::read_utf8(&session_dir.join("after.md")).unwrap(),
            "Forced protected"
        );

        // Restore before.md, add receipt.json -> finished session must be refused.
        fs_io::atomic_write_text(&session_dir.join("before.md"), before).unwrap();
        fs_io::atomic_write_text(&session_dir.join("receipt.json"), "{}\n").unwrap();
        let err = write_session_direct(
            &base,
            session_id,
            Some(&before_sha),
            "After finish",
            "",
            None,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already finished"), "{err}");

        let _ = fs::remove_dir_all(&base);
    }
}
