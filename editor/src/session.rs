use std::{env, path::PathBuf};
use serde_json::Value;
use crate::{cli::{resolve_project, require_project, require_draft, run_cli, discover_voice_project}, prefs};

#[derive(Clone)]
pub(crate) struct Session {
    pub(crate) project: PathBuf,
    pub(crate) id: String,
    pub(crate) source_name: String,
    pub(crate) original: String,
    pub(crate) after: String,
    pub(crate) notes: String,
    pub(crate) before_sha256: String,
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

pub(crate) enum LaunchTarget {
    Session(Session),
    Workspace { folder: PathBuf, voice_project: Option<PathBuf>, initial_file: Option<PathBuf> },
    Welcome { voice_project: Option<PathBuf> },
}

pub(crate) fn prepare() -> Result<LaunchTarget, String> {
    parse_launch(env::args().skip(1).collect())
}

pub(crate) fn parse_launch(args: Vec<String>) -> Result<LaunchTarget, String> {
    let mut project = None;
    let mut draft = None;
    let mut session = None;
    let mut link_draft = false;
    let mut folder = None;
    let mut positional = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => project = Some(PathBuf::from(args.next().ok_or("Missing value after --project")?)),
            "--draft" => draft = Some(PathBuf::from(args.next().ok_or("Missing value after --draft")?)),
            "--session" => session = Some(args.next().ok_or("Missing value after --session")?),
            "--link-draft" => link_draft = true,
            "--folder" => folder = Some(PathBuf::from(args.next().ok_or("Missing value after --folder")?)),
            "--snap" | "--tree" | "--size" | "--theme" | "--scene" | "--at" | "--script" => { args.next().ok_or(format!("Missing value after {arg}"))?; }
            "-h" | "--help" => { println!("ghostless-editor [--folder DIR | FILE] [--project DIR] [--draft FILE [--link-draft] | --session ID]"); std::process::exit(0); }
            _ if !arg.starts_with('-') && positional.is_none() => positional = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unknown argument: {arg}")),
        }
    }
    if link_draft && draft.is_none() { return Err("--link-draft requires --draft FILE".into()); }
    if draft.is_some() && session.is_some() { return Err("Pass exactly one of --draft FILE or --session ID".into()); }
    if (draft.is_some() || session.is_some()) && (folder.is_some() || positional.is_some()) { return Err("Cannot combine workspace and session targets".into()); }
    if folder.is_some() && positional.is_some() { return Err("Pass one workspace target".into()); }
    if draft.is_none() && session.is_none() {
        let explicit = project.as_deref().map(require_project).transpose()?;
        let target = folder.or(positional).or_else(|| prefs::load_store().last_workspace.filter(|p| p.is_dir()));
        if let Some(path) = target {
            let path = path.canonicalize().map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
            let (folder, initial_file) = if path.is_file() { (path.parent().ok_or("File has no parent")?.to_path_buf(), Some(path)) } else if path.is_dir() { (path, None) } else { return Err("Workspace must be a folder or file".into()); };
            let voice_project = discover_voice_project(&folder, explicit.as_deref());
            return Ok(LaunchTarget::Workspace { folder, voice_project, initial_file });
        }
        return Ok(LaunchTarget::Welcome { voice_project: explicit.or_else(|| resolve_project(None).ok()) });
    }
    let project = resolve_project(project)?;
    let linked_draft = if link_draft { draft.as_deref().map(require_draft).transpose()? } else { None };
    let id = if let Some(draft) = draft {
        let draft = require_draft(&draft)?;
        let output = run_cli(&["start", draft.to_str().ok_or("Draft path is not UTF-8")?,
            "--project", project.to_str().ok_or("Project path is not UTF-8")?, "--json"], None)?;
        serde_json::from_str::<Value>(&output).map_err(|error| error.to_string())?
            .get("id").and_then(Value::as_str).ok_or("CLI did not return a session ID")?.to_owned()
    } else {
        session.expect("one session supplied")
    };
    let loaded = load_session(project, id)?;
    if let Some(path) = linked_draft {
        let current = crate::fs_io::read_utf8(&path)?;
        if crate::fs_io::sha256_text(&current) != loaded.before_sha256 {
            return Err("Draft changed while starting the session; the session was created without a link".into());
        }
        let mut store = prefs::load_store();
        store.session_links.insert(prefs::EditorStateStore::session_link_key(&loaded.project, &loaded.id), prefs::SessionLink { workspace_file: path, last_synced_sha256: loaded.before_sha256.clone() });
        prefs::save_store(&store)?;
    }
    Ok(LaunchTarget::Session(loaded))
}

