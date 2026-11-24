use rune::{Cx, El, Tx, col, row};
use rune_kit::{
    Divider, Kit, file_icon,
    shell::{TabItem, TabItemStatus, TabOrientation, TabStrip},
};
use crate::state::Editor;

pub(crate) fn build(editor: &Editor, cx: &Cx<Editor>, kit: &Kit) -> El {
    let active_id = editor.active_tab_ref().map(|t| t.id.clone());
    let items: Vec<TabItem<'_, Editor>> = editor
        .tabs
        .iter()
        .map(|tab| {
            let label = if tab.pinned {
                tab.title.clone()
            } else {
                format!("~ {}", tab.title)
            };
            let status = if editor.tab_is_dirty(tab) {
                Some(TabItemStatus::Unread)
            } else {
                None
            };
            TabItem::new(tab.id.clone(), label)
                .icon(file_icon(&tab.title))
                .status(status)
        })
        .collect();

    let strip = TabStrip::new("doc-tabs", TabOrientation::Horizontal)
        .tabs(items)
        .active(active_id.as_deref())
        .viewport_width(cx.layout_size("doc-tabs").map(|s| s[0]))
        .fill(kit.c.bg_base)
        .on_select(|e: &mut Editor, id: &str, tx: &mut Tx| e.activate_tab(id, tx))
        .on_close(|e: &mut Editor, id: &str, _| {
            e.close_tab_by_id(id);
        })
        .on_double_click(|e: &mut Editor, id: &str, _| e.pin_tab_by_id(id))
        .build(kit);

    col()
        .key("tabs-header")
        .w_full()
        .child(
            row()
                .key("tab-bar")
                .w_full()
                .h(36.0)
                .px(8.0)
                .center_y()
                .fill(kit.c.bg_base)
                .child(strip),
        )
        .child(Divider::horizontal().build(kit))
}
