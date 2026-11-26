use crate::contracts::{StartedSession, session_metadata};
use crate::error::EditorError;
use crate::{
    cli::{discover_voice_project, require_draft, require_project, resolve_project, run_cli},
    prefs,
};
use std::{env, path::PathBuf};

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
    // Python `SESSION_ID_RE`: `[0-9]{4}-[0-9]{2}-[0-9]{2}(?:-[a-z0-9]+)*\Z`
    // A date, then optional `-[a-z0-9]+` segments. Rejects a missing hyphen,
    // a doubled `--`, and a trailing `-`.
    let mut parts = id.split('-');
    let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let date_ok = |part: &str, len: usize| {
        part.len() == len && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    date_ok(year, 4)
        && date_ok(month, 2)
        && date_ok(day, 2)
        && parts.all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
        })
}

pub(crate) enum LaunchTarget {
    Session(Session),
    Workspace {
        folder: PathBuf,
        voice_project: Option<PathBuf>,
        initial_file: Option<PathBuf>,
    },
    Welcome {
        voice_project: Option<PathBuf>,
    },
}

pub(crate) fn prepare() -> Result<LaunchTarget, EditorError> {
    parse_launch(env::args().skip(1).collect())
}

pub(crate) fn parse_launch(args: Vec<String>) -> Result<LaunchTarget, EditorError> {
    let mut project = None;
    let mut draft = None;
    let mut session = None;
    let mut link_draft = false;
    let mut link_path = None;
    let mut folder = None;
    let mut positional = None;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => {
                project = Some(PathBuf::from(
                    args.next().ok_or("Missing value after --project")?,
                ))
            }
            "--draft" => {
                draft = Some(PathBuf::from(
                    args.next().ok_or("Missing value after --draft")?,
                ))
            }
            "--session" => session = Some(args.next().ok_or("Missing value after --session")?),
            "--link-draft" => {
                link_draft = true;
                if args.peek().is_some_and(|next| !next.starts_with('-')) {
                    link_path = Some(PathBuf::from(args.next().expect("peeked link path")));
                }
            }
            "--folder" => {
                folder = Some(PathBuf::from(
                    args.next().ok_or("Missing value after --folder")?,
                ))
            }
            "--snap" | "--tree" | "--size" | "--scale" | "--theme" | "--scene" | "--at"
            | "--script" => {
                args.next().ok_or(format!("Missing value after {arg}"))?;
            }
            "--realtime" => {}
            "-h" | "--help" => {
                println!(
                    "ghostless-editor [--folder DIR | FILE] [--project DIR] [--draft FILE [--link-draft] | --session ID [--link-draft FILE]]"
                );
                std::process::exit(0);
            }
            _ if !arg.starts_with('-') && positional.is_none() => {
                positional = Some(PathBuf::from(arg))
            }
            _ => return Err(EditorError::message(format!("Unknown argument: {arg}"))),
        }
    }
    if link_draft && draft.is_none() && link_path.is_none() {
        return Err("--link-draft requires a draft file".into());
    }
    if link_path.is_some() && draft.is_some() {
        return Err("Pass the draft once, either as --draft or after --link-draft".into());
    }
    if draft.is_some() && session.is_some() {
        return Err("Pass exactly one of --draft FILE or --session ID".into());
    }
    if (draft.is_some() || session.is_some()) && (folder.is_some() || positional.is_some()) {
        return Err("Cannot combine workspace and session targets".into());
    }
    if folder.is_some() && positional.is_some() {
        return Err("Pass one workspace target".into());
    }
    if draft.is_none() && session.is_none() {
        let explicit = project.as_deref().map(require_project).transpose()?;
        let target = folder
            .or(positional)
            .or_else(|| prefs::load_store().last_workspace.filter(|p| p.is_dir()));
        if let Some(path) = target {
            let path = path
                .canonicalize()
                .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
            let (folder, initial_file) = if path.is_file() {
                (
                    path.parent().ok_or("File has no parent")?.to_path_buf(),
                    Some(path),
                )
            } else if path.is_dir() {
                (path, None)
            } else {
                return Err("Workspace must be a folder or file".into());
            };
            let voice_project = discover_voice_project(&folder, explicit.as_deref());
            return Ok(LaunchTarget::Workspace {
                folder,
                voice_project,
                initial_file,
            });
        }
        return Ok(LaunchTarget::Welcome {
            voice_project: explicit.or_else(|| resolve_project(None).ok()),
        });
    }
    let project = resolve_project(project)?;
    let linked_draft = if link_draft {
        let path = link_path
            .as_deref()
            .or(draft.as_deref())
            .ok_or("Missing linked draft")?;
        Some(require_draft(path)?)
    } else {
        None
    };
    let id = if let Some(draft) = draft {
        let draft = require_draft(&draft)?;
        let output = run_cli(
            &[
                "start",
                draft.to_str().ok_or("Draft path is not UTF-8")?,
                "--project",
                project.to_str().ok_or("Project path is not UTF-8")?,
                "--json",
            ],
            None,
        )?;
        serde_json::from_str::<StartedSession>(&output)
            .map_err(|error| EditorError::Json {
                context: format!("Invalid start response: {error}"),
            })?
            .id
    } else {
        session.expect("one session supplied")
    };
    let loaded = load_session(project, id)?;
    if let Some(path) = linked_draft {
        let current = crate::fs_io::read_utf8(&path)?;
        // Python reads drafts in universal-newline mode. Compare the same text
        // before linking, then store a hash of the actual draft for later writes.
        let normalized = current.replace("\r\n", "\n").replace('\r', "\n");
        if crate::fs_io::sha256_text(&normalized) != loaded.before_sha256 {
            return Err(
                "Draft changed while starting the session; the session was created without a link"
                    .into(),
            );
        }
        let mut store = prefs::load_store();
        store.session_links.insert(
            prefs::EditorStateStore::session_link_key(&loaded.project, &loaded.id),
            prefs::SessionLink {
                workspace_file: path,
                last_synced_sha256: crate::fs_io::sha256_text(&current),
            },
        );
        prefs::save_store(&store)?;
    }
    Ok(LaunchTarget::Session(loaded))
}

