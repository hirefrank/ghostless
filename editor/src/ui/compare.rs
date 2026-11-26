//! Compare header: mode, word delta, and undoable hunk reverts.

use rune::{El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, SegmentedControl, diff::DiffView};

use crate::{documents::count_words, hunks, state::Editor};

pub(crate) fn build(editor: &Editor, kit: &Kit) -> El {
    let Some(original) = editor.compare_left_text().map(str::to_owned) else {
        return col().key("compare-body");
    };
    let Some(current) = editor.compare_right_text() else {
        return col().key("compare-body");
    };
    let heading = if editor.is_external_compare() {
        if editor
            .active_doc_ref()
            .is_some_and(|doc| doc.external_conflict.is_some())
        {
            "Your edits vs disk"
        } else {
            "Previous draft vs disk"
        }
    } else if editor.is_conflict_compare() {
        "Disk vs session"
    } else {
        "Baseline vs draft"
    };
    let hunks = hunks::diff_hunks(&original, current);
    let before_words = count_words(&original);
    let after_words = count_words(current);
    let delta = after_words as i64 - before_words as i64;
    let signed = if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    };
    let summary = format!(
        "{heading} · Words: {before_words} → {after_words} ({signed}) · Changed hunks: {}",
        hunks.len()
    );
    let mode = if editor.compare_unified() { 1 } else { 0 };
    let editable = editor.can_revert();
    let show_revert = !editor.is_conflict_compare() && !editor.is_external_compare();
    let mut column = col().gap(8.0).grow(1.0).w_full().min_h(0.0).child(
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .child(
                SegmentedControl::new(&["Split", "Unified"], Some(mode))
                    .width(Some(180.0))
                    .label("Diff layout")
                    .on_pick(|editor: &mut Editor, index, _tx: &mut Tx| {
                        editor.set_compare_unified(index == 1);
                    })
                    .build(kit),
            )
            .child(
                text(summary)
                    .font(kit.ty.small)
                    .color(kit.c.text_muted)
                    .grow(1.0)
                    .shrink(),
            ),
    );
    if show_revert {
        column = column.child(
            row().w_full().child(
                Button::new("Revert to Original")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .disabled(!editable)
                    .on_press(|editor: &mut Editor| editor.confirm_revert())
                    .build(kit)
                    .key("revert-original"),
            ),
        );
    }
    if !hunks.is_empty() {
        let mut list = col().key("changed-hunks").gap(4.0).w_full();
        list = list.child(
            text("Changed hunks")
                .font(kit.ty.text_xsmall_medium)
                .color(kit.c.text_muted),
        );
        for (index, hunk) in hunks.iter().enumerate() {
            let removed = preview(&original, hunk.before_bytes.clone());
            let added = preview(current, hunk.after_bytes.clone());
            let mut line = row().w_full().center_y().gap(8.0).child(
                text(format!("− {removed} / + {added}"))
                    .font(kit.ty.small)
                    .color(kit.c.text_base)
                    .grow(1.0)
                    .shrink(),
            );
            if show_revert {
                line = line.child(
                    Button::new("Revert hunk")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .disabled(!editable)
                        .on_press_tx(move |editor: &mut Editor, tx| {
                            editor.revert_hunk(index, tx);
                        })
                        .build(kit)
                        .key(format!("revert-hunk-{index}")),
                );
            }
            list = list.child(line);
        }
        column = column.child(list);
    }
    column.child(
        col()
            .key("compare-body")
            .focusable(true)
            .role("region")
            .label("Draft comparison")
            .grow(1.0)
            .w_full()
            .min_h(0.0)
            .child(
                DiffView::new(
                    |editor: &Editor| &editor.diff,
                    |editor: &mut Editor| &mut editor.diff,
                )
                .build(editor, kit),
            ),
    )
}

fn preview(text: &str, range: std::ops::Range<usize>) -> String {
    let slice = text.get(range).unwrap_or("");
    let line = slice.lines().next().unwrap_or("").trim();
    let shortened = line.chars().take(42).collect::<String>();
    if line.chars().count() > 42 {
        format!("{shortened}…")
    } else {
        shortened
    }
}
