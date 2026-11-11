//! Local Rune editor over ghostless's versioned edit-session files.

use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rune::{AppSpec, Cx, El, KeyEvent, ScrollSpec, TextEdit, Tx, col, row, text};
use rune_kit::diff::{DiffMode, DiffState, DiffView, FileDiff};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea};
use serde_json::{Value, json};

static SESSION: OnceLock<Session> = OnceLock::new();
const SAVE_DELAY: Duration = Duration::from_millis(800);

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

struct Editor {
    session: Session,
    after: TextEdit,
    notes: TextEdit,
    diff: DiffState,
    show_diff: bool,
    dirty: bool,
    changed_at: Option<Instant>,
    status: String,
}

impl Editor {
    fn new() -> Self {
        let session = SESSION.get().expect("session prepared before Rune starts").clone();
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &session.source_name, &session.original, &session.after,
        )]);
        diff.set_mode(DiffMode::Split);
        Self {
            after: TextEdit::new(session.after.clone()),
            notes: TextEdit::new(session.notes.clone()),
            diff, show_diff: false, dirty: false, changed_at: None,
            status: if session.finished { "Finished".into() } else { "Ready".into() },
            session,
        }
    }

    fn changed(&mut self, tx: &mut Tx) {
        self.dirty = true;
        self.changed_at = Some(Instant::now());
        self.status = "Unsaved changes".into();
        tx.wake_after(SAVE_DELAY);
    }

    fn refresh_diff(&mut self) {
        let mut diff = DiffState::new(vec![FileDiff::from_texts(
            &self.session.source_name, &self.session.original, &self.after.text,
        )]);
        diff.set_mode(DiffMode::Split);
        self.diff = diff;
    }

    fn save(&mut self) -> Result<(), String> {
        if self.session.finished { return Ok(()); }
        let payload = json!({"after": self.after.text, "notes": self.notes.text});
        let project = self.session.project.to_str().ok_or("Project path is not UTF-8")?;
        run_cli(&["snapshot", "--session", &self.session.id, "--project", project],
                Some(&payload.to_string()))?;
        self.dirty = false;
        self.changed_at = None;
        self.status = "Saved locally".into();
        Ok(())
    }

    fn save_clicked(&mut self) {
        if let Err(error) = self.save() { self.status = format!("Save failed: {error}"); }
    }

    fn finish(&mut self) {
        if self.session.finished { return; }
        if let Err(error) = self.save() {
            self.status = format!("Finish stopped: {error}");
            return;
        }
        let Some(project) = self.session.project.to_str() else {
            self.status = "Project path is not UTF-8".into();
            return;
        };
        match run_cli(&["finish", "--session", &self.session.id, "--project", project, "--json"], None) {
            Ok(_) => {
                self.session.finished = true;
                self.status = "Finished. Final and receipt saved locally.".into();
            }
            Err(error) => self.status = format!("Finish stopped: {error}"),
        }
    }
}

fn poll(editor: &mut Editor, _: f64, tx: &mut Tx) -> bool {
    if !editor.dirty || editor.session.finished { return false; }
    let elapsed = editor.changed_at.map_or(SAVE_DELAY, |at| at.elapsed());
    if elapsed >= SAVE_DELAY {
        editor.save_clicked();
        true
    } else {
        tx.wake_after(SAVE_DELAY - elapsed);
        false
    }
}

fn on_key(editor: &mut Editor, key: &KeyEvent, _: &mut Tx) -> bool {
    if key.is_command('s') {
        editor.save_clicked();
        return true;
    }
    if key.is_command('d') {
        editor.show_diff = !editor.show_diff;
        if editor.show_diff { editor.refresh_diff(); }
        return true;
    }
    false
}

