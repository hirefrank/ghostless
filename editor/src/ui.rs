use rune::{Cx, El, ScrollSpec, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea, diff::DiffView};
use crate::state::Editor;

pub(crate) const TEXTAREA_LINE_HEIGHT: f32 = 17.5;
pub(crate) fn working_textarea_metrics(pane_height: Option<f32>) -> (f32, usize) {
    let height = pane_height.unwrap_or(80.0).floor().max(80.0);
    let rows = ((height / TEXTAREA_LINE_HEIGHT).ceil() as usize).max(4);
    (height, rows)
}

pub(crate) fn view(editor: &Editor, cx: &mut Cx<Editor>) -> El {
    let kit = Kit::neutral(cx.system_dark());
    let locked = editor.is_locked();
    let modes = SegmentedControl::new(&["Edit", "Diff"], Some(usize::from(editor.show_diff)))
        .width(Some(140.0))
        .on_pick(|editor: &mut Editor, index, _: &mut Tx| {
            editor.show_diff = index == 1;
            if editor.show_diff {
                editor.refresh_diff();
            }
        });
    let top = row().key("toolbar").h(58.0).w_full().px(18.0).gap(12.0)
        .items_center().fill(kit.c.layer_01)
        .child(text("ghostless").font(kit.ty.ui_medium).color(kit.c.text_base))
        .child(text(format!("/ {}", editor.session.source_name)).font(kit.ty.ui).color(kit.c.text_muted))
        .child(row().grow(1.0)).child(modes.build(&kit))
        .child(Button::new("Save").variant(ButtonVariant::Outline)
            .disabled(locked).on_press(|editor: &mut Editor| editor.save_clicked()).build(&kit))
        .child(Button::new("Finish").variant(ButtonVariant::Submit)
            .disabled(locked).on_press(|editor: &mut Editor| editor.finish()).build(&kit));
    let content = if editor.show_diff {
        DiffView::new(|editor: &Editor| &editor.diff, |editor: &mut Editor| &mut editor.diff)
            .build(editor, &kit)
    } else {
        let (working_height, working_rows) =
            working_textarea_metrics(cx.layout_size("original-body").map(|size| size[1]));
        let original = col().key("original-pane").grow(1.0).h_full().min_h(0.0).gap(12.0)
            .padding(18.0).fill(kit.c.bg_base)
            .child(text("ORIGINAL · READ ONLY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(col().key("original-body").grow(1.0).w_full().min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(text(editor.session.original.clone()).font(kit.ty.body)
                    .color(kit.c.text_base).wrap().selectable(true)));
        let working = col().key("working-pane").grow(1.0).h_full().min_h(0.0).gap(12.0)
            .padding(18.0).fill(kit.c.layer_01)
            .child(text("WORKING COPY").font(kit.ty.ui_medium).color(kit.c.text_muted))
            .child(col().key("working-body").grow(1.0).w_full().min_h(0.0)
                .child(TextArea::new(&editor.after).label("Working draft")
                    .width(None).rows(working_rows).max_height(working_height)
                    .disabled(locked)
                    .on_edit(|editor: &mut Editor| &mut editor.after)
                    .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed(tx))
                    .build(&kit)));
        row().key("edit-panes").grow(1.0).w_full().min_h(0.0).child(original).child(working)
    };
    let notes = col().key("notes-pane").h(142.0).w_full().padding(14.0)
        .gap(7.0).fill(kit.c.layer_01)
        .child(text("NOTES · WHY DID YOU CHANGE IT?").font(kit.ty.ui_medium).color(kit.c.text_muted))
        .child(TextArea::new(&editor.notes).label("Edit notes")
            .placeholder("Optional context for your future self")
            .width(None).rows(3).disabled(locked)
            .on_edit(|editor: &mut Editor| &mut editor.notes)
            .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed(tx))
            .build(&kit));
    let footer = row().h(32.0).w_full().px(18.0).items_center()
        .fill(kit.c.bg_base)
        .child(text(editor.status.clone()).font(kit.ty.ui).color(kit.c.text_muted))
        .child(row().grow(1.0))
        .child(text("Ctrl/Cmd+S save  ·  Ctrl/Cmd+D diff").font(kit.ty.ui).color(kit.c.text_faint));
    col().w_full().h_full().fill(kit.c.bg_base)
        .child(top).child(content).child(notes).child(footer)
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
