use rune::{El, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Divider, Kit};
use crate::state::Editor;

pub(crate) fn build(editor: &Editor, kit: &Kit) -> El {
    let active = editor.active_doc_ref();

    let save_label = match active {
        _ if editor.status.starts_with("Could not close:")
            || editor.status.starts_with("Saved locally.")
            || editor.status.starts_with("Finished locally.") =>
        {
            editor.status.clone()
        }
        None => "Ready".to_owned(),
        Some(doc) if doc.read_only && !doc.finished => "Read-only".to_owned(),
        Some(doc) if doc.user_locked => "Locked".to_owned(),
        Some(doc) if doc.save_error.is_some() => {
            format!("Save failed: {}", doc.save_error.as_deref().unwrap_or("error"))
        }
        Some(doc) if doc.last_saved_label.is_some() => {
            doc.last_saved_label.clone().unwrap_or_default()
        }
        Some(doc) if doc.is_dirty() => "Unsaved".to_owned(),
        Some(_) => "Ready".to_owned(),
    };

    let mut bar = row()
        .key("status-bar")
        .w_full()
        .h(27.0)
        .px(12.0)
        .gap(10.0)
        .center_y()
        .fill(kit.c.bg_base)
        .child(
            text(save_label)
                .font(kit.ty.small)
                .color(kit.c.text_muted)
                .truncate()
                .shrink(),
        );

    if let Some(doc) = active
        && doc.is_session()
    {
        let sess_label = if doc.finished {
            "Session: finished"
        } else {
            "Session: editing"
        };
        bar = bar.child(
            text(sess_label)
                .font(kit.ty.small_medium)
                .color(kit.c.text_faint),
        );
    }

    bar = bar.child(row().grow(1.0));

    if let Some(doc) = active {
        let (total, selected) = doc.word_counts();
        let word_text = match selected {
            Some(sel) => format!("{sel} / {total} words"),
            None => format!("{total} words"),
        };
        let read_text = format!("~{} min read", doc.reading_minutes());
        bar = bar
            .child(
                text(word_text)
                    .font(kit.ty.small)
                    .color(kit.c.text_muted),
            )
            .child(
                text(read_text)
                    .font(kit.ty.small)
                    .color(kit.c.text_faint),
            );
    }

    if let Some(voice) = &editor.voice_project {
        let name = voice
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice");
        bar = bar.child(
            Button::new(format!("Voice: {name}"))
                .control_size(ControlSize::Small)
                .variant(ButtonVariant::GhostMuted)
                .help("Toggle inspector")
                .on_press(|e: &mut Editor| e.toggle_inspector())
                .build(kit),
        );
    }

    let autosave_text = if editor.store.ui.autosave_workspace_files {
        "Auto-save: On"
    } else {
        "Auto-save: Off"
    };
    bar = bar.child(
        Button::new(autosave_text)
            .control_size(ControlSize::Small)
            .variant(ButtonVariant::GhostMuted)
            .help("Toggle automatic saving for workspace files")
            .on_press(|e: &mut Editor| e.toggle_autosave())
            .build(kit),
    );

    col()
        .key("status-footer")
        .w_full()
        .h(28.0)
        .child(Divider::horizontal().build(kit))
        .child(bar)
}
