use rune::{Cx, El, Tx, col, icon, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Dialog, Divider, Kit, TextArea, TextInput,
    diff::DiffView, glyphs, shell::SplitView,
};
use crate::state::{DialogKind, Editor, ViewMode};

mod canvas;
mod find_bar;
pub(crate) mod folder_search;
pub(crate) mod quick_switcher;
mod preview;
pub(crate) use preview::{PREVIEW_BODY, reveal_find_position};
pub(crate) mod reference;
mod sidebar;
mod status_bar;
mod tabs;
mod toolbar;

#[cfg(test)]
pub(crate) const TEXTAREA_LINE_HEIGHT: f32 = 17.5;

#[cfg(test)]
pub(crate) fn working_textarea_metrics(pane_height: Option<f32>) -> (f32, usize) {
    let height = pane_height.unwrap_or(80.0).floor().max(80.0);
    let rows = ((height / TEXTAREA_LINE_HEIGHT).ceil() as usize).max(4);
    (height, rows)
}

pub(crate) fn view(editor: &Editor, cx: &mut Cx<Editor>) -> El {
    if editor.is_landing_view() {
        return crate::ui_welcome::view(editor, cx);
    }

    let kit = crate::theme::build_kit(editor.store.ui.theme, cx.system_dark());
    let mut overlays: Vec<El> = Vec::new();

    let tb = toolbar::build(editor, &kit, &mut overlays);
    let sb = sidebar::build(editor, &kit, &mut overlays);
    let insp = build_inspector(editor, cx, &kit);

    let stage = build_stage(editor, cx, &kit);
    let stage = if editor.reference.open && editor.view_mode != ViewMode::Compare {
        row().grow(1.0).w_full().min_h(0.0)
            .child(col().grow(1.0).w(0.0).min_h(0.0).child(reference::build(editor, &kit, &mut overlays)))
            .child(col().w(1.0).h_full().fill(kit.c.border_muted))
            .child(col().grow(1.0).w(0.0).min_h(0.0).child(stage))
    } else { stage };
    let stage = if editor.find.open && editor.view_mode != ViewMode::Compare && editor.active_doc_ref().is_some() {
        col().grow(1.0).w_full().min_h(0.0).child(stage)
            .child(find_bar::build(editor, &kit).pin(None, Some(8.0), Some(8.0), None))
    } else { stage };
    let mut main = col()
        .key("stage")
        .w_full()
        .h_full()
        .min_h(0.0)
        .fill(kit.c.bg_base);
    if !editor.documents.is_empty() && !editor.focus_mode {
        main = main.child(tabs::build(editor, cx, &kit));
    }
    main = main.child(stage);
    if editor.focus_mode {
        let count = editor.active_doc_ref().map(|doc| doc.word_counts().0).unwrap_or(0);
        main = main.child(row().w_full().px(12.0).center_y()
            .child(text(format!("{count} words")).font(kit.ty.small).color(kit.c.text_faint))
            .child(row().grow(1.0))
            .child(Button::new("Exit Focus (Esc / F11)").variant(ButtonVariant::GhostMuted)
                .on_press(|e: &mut Editor| e.focus_mode = false).build(&kit)));
    } else { main = main.child(status_bar::build(editor, &kit)); }

    let mut split = SplitView::new(main);
    if !editor.focus_mode {
        split = split.sidebar(&editor.sidebar_pane, |e: &mut Editor| &mut e.sidebar_pane, sb)
            .inspector(&editor.inspector_pane, |e: &mut Editor| &mut e.inspector_pane, insp);
    }
    let shell = split.toolbar(tb)
        .titlebar_inset(cx.titlebar_inset())
        .reduced_motion(cx.is_reduced_motion())
        .build(&kit);

    mount_dialogs(editor, &kit, &mut overlays);
    quick_switcher::mount(editor, &kit, &mut overlays);
    folder_search::mount(editor, &kit, &mut overlays);
    rune_kit::root(shell, overlays)
}

fn mount_search_dialog(dialog: Dialog, editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    let before = overlays.len();
    dialog.build(kit, overlays);
    // Rune-Kit appends the scrim, then the layer containing the entire dialog.
    // Set the return target on that layer so the close button and outside click
    // restore focus as well as Escape, even if the dialog opened without focus.
    if let Some(key) = editor.document_focus_key()
        && overlays.len() > before
        && let Some(layer) = overlays.pop()
    {
        overlays.push(layer.focus_return(key));
    }
}

