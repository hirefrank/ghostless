use super::*;
use crate::error::EditorError;
use crate::{
    documents::{CachedJudge, DocId, DocumentBuffer},
    fs_io, prefs,
    session::{Session, load_session},
    worker::{
        CliRunner, InFlight, SearchReply, WakeFn, WorkerReply, WriteOp, WriteOutcome, WriteRunner,
    },
};
use rune::{Key, KeyEvent, Tx};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::{thread, time::Duration};

fn test_session() -> Session {
    Session {
        project: PathBuf::from("/tmp/ghostless-test-voice"),
        id: "2026-01-02-notes".into(),
        source_name: "draft.md".into(),
        original: "Original text.".into(),
        after: "Original text.".into(),
        notes: String::new(),
        before_sha256: "test".into(),
        finished: false,
    }
}

#[test]
fn folder_search_replies_leave_saves_alone_and_ignore_stale_results() {
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.folder_search.open = true;
    editor.folder_search.request_id = Some(2);
    editor.worker_busy = Some((DocId::File("/tmp/a.md".into()), InFlight::Save));
    assert!(!editor.accept_folder_search(SearchReply {
        id: 1,
        matches: Vec::new()
    }));
    assert_eq!(editor.folder_search.request_id, Some(2));
    assert!(editor.accept_folder_search(SearchReply {
        id: 2,
        matches: Vec::new()
    }));
    assert_eq!(editor.folder_search.request_id, None);
    assert!(editor.worker_busy.take().is_some());
}

#[test]
fn find_keeps_the_active_occurrence_and_replace_advances_past_inserted_text() {
    let mut session = test_session();
    session.after = "Café / Café / Café".into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("café");
    editor.open_find(true, &mut tx);
    editor.step_find(true, &mut tx);
    let before = editor.find.current_range().cloned().unwrap();

    let edit = editor.active_working_edit_mut();
    edit.set_caret(0);
    edit.insert("Café — ");
    editor.changed_active(&mut tx);
    assert_eq!(editor.find.current, Some(2));
    assert_eq!(
        editor.find.current_range().unwrap().start,
        before.start + "Café — ".len()
    );
    assert_eq!(
        editor.active_working_edit().range(),
        ("Café — ".len(), "Café — ".len())
    );

    editor.find.replacement.replace_text("Café again");
    editor.replace_find(false, &mut tx);
    assert_eq!(editor.find.current, Some(3));
    assert_eq!(editor.active_working_edit().selected(), "Café");
    assert!(
        editor
            .active_working_edit()
            .text
            .ends_with("Café again / Café")
    );

    editor.find.replacement.replace_text("");
    editor.replace_find(false, &mut tx);
    assert_eq!(editor.find.current, Some(0));
}

#[test]
fn editing_inside_a_match_keeps_navigation_anchored_there() {
    let mut session = test_session();
    session.after = "Café / Café / Café".into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("café");
    editor.open_find(false, &mut tx);
    editor.step_find(true, &mut tx);
    let middle = editor.find.current_range().unwrap().start;

    let edit = editor.active_working_edit_mut();
    edit.set_selection(middle + 1, middle + 2);
    edit.insert("A");
    editor.changed_active(&mut tx);
    assert_eq!(editor.find.current, Some(1));

    let edit = editor.active_working_edit_mut();
    edit.set_caret(middle + 3);
    edit.insert("✨");
    editor.changed_active(&mut tx);
    assert_eq!(editor.find.matches.len(), 2);
    assert_eq!(editor.find.current, None);
    assert_eq!(editor.find.navigation_anchor(), Some(middle));
    let caret = middle + 3 + "✨".len();
    assert_eq!(editor.active_working_edit().range(), (caret, caret));

    editor.active_working_edit_mut().undo();
    editor.changed_active(&mut tx);
    assert_eq!(editor.find.current, Some(1));
    assert_eq!(editor.find.current_range().unwrap().start, middle);

    editor.active_working_edit_mut().redo();
    editor.changed_active(&mut tx);
    editor.step_find(true, &mut tx);
    assert_eq!(editor.find.current, Some(1));
    assert!(editor.active_working_edit().range().0 > middle);
}

#[test]
fn unsuccessful_replace_preserves_selection_and_document_generation() {
    let mut session = test_session();
    session.after = "Café / Café".into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("café");
    editor.open_find(true, &mut tx);
    let selection = editor.active_working_edit().range();
    let generation = editor.active_doc_ref().unwrap().generation;

    editor.find.replacement.replace_text("Café");
    editor.replace_find(false, &mut tx);
    editor.replace_find(true, &mut tx);
    assert_eq!(editor.find.current, Some(0));
    assert_eq!(editor.active_working_edit().range(), selection);
    assert_eq!(editor.active_doc_ref().unwrap().generation, generation);

    // A range that lands inside é is rejected without moving to the next hit.
    editor.find.matches[0] = 4..5;
    editor.find.replacement.replace_text("Tea");
    editor.replace_find(false, &mut tx);
    assert_eq!(editor.find.current, Some(0));
    assert_eq!(editor.active_working_edit().range(), selection);
    assert_eq!(editor.active_doc_ref().unwrap().generation, generation);
    assert_eq!(editor.active_working_edit().text, "Café / Café");
}

#[test]
fn replace_all_reconciles_selection_with_remaining_matches_and_caret() {
    let mut session = test_session();
    session.after = "Café / Café / Café.".into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("café");
    editor.open_find(true, &mut tx);
    editor.step_find(true, &mut tx);

    editor.find.replacement.replace_text("✨ CAFÉ");
    editor.replace_find(true, &mut tx);
    let replaced = "✨ CAFÉ / ✨ CAFÉ / ✨ CAFÉ.";
    assert_eq!(editor.active_working_edit().text, replaced);
    assert_eq!(editor.find.current, Some(2));
    let range = editor.find.current_range().unwrap();
    assert_eq!(
        editor.active_working_edit().range(),
        (range.start, range.end)
    );
    assert_eq!(editor.active_working_edit().selected(), "CAFÉ");

    editor.find.replacement.replace_text("tea");
    editor.replace_find(true, &mut tx);
    assert_eq!(
        editor.active_working_edit().text,
        "✨ tea / ✨ tea / ✨ tea."
    );
    assert!(editor.find.matches.is_empty());
    assert_eq!(editor.find.current, None);
    let caret = "✨ tea / ✨ tea / ✨ tea".len();
    assert_eq!(editor.find.navigation_anchor(), Some(caret));
    assert_eq!(editor.active_working_edit().range(), (caret, caret));

    editor.active_working_edit_mut().undo();
    editor.changed_active(&mut tx);
    assert_eq!(editor.active_working_edit().text, replaced);
}

#[test]
fn preview_blocks_replacement_and_replace_shortcut_opens_write() {
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("Original");
    editor.find.replacement.replace_text("Revised");
    editor.open_find(true, &mut tx);
    editor.set_view_mode(ViewMode::Preview, &mut tx);
    assert!(editor.find.open);
    assert!(!editor.find.show_replace);

    editor.replace_find(false, &mut tx);
    editor.replace_find(true, &mut tx);
    assert_eq!(editor.active_working_edit().text, "Original text.");
    assert_eq!(editor.active_doc_ref().unwrap().generation, 0);

    let shortcut = KeyEvent {
        key: Key::Command('h'),
        mods: rune::Mods::default(),
    };
    assert!(on_key(&mut editor, &shortcut, &mut tx));
    assert_eq!(editor.view_mode, ViewMode::Write);
    assert!(editor.find.show_replace);
    assert_eq!(editor.active_working_edit().text, "Original text.");

    editor.set_view_mode(ViewMode::Preview, &mut tx);
    editor.active_doc_mut().unwrap().read_only = true;
    assert!(on_key(&mut editor, &shortcut, &mut tx));
    assert_eq!(editor.view_mode, ViewMode::Preview);
    assert!(!editor.find.show_replace);
}

#[test]
fn preview_scrolls_to_orphaned_find_anchor() {
    let prose_padding = "A paragraph above or below the edit.\n\n".repeat(70);
    let yaml_padding = "# A metadata comment above the edit.\n".repeat(120);
    let fixtures = [
        (
            format!("First Café.\n\n{prose_padding}Middle Café.\n\n{prose_padding}Last Café."),
            true,
            2,
        ),
        (
            format!("---\n{yaml_padding}stage: Café\n---\n# Preview"),
            false,
            0,
        ),
    ];
    for (source, step_to_middle, remaining) in fixtures {
        let mut session = test_session();
        session.after = source;
        let mut editor = Editor::with_handlers(
            session,
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        let mut tx = Tx::default();
        editor.find.query.replace_text("café");
        editor.open_find(false, &mut tx);
        if step_to_middle {
            editor.step_find(true, &mut tx);
        }
        let start = editor.find.current_range().unwrap().start;
        editor.active_working_edit_mut().insert("tea");
        editor.changed_active(&mut tx);
        assert_eq!(editor.find.navigation_anchor(), Some(start));
        assert_eq!(editor.find.current, None);
        let caret = editor.active_working_edit().range();

        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        rt.update(|editor, tx| editor.set_view_mode(ViewMode::Preview, tx));
        rt.frame(0.0);

        let elements = rt.elements();
        let element = |key| {
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap()
        };
        let viewport = element("preview-body").rect;
        let mark = element("preview-find-anchor");
        assert_eq!(mark.text.as_deref(), Some("t"));
        assert_eq!(mark.path.contains("/preview-frontmatter/"), !step_to_middle);
        assert!(
            mark.rect[1] >= viewport[1] && mark.rect[1] + mark.rect[3] <= viewport[1] + viewport[3],
            "anchor {:?} is outside preview {viewport:?}",
            mark.rect,
        );
        assert_eq!(rt.state.find.current, None);
        assert_eq!(rt.state.find.matches.len(), remaining);
        assert_eq!(rt.state.active_working_edit().range(), caret);

        // The writer can move the caret independently of Find. Switching
        // back to Write should still reveal the saved occurrence's position.
        rt.update(|editor, tx| {
            editor.active_working_edit_mut().set_caret(0);
            editor.set_view_mode(ViewMode::Write, tx);
        });
        rt.frame(0.0);
        let elements = rt.elements();
        let element = |key| {
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap()
        };
        let viewport = element("working-body").rect;
        let mark = element(crate::search::SOURCE_ANCHOR_KEY);
        assert_eq!(mark.text.as_deref(), Some("t"));
        assert!(
            mark.rect[1] >= viewport[1] && mark.rect[1] + mark.rect[3] <= viewport[1] + viewport[3],
            "saved anchor {:?} is outside Write viewport {viewport:?}",
            mark.rect,
        );
        assert_eq!(rt.state.active_working_edit().range(), (0, 0));
    }
}

#[test]
fn preview_suppresses_hidden_comment_find_anchor_and_scrolls_to_visible_fallback() {
    let prose_padding = "A paragraph above or below the edit.\n\n".repeat(70);
    let mut session = test_session();
    session.after = format!(
        "First Café with more text here.\n\n{prose_padding}<!-- Café Café -->\n\n{prose_padding}Last Café."
    );
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("café");
    editor.open_find(false, &mut tx);
    editor.step_find(true, &mut tx);
    editor.step_find(true, &mut tx);
    assert_eq!(editor.find.current, Some(2));
    let start = editor.find.current_range().unwrap().start;
    editor.active_working_edit_mut().insert("tea");
    editor.changed_active(&mut tx);
    assert_eq!(editor.find.current, None);
    assert_eq!(editor.find.navigation_anchor(), Some(start));
    assert_eq!(editor.find.matches.len(), 3);

    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.frame(0.0);
    rt.update(|editor, tx| editor.set_view_mode(ViewMode::Preview, tx));
    rt.frame(0.0);

    let elements = rt.elements();
    assert!(
        elements
            .iter()
            .all(|e| e.key.as_deref() != Some("preview-find-anchor"))
    );
    let viewport = elements
        .iter()
        .find(|e| e.key.as_deref() == Some("preview-body"))
        .unwrap()
        .rect;
    let fallback = elements
        .iter()
        .find(|e| e.key.as_deref() == Some("md-mark-2"))
        .unwrap();
    assert_eq!(fallback.text.as_deref(), Some("Café"));
    assert!(
        fallback.rect[1] >= viewport[1]
            && fallback.rect[1] + fallback.rect[3] <= viewport[1] + viewport[3],
        "fallback {:?} is outside preview {viewport:?}",
        fallback.rect,
    );
}

#[test]
fn closing_find_focuses_the_document_surface_that_is_mounted() {
    for (mode, read_only, title, expected) in [
        (ViewMode::Write, false, "draft.md", "prose-input"),
        (ViewMode::Write, true, "draft.md", "working-body"),
        (ViewMode::Preview, false, "draft.md", "preview-body"),
        (ViewMode::Write, true, "voice-guide.md", "preview-body"),
    ] {
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        editor.documents.push(DocumentBuffer::from_workspace_file(
            PathBuf::from(title),
            title.into(),
            "Preview words.".into(),
            read_only,
            false,
            true,
            None,
        ));
        editor.active_doc = Some(0);
        editor.view_mode = mode;
        editor.reference.open = true;
        editor.reference.source = Some(crate::ui::reference::ReferenceSource::File(
            "reference.md".into(),
        ));
        editor.reference.cached_content = "Reference words.".into();

        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        rt.update(|editor, tx| editor.open_find(false, tx));
        rt.frame(0.0);
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.focused && e.label.as_deref() == Some("Find"))
        );
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Default::default(),
        });
        rt.frame(0.0);

        assert!(!rt.state.find.open);
        let elements = rt.elements();
        let focused = elements
            .iter()
            .find(|element| element.focused)
            .and_then(|element| element.key.as_deref());
        assert_eq!(
            focused,
            Some(expected),
            "{mode:?}, {title}, read-only={read_only}"
        );
    }
}

#[test]
fn search_dialogs_restore_document_focus_on_all_dismissal_paths() {
    for (mode, read_only, title, expected) in [
        (ViewMode::Preview, false, "draft.md", "preview-body"),
        (ViewMode::Write, true, "voice-guide.md", "preview-body"),
        (ViewMode::Compare, false, "draft.md", "compare-body"),
    ] {
        for folder in [false, true] {
            for dismissal in ["Escape", "Close", "Outside"] {
                let mut editor = Editor::with_handlers(
                    test_session(),
                    prefs::EditorStateStore::default(),
                    ok_runner(),
                    ok_writer(),
                    noop_wake(),
                );
                let doc = editor.active_doc_mut().unwrap();
                doc.title = title.into();
                doc.path.set_file_name(title);
                doc.read_only = read_only;
                editor.view_mode = mode;
                editor.reference.open = true;
                editor.reference.source =
                    Some(crate::ui::reference::ReferenceSource::SessionOriginal);

                let mut rt = rune::Runtime::new(
                    editor,
                    crate::ui::view,
                    (1120.0, 800.0),
                    rune::Env::new(false, 1.0),
                );
                rt.on_key = Some(on_key);
                rt.frame(0.0);
                assert!(!rt.elements().iter().any(|e| e.focused));
                rt.update(|editor, tx| {
                    if folder {
                        editor.workspace = Some(PathBuf::from("/tmp"));
                        editor.open_folder_search(tx);
                        editor.workspace = None;
                    } else {
                        editor.open_quick(tx);
                    }
                });
                rt.frame(0.0);
                let label = if folder {
                    "Search in workspace"
                } else {
                    "Go to file or session"
                };
                assert!(
                    rt.elements()
                        .iter()
                        .any(|e| e.focused && e.label.as_deref() == Some(label))
                );
                match dismissal {
                    "Escape" => rt.key(KeyEvent {
                        key: Key::Escape,
                        mods: Default::default(),
                    }),
                    "Close" => {
                        let [x, y, w, h] = rt
                            .elements()
                            .iter()
                            .find(|e| e.key.as_deref() == Some("close"))
                            .unwrap()
                            .rect;
                        rt.pointer_down(x + w / 2.0, y + h / 2.0);
                        rt.pointer_up(x + w / 2.0, y + h / 2.0);
                    }
                    _ => {
                        rt.pointer_down(10.0, 80.0);
                        rt.pointer_up(10.0, 80.0);
                    }
                }
                rt.frame(0.0);

                assert!(!rt.state.quick.open && !rt.state.folder_search.open);
                let elements = rt.elements();
                let focused = elements
                    .iter()
                    .find(|e| e.focused)
                    .and_then(|e| e.key.as_deref());
                assert_eq!(
                    focused,
                    Some(expected),
                    "{mode:?}, {title}, folder={folder}, {dismissal}"
                );
            }
        }
    }
}

