use super::mount_search_dialog;
use crate::state::{Editor, filtered_commands};
use rune::{El, Key, ScrollSpec, col, row, text};
use rune_kit::{Dialog, Keybind, Kit, TextInput};

pub(super) fn mount(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    let palette = &editor.chrome.palette;
    if !palette.open {
        return;
    }
    let query = TextInput::new(&palette.query)
        .label("Search commands")
        .placeholder("Type a command")
        .width(None)
        .on_edit(|editor: &mut Editor| &mut editor.chrome.palette.query)
        .on_change(|editor: &mut Editor, tx| {
            editor.select_first_command();
            reveal_selection(editor, tx);
        })
        .on_submit(|editor: &mut Editor, tx| {
            if let Some(index) = editor.chrome.palette.selected {
                editor.choose_command(index, tx);
            }
        })
        .on_key(|editor: &mut Editor, key, tx| match key.key {
            Key::Escape => {
                editor.close_palette();
                editor.focus_document(tx);
                true
            }
            Key::Up | Key::Down => {
                editor.step_palette(key.key == Key::Down);
                reveal_selection(editor, tx);
                true
            }
            _ => false,
        })
        .build(kit)
        .key("command-query");

    let matches = filtered_commands(&palette.query.text);
    let mut results = col().gap(2.0).w_full();
    if matches.is_empty() {
        results = results.child(
            text("No matching commands")
                .font(kit.ty.ui)
                .color(kit.c.text_muted),
        );
    }
    for (index, command) in matches.into_iter().enumerate() {
        let enabled = command.enabled(editor);
        let selected = enabled && palette.selected == Some(index);
        let color = if enabled {
            kit.c.text_base
        } else {
            kit.c.text_faint
        };
        let mut item = row()
            .key(format!("command-{index}"))
            .w_full()
            .px(8.0)
            .py(6.0)
            .gap(8.0)
            .center_y()
            .radius(4.0)
            .role("button")
            .label(command.name)
            .disabled(!enabled)
            .child(
                text(command.name)
                    .font(kit.ty.ui)
                    .color(color)
                    .grow(1.0)
                    .truncate(),
            );
        if selected {
            item = item.fill(kit.c.layer_02);
        }
        if !command.keys.is_empty() {
            item = item.child(Keybind::new(command.keys).build(kit));
        }
        if enabled {
            item = item
                .hit()
                .on_press_tx(move |editor: &mut Editor, tx| editor.choose_command(index, tx));
        }
        results = results.child(item);
    }
    let body = col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(16.0)
        .pb(16.0)
        .gap(8.0)
        .child(query)
        .child(
            col()
                .key("command-results")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(results),
        );
    mount_search_dialog(
        Dialog::new("command-palette", "Command Palette")
            .width(560.0)
            .open(true)
            .child(body)
            .on_close(Editor::close_palette),
        editor,
        kit,
        overlays,
    );
}

fn reveal_selection(editor: &Editor, tx: &mut rune::Tx) {
    if let Some(index) = editor.chrome.palette.selected {
        tx.scroll_into_view_in("command-results", format!("command-{index}"));
    }
}