fn build_stage(editor: &Editor, cx: &Cx<Editor>, kit: &Kit) -> El {
    let Some(doc) = editor.active_doc_ref() else {
        return col()
            .key("empty-stage")
            .grow(1.0)
            .w_full()
            .min_h(0.0)
            .center()
            .gap(10.0)
            .padding(24.0)
            .child(text("No draft open").font(kit.ty.ui_medium).color(kit.c.text_base))
            .child(
                text("Select a file from the sidebar or press Cmd/Ctrl+N to create a new draft.")
                    .font(kit.ty.ui)
                    .color(kit.c.text_muted),
            );
    };

    if editor.active_uses_rendered_preview() {
        return preview::build(&doc.edit.text, &editor.store.ui, kit, Some(&editor.find));
    }

    match editor.view_mode {
        ViewMode::Compare if editor.active_has_baseline() => col()
            .key("compare-body")
            .focusable(true)
            .role("region")
            .label("Draft comparison")
            .grow(1.0)
            .w_full()
            .min_h(0.0)
            .child(
                DiffView::new(|e: &Editor| &e.diff, |e: &mut Editor| &mut e.diff)
                    .build(editor, kit),
            ),
        _ => {
            let locked = editor.active_is_locked();
            let body = canvas::build(editor, cx, kit, locked);
            let mut pane = col()
                .key("working-pane")
                .grow(1.0)
                .w_full()
                .h_full()
                .min_h(0.0)
                .padding(18.0)
                .gap(8.0);
            if doc.read_only || doc.finished || doc.lockable {
                let banner_text = if doc.finished {
                    "Finished session — read-only archival copy"
                } else if doc.read_only {
                    "Read-only artifact — editing is disabled"
                } else if doc.user_locked {
                    "Reference file locked — unlock to edit"
                } else {
                    "Reference file — click Lock to protect from accidental edits"
                };
                let mut banner = row()
                    .key("doc-guard-banner")
                    .w_full()
                    .px(10.0)
                    .py(5.0)
                    .gap(8.0)
                    .center_y()
                    .radius(6.0)
                    .fill(kit.c.layer_01)
                    .child(icon(&glyphs::LOCK, 14.0).color(kit.c.icon_muted))
                    .child(
                        text(banner_text)
                            .font(kit.ty.small_medium)
                            .color(kit.c.text_muted)
                            .grow(1.0)
                            .shrink(),
                    );
                if doc.lockable && !doc.read_only && !doc.finished {
                    banner = banner.child(
                        Button::new(if doc.user_locked { "Unlock" } else { "Lock" })
                            .control_size(ControlSize::Small)
                            .variant(ButtonVariant::Outline)
                            .on_press(|e: &mut Editor| e.toggle_active_user_lock())
                            .build(kit),
                    );
                }
                pane = pane.child(banner);
            }
            pane.child(body)
        }
    }
}

fn build_inspector(editor: &Editor, cx: &Cx<Editor>, kit: &Kit) -> El {
    if !editor.active_is_session() {
        return col()
            .key("inspector-empty")
            .w_full()
            .h_full()
            .padding(16.0)
            .gap(8.0)
            .child(text("INSPECTOR").font(kit.ty.text_xsmall_medium).color(kit.c.text_muted))
            .child(
                text("Open or start an edit session to record writer notes here.")
                    .font(kit.ty.ui)
                    .color(kit.c.text_faint)
                    .wrap(),
            );
    }
    let locked = editor.active_is_locked();
    let max_h = cx
        .layout_size("inspector-notes")
        .map(|s| (s[1] - 64.0).max(120.0))
        .unwrap_or(220.0);
    col()
        .key("inspector-notes")
        .w_full()
        .h_full()
        .min_h(0.0)
        .padding(14.0)
        .gap(8.0)
        .child(
            text("NOTES · WHY DID YOU CHANGE IT?")
                .font(kit.ty.text_xsmall_medium)
                .color(kit.c.text_muted),
        )
        .child(
            TextArea::new(editor.active_notes_edit())
                .label("Edit notes")
                .placeholder("Optional context for your future self")
                .width(None)
                .rows(6)
                .max_height(max_h)
                .disabled(locked)
                .on_edit(|e: &mut Editor| e.active_notes_edit_mut())
                .on_change(|e: &mut Editor, tx: &mut Tx| e.changed_active(tx))
                .build(kit),
        )
        .child(row().grow(1.0))
        .child(
            text(editor.status.clone())
                .font(kit.ty.small)
                .color(kit.c.text_faint)
                .wrap(),
        )
}

