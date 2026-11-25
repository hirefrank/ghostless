use crate::state::{Editor, ViewMode};
use rune::{El, Tx, col, row, text};
use rune_kit::{
    Button, ButtonVariant, IconButton, IconButtonVariant, Kit, Popover, SegmentedControl, State,
    Switch,
    shell::{INSPECTOR, Toolbar},
};

pub(crate) fn build(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> Toolbar {
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
    .on_pick(|e: &mut Editor, index, tx: &mut Tx| {
        e.set_view_mode(ViewMode::from_index(index), tx);
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

    if editor.can_track_session() {
        tb = tb.trailing(
            Button::new("Track Session")
                .variant(ButtonVariant::Outline)
                .on_press(|editor: &mut Editor| editor.open_track_session())
                .build(kit)
                .key("track-session"),
        );
    }
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

    tb = tb.leading(view_settings(editor, kit, overlays));
    let shortcut = if cfg!(target_os = "macos") {
        "cmd+j"
    } else {
        "ctrl+j"
    };
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

fn view_settings(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> El {
    use crate::prefs::{FontChoice, MeasurePreset, ThemeMode};
    let mut body = col().gap(8.0).w_full();
    body = body.child(text("Theme").font(kit.ty.small));
    let mut themes = row().gap(4.0);
    for (label, mode) in [
        ("System", ThemeMode::System),
        ("Light", ThemeMode::Light),
        ("Dark", ThemeMode::Dark),
        ("Paper", ThemeMode::Paper),
    ] {
        themes = themes.child(
            Button::new(label)
                .on_press(move |e: &mut Editor| {
                    e.store.ui.theme = mode;
                    let _ = crate::prefs::save_store(&e.store);
                })
                .build(kit),
        );
    }
    body = body.child(themes).child(text("Font").font(kit.ty.small));
    let mut fonts = row().gap(4.0);
    for (label, font) in [
        ("Sans", FontChoice::Sans),
        ("Serif", FontChoice::Serif),
        ("Mono", FontChoice::Mono),
    ] {
        fonts = fonts.child(
            Button::new(label)
                .on_press_tx(move |e: &mut Editor, tx: &mut Tx| {
                    e.store.ui.font_family = font;
                    tx.set_font_families(
                        if font == FontChoice::Serif {
                            "Georgia, serif"
                        } else {
                            "Inter, sans-serif"
                        },
                        "IBM Plex Mono, monospace",
                    );
                    let _ = crate::prefs::save_store(&e.store);
                })
                .build(kit),
        );
    }
    body = body
        .child(fonts)
        .child(
            row()
                .gap(8.0)
                .child(
                    Button::new("−")
                        .on_press(|e: &mut Editor| {
                            e.store.ui.font_size = (e.store.ui.font_size - 1.0).clamp(13.0, 24.0);
                            let _ = crate::prefs::save_store(&e.store);
                        })
                        .build(kit),
                )
                .child(text(format!("{:.1}", editor.store.ui.font_size)).font(kit.ty.ui))
                .child(
                    Button::new("+")
                        .on_press(|e: &mut Editor| {
                            e.store.ui.font_size = (e.store.ui.font_size + 1.0).clamp(13.0, 24.0);
                            let _ = crate::prefs::save_store(&e.store);
                        })
                        .build(kit),
                ),
        )
        .child(text("Column width").font(kit.ty.small));
    let mut widths = row().gap(4.0);
    for (label, width) in [
        ("Narrow", MeasurePreset::Narrow),
        ("Medium", MeasurePreset::Medium),
        ("Wide", MeasurePreset::Wide),
        ("Full", MeasurePreset::Full),
    ] {
        widths = widths.child(
            Button::new(label)
                .on_press(move |e: &mut Editor| {
                    e.store.ui.measure = width;
                    let _ = crate::prefs::save_store(&e.store);
                })
                .build(kit),
        );
    }
    body = body
        .child(widths)
        .child(
            Switch::new(editor.store.ui.autosave_workspace_files)
                .label("Auto-save files")
                .on_toggle(|e: &mut Editor| {
                    e.store.ui.autosave_workspace_files = !e.store.ui.autosave_workspace_files;
                    let _ = crate::prefs::save_store(&e.store);
                })
                .build(kit),
        )
        .child(
            Switch::new(editor.store.ui.spellcheck)
                .label("Spellcheck")
                .on_toggle(|e: &mut Editor| {
                    e.store.ui.spellcheck = !e.store.ui.spellcheck;
                    let _ = crate::prefs::save_store(&e.store);
                })
                .build(kit),
        );
    Popover::new("view-settings")
        .label("View settings")
        .open(editor.view_settings_open)
        .trigger(
            Button::new("Aa")
                .on_press(|e: &mut Editor| e.view_settings_open = !e.view_settings_open)
                .build(kit),
        )
        .child(body)
        .on_close(|e: &mut Editor| e.view_settings_open = false)
        .build(kit, overlays)
}
