use crate::fs_io;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
    Paper,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FontChoice {
    #[default]
    Sans,
    Serif,
    Mono,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MeasurePreset {
    Narrow,
    #[default]
    Medium,
    Wide,
    Full,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiPrefs {
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub inspector_open: bool,
    pub inspector_width: f32,
    pub autosave_workspace_files: bool,
    pub theme: ThemeMode,
    pub font_family: FontChoice,
    pub font_size: f32,
    pub measure: MeasurePreset,
    pub show_all_files: bool,
    pub voice_lens: bool,
    pub spellcheck: bool,
}
impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            sidebar_width: 250.0,
            inspector_open: false,
            inspector_width: 310.0,
            autosave_workspace_files: true,
            theme: ThemeMode::default(),
            font_family: FontChoice::default(),
            font_size: 16.5,
            measure: MeasurePreset::default(),
            show_all_files: false,
            voice_lens: false,
            spellcheck: false,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SessionLink {
    pub workspace_file: PathBuf,
    pub last_synced_sha256: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct EditorStateStore {
    pub recent_workspaces: Vec<PathBuf>,
    pub last_workspace: Option<PathBuf>,
    pub last_open_files: HashMap<String, Vec<String>>,
    pub session_links: HashMap<String, SessionLink>,
    pub ui: UiPrefs,
}
impl EditorStateStore {
    pub(crate) fn record_workspace(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.recent_workspaces.retain(|p| p != &dir);
        self.recent_workspaces.insert(0, dir.clone());
        self.recent_workspaces.truncate(10);
        self.last_workspace = Some(dir);
    }
    pub(crate) fn session_link_key(project: &Path, session_id: &str) -> String {
        format!(
            "{}::{session_id}",
            project
                .canonicalize()
                .unwrap_or_else(|_| project.to_path_buf())
                .display()
        )
    }
}
pub(crate) fn ghostless_home() -> PathBuf {
    std::env::var_os("GHOSTLESS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .unwrap_or_default(),
            )
            .join(".ghostless")
        })
}
pub(crate) fn state_path() -> PathBuf {
    ghostless_home().join("editor-state.json")
}
pub(crate) fn recovery_dir() -> PathBuf {
    ghostless_home().join("recovery")
}
pub(crate) fn load_store() -> EditorStateStore {
    let text = match std::fs::read_to_string(state_path()) {
        Ok(text) => text,
        Err(_) => return EditorStateStore::default(),
    };
    match serde_json::from_str::<EditorStateStore>(&text) {
        Ok(mut state) => {
            state.ui.font_size = state.ui.font_size.clamp(13.0, 24.0);
            state
        }
        Err(e) => {
            eprintln!("ghostless-editor: cannot read editor state: {e}");
            EditorStateStore::default()
        }
    }
}
pub(crate) fn save_store(store: &EditorStateStore) -> Result<(), String> {
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs_io::atomic_write_text(&state_path(), &(text + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_and_recent_workspaces() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("ghostless-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("GHOSTLESS_HOME");
        // Environment changes are serialized across tests.
        unsafe {
            std::env::set_var("GHOSTLESS_HOME", &home);
        }
        let mut store = EditorStateStore::default();
        for n in 0..12 {
            store.record_workspace(&home.join(n.to_string()));
        }
        store.record_workspace(&home.join("5"));
        assert_eq!(store.recent_workspaces.len(), 10);
        assert_eq!(store.recent_workspaces[0], home.join("5"));
        save_store(&store).unwrap();
        assert_eq!(load_store(), store);
        std::fs::write(state_path(), "not json").unwrap();
        assert_eq!(load_store(), EditorStateStore::default());
        unsafe {
            if let Some(value) = previous {
                std::env::set_var("GHOSTLESS_HOME", value);
            } else {
                std::env::remove_var("GHOSTLESS_HOME");
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }
}
