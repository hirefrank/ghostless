//! Local Rune editor over ghostless edit sessions.
use rune::{App, AppSpec};
mod cli;
mod documents;
mod fs_io;
mod prefs;
mod session;
mod state;
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
    let app = app
        .init(move || Editor::new(session, Box::new(move || handle.wake())))
        .on_focus(|editor, focused, _| {
            if !focused {
                editor.flush_autosave_buffers();
            }
        })
        .on_close(|editor, _| editor.flush_on_exit())
        .on_quit(|editor, _| editor.flush_on_exit());
    app.run();
}
