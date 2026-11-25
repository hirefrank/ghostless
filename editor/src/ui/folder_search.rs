use rune::{col, row, text, El, Key, ScrollSpec, TextEdit, Tx};
use rune_kit::{Dialog, Kit, TextInput};
use crate::{quick_open::FolderMatch, state::Editor};

#[derive(Default)]
pub(crate) struct FolderSearchState {
    pub open: bool,
    pub query: TextEdit,
    pub matches: Vec<FolderMatch>,
    pub selected: usize,
    pub request_id: u64,
    pub due: Option<f64>,
}
pub(crate) fn mount(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    if !editor.folder_search.open { return; }
    let query = TextInput::new(&editor.folder_search.query).label("Search in workspace").placeholder("Search text in files").width(None)
        .on_edit(|e: &mut Editor| &mut e.folder_search.query)
        .on_change(|e: &mut Editor, tx| e.schedule_folder_search(tx))
        .on_submit(|e: &mut Editor, tx| e.choose_folder_match(e.folder_search.selected, tx))
        .on_key(|e: &mut Editor, key, tx: &mut Tx| match key.key {
            Key::Escape => { e.folder_search.open = false; e.folder_search.due = None; tx.focus("prose-input", false); true },
            Key::Down => { let len = e.folder_search.matches.len(); if len > 0 { e.folder_search.selected = (e.folder_search.selected + 1) % len; } true },
            Key::Up => { let len = e.folder_search.matches.len(); if len > 0 { e.folder_search.selected = (e.folder_search.selected + len - 1) % len; } true },
            _ => false,
        }).build(kit).key("folder-query");
    let mut list = col().w_full().gap(2.0);
    for (index, hit) in editor.folder_search.matches.iter().enumerate() {
        let selected = index == editor.folder_search.selected;
        let item = col().w_full().px(10.0).py(6.0).gap(2.0).radius(4.0)
            .fill(if selected { kit.c.layer_02 } else { rune::Color::TRANSPARENT })
            .child(text(format!("{}:{}", hit.rel_path, hit.line_no)).font(kit.ty.small_medium).color(kit.c.text_base).truncate())
            .child(text(hit.preview.clone()).font(kit.ty.small).color(kit.c.text_muted).truncate())
            .hit().on_press_tx(move |e: &mut Editor, tx| e.choose_folder_match(index, tx));
        list = list.child(item);
    }
    let body = col().w_full().grow(1.0).min_h(0.0).px(16.0).pb(16.0).gap(8.0).child(query)
        .child(row().w_full().child(text("Type at least two characters").font(kit.ty.small).color(kit.c.text_faint)))
        .child(col().grow(1.0).w_full().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list));
    Dialog::new("folder-search", "Search in Workspace").width(640.0).open(true).child(body)
        .on_close(|e: &mut Editor| { e.folder_search.open = false; e.folder_search.due = None; }).build(kit, overlays);
}
