use super::{Editor, ViewMode};
use crate::prefs;
use rune::{Key, KeyEvent, Tx};
use rune_kit::shell::is_sidebar_toggle;

pub(crate) fn on_key(editor: &mut Editor, key: &KeyEvent, tx: &mut Tx) -> bool {
    if key.is_command('q') && !key.mods.shift && !key.mods.alt {
        if editor.flush_on_exit() {
            tx.quit();
        } else {
            editor.show_close_error();
        }
        return true;
    }
    if editor.chrome.active_dialog.is_some() || editor.chrome.finish.is_some() {
        return false;
    }
    if key.is_command('o') {
        if key.mods.shift {
            crate::ui_welcome::open_file(tx);
        } else {
            crate::ui_welcome::open_folder(tx);
        }
        return true;
    }
    if editor.is_landing_view() {
        return false;
    }
    if editor.find.open && key.key == Key::Escape {
        editor.close_find(tx);
        return true;
    }
    if key.key == Key::Function(11)
        || (key.key == Key::Enter && key.mods.shift && (key.mods.cmd || key.mods.ctrl))
        || (key.key == Key::Escape
            && editor.chrome.focus_mode
            && !editor.chrome.view_settings_open
            && !editor.find.open)
    {
        editor.chrome.focus_mode = !editor.chrome.focus_mode;
        return true;
    }
    if !key.mods.shift && !key.mods.alt {
        let delta = if key.is_command('=') {
            Some(1.0)
        } else if key.is_command('-') {
            Some(-1.0)
        } else {
            None
        };
        if let Some(delta) = delta {
            editor.store.ui.font_size = (editor.store.ui.font_size + delta).clamp(13.0, 24.0);
            let _ = prefs::save_store(&editor.store);
            return true;
        }
        if key.is_command('0') {
            editor.store.ui.font_size = 16.5;
            let _ = prefs::save_store(&editor.store);
            return true;
        }
    }
    if key.is_command('p') && !key.mods.shift && !key.mods.alt {
        editor.open_quick(tx);
        return true;
    }
    if key.is_command('f') && key.mods.shift && !key.mods.alt {
        editor.open_folder_search(tx);
        return true;
    }
    if key.is_command('f') && !key.mods.shift && !key.mods.alt {
        editor.open_find(false, tx);
        return true;
    }
    if key.is_command('h') && !key.mods.shift && !key.mods.alt {
        editor.open_find(true, tx);
        return true;
    }
    if key.is_command('v') && key.mods.shift && !key.mods.alt {
        let mode = if editor.view_mode == ViewMode::Preview {
            ViewMode::Write
        } else {
            ViewMode::Preview
        };
        editor.set_view_mode(mode, tx);
        return true;
    }
    if key.is_command('\\') && !key.mods.shift && !key.mods.alt {
        editor.toggle_reference();
        return true;
    }
    if is_sidebar_toggle(key) {
        editor.toggle_sidebar();
        return true;
    }
    if key.is_command('j') && !key.mods.shift && !key.mods.alt {
        editor.toggle_inspector();
        return true;
    }
    if editor.section_menu_open && key.key == Key::Escape {
        editor.section_menu_open = false;
        editor.focus_document(tx);
        return true;
    }
    if key.key == Key::Function(8) || (key.is_command('.') && key.mods.shift) {
        editor.jump_next_placeholder(tx);
        return true;
    }
    if key.is_command('l') && !key.mods.shift && !key.mods.alt {
        editor.toggle_voice_lens();
        return true;
    }
    if key.is_command('n') && !key.mods.shift && editor.workspace.is_some() {
        editor.open_new_file_dialog();
        return true;
    }
    if key.is_command('w') && !key.mods.shift && editor.close_active_tab(tx) {
        return true;
    }
    if key.key == Key::Tab && (key.mods.cmd || key.mods.ctrl) {
        editor.cycle_tab(key.mods.shift, tx);
        return true;
    }
    if key.is_command('s') && !key.mods.shift {
        editor.save_clicked();
        return true;
    }
    if key.is_command('d') && !key.mods.shift && editor.active_has_baseline() {
        let mode = if editor.view_mode == ViewMode::Compare {
            ViewMode::Write
        } else {
            ViewMode::Compare
        };
        editor.set_view_mode(mode, tx);
        return true;
    }
    false
}