pub(crate) fn load_session(project: PathBuf, id: String) -> Result<Session, EditorError> {
    if !valid_session_id(&id) {
        return Err(EditorError::InvalidSessionId(id));
    }
    let folder = project.join("sessions").join(&id);
    if !folder.join("session.json").is_file() {
        return Err(EditorError::message(format!(
            "Session does not exist: {}",
            folder.display()
        )));
    }
    let metadata = session_metadata(&folder)?;
    let read = |file: &str| {
        let path = folder.join(file);
        std::fs::read_to_string(&path)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))
    };
    let original = read("before.md")?;
    let before_sha256 = metadata
        .before_sha256
        .unwrap_or_else(|| crate::fs_io::sha256_text(&original));
    Ok(Session {
        project,
        id,
        source_name: metadata
            .source_draft_name
            .unwrap_or_else(|| "draft.md".into()),
        original,
        after: read("after.md")?,
        notes: if folder.join("notes.md").exists() {
            read("notes.md")?
        } else {
            String::new()
        },
        before_sha256,
        finished: folder.join("receipt.json").is_file(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn session_ids_match_pythons_session_id_re() {
        for id in [
            "2026-01-01",
            "2026-01-01-notes",
            "2026-01-01-notes-2",
            "2026-01-01-a1",
        ] {
            assert!(valid_session_id(id), "{id}");
        }
        for id in [
            "2026-01-01abc",
            "2026-01-01--",
            "2026-01-01-",
            "2026-01-01-Notes",
            "2026-1-01",
            "2026-01-01/notes",
            "../2026-01-01",
        ] {
            assert!(!valid_session_id(id), "{id}");
        }
    }

    #[test]
    fn opt_in_draft_link_is_persisted_with_baseline_hash() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base =
            std::env::temp_dir().join(format!("ghostless-link-launch-{}", std::process::id()));
        let home = base.join("home");
        let voice = base.join("voice");
        fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe {
            std::env::set_var("GHOSTLESS_HOME", &home);
        }
        run_cli(&["init", voice.to_str().unwrap(), "--no-git"], None).unwrap();
        let draft = base.join("draft.md");
        let original = "Original draft text.\r\nSecond line.\r\n";
        fs::write(&draft, original).unwrap();
        let target = parse_launch(vec![
            "--project".into(),
            voice.display().to_string(),
            "--draft".into(),
            draft.display().to_string(),
            "--link-draft".into(),
        ])
        .unwrap();
        let LaunchTarget::Session(session) = target else {
            panic!("expected session");
        };
        let store = prefs::load_store();
        let link = store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &session.project,
                &session.id,
            ))
            .unwrap();
        assert_eq!(link.workspace_file, draft.canonicalize().unwrap());
        assert_eq!(link.last_synced_sha256, crate::fs_io::sha256_text(original));
        assert_eq!(session.original, original.replace("\r\n", "\n"));
        let key = prefs::EditorStateStore::session_link_key(&session.project, &session.id);
        let mut editor = crate::state::Editor::with_handlers(
            session.clone(),
            store,
            Box::new(run_cli),
            Box::new(crate::worker::execute_write_op),
            Box::new(|| {}),
        );
        editor.active_working_edit_mut().text = "Revised text.\n".into();
        editor.changed_active(&mut rune::Tx::default());
        editor.flush_on_exit();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "Revised text.\n");
        assert_eq!(
            prefs::load_store().session_links[&key].last_synced_sha256,
            crate::fs_io::sha256_text("Revised text.\n")
        );
        fs::write(&draft, "External update.\n").unwrap();
        let loaded = load_session(session.project.clone(), session.id.clone()).unwrap();
        let mut reopened = crate::state::Editor::with_handlers(
            loaded,
            prefs::load_store(),
            Box::new(run_cli),
            Box::new(crate::worker::execute_write_op),
            Box::new(|| {}),
        );
        reopened.active_working_edit_mut().text = "Must not overwrite.\n".into();
        reopened.changed_active(&mut rune::Tx::default());
        assert!(reopened.flush_on_exit());
        assert_eq!(fs::read_to_string(&draft).unwrap(), "External update.\n");
        let saved = load_session(session.project.clone(), session.id.clone()).unwrap();
        assert_eq!(saved.after, "Must not overwrite.\n");
        assert_eq!(
            prefs::load_store().session_links[&key].last_synced_sha256,
            crate::fs_io::sha256_text("Revised text.\n")
        );
        assert!(reopened.status.contains("changed outside"));
        unsafe {
            if let Some(value) = old_home {
                std::env::set_var("GHOSTLESS_HOME", value);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn opening_an_existing_session_can_link_its_draft() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base =
            std::env::temp_dir().join(format!("ghostless-session-link-{}", std::process::id()));
        let home = base.join("home");
        let voice = base.join("voice");
        fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME");
        unsafe {
            std::env::set_var("GHOSTLESS_HOME", &home);
        }
        run_cli(&["init", voice.to_str().unwrap(), "--no-git"], None).unwrap();
        let id = "2026-04-02-notes";
        let folder = voice.join("sessions").join(id);
        fs::create_dir_all(&folder).unwrap();
        let before = "Original draft text.\n";
        let sha = crate::fs_io::sha256_text(before);
        crate::fs_io::atomic_write_text(&folder.join("before.md"), before).unwrap();
        crate::fs_io::atomic_write_text(&folder.join("after.md"), before).unwrap();
        crate::fs_io::atomic_write_text(&folder.join("session.json"), &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{sha}","sourceDraftName":"draft.md"}}"#)).unwrap();
        let draft = base.join("draft.md");
        fs::write(&draft, before).unwrap();
        let target = parse_launch(vec![
            "--project".into(),
            voice.display().to_string(),
            "--session".into(),
            id.into(),
            "--link-draft".into(),
            draft.display().to_string(),
        ])
        .unwrap();
        let LaunchTarget::Session(session) = target else {
            panic!("expected session");
        };
        assert_eq!(session.id, id);
        let store = prefs::load_store();
        let link = store
            .session_links
            .get(&prefs::EditorStateStore::session_link_key(
                &session.project,
                id,
            ))
            .unwrap();
        assert_eq!(link.workspace_file, draft.canonicalize().unwrap());
        assert_eq!(link.last_synced_sha256, sha);
        assert!(folder.join("session.json").is_file());
        unsafe {
            if let Some(value) = old_home {
                std::env::set_var("GHOSTLESS_HOME", value);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
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
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&voice).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(voice.join("manifest.json"), "{\"kind\":\"ghostless\"}").unwrap();
        let old_home = std::env::var_os("GHOSTLESS_HOME");
        let old_project = std::env::var_os("GHOSTLESS_PROJECT");
        let old_cwd = std::env::current_dir().unwrap();
        unsafe {
            std::env::set_var("GHOSTLESS_HOME", &home);
            std::env::remove_var("GHOSTLESS_PROJECT");
        }
        std::env::set_current_dir(&home).unwrap();
        assert!(matches!(
            parse_launch(vec![]).unwrap(),
            LaunchTarget::Welcome { .. }
        ));
        assert_eq!(
            discover_voice_project(&voice, None),
            Some(voice.canonicalize().unwrap())
        );
        assert_eq!(
            discover_voice_project(&workspace, None),
            Some(voice.canonicalize().unwrap())
        );
        assert_eq!(
            discover_voice_project(&nested, None),
            Some(voice.canonicalize().unwrap())
        );
        assert_eq!(discover_voice_project(&home, None), None);
        fs::write(
            home.join("config.json"),
            serde_json::json!({"defaultProject": voice}).to_string(),
        )
        .unwrap();
        assert_eq!(discover_voice_project(&home, None), None);
        unsafe {
            std::env::set_var("GHOSTLESS_PROJECT", &voice);
        }
        assert_eq!(discover_voice_project(&home, None), None);
        assert_eq!(
            discover_voice_project(&home, Some(&voice)),
            Some(voice.canonicalize().unwrap())
        );
        match parse_launch(vec![
            "--folder".into(),
            home.display().to_string(),
            "--project".into(),
            voice.display().to_string(),
        ])
        .unwrap()
        {
            LaunchTarget::Workspace { voice_project, .. } => {
                assert_eq!(voice_project, Some(voice.canonicalize().unwrap()))
            }
            _ => panic!("expected workspace launch"),
        }
        unsafe {
            std::env::remove_var("GHOSTLESS_PROJECT");
        }
        assert!(matches!(
            parse_launch(vec!["--folder".into(), workspace.display().to_string()]).unwrap(),
            LaunchTarget::Workspace { .. }
        ));
        let mut store = prefs::EditorStateStore::default();
        store.record_workspace(&workspace);
        prefs::save_store(&store).unwrap();
        assert!(matches!(
            parse_launch(vec![]).unwrap(),
            LaunchTarget::Workspace { .. }
        ));
        assert!(parse_launch(vec!["--project".into(), home.display().to_string()]).is_err());
        assert!(
            parse_launch(vec![
                "--draft".into(),
                "a".into(),
                "--session".into(),
                "b".into()
            ])
            .is_err()
        );
        assert!(
            parse_launch(vec![
                "--project".into(),
                voice.display().to_string(),
                "--draft".into(),
                "missing".into()
            ])
            .is_err()
        );
        assert!(
            parse_launch(vec![
                "--project".into(),
                voice.display().to_string(),
                "--session".into(),
                "bad".into()
            ])
            .is_err()
        );
        std::env::set_current_dir(old_cwd).unwrap();
        unsafe {
            if let Some(v) = old_home {
                std::env::set_var("GHOSTLESS_HOME", v);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
            if let Some(v) = old_project {
                std::env::set_var("GHOSTLESS_PROJECT", v);
            } else {
                std::env::remove_var("GHOSTLESS_PROJECT");
            }
        }
        fs::remove_dir_all(base).unwrap();
    }
}
