use rune::{Tx, text};
use rune_kit::{
    Button, ButtonVariant, IconButton, IconButtonVariant, Kit, SegmentedControl, State,
    shell::{INSPECTOR, Toolbar},
};
use crate::state::{Editor, ViewMode};

pub(crate) fn build(editor: &Editor, kit: &Kit) -> Toolbar {
    let ws_name = editor
        .workspace
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .or_else(|| {
            editor
                .voice_project
                .as_deref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
        })
        .unwrap_or("ghostless");

    let breadcrumb = match editor.active_doc_ref() {
        Some(doc) => format!("{ws_name} / {}", doc.title),
        None => ws_name.to_owned(),
    };

    let has_baseline = editor.active_has_baseline();
    let mut modes = SegmentedControl::new(
        &["Write", "Preview", "Compare"],
        Some(editor.view_mode.as_index()),
    )
    .width(Some(240.0))
    .label("Editor mode")
    .on_pick(|e: &mut Editor, index, _tx: &mut Tx| {
        let mode = ViewMode::from_index(index);
        if mode == ViewMode::Compare && !e.active_has_baseline() {
            return;
        }
        e.view_mode = mode;
        if mode == ViewMode::Compare {
            e.refresh_diff();
        }
    });
    if !has_baseline {
        modes = modes.segment_state(2, State::Disabled);
    }

    let mut tb = Toolbar::new()
        .leading(
            text(breadcrumb)
                .font(kit.ty.ui_medium)
                .color(kit.c.text_base)
                .truncate()
                .shrink(),
        )
        .center(modes.build(kit));

    if editor.active_is_session() {
        let locked = editor.active_is_locked();
        tb = tb
            .trailing(
                Button::new("Save")
                    .variant(ButtonVariant::Outline)
                    .disabled(locked)
                    .on_press(|e: &mut Editor| e.save_clicked())
                    .build(kit),
            )
            .trailing(
                Button::new("Finish")
                    .variant(ButtonVariant::Submit)
                    .disabled(locked)
                    .on_press(|e: &mut Editor| e.finish())
                    .build(kit),
            );
    }

    let shortcut = if cfg!(target_os = "macos") { "cmd+j" } else { "ctrl+j" };
    tb.trailing(
        IconButton::new(&INSPECTOR)
            .variant(IconButtonVariant::GhostMuted)
            .label("Toggle inspector")
            .help("Toggle inspector")
            .shortcut(shortcut)
            .expanded(editor.inspector_pane.is_open())
            .on_press(|e: &mut Editor| e.toggle_inspector())
            .build(kit),
    )
}
