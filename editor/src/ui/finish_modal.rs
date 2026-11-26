use super::mount_search_dialog;
use crate::state::Editor;
use rune::{El, col, row, text};
use rune_kit::{Button, ButtonVariant, Checkbox, Dialog, Kit, TextArea, TextInput};

pub(super) fn mount(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    let Some(modal) = editor.chrome.finish.as_ref() else {
        return;
    };
    let busy = modal.submitted;
    let disk_changed = editor
        .active_doc_ref()
        .is_some_and(|doc| doc.external_conflict.is_some());
    let mut body = col()
        .w_full()
        .gap(9.0)
        .px(16.0)
        .pb(16.0)
        .child(caption(&modal.preview, kit))
        .child(caption(
            format!("{} · {}", modal.provenance, modal.tier()),
            kit,
        ))
        .child(caption(&modal.sync_status, kit));
    if modal.standalone {
        body = body.child(
            button("Save Final Copy As…", ButtonVariant::Outline)
                .disabled(busy)
                .on_press_tx(|editor: &mut Editor, tx| editor.save_final_copy(tx))
                .build(kit),
        );
    }
    if modal.identical {
        body = body.child(caption(
            "No edits made yet. Make at least one edit before finishing.",
            kit,
        ));
    }
    if !modal.placeholders.is_empty() {
        body = body.child(
            row()
                .gap(8.0)
                .center_y()
                .child(caption(
                    format!(
                        "⚠ {} placeholder(s) still in draft",
                        modal.placeholders.len()
                    ),
                    kit,
                ))
                .child(
                    button("Jump to placeholder", ButtonVariant::Outline)
                        .disabled(busy)
                        .on_press_tx(|editor: &mut Editor, tx| {
                            editor.jump_to_finish_placeholder(tx)
                        })
                        .build(kit),
                ),
        );
    }
    body = body
        .child(
            TextArea::new(&modal.notes)
                .label("Edit notes")
                .rows(3)
                .disabled(busy)
                .on_edit(|editor: &mut Editor| match editor.chrome.finish.as_mut() {
                    Some(modal) => &mut modal.notes,
                    None => &mut editor.fallback_edit,
                })
                .build(kit)
                .key("finish-notes"),
        )
        .child(
            TextInput::new(&modal.check)
                .label("Optional yes/no check")
                .placeholder("e.g. The opener does not copy \"Level 9:\"")
                .width(None)
                .disabled(busy)
                .on_edit(|editor: &mut Editor| match editor.chrome.finish.as_mut() {
                    Some(modal) => &mut modal.check,
                    None => &mut editor.fallback_edit,
                })
                .build(kit)
                .key("finish-check"),
        )
        .child(caption(
            "Quote the phrase you cut or added. The check stays pending until confirmed.",
            kit,
        ));
    if disk_changed {
        body = body.child(caption(
            "The draft changed on disk. Close this dialog to review the change.",
            kit,
        ));
    }
    if let Some(error) = &modal.error {
        body = body.child(
            text(error.to_string())
                .font(kit.ty.ui)
                .color(kit.c.text_base)
                .wrap()
                .key("finish-error"),
        );
    }
    body = body.child(
        Checkbox::new(modal.rebuild)
            .label("Rebuild voice guide")
            .disabled(busy)
            .on_toggle(|editor: &mut Editor| {
                if let Some(modal) = &mut editor.chrome.finish {
                    modal.rebuild = !modal.rebuild;
                }
            })
            .build(kit),
    );
    if modal.ai_first {
        body = body
            .child(
                Checkbox::new(modal.promote)
                    .label("Promote to gold")
                    .disabled(busy)
                    .on_toggle(|editor: &mut Editor| {
                        if let Some(modal) = &mut editor.chrome.finish {
                            modal.promote = !modal.promote;
                        }
                    })
                    .build(kit),
            )
            .child(caption(
                "AI-first finals stay in the edited tier unless you promote them.",
                kit,
            ));
    }
    body = body.child(
        row()
            .gap(8.0)
            .child(row().grow(1.0))
            .child(
                button("Cancel", ButtonVariant::Outline)
                    .disabled(busy)
                    .on_press(Editor::dismiss_finish)
                    .build(kit),
            )
            .child(
                button(
                    if busy {
                        "Finishing…"
                    } else {
                        "Finish & Learn"
                    },
                    ButtonVariant::Submit,
                )
                .disabled(!modal.can_confirm() || disk_changed)
                .on_press(Editor::confirm_finish)
                .build(kit)
                .key("confirm-finish"),
            ),
    );

    mount_search_dialog(
        Dialog::new("finish-session", "Finish Edit Session")
            .width(540.0)
            .fit()
            .open(true)
            .click_outside(!busy)
            .on_close(Editor::dismiss_finish)
            .child(body),
        editor,
        kit,
        overlays,
    );
}

fn caption(value: impl Into<String>, kit: &Kit) -> El {
    text(value)
        .font(kit.ty.small)
        .color(kit.c.text_muted)
        .wrap()
}

fn button(label: &str, variant: ButtonVariant) -> Button {
    Button::new(label).variant(variant)
}
