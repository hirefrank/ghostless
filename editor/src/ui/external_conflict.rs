use crate::state::Editor;
use rune::{El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

pub(super) fn wrap(editor: &Editor, kit: &Kit, body: El) -> El {
    let Some(doc) = editor
        .active_doc_ref()
        .filter(|doc| doc.external_conflict.is_some())
    else {
        return body;
    };
    let button = |label, action: fn(&mut Editor, &mut Tx), disabled| {
        Button::new(label)
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(disabled)
            .on_press_tx(action)
            .build(kit)
    };
    let locked = editor.active_is_locked();
    let banner = col()
        .key("external-conflict")
        .w_full()
        .padding(10.0)
        .gap(6.0)
        .fill(kit.c.layer_01)
        .child(
            text(format!(
                "{} was modified on disk while you have unsaved changes.",
                doc.title
            ))
            .font(kit.ty.small_medium)
            .color(kit.c.text_base)
            .wrap(),
        )
        .child(
            row()
                .gap(6.0)
                .flex_wrap()
                .child(button("Keep Mine", Editor::keep_external_mine, locked))
                .child(button(
                    "Reload from Disk",
                    Editor::reload_external_disk,
                    locked,
                ))
                .child(button("Compare", Editor::compare_active_external, false)),
        );
    col()
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .child(banner)
        .child(body)
}
