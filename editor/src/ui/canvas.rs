use rune::{col, text_input, Cx, El, ScrollSpec, Tx};
use rune_kit::Kit;
use crate::{state::Editor, theme};

pub(crate) fn build(editor: &Editor, cx: &Cx<Editor>, kit: &Kit, locked: bool) -> El {
    let height = cx.layout_size("working-body").map(|s| s[1]).unwrap_or(80.0).max(80.0);
    let mut field = text_input(editor.active_working_edit(), theme::prose_text_style(&editor.store.ui))
        .key("prose-input").label("Working draft").w_full().min_h((height - 32.0).max(80.0))
        .color(kit.c.text_base).caret(kit.c.text_base, kit.selection())
        .placeholder("Start writing…", kit.c.text_faint).spellcheck(editor.store.ui.spellcheck)
        .disabled(locked);
    if !locked {
        field = field.on_edit(|e: &mut Editor| e.active_working_edit_mut())
            .on_input_key(|e: &mut Editor, key, tx: &mut Tx| {
                let handled = crate::markdown_edit::handle(e.active_working_edit_mut(), key);
                if handled { e.changed_active(tx); }
                handled
            })
            .on_input_change(|e: &mut Editor, tx: &mut Tx| e.changed_active(tx))
            .on_submit(|e: &mut Editor, tx: &mut Tx| { e.active_working_edit_mut().insert("\n"); e.changed_active(tx); });
    }
    let mut inner = col().w_full().child(field);
    if let Some(width) = theme::measure_max_width(editor.store.ui.measure) { inner = inner.max_w(width); }
    col().key("working-body").grow(1.0).w_full().min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).items_center().px(32.0).py(16.0).child(inner)
}
