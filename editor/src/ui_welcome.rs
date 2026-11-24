use rune::{col, row, text, Cx, El, FileDialogOptions, Tx};
use rune_kit::{Button, ButtonVariant, Kit};
use crate::state::Editor;

pub(crate) fn open_folder(tx: &mut Tx) {
    tx.open_folder(FileDialogOptions { title: "Open Folder".into(), ..Default::default() }, |editor: &mut Editor, result, _| {
        match result { Ok(Some(path)) => editor.open_workspace(path, None), Ok(None) => (), Err(error) => editor.status = error.to_string() }
    });
}
pub(crate) fn open_file(tx: &mut Tx) {
    tx.open_file(FileDialogOptions { title: "Open File".into(), ..Default::default() }.filter("Writing", &["md", "markdown", "txt"]), |editor: &mut Editor, result, _| {
        match result {
            Ok(Some(path)) => { if let Some(parent) = path.parent() { editor.open_workspace(parent.to_path_buf(), Some(path)); } },
            Ok(None) => (), Err(error) => editor.status = error.to_string(),
        }
    });
}
pub(crate) fn view(editor: &Editor, cx: &mut Cx<Editor>) -> El {
    let kit = Kit::neutral(cx.system_dark());
    let voice = editor.voice_project.as_ref().map(|p| format!("Voice profile: {}", p.display())).unwrap_or_else(|| "No voice project attached — plain Markdown mode".into());
    let mut card = col().max_w(520.0).gap(14.0).padding(24.0)
        .child(text("ghostless").font(kit.ty.ui_medium).color(kit.c.text_base))
        .child(text("Calm writing & voice review studio").font(kit.ty.body).color(kit.c.text_muted));
    if let Some(folder) = &editor.workspace {
        card = card.child(text(format!("Workspace: {}", folder.display())).font(kit.ty.body).color(kit.c.text_base));
        if let Some(file) = &editor.initial_file { card = card.child(text(format!("File: {}", file.display())).font(kit.ty.ui).color(kit.c.text_muted)); }
    }
    card = card.child(row().gap(8.0)
        .child(Button::new("Open Folder…").variant(ButtonVariant::Submit).on_press_tx(|_: &mut Editor, tx| open_folder(tx)).build(&kit))
        .child(Button::new("Open File…").variant(ButtonVariant::Outline).on_press_tx(|_: &mut Editor, tx| open_file(tx)).build(&kit)));
    if editor.workspace.is_none() {
        card = card.child(text("Recent Workspaces").font(kit.ty.ui_medium).color(kit.c.text_muted));
        for folder in editor.store.recent_workspaces.iter().filter(|p| p.is_dir()) {
            let path = folder.clone();
            card = card.child(Button::new(folder.display().to_string()).variant(ButtonVariant::Outline).on_press(move |editor: &mut Editor| editor.open_workspace(path.clone(), None)).build(&kit));
        }
    }
    card = card.child(text(voice).font(kit.ty.ui).color(kit.c.text_muted));
    col().w_full().h_full().items_center().justify_center().fill(kit.c.bg_base).child(card)
}
