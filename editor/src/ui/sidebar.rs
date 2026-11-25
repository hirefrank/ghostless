use crate::{state::Editor, workspace_tree::SessionStatus};
use rune::{El, Icon, ScrollSpec, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Divider, FileNode, IconButton, IconButtonVariant, Kit, Menu, MenuItem,
    Tree, TreeItem, glyphs,
    shell::{RowStatus, SidebarHeader, SidebarRow, SidebarSection},
};

static ICON_PLUS: Icon = Icon {
    name: "ghostless-plus",
    view: 16.0,
    body: r#"<path d="M8 3.25V12.75M3.25 8H12.75" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};

static ICON_NEW_FOLDER: Icon = Icon {
    name: "ghostless-new-folder",
    view: 16.0,
    body: r#"<path d="M1.5 3.5H6L7.5 5H14.5V13.5H1.5V3.5Z" stroke="currentColor" stroke-linejoin="round"/><path d="M8 7.5V11.5M6 9.5H10" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_MORE: Icon = Icon {
    name: "ghostless-more",
    view: 16.0,
    body: r#"<circle cx="4" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12" cy="8" r="1.1" fill="currentColor"/>"#,
};

pub(crate) fn build(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> El {
    let ws_title = editor
        .workspace
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Workspace");

    let has_ws = editor.workspace.is_some();
    let header_actions = row()
        .key("sidebar-header-actions")
        .w_full()
        .px(4.0)
        .pb(4.0)
        .gap(4.0)
        .center_y()
        .child(
            text(ws_title.to_owned())
                .font(kit.ty.ui_medium)
                .color(kit.c.text_base)
                .truncate()
                .grow(1.0)
                .shrink(),
        )
        .child(
            IconButton::new(&glyphs::FOLDER_OPEN)
                .variant(IconButtonVariant::GhostMuted)
                .label("Open Folder")
                .help("Open Folder")
                .on_press_tx(|_: &mut Editor, tx| crate::ui_welcome::open_folder(tx))
                .build(kit),
        )
        .child(
            IconButton::new(&ICON_PLUS)
                .variant(IconButtonVariant::GhostMuted)
                .label("New File")
                .help("New File")
                .disabled(!has_ws)
                .on_press(|e: &mut Editor| e.open_new_file_dialog())
                .build(kit),
        )
        .child(
            IconButton::new(&ICON_NEW_FOLDER)
                .variant(IconButtonVariant::GhostMuted)
                .label("New Folder")
                .help("New Folder")
                .disabled(!has_ws)
                .on_press(|e: &mut Editor| e.open_new_folder_dialog())
                .build(kit),
        )
        .child(
            Button::new(if editor.store.ui.show_all_files {
                "All"
            } else {
                "Writing"
            })
            .variant(ButtonVariant::GhostMuted)
            .disabled(!has_ws)
            .help("Toggle writing filter vs all workspace files")
            .on_press(|e: &mut Editor| e.toggle_show_all_files())
            .build(kit),
        );

    let mut root = col()
        .key("workspace-sidebar")
        .w_full()
        .h_full()
        .min_h(0.0)
        .px(8.0)
        .pb(8.0)
        .gap(6.0)
        .child(header_actions);

    if has_ws {
        let (can_mutate, is_file) = editor.can_mutate_selected();
        let more_btn = IconButton::new(&ICON_MORE)
            .variant(IconButtonVariant::GhostMuted)
            .label("File actions")
            .help("File actions")
            .expanded(editor.file_menu_open)
            .on_press(|e: &mut Editor| e.file_menu_open = !e.file_menu_open)
            .build(kit);
        let menu = Menu::new("file-actions")
            .trigger(more_btn)
            .open(editor.file_menu_open)
            .on_close(|e: &mut Editor| e.file_menu_open = false)
            .item(
                MenuItem::new("New File…")
                    .shortcut("cmd+n")
                    .on_select(|e: &mut Editor| {
                        e.file_menu_open = false;
                        e.open_new_file_dialog();
                    }),
            )
            .item(MenuItem::new("New Folder…").on_select(|e: &mut Editor| {
                e.file_menu_open = false;
                e.open_new_folder_dialog();
            }))
            .separator()
            .item(
                MenuItem::new("Pin to Reference Split")
                    .disabled(!is_file)
                    .on_select(|e: &mut Editor| {
                        e.file_menu_open = false;
                        e.pin_selected_reference();
                    }),
            )
            .item(
                MenuItem::new("Rename…")
                    .disabled(!can_mutate)
                    .on_select(|e: &mut Editor| {
                        e.file_menu_open = false;
                        e.open_rename_dialog();
                    }),
            )
            .item(
                MenuItem::new("Duplicate")
                    .disabled(!is_file || !can_mutate)
                    .on_select(|e: &mut Editor| {
                        e.file_menu_open = false;
                        e.duplicate_selected();
                    }),
            )
            .item(
                MenuItem::new("Delete…")
                    .danger(true)
                    .disabled(!can_mutate)
                    .on_select(|e: &mut Editor| {
                        e.file_menu_open = false;
                        e.open_delete_dialog();
                    }),
            )
            .build(kit, overlays);

        let files_hdr = SidebarHeader::new("Files").action(menu).build(kit);
        let dirty_paths = editor.dirty_file_paths();
        let accent = kit.c.bg_accent;
        let faint = kit.c.text_faint;
        let tree = Tree::new(
            |e: &Editor| &e.file_tree,
            |e: &mut Editor| &mut e.file_tree,
            FileNode::label,
        )
        .name("Files")
        .row(move |node, row| {
            let mut item = TreeItem::file(node, row);
            if let Some(entry) = &node.data {
                if entry.read_only {
                    item = item.badge("RO").label_color(faint);
                }
                if dirty_paths.contains(&entry.abs_path) {
                    item = item.dot(accent);
                }
            }
            item
        })
        .on_select(|e: &mut Editor, id, tx| e.on_tree_select(id, tx))
        .on_open(|e: &mut Editor, id, tx| e.on_tree_open(id, tx))
        .build(editor, kit)
        .grow(1.0)
        .min_h(0.0);

        root = root.child(
            col()
                .key("files-section")
                .w_full()
                .grow(1.0)
                .min_h(120.0)
                .gap(2.0)
                .child(files_hdr)
                .child(tree),
        );
    } else {
        root = root.child(el().key("sidebar-spacer").grow(1.0));
    }

    if let Some(voice) = &editor.voice_project {
        root = root.child(Divider::horizontal().build(kit));

        let mut sessions_sec = SidebarSection::new("Sessions")
            .count(editor.sessions.len())
            .collapsed(editor.sessions_collapsed)
            .on_toggle(|e: &mut Editor, _| e.sessions_collapsed = !e.sessions_collapsed);

        for summary in &editor.sessions {
            let project = voice.clone();
            let id = summary.id.clone();
            let selected = editor
                .active_doc_ref()
                .is_some_and(|d| d.matches_session(&project, &id));
            let status = match summary.status {
                SessionStatus::Editing => RowStatus::Attention,
                SessionStatus::Finished => {
                    RowStatus::Other(el().size(7.0, 7.0).radius(3.5).fill(kit.c.icon_faint))
                }
            };
            let row_el = SidebarRow::new(summary.source_name.clone())
                .subtitle(summary.id.clone())
                .status(status)
                .selected(selected)
                .on_select(move |e: &mut Editor, _| {
                    e.open_session_entry(project.clone(), id.clone())
                })
                .build(kit)
                .key(format!("session-{}", summary.id));
            sessions_sec = sessions_sec.row(row_el);
        }

        if editor.sessions.is_empty() && !editor.sessions_collapsed {
            sessions_sec = sessions_sec.row(
                row().key("sessions-empty").px(8.0).py(4.0).child(
                    text("No sessions yet")
                        .font(kit.ty.small)
                        .color(kit.c.text_faint),
                ),
            );
        }

        root = root.child(
            col()
                .key("sessions-scroll")
                .w_full()
                .max_h(190.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(sessions_sec.build(kit)),
        );

        root = root.child(Divider::horizontal().build(kit));
        let voice_sec = SidebarSection::new("Voice")
            .collapsed(editor.voice_collapsed)
            .on_toggle(|e: &mut Editor, _| e.voice_collapsed = !e.voice_collapsed)
            .row(
                SidebarRow::new("STYLE.md")
                    .icon(&glyphs::FILE_TEXT)
                    .on_select(|e: &mut Editor, _| e.open_voice_doc("STYLE.md", false))
                    .build(kit)
                    .key("voice-style"),
            )
            .row(
                SidebarRow::new("AUDIENCE.md")
                    .icon(&glyphs::FILE_TEXT)
                    .on_select(|e: &mut Editor, _| e.open_voice_doc("AUDIENCE.md", false))
                    .build(kit)
                    .key("voice-audience"),
            )
            .row(
                SidebarRow::new("Voice Guide")
                    .icon(&glyphs::LOCK)
                    .detail("RO")
                    .on_select(|e: &mut Editor, _| e.open_voice_doc("voice-guide.md", true))
                    .build(kit)
                    .key("voice-guide"),
            )
            .build(kit);
        root = root.child(voice_sec);
    }

    root
}
