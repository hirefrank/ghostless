use super::{Editor, ViewMode};
use crate::{
    prefs::{self, FontChoice, ThemeMode},
    quick_open::fuzzy_score,
    ui_welcome,
};
use rune::{TextEdit, Tx};

#[derive(Default)]
pub(crate) struct PaletteState {
    pub(crate) open: bool,
    pub(crate) query: TextEdit,
    pub(crate) selected: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) enum Action {
    OpenFolder,
    OpenFile,
    NewFile,
    Quick,
    Search,
    Save,
    Autosave,
    Sidebar,
    Inspector,
    Reference,
    Focus,
    Mode(ViewMode),
    VoiceLens,
    Placeholder,
    Start,
    Finish,
    Build,
    Studio,
    VoiceDoc(&'static str),
    Theme(ThemeMode),
    Font(FontChoice),
    FontSize(f32),
    ResetFontSize,
}

pub(crate) struct Command {
    pub(crate) action: Action,
    pub(crate) name: &'static str,
    pub(crate) keys: &'static [&'static str],
}

const MOD: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

impl Command {
    const fn new(action: Action, name: &'static str, keys: &'static [&'static str]) -> Self {
        Self { action, name, keys }
    }

    pub(crate) fn enabled(&self, editor: &Editor) -> bool {
        let has_doc = editor.active_doc_ref().is_some();
        let has_workspace = editor.workspace.is_some();
        let has_voice = editor.voice_project.is_some();
        match self.action {
            Action::NewFile | Action::Search => has_workspace,
            Action::Quick => has_workspace || has_voice,
            Action::Save => editor
                .active_doc_ref()
                .is_some_and(|doc| doc.can_save() && !doc.blocks_file_save()),
            Action::Sidebar | Action::Inspector | Action::Focus => !editor.is_landing_view(),
            Action::Reference => has_doc || has_workspace,
            Action::Mode(ViewMode::Compare) => editor.active_has_baseline(),
            Action::Mode(_) | Action::VoiceLens | Action::Placeholder => has_doc,
            Action::Start => editor.can_track_session(),
            Action::Finish => editor.can_open_finish(),
            Action::Build => has_voice && !editor.studio.building(),
            Action::Studio => has_voice && !editor.is_landing_view(),
            Action::VoiceDoc(_) => has_voice,
            _ => true,
        }
    }
}

const COMMANDS: &[Command] = &[
    Command::new(Action::OpenFolder, "Open Folder", &[MOD, "O"]),
    Command::new(Action::OpenFile, "Open File", &[MOD, "Shift", "O"]),
    Command::new(Action::NewFile, "New File", &[MOD, "N"]),
    Command::new(Action::Quick, "Go to File / Session", &[MOD, "P"]),
    Command::new(Action::Search, "Search in Workspace", &[MOD, "Shift", "F"]),
    Command::new(Action::Save, "Save Now", &[MOD, "S"]),
    Command::new(
        Action::Autosave,
        "Toggle Auto-save for Workspace Files",
        &[],
    ),
    Command::new(Action::Sidebar, "Toggle Sidebar", &[MOD, "Shift", "B"]),
    Command::new(Action::Inspector, "Toggle Right Inspector", &[MOD, "J"]),
    Command::new(Action::Reference, "Toggle Reference Split", &[MOD, "\\"]),
    Command::new(Action::Focus, "Toggle Focus Mode", &["F11"]),
    Command::new(Action::Mode(ViewMode::Write), "Switch to Write", &[]),
    Command::new(
        Action::Mode(ViewMode::Preview),
        "Switch to Preview",
        &[MOD, "Shift", "V"],
    ),
    Command::new(
        Action::Mode(ViewMode::Compare),
        "Switch to Compare",
        &[MOD, "D"],
    ),
    Command::new(Action::VoiceLens, "Toggle Voice Lens", &[MOD, "L"]),
    Command::new(Action::Placeholder, "Jump to Next Placeholder", &["F8"]),
    Command::new(Action::Start, "Start Edit Session", &[]),
    Command::new(Action::Finish, "Finish Edit Session", &[]),
    Command::new(Action::Build, "Rebuild Voice Guide", &[]),
    Command::new(Action::Studio, "Open Rules & Checks", &[]),
    Command::new(Action::VoiceDoc("STYLE.md"), "Open STYLE.md", &[]),
    Command::new(Action::VoiceDoc("AUDIENCE.md"), "Open AUDIENCE.md", &[]),
    Command::new(
        Action::VoiceDoc("voice-guide.md"),
        "Open voice-guide.md",
        &[],
    ),
    Command::new(
        Action::Theme(ThemeMode::System),
        "Switch Theme to System",
        &[],
    ),
    Command::new(
        Action::Theme(ThemeMode::Light),
        "Switch Theme to Light",
        &[],
    ),
    Command::new(Action::Theme(ThemeMode::Dark), "Switch Theme to Dark", &[]),
    Command::new(
        Action::Theme(ThemeMode::Paper),
        "Switch Theme to Paper",
        &[],
    ),
    Command::new(Action::Font(FontChoice::Sans), "Switch Font to Sans", &[]),
    Command::new(Action::Font(FontChoice::Serif), "Switch Font to Serif", &[]),
    Command::new(Action::Font(FontChoice::Mono), "Switch Font to Mono", &[]),
    Command::new(Action::FontSize(1.0), "Font Size Up", &[MOD, "="]),
    Command::new(Action::FontSize(-1.0), "Font Size Down", &[MOD, "-"]),
    Command::new(Action::ResetFontSize, "Font Size Reset", &[MOD, "0"]),
];

pub(crate) fn filtered_commands(query: &str) -> Vec<&'static Command> {
    let query = query.trim();
    if query.is_empty() {
        return COMMANDS.iter().collect();
    }
    let mut matches: Vec<_> = COMMANDS
        .iter()
        .filter_map(|item| fuzzy_score(query, item.name).map(|score| (score, item)))
        .collect();
    matches.sort_by_key(|item| std::cmp::Reverse(item.0));
    matches.into_iter().map(|(_, item)| item).collect()
}

