use super::*;
use crate::test_util::{Seeded, TestDir};
use std::{env, ffi::OsString};

struct RestoreHome(Option<OsString>);
impl Drop for RestoreHome {
    fn drop(&mut self) {
        // The caller holds the same environment lock as other prefs tests.
        unsafe {
            if let Some(previous) = self.0.as_ref() {
                env::set_var("GHOSTLESS_HOME", previous);
            } else {
                env::remove_var("GHOSTLESS_HOME");
            }
        }
    }
}

#[test]
fn seeded_editor_lifecycle_preserves_edits_baselines_and_finished_state() {
    let _env_lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let area = TestDir::new("lifecycle-property");
    let _restore_home = RestoreHome(env::var_os("GHOSTLESS_HOME"));
    unsafe { env::set_var("GHOSTLESS_HOME", area.join("home")) };
    for seed in [17, 0x84c3, 0x90face] {
        exercise(&area, seed);
    }
}

fn exercise(area: &Path, seed: u64) {
    const SESSION_ID: &str = "2026-01-02-notes";
    const BASELINE: &str = "First draft — café.\n";
    let workspace = area.join(format!("work-{seed}"));
    let other_workspace = area.join(format!("other-{seed}"));
    let project = workspace.join("voice");
    let folder = project.join("sessions").join(SESSION_ID);
    let draft = workspace.join("draft.md");
    let receipt = folder.join("receipt.json");
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(&other_workspace).unwrap();
    for path in [&draft, &folder.join("before.md"), &folder.join("after.md")] {
        fs::write(path, BASELINE).unwrap();
    }
    fs::write(project.join("manifest.json"), r#"{"kind":"ghostless"}"#).unwrap();
    let baseline_hash = fs_io::sha256_text(BASELINE);
    fs::write(
        folder.join("session.json"),
        serde_json::json!({
            "kind": "ghostless-edit-session", "version": 1,
            "id": SESSION_ID, "sourceDraftName": "draft.md",
            "origin": "writer-first", "status": "open",
            "createdAt": "2026-01-02T00:00:00Z", "beforeSha256": baseline_hash
        })
        .to_string(),
    )
    .unwrap();

    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&project, SESSION_ID),
        prefs::SessionLink {
            workspace_file: "draft.md".into(),
            last_synced_sha256: baseline_hash,
        },
    );
    let receipt_path = receipt.clone();
    let runner: CliRunner = Box::new(move |args, _| {
        assert_eq!(args, ["bridge"]);
        fs::write(&receipt_path, "{}").unwrap();
        Ok(r#"{"id":1,"ok":true,"result":{}}"#.into())
    });
    let mut editor =
        Editor::empty_with_handlers(store, runner, Box::new(execute_write_op), noop_wake());
    editor.open_workspace(workspace.clone(), None);
    editor.open_session_entry(project.clone(), SESSION_ID.into());
    let mut tx = Tx::default();
    let mut rng = Seeded::new(seed);
    let mut expected = BASELINE.to_owned();
    let mut finished = false;
    for step in 0..36 {
        let action = rng.index(11);
        match action {
            0..=2 if !finished => {
                editor.open_session_entry(project.clone(), SESSION_ID.into());
                if editor
                    .active_doc_ref()
                    .is_some_and(DocumentBuffer::is_editable)
                {
                    expected = format!("Revision {step} — 複数.\n");
                    editor.active_working_edit_mut().replace_text(&expected);
                    editor.changed_active(&mut tx);
                }
            }
            3 => editor.save_clicked(),
            4 => editor.toggle_autosave(),
            5 => editor.toggle_active_user_lock(),
            6 => {
                if let Some(key) = editor.active_doc_ref().map(|doc| doc.id.tab_key()) {
                    editor.close_tab_by_id(&key, &mut tx);
                    editor.confirm_close_save();
                }
            }
            7 => editor.on_application_quit(),
            8 if !finished => {
                editor.open_session_entry(project.clone(), SESSION_ID.into());
                editor.finish();
                drain_until_idle(&mut editor);
                poll(&mut editor, 10.0, &mut tx);
                editor.confirm_finish();
                drain_until_idle(&mut editor);
                editor.dismiss_finish();
                finished = receipt.exists();
            }
            9 => {
                editor.open_workspace(other_workspace.clone(), None);
                editor.open_workspace(workspace.clone(), None);
            }
            _ => {
                poll(&mut editor, 10.0, &mut tx);
            }
        }
        drain_until_idle(&mut editor);
        let context = format!("seed {seed:#x}, step {step}, action {action}");
        assert_eq!(
            fs::read_to_string(folder.join("before.md")).unwrap(),
            BASELINE,
            "{context}"
        );
        assert_eq!(receipt.exists(), finished, "{context}");
        if let Some(doc) = editor.documents.iter().find(|doc| doc.is_session()) {
            assert_eq!(doc.edit.text, expected, "{context}");
            assert!(doc.saved_generation <= doc.generation, "{context}");
            if finished {
                assert!(
                    doc.finished && doc.read_only && !doc.is_editable(),
                    "{context}"
                );
            }
            if doc.is_dirty() {
                continue;
            }
        }
        assert_eq!(
            fs::read_to_string(folder.join("after.md")).unwrap(),
            expected,
            "{context}"
        );
        assert_eq!(fs::read_to_string(&draft).unwrap(), expected, "{context}");
    }
    drop(editor);
    assert_eq!(
        fs::read_to_string(folder.join("after.md")).unwrap(),
        expected,
        "seed {seed:#x}, drop"
    );
    assert_eq!(
        fs::read_to_string(draft).unwrap(),
        expected,
        "seed {seed:#x}, drop"
    );
}