fn view(editor: &Editor, cx: &mut Cx<Editor>) -> El {
    let kit = Kit::neutral(cx.system_dark());
    let modes = SegmentedControl::new(&["Edit", "Diff"], Some(usize::from(editor.show_diff)))
        .width(Some(140.0))
        .on_pick(|editor: &mut Editor, index, _: &mut Tx| {
            editor.show_diff = index == 1;
            if editor.show_diff { editor.refresh_diff(); }
        });
    let top = row().key("toolbar").h(58.0).w_full().px(18.0).gap(12.0)
        .items_center().fill(kit.c.layer_01)
        .child(text("ghostless").font(kit.ty.ui_medium).color(kit.c.text_base))
        .child(text(format!("/ {}", editor.session.source_name)).font(kit.ty.ui).color(kit.c.text_muted))
        .child(row().grow(1.0)).child(modes.build(&kit))
        .child(Button::new("Save").variant(ButtonVariant::Outline)
            .disabled(editor.session.finished).on_press(|editor: &mut Editor| editor.save_clicked()).build(&kit))
        .child(Button::new("Finish").variant(ButtonVariant::Submit)
            .disabled(editor.session.finished).on_press(|editor: &mut Editor| editor.finish()).build(&kit));
    let content = if editor.show_diff {
        DiffView::new(|editor: &Editor| &editor.diff, |editor: &mut Editor| &mut editor.diff)
            .build(editor, &kit)
    } else {
        let original = col().key("original-pane").grow(1.0).h_full().gap(12.0)
            .padding(18.0).fill(kit.c.bg_base)
            .child(text("ORIGINAL · READ ONLY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(col().grow(1.0).w_full().scroll(ScrollSpec { stick_bottom: false })
                .child(text(editor.session.original.clone()).font(kit.ty.body)
                    .color(kit.c.text_base).wrap().selectable(true)));
        let working = col().key("working-pane").grow(1.0).h_full().gap(12.0)
            .padding(18.0).fill(kit.c.layer_01)
            .child(text("WORKING COPY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(TextArea::new(&editor.after).label("Working draft")
                .width(None).rows(21).disabled(editor.session.finished)
                .on_edit(|editor: &mut Editor| &mut editor.after)
                .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed(tx))
                .build(&kit));
        row().key("edit-panes").grow(1.0).w_full().child(original).child(working)
    };
    let notes = col().key("notes-pane").h(142.0).w_full().padding(14.0)
        .gap(7.0).fill(kit.c.layer_01)
        .child(text("NOTES · WHY DID YOU CHANGE IT?").font(kit.ty.ui_medium).color(kit.c.text_muted))
        .child(TextArea::new(&editor.notes).label("Edit notes")
            .placeholder("Optional context for your future self")
            .width(None).rows(3).disabled(editor.session.finished)
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
    if let Some(path) = env::var_os("GHOSTLESS_CLI") { return Command::new(path); }
    // The checkout launcher is a Unix script; on Windows it cannot run, so
    // fall through to an installed `ghostless` on PATH there.
    #[cfg(not(windows))]
    {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bin/ghostless");
        if source.is_file() {
            return Command::new(source);
        }
    }
    Command::new("ghostless")
}

fn run_cli(args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut command = cli_command();
    command.args(args).env("GHOSTLESS_NO_UPDATE", "1");
    if input.is_some() { command.stdin(Stdio::piped()); }
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

fn valid_session_id(id: &str) -> bool {
    // `YYYY-MM-DD[-section][-N]`: a date, then lowercase slug segments only.
    // This also rejects `/`, `..`, and other path traversal.
    let bytes = id.as_bytes();
    if bytes.len() < 10 { return false; }
    for (i, byte) in bytes.iter().enumerate() {
        let ok = match i {
            4 | 7 => *byte == b'-',
            0..=9 => byte.is_ascii_digit(),
            _ => byte.is_ascii_digit() || byte.is_ascii_lowercase() || *byte == b'-',
        };
        if !ok { return false; }
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
                println!("ghostless-editor --project DIR (--draft FILE | --session ID)\nStart from a draft or reopen a local edit session.\nSet GHOSTLESS_CLI to an installed CLI path if needed.");
                std::process::exit(0);
            }
            _ => return Err(format!("Unknown argument: {arg}")),
        }
    }
    let project = project.ok_or("Pass --project DIR")?.canonicalize().map_err(|error| error.to_string())?;
    if draft.is_some() == session.is_some() { return Err("Pass exactly one of --draft FILE or --session ID".into()); }
    let id = if let Some(draft) = draft {
        let draft = draft.canonicalize().map_err(|error| error.to_string())?;
        let output = run_cli(&["start", draft.to_str().ok_or("Draft path is not UTF-8")?,
            "--project", project.to_str().ok_or("Project path is not UTF-8")?, "--json"], None)?;
        serde_json::from_str::<Value>(&output).map_err(|error| error.to_string())?
            .get("id").and_then(Value::as_str).ok_or("CLI did not return a session ID")?.to_owned()
    } else { session.expect("one session supplied") };
    if !valid_session_id(&id) {
        return Err("Invalid session ID".into());
    }
    let folder = project.join("sessions").join(&id);
    let metadata: Value = serde_json::from_slice(&std::fs::read(folder.join("session.json"))
        .map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    if metadata.get("kind").and_then(Value::as_str) != Some("ghostless-edit-session") {
        return Err("Unsupported session format".into());
    }
    let read = |file: &str| std::fs::read_to_string(folder.join(file)).map_err(|error| error.to_string());
    Ok(Session {
        project, id,
        source_name: metadata.get("sourceDraftName").and_then(Value::as_str).unwrap_or("draft.md").into(),
        original: read("before.md")?, after: read("after.md")?,
        notes: if folder.join("notes.md").exists() { read("notes.md")? } else { String::new() },
        finished: folder.join("receipt.json").is_file(),
    })
}

fn main() {
    let session = match prepare() {
        Ok(session) => session,
        Err(error) => { eprintln!("ghostless-editor: {error}"); std::process::exit(2); }
    };
    SESSION.set(session).unwrap_or_else(|_| panic!("editor started twice"));
    rune::run(AppSpec {
        min_size: Some((800.0, 620.0)), poll: Some(poll), on_key: Some(on_key),
        scenes: &[("diff", |editor| { editor.show_diff = true; editor.refresh_diff(); })],
        ..AppSpec::new("ghostless editor", (1120.0, 800.0), Editor::new, view)
    });
}