impl Editor {
    pub(crate) fn open_palette(&mut self, tx: &mut Tx) {
        if self.chrome.palette.open {
            return;
        }
        self.chrome.palette = PaletteState {
            open: true,
            ..Default::default()
        };
        self.select_first_command();
        tx.focus("command-query", false);
    }

    pub(crate) fn close_palette(&mut self) {
        self.chrome.palette.open = false;
        self.pending_document_focus = true;
    }

    pub(crate) fn select_first_command(&mut self) {
        self.chrome.palette.selected = filtered_commands(&self.chrome.palette.query.text)
            .iter()
            .position(|item| item.enabled(self));
    }

    pub(crate) fn step_palette(&mut self, forward: bool) {
        let choices: Vec<_> = filtered_commands(&self.chrome.palette.query.text)
            .into_iter()
            .enumerate()
            .filter_map(|(index, item)| item.enabled(self).then_some(index))
            .collect();
        if choices.is_empty() {
            self.chrome.palette.selected = None;
            return;
        }
        let increment = if forward { 1 } else { choices.len() - 1 };
        let current = self
            .chrome
            .palette
            .selected
            .and_then(|selected| choices.iter().position(|&i| i == selected));
        self.chrome.palette.selected = current.map_or_else(
            || choices.first().copied(),
            |i| choices.get((i + increment) % choices.len()).copied(),
        );
    }

    pub(crate) fn choose_command(&mut self, index: usize, tx: &mut Tx) {
        let commands = filtered_commands(&self.chrome.palette.query.text);
        let Some(command) = commands.get(index).filter(|item| item.enabled(self)) else {
            return;
        };
        self.close_palette();
        if self.studio.open
            && matches!(
                command.action,
                Action::Quick
                    | Action::Search
                    | Action::Save
                    | Action::Mode(_)
                    | Action::Reference
                    | Action::Placeholder
                    | Action::Start
                    | Action::Finish
                    | Action::VoiceDoc(_)
            )
        {
            self.close_voice_studio(tx);
        }
        match command.action {
            Action::OpenFolder => ui_welcome::open_folder(tx),
            Action::OpenFile => ui_welcome::open_file(tx),
            Action::NewFile => self.open_new_file_dialog(),
            Action::Quick => self.open_quick(tx),
            Action::Search => self.open_folder_search(tx),
            Action::Save => self.save_clicked(),
            Action::Autosave => self.toggle_autosave(),
            Action::Sidebar => self.toggle_sidebar(),
            Action::Inspector => self.toggle_inspector(),
            Action::Reference => self.toggle_reference(),
            Action::Focus => self.chrome.focus_mode = !self.chrome.focus_mode,
            Action::Mode(mode) => self.set_view_mode(mode, tx),
            Action::VoiceLens => self.toggle_voice_lens(),
            Action::Placeholder => self.jump_next_placeholder(tx),
            Action::Start => self.open_track_session(),
            Action::Finish => self.finish(),
            Action::Build => self.request_voice_build(),
            Action::Studio => {
                self.pending_document_focus = false;
                self.open_voice_studio(tx);
            }
            Action::VoiceDoc(name) => self.open_voice_doc(name, name == "voice-guide.md"),
            Action::Theme(mode) => self.set_theme_mode(mode),
            Action::Font(font) => self.set_font_choice(font, tx),
            Action::FontSize(delta) => self.adjust_font_size(delta),
            Action::ResetFontSize => self.reset_font_size(),
        }
    }