#[test]
fn accepting_search_results_focuses_the_rendered_document() {
    use crate::quick_open::{QuickItem, QuickTarget, search_workspace_files};

    for folder in [false, true] {
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        let name = if folder { "voice-guide.md" } else { "draft.md" };
        let path = PathBuf::from(name);
        let source = "A Café note.";
        editor.documents.push(DocumentBuffer::from_workspace_file(
            path.clone(),
            name.into(),
            source.into(),
            folder,
            false,
            true,
            None,
        ));
        editor.active_doc = Some(0);
        editor.view_mode = ViewMode::Preview;

        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        rt.update(|editor, tx| {
            if folder {
                editor.workspace = Some(PathBuf::from("/tmp"));
                editor.open_folder_search(tx);
                editor.workspace = None;
                editor.folder_search.query.replace_text("café");
                editor.folder_search.matches = search_workspace_files(
                    &[WorkspaceEntry {
                        abs_path: path.clone(),
                        rel_path: name.into(),
                        kind: workspace_tree::FileKind::Writing,
                        read_only: true,
                    }],
                    &[(path, Some(source.into()))].into_iter().collect(),
                    "café",
                    10,
                    || false,
                );
            } else {
                editor.open_quick(tx);
                editor.quick.items.push(QuickItem {
                    id: name.into(),
                    title: name.into(),
                    subtitle: String::new(),
                    target: QuickTarget::File(path),
                });
            }
        });
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Default::default(),
        });
        rt.frame(0.0);

        assert!(!rt.state.quick.open && !rt.state.folder_search.open);
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.focused && e.key.as_deref() == Some("preview-body"))
        );
        assert_eq!(rt.state.active_working_edit().text, source);
        if folder {
            assert_eq!(rt.state.active_working_edit().selected(), "Café");
        }
    }
}

#[test]
fn compare_closes_find_and_find_shortcut_returns_to_write() {
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.find.query.replace_text("Original");
    editor.open_find(false, &mut tx);
    editor.set_view_mode(ViewMode::Compare, &mut tx);
    assert!(!editor.find.open);
    let key = KeyEvent {
        key: Key::Command('h'),
        mods: rune::Mods::default(),
    };
    assert!(on_key(&mut editor, &key, &mut tx));
    assert_eq!(editor.view_mode, ViewMode::Write);
    assert!(editor.find.open && editor.find.show_replace);
    assert_eq!(editor.active_working_edit().selected(), "Original");
}

#[test]
fn folder_hit_uses_open_buffer_and_relocates_after_an_edit() {
    use crate::{quick_open::search_workspace_files, workspace_tree::FileKind};
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let path = std::env::temp_dir().join(format!("ghostless-folder-hit-{}.md", std::process::id()));
    let entry = WorkspaceEntry {
        abs_path: path.clone(),
        rel_path: "draft.md".into(),
        kind: FileKind::Writing,
        read_only: false,
    };
    let source = "Opening\nSecond CAFÉtéria\nSeparator\nSecond CAFÉtéria";
    let buffers = [(path.clone(), Some(source.into()))].into_iter().collect();
    editor.documents.push(DocumentBuffer::from_workspace_file(
        path,
        "draft.md".into(),
        format!("New ✨ line\n{source}"),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    editor.folder_search.open = true;
    editor.folder_search.query.replace_text("café");
    editor.folder_search.matches =
        search_workspace_files(&[entry], &buffers, "café", 200, || false);
    editor.find.case_sensitive = true;
    editor.find.whole_word = true;
    editor.view_mode = ViewMode::Preview;

    editor.choose_folder_match(0, &mut Tx::default());

    assert!(!editor.folder_search.open);
    assert!(!editor.find.case_sensitive && !editor.find.whole_word);
    assert_eq!(editor.find.current, Some(0));
    assert_eq!(editor.active_working_edit().selected(), "CAFÉ");
    assert_eq!(
        editor.active_working_edit().range().0,
        "New ✨ line\nOpening\nSecond ".len()
    );
    assert_eq!(editor.view_mode, ViewMode::Write);
    assert_eq!(
        editor.find.notice.as_deref(),
        Some("File changed. Showing the closest match."),
    );
}

fn noop_wake() -> WakeFn {
    Box::new(|| {})
}

fn ok_writer() -> WriteRunner {
    Box::new(|_| Ok(WriteOutcome::Local))
}

fn ok_runner() -> CliRunner {
    Box::new(|args, _| {
        Ok(if args == ["bridge"] {
            r#"{"id":1,"ok":true,"result":{}}"#
        } else {
            "{}"
        }
        .into())
    })
}

fn drain_until_idle(editor: &mut Editor) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while editor.worker_busy.is_some() {
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
    let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = writes.clone();

    let writer: WriteRunner = Box::new(move |op| {
        recorded.lock().unwrap().push(op.clone());
        let _ = entered_tx.send(());
        let _ = release_rx.lock().unwrap().recv();
        Ok(WriteOutcome::Local)
    });

    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "First draft edit.".into();
    editor.changed_active(&mut tx);
    editor.save_clicked();

    assert_eq!(
        editor.worker_busy.as_ref().map(|(_, k)| *k),
        Some(InFlight::Save)
    );
    assert_eq!(editor.status, "Saving…");
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker started running write");

    editor.active_working_edit_mut().text = "Second draft edit.".into();
    editor.changed_active(&mut tx);
    assert!(editor.documents[0].pending_save);

    release_tx.send(()).unwrap();
    release_tx.send(()).unwrap();
    drain_until_idle(&mut editor);
    assert_eq!(editor.status, "Saved locally");
    assert!(!editor.documents[0].is_dirty());
    assert_eq!(writes.lock().unwrap().len(), 2);
}

#[test]
fn rapid_edits_during_in_flight_save_coalesce_into_one_follow_up() {
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = writes.clone();

    let writer: WriteRunner = Box::new(move |op| {
        let count = {
            let mut guard = recorded.lock().unwrap();
            guard.push(op.clone());
            guard.len()
        };
        if count == 1 {
            let _ = started_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
        }
        Ok(WriteOutcome::Local)
    });

    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Edit 0".into();
    editor.changed_active(&mut tx);
    assert!(poll(&mut editor, 1.0, &mut tx));
    assert_eq!(
        editor.worker_busy.as_ref().map(|(_, k)| *k),
        Some(InFlight::Save)
    );
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first save entered worker");

    for idx in 1..=5 {
        editor.active_working_edit_mut().text = format!("Edit {idx}");
        editor.active_notes_edit_mut().text = format!("Note {idx}");
        editor.changed_active(&mut tx);
    }

    release_tx.send(()).unwrap();
    drain_until_idle(&mut editor);

    let guard = writes.lock().unwrap();
    assert_eq!(
        guard.len(),
        2,
        "5 rapid edits must coalesce into one follow-up save"
    );
    match &guard[1] {
        WriteOp::Session { after, notes, .. } => {
            assert_eq!(after, "Edit 5");
            assert_eq!(notes, "Note 5");
        }
        other => panic!("expected WriteOp::Session, got {other:?}"),
    }
    assert_eq!(editor.status, "Saved locally");
    assert!(!editor.documents[0].is_dirty());
}

#[test]
fn recovery_copy_can_be_reviewed_without_overwriting_disk() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-recovery-{}", std::process::id()));
    let home = base.join("home");
    let workspace = base.join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let old_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let file = workspace.join("draft.md");
    fs::write(&file, "disk text").unwrap();
    let file = file.canonicalize().unwrap();
    let recovery = crate::documents::recovery_path_for(&file);
    fs_io::atomic_write_text(&recovery, "recovered text").unwrap();
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.workspace = Some(workspace);
    editor.open_workspace_file(file.clone(), true);
    assert!(matches!(
        editor.chrome.active_dialog,
        Some(DialogKind::Recover { .. })
    ));
    assert_eq!(editor.active_working_edit().text, "disk text");
    editor.close_dialog();
    assert!(recovery.exists());
    editor.open_workspace_file(file.clone(), true);
    assert!(matches!(
        editor.chrome.active_dialog,
        Some(DialogKind::Recover { .. })
    ));
    editor.resolve_recovery(true);
    assert_eq!(editor.active_working_edit().text, "recovered text");
    assert!(editor.active_doc_ref().unwrap().is_dirty());
    assert_eq!(fs::read_to_string(&file).unwrap(), "disk text");
    assert!(recovery.exists());
    editor.documents.clear();
    editor.workspace = None;
    unsafe {
        if let Some(v) = old_home {
            std::env::set_var("GHOSTLESS_HOME", v);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dirty_locked_file_saves_on_tab_or_window_close() {
    for action in ["tab", "manual tab", "window"] {
        let mut store = prefs::EditorStateStore::default();
        store.ui.autosave_workspace_files = action != "manual tab";
        let saved = Arc::new(Mutex::new(Vec::new()));
        let recorded = saved.clone();
        let writer: WriteRunner = Box::new(move |op| {
            if let WriteOp::File { content, .. } = op {
                recorded.lock().unwrap().push(content.clone());
            }
            Ok(WriteOutcome::Local)
        });
        let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
        editor.documents.push(DocumentBuffer::from_workspace_file(
            "/tmp/pre-answers.md".into(),
            "pre-answers.md".into(),
            "old".into(),
            false,
            true,
            true,
            None,
        ));
        editor.active_doc = Some(0);
        let mut tx = Tx::default();
        editor.active_working_edit_mut().text = "latest edit".into();
        editor.changed_active(&mut tx);
        editor.toggle_active_user_lock();
        assert!(editor.active_doc_ref().unwrap().user_locked);
        match action {
            "tab" => assert!(editor.close_active_tab(&mut tx)),
            "manual tab" => {
                assert!(!editor.close_active_tab(&mut tx));
                assert!(matches!(
                    editor.chrome.active_dialog,
                    Some(DialogKind::ConfirmClose { .. })
                ));
                editor.confirm_close_save();
                drain_until_idle(&mut editor);
                assert!(editor.documents.is_empty());
            }
            _ => assert!(editor.flush_on_exit()),
        }
        assert_eq!(*saved.lock().unwrap(), ["latest edit"]);
    }
}

fn block_first_write(writer: WriteRunner) -> (WriteRunner, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (started, started_rx) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let release = Mutex::new(Some(release));
    let wrapped: WriteRunner = Box::new(move |op| {
        if let Some(release) = release.lock().unwrap().take() {
            started.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        writer(op)
    });
    (wrapped, started_rx, release_tx)
}

#[test]
fn manual_save_does_not_write_edits_made_during_the_save() {
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let saved = Arc::new(Mutex::new(Vec::new()));
    let recorded = saved.clone();
    let (writer, started, release) = block_first_write(Box::new(move |op| {
        if let WriteOp::File { content, .. } = op {
            recorded.lock().unwrap().push(content.clone());
        }
        Ok(WriteOutcome::Local)
    }));
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
    editor.documents.push(DocumentBuffer::from_workspace_file(
        "/tmp/manual.md".into(),
        "manual.md".into(),
        "old".into(),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "explicit save".into();
    editor.changed_active(&mut tx);
    editor.save_clicked();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    editor.active_working_edit_mut().text = "subsequent edit".into();
    editor.changed_active(&mut tx);
    release.send(()).unwrap();
    drain_until_idle(&mut editor);
    assert_eq!(*saved.lock().unwrap(), ["explicit save"]);
    assert!(editor.active_doc_ref().unwrap().is_dirty());
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert_eq!(*saved.lock().unwrap(), ["explicit save", "subsequent edit"]);
}

#[test]
fn rename_waits_for_save_and_keeps_latest_contents() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("ghostless-rename-busy-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", root.join("home"));
    }
    let original = root.join("draft.md");
    fs::write(&original, "old").unwrap();
    let (writer, started, release) = block_first_write(Box::new(execute_write_op));
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    editor.open_workspace_file(original.clone(), true);
    editor.workspace = Some(root.clone());
    editor.active_working_edit_mut().text = "latest".into();
    let mut tx = Tx::default();
    editor.changed_active(&mut tx);
    editor.save_clicked();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    editor.chrome.active_dialog = Some(DialogKind::Rename {
        rel_path: "draft.md".into(),
    });
    editor.chrome.dialog_input.text = "renamed.md".into();
    editor.submit_dialog(&mut tx);
    assert!(editor.chrome.dialog_error.is_some());
    assert!(original.is_file());

    release.send(()).unwrap();
    drain_until_idle(&mut editor);
    editor.submit_dialog(&mut tx);
    assert!(!original.exists());
    assert_eq!(
        fs::read_to_string(root.join("renamed.md")).unwrap(),
        "latest"
    );
    assert!(editor.flush_on_exit());
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discard_waits_for_inflight_recovery_before_removing_copy() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("ghostless-discard-busy-{}", std::process::id()));
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", root.join("home"));
    }
    fs::create_dir_all(&root).unwrap();
    let path = root.join("draft.md");
    fs::write(&path, "disk version").unwrap();
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let (writer, started, release) = block_first_write(Box::new(execute_write_op));
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
    editor.open_workspace_file(path.clone(), true);
    let recovery = editor.active_doc_ref().unwrap().recovery_path();
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "discarded text".into();
    editor.changed_active(&mut tx);
    editor.flush_autosave_buffers();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!editor.close_active_tab(&mut tx));
    release.send(()).unwrap();
    editor.confirm_close_discard(&mut tx);
    assert!(editor.worker_busy.is_none());
    assert!(editor.documents.is_empty());
    assert!(!recovery.exists());
    assert_eq!(fs::read_to_string(path).unwrap(), "disk version");
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn application_quit_preserves_failed_session_save_for_reopen() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("ghostless-quit-recovery-{}", std::process::id()));
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", root.join("home"));
    }
    let mut session = test_session();
    session.project = root.join("voice");
    session.before_sha256 = fs_io::sha256_text(&session.original);
    let folder = session.project.join("sessions").join(&session.id);
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("before.md"), &session.original).unwrap();
    fs::write(folder.join("after.md"), &session.after).unwrap();
    fs::write(
        folder.join("session.json"),
        serde_json::json!({
            "kind": "ghostless-edit-session",
            "beforeSha256": session.before_sha256,
        })
        .to_string(),
    )
    .unwrap();
    let mut editor = Editor::with_handlers(
        session.clone(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        Box::new(|_| Err("session directory is read-only".into())),
        noop_wake(),
    );
    editor.active_working_edit_mut().text = "unsaved body".into();
    editor.active_notes_edit_mut().text = "unsaved notes".into();
    let mut tx = Tx::default();
    editor.changed_active(&mut tx);
    let quit = KeyEvent {
        key: Key::Command('q'),
        mods: Default::default(),
    };
    assert!(on_key(&mut editor, &quit, &mut tx));
    assert!(!editor.flushed);
    assert!(editor.active_doc_ref().unwrap().is_dirty());
    assert!(editor.status.contains("Could not close"));
    editor.on_application_quit();
    assert!(!editor.flushed);
    let copy = editor.active_doc_ref().unwrap().recovery_path();
    assert!(copy.is_file());
    drop(editor);

    let mut reopened = Editor::new(LaunchTarget::Session(session), noop_wake());
    assert!(matches!(
        reopened.chrome.active_dialog,
        Some(DialogKind::Recover { .. })
    ));
    reopened.resolve_recovery(true);
    assert_eq!(reopened.active_working_edit().text, "unsaved body");
    assert_eq!(reopened.active_notes_edit().text, "unsaved notes");
    assert!(reopened.active_doc_ref().unwrap().is_dirty());
    assert!(copy.is_file());
    reopened.save_clicked();
    drain_until_idle(&mut reopened);
    assert!(!copy.exists());
    assert_eq!(
        fs::read_to_string(folder.join("after.md")).unwrap(),
        "unsaved body"
    );
    assert_eq!(
        fs::read_to_string(folder.join("notes.md")).unwrap(),
        "unsaved notes"
    );
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn drop_retries_when_quit_could_not_save_or_recover() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("ghostless-quit-retry-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let blocked_home = root.join("recovery-unavailable");
    fs::write(&blocked_home, "").unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &blocked_home);
    }
    let file = root.join("draft.md");
    fs::write(&file, "disk version").unwrap();
    let save_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready = save_ready.clone();
    let writer: WriteRunner = Box::new(move |op| {
        if ready.load(std::sync::atomic::Ordering::SeqCst) {
            execute_write_op(op)
        } else {
            Err("Save temporarily unavailable".into())
        }
    });
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    editor.open_workspace_file(file.clone(), true);
    editor.active_working_edit_mut().text = "latest edit".into();
    editor.changed_active(&mut Tx::default());
    let recovery = editor.active_doc_ref().unwrap().recovery_path();

    editor.on_application_quit();
    assert!(!editor.flushed);
    assert!(!recovery.is_file());
    assert!(editor.active_doc_ref().unwrap().is_dirty());
    assert_eq!(fs::read_to_string(&file).unwrap(), "disk version");

    fs::remove_file(blocked_home).unwrap();
    save_ready.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(editor);
    assert_eq!(fs::read_to_string(file).unwrap(), "latest edit");
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleting_file_or_folder_with_dirty_tab_is_rejected() {
    let root = std::env::temp_dir().join(format!("ghostless-delete-dirty-{}", std::process::id()));
    fs::create_dir_all(root.join("drafts")).unwrap();
    let file = root.join("drafts/draft.md");
    fs::write(&file, "disk version").unwrap();
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.open_workspace_file(file.clone(), true);
    editor.workspace = Some(root.clone());
    editor.active_working_edit_mut().text = "unsaved work".into();
    editor.changed_active(&mut Tx::default());
    for (rel, is_dir) in [("drafts/draft.md", false), ("drafts", true)] {
        editor.chrome.active_dialog = Some(DialogKind::Delete {
            rel_path: rel.into(),
            display_name: rel.into(),
            is_dir,
        });
        editor.submit_dialog(&mut Tx::default());
        assert!(file.exists());
        assert_eq!(editor.active_working_edit().text, "unsaved work");
        assert!(
            editor
                .chrome
                .dialog_error
                .as_deref()
                .unwrap()
                .contains("unsaved")
        );
    }
    editor.documents.clear();
    editor.workspace = None;
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn blank_link_hash_saves_session_locally_without_touching_draft() {
    let base = std::env::temp_dir().join(format!("ghostless-unknown-link-{}", std::process::id()));
    let project = base.join("voice");
    let session_id = "2026-04-02-notes";
    let folder = project.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let before = "Original text.\n";
    fs::write(folder.join("before.md"), before).unwrap();
    fs::write(folder.join("after.md"), before).unwrap();
    fs::write(folder.join("session.json"), serde_json::json!({"kind":"ghostless-edit-session", "beforeSha256":fs_io::sha256_text(before)}).to_string()).unwrap();
    let draft = base.join("draft.md");
    fs::write(&draft, "External draft\n").unwrap();
    let mut store = prefs::EditorStateStore::default();
    let key = prefs::EditorStateStore::session_link_key(&project, session_id);
    store.session_links.insert(
        key.clone(),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: String::new(),
        },
    );
    for (sidebar, relative_link) in [(false, false), (true, false), (true, true)] {
        let mut current_store = store.clone();
        if relative_link {
            current_store
                .session_links
                .get_mut(&key)
                .unwrap()
                .workspace_file = "draft.md".into();
        }
        let mut editor = if sidebar {
            let mut app = Editor::empty_with_handlers(
                current_store,
                ok_runner(),
                Box::new(execute_write_op),
                noop_wake(),
            );
            if relative_link {
                app.workspace = Some(base.clone());
            }
            app.open_session_entry(project.clone(), session_id.into());
            app
        } else {
            let session = load_session(project.clone(), session_id.into()).unwrap();
            Editor::with_handlers(
                session,
                current_store,
                ok_runner(),
                Box::new(execute_write_op),
                noop_wake(),
            )
        };
        assert!(editor.status.contains("no known sync state"));
        assert!(editor.active_doc_ref().unwrap().mirror_path.is_none());
        editor.active_working_edit_mut().text = "Locally saved session\n".into();
        editor.active_notes_edit_mut().text = "Notes saved".into();
        editor.changed_active(&mut Tx::default());
        editor.save_clicked();
        drain_until_idle(&mut editor);
        assert!(!editor.active_doc_ref().unwrap().is_dirty());
        assert!(editor.status.contains("no known sync state"));
        assert_eq!(
            fs::read_to_string(folder.join("after.md")).unwrap(),
            "Locally saved session\n"
        );
        assert_eq!(
            fs::read_to_string(folder.join("notes.md")).unwrap(),
            "Notes saved"
        );
        assert_eq!(fs::read_to_string(&draft).unwrap(), "External draft\n");
        assert!(
            editor.store.session_links[&key]
                .last_synced_sha256
                .is_empty()
        );
        editor.workspace = None;
        editor.flush_on_exit();
    }
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn sidebar_session_with_relative_link_saves_locally_without_workspace() {
    let base =
        std::env::temp_dir().join(format!("ghostless-sidebar-relative-{}", std::process::id()));
    let project = base.join("voice");
    let session_id = "2026-04-02-notes";
    let folder = project.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let before = "Original text.\n";
    fs::write(folder.join("before.md"), before).unwrap();
    fs::write(folder.join("after.md"), before).unwrap();
    fs::write(
        folder.join("session.json"),
        serde_json::json!({
            "kind": "ghostless-edit-session", "beforeSha256": fs_io::sha256_text(before)
        })
        .to_string(),
    )
    .unwrap();
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&project, session_id),
        prefs::SessionLink {
            workspace_file: "draft.md".into(),
            last_synced_sha256: fs_io::sha256_text(before),
        },
    );
    let mut editor =
        Editor::empty_with_handlers(store, ok_runner(), Box::new(execute_write_op), noop_wake());
    editor.open_session_entry(project, session_id.into());
    assert!(editor.status.contains("Linked file unavailable"));
    assert!(editor.active_doc_ref().unwrap().mirror_path.is_none());
    editor.active_working_edit_mut().text = "Saved locally.\n".into();
    editor.active_notes_edit_mut().text = "My notes".into();
    editor.changed_active(&mut Tx::default());
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert!(!editor.active_doc_ref().unwrap().is_dirty());
    assert!(editor.status.contains("unavailable without a workspace"));
    assert_eq!(
        fs::read_to_string(folder.join("after.md")).unwrap(),
        "Saved locally.\n"
    );
    assert_eq!(
        fs::read_to_string(folder.join("notes.md")).unwrap(),
        "My notes"
    );
    editor.flush_on_exit();
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn standalone_session_saves_when_link_has_no_workspace() {
    let session = test_session();
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&session.project, &session.id),
        prefs::SessionLink {
            workspace_file: "draft.md".into(),
            last_synced_sha256: "known".into(),
        },
    );
    let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = writes.clone();
    let writer: WriteRunner = Box::new(move |op| {
        captured.lock().unwrap().push(op.clone());
        Ok(WriteOutcome::Local)
    });
    let mut editor = Editor::with_handlers(session, store, ok_runner(), writer, noop_wake());
    assert!(
        editor.status.contains("unavailable without a workspace")
            || editor.status.contains("Linked file unavailable")
    );
    editor.active_working_edit_mut().text = "session edit".into();
    editor.changed_active(&mut Tx::default());
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert!(matches!(
        &writes.lock().unwrap()[0],
        WriteOp::Session {
            mirror_path: None,
            ..
        }
    ));
    assert!(!editor.documents[0].is_dirty());
    assert!(editor.status.contains("unavailable without a workspace"));
}

