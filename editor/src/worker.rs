use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};
use serde_json::Value;
use crate::{documents::DocId, fs_io, session::valid_session_id, workspace_tree};

pub(crate) type CliFn = dyn Fn(&[&str], Option<&str>) -> Result<String, String> + Send;
pub(crate) type CliRunner = Box<CliFn>;
pub(crate) type WriteFn = dyn Fn(&WriteOp) -> Result<WriteOutcome, String> + Send;
pub(crate) type WriteRunner = Box<WriteFn>;
pub(crate) type WakeFn = Box<dyn Fn() + Send>;

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
    },
}

pub(crate) enum WorkerRequest {
    FolderSearch { request_id: u64, files: Vec<workspace_tree::WorkspaceEntry>, query: String },
    WriteFile {
        doc_id: DocId,
        generation: u64,
        path: PathBuf,
        content: String,
        clear_recovery: Option<PathBuf>,
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
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    Local,
    Mirrored,
    MirrorSkipped(String),
}

pub(crate) enum WorkerReply {
    FolderSearch { request_id: u64, matches: Vec<crate::quick_open::FolderMatch> },
    Saved {
        doc_id: DocId,
        generation: u64,
        sha256: String,
        result: Result<WriteOutcome, String>,
    },
    RecoveryWritten {
        doc_id: DocId,
        result: Result<WriteOutcome, String>,
    },
    Finished {
        doc_id: DocId,
        generation: u64,
        snapshot: Option<(String, WriteOutcome)>,
        result: Result<(), String>,
    },
}

pub(crate) struct Worker {
    pub(crate) tx: Option<mpsc::Sender<WorkerRequest>>,
    pub(crate) rx: mpsc::Receiver<WorkerReply>,
    pub(crate) join: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn spawn_with_handlers(
        runner: CliRunner,
        writer: WriteRunner,
        wake: WakeFn,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (rep_tx, rep_rx) = mpsc::channel::<WorkerReply>();
        let join = thread::spawn(move || {
            while let Ok(request) = req_rx.recv() {
                let reply = match request {
                    WorkerRequest::FolderSearch { request_id, files, query } => WorkerReply::FolderSearch {
                        request_id, matches: crate::quick_open::search_workspace_files(&files, &query, 200),
                    },
                    WorkerRequest::WriteFile {
                        doc_id,
                        generation,
                        path,
                        content,
                        clear_recovery,
                    } => {
                        let sha256 = fs_io::sha256_text(&content);
                        let result = writer(&WriteOp::File {
                            path,
                            content,
                            clear_recovery,
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
        }
    }

    pub(crate) fn send(&self, request: WorkerRequest) -> Result<(), String> {
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
        self.tx.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn execute_write_op(op: &WriteOp) -> Result<WriteOutcome, String> {
    match op {
        WriteOp::File {
            path,
            content,
            clear_recovery,
        } => {
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
        } => write_session_direct(
            project,
            session_id,
            before_sha256.as_deref(),
            after,
            notes,
            mirror_path.as_deref(),
            mirror_expected_sha256.as_deref(),
        ),
    }
}

pub(crate) fn write_session_direct(
    project: &Path,
    session_id: &str,
    cached_before_sha256: Option<&str>,
    after: &str,
    notes: &str,
    mirror_path: Option<&Path>,
    mirror_expected_sha256: Option<&str>,
) -> Result<WriteOutcome, String> {
    if !valid_session_id(session_id) {
        return Err("Invalid session ID".into());
    }
    let folder = project.join("sessions").join(session_id);
    if !folder.join("session.json").is_file() {
        return Err(format!("Session {session_id} not found in {}.", project.display()));
    }
    if folder.join("receipt.json").exists() {
        return Err("This session is already finished. Start a new session to edit again.".into());
    }
    let meta_raw = fs_io::read_utf8(&folder.join("session.json"))?;
    let meta: Value = serde_json::from_str(&meta_raw)
        .map_err(|e| format!("Invalid session metadata in {}: {e}", folder.display()))?;
    if meta.get("kind").and_then(Value::as_str) != Some("ghostless-edit-session") {
        return Err("Unsupported session format".into());
    }
    let expected_sha = meta
        .get("beforeSha256")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Session metadata missing beforeSha256 in {}", folder.display()))?;
    if cached_before_sha256.is_some_and(|cached| cached != expected_sha) {
        return Err(format!("The immutable baseline metadata changed in {}.", folder.display()));
    }
    let before = fs_io::read_utf8(&folder.join("before.md"))?;
    if fs_io::sha256_text(&before) != expected_sha {
        return Err(format!("The immutable baseline has changed in {}.", folder.display()));
    }
    let after_path = folder.join("after.md");
    fs_io::atomic_write_text(&after_path, after)?;
    fs_io::atomic_write_text(&folder.join("notes.md"), notes)?;
    clear_recovery_copy(&crate::documents::session_recovery_path(&after_path))?;

    let Some(mirror) = mirror_path else {
        return Ok(WriteOutcome::Local);
    };
    match write_mirror(mirror, mirror_expected_sha256, after) {
        Ok(()) => Ok(WriteOutcome::Mirrored),
        Err(error) => Ok(WriteOutcome::MirrorSkipped(error)),
    }
}

pub(crate) fn clear_recovery_copy(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Cannot remove recovery copy {}: {error}", path.display())),
    }
}

fn write_mirror(path: &Path, expected_sha256: Option<&str>, after: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Linked path is outside the workspace".into());
    }
    let expected = expected_sha256.filter(|hash| !hash.is_empty())
        .ok_or("Linked draft has no known sync state")?;
    let current = fs_io::read_utf8(path)?;
    if fs_io::sha256_text(&current) != expected {
        return Err("Linked draft changed outside the editor".into());
    }
    write_workspace_file(path, after)
}

fn write_workspace_file(path: &Path, content: &str) -> Result<(), String> {
    if workspace_tree::is_protected_voice_path(path) {
        return Err(format!("Protected voice data is read-only: {}", path.display()));
    }
    fs_io::atomic_write_text(path, content)
}

pub(crate) fn run_finish(project: &Path, session_id: &str, runner: &CliFn) -> Result<(), String> {
    let project = project.to_str().ok_or("Project path is not UTF-8")?;
    runner(
        &["finish", "--session", session_id, "--project", project, "--json"],
        None,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        )
        .unwrap();
        assert_eq!(fs_io::read_utf8(&session_dir.join("after.md")).unwrap(), "Revised prose.\n");
        assert_eq!(fs_io::read_utf8(&session_dir.join("notes.md")).unwrap(), "Tightened lead.");
        assert_eq!(fs_io::read_utf8(&mirror).unwrap(), "Revised prose.\n");

        fs_io::atomic_write_text(&mirror, "external edit").unwrap();
        let outcome = write_session_direct(
            &base, session_id, Some(&before_sha), "Local edits", "Local notes",
            Some(&mirror), Some(&fs_io::sha256_text("Revised prose.\n")),
        ).unwrap();
        assert!(matches!(outcome, WriteOutcome::MirrorSkipped(error) if error.contains("changed outside")));
        assert_eq!(fs_io::read_utf8(&mirror).unwrap(), "external edit");
        assert_eq!(fs_io::read_utf8(&session_dir.join("after.md")).unwrap(), "Local edits");
        assert_eq!(fs_io::read_utf8(&session_dir.join("notes.md")).unwrap(), "Local notes");

        let missing = base.join("missing.md");
        for invalid_mirror in [missing.as_path(), Path::new("../outside.md")] {
            let outcome = write_session_direct(
                &base, session_id, Some(&before_sha), "Local edits", "Local notes",
                Some(invalid_mirror), Some(&before_sha),
            ).unwrap();
            assert!(matches!(outcome, WriteOutcome::MirrorSkipped(_)));
            assert_eq!(fs_io::read_utf8(&session_dir.join("after.md")).unwrap(), "Local edits");
            assert_eq!(fs_io::read_utf8(&session_dir.join("notes.md")).unwrap(), "Local notes");
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
            }).unwrap_err();
            assert!(error.contains("Protected voice data"));
            let outcome = write_session_direct(
                &base, session_id, Some(&before_sha), "Local edits", "Local notes",
                Some(&protected), Some(&before_sha),
            ).unwrap();
            assert!(matches!(outcome, WriteOutcome::MirrorSkipped(error) if error.contains("Protected voice data")));
            assert_eq!(fs_io::read_utf8(&protected).unwrap(), before);
            assert_eq!(fs_io::read_utf8(&session_dir.join("after.md")).unwrap(), "Local edits");
        }

        // Tampered before.md must be refused without overwriting after.md.
        fs_io::atomic_write_text(&session_dir.join("before.md"), "Tampered!\n").unwrap();
        let err = write_session_direct(&base, session_id, Some(&before_sha), "Bad write", "", None, None)
            .unwrap_err();
        assert!(err.contains("immutable baseline has changed"), "{err}");
        assert_eq!(fs_io::read_utf8(&session_dir.join("after.md")).unwrap(), "Local edits");

        // Restore before.md, add receipt.json -> finished session must be refused.
        fs_io::atomic_write_text(&session_dir.join("before.md"), before).unwrap();
        fs_io::atomic_write_text(&session_dir.join("receipt.json"), "{}\n").unwrap();
        let err = write_session_direct(&base, session_id, Some(&before_sha), "After finish", "", None, None)
            .unwrap_err();
        assert!(err.contains("already finished"), "{err}");

        let _ = fs::remove_dir_all(&base);
    }
}