    pub(crate) fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.store.ui.theme = mode;
        let _ = prefs::save_store(&self.store);
    }

    pub(crate) fn set_font_choice(&mut self, font: FontChoice, tx: &mut Tx) {
        self.store.ui.font_family = font;
        crate::theme::apply_font_choice(font, tx);
        let _ = prefs::save_store(&self.store);
    }

    pub(crate) fn adjust_font_size(&mut self, delta: f32) {
        self.store.ui.font_size = (self.store.ui.font_size + delta).clamp(13.0, 24.0);
        let _ = prefs::save_store(&self.store);
    }

    pub(crate) fn reset_font_size(&mut self) {
        self.store.ui.font_size = 16.5;
        let _ = prefs::save_store(&self.store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{session::Session, test_util::TestDir};

    #[test]
    fn fuzzy_search_and_context_match_the_active_session() {
        let root = TestDir::new("palette");
        let mut editor = Editor::empty_with_handlers(
            prefs::EditorStateStore::default(),
            Box::new(|_, _| Ok(String::new())),
            Box::new(|_| Ok(crate::worker::WriteOutcome::Local)),
            Box::new(|| {}),
        );
        assert_eq!(filtered_commands("opn fld")[0].name, "Open Folder");
        let finish = filtered_commands("fin edt sess")[0];
        assert_eq!(finish.name, "Finish Edit Session");
        assert!(!finish.enabled(&editor));
        let studio = filtered_commands("rules checks")[0];
        assert!(!studio.enabled(&editor));
        editor.voice_project = Some(root.to_path_buf());
        editor
            .documents
            .push(crate::documents::DocumentBuffer::from_session(
                Session {
                    project: root.to_path_buf(),
                    id: "2026-01-02-notes".into(),
                    source_name: "draft.md".into(),
                    original: "Café original".into(),
                    after: "Café final".into(),
                    notes: String::new(),
                    before_sha256: "test".into(),
                    finished: false,
                },
                true,
                None,
            ));
        editor.active_doc = Some(0);
        assert!(finish.enabled(&editor) && studio.enabled(&editor));
        editor.documents[0].read_only = true;
        assert!(!finish.enabled(&editor));
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::{session::Session, test_util::TestDir, worker::WriteOutcome};
    use rune::{Key, KeyEvent, Mods};

    #[test]
    fn palette_shortcut_opens_from_preview_and_escape_restores_focus() {
        let fixed = TestDir::new("palette-focus");
        let editor = Editor::with_handlers(
            Session {
                project: fixed.to_path_buf(),
                id: "2026-01-02-notes".into(),
                source_name: "draft.md".into(),
                original: "Café original".into(),
                after: "---\r\nsection: notes\r\n---\r\n# Café\n\n<!-- private -->\n".into(),
                notes: String::new(),
                before_sha256: "test".into(),
                finished: false,
            },
            prefs::EditorStateStore::default(),
            Box::new(|_, _| Ok(String::new())),
            Box::new(|_| Ok(WriteOutcome::Local)),
            Box::new(|| {}),
        );
        let mut rt = rune::Runtime::new(
            editor,
            crate::ui::view,
            (1120.0, 800.0),
            rune::Env::new(false, 1.0),
        );
        rt.on_key = Some(super::super::on_key);
        rt.update(|editor, tx| editor.set_view_mode(ViewMode::Preview, tx));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Command('p'),
            mods: Mods {
                shift: true,
                ..Default::default()
            },
        });
        rt.frame(0.1);
        let elements = rt.elements();
        assert!(
            elements
                .iter()
                .any(|el| el.key.as_deref() == Some("command-palette-dialog"))
        );
        assert!(
            elements
                .iter()
                .any(|el| el.label.as_deref() == Some("Search commands") && el.focused)
        );
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.2);
        assert!(!rt.state.chrome.palette.open);
        rt.frame(0.8);
        let elements = rt.elements();
        assert!(
            !elements
                .iter()
                .any(|el| el.key.as_deref() == Some("command-palette-dialog"))
        );
        assert!(
            elements
                .iter()
                .any(|el| el.key.as_deref() == Some("preview-body") && el.focused)
        );
    }
}
