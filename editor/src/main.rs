//! Local Rune editor over ghostless edit sessions.
use rune::{App, AppSpec};
#[allow(dead_code)]
mod fs_io;
#[allow(dead_code)]
mod prefs;
#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
mod cli;
mod session;
mod state;
mod ui;
mod ui_welcome;
mod worker;
#[allow(dead_code)]
mod workspace_tree;
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
            editor.show_diff = true;
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
        .on_close(|editor, _| editor.flush_on_exit())
        .on_quit(|editor, _| editor.flush_on_exit());
    app.run();
}