fn mount_dialogs(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    let Some(dialog) = &editor.active_dialog else {
        return;
    };
    match dialog {
        DialogKind::NewFile { parent_rel } | DialogKind::NewFolder { parent_rel } => {
            let is_file = matches!(dialog, DialogKind::NewFile { .. });
            let (key, title, placeholder, action_label) = if is_file {
                ("new-file", "New File", "untitled.md", "Create File")
            } else {
                ("new-folder", "New Folder", "folder-name", "Create Folder")
            };
            let location = if parent_rel.is_empty() {
                "Location: workspace root".to_owned()
            } else {
                format!("Location: {parent_rel}/")
            };
            let mut body = col().w_full().px(16.0).pb(16.0).gap(12.0);
            body = body.child(text(location).font(kit.ty.small).color(kit.c.text_muted));
            body = body.child(
                TextInput::new(&editor.dialog_input)
                    .label(title)
                    .placeholder(placeholder)
                    .width(None)
                    .invalid(editor.dialog_error.is_some())
                    .on_edit(|e: &mut Editor| &mut e.dialog_input)
                    .on_submit(|e: &mut Editor, tx| e.submit_dialog(tx))
                    .build(kit),
            );
            if let Some(err) = &editor.dialog_error {
                body = body.child(text(err.clone()).font(kit.ty.small).color(kit.c.danger.fg).wrap());
            }
            body = body.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(row().grow(1.0))
                    .child(
                        Button::new("Cancel")
                            .variant(ButtonVariant::Outline)
                            .on_press(|e: &mut Editor| e.close_dialog())
                            .build(kit),
                    )
                    .child(
                        Button::new(action_label)
                            .variant(ButtonVariant::Submit)
                            .on_press_tx(|e: &mut Editor, tx| e.submit_dialog(tx))
                            .build(kit),
                    ),
            );
            Dialog::new(key, title)
                .open(true)
                .fit()
                .width(420.0)
                .child(body)
                .on_close(|e: &mut Editor| e.close_dialog())
                .build(kit, overlays);
        }
        DialogKind::Rename { rel_path } => {
            let mut body = col().w_full().px(16.0).pb(16.0).gap(12.0);
            body = body.child(
                text(format!("Rename: {rel_path}"))
                    .font(kit.ty.small)
                    .color(kit.c.text_muted),
            );
            body = body.child(
                TextInput::new(&editor.dialog_input)
                    .label("New name")
                    .placeholder("name.md")
                    .width(None)
                    .invalid(editor.dialog_error.is_some())
                    .on_edit(|e: &mut Editor| &mut e.dialog_input)
                    .on_submit(|e: &mut Editor, tx| e.submit_dialog(tx))
                    .build(kit),
            );
            if let Some(err) = &editor.dialog_error {
                body = body.child(text(err.clone()).font(kit.ty.small).color(kit.c.danger.fg).wrap());
            }
            body = body.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(row().grow(1.0))
                    .child(
                        Button::new("Cancel")
                            .variant(ButtonVariant::Outline)
                            .on_press(|e: &mut Editor| e.close_dialog())
                            .build(kit),
                    )
                    .child(
                        Button::new("Rename")
                            .variant(ButtonVariant::Submit)
                            .on_press_tx(|e: &mut Editor, tx| e.submit_dialog(tx))
                            .build(kit),
                    ),
            );
            Dialog::new("rename-entry", "Rename")
                .open(true)
                .fit()
                .width(420.0)
                .child(body)
                .on_close(|e: &mut Editor| e.close_dialog())
                .build(kit, overlays);
        }
        DialogKind::Delete {
            display_name,
            is_dir,
            ..
        } => {
            let kind_word = if *is_dir { "folder" } else { "file" };
            let mut body = col().w_full().px(16.0).pb(16.0).gap(12.0);
            body = body.child(
                text(format!("Delete {kind_word} \"{display_name}\"? This cannot be undone."))
                    .font(kit.ty.ui)
                    .color(kit.c.text_base)
                    .wrap(),
            );
            if let Some(err) = &editor.dialog_error {
                body = body.child(text(err.clone()).font(kit.ty.small).color(kit.c.danger.fg).wrap());
            }
            body = body.child(Divider::horizontal().build(kit));
            body = body.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(row().grow(1.0))
                    .child(
                        Button::new("Cancel")
                            .variant(ButtonVariant::Outline)
                            .on_press(|e: &mut Editor| e.close_dialog())
                            .build(kit),
                    )
                    .child(
                        Button::new("Delete")
                            .variant(ButtonVariant::Danger)
                            .on_press_tx(|e: &mut Editor, tx| e.submit_dialog(tx))
                            .build(kit),
                    ),
            );
            Dialog::new("delete-entry", "Delete")
                .open(true)
                .fit()
                .width(400.0)
                .child(body)
                .on_close(|e: &mut Editor| e.close_dialog())
                .build(kit, overlays);
        }
        DialogKind::Recover { title, .. } => {
            let mut body = col().w_full().px(16.0).pb(16.0).gap(12.0)
                .child(text(format!("An unsaved recovery copy exists for \"{title}\". Restore it to the editor (auto-save may then write it to disk), or keep the disk file and discard the recovery copy.")).font(kit.ty.ui).color(kit.c.text_base).wrap());
            if let Some(err) = &editor.dialog_error {
                body = body.child(text(err.clone()).font(kit.ty.small).color(kit.c.danger.fg).wrap());
            }
            body = body.child(row().w_full().gap(8.0)
                .child(Button::new("Keep Disk File").variant(ButtonVariant::Outline).on_press(|e: &mut Editor| e.resolve_recovery(false)).build(kit))
                .child(row().grow(1.0))
                .child(Button::new("Restore Recovery").variant(ButtonVariant::Submit).on_press(|e: &mut Editor| e.resolve_recovery(true)).build(kit)));
            Dialog::new("recover-file", "Unsaved Recovery").open(true).fit().width(480.0).child(body).on_close(|e: &mut Editor| e.close_dialog()).build(kit, overlays);
        }
        DialogKind::ConfirmClose { title, .. } => {
            let mut body = col().w_full().px(16.0).pb(16.0).gap(12.0);
            body = body.child(
                text(format!("Save changes to \"{title}\" before closing?"))
                    .font(kit.ty.ui)
                    .color(kit.c.text_base)
                    .wrap(),
            );
            if let Some(error) = &editor.dialog_error {
                body = body.child(text(error.clone()).font(kit.ty.small).color(kit.c.danger.fg).wrap());
            }
            body = body.child(Divider::horizontal().build(kit));
            body = body.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(
                        Button::new("Discard")
                            .variant(ButtonVariant::Danger)
                            .on_press_tx(|e: &mut Editor, tx| e.confirm_close_discard(tx))
                            .build(kit),
                    )
                    .child(row().grow(1.0))
                    .child(
                        Button::new("Cancel")
                            .variant(ButtonVariant::Outline)
                            .on_press(|e: &mut Editor| e.close_dialog())
                            .build(kit),
                    )
                    .child(
                        Button::new("Save")
                            .variant(ButtonVariant::Submit)
                            .on_press(|e: &mut Editor| e.confirm_close_save())
                            .build(kit),
                    ),
            );
            Dialog::new("confirm-close", "Unsaved Changes")
                .open(true)
                .fit()
                .width(420.0)
                .child(body)
                .on_close(|e: &mut Editor| e.close_dialog())
                .build(kit, overlays);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_textarea_metrics_scales_with_pane_height() {
        assert_eq!(working_textarea_metrics(None), (80.0, 5));
        assert_eq!(working_textarea_metrics(Some(40.0)), (80.0, 5));
        let (small_h, small_rows) = working_textarea_metrics(Some(320.0));
        let (large_h, large_rows) = working_textarea_metrics(Some(720.0));
        assert!(large_h > small_h);
        assert!(large_rows > small_rows);
    }
}
