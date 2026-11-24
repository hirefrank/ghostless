use std::{path::{Path, PathBuf}, sync::mpsc, thread};
use serde_json::json;

pub(crate) type CliFn = dyn Fn(&[&str], Option<&str>) -> Result<String, String> + Send;
pub(crate) type CliRunner = Box<CliFn>;
pub(crate) type WakeFn = Box<dyn Fn() + Send>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InFlight {
    Save,
    Finish,
}

pub(crate) enum WorkerRequest {
    Snapshot {
        generation: u64,
        after: String,
        notes: String,
    },
    Finish {
        generation: u64,
        snapshot: Option<(String, String)>,
    },
}

pub(crate) enum WorkerReply {
    Snapshot {
        generation: u64,
        result: Result<(), String>,
    },
    Finish {
        generation: u64,
        saved_snapshot: bool,
        result: Result<(), String>,
    },
}

pub(crate) struct Worker {
    pub(crate) tx: Option<mpsc::Sender<WorkerRequest>>,
    pub(crate) rx: mpsc::Receiver<WorkerReply>,
    pub(crate) join: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn spawn(project: PathBuf, session_id: String, runner: CliRunner, wake: WakeFn) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (rep_tx, rep_rx) = mpsc::channel::<WorkerReply>();
        let join = thread::spawn(move || {
            while let Ok(request) = req_rx.recv() {
                let reply = match request {
                    WorkerRequest::Snapshot {
                        generation,
                        after,
                        notes,
                    } => {
                        let result = run_snapshot(&project, &session_id, &after, &notes, &*runner);
                        WorkerReply::Snapshot { generation, result }
                    }
                    WorkerRequest::Finish {
                        generation,
                        snapshot,
                    } => {
                        let mut saved_snapshot = false;
                        let result = if let Some((after, notes)) = snapshot {
                            match run_snapshot(&project, &session_id, &after, &notes, &*runner) {
                                Ok(()) => {
                                    saved_snapshot = true;
                                    run_finish(&project, &session_id, &*runner)
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            run_finish(&project, &session_id, &*runner)
                        };
                        WorkerReply::Finish {
                            generation,
                            saved_snapshot,
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

pub(crate) fn run_snapshot(
    project: &Path,
    session_id: &str,
    after: &str,
    notes: &str,
    runner: &CliFn,
) -> Result<(), String> {
    let project = project.to_str().ok_or("Project path is not UTF-8")?;
    let payload = json!({"after": after, "notes": notes});
    runner(
        &["snapshot", "--session", session_id, "--project", project],
        Some(&payload.to_string()),
    )?;
    Ok(())
}

pub(crate) fn run_finish(project: &Path, session_id: &str, runner: &CliFn) -> Result<(), String> {
    let project = project.to_str().ok_or("Project path is not UTF-8")?;
    runner(
        &["finish", "--session", session_id, "--project", project, "--json"],
        None,
    )?;
    Ok(())
}

