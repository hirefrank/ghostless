use rune::{col, row, text, El, Key, ScrollSpec, TextEdit, Tx};
use rune_kit::{Dialog, Kit, TextInput};
use crate::{quick_open::{fuzzy_score, QuickItem}, state::Editor};

#[derive(Default)]
pub(crate) struct QuickState {
    pub open: bool,
    pub query: TextEdit,
    pub selected: usize,
    pub items: Vec<QuickItem>,
}
pub(crate) fn filtered(state: &QuickState) -> Vec<QuickItem> {
    if state.query.text.is_empty() { return state.items.clone(); }
    let mut items: Vec<_> = state.items.iter().filter_map(|item| fuzzy_score(&state.query.text, &format!("{} {}", item.subtitle, item.title)).map(|score| (score, item.clone()))).collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    items.into_iter().map(|(_, item)| item).collect()
}
pub(crate) fn mount(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    if !editor.quick.open { return; }
    let items = filtered(&editor.quick);
    let query = TextInput::new(&editor.quick.query).label("Go to file or session").placeholder("Search files and sessions").width(None)
        .on_edit(|e: &mut Editor| &mut e.quick.query)
        .on_change(|e: &mut Editor, _| e.quick.selected = 0)
        .on_submit(|e: &mut Editor, tx| e.choose_quick(e.quick.selected, tx))
        .on_key(|e: &mut Editor, key, tx: &mut Tx| match key.key {
            Key::Escape => { e.quick.open = false; e.focus_document(tx); true },
            Key::Down => { let len = filtered(&e.quick).len(); if len > 0 { e.quick.selected = (e.quick.selected + 1) % len; } true },
            Key::Up => { let len = filtered(&e.quick).len(); if len > 0 { e.quick.selected = (e.quick.selected + len - 1) % len; } true },
            _ => false,
        }).build(kit).key("quick-query");
    let mut list = col().w_full().gap(2.0);
    for (index, item) in items.iter().enumerate() {
        let selected = index == editor.quick.selected;
        let row = row().w_full().px(10.0).py(7.0).gap(8.0).center_y().radius(4.0)
            .fill(if selected { kit.c.layer_02 } else { rune::Color::TRANSPARENT })
            .child(text(item.title.clone()).font(kit.ty.ui).color(kit.c.text_base).grow(1.0).truncate())
            .child(text(item.subtitle.clone()).font(kit.ty.small).color(kit.c.text_muted).truncate())
            .hit().on_press_tx(move |e: &mut Editor, tx| e.choose_quick(index, tx));
        list = list.child(row);
    }
    let body = col().w_full().grow(1.0).min_h(0.0).px(16.0).pb(16.0).gap(8.0).child(query)
        .child(col().grow(1.0).w_full().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list));
    let dialog = Dialog::new("quick-switcher", "Go to File or Session")
        .width(560.0).open(true).child(body)
        .on_close(|e: &mut Editor| e.quick.open = false);
    super::mount_search_dialog(dialog, editor, kit, overlays);
}
