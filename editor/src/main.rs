//! Local Rune editor over ghostless edit sessions.
use rune::{App, AppSpec};
mod bridge;
mod cli;
mod contracts;
mod documents;
mod error;
mod fs_io;
mod hunks;
mod markdown_decor;
mod markdown_edit;
mod prefs;
#[cfg(test)]
mod properties;
mod quick_open;
mod search;
mod session;
mod state;
#[cfg(test)]
mod test_util;
mod theme;
mod ui;
mod ui_welcome;
mod voice_lens;
mod watcher;
mod worker;
mod workspace_tree;

#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use session::*;
use state::*;
use ui::*;

fn main() {
    let session = match prepare() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("ghostless-editor: {error}");
            std::process::exit(2);
        }
    };
    let spec = AppSpec {
        min_size: Some((800.0, 620.0)),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: &[("diff", |editor| {
            editor.view_mode = ViewMode::Compare;
            editor.refresh_diff();
        })],
        ..AppSpec::new(
            "ghostless editor",
            (1120.0, 800.0),
            || unreachable!("initialized via App::init"),
            view,
        )
    };
    let app = App::new(spec);
    let handle = app.handle();
    let wake_handle = handle.clone();
    let app = app
        .keep_running()
        .init(move || {
            let mut editor = Editor::new(session, Box::new(move || wake_handle.wake()));
            editor.reopen_window = Some(Box::new(move || handle.show()));
            editor
        })
        .on_focus(|editor, focused, _| {
            if !focused {
                editor.flush_autosave_buffers();
            }
        })
        .on_close(on_window_close)
        .on_quit(|editor, _| editor.on_application_quit());
    app.run();
}
