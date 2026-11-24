use std::{env, path::PathBuf};
use serde_json::Value;
use crate::cli::{resolve_project, require_draft, run_cli};

#[derive(Clone)]
pub(crate) struct Session {
    pub(crate) project: PathBuf,
    pub(crate) id: String,
    pub(crate) source_name: String,
    pub(crate) original: String,
    pub(crate) after: String,
    pub(crate) notes: String,
    pub(crate) finished: bool,
}

pub(crate) fn valid_session_id(id: &str) -> bool {
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

pub(crate) fn prepare() -> Result<Session, String> {
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

