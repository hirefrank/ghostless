use crate::state::Editor;
use rune::{El, Key, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};

pub(crate) fn build(editor: &Editor, kit: &Kit) -> El {
    let count = match editor.find.current {
        Some(index) => format!("{} / {}", index + 1, editor.find.matches.len()),
        None if !editor.find.matches.is_empty() => format!("{} matches", editor.find.matches.len()),
        None => "No matches".to_owned(),
    };
    let input = TextInput::new(&editor.find.query)
        .label("Find")
        .placeholder("Find in file")
        .width(Some(180.0))
        .on_edit(|e: &mut Editor| &mut e.find.query)
        .on_change(|e: &mut Editor, tx| e.update_find_query(tx))
        .on_submit(|e: &mut Editor, tx| e.step_find(true, tx))
        .on_key(|e: &mut Editor, key, tx: &mut Tx| {
            if key.key == Key::Escape {
                e.close_find(tx);
                true
            } else if key.key == Key::Enter && key.mods.shift {
                e.step_find(false, tx);
                true
            } else {
                false
            }
        })
        .build(kit)
        .key("find-query");
    let mut bar = col()
        .key("find-bar")
        .padding(8.0)
        .gap(6.0)
        .radius(8.0)
        .fill(kit.c.layer_01)
        .child(
            row()
                .gap(4.0)
                .center_y()
                .child(input)
                .child(text(count).font(kit.ty.small).color(kit.c.text_muted))
                .child(
                    Button::new("Aa")
                        .variant(if editor.find.case_sensitive {
                            ButtonVariant::Outline
                        } else {
                            ButtonVariant::GhostMuted
                        })
                        .on_press_tx(|e: &mut Editor, tx| {
                            e.find.case_sensitive = !e.find.case_sensitive;
                            e.update_find_query(tx);
                        })
                        .build(kit),
                )
                .child(
                    Button::new("\\b")
                        .variant(if editor.find.whole_word {
                            ButtonVariant::Outline
                        } else {
                            ButtonVariant::GhostMuted
                        })
                        .on_press_tx(|e: &mut Editor, tx| {
                            e.find.whole_word = !e.find.whole_word;
                            e.update_find_query(tx);
                        })
                        .build(kit),
                )
                .child(
                    Button::new("↑")
                        .on_press_tx(|e: &mut Editor, tx| e.step_find(false, tx))
                        .build(kit),
                )
                .child(
                    Button::new("↓")
                        .on_press_tx(|e: &mut Editor, tx| e.step_find(true, tx))
                        .build(kit),
                )
                .child(
                    Button::new("Replace")
                        .on_press(|e: &mut Editor| e.find.show_replace = !e.find.show_replace)
                        .build(kit),
                )
                .child(
                    Button::new("×")
                        .on_press_tx(|e: &mut Editor, tx| e.close_find(tx))
                        .build(kit),
                ),
        );
    if editor.find.show_replace {
        bar = bar.child(
            row()
                .gap(4.0)
                .center_y()
                .child(
                    TextInput::new(&editor.find.replacement)
                        .label("Replacement")
                        .placeholder("Replace with")
                        .width(Some(180.0))
                        .on_edit(|e: &mut Editor| &mut e.find.replacement)
                        .on_key(|e: &mut Editor, key, tx| {
                            if key.key == Key::Escape {
                                e.close_find(tx);
                                true
                            } else {
                                false
                            }
                        })
                        .build(kit),
                )
                .child(
                    Button::new("Replace")
                        .disabled(editor.active_is_locked() || editor.find.current.is_none())
                        .on_press_tx(|e: &mut Editor, tx| e.replace_find(false, tx))
                        .build(kit),
                )
                .child(
                    Button::new("Replace All")
                        .disabled(editor.active_is_locked() || editor.find.matches.is_empty())
                        .on_press_tx(|e: &mut Editor, tx| e.replace_find(true, tx))
                        .build(kit),
                ),
        );
    }
    if let Some(notice) = &editor.find.notice {
        bar = bar.child(
            text(notice.clone())
                .font(kit.ty.small)
                .color(kit.c.text_muted),
        );
    }
    bar
}