pub(crate) fn load_session(project: PathBuf, id: String) -> Result<Session, String> {
    if !valid_session_id(&id) {
        return Err("Invalid session ID".into());
    }
    let folder = project.join("sessions").join(&id);
    if !folder.join("session.json").is_file() {
        return Err(format!("Session does not exist: {}", folder.display()));
    }
    let metadata: Value = serde_json::from_slice(
        &std::fs::read(folder.join("session.json"))
            .map_err(|error| format!("Cannot read {}: {error}", folder.join("session.json").display()))?,
    )
    .map_err(|error| format!("Cannot read session metadata in {}: {error}", folder.display()))?;
    if metadata.get("kind").and_then(Value::as_str) != Some("ghostless-edit-session") {
        return Err("Unsupported session format".into());
    }
    let read = |file: &str| {
        let path = folder.join(file);
        std::fs::read_to_string(&path).map_err(|error| format!("Cannot read {}: {error}", path.display()))
    };
    let original = read("before.md")?;
    let before_sha256 = metadata
        .get("beforeSha256")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| crate::fs_io::sha256_text(&original));
    Ok(Session {
        project,
        id,
        source_name: metadata.get("sourceDraftName").and_then(Value::as_str).unwrap_or("draft.md").into(),
        original,
        after: read("after.md")?,
        notes: if folder.join("notes.md").exists() { read("notes.md")? } else { String::new() },
        before_sha256,
        finished: folder.join("receipt.json").is_file(),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn opt_in_draft_link_is_persisted_with_baseline_hash() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-link-launch-{}", std::process::id()));
        let home = base.join("home"); let voice = base.join("voice");
        fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); }
        run_cli(&["init", voice.to_str().unwrap(), "--no-git"], None).unwrap();
        let draft = base.join("draft.md"); fs::write(&draft, "Original draft text.\n").unwrap();
        let target = parse_launch(vec!["--project".into(), voice.display().to_string(), "--draft".into(), draft.display().to_string(), "--link-draft".into()]).unwrap();
        let LaunchTarget::Session(session) = target else { panic!("expected session"); };
        let store = prefs::load_store();
        let link = store.session_links.get(&prefs::EditorStateStore::session_link_key(&session.project, &session.id)).unwrap();
        assert_eq!(link.workspace_file, draft.canonicalize().unwrap());
        assert_eq!(link.last_synced_sha256, crate::fs_io::sha256_text("Original draft text.\n"));
        let key = prefs::EditorStateStore::session_link_key(&session.project, &session.id);
        let mut editor = crate::state::Editor::with_handlers(session.clone(), store, Box::new(run_cli), Box::new(crate::worker::execute_write_op), Box::new(|| {}));
        editor.active_working_edit_mut().text = "Revised text.\n".into();
        editor.changed_active(&mut rune::Tx::default());
        editor.flush_on_exit();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "Revised text.\n");
        assert_eq!(prefs::load_store().session_links[&key].last_synced_sha256, crate::fs_io::sha256_text("Revised text.\n"));
        fs::write(&draft, "External update.\n").unwrap();
        let loaded = load_session(session.project.clone(), session.id.clone()).unwrap();
        let mut reopened = crate::state::Editor::with_handlers(loaded, prefs::load_store(), Box::new(run_cli), Box::new(crate::worker::execute_write_op), Box::new(|| {}));
        reopened.active_working_edit_mut().text = "Must not overwrite.\n".into();
        reopened.changed_active(&mut rune::Tx::default());
        reopened.flush_on_exit();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "External update.\n");
        assert!(reopened.status.contains("changed outside"));
        unsafe { if let Some(value) = old_home { std::env::set_var("GHOSTLESS_HOME", value); } else { std::env::remove_var("GHOSTLESS_HOME"); } }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn launch_targets_and_voice_discovery() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ghostless-launch-{}", std::process::id()));
        let home = base.join("home");
        let workspace = base.join("writing");
        let voice = workspace.join("voice");
        let nested = workspace.join("seeds/demo");
        fs::create_dir_all(&home).unwrap(); fs::create_dir_all(&voice).unwrap(); fs::create_dir_all(&nested).unwrap();
        fs::write(voice.join("manifest.json"), "{\"kind\":\"ghostless\"}").unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME"); let old_project = std::env::var_os("GHOSTLESS_PROJECT");
        let old_cwd = std::env::current_dir().unwrap();
        unsafe { std::env::set_var("GHOSTLESS_HOME", &home); std::env::remove_var("GHOSTLESS_PROJECT"); }
        std::env::set_current_dir(&home).unwrap();
        assert!(matches!(parse_launch(vec![]).unwrap(), LaunchTarget::Welcome { .. }));
        assert_eq!(discover_voice_project(&voice, None), Some(voice.canonicalize().unwrap()));
        assert_eq!(discover_voice_project(&workspace, None), Some(voice.canonicalize().unwrap()));
        assert_eq!(discover_voice_project(&nested, None), Some(voice.canonicalize().unwrap()));
        assert_eq!(discover_voice_project(&home, None), None);
        fs::write(home.join("config.json"), serde_json::json!({"defaultProject": voice}).to_string()).unwrap();
        assert_eq!(discover_voice_project(&home, None), None);
        unsafe { std::env::set_var("GHOSTLESS_PROJECT", &voice); }
        assert_eq!(discover_voice_project(&home, None), None);
        assert_eq!(discover_voice_project(&home, Some(&voice)), Some(voice.canonicalize().unwrap()));
        match parse_launch(vec!["--folder".into(), home.display().to_string(), "--project".into(), voice.display().to_string()]).unwrap() {
            LaunchTarget::Workspace { voice_project, .. } => assert_eq!(voice_project, Some(voice.canonicalize().unwrap())),
            _ => panic!("expected workspace launch"),
        }
        unsafe { std::env::remove_var("GHOSTLESS_PROJECT"); }
        assert!(matches!(parse_launch(vec!["--folder".into(), workspace.display().to_string()]).unwrap(), LaunchTarget::Workspace { .. }));
        let mut store = prefs::EditorStateStore::default(); store.record_workspace(&workspace); prefs::save_store(&store).unwrap();
        assert!(matches!(parse_launch(vec![]).unwrap(), LaunchTarget::Workspace { .. }));
        assert!(parse_launch(vec!["--project".into(), home.display().to_string()]).is_err());
        assert!(parse_launch(vec!["--draft".into(), "a".into(), "--session".into(), "b".into()]).is_err());
        assert!(parse_launch(vec!["--project".into(), voice.display().to_string(), "--draft".into(), "missing".into()]).is_err());
        assert!(parse_launch(vec!["--project".into(), voice.display().to_string(), "--session".into(), "bad".into()]).is_err());
        std::env::set_current_dir(old_cwd).unwrap();
        unsafe { if let Some(v) = old_home { std::env::set_var("GHOSTLESS_HOME", v); } else { std::env::remove_var("GHOSTLESS_HOME"); } if let Some(v) = old_project { std::env::set_var("GHOSTLESS_PROJECT", v); } else { std::env::remove_var("GHOSTLESS_PROJECT"); } }
        fs::remove_dir_all(base).unwrap();
    }
}
