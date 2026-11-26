//! Isolate OS watch registration and event batching from the editor's save lane.
use crate::{error::EditorError, worker::WakeFn};
use notify::{Event, EventKind, RecursiveMode, Watcher as _, event::ModifyKind};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct WatchBatch {
    pub generation: u64,
    pub tree_changed: bool,
    pub modified_files: BTreeSet<PathBuf>,
    pub voice_data_changed: bool,
}

enum Message {
    Event(notify::Result<Event>),
    Stop,
}

pub(crate) struct Watcher {
    stop: mpsc::Sender<Message>,
    pub(crate) batches: mpsc::Receiver<WatchBatch>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn start(
        workspace: Option<PathBuf>,
        voice: Option<PathBuf>,
        generation: u64,
        wake: Arc<WakeFn>,
    ) -> Result<Self, EditorError> {
        let (events, rx) = mpsc::channel();
        let (send_batch, batches) = mpsc::channel();
        let stop = events.clone();
        let thread = thread::Builder::new()
            .name("ghostless-watcher".into())
            .spawn(move || {
                let mut warned = false;
                let mut warn = |error: notify::Error| {
                    if !warned {
                        eprintln!("ghostless-editor: live file watching limited: {error}");
                        warned = true;
                    }
                };
                let mut backend = match notify::recommended_watcher(move |event| {
                    let _ = events.send(Message::Event(event));
                }) {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        warn(error);
                        return;
                    }
                };
                let roots = workspace.iter().chain(voice.iter().filter(|path| {
                    !workspace
                        .as_ref()
                        .is_some_and(|root| path.starts_with(root))
                }));
                let mut registered = false;
                for root in roots {
                    match backend.watch(root, RecursiveMode::Recursive) {
                        Ok(()) => registered = true,
                        Err(error) => warn(error),
                    }
                }
                if !registered {
                    return;
                }
                let send = |batch| {
                    let ok = send_batch.send(batch).is_ok();
                    if ok {
                        wake();
                    }
                    ok
                };
                // Recheck the initial scan after registration so writes that
                // occurred during setup still appear in the sidebar.
                if !send(WatchBatch {
                    generation,
                    tree_changed: workspace.is_some(),
                    voice_data_changed: voice.is_some(),
                    ..Default::default()
                }) {
                    return;
                }
                let mut batch = WatchBatch {
                    generation,
                    ..Default::default()
                };
                let mut deadline: Option<Instant> = None;
                loop {
                    let message = if let Some(due) = deadline {
                        rx.recv_timeout(due.saturating_duration_since(Instant::now()))
                    } else {
                        rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    match message {
                        Ok(Message::Event(Ok(event))) => {
                            if merge_event(
                                &mut batch,
                                event,
                                workspace.as_deref(),
                                voice.as_deref(),
                            ) {
                                deadline.get_or_insert_with(|| Instant::now() + DEBOUNCE);
                            }
                        }
                        Ok(Message::Event(Err(error))) => warn(error),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if deadline.is_some_and(|due| Instant::now() >= due) {
                        deadline = None;
                        let ready = std::mem::replace(
                            &mut batch,
                            WatchBatch {
                                generation,
                                ..Default::default()
                            },
                        );
                        if !send(ready) {
                            break;
                        }
                    }
                }
            })
            .map_err(|error| EditorError::Io {
                context: format!("Cannot start file watcher: {error}"),
            })?;
        Ok(Self {
            stop,
            batches,
            thread: Some(thread),
        })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(Message::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn is_noise(path: &Path) -> bool {
    path.iter().any(|name| {
        let name = name.to_string_lossy();
        matches!(name.as_ref(), ".git" | "target" | "node_modules")
            || name.starts_with(".ghostless-tmp-")
            || [".swp", ".tmp", "~"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
    })
}

fn is_voice_data(path: &Path) -> bool {
    let Some(first) = path.iter().next().and_then(|name| name.to_str()) else {
        return true;
    };
    matches!(
        first,
        "manifest.json"
            | "voice-guide.md"
            | "voice-stats.json"
            | "edits-summary.json"
            | "rules.json"
            | "checks.json"
            | "STYLE.md"
            | "AUDIENCE.md"
            | "sessions"
            | "corpus"
            | "transcripts"
            | "gold"
    )
}

fn merge_event(
    batch: &mut WatchBatch,
    event: Event,
    workspace: Option<&Path>,
    voice: Option<&Path>,
) -> bool {
    if event.kind.is_access() {
        return false;
    }
    if event.need_rescan() {
        batch.tree_changed |= workspace.is_some();
        batch.voice_data_changed |= voice.is_some();
    }
    let structural = event.kind.is_create()
        || event.kind.is_remove()
        || matches!(
            event.kind,
            EventKind::Any | EventKind::Modify(ModifyKind::Name(_))
        );
    let mut relevant = event.need_rescan();
    for path in event.paths {
        let workspace_path = workspace.and_then(|root| path.strip_prefix(root).ok());
        let voice_path = voice.and_then(|root| path.strip_prefix(root).ok());
        if workspace_path.or(voice_path).is_none_or(is_noise) {
            continue;
        }
        batch.tree_changed |= structural && workspace_path.is_some();
        batch.voice_data_changed |= voice_path.is_some_and(is_voice_data);
        if !path.is_dir() {
            batch
                .modified_files
                .insert(path.canonicalize().unwrap_or(path));
        }
        relevant = true;
    }
    relevant
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TestDir;

    #[test]
    fn emits_debounced_creation_and_ignores_temporary_files() {
        let root = TestDir::new("watch");
        let root = root.canonicalize().unwrap();
        let watcher =
            Watcher::start(Some(root.clone()), None, 9, Arc::new(Box::new(|| {}))).unwrap();
        let wait = Duration::from_secs(5);
        watcher.batches.recv_timeout(wait).unwrap();
        let file = root.join("added.md");
        std::fs::write(root.join("scratch.tmp"), "temporary").unwrap();
        std::fs::write(&file, "Café").unwrap();
        let batch = watcher.batches.recv_timeout(wait).unwrap();
        assert_eq!(batch.generation, 9);
        assert!(batch.tree_changed);
        assert_eq!(batch.modified_files, BTreeSet::from([file]));
        assert!(!batch.voice_data_changed);
    }
}