#[test]
fn workspace_switch_preserves_dirty_documents_when_save_fails() {
    let writer: WriteRunner = Box::new(|_| Err("disk full".into()));
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "unsaved work".into();
    editor.changed_active(&mut tx);
    editor.open_workspace(PathBuf::from("/tmp/other-workspace"), None);
    assert!(editor.workspace.is_none());
    assert_eq!(editor.active_working_edit().text, "unsaved work");
    assert!(editor.documents[0].is_dirty());
    assert!(editor.status.contains("disk full"));
}

#[test]
fn workspace_switch_preserves_manual_buffer_when_recovery_fails() {
    let writer: WriteRunner = Box::new(|_| Err("recovery disk full".into()));
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
    editor.workspace = Some(PathBuf::from("/tmp/old-workspace"));
    editor.documents.push(DocumentBuffer::from_workspace_file(
        PathBuf::from("/tmp/draft.md"),
        "draft.md".into(),
        "before".into(),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    editor.active_working_edit_mut().text = "unsaved manual text".into();
    editor.changed_active(&mut Tx::default());
    editor.open_workspace(PathBuf::from("/tmp/new-workspace"), None);
    assert_eq!(
        editor.workspace.as_deref(),
        Some(Path::new("/tmp/old-workspace"))
    );
    assert_eq!(editor.active_working_edit().text, "unsaved manual text");
    assert!(editor.status.contains("recovery disk full"));
}

#[test]
fn workspace_switch_retains_dirty_buffer_when_worker_stops() {
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.active_working_edit_mut().text = "still here".into();
    editor.changed_active(&mut Tx::default());
    editor.worker.shutdown();
    editor.open_workspace(PathBuf::from("/tmp/other-workspace"), None);
    assert!(editor.workspace.is_none());
    assert_eq!(editor.active_working_edit().text, "still here");
    assert!(editor.status.contains("Workspace not changed"));
}

#[test]
fn voice_data_files_are_read_only_even_outside_tree() {
    let voice = Path::new("/tmp/voice");
    for relative in [
        "corpus/gold/example.md",
        "gold/example.md",
        "holdouts/example.md",
        "transcripts/example.txt",
        "sessions/2026-11-23/after.md",
        "sessions/2026-11-23/notes.md",
    ] {
        let path = voice.join(relative);
        assert_eq!(classify_file_flags(None, &path, Some(voice)), (true, false));
    }
    assert!(!classify_file_flags(None, &voice.join("drafts/after.md"), Some(voice)).0);
}

#[test]
fn mirror_paths_require_and_stay_within_workspace() {
    let root = std::env::temp_dir();
    assert_eq!(
        resolve_mirror_path(Path::new("draft.md"), Some(&root)),
        root.join("draft.md")
    );
    assert!(!resolve_mirror_path(Path::new("../outside.md"), Some(&root)).is_absolute());
    assert!(!resolve_mirror_path(Path::new("draft.md"), None).is_absolute());
}

#[test]
fn window_close_reopens_when_recovery_write_fails() {
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let mut editor = Editor::empty_with_handlers(
        store,
        ok_runner(),
        Box::new(|_| Err("recovery unavailable".into())),
        noop_wake(),
    );
    editor.documents.push(DocumentBuffer::from_workspace_file(
        PathBuf::from("/tmp/recovery-draft.md"),
        "draft.md".into(),
        "before".into(),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    let reopened = Arc::new(Mutex::new(false));
    let signal = reopened.clone();
    editor.reopen_window = Some(Box::new(move || *signal.lock().unwrap() = true));
    editor.active_working_edit_mut().text = "unsaved".into();
    editor.changed_active(&mut Tx::default());
    on_window_close(&mut editor, &mut Tx::default());
    assert!(*reopened.lock().unwrap());
    assert!(editor.documents[0].is_dirty());
    assert!(!editor.flushed);
    assert!(editor.status.contains("recovery unavailable"));
    editor.documents.clear();
}

#[test]
fn window_close_reopens_on_save_failure_and_retries() {
    let calls = Arc::new(Mutex::new(0));
    let attempts = calls.clone();
    let writer: WriteRunner = Box::new(move |_| {
        let mut count = attempts.lock().unwrap();
        *count += 1;
        if *count == 1 {
            Err("disk full".into())
        } else {
            Ok(WriteOutcome::Local)
        }
    });
    let reopened = Arc::new(Mutex::new(0));
    let reopen_count = reopened.clone();
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    editor.reopen_window = Some(Box::new(move || *reopen_count.lock().unwrap() += 1));
    editor.active_working_edit_mut().text = "Keep this edit".into();
    editor.changed_active(&mut Tx::default());
    on_window_close(&mut editor, &mut Tx::default());
    assert_eq!(*reopened.lock().unwrap(), 1);
    assert!(!editor.flushed);
    assert!(editor.documents[0].is_dirty());
    assert!(editor.status.contains("disk full"));
    on_window_close(&mut editor, &mut Tx::default());
    assert_eq!(*calls.lock().unwrap(), 2);
    assert!(editor.flushed);
    assert!(!editor.documents[0].is_dirty());
}

#[test]
fn flush_on_exit_saves_dirty_edits_before_autosave_timer_and_is_idempotent() {
    let writes: Arc<Mutex<Vec<WriteOp>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = writes.clone();
    let writer: WriteRunner = Box::new(move |op| {
        recorded.lock().unwrap().push(op.clone());
        Ok(WriteOutcome::Local)
    });

    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Typed right before closing.".into();
    editor.active_notes_edit_mut().text = "Keep this note.".into();
    editor.changed_active(&mut tx);
    assert!(editor.documents[0].is_dirty());
    assert!(editor.worker_busy.is_none());

    editor.flush_on_exit();
    editor.flush_on_exit();

    let guard = writes.lock().unwrap();
    assert_eq!(guard.len(), 1);
    match &guard[0] {
        WriteOp::Session { after, notes, .. } => {
            assert_eq!(after, "Typed right before closing.");
            assert_eq!(notes, "Keep this note.");
        }
        other => panic!("expected WriteOp::Session, got {other:?}"),
    }
    assert!(!editor.documents[0].is_dirty());
}

#[test]
fn finish_waits_for_in_flight_save_then_finishes_once() {
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let write_events = events.clone();
    let cli_events = events.clone();

    let writer: WriteRunner = Box::new(move |_| {
        let count = {
            let mut g = write_events.lock().unwrap();
            g.push("write_session");
            g.len()
        };
        if count == 1 {
            let _ = started_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
        }
        Ok(WriteOutcome::Local)
    });
    let runner: CliRunner = Box::new(move |args, _| {
        if args[0] == "bridge" {
            cli_events.lock().unwrap().push("finish");
        }
        Ok(r#"{"id":1,"ok":true,"result":{}}"#.into())
    });

    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        runner,
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Final prose.".into();
    editor.changed_active(&mut tx);
    editor.save_clicked();
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("snapshot started");

    editor.queue_finish();
    editor.queue_finish();
    assert!(editor.active_is_locked());

    release_tx.send(()).unwrap();
    editor.flush_on_exit();

    assert_eq!(*events.lock().unwrap(), vec!["write_session", "finish"]);
    assert!(editor.documents[0].finished);
    assert_eq!(editor.status, "Finished. Final and receipt saved locally.");
}

#[test]
fn failed_save_keeps_dirty_without_spinning_and_flush_does_not_loop() {
    let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    let writer: WriteRunner = Box::new(move |_| {
        *counter.lock().unwrap() += 1;
        Err("disk full".into())
    });

    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Will fail once.".into();
    editor.changed_active(&mut tx);
    editor.save_clicked();
    drain_until_idle(&mut editor);

    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(editor.documents[0].is_dirty());
    assert!(editor.status.contains("Save failed: disk full"));
    assert!(!poll(&mut editor, 10.0, &mut tx));
    assert_eq!(*calls.lock().unwrap(), 1);

    assert!(!editor.flush_on_exit());
    assert!(!editor.flush_on_exit());
    assert_eq!(*calls.lock().unwrap(), 3);
}

#[test]
fn per_tab_undo_redo_survives_switching_tabs() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-undo-tabs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let ws = base.join("ws");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(ws.join("drafts")).unwrap();
    fs::write(ws.join("drafts/a.md"), "Alpha").unwrap();
    fs::write(ws.join("drafts/b.md"), "Beta").unwrap();

    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.open_workspace(ws.clone(), Some(ws.join("drafts/a.md")));
    editor.open_workspace_file(ws.join("drafts/b.md"), true);

    let key_a = DocId::File(ws.join("drafts/a.md").canonicalize().unwrap()).tab_key();
    let key_b = DocId::File(ws.join("drafts/b.md").canonicalize().unwrap()).tab_key();
    let mut tx = Tx::default();

    // Edit Tab A
    editor.activate_tab(&key_a, &mut tx);
    editor.active_working_edit_mut().insert(" one");
    editor.changed_active(&mut tx);
    assert_eq!(editor.active_working_edit().text, "Alpha one");

    // Switch to Tab B and edit Tab B
    editor.activate_tab(&key_b, &mut tx);
    editor.active_working_edit_mut().insert(" two");
    editor.changed_active(&mut tx);
    assert_eq!(editor.active_working_edit().text, "Beta two");

    // Switch back to Tab A: undo/redo must still be intact and independent
    editor.activate_tab(&key_a, &mut tx);
    assert_eq!(editor.active_working_edit().text, "Alpha one");
    editor.active_working_edit_mut().undo();
    assert_eq!(editor.active_working_edit().text, "Alpha");
    editor.active_working_edit_mut().redo();
    assert_eq!(editor.active_working_edit().text, "Alpha one");

    // Tab B stayed untouched
    editor.activate_tab(&key_b, &mut tx);
    assert_eq!(editor.active_working_edit().text, "Beta two");

    drop(editor);
    unsafe {
        if let Some(v) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", v);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn blur_flushes_dirty_autosave_buffers_and_manual_save_respects_mode() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-blur-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let ws = base.join("ws");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(ws.join("drafts")).unwrap();
    let file_a = ws.join("drafts/a.md");
    let file_b = ws.join("drafts/b.md");
    fs::write(&file_a, "Original A").unwrap();
    fs::write(&file_b, "Original B").unwrap();

    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    // Auto-save ON: blur flushes file A immediately to disk.
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        Box::new(execute_write_op),
        noop_wake(),
    );
    editor.open_workspace(ws.clone(), Some(file_a.clone()));
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Updated A on blur".into();
    editor.changed_active(&mut tx);
    assert!(editor.documents[0].is_dirty());

    editor.flush_autosave_buffers();
    drain_until_idle(&mut editor);
    assert_eq!(fs_io::read_utf8(&file_a).unwrap(), "Updated A on blur");
    assert!(!editor.documents[0].is_dirty());

    // Auto-save OFF (Manual Save): blur writes only crash-recovery shadow file,
    // NOT file B; explicit Cmd/Ctrl+S writes file B and clears recovery file.
    editor.store.ui.autosave_workspace_files = false;
    editor.open_workspace_file(file_b.clone(), true);
    let b_rec = editor.active_doc_ref().unwrap().recovery_path();
    editor.active_working_edit_mut().text = "Manual B edit".into();
    editor.changed_active(&mut tx);
    editor.flush_autosave_buffers();
    drain_until_idle(&mut editor);
    assert_eq!(fs_io::read_utf8(&file_b).unwrap(), "Original B");
    assert_eq!(fs_io::read_utf8(&b_rec).unwrap(), "Manual B edit");
    assert!(editor.active_doc_ref().unwrap().is_dirty());

    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert_eq!(fs_io::read_utf8(&file_b).unwrap(), "Manual B edit");
    assert!(!b_rec.exists());
    assert!(!editor.active_doc_ref().unwrap().is_dirty());

    drop(editor);
    unsafe {
        if let Some(v) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", v);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn section_inference_uses_frontmatter_until_overridden() {
    let doc = DocumentBuffer::from_workspace_file(
        PathBuf::from("/tmp/draft.md"),
        "draft.md".into(),
        "---\nsection: notes\n---\nHello.\n".into(),
        false,
        false,
        true,
        None,
    );
    assert_eq!(section_for_document(&doc), "notes");
    let mut picked = doc;
    picked.section_override = Some("all".into());
    assert_eq!(section_for_document(&picked), "all");
    let manifest = DocumentBuffer::from_workspace_file(
        PathBuf::from("/tmp/manifest.json"),
        "manifest.json".into(),
        "{}".into(),
        true,
        false,
        true,
        None,
    );
    assert!(!judge_target(&manifest));
}

#[test]
fn rapid_edits_coalesce_into_one_judge_and_stale_replies_are_dropped() {
    use crate::bridge::{BridgeBackend, BridgeReply, JudgeReport};
    use serde_json::{Value, json};

    struct RecordBackend {
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl BridgeBackend for RecordBackend {
        fn call(&mut self, method: &str, params: &Value) -> Result<Value, EditorError> {
            self.calls
                .lock()
                .unwrap()
                .push(params["text"].as_str().unwrap_or(method).to_owned());
            Ok(json!({
                "ok": true, "score": 86, "failUnder": 70, "section": "notes",
                "checks": [{"metric": "medianSentenceLength", "label": "Median sentence", "target": 10.0, "actual": 8.0, "low": 6.0, "high": 14.0, "status": "low", "penalty": 4}, {"metric": "emDashesPer1k", "label": "Em dashes / 1k", "target": 1.0, "actual": 1.0, "low": 0.0, "high": 2.0, "status": "ok", "penalty": 0}],
                "needsBuild": false
            }))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut session = test_session();
    session.after = "---\nsection: notes\n---\nFirst.\n".into();
    let mut editor = Editor::with_handlers_backend(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
        Some(Box::new(RecordBackend {
            calls: Arc::clone(&calls),
        })),
    );
    let mut tx = Tx::default();
    editor.set_judge_section("archive".into(), 0.0);
    assert_eq!(
        editor.documents[0].section_override.as_deref(),
        Some("archive")
    );
    for text in [
        "---\nsection: notes\n---\nOne.\n",
        "---\nsection: notes\n---\nTwo.\n",
        "---\nsection: notes\n---\nThree.\n",
    ] {
        editor.active_working_edit_mut().text = text.into();
        editor.changed_active(&mut tx);
    }
    assert_eq!(
        editor
            .bridge
            .submitted
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(!poll(&mut editor, 1.0, &mut tx) || editor.worker_busy.is_some());
    assert_eq!(
        editor
            .bridge
            .submitted
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    poll(&mut editor, 1.2, &mut tx);
    assert_eq!(
        editor
            .bridge
            .submitted
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while editor.active_judge_report().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "judge reply did not arrive"
        );
        poll(&mut editor, 1.2, &mut tx);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["---\nsection: notes\n---\nThree.\n"]
    );
    assert_eq!(editor.active_judge_report().unwrap().score, 86);
    assert_eq!(
        editor.voice_pill_label().as_deref(),
        Some("Voice: 86 · 1 gap")
    );
    let doc_id = editor.documents[0].id.clone();
    editor.apply_bridge_reply(BridgeReply::Judge {
        request_id: 0,
        doc_id: doc_id.clone(),
        generation: 1,
        section: "notes".into(),
        result: Box::new(JudgeReport {
            score: 1,
            ..JudgeReport::default()
        }),
    });
    assert_eq!(editor.active_judge_report().unwrap().score, 86);

    editor.active_working_edit_mut().text = "---\nsection: notes\n---\nFour.\n".into();
    editor.changed_active(&mut tx);
    let superseded = editor.documents[0].generation - 1;
    editor.apply_bridge_reply(BridgeReply::Judge {
        request_id: 0,
        doc_id,
        generation: superseded,
        section: "notes".into(),
        result: Box::new(JudgeReport {
            score: 2,
            ..JudgeReport::default()
        }),
    });
    assert_eq!(editor.active_judge_report().unwrap().score, 86);
}

#[test]
fn edit_during_judge_drops_the_superseded_reply() {
    use crate::bridge::BridgeBackend;
    use serde_json::{Value, json};
    use std::sync::Condvar;

    struct HoldBackend {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        started: mpsc::Sender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }
    impl BridgeBackend for HoldBackend {
        fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, EditorError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = self.started.send(());
            let (lock, cv) = &*self.release;
            let _guard = cv
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |ready| {
                    !*ready
                })
                .unwrap();
            Ok(json!({"ok": true, "score": 40, "failUnder": 70, "needsBuild": false}))
        }
    }

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let mut editor = Editor::with_handlers_backend(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
        Some(Box::new(HoldBackend {
            calls: Arc::clone(&calls),
            started: started_tx,
            release: Arc::clone(&release),
        })),
    );
    editor.judge_armed = false;
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "Before the reply.\n".into();
    editor.changed_active(&mut tx);
    poll(&mut editor, 1.2, &mut tx);
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    editor.active_working_edit_mut().text = "Edited while judging.\n".into();
    editor.changed_active(&mut tx);
    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while editor.judge_inflight.is_some() {
        assert!(std::time::Instant::now() < deadline, "judge did not settle");
        poll(&mut editor, 1.0, &mut tx);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(editor.active_judge_report().is_none());
    assert!(editor.judge_due.is_some());
}

struct ScriptedHold {
    scores: Vec<i32>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    started: mpsc::Sender<usize>,
    release: Arc<(Mutex<usize>, std::sync::Condvar)>,
}

impl crate::bridge::BridgeBackend for ScriptedHold {
    fn call(
        &mut self,
        _method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, EditorError> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = self.started.send(n);
        let (lock, cv) = &*self.release;
        let _guard = cv
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |ready| {
                *ready <= n
            })
            .unwrap();
        let score = self.scores.get(n).copied().unwrap_or(1);
        let section = params
            .get("section")
            .and_then(|value| value.as_str())
            .unwrap_or("all");
        Ok(serde_json::json!({
            "ok": true, "score": score, "failUnder": 70, "section": section, "needsBuild": false
        }))
    }
}

fn release_holds(release: &Arc<(Mutex<usize>, std::sync::Condvar)>, ready: usize) {
    let (lock, cv) = &**release;
    *lock.lock().unwrap() = ready;
    cv.notify_all();
}

fn wait_for_judge_score(editor: &mut Editor, score: i32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut tx = Tx::default();
    while editor.active_judge_report().map(|report| report.score) != Some(score) {
        assert!(
            std::time::Instant::now() < deadline,
            "judge score {score} did not land"
        );
        poll(editor, 10.0, &mut tx);
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn section_switch_during_judge_keeps_the_new_section_score() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    let mut editor = Editor::with_handlers_backend(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
        Some(Box::new(ScriptedHold {
            scores: vec![11, 91],
            calls: Arc::clone(&calls),
            started: started_tx,
            release: Arc::clone(&release),
        })),
    );
    editor.judge_armed = false;
    let mut tx = Tx::default();
    editor.request_judge(0.0);
    poll(&mut editor, JUDGE_DELAY.as_secs_f64(), &mut tx);
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    editor.set_judge_section("essays".into(), JUDGE_DELAY.as_secs_f64());
    assert!(!editor.documents[0].is_dirty());
    assert!(editor.active_judge_report().is_none());
    poll(
        &mut editor,
        (JUDGE_DELAY + JUDGE_DELAY).as_secs_f64(),
        &mut tx,
    );
    assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 2);
    assert_eq!(
        editor
            .judge_inflight
            .as_ref()
            .map(|inflight| inflight.section.as_str()),
        Some("essays")
    );
    release_holds(&release, 2);
    wait_for_judge_score(&mut editor, 91);
    assert_eq!(
        editor.documents[0]
            .cached_judge
            .as_ref()
            .map(|cached| cached.report.score),
        Some(91)
    );
    assert_eq!(editor.voice_pill_label().as_deref(), Some("Voice: 91"));
    assert!(!editor.documents[0].is_dirty());
    assert!(
        editor
            .active_judge_report()
            .is_some_and(|report| report.score != 11)
    );
}

#[test]
fn aba_section_switch_and_rebuild_drop_stale_judge_replies() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let calls = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    let mut editor = Editor::with_handlers_backend(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
        Some(Box::new(ScriptedHold {
            scores: vec![11, 22, 33],
            calls: Arc::clone(&calls),
            started: started_tx,
            release: Arc::clone(&release),
        })),
    );
    editor.judge_armed = false;
    let mut tx = Tx::default();
    editor.request_judge(0.0);
    let delay = JUDGE_DELAY.as_secs_f64();
    poll(&mut editor, delay, &mut tx);
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    editor.set_judge_section("essays".into(), delay);
    poll(&mut editor, delay * 2.0, &mut tx);
    editor.set_judge_section("all".into(), delay * 2.0);
    poll(&mut editor, delay * 3.0, &mut tx);
    assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 3);
    assert!(!editor.documents[0].is_dirty());
    release_holds(&release, 3);
    wait_for_judge_score(&mut editor, 33);
    assert_eq!(
        editor.active_judge_report().map(|report| report.score),
        Some(33)
    );
    assert!(!editor.documents[0].is_dirty());

    let calls = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    let mut editor = Editor::with_handlers_backend(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
        Some(Box::new(ScriptedHold {
            scores: vec![11, 44],
            calls,
            started: started_tx,
            release: Arc::clone(&release),
        })),
    );
    editor.judge_armed = false;
    editor.request_judge(0.0);
    poll(&mut editor, delay, &mut tx);
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    editor.invalidate_voice_analysis();
    assert_eq!(editor.documents[0].judge_epoch, 1);
    assert!(editor.documents[0].latest_judge_request.is_none());
    assert!(editor.active_judge_report().is_none());
    assert!(editor.judge_inflight.is_none());
    assert!(!editor.documents[0].is_dirty());
    poll(&mut editor, delay, &mut tx);
    poll(&mut editor, delay * 2.0, &mut tx);
    assert_eq!(editor.bridge.submitted.load(Ordering::Relaxed), 2);
    release_holds(&release, 2);
    wait_for_judge_score(&mut editor, 44);
    assert!(!editor.documents[0].is_dirty());
}

fn plant_judge(editor: &mut Editor, section: &str, score: i32) {
    let voice = editor.voice_project.clone().expect("voice project");
    let doc = editor.active_doc_mut().unwrap();
    let epoch = doc.judge_epoch;
    doc.cached_judge = Some(CachedJudge {
        voice_project: voice,
        judge_epoch: epoch,
        section: section.into(),
        report: crate::bridge::JudgeReport {
            ok: true,
            score,
            section: section.into(),
            banned: vec![crate::bridge::BannedHit {
                tell: "delve".into(),
                count: 1,
            }],
            ..crate::bridge::JudgeReport::default()
        },
    });
}

fn element_mentions(elements: &[rune::ElementInfo], needle: &str) -> bool {
    elements
        .iter()
        .filter(|element| !element.exiting)
        .any(|element| {
            element
                .text
                .as_deref()
                .is_some_and(|text| text.contains(needle))
                || element
                    .label
                    .as_deref()
                    .is_some_and(|text| text.contains(needle))
        })
}

#[test]
fn yaml_section_edit_drops_the_cached_judge_across_tab_switches() {
    let essays = "---\nsection: essays\n---\nPlease delve here.\n";
    let mut session = test_session();
    session.after = essays.into();
    session.original = essays.into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.store.ui.voice_lens = true;
    if !editor.inspector_pane.is_open() {
        editor.inspector_pane.toggle();
    }
    plant_judge(&mut editor, "essays", 77);
    assert_eq!(
        editor.active_judge_report().map(|report| report.score),
        Some(77)
    );
    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1280.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    assert!(element_mentions(&rt.elements(), "Score 77"));
    assert!(element_mentions(&rt.elements(), "Voice: 77"));
    rt.update(|editor, tx| {
        editor.active_working_edit_mut().text =
            "---\nsection: notes\n---\nPlease delve here.\n".into();
        editor.changed_active(tx);
    });
    assert!(rt.state.active_judge_report().is_none());
    assert!(rt.state.documents[0].cached_judge.is_none());
    rt.frame(1.0 / 60.0);
    assert!(!element_mentions(&rt.elements(), "Score 77"));
    assert!(!element_mentions(&rt.elements(), "Voice: 77"));
    let draft_key = rt.state.documents[0].id.tab_key();
    rt.update(|editor, tx| {
        let other = DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/other-judge.md"),
            "other-judge.md".into(),
            "Other note.\n".into(),
            false,
            false,
            true,
            editor.voice_project.clone(),
        );
        let other_key = other.id.tab_key();
        editor.documents.push(other);
        editor.activate_tab(&other_key, tx);
    });
    assert!(rt.state.active_judge_report().is_none());
    rt.update(|editor, tx| editor.activate_tab(&draft_key, tx));
    assert!(rt.state.active_judge_report().is_none());
    assert!(
        rt.state
            .documents
            .iter()
            .find(|doc| doc.id.tab_key() == draft_key)
            .unwrap()
            .cached_judge
            .is_none()
    );
    rt.frame(0.0);
    assert!(!element_mentions(&rt.elements(), "Score 77"));
    assert!(!element_mentions(&rt.elements(), "Voice: 77"));
}

#[test]
fn judge_report_does_not_leak_across_close_delete_workspace_or_recovery() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home =
        std::env::temp_dir().join(format!("ghostless-judge-life-home-{}", std::process::id()));
    let root = std::env::temp_dir().join(format!("ghostless-judge-life-{}", std::process::id()));
    let other =
        std::env::temp_dir().join(format!("ghostless-judge-life-other-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&other).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let mut tx = Tx::default();

    let essays = "---\nsection: essays\n---\nPlease delve here.\n";
    let mut session = test_session();
    session.after = essays.into();
    session.original = essays.into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    plant_judge(&mut editor, "essays", 77);
    let key = editor.documents[0].id.tab_key();
    assert!(editor.close_tab_by_id(&key, &mut tx));
    assert!(editor.documents.is_empty());
    assert!(editor.active_judge_report().is_none());

    let draft = root.join("draft.md");
    let keep = root.join("keep.md");
    fs_io::atomic_write_text(&draft, essays).unwrap();
    fs_io::atomic_write_text(&keep, "Keep this.\n").unwrap();
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.open_workspace_with_voice(root.clone(), Some(draft.clone()), Some(root.clone()));
    plant_judge(&mut editor, "essays", 77);
    let mut linked = DocumentBuffer::from_session(test_session(), true, Some(draft.clone()));
    linked.mirror_path = Some(draft.canonicalize().unwrap_or(draft.clone()));
    editor.documents.push(linked);
    editor.chrome.active_dialog = Some(DialogKind::Delete {
        rel_path: "draft.md".into(),
        display_name: "draft.md".into(),
        is_dir: false,
    });
    editor.submit_dialog(&mut tx);
    assert!(editor.active_judge_report().is_none());
    assert!(
        editor
            .documents
            .iter()
            .all(|doc| doc.cached_judge.is_none())
    );
    let linked = editor
        .documents
        .iter()
        .find(|doc| doc.is_session())
        .unwrap();
    assert!(linked.mirror_path.is_none());

    editor.open_workspace_file(keep.clone(), true);
    plant_judge(&mut editor, "all", 77);
    assert_eq!(
        editor.active_judge_report().map(|report| report.score),
        Some(77)
    );
    editor.open_workspace_with_voice(other.clone(), None, Some(other.clone()));
    assert!(editor.active_judge_report().is_none());
    assert!(editor.judge_inflight.is_none());
    assert!(editor.judge_due.is_none());
    assert!(!editor.conflict_compare);
    assert!(
        editor
            .documents
            .iter()
            .all(|doc| doc.cached_judge.is_none())
    );

    let recover = root.join("recover.md");
    fs_io::atomic_write_text(&recover, essays).unwrap();
    editor.open_workspace_with_voice(root.clone(), Some(recover.clone()), Some(root.clone()));
    plant_judge(&mut editor, "essays", 77);
    let recovery = editor.active_doc_ref().unwrap().recovery_path();
    fs_io::atomic_write_text(&recovery, "---\nsection: essays\n---\nRecovered body.\n").unwrap();
    let doc_id = editor.active_doc_ref().unwrap().id.clone();
    let title = editor.active_doc_ref().unwrap().title.clone();
    editor.chrome.active_dialog = Some(DialogKind::Recover { doc_id, title });
    editor.resolve_recovery(true);
    assert!(editor.active_doc_ref().unwrap().cached_judge.is_none());
    assert!(editor.active_judge_report().is_none());
    assert_eq!(
        editor.active_working_edit().text,
        "---\nsection: essays\n---\nRecovered body.\n"
    );

    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
}

#[test]
fn voice_navigation_keeps_text_and_returns_focus_in_every_mode() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!("ghostless-lens-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    let draft =
        "# Opening\n\nKeep [PLACEHOLDER: one] then [PLACEHOLDER: two].\n\nPlease delve here.\n";
    let cases = [
        (ViewMode::Write, false, false, false),
        (ViewMode::Preview, false, false, false),
        (ViewMode::Compare, false, false, false),
        (ViewMode::Write, true, false, false),
        (ViewMode::Preview, true, false, false),
        (ViewMode::Compare, true, false, false),
        (ViewMode::Write, false, true, false),
        (ViewMode::Preview, false, true, false),
        (ViewMode::Compare, false, true, false),
        (ViewMode::Write, true, false, true),
        (ViewMode::Preview, true, false, true),
        (ViewMode::Compare, true, false, true),
    ];
    for (mode, read_only, locked, voice_guide) in cases {
        let mut session = test_session();
        session.after = draft.into();
        session.original = draft.into();
        let mut editor = Editor::with_handlers(
            session,
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        {
            let doc = editor.active_doc_mut().unwrap();
            doc.read_only = read_only || voice_guide;
            doc.lockable = locked;
            doc.user_locked = locked;
            if voice_guide {
                doc.path.set_file_name("voice-guide.md");
                doc.title = "voice-guide.md".into();
            }
        }
        editor.view_mode = mode;
        editor.store.ui.voice_lens = false;
        let generation = editor.active_doc_ref().unwrap().generation;
        let focus = if voice_guide {
            "preview-body"
        } else if read_only || locked {
            "working-body"
        } else {
            "prose-input"
        };

        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.on_key = Some(on_key);
        rt.poll = Some(poll);
        rt.frame(0.0);
        if mode == ViewMode::Write && !read_only && !locked && !voice_guide {
            assert!(
                rt.elements()
                    .iter()
                    .any(|element| element.key.as_deref() == Some("next-placeholder"))
            );
        }
        rt.key(KeyEvent {
            key: Key::Function(8),
            mods: rune::Mods::default(),
        });
        rt.frame(0.0);
        assert_eq!(
            rt.state.active_working_edit().text,
            draft,
            "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}"
        );
        assert_eq!(rt.state.active_doc_ref().unwrap().generation, generation);
        assert_eq!(
            rt.state.active_working_edit().selected(),
            "[PLACEHOLDER: one]"
        );
        assert_eq!(rt.state.view_mode, ViewMode::Write);
        assert_surface(&rt, focus);
        rt.key(KeyEvent {
            key: Key::Command('l'),
            mods: rune::Mods::default(),
        });
        rt.frame(0.0);
        assert!(rt.state.store.ui.voice_lens);
        assert_eq!(rt.state.active_working_edit().text, draft);
        assert_surface(&rt, focus);
        rt.update(|editor, tx| editor.jump_to_query("delve", tx));
        rt.frame(0.0);
        assert_eq!(rt.state.active_working_edit().selected(), "delve");
        assert_eq!(rt.state.active_working_edit().text, draft);
        assert_eq!(rt.state.active_doc_ref().unwrap().generation, generation);
        let focused = rt
            .elements()
            .iter()
            .find(|element| element.focused)
            .and_then(|element| element.key.clone());
        assert_eq!(
            focused.as_deref(),
            Some(focus),
            "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}"
        );
    }

    let mut session = test_session();
    session.after = draft.into();
    let editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);
    rt.update(|editor, tx| editor.jump_next_placeholder(tx));
    rt.frame(0.0);
    assert_eq!(
        rt.state.active_working_edit().selected(),
        "[PLACEHOLDER: one]"
    );
    rt.key(KeyEvent {
        key: Key::Command('.'),
        mods: rune::Mods {
            shift: true,
            ..rune::Mods::default()
        },
    });
    rt.frame(0.0);
    assert_eq!(
        rt.state.active_working_edit().selected(),
        "[PLACEHOLDER: two]"
    );
    rt.key(KeyEvent {
        key: Key::Function(8),
        mods: rune::Mods::default(),
    });
    rt.frame(0.0);
    assert_eq!(
        rt.state.active_working_edit().selected(),
        "[PLACEHOLDER: one]"
    );
    assert_eq!(rt.state.active_working_edit().text, draft);

    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = std::fs::remove_dir_all(home);
}

fn assert_surface(rt: &rune::Runtime<Editor>, focus: &str) {
    let elements = rt.elements();
    let focused = elements
        .iter()
        .find(|element| element.focused)
        .unwrap_or_else(|| {
            panic!("nothing focused; wanted {focus}");
        });
    assert_eq!(focused.key.as_deref(), Some(focus));
    let rect = focused.rect;
    assert!(
        rect[2] > 1.0 && rect[3] > 1.0,
        "focus {focus} has no area {rect:?}"
    );
    assert!(
        rect[0] >= -1.0
            && rect[1] >= -1.0
            && rect[0] + rect[2] <= 1121.0
            && rect[1] + rect[3] <= 801.0,
        "{focus} {:?} is outside the window",
        rect
    );
    if let Some(parent) = elements
        .iter()
        .find(|element| element.key.as_deref() == Some("working-body"))
    {
        assert!(
            rect[1] >= parent.rect[1] - 1.0
                && rect[1] + rect[3] <= parent.rect[1] + parent.rect[3] + 1.0,
            "{focus} {:?} outside working body {:?}",
            rect,
            parent.rect
        );
    }
    for stray in [
        "find-source-match",
        "find-source-anchor",
        "preview-find-anchor",
    ] {
        assert!(
            elements
                .iter()
                .all(|element| element.key.as_deref() != Some(stray)),
            "{stray} left behind"
        );
    }
}

#[test]
fn linked_reopen_detects_divergence_without_writing_either_copy() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!("ghostless-conflict-home-{}", std::process::id()));
    let root = std::env::temp_dir().join(format!("ghostless-conflict-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let session_id = "2026-04-02-notes";
    let folder = root.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let before = "Session copy.\n";
    let before_sha = fs_io::sha256_text(before);
    fs_io::atomic_write_text(&folder.join("before.md"), before).unwrap();
    fs_io::atomic_write_text(&folder.join("after.md"), before).unwrap();
    fs_io::atomic_write_text(
        &folder.join("session.json"),
        &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#),
    )
    .unwrap();
    let draft = root.join("draft.md");
    fs_io::atomic_write_text(&draft, before).unwrap();
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&root, session_id),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        },
    );
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.workspace = Some(root.clone());
    editor.voice_project = Some(root.clone());
    editor.open_workspace_file(draft.clone(), true);
    assert!(editor.active_doc_ref().unwrap().conflict.is_none());
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), before);
    assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), before);
    assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), before);

    fs_io::atomic_write_text(&draft, "Disk edit.\n").unwrap();
    let store = prefs::load_store();
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.workspace = Some(root.clone());
    editor.voice_project = Some(root.clone());
    editor.open_workspace_file(draft.clone(), true);
    let doc = editor.active_doc_ref().unwrap();
    assert!(doc.conflict.is_some());
    assert_eq!(doc.edit.text, before);
    assert_eq!(doc.conflict.as_ref().unwrap().disk_text, "Disk edit.\n");
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), "Disk edit.\n");
    assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), before);
    assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), before);

    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn diverged_linked_open_stays_clean_until_keep_session_mirrors() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!(
        "ghostless-conflict-save-home-{}",
        std::process::id()
    ));
    let root = std::env::temp_dir().join(format!("ghostless-conflict-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let session_id = "2026-04-02-notes";
    let folder = root.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let session_copy = "Session copy.\n";
    let disk_text = "Disk edit.\n";
    let before_sha = fs_io::sha256_text(session_copy);
    fs_io::atomic_write_text(&folder.join("before.md"), session_copy).unwrap();
    fs_io::atomic_write_text(&folder.join("after.md"), session_copy).unwrap();
    fs_io::atomic_write_text(
        &folder.join("session.json"),
        &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#),
    )
    .unwrap();
    let draft = root.join("draft.md");
    fs_io::atomic_write_text(&draft, disk_text).unwrap();
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&root, session_id),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        },
    );
    let mut editor = Editor::empty_with_handlers(
        store,
        ok_runner(),
        Box::new(crate::worker::execute_write_op),
        noop_wake(),
    );
    editor.workspace = Some(root.clone());
    editor.voice_project = Some(root.clone());
    editor.open_workspace_file(draft.clone(), true);
    {
        let doc = editor.active_doc_ref().unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(doc.edit.text, session_copy);
        assert_eq!(doc.conflict.as_ref().unwrap().disk_text, disk_text);
    }
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), disk_text);

    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    let elements = rt.elements();
    assert!(
        elements
            .iter()
            .any(|element| element.text.as_deref() == Some("Disk conflict — resolve above"))
    );
    assert!(
        elements
            .iter()
            .any(|element| element.key.as_deref() == Some("keep-session"))
    );
    rt.key(KeyEvent {
        key: Key::Command('s'),
        mods: rune::Mods::default(),
    });
    rt.frame(0.0);
    rt.update(|editor, _| editor.save_clicked());
    assert_eq!(
        rt.state.status,
        "Resolve the disk conflict (Load Disk or Keep Session) before syncing to disk.",
    );
    assert!(!rt.state.status.contains("Saved locally"));
    assert!(!rt.state.active_doc_ref().unwrap().is_dirty());
    assert!(rt.state.worker_busy.is_none());
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), disk_text);
    assert_eq!(
        fs_io::read_utf8(&folder.join("before.md")).unwrap(),
        session_copy
    );

    let [x, y, w, h] = rt
        .elements()
        .iter()
        .find(|element| element.key.as_deref() == Some("keep-session"))
        .unwrap()
        .rect;
    rt.pointer_down(x + w / 2.0, y + h / 2.0);
    rt.pointer_up(x + w / 2.0, y + h / 2.0);
    rt.frame(0.0);
    assert!(rt.state.active_doc_ref().unwrap().conflict.is_none());
    assert_eq!(
        rt.state.worker_busy.as_ref().map(|(_, kind)| *kind),
        Some(InFlight::Save)
    );
    assert!(!rt.state.active_doc_ref().unwrap().is_dirty());
    rt.update(|editor, _| drain_until_idle(editor));
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), session_copy);
    assert_eq!(
        fs_io::read_utf8(&folder.join("after.md")).unwrap(),
        session_copy
    );
    assert_eq!(
        fs_io::read_utf8(&folder.join("before.md")).unwrap(),
        session_copy
    );

    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn compare_reverts_are_undoable_and_refuse_locked_surfaces() {
    let original = "Alpha paragraph.\n\nMiddle paragraph.\n\nOmega paragraph.\n";
    let edited = "Alpha changed.\n\nMiddle edited.\n\nOmega changed.\n";
    let mut session = test_session();
    session.original = original.into();
    session.after = edited.into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.view_mode = ViewMode::Compare;
    editor.refresh_diff();
    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);
    rt.update(|editor, tx| editor.revert_hunk(1, tx));
    rt.frame(0.0);
    assert_eq!(
        rt.state.active_working_edit().text,
        "Alpha changed.\n\nMiddle paragraph.\n\nOmega changed.\n"
    );
    rt.update(|editor, _| editor.active_working_edit_mut().undo());
    assert_eq!(rt.state.active_working_edit().text, edited);
    rt.update(|editor, tx| editor.revert_to_original(tx));
    rt.frame(0.0);
    assert_eq!(rt.state.active_working_edit().text, original);
    rt.update(|editor, _| editor.active_working_edit_mut().undo());
    assert_eq!(rt.state.active_working_edit().text, edited);
    assert_surface(&rt, "compare-body");

    for (mode, read_only, locked, voice_guide) in [
        (ViewMode::Write, false, false, false),
        (ViewMode::Preview, false, false, false),
        (ViewMode::Compare, true, false, false),
        (ViewMode::Compare, false, true, false),
        (ViewMode::Compare, true, false, true),
    ] {
        let mut session = test_session();
        session.original = original.into();
        session.after = edited.into();
        let mut editor = Editor::with_handlers(
            session,
            prefs::EditorStateStore::default(),
            ok_runner(),
            ok_writer(),
            noop_wake(),
        );
        {
            let doc = editor.active_doc_mut().unwrap();
            doc.read_only = read_only || voice_guide;
            doc.lockable = locked;
            doc.user_locked = locked;
            if voice_guide {
                doc.path.set_file_name("voice-guide.md");
            }
        }
        editor.view_mode = mode;
        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        rt.update(|editor, tx| {
            editor.revert_hunk(1, tx);
            editor.revert_to_original(tx);
        });
        rt.frame(0.0);
        assert_eq!(
            rt.state.active_working_edit().text,
            edited,
            "{mode:?} read_only={read_only} locked={locked} guide={voice_guide}"
        );
        let elements = rt.elements();
        for stray in [
            "find-source-match",
            "find-source-anchor",
            "preview-find-anchor",
        ] {
            assert!(
                elements
                    .iter()
                    .all(|element| element.key.as_deref() != Some(stray))
            );
        }
    }
}

