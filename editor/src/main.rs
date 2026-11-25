//! Local Rune editor over ghostless edit sessions.
use rune::{App, AppSpec};
mod cli;
mod documents;
mod fs_io;
mod markdown_edit;
mod prefs;
mod session;
mod state;
mod theme;
mod ui;
mod ui_welcome;
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
