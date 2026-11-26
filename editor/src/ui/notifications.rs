use crate::state::{Editor, NoticeAction};
use rune::El;
use rune_kit::{Kit, Toast, ToastRegion};

pub(super) fn mount(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) {
    let entries = editor.notifications.stack.entries();
    if entries.is_empty() {
        return;
    }
    let mut region = ToastRegion::new();
    for entry in &entries[entries.len().saturating_sub(rune_kit::toast_stack::VISIBLE)..] {
        let id = entry.id;
        let mut toast = Toast::new(format!("toast-{id}"), &entry.options.text)
            .on_close(move |editor: &mut Editor| editor.notifications.dismiss(id));
        if let Some(action) = entry.options.action.clone() {
            let label = match &action {
                NoticeAction::Compare(_) => "Compare",
            };
            toast = toast.action(kit, label, true, move |editor: &mut Editor, tx| {
                editor.run_notice_action(id, &action, tx);
            });
        }
        region = region.toast(toast.build(kit));
    }
    overlays.push(region.build(kit));
}
