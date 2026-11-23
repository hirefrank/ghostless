//! Local Rune editor over ghostless's versioned edit-session files.

use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rune::{App, AppSpec, Cx, El, KeyEvent, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea};
use serde_json::{Value, json};

const SAVE_DELAY: Duration = Duration::from_millis(800);
const TEXTAREA_LINE_HEIGHT: f32 = 17.5;

type CliFn = dyn Fn(&[&str], Option<&str>) -> Result<String, String> + Send;
type CliRunner = Box<CliFn>;
type WakeFn = Box<dyn Fn() + Send>;

#[derive(Clone)]
struct Session {
    project: PathBuf,
    id: String,
    source_name: String,
    original: String,
    after: String,
    notes: String,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InFlight {
    Save,
    Finish,
}

enum WorkerRequest {
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

enum WorkerReply {
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

struct Worker {
    tx: Option<mpsc::Sender<WorkerRequest>>,
    rx: mpsc::Receiver<WorkerReply>,
    join: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn spawn(project: PathBuf, session_id: String, runner: CliRunner, wake: WakeFn) -> Self {
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

    fn send(&self, request: WorkerRequest) -> Result<(), String> {
        let Some(tx) = &self.tx else {
            return Err("Background save worker stopped".into());
        };
        tx.send(request)
            .map_err(|_| "Background save worker stopped".into())
    }

    fn try_recv(&self) -> Result<WorkerReply, mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    fn recv(&self) -> Result<WorkerReply, mpsc::RecvError> {
        self.rx.recv()
    }

    fn shutdown(&mut self) {
        self.tx.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn run_snapshot(
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

fn run_finish(project: &Path, session_id: &str, runner: &CliFn) -> Result<(), String> {
    let project = project.to_str().ok_or("Project path is not UTF-8")?;
    runner(
        &["finish", "--session", session_id, "--project", project, "--json"],
        None,
    )?;
    Ok(())
}

struct Editor {
    session: Session,
    after: TextEdit,
    notes: TextEdit,
    diff: DiffState,
    show_diff: bool,
    generation: u64,
    saved_generation: u64,
    autosave_due: Option<f64>,
    pending_save: bool,
    finish_requested: bool,
    in_flight: Option<InFlight>,
    flushed: bool,
    status: String,
    worker: Worker,
}

impl Editor {
    fn new(session: Session, wake: WakeFn) -> Self {
        Self::with_worker(session, Box::new(run_cli), wake)
    }

    fn with_worker(session: Session, runner: CliRunner, wake: WakeFn) -> Self {
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
            worker,
        }
    }

    fn is_locked(&self) -> bool {
        self.session.finished
            || self.finish_requested
            || matches!(self.in_flight, Some(InFlight::Finish))
    }

    fn is_dirty(&self) -> bool {
        !self.session.finished && self.generation > self.saved_generation
    }

    fn changed(&mut self, tx: &mut Tx) {
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

    fn refresh_diff(&mut self) {
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &self.session.source_name,
            &self.session.original,
            &self.after.text,
        )]);
        diff.set_mode(DiffMode::Split);
        self.diff = diff;
    }

    fn save_clicked(&mut self) {
        if self.is_locked() {
            return;
        }
        self.pending_save = true;
        self.autosave_due = None;
        self.dispatch_next();
    }

    fn finish(&mut self) {
        if self.is_locked() {
            return;
        }
        self.finish_requested = true;
        self.pending_save = false;
        self.autosave_due = None;
        self.status = "Finishing…".into();
        self.dispatch_next();
    }

    fn dispatch_next(&mut self) {
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

    fn apply_reply(&mut self, reply: WorkerReply) -> bool {
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

    fn flush_on_exit(&mut self) {
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

fn working_textarea_metrics(pane_height: Option<f32>) -> (f32, usize) {
    let height = pane_height.unwrap_or(80.0).floor().max(80.0);
    let rows = ((height / TEXTAREA_LINE_HEIGHT).ceil() as usize).max(4);
    (height, rows)
}

fn poll(editor: &mut Editor, clock: f64, tx: &mut Tx) -> bool {
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

fn on_key(editor: &mut Editor, key: &KeyEvent, _: &mut Tx) -> bool {
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

fn view(editor: &Editor, cx: &mut Cx<Editor>) -> El {
    let kit = Kit::neutral(cx.system_dark());
    let locked = editor.is_locked();
    let modes = SegmentedControl::new(&["Edit", "Diff"], Some(usize::from(editor.show_diff)))
        .width(Some(140.0))
        .on_pick(|editor: &mut Editor, index, _: &mut Tx| {
            editor.show_diff = index == 1;
            if editor.show_diff {
                editor.refresh_diff();
            }
        });
    let top = row().key("toolbar").h(58.0).w_full().px(18.0).gap(12.0)
        .items_center().fill(kit.c.layer_01)
        .child(text("ghostless").font(kit.ty.ui_medium).color(kit.c.text_base))
        .child(text(format!("/ {}", editor.session.source_name)).font(kit.ty.ui).color(kit.c.text_muted))
        .child(row().grow(1.0)).child(modes.build(&kit))
        .child(Button::new("Save").variant(ButtonVariant::Outline)
            .disabled(locked).on_press(|editor: &mut Editor| editor.save_clicked()).build(&kit))
        .child(Button::new("Finish").variant(ButtonVariant::Submit)
            .disabled(locked).on_press(|editor: &mut Editor| editor.finish()).build(&kit));
    let content = if editor.show_diff {
        DiffView::new(|editor: &Editor| &editor.diff, |editor: &mut Editor| &mut editor.diff)
            .build(editor, &kit)
    } else {
        let (working_height, working_rows) =
            working_textarea_metrics(cx.layout_size("original-body").map(|size| size[1]));
        let original = col().key("original-pane").grow(1.0).h_full().min_h(0.0).gap(12.0)
            .padding(18.0).fill(kit.c.bg_base)
            .child(text("ORIGINAL · READ ONLY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(col().key("original-body").grow(1.0).w_full().min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(text(editor.session.original.clone()).font(kit.ty.body)
                    .color(kit.c.text_base).wrap().selectable(true)));
        let working = col().key("working-pane").grow(1.0).h_full().min_h(0.0).gap(12.0)
            .padding(18.0).fill(kit.c.layer_01)
            .child(text("WORKING COPY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(col().key("working-body").grow(1.0).w_full().min_h(0.0)
                .child(TextArea::new(&editor.after).label("Working draft")
                    .width(None).rows(working_rows).max_height(working_height)
                    .disabled(locked)
                    .on_edit(|editor: &mut Editor| &mut editor.after)
                    .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed(tx))
                    .build(&kit)));
        row().key("edit-panes").grow(1.0).w_full().min_h(0.0).child(original).child(working)
    };
    let notes = col().key("notes-pane").h(142.0).w_full().padding(14.0)
        .gap(7.0).fill(kit.c.layer_01)
        .child(text("NOTES · WHY DID YOU CHANGE IT?").font(kit.ty.ui_medium).color(kit.c.text_muted))
        .child(TextArea::new(&editor.notes).label("Edit notes")
            .placeholder("Optional context for your future self")
            .width(None).rows(3).disabled(locked)
            .on_edit(|editor: &mut Editor| &mut editor.notes)
            .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed(tx))
            .build(&kit));
    let footer = row().h(32.0).w_full().px(18.0).items_center()
        .fill(kit.c.bg_base)
        .child(text(editor.status.clone()).font(kit.ty.ui).color(kit.c.text_muted))
        .child(row().grow(1.0))
        .child(text("Ctrl/Cmd+S save  ·  Ctrl/Cmd+D diff").font(kit.ty.ui).color(kit.c.text_faint));
    col().w_full().h_full().fill(kit.c.bg_base)
        .child(top).child(content).child(notes).child(footer)
}

fn cli_command() -> Command {
    if let Some(path) = env::var_os("GHOSTLESS_CLI") {
        return Command::new(path);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // The checkout launcher is a Unix script. On Windows, run the Python
    // package beside this crate while the source tree is still present
    // (a release binary's compile path is gone, so it uses PATH).
    #[cfg(not(windows))]
    {
        let source = root.join("bin/ghostless");
        if source.is_file() {
            return Command::new(source);
        }
    }
    #[cfg(windows)]
    if root.join("lib/ghostless/__main__.py").is_file() {
        return python_module_command(&root);
    }
    Command::new("ghostless")
}

#[cfg(windows)]
fn python_module_command(root: &std::path::Path) -> Command {
    let python = env::var_os("GHOSTLESS_PYTHON").unwrap_or_else(|| "python".into());
    let mut command = Command::new(python);
    command.arg("-m").arg("ghostless");
    let mut paths = vec![root.join("lib")];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    if let Ok(joined) = env::join_paths(paths) {
        command.env("PYTHONPATH", joined);
    }
    command.env("GHOSTLESS_SUITE", root);
    command
}

fn run_cli(args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut command = cli_command();
    command.args(args)
        .env("GHOSTLESS_NO_UPDATE", "1")
        // Keep the CLI on UTF-8 stdin/stdout on every OS (Windows defaults to
        // the locale, which would double-encode non-ASCII working text).
        .env("PYTHONUTF8", "1");
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| format!("Cannot launch ghostless CLI: {error}"))?;
    if let Some(value) = input {
        child.stdin.take().ok_or("Cannot open CLI input")?
            .write_all(value.as_bytes()).map_err(|error| error.to_string())?;
    }
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn resolve_project(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return require_project(&path);
    }
    let output = run_cli(&["which"], None)?;
    let reported = output.lines().find_map(|line| line.strip_prefix("project="));
    match reported.map(str::trim) {
        Some(value) if !value.is_empty() && value != "none" => require_project(&PathBuf::from(value)),
        _ => Err("Pass --project DIR, or run `ghostless init` in the folder where your writing lives.".into()),
    }
}

fn require_project(path: &std::path::Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!(
            "Project directory does not exist: {}. Run `ghostless init` to create one.",
            path.display()
        ));
    }
    if !path.is_dir() || !manifest_is_ghostless(path) {
        return Err(format!(
            "Not a ghostless project: {}. Run `ghostless init` to create one.",
            path.display()
        ));
    }
    path.canonicalize().map_err(|error| {
        format!("Project directory does not exist: {} ({error})", path.display())
    })
}

fn require_draft(path: &std::path::Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("Draft does not exist: {}", path.display()));
    }
    path.canonicalize().map_err(|error| format!("Draft does not exist: {} ({error})", path.display()))
}

fn manifest_is_ghostless(dir: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) else { return false };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return false };
    value.get("kind").and_then(Value::as_str) == Some("ghostless")
}

fn valid_session_id(id: &str) -> bool {
    // `YYYY-MM-DD[-section][-N]`: a date, then lowercase slug segments only.
    // This also rejects `/`, `..`, and other path traversal.
    let bytes = id.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    for (i, byte) in bytes.iter().enumerate() {
        let ok = match i {
            4 | 7 => *byte == b'-',
            0..=9 => byte.is_ascii_digit(),
            _ => byte.is_ascii_digit() || byte.is_ascii_lowercase() || *byte == b'-',
        };
        if !ok {
            return false;
        }
    }
    true
}

fn prepare() -> Result<Session, String> {
    let mut project = None;
    let mut draft = None;
    let mut session = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => project = args.next().map(PathBuf::from),
            "--draft" => draft = args.next().map(PathBuf::from),
            "--session" => session = args.next(),
            "--snap" | "--tree" | "--size" | "--theme" | "--scene" | "--at" | "--script" => {
                args.next().ok_or(format!("Missing value after {arg}"))?;
            }
            "-h" | "--help" => {
                println!("ghostless-editor [--project DIR] (--draft FILE | --session ID)\n--project is optional when the current directory, GHOSTLESS_PROJECT, or defaultProject already names a voice project.\nStart from a draft or reopen a local edit session.\nSet GHOSTLESS_CLI to an installed CLI path if needed.");
                std::process::exit(0);
            }
            _ => return Err(format!("Unknown argument: {arg}")),
        }
    }
    let project = resolve_project(project)?;
    if draft.is_some() == session.is_some() {
        return Err("Pass exactly one of --draft FILE or --session ID".into());
    }
    let id = if let Some(draft) = draft {
        let draft = require_draft(&draft)?;
        let output = run_cli(&["start", draft.to_str().ok_or("Draft path is not UTF-8")?,
            "--project", project.to_str().ok_or("Project path is not UTF-8")?, "--json"], None)?;
        serde_json::from_str::<Value>(&output).map_err(|error| error.to_string())?
            .get("id").and_then(Value::as_str).ok_or("CLI did not return a session ID")?.to_owned()
    } else {
        session.expect("one session supplied")
    };
    if !valid_session_id(&id) {
        return Err("Invalid session ID".into());
    }
    let folder = project.join("sessions").join(&id);
    if !folder.join("session.json").is_file() {
        return Err(format!("Session does not exist: {}", folder.display()));
    }
    let metadata: Value = serde_json::from_slice(&std::fs::read(folder.join("session.json"))
        .map_err(|error| format!("Cannot read {}: {error}", folder.join("session.json").display()))?)
        .map_err(|error| format!("Cannot read session metadata in {}: {error}", folder.display()))?;
    if metadata.get("kind").and_then(Value::as_str) != Some("ghostless-edit-session") {
        return Err("Unsupported session format".into());
    }
    let read = |file: &str| {
        let path = folder.join(file);
        std::fs::read_to_string(&path).map_err(|error| format!("Cannot read {}: {error}", path.display()))
    };
    Ok(Session {
        project,
        id,
        source_name: metadata.get("sourceDraftName").and_then(Value::as_str).unwrap_or("draft.md").into(),
        original: read("before.md")?,
        after: read("after.md")?,
        notes: if folder.join("notes.md").exists() { read("notes.md")? } else { String::new() },
        finished: folder.join("receipt.json").is_file(),
    })
}

fn main() {
    let session = match prepare() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("ghostless-editor: {error}");
            std::process::exit(2);
        }
    };
    let spec = AppSpec {
        min_size: Some((800.0, 620.0)),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: &[("diff", |editor| {
            editor.show_diff = true;
            editor.refresh_diff();
        })],
        ..AppSpec::new(
            "ghostless editor",
            (1120.0, 800.0),
            || unreachable!("initialized via App::init"),
            view,
        )
    };
    let app = App::new(spec);
    let handle = app.handle();
    let app = app
        .init(move || Editor::new(session, Box::new(move || handle.wake())))
        .on_close(|editor, _| editor.flush_on_exit())
        .on_quit(|editor, _| editor.flush_on_exit());
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
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

    #[test]
    fn working_textarea_metrics_scales_with_pane_height() {
        assert_eq!(working_textarea_metrics(None), (80.0, 5));
        assert_eq!(working_textarea_metrics(Some(40.0)), (80.0, 5));
        let (small_h, small_rows) = working_textarea_metrics(Some(320.0));
        let (large_h, large_rows) = working_textarea_metrics(Some(720.0));
        assert!(large_h > small_h);
        assert!(large_rows > small_rows);
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    struct EnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVar {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            // Tests that call the CLI hold ENV_LOCK, so no other test thread
            // reads these variables while they change.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ghostless-editor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_project(dir: &Path) {
        std::fs::write(dir.join("manifest.json"), "{\"kind\":\"ghostless\"}\n").unwrap();
    }

    #[test]
    fn missing_paths_name_the_path_and_init() {
        let missing = scratch("missing-project");
        std::fs::remove_dir_all(&missing).unwrap();
        let error = require_project(&missing).unwrap_err();
        assert!(error.contains("Project directory does not exist"), "{error}");
        assert!(error.contains(&missing.display().to_string()), "{error}");
        assert!(error.contains("ghostless init"), "{error}");

        let empty = scratch("not-a-project");
        let error = require_project(&empty).unwrap_err();
        assert!(error.contains("Not a ghostless project"), "{error}");
        assert!(error.contains(&empty.display().to_string()), "{error}");
        assert!(error.contains("ghostless init"), "{error}");

        let draft = missing.join("draft.md");
        let error = require_draft(&draft).unwrap_err();
        assert!(error.contains("Draft does not exist"), "{error}");
        assert!(error.contains(&draft.display().to_string()), "{error}");
    }

    #[test]
    fn resolves_project_from_cwd_env_and_default() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let project = scratch("cwd");
        write_project(&project);
        let elsewhere = scratch("empty");
        let home = scratch("home");
        let data = home.join(".ghostless");
        std::fs::create_dir_all(&data).unwrap();
        let canonical = project.canonicalize().unwrap();

        let _cwd = CwdGuard(std::env::current_dir().unwrap());
        let _clear_project = EnvVar::set("GHOSTLESS_PROJECT", None);
        let _home = EnvVar::set("GHOSTLESS_HOME", Some(data.to_str().unwrap()));

        std::env::set_current_dir(&project).unwrap();
        let resolved = resolve_project(None).expect("cwd project");
        assert_eq!(resolved, canonical);

        std::env::set_current_dir(&elsewhere).unwrap();
        let via_env = {
            let _project_env = EnvVar::set("GHOSTLESS_PROJECT", Some(project.to_str().unwrap()));
            resolve_project(None).expect("GHOSTLESS_PROJECT")
        };
        assert_eq!(via_env, canonical);

        let payload = serde_json::json!({"defaultProject": canonical});
        std::fs::write(data.join("config.json"), serde_json::to_string(&payload).unwrap()).unwrap();
        let via_default = resolve_project(None).expect("defaultProject");
        assert_eq!(via_default, canonical);
    }
}