#[test]
fn conflict_compare_uses_the_disk_text_and_refuses_revert() {
    let original = "Baseline only.\n";
    let edited = "one two three\n";
    let disk = "alpha beta gamma delta epsilon\n";
    let mut session = test_session();
    session.original = original.into();
    session.after = edited.into();
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.active_doc_mut().unwrap().conflict = Some(crate::documents::SessionConflict {
        disk_text: disk.into(),
        session_text: edited.into(),
    });
    editor.conflict_compare = true;
    editor.view_mode = ViewMode::Compare;
    editor.refresh_diff();
    assert!(!editor.can_revert());
    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);
    let elements = rt.elements();
    let summary = elements
        .iter()
        .find_map(|element| {
            element
                .text
                .as_deref()
                .filter(|text| text.contains("Disk vs session"))
        })
        .expect("disk compare summary");
    assert!(summary.contains("Words: 5 → 3 (-2)"), "{summary}");
    assert!(summary.contains("Changed hunks: 1"), "{summary}");
    assert!(
        elements
            .iter()
            .all(|element| element.key.as_deref() != Some("revert-original"))
    );
    assert!(elements.iter().all(|element| {
        !element
            .key
            .as_deref()
            .unwrap_or("")
            .starts_with("revert-hunk")
    }));
    let before = rt.state.active_working_edit().text.clone();
    rt.update(|editor, tx| {
        editor.revert_hunk(0, tx);
        editor.revert_to_original(tx);
    });
    assert_eq!(rt.state.active_working_edit().text, before);
    assert!(!rt.state.can_revert());
    rt.update(|editor, tx| editor.set_view_mode(ViewMode::Write, tx));
    assert!(!rt.state.conflict_compare);
    rt.update(|editor, tx| {
        editor.conflict_compare = true;
        editor.set_view_mode(ViewMode::Compare, tx);
        let other = DocumentBuffer::from_workspace_file(
            PathBuf::from("/tmp/other.md"),
            "other.md".into(),
            "Other note.\n".into(),
            false,
            false,
            true,
            None,
        );
        let key = other.id.tab_key();
        editor.documents.push(other);
        editor.activate_tab(&key, tx);
    });
    assert!(!rt.state.conflict_compare);
}

#[test]
fn reactivating_a_linked_tab_keeps_unsaved_text_and_shows_external_drift() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home =
        std::env::temp_dir().join(format!("ghostless-reactivate-home-{}", std::process::id()));
    let root = std::env::temp_dir().join(format!("ghostless-reactivate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let session_id = "2026-04-02-notes";
    let folder = root.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let session_text = "Session copy.\n";
    let before_sha = fs_io::sha256_text(session_text);
    fs_io::atomic_write_text(&folder.join("before.md"), session_text).unwrap();
    fs_io::atomic_write_text(&folder.join("after.md"), session_text).unwrap();
    fs_io::atomic_write_text(
        &folder.join("session.json"),
        &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#),
    )
    .unwrap();
    let draft = root.join("draft.md");
    fs_io::atomic_write_text(&draft, session_text).unwrap();
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&root, session_id),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        },
    );
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.workspace = Some(root.clone());
    editor.voice_project = Some(root.clone());
    editor.open_workspace_file(draft.clone(), true);
    let other = DocumentBuffer::from_workspace_file(
        root.join("other.md"),
        "other.md".into(),
        "Other note.\n".into(),
        false,
        false,
        true,
        None,
    );
    let other_key = other.id.tab_key();
    let draft_key = editor.active_doc_ref().unwrap().id.tab_key();
    editor.documents.push(other);
    fs_io::atomic_write_text(&draft, "External clean.\n").unwrap();
    let mut tx = Tx::default();
    editor.activate_tab(&other_key, &mut tx);
    editor.activate_tab(&draft_key, &mut tx);
    let doc = editor.active_doc_ref().unwrap();
    assert_eq!(doc.edit.text, session_text);
    assert_eq!(
        doc.conflict
            .as_ref()
            .map(|conflict| conflict.disk_text.as_str()),
        Some("External clean.\n")
    );
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), "External clean.\n");
    assert_eq!(
        fs_io::read_utf8(&folder.join("after.md")).unwrap(),
        session_text
    );
    assert_eq!(
        fs_io::read_utf8(&folder.join("before.md")).unwrap(),
        session_text
    );

    let unsaved = "Unsaved words.\n";
    {
        let doc = editor.active_doc_mut().unwrap();
        doc.edit = TextEdit::new(unsaved);
        doc.generation += 1;
    }
    fs_io::atomic_write_text(&draft, "External dirty.\n").unwrap();
    editor.activate_tab(&other_key, &mut tx);
    editor.open_workspace_file(draft.clone(), true);
    let doc = editor.active_doc_ref().unwrap();
    assert_eq!(doc.edit.text, unsaved);
    assert_eq!(
        doc.conflict
            .as_ref()
            .map(|conflict| conflict.disk_text.as_str()),
        Some("External dirty.\n")
    );
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), "External dirty.\n");
    assert_eq!(
        fs_io::read_utf8(&folder.join("before.md")).unwrap(),
        session_text
    );

    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn stale_save_reply_does_not_rewind_the_mirror_hash() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!("ghostless-stale-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let mut editor = Editor::with_handlers(
        test_session(),
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let project = editor.voice_project.clone().unwrap();
    let session_id = editor.active_doc_ref().unwrap().session_id.clone().unwrap();
    let key = prefs::EditorStateStore::session_link_key(&project, &session_id);
    editor.store.session_links.insert(
        key.clone(),
        prefs::SessionLink {
            workspace_file: PathBuf::from("/tmp/draft.md"),
            last_synced_sha256: "hash-v2".into(),
        },
    );
    let doc_id = {
        let doc = editor.active_doc_mut().unwrap();
        doc.saved_generation = 2;
        doc.generation = 2;
        doc.last_disk_sha256 = "hash-v2".into();
        doc.id.clone()
    };
    editor.status = "Saved locally".into();
    let saved = editor.apply_reply(
        WorkerReply::Saved {
            doc_id,
            generation: 1,
            sha256: "hash-v1".into(),
            result: Ok(WriteOutcome::Mirrored),
        },
        None,
    );
    assert!(!saved);
    let doc = editor.active_doc_ref().unwrap();
    assert_eq!(doc.last_disk_sha256, "hash-v2");
    assert!(doc.save_error.is_none());
    assert_eq!(
        editor.store.session_links[&key].last_synced_sha256,
        "hash-v2"
    );
    assert_eq!(editor.status, "Saved locally");
    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn edit_during_save_keeps_the_buffer_dirty_and_the_next_mirror_succeeds() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home =
        std::env::temp_dir().join(format!("ghostless-edit-save-home-{}", std::process::id()));
    let root = std::env::temp_dir().join(format!("ghostless-edit-save-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&home).unwrap();
    let previous = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let session_id = "2026-04-02-notes";
    let folder = root.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let v0 = "v0\n";
    let v1 = "v1\n";
    let v2 = "v2\n";
    let before_sha = fs_io::sha256_text(v0);
    fs_io::atomic_write_text(&folder.join("before.md"), v0).unwrap();
    fs_io::atomic_write_text(&folder.join("after.md"), v0).unwrap();
    fs_io::atomic_write_text(&folder.join("notes.md"), "").unwrap();
    fs_io::atomic_write_text(
        &folder.join("session.json"),
        &format!(r#"{{"kind":"ghostless-edit-session","beforeSha256":"{before_sha}"}}"#),
    )
    .unwrap();
    let draft = root.join("draft.md");
    fs_io::atomic_write_text(&draft, v0).unwrap();
    let mut store = prefs::EditorStateStore::default();
    let key = prefs::EditorStateStore::session_link_key(&root, session_id);
    store.session_links.insert(
        key.clone(),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha.clone(),
        },
    );
    let mut session = test_session();
    session.project = root.clone();
    session.id = session_id.into();
    session.original = v0.into();
    session.after = v0.into();
    session.before_sha256 = before_sha.clone();
    let mirrored = crate::worker::write_session_direct(
        &root,
        session_id,
        Some(&before_sha),
        v1,
        "",
        Some(&draft),
        Some(&before_sha),
        false,
    )
    .unwrap();
    assert!(matches!(mirrored, WriteOutcome::Mirrored));
    let mut editor = Editor::with_handlers(session, store, ok_runner(), ok_writer(), noop_wake());
    let doc_id = {
        let doc = editor.active_doc_mut().unwrap();
        doc.edit = TextEdit::new(v2);
        doc.generation = 2;
        doc.saved_generation = 0;
        doc.id.clone()
    };
    assert!(editor.apply_reply(
        WorkerReply::Saved {
            doc_id,
            generation: 1,
            sha256: fs_io::sha256_text(v1),
            result: Ok(WriteOutcome::Mirrored),
        },
        None
    ));
    let doc = editor.active_doc_ref().unwrap();
    assert!(doc.is_dirty());
    assert_eq!(doc.edit.text, v2);
    assert_eq!(doc.last_disk_sha256, fs_io::sha256_text(v1));
    assert_eq!(
        editor.store.session_links[&key].last_synced_sha256,
        fs_io::sha256_text(v1)
    );
    assert!(!editor.status.contains("Saved locally"));
    let expected = editor.store.session_links[&key].last_synced_sha256.clone();
    let next = crate::worker::write_session_direct(
        &root,
        session_id,
        Some(&before_sha),
        v2,
        "",
        Some(&draft),
        Some(&expected),
        false,
    )
    .unwrap();
    assert!(matches!(next, WriteOutcome::Mirrored), "{next:?}");
    assert_eq!(fs_io::read_utf8(&draft).unwrap(), v2);
    assert_eq!(fs_io::read_utf8(&folder.join("after.md")).unwrap(), v2);
    assert_eq!(fs_io::read_utf8(&folder.join("before.md")).unwrap(), v0);
    unsafe {
        if let Some(value) = previous {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn stale_session_start_does_not_attach_or_store_a_link() {
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let voice = PathBuf::from("/tmp/ghostless-session-start-voice");
    editor.voice_project = Some(voice.clone());
    editor.documents.push(DocumentBuffer::from_workspace_file(
        PathBuf::from("/tmp/ghostless-session-start-draft.md"),
        "draft.md".into(),
        "Hello\n".into(),
        false,
        false,
        true,
        Some(voice),
    ));
    editor.active_doc = Some(0);
    let doc_id = {
        let doc = editor.active_doc_mut().unwrap();
        doc.edit = TextEdit::new("Hello edited\n");
        doc.generation = 2;
        doc.saved_generation = 1;
        doc.id.clone()
    };
    editor.session_start_inflight = Some((doc_id.clone(), 1));
    editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
        doc_id: Some(doc_id.clone()),
        generation: 1,
        result: Ok(crate::bridge::SessionStartResult {
            id: "2026-04-02-notes".into(),
            path: "sessions/2026-04-02-notes".into(),
            before_sha256: "abc".into(),
            source_draft_name: "draft.md".into(),
            section: None,
            origin: "ai-first".into(),
        }),
    });
    assert!(editor.active_doc_ref().unwrap().session_id.is_none());
    assert!(editor.store.session_links.is_empty());
    assert!(editor.status.contains("changed"));
    assert!(editor.session_start_inflight.is_none());
    editor.status = "Ready".into();
    editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
        doc_id: Some(doc_id),
        generation: 1,
        result: Err("bridge failed".into()),
    });
    assert_eq!(editor.status, "Ready");
    assert!(editor.store.session_links.is_empty());
}

fn write_test_session_dir(
    project: &Path,
    session_id: &str,
    source_draft_name: &str,
    created_at: &str,
    before: &str,
    after: &str,
    finished: bool,
) -> String {
    let folder = project.join("sessions").join(session_id);
    fs::create_dir_all(&folder).unwrap();
    let before_sha = fs_io::sha256_text(before);
    fs_io::atomic_write_text(&folder.join("before.md"), before).unwrap();
    fs_io::atomic_write_text(&folder.join("after.md"), after).unwrap();
    fs_io::atomic_write_text(&folder.join("notes.md"), "").unwrap();
    fs_io::atomic_write_text(
        &folder.join("session.json"),
        &serde_json::json!({
            "kind": "ghostless-edit-session",
            "id": session_id,
            "sourceDraftName": source_draft_name,
            "origin": "ai-first",
            "createdAt": created_at,
            "beforeSha256": before_sha,
        })
        .to_string(),
    )
    .unwrap();
    if finished {
        fs_io::atomic_write_text(&folder.join("receipt.json"), "{}\n").unwrap();
    }
    before_sha
}

#[test]
fn in_place_track_session_and_sidebar_open_dedupe_both_ways() {
    use crate::{quick_open::search_workspace_files, workspace_tree::FileKind};

    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-dedupe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let root_raw = base.join("ws");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root_raw).unwrap();
    let root = root_raw.canonicalize().unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    fs_io::atomic_write_text(&root.join("manifest.json"), r#"{"kind":"ghostless"}"#).unwrap();
    let draft = root.join("draft.md");
    let body = "Hello linked world.\n";
    fs_io::atomic_write_text(&draft, body).unwrap();
    let session_id = "2026-04-02-notes";
    let before_sha = write_test_session_dir(
        &root,
        session_id,
        "draft.md",
        "2026-04-02T10:00:00Z",
        body,
        body,
        false,
    );

    // 1. In-place Track Session -> clicking sidebar session or reopening file stays on one tab.
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.open_workspace_with_voice(root.clone(), Some(draft.clone()), Some(root.clone()));
    let draft_id = DocId::File(draft.clone());
    editor.session_start_inflight = Some((draft_id.clone(), 0));
    editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
        doc_id: Some(draft_id),
        generation: 0,
        result: Ok(crate::bridge::SessionStartResult {
            id: session_id.into(),
            path: format!("sessions/{session_id}"),
            before_sha256: before_sha.clone(),
            source_draft_name: "draft.md".into(),
            section: None,
            origin: "ai-first".into(),
        }),
    });
    assert_eq!(editor.documents.len(), 1);
    assert_eq!(editor.documents[0].session_id.as_deref(), Some(session_id));

    let mut rt = rune::Runtime::new(
        editor,
        crate::ui::view,
        (1120.0, 800.0),
        rune::Env::new(false, 1.0),
    );
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some("session-2026-04-02-notes"))
    );
    rt.update(|editor, _| editor.open_session_entry(root.clone(), session_id.into()));
    rt.frame(0.0);
    assert_eq!(rt.state.documents.len(), 1);
    assert_eq!(rt.state.active_doc, Some(0));
    assert!(
        rt.state
            .active_doc_ref()
            .unwrap()
            .matches_session(&root, session_id)
    );
    rt.update(|editor, _| editor.open_workspace_file(draft.clone(), true));
    assert_eq!(rt.state.documents.len(), 1);
    drop(rt);

    // 2. Open session from sidebar first -> open_workspace_file & choose_folder_match dedupe onto it.
    let mut store = prefs::EditorStateStore::default();
    store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&root, session_id),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: before_sha,
        },
    );
    let mut editor =
        Editor::empty_with_handlers(store.clone(), ok_runner(), ok_writer(), noop_wake());
    editor.open_workspace_with_voice(root.clone(), None, Some(root.clone()));
    editor.open_session_entry(root.clone(), session_id.into());
    assert_eq!(editor.documents.len(), 1);
    assert!(matches!(editor.documents[0].id, DocId::Session { .. }));
    assert_eq!(editor.documents[0].mirror_path.as_ref(), Some(&draft));

    editor.open_workspace_file(draft.clone(), true);
    assert_eq!(editor.documents.len(), 1);
    assert!(matches!(editor.documents[0].id, DocId::Session { .. }));

    editor.folder_search.open = true;
    editor.folder_search.query.replace_text("linked");
    editor.folder_search.matches = search_workspace_files(
        &[WorkspaceEntry {
            abs_path: draft.clone(),
            rel_path: "draft.md".into(),
            kind: FileKind::Writing,
            read_only: false,
        }],
        &[(draft.clone(), Some(body.into()))].into_iter().collect(),
        "linked",
        10,
        || false,
    );
    let mut tx = Tx::default();
    editor.choose_folder_match(0, &mut tx);
    assert_eq!(editor.documents.len(), 1);
    assert!(editor.find.open);
    assert_eq!(editor.active_working_edit().selected(), "linked");
    assert!(editor.flush_on_exit());

    // 3. Unlinked dirty file open in another tab -> open_session_entry drops mirror on new session tab.
    let dirty_file = root.join("dirty.md");
    fs_io::atomic_write_text(&dirty_file, "Initial dirty.\n").unwrap();
    let dirty_session = "2026-04-03-dirty";
    let dirty_sha = write_test_session_dir(
        &root,
        dirty_session,
        "dirty.md",
        "2026-04-03T10:00:00Z",
        "Initial dirty.\n",
        "Initial dirty.\n",
        false,
    );
    let mut manual_store = prefs::EditorStateStore::default();
    manual_store.ui.autosave_workspace_files = false;
    let mut editor =
        Editor::empty_with_handlers(manual_store, ok_runner(), ok_writer(), noop_wake());
    editor.open_workspace_with_voice(root.clone(), Some(dirty_file.clone()), Some(root.clone()));
    editor.active_working_edit_mut().text = "Unsaved dirty edit.\n".into();
    editor.changed_active(&mut tx);
    editor.store.session_links.insert(
        prefs::EditorStateStore::session_link_key(&root, dirty_session),
        prefs::SessionLink {
            workspace_file: dirty_file,
            last_synced_sha256: dirty_sha,
        },
    );
    editor.open_session_entry(root.clone(), dirty_session.into());
    assert_eq!(editor.documents.len(), 2);
    assert!(editor.active_doc_ref().unwrap().mirror_path.is_none());
    assert!(editor.status.contains("open in another tab"));
    let _ = fs::remove_file(editor.documents[0].recovery_path());
    editor.documents.clear();

    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn rename_and_delete_keep_linked_session_mirror_and_store_consistent() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-rename-link-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let root_raw = base.join("ws");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(root_raw.join("dir")).unwrap();
    let root = root_raw.canonicalize().unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    fs_io::atomic_write_text(&root.join("manifest.json"), r#"{"kind":"ghostless"}"#).unwrap();
    let draft = root.join("draft.md");
    let sub = root.join("dir/sub.md");
    fs_io::atomic_write_text(&draft, "Draft one.\n").unwrap();
    fs_io::atomic_write_text(&sub, "Sub two.\n").unwrap();

    let one_id = "2026-04-02-one";
    let one_sha = write_test_session_dir(
        &root,
        one_id,
        "draft.md",
        "2026-04-02T10:00:00Z",
        "Draft one.\n",
        "Draft one.\n",
        false,
    );
    let two_id = "2026-04-03-two";
    let two_sha = write_test_session_dir(
        &root,
        two_id,
        "sub.md",
        "2026-04-03T10:00:00Z",
        "Sub two.\n",
        "Sub two.\n",
        false,
    );

    let mut store = prefs::EditorStateStore::default();
    let two_key = prefs::EditorStateStore::session_link_key(&root, two_id);
    store.session_links.insert(
        two_key.clone(),
        prefs::SessionLink {
            workspace_file: "dir/sub.md".into(),
            last_synced_sha256: two_sha,
        },
    );
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.open_workspace_with_voice(root.clone(), Some(draft.clone()), Some(root.clone()));

    // Attach one_id in place on DocId::File(draft).
    let one_doc_id = DocId::File(draft.clone());
    editor.session_start_inflight = Some((one_doc_id.clone(), 0));
    editor.apply_bridge_reply(crate::bridge::BridgeReply::SessionStart {
        doc_id: Some(one_doc_id),
        generation: 0,
        result: Ok(crate::bridge::SessionStartResult {
            id: one_id.into(),
            path: format!("sessions/{one_id}"),
            before_sha256: one_sha,
            source_draft_name: "draft.md".into(),
            section: None,
            origin: "ai-first".into(),
        }),
    });
    let one_key = prefs::EditorStateStore::session_link_key(&root, one_id);
    // Open two_id as DocId::Session.
    editor.open_session_entry(root.clone(), two_id.into());
    assert_eq!(editor.documents.len(), 2);

    // Rename draft.md -> renamed.md
    let mut tx = Tx::default();
    editor.chrome.active_dialog = Some(DialogKind::Rename {
        rel_path: "draft.md".into(),
    });
    editor.chrome.dialog_input.replace_text("renamed.md");
    editor.submit_dialog(&mut tx);
    let renamed = root.join("renamed.md");
    assert_eq!(editor.documents[0].id, DocId::File(renamed.clone()));
    assert_eq!(editor.documents[0].path, renamed);
    assert_eq!(editor.documents[0].title, "renamed.md");
    assert_eq!(editor.documents[0].mirror_path.as_ref(), Some(&renamed));
    assert_eq!(editor.store.session_links[&one_key].workspace_file, renamed);
    assert_eq!(
        prefs::load_store().session_links[&one_key].workspace_file,
        renamed
    );
    editor.switch_active_doc(0);
    editor.reconcile_active_linked_doc();
    assert_eq!(editor.documents[0].mirror_path.as_ref(), Some(&renamed));
    assert!(!editor.status.contains("gone"));

    // Rename folder dir -> dir2 (remaps DocId::Session mirror_path & relative link).
    editor.chrome.active_dialog = Some(DialogKind::Rename {
        rel_path: "dir".into(),
    });
    editor.chrome.dialog_input.replace_text("dir2");
    editor.submit_dialog(&mut tx);
    let renamed_sub = root.join("dir2/sub.md");
    assert_eq!(editor.documents[1].mirror_path.as_ref(), Some(&renamed_sub));
    assert_eq!(
        editor.store.session_links[&two_key].workspace_file,
        PathBuf::from("dir2/sub.md")
    );
    assert_eq!(
        prefs::load_store().session_links[&two_key].workspace_file,
        PathBuf::from("dir2/sub.md")
    );
    assert!(editor.sessions.iter().any(|s| {
        s.id == two_id && s.linked_workspace_file.as_deref() == Some(Path::new("dir2/sub.md"))
    }));

    // Delete renamed.md -> in-place session tab survives as DocId::Session without mirror.
    editor.chrome.active_dialog = Some(DialogKind::Delete {
        rel_path: "renamed.md".into(),
        display_name: "renamed.md".into(),
        is_dir: false,
    });
    editor.submit_dialog(&mut tx);
    assert_eq!(editor.documents.len(), 2);
    assert_eq!(
        editor.documents[0].id,
        DocId::Session {
            project: root.clone(),
            id: one_id.into()
        },
    );
    assert_eq!(
        editor.documents[0].path,
        root.join("sessions").join(one_id).join("after.md"),
    );
    assert!(editor.documents[0].mirror_path.is_none());
    assert!(!editor.store.session_links.contains_key(&one_key));
    assert!(!prefs::load_store().session_links.contains_key(&one_key));
    assert!(editor.flush_on_exit());

    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn linked_session_files_skips_finished_sessions_and_picks_newest_deterministically() {
    let _lock = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let base = std::env::temp_dir().join(format!("ghostless-linked-pick-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let root_raw = base.join("ws");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root_raw).unwrap();
    let root = root_raw.canonicalize().unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }

    fs_io::atomic_write_text(&root.join("manifest.json"), r#"{"kind":"ghostless"}"#).unwrap();
    let draft = root.join("draft.md");
    let body = "Live draft.\n";
    fs_io::atomic_write_text(&draft, body).unwrap();

    let older_id = "2026-04-01-older";
    let newer_id = "2026-04-02-newer";
    let done_id = "2026-04-03-done";
    let corrupt_id = "2026-04-04-corrupt";
    let sha_old = write_test_session_dir(
        &root,
        older_id,
        "draft.md",
        "2026-04-01T10:00:00Z",
        body,
        body,
        false,
    );
    let sha_new = write_test_session_dir(
        &root,
        newer_id,
        "draft.md",
        "2026-04-02T10:00:00Z",
        body,
        body,
        false,
    );
    let sha_done = write_test_session_dir(
        &root,
        done_id,
        "draft.md",
        "2026-04-03T10:00:00Z",
        body,
        body,
        true,
    );

    let mut store = prefs::EditorStateStore::default();
    for (id, sha) in [
        (corrupt_id, sha_old.clone()),
        (done_id, sha_done),
        (older_id, sha_old),
        (newer_id, sha_new),
    ] {
        store.session_links.insert(
            prefs::EditorStateStore::session_link_key(&root, id),
            prefs::SessionLink {
                workspace_file: draft.clone(),
                last_synced_sha256: sha,
            },
        );
    }

    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.open_workspace_with_voice(root.clone(), Some(draft.clone()), Some(root.clone()));
    assert_eq!(
        editor.active_doc_ref().unwrap().session_id.as_deref(),
        Some(newer_id)
    );

    // Finish newer_id and mark both active sessions finished -> reopening draft.md opens a fresh editable file.
    editor.apply_reply(
        WorkerReply::Finished {
            doc_id: DocId::File(draft.clone()),
            generation: 0,
            snapshot: None,
            result: Ok(crate::bridge::FinishReceipt::default()),
        },
        None,
    );
    fs_io::atomic_write_text(
        &root.join("sessions").join(newer_id).join("receipt.json"),
        "{}\n",
    )
    .unwrap();
    fs_io::atomic_write_text(
        &root.join("sessions").join(older_id).join("receipt.json"),
        "{}\n",
    )
    .unwrap();

    let finished_doc = editor.active_doc_ref().unwrap();
    assert_eq!(
        finished_doc.id,
        DocId::Session {
            project: root.clone(),
            id: newer_id.into()
        }
    );
    assert_eq!(
        finished_doc.path,
        root.join("sessions").join(newer_id).join("after.md")
    );
    assert!(finished_doc.mirror_path.is_none());
    assert!(finished_doc.finished && finished_doc.read_only);

    editor.open_workspace_file(draft.clone(), true);
    assert_eq!(editor.documents.len(), 2);
    let reopened = editor.active_doc_ref().unwrap();
    assert_eq!(reopened.id, DocId::File(draft));
    assert!(reopened.session_id.is_none());
    assert!(!reopened.read_only);
    assert!(editor.flush_on_exit());

    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn drop_writes_recovery_after_a_cancelled_quit_and_a_later_edit() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = crate::test_util::TestDir::new("quit-cancel");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let file = root.join("draft.md");
    fs::write(&file, "disk").unwrap();
    let writer: WriteRunner = Box::new(|_| Err("save unavailable".into()));
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    editor.open_workspace_file(file, true);
    editor.active_working_edit_mut().text = "first edit".into();
    editor.changed_active(&mut Tx::default());
    let recovery = editor.active_doc_ref().unwrap().recovery_path();
    editor.on_application_quit();
    assert!(recovery.is_file());
    fs::remove_file(&recovery).unwrap();
    editor.active_working_edit_mut().text = "edited after cancel".into();
    editor.changed_active(&mut Tx::default());
    drop(editor);
    let restored = fs::read_to_string(recovery).unwrap();
    assert!(restored.contains("edited after cancel"), "{restored}");
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
}

#[test]
fn back_to_back_close_saves_do_not_drop_a_tab_and_an_edit_cancels_close() {
    let (writer, started, release) = block_first_write(Box::new(|op| {
        if let WriteOp::File { content, .. } = op {
            assert!(!content.is_empty());
        }
        Ok(WriteOutcome::Local)
    }));
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
    for name in ["a.md", "b.md"] {
        editor.documents.push(DocumentBuffer::from_workspace_file(
            PathBuf::from(format!("/tmp/{name}")),
            name.into(),
            "old".into(),
            false,
            false,
            true,
            None,
        ));
    }
    editor.active_doc = Some(0);
    let mut tx = Tx::default();
    editor.active_working_edit_mut().text = "A".into();
    editor.changed_active(&mut tx);
    editor.chrome.active_dialog = Some(DialogKind::ConfirmClose {
        doc_id: editor.documents[0].id.clone(),
        title: "a.md".into(),
    });
    editor.confirm_close_save();
    started.recv_timeout(Duration::from_secs(2)).unwrap();

    editor.active_doc = Some(1);
    editor.active_working_edit_mut().text = "B".into();
    editor.changed_active(&mut tx);
    editor.chrome.active_dialog = Some(DialogKind::ConfirmClose {
        doc_id: editor.documents[1].id.clone(),
        title: "b.md".into(),
    });
    editor.confirm_close_save();
    assert!(editor.documents.iter().all(|doc| doc.close_after_save));
    release.send(()).unwrap();
    drain_until_idle(&mut editor);
    assert!(editor.documents.is_empty());

    let mut editor = Editor::empty_with_handlers(
        {
            let mut store = prefs::EditorStateStore::default();
            store.ui.autosave_workspace_files = false;
            store
        },
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    editor.documents.push(DocumentBuffer::from_workspace_file(
        "/tmp/stay.md".into(),
        "stay.md".into(),
        "old".into(),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    editor.active_working_edit_mut().text = "dirty".into();
    editor.changed_active(&mut tx);
    editor.chrome.active_dialog = Some(DialogKind::ConfirmClose {
        doc_id: editor.documents[0].id.clone(),
        title: "stay.md".into(),
    });
    editor.confirm_close_save();
    editor.active_working_edit_mut().text = "still editing".into();
    editor.changed_active(&mut tx);
    assert!(!editor.documents[0].close_after_save);
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert_eq!(editor.documents.len(), 1);
}

#[test]
fn finish_failure_survives_a_stale_save_and_recovery_keeps_save_errors() {
    let mut editor = Editor::empty_with_handlers(
        prefs::EditorStateStore::default(),
        ok_runner(),
        ok_writer(),
        noop_wake(),
    );
    let doc = DocumentBuffer::from_session(test_session(), true, None);
    let doc_id = doc.id.clone();
    editor.documents.push(doc);
    editor.active_doc = Some(0);
    editor.documents[0].generation = 2;
    editor.documents[0].finish_requested = true;
    editor.documents[0].pending_save = true;
    editor.apply_reply(
        crate::worker::WorkerReply::Finished {
            doc_id: doc_id.clone(),
            generation: 2,
            snapshot: None,
            result: Err(EditorError::AlreadyFinished(
                "This session is already finished. Start a new session to edit again.".into(),
            )),
        },
        None,
    );
    assert!(!editor.documents[0].pending_save);
    let finish_error = editor.documents[0].finish_error.clone().unwrap();
    editor.apply_reply(
        crate::worker::WorkerReply::Saved {
            doc_id: doc_id.clone(),
            generation: 1,
            sha256: "stale".into(),
            result: Ok(WriteOutcome::Local),
        },
        None,
    );
    assert_eq!(
        editor.documents[0].finish_error.as_deref(),
        Some(finish_error.as_str())
    );
    assert_eq!(
        editor.documents[0].save_error.as_deref(),
        Some(finish_error.as_str())
    );

    editor.documents[0].save_error = Some("disk failed".into());
    editor.apply_reply(
        crate::worker::WorkerReply::RecoveryWritten {
            doc_id: doc_id.clone(),
            result: Ok(WriteOutcome::Local),
        },
        None,
    );
    assert_eq!(
        editor.documents[0].save_error.as_deref(),
        Some("disk failed")
    );
    assert!(editor.documents[0].recovery_error.is_none());
    editor.apply_reply(
        crate::worker::WorkerReply::RecoveryWritten {
            doc_id,
            result: Err("recovery disk full".into()),
        },
        None,
    );
    assert_eq!(
        editor.documents[0].save_error.as_deref(),
        Some("disk failed")
    );
    assert_eq!(
        editor.documents[0].recovery_error.as_deref(),
        Some("recovery disk full")
    );
}

#[test]
fn manual_save_writes_a_dirty_user_locked_file_and_rejects_a_stale_sha() {
    let saved = Arc::new(Mutex::new(Vec::new()));
    let recorded = saved.clone();
    let writer: WriteRunner = Box::new(move |op| {
        if let WriteOp::File { content, .. } = op {
            recorded.lock().unwrap().push(content.clone());
        }
        Ok(WriteOutcome::Local)
    });
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = false;
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), writer, noop_wake());
    editor.documents.push(DocumentBuffer::from_workspace_file(
        "/tmp/locked.md".into(),
        "locked.md".into(),
        "old".into(),
        false,
        true,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    editor.documents[0].user_locked = true;
    editor.documents[0].generation = 1;
    editor.active_working_edit_mut().text = "locked save".into();
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert_eq!(*saved.lock().unwrap(), ["locked save"]);

    let root = crate::test_util::TestDir::new("sha-guard");
    let file = root.join("draft.md");
    fs::write(&file, "original\n").unwrap();
    let mut editor = Editor::empty_with_handlers(
        {
            let mut store = prefs::EditorStateStore::default();
            store.ui.autosave_workspace_files = false;
            store
        },
        ok_runner(),
        Box::new(crate::worker::execute_write_op),
        noop_wake(),
    );
    editor.open_workspace_file(file.clone(), true);
    fs::write(&file, "external\n").unwrap();
    editor.active_working_edit_mut().text = "local\n".into();
    editor.changed_active(&mut Tx::default());
    editor.save_clicked();
    drain_until_idle(&mut editor);
    assert_eq!(fs::read_to_string(&file).unwrap(), "external\n");
    assert!(
        editor.status.contains("changed outside"),
        "{}",
        editor.status
    );
}

#[test]
fn toggling_autosave_off_arms_recovery_and_workspace_switch_clears_chrome() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = crate::test_util::TestDir::new("autosave-off");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let mut store = prefs::EditorStateStore::default();
    store.ui.autosave_workspace_files = true;
    let mut editor = Editor::empty_with_handlers(store, ok_runner(), ok_writer(), noop_wake());
    editor.documents.push(DocumentBuffer::from_workspace_file(
        root.join("note.md"),
        "note.md".into(),
        "old".into(),
        false,
        false,
        true,
        None,
    ));
    editor.active_doc = Some(0);
    editor.documents[0].generation = 1;
    editor.documents[0].autosave_due = Some(50.0);
    editor.toggle_autosave();
    assert!(editor.documents[0].autosave_due.is_none());
    assert_eq!(editor.documents[0].recovery_due, Some(0.0));

    let file = root.join("clean.md");
    fs::write(&file, "clean").unwrap();
    editor.documents.clear();
    editor.active_doc = None;
    editor.open_workspace_file(file, true);
    editor.chrome.focus_mode = true;
    editor.chrome.view_settings_open = true;
    editor.chrome.active_dialog = Some(DialogKind::StartSession);
    editor.chrome.dialog_error = Some("stale".into());
    let other = root.join("other");
    fs::create_dir_all(&other).unwrap();
    editor.open_workspace_with_voice(other, None, None);
    assert!(editor.chrome.active_dialog.is_none());
    assert!(editor.chrome.dialog_error.is_none());
    assert!(!editor.chrome.focus_mode);
    assert!(!editor.chrome.view_settings_open);
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
}

#[test]
fn keep_session_then_finish_mirrors_and_reconcile_refreshes_the_sync_hash() {
    let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = crate::test_util::TestDir::new("keep-finish");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let prev_home = std::env::var_os("GHOSTLESS_HOME");
    unsafe {
        std::env::set_var("GHOSTLESS_HOME", &home);
    }
    let project = root.join("voice");
    fs::create_dir_all(&project).unwrap();
    let draft = project.join("draft.md");
    fs::write(&draft, "disk copy\n").unwrap();
    let session_id = "2026-05-01-notes";
    let body = "session copy\n";
    let before_sha = write_test_session_dir(
        &project,
        session_id,
        "draft.md",
        "2026-05-01T00:00:00Z",
        body,
        body,
        false,
    );
    let mut session = test_session();
    session.project = project.clone();
    session.id = session_id.into();
    session.original = body.into();
    session.after = body.into();
    session.before_sha256 = before_sha;
    let mut store = prefs::EditorStateStore::default();
    let key = prefs::EditorStateStore::session_link_key(&project, session_id);
    store.session_links.insert(
        key.clone(),
        prefs::SessionLink {
            workspace_file: draft.clone(),
            last_synced_sha256: "stale".into(),
        },
    );
    let mut editor = Editor::with_handlers(
        session,
        store,
        ok_runner(),
        Box::new(crate::worker::execute_write_op),
        noop_wake(),
    );
    {
        let doc = editor.active_doc_mut().unwrap();
        doc.mirror_path = Some(draft.clone());
        doc.conflict = Some(crate::documents::SessionConflict {
            disk_text: "disk copy\n".into(),
            session_text: body.into(),
        });
        doc.force_mirror = true;
    }
    editor.queue_finish();
    drain_until_idle(&mut editor);
    assert_eq!(fs::read_to_string(&draft).unwrap(), body);

    fs::write(&draft, body).unwrap();
    {
        let doc = editor.active_doc_mut().unwrap();
        doc.finished = false;
        doc.read_only = false;
        doc.mirror_path = Some(draft.clone());
        doc.edit = rune::TextEdit::new(body);
        doc.conflict = Some(crate::documents::SessionConflict {
            disk_text: body.into(),
            session_text: body.into(),
        });
        doc.saved_generation = doc.generation;
    }
    editor
        .store
        .session_links
        .get_mut(&key)
        .unwrap()
        .last_synced_sha256 = "stale".into();
    editor.reconcile_active_linked_doc();
    assert!(editor.active_doc_ref().unwrap().conflict.is_none());
    assert_eq!(
        editor.store.session_links[&key].last_synced_sha256,
        fs_io::sha256_text(body)
    );

    let clean = DocumentBuffer::from_workspace_file(
        draft.clone(),
        "draft.md".into(),
        "old tab".into(),
        false,
        false,
        true,
        None,
    );
    let mut dirty = DocumentBuffer::from_workspace_file(
        draft.clone(),
        "draft.md".into(),
        "dirty tab".into(),
        false,
        false,
        true,
        None,
    );
    dirty.generation = 1;
    let saved_id = editor.documents[0].id.clone();
    editor.documents.push(clean);
    editor.documents.push(dirty);
    editor.documents[0].mirror_path = Some(draft);
    editor.documents[0].edit = rune::TextEdit::new("mirrored body\n");
    editor.apply_reply(
        crate::worker::WorkerReply::Saved {
            doc_id: saved_id,
            generation: editor.documents[0].generation,
            sha256: fs_io::sha256_text("mirrored body\n"),
            result: Ok(WriteOutcome::Mirrored),
        },
        None,
    );
    assert_eq!(editor.documents[1].edit.text, "mirrored body\n");
    assert!(editor.documents[2].conflict.is_some());
    unsafe {
        if let Some(value) = prev_home {
            std::env::set_var("GHOSTLESS_HOME", value);
        } else {
            std::env::remove_var("GHOSTLESS_HOME");
        }
    }
}

#[test]
fn finish_after_an_inflight_save_still_mirrors_a_kept_session() {
    let root = crate::test_util::TestDir::new("finish-inflight");
    let project = root.join("voice");
    fs::create_dir_all(&project).unwrap();
    let draft = project.join("draft.md");
    fs::write(&draft, "disk copy\n").unwrap();
    let session_id = "2026-05-02-notes";
    let body = "session copy\n";
    let before_sha = write_test_session_dir(
        &project,
        session_id,
        "draft.md",
        "2026-05-02T00:00:00Z",
        body,
        body,
        false,
    );
    let mut session = test_session();
    session.project = project;
    session.id = session_id.into();
    session.original = body.into();
    session.after = body.into();
    session.before_sha256 = before_sha;
    let (writer, started, release) = block_first_write(Box::new(crate::worker::execute_write_op));
    let mut editor = Editor::with_handlers(
        session,
        prefs::EditorStateStore::default(),
        ok_runner(),
        writer,
        noop_wake(),
    );
    {
        let doc = editor.active_doc_mut().unwrap();
        doc.mirror_path = Some(draft.clone());
        doc.edit = rune::TextEdit::new("session copy\n");
        doc.generation = doc.saved_generation + 1;
    }
    editor.save_clicked();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    editor.active_doc_mut().unwrap().force_mirror = true;
    editor.queue_finish();
    release.send(()).unwrap();
    drain_until_idle(&mut editor);
    assert_eq!(fs::read_to_string(&draft).unwrap(), body);
}
