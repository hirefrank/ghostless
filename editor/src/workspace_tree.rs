use crate::{
    cli::manifest_is_ghostless,
    fs_io,
    prefs::{EditorStateStore, SessionLink},
    session::valid_session_id,
};
use rune_kit::{FileNode, FileTree, TreeModel, natural_cmp};
use serde::Deserialize;
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum FileKind {
    Writing,
    EditorialNote,
    Corpus,
    ReadOnlyImmutable,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceEntry {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub kind: FileKind,
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Editing,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct SessionSummary {
    pub id: String,
    pub source_name: String,
    pub section: Option<String>,
    pub origin: String,
    pub created_at: String,
    pub status: SessionStatus,
    pub session_dir: PathBuf,
    pub linked_workspace_file: Option<PathBuf>,
}

const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    ".private",
];

pub(crate) fn scan_workspace(
    root: &Path,
    voice_project: Option<&Path>,
    show_all_files: bool,
) -> TreeModel<FileNode<WorkspaceEntry>> {
    let tree = FileTree::new()
        .compact_folders(false)
        .dirs_first(true)
        .key("workspace-files");
    let Ok(root) = root.canonicalize() else {
        return tree.build(std::iter::empty::<(&str, WorkspaceEntry)>());
    };
    let voice = voice_project
        .and_then(|project| project.canonicalize().ok())
        .filter(|project| project.starts_with(&root));
    let mut ignores = Vec::new();
    if !show_all_files {
        ignores.push(IgnoreRules::load(&root));
        if let Some(voice) = &voice
            && *voice != root
        {
            ignores.push(IgnoreRules::load(voice));
        }
    }

    let mut scan = WorkspaceScan {
        root: &root,
        voice: voice.as_deref(),
        ignores,
        show_all_files,
        entries: Vec::new(),
        directories: Vec::new(),
    };
    scan.collect(&root, "");
    let mut model = tree.build(
        scan.entries
            .into_iter()
            .map(|entry| (entry.rel_path.clone(), entry)),
    );
    // FileTree builds folders from files. Insert visible folders that have no file descendants.
    let mut added = false;
    for path in scan.directories.into_iter().rev() {
        if !model.contains(&path) {
            let (parent, name) = path
                .rsplit_once('/')
                .map_or((None, path.as_str()), |(parent, name)| (Some(parent), name));
            added |= model.insert(
                parent,
                path.clone(),
                FileNode {
                    name: name.to_owned(),
                    path: path.clone(),
                    dir: true,
                    data: None,
                },
            );
        }
    }
    if added {
        model.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| natural_cmp(&a.name, &b.name))
        });
    }
    for folder in ["seeds", "drafts"] {
        if model.get(folder).is_some_and(|node| node.dir) {
            model.set_expanded(folder, true);
        }
    }
    model
}

struct WorkspaceScan<'a> {
    root: &'a Path,
    voice: Option<&'a Path>,
    ignores: Vec<IgnoreRules>,
    show_all_files: bool,
    entries: Vec<WorkspaceEntry>,
    directories: Vec<String>,
}

impl WorkspaceScan<'_> {
    fn collect(&mut self, directory: &Path, relative: &str) -> bool {
        let Ok(children) = fs::read_dir(directory) else {
            return false;
        };
        let mut visible = false;
        for item in children.flatten() {
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            let path = item.path();
            let dir = file_type.is_dir();
            if name == ".git" || dir && SKIPPED_DIRECTORIES.contains(&name) {
                continue;
            }
            if !dir && !file_type.is_file() {
                continue;
            }

            let voice_relative = self.voice.and_then(|voice| path.strip_prefix(voice).ok());
            let work_folder = dir && matches!(name, "drafts" | "seeds");
            if !self.show_all_files {
                if voice_relative.is_some_and(|path| {
                    matches!(path.to_str(), Some("sessions" | "gold" | "voice-guide.md"))
                }) {
                    continue;
                }
                // Ghostless ignores its scratch drafts in Git but they belong in the editor.
                if !work_folder
                    && self
                        .ignores
                        .iter()
                        .filter_map(|rules| rules.matches(&path, dir))
                        .next_back()
                        == Some(true)
                {
                    continue;
                }
            }

            let rel_path = if relative.is_empty() {
                name.to_owned()
            } else {
                format!("{relative}/{name}")
            };
            if dir {
                if self.collect(&path, &rel_path) || work_folder || self.show_all_files {
                    self.directories.push(rel_path);
                    visible = true;
                }
            } else if self.show_all_files || is_writing(&path) {
                let kind = file_kind(&path, voice_relative);
                let read_only = voice_relative.is_some_and(is_protected_voice_relative);
                debug_assert!(path.starts_with(self.root));
                self.entries.push(WorkspaceEntry {
                    abs_path: path,
                    rel_path,
                    kind,
                    read_only,
                });
                visible = true;
            }
        }
        visible
    }
}

fn is_writing(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        ["md", "markdown", "txt"]
            .iter()
            .any(|writing_ext| ext.eq_ignore_ascii_case(writing_ext))
    })
}

fn immutable(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    if parent.as_os_str().is_empty() {
        matches!(name, "voice-guide.md" | "voice-stats.json") || name.starts_with("edits-summary.")
    } else {
        matches!(name, "before.md" | "after.md" | "notes.md" | "receipt.json" | "session.json")
            && parent.parent() == Some(Path::new("sessions"))
    }
}

fn is_corpus_tree(relative: &Path) -> bool {
    relative.starts_with("corpus") || relative.starts_with("transcripts")
}

pub(crate) fn is_protected_voice_relative(relative: &Path) -> bool {
    relative == Path::new("manifest.json")
        || immutable(relative)
        || is_corpus_tree(relative)
        || ["gold", "holdouts", "sessions"]
            .iter()
            .any(|name| relative.starts_with(name))
}

fn file_kind(path: &Path, voice_relative: Option<&Path>) -> FileKind {
    if let Some(relative) = voice_relative {
        if immutable(relative) {
            return FileKind::ReadOnlyImmutable;
        }
        if matches!(relative.to_str(), Some("STYLE.md" | "AUDIENCE.md")) {
            return FileKind::EditorialNote;
        }
        if relative.parent().is_some_and(is_corpus_tree) {
            return FileKind::Corpus;
        }
    }
    if is_writing(path) {
        FileKind::Writing
    } else {
        FileKind::Other
    }
}

struct IgnoreRules {
    base: PathBuf,
    rules: Vec<IgnoreRule>,
}

struct IgnoreRule {
    pattern: String,
    directory_only: bool,
    anchored: bool,
    ignored: bool,
}

impl IgnoreRules {
    fn load(base: &Path) -> Self {
        let path = base.join(".gitignore");
        let contents = if is_regular_file(&path) {
            fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };
        let rules = contents
            .lines()
            .filter_map(|line| {
                let line = line.trim_end();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (pattern, ignored) = line
                    .strip_prefix('!')
                    .map_or((line, true), |pattern| (pattern, false));
                let directory_only = pattern.ends_with('/');
                let anchored = pattern.starts_with('/');
                let pattern = pattern.trim_matches('/');
                (!pattern.is_empty()).then(|| IgnoreRule {
                    pattern: pattern.to_owned(),
                    directory_only,
                    anchored: anchored || pattern.contains('/'),
                    ignored,
                })
            })
            .collect();
        Self {
            base: base.to_owned(),
            rules,
        }
    }

    fn matches(&self, path: &Path, dir: bool) -> Option<bool> {
        let relative = path.strip_prefix(&self.base).ok()?;
        self.rules.iter().rev().find_map(|rule| {
            if rule.directory_only && !dir {
                return None;
            }
            let matched = if rule.anchored {
                let parts = relative.iter().map_while(OsStr::to_str).collect::<Vec<_>>();
                match_path(&rule.pattern.split('/').collect::<Vec<_>>(), &parts)
            } else {
                relative
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| match_component(&rule.pattern, name))
            };
            matched.then_some(rule.ignored)
        })
    }
}

fn match_path(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.split_first(), path.split_first()) {
        (None, _) => path.is_empty(),
        (Some((&"**", rest)), _) => (0..=path.len()).any(|start| match_path(rest, &path[start..])),
        (Some((prefix, rest)), Some((name, tail))) => {
            match_component(prefix, name) && match_path(rest, tail)
        }
        _ => false,
    }
}

fn match_component(pattern: &str, name: &str) -> bool {
    let mut pattern = pattern.chars();
    let mut name = name.chars();
    let mut retry = None;
    loop {
        let remaining = name.clone();
        match (pattern.next(), name.next()) {
            (None, None) => return true,
            (Some('*'), _) => {
                name = remaining.clone();
                retry = Some((pattern.clone(), remaining));
            }
            (Some(p), Some(n)) if p == '?' || p == n => {}
            _ => {
                let Some((after_star, rest)) = &mut retry else {
                    return false;
                };
                if rest.next().is_none() {
                    return false;
                }
                pattern = after_star.clone();
                name = rest.clone();
            }
        }
    }
}

pub(crate) fn preserve_open_folders(
    old: &TreeModel<FileNode<WorkspaceEntry>>,
    new: &mut TreeModel<FileNode<WorkspaceEntry>>,
) {
    for folder in old
        .children(None)
        .flat_map(|path| old.subtree(path))
        .filter(|node| node.dir)
    {
        if new.get(&folder.path).is_some_and(|node| node.dir) {
            new.set_expanded(&folder.path, old.is_expanded(&folder.path));
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetadata {
    kind: String,
    id: String,
    source_draft_name: String,
    section: Option<String>,
    origin: String,
    created_at: String,
}

pub(crate) fn scan_sessions(
    voice_project: &Path,
    links: &HashMap<String, SessionLink>,
) -> Vec<SessionSummary> {
    let sessions_dir = voice_project.join("sessions");
    if !fs::symlink_metadata(&sessions_dir).is_ok_and(|meta| meta.is_dir()) {
        return Vec::new();
    }
    let Ok(folders) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<_> = folders
        .flatten()
        .filter_map(|folder| {
            if !folder.file_type().ok()?.is_dir() {
                return None;
            }
            let session_dir = folder.path();
            let path = session_dir.join("session.json");
            if !is_regular_file(&path) {
                return None;
            }
            let metadata: SessionMetadata = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
            if metadata.kind != "ghostless-edit-session"
                || !valid_session_id(&metadata.id)
                || folder.file_name() != OsStr::new(&metadata.id)
            {
                return None;
            }
            let status = if session_dir.join("receipt.json").exists() {
                SessionStatus::Finished
            } else {
                SessionStatus::Editing
            };
            let linked_workspace_file = links
                .get(&EditorStateStore::session_link_key(
                    voice_project,
                    &metadata.id,
                ))
                .map(|link| link.workspace_file.clone());
            Some(SessionSummary {
                id: metadata.id,
                source_name: metadata.source_draft_name,
                section: metadata.section,
                origin: metadata.origin,
                created_at: metadata.created_at,
                status,
                session_dir,
                linked_workspace_file,
            })
        })
        .collect();
    sessions.sort_by(|a, b| {
        (a.status == SessionStatus::Finished)
            .cmp(&(b.status == SessionStatus::Finished))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| b.id.cmp(&a.id))
    });
    sessions
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', '\0'])
        || Path::new(name).file_name() != Some(OsStr::new(name))
        || cfg!(windows) && name.contains(':')
    {
        return Err(format!("Invalid workspace name: {name:?}"));
    }
    Ok(())
}

fn checked_path(root: &Path, path: PathBuf) -> Result<PathBuf, String> {
    if !fs_io::is_contained_in(root, &path) {
        return Err(format!("Path is outside the workspace: {}", path.display()));
    }
    Ok(path)
}

fn entry_path(root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    for part in rel_path.split('/') {
        validate_name(part)?;
    }
    checked_path(root, root.join(rel_path))
}

fn new_path(root: &Path, parent_rel: &str, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let parent = if parent_rel.is_empty() {
        root.to_owned()
    } else {
        entry_path(root, parent_rel)?
    };
    checked_path(root, parent.join(name))
}

fn path_error(path: &Path, error: io::Error) -> String {
    format!("{}: {error}", path.display())
}

pub(crate) fn create_file(root: &Path, parent_rel: &str, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let name = if Path::new(name).extension().is_none() {
        format!("{name}.md")
    } else {
        name.to_owned()
    };
    let path = new_path(root, parent_rel, &name)?;
    ensure_creation_allowed(&path)?;
    // Exclusive creation publishes an empty file in one operation, even if another writer is creating it.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| path_error(&path, error))?;
    Ok(path)
}

pub(crate) fn create_folder(root: &Path, parent_rel: &str, name: &str) -> Result<PathBuf, String> {
    let path = new_path(root, parent_rel, name)?;
    ensure_creation_allowed(&path)?;
    fs::create_dir(&path).map_err(|error| path_error(&path, error))?;
    Ok(path)
}

fn in_protected_voice_project(path: &Path) -> bool {
    path.ancestors().skip(1).any(|base| {
        path.strip_prefix(base).is_ok_and(|relative| {
            is_protected_voice_relative(relative)
                && base.join("manifest.json").is_file()
                && manifest_is_ghostless(base)
        })
    })
}

pub(crate) fn is_protected_voice_path(path: &Path) -> bool {
    if in_protected_voice_project(path) {
        return true;
    }
    let resolved = path.canonicalize().ok().or_else(|| {
        let name = path.file_name()?;
        Some(path.parent()?.canonicalize().ok()?.join(name))
    });
    resolved.as_deref().is_some_and(in_protected_voice_project)
}

fn ensure_creation_allowed(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("Destination has no parent")?;
    let parent = parent.canonicalize().map_err(|error| path_error(parent, error))?;
    let target = parent.join(path.file_name().ok_or("Destination has no name")?);
    if is_protected_voice_path(&target) {
        return Err(format!("Protected voice data: {}", target.display()));
    }
    Ok(())
}

fn ensure_mutable(path: &Path) -> Result<(), String> {
    let kind = fs::symlink_metadata(path)
        .map_err(|error| path_error(path, error))?
        .file_type();
    if path.file_name() == Some(OsStr::new("manifest.json")) || is_protected_voice_path(path) {
        return Err(format!("Protected workspace file: {}", path.display()));
    }
    if kind.is_dir() {
        for item in fs::read_dir(path).map_err(|error| path_error(path, error))? {
            let item = item.map_err(|error| path_error(path, error))?;
            ensure_mutable(&item.path())?;
        }
    }
    Ok(())
}

pub(crate) fn rename_entry(root: &Path, rel_path: &str, new_name: &str) -> Result<PathBuf, String> {
    validate_name(new_name)?;
    let root = root
        .canonicalize()
        .map_err(|error| path_error(root, error))?;
    let source = entry_path(&root, rel_path)?;
    ensure_mutable(&source)?;
    let target = checked_path(&root, source.with_file_name(new_name))?;
    if source != target {
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(format!(
                    "Workspace entry already exists: {}",
                    target.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(path_error(&target, error)),
        }
        ensure_creation_allowed(&target)?;
        fs::rename(&source, &target).map_err(|error| path_error(&source, error))?;
    }
    Ok(target)
}

pub(crate) fn duplicate_file(root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    duplicate_file_with_limit(root, rel_path, 10_000)
}

fn duplicate_file_with_limit(root: &Path, rel_path: &str, limit: u32) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|error| path_error(root, error))?;
    let source = entry_path(&root, rel_path)?;
    ensure_mutable(&source)?;
    if !source.is_file() {
        return Err(format!(
            "Workspace entry is not a file: {}",
            source.display()
        ));
    }
    let stem = source
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or("Invalid workspace filename")?;
    let extension = source.extension().and_then(OsStr::to_str).map(|ext| format!(".{ext}")).unwrap_or_default();
    let bytes = fs::read(&source).map_err(|error| path_error(&source, error))?;
    for number in 1..=limit {
        let name = if number == 1 {
            format!("{stem}-copy{extension}")
        } else {
            format!("{stem}-copy-{number}{extension}")
        };
        let target = source.with_file_name(name);
        // Skip occupied names before checking containment, including symlinks to another workspace.
        match fs::symlink_metadata(&target) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(path_error(&target, error)),
        }
        let target = checked_path(&root, target)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&bytes) {
                    drop(file);
                    let _ = fs::remove_file(&target);
                    return Err(path_error(&target, error));
                }
                return Ok(target);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(path_error(&target, error)),
        }
    }
    Err("Could not find an available copy filename".into())
}

pub(crate) fn delete_entry(root: &Path, rel_path: &str) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|error| path_error(root, error))?;
    let path = entry_path(&root, rel_path)?;
    ensure_mutable(&path)?;
    let meta = fs::symlink_metadata(&path).map_err(|error| path_error(&path, error))?;
    if meta.is_dir() {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_file(&path)
    }
    .map_err(|error| path_error(&path, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Workspace(PathBuf);

    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let root = std::env::temp_dir().join(format!(
                    "ghostless-workspace-tree-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&root) {
                    Ok(()) => return Self(root.canonicalize().unwrap()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("{error}"),
                }
            }
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn session(&self, id: &str) {
            self.write(
                &format!("voice/sessions/{id}/session.json"),
                &serde_json::json!({
                    "kind": "ghostless-edit-session",
                    "id": id,
                    "createdAt": format!("{}T14:00:00Z", &id[..10]),
                    "sourceDraftName": "draft.md",
                    "section": "essays",
                    "origin": "writer-first"
                })
                .to_string(),
            );
        }

        fn fixture() -> Self {
            let workspace = Self::new();
            for path in [
                "seeds/demo/NEXT.md",
                "seeds/demo/questions.md",
                "voice/STYLE.md",
                "voice/AUDIENCE.md",
                "voice/corpus/gold/a.md",
                "voice/transcripts/a.txt",
                "voice/holdouts/b.markdown",
                "voice/drafts/a.md",
                "voice/gold/a.md",
                "voice/sessions/2026-11-23/before.md",
                "voice/sessions/2026-11-23/after.md",
                "voice/sessions/2026-11-23/notes.md",
                "voice/raw/clean.json",
                "voice/raw/transcription.md",
                "voice/voice-guide.md",
                "voice/voice-stats.json",
                "voice/edits-summary.md",
                "voice/corpus/report.tmp",
                "legacy/old.py",
                ".private/secret.txt",
            ] {
                workspace.write(path, "");
            }
            workspace.write(".gitignore", "seeds/**/skip?.md\n");
            workspace.write("seeds/demo/skip1.md", "");
            workspace.write(
                "voice/.gitignore",
                "/raw/\n/drafts/\n*.tmp\nvoice-stats.json\nedits-summary.*\n",
            );
            workspace.write("voice/manifest.json", r#"{"kind":"ghostless"}"#);
            workspace.session("2026-11-23");
            fs::create_dir(workspace.0.join("seeds/empty")).unwrap();
            fs::create_dir(workspace.0.join("drafts")).unwrap();
            workspace
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scans_writing_and_keeps_workspace_folders() {
        let workspace = Workspace::fixture();
        let voice = workspace.0.join("voice");
        let tree = scan_workspace(&workspace.0, Some(&voice), false);
        for path in [
            "seeds/demo/NEXT.md",
            "seeds/demo/questions.md",
            "voice/STYLE.md",
            "voice/AUDIENCE.md",
            "voice/corpus/gold/a.md",
            "voice/transcripts/a.txt",
            "voice/holdouts/b.markdown",
            "voice/drafts/a.md",
            "drafts",
        ] {
            assert!(tree.contains(path), "{path}");
        }
        assert!(tree.get("drafts").unwrap().dir);
        assert!(tree.is_expanded("drafts"));
        for path in [
            ".private",
            "voice/raw",
            "legacy/old.py",
            "voice/sessions",
            "voice/gold",
            "voice/voice-guide.md",
            "voice/edits-summary.md",
            "seeds/demo/skip1.md",
            "seeds/empty",
        ] {
            assert!(!tree.contains(path), "{path}");
        }
        assert_eq!(
            tree.get("voice/STYLE.md")
                .unwrap()
                .data
                .as_ref()
                .unwrap()
                .kind,
            FileKind::EditorialNote
        );
        let corpus = tree
            .get("voice/corpus/gold/a.md")
            .unwrap()
            .data
            .as_ref()
            .unwrap();
        assert_eq!(corpus.kind, FileKind::Corpus);
        assert!(corpus.read_only);
        assert_eq!(corpus.abs_path, voice.join("corpus/gold/a.md"));
        assert_eq!(corpus.rel_path, "voice/corpus/gold/a.md");
        let transcript = tree.get("voice/transcripts/a.txt").unwrap();
        assert_eq!(transcript.data.as_ref().unwrap().kind, FileKind::Corpus);
        assert!(transcript.data.as_ref().unwrap().read_only);
        let holdout = tree.get("voice/holdouts/b.markdown").unwrap();
        assert!(holdout.data.as_ref().unwrap().read_only);
        let draft = tree.get("voice/drafts/a.md").unwrap();
        assert!(!draft.data.as_ref().unwrap().read_only);

        let tree = scan_workspace(&workspace.0, Some(&voice), true);
        assert!(tree.contains("legacy/old.py"));
        assert!(tree.contains("voice/raw/clean.json"));
        assert!(tree.contains("seeds/empty"));
        assert!(!tree.contains(".private"));
        for path in [
            "voice/sessions/2026-11-23/before.md",
            "voice/sessions/2026-11-23/after.md",
            "voice/sessions/2026-11-23/notes.md",
            "voice/sessions/2026-11-23/session.json",
            "voice/voice-guide.md",
            "voice/voice-stats.json",
            "voice/edits-summary.md",
        ] {
            let entry = tree.get(path).unwrap().data.as_ref().unwrap();
            assert_eq!(entry.kind, FileKind::ReadOnlyImmutable, "{path}");
            assert!(entry.read_only, "{path}");
        }
    }

    #[test]
    fn lists_valid_sessions_with_links_and_receipt_status() {
        let workspace = Workspace::fixture();
        let voice = workspace.0.join("voice");
        workspace.write("voice/sessions/2026-11-22/session.json", "invalid");
        let original_id = "2026-11-23";
        let links = HashMap::from([(
            EditorStateStore::session_link_key(&voice, original_id),
            SessionLink {
                workspace_file: "draft.md".into(),
                last_synced_sha256: String::new(),
            },
        )]);
        let sessions = scan_sessions(&voice, &links);
        assert_eq!(
            sessions,
            vec![SessionSummary {
                id: original_id.to_owned(),
                source_name: "draft.md".into(),
                section: Some("essays".into()),
                origin: "writer-first".into(),
                created_at: "2026-11-23T14:00:00Z".into(),
                status: SessionStatus::Editing,
                session_dir: voice.join("sessions/2026-11-23"),
                linked_workspace_file: Some("draft.md".into()),
            }]
        );
        workspace.write("voice/sessions/2026-11-23/receipt.json", "{}");
        workspace.session("2026-11-20");
        workspace.session("2026-11-20-2");
        workspace.session("2026-11-21");
        let sessions = scan_sessions(&voice, &links);
        let ids: Vec<_> = sessions.iter().map(|session| session.id.as_str()).collect();
        assert_eq!(
            ids,
            ["2026-11-21", "2026-11-20-2", "2026-11-20", original_id]
        );
        assert_eq!(sessions.last().unwrap().status, SessionStatus::Finished);
    }

    #[test]
    fn retains_nested_expansion_on_rescan() {
        let workspace = Workspace::fixture();
        let mut old = scan_workspace(&workspace.0, None, false);
        assert!(old.is_expanded("seeds"));
        old.set_expanded("seeds/demo", true);
        old.set_expanded("seeds", false);
        workspace.write("seeds/demo/another.md", "");
        let mut new = scan_workspace(&workspace.0, None, false);
        preserve_open_folders(&old, &mut new);
        assert!(!new.is_expanded("seeds"));
        assert!(new.is_expanded("seeds/demo"));
        assert!(new.contains("seeds/demo/another.md"));
    }

    #[test]
    fn duplicate_reports_when_all_names_are_taken() {
        let workspace = Workspace::fixture();
        let root = &workspace.0;
        workspace.write("draft.md", "original");
        workspace.write("draft-copy.md", "first");
        workspace.write("draft-copy-2.md", "second");
        assert!(duplicate_file_with_limit(root, "draft.md", 2).is_err());
        assert_eq!(fs::read_to_string(root.join("draft-copy.md")).unwrap(), "first");
    }

    #[test]
    fn creates_renames_duplicates_and_deletes_workspace_files() {
        let workspace = Workspace::fixture();
        let root = &workspace.0;
        workspace.write("sample.txt", "plain");
        workspace.write("sample.markdown", "markdown");
        workspace.write("extensionless", "text");
        assert_eq!(duplicate_file(root, "sample.txt").unwrap(), root.join("sample-copy.txt"));
        assert_eq!(duplicate_file(root, "sample.markdown").unwrap(), root.join("sample-copy.markdown"));
        assert_eq!(duplicate_file(root, "extensionless").unwrap(), root.join("extensionless-copy"));
        let folder = create_folder(root, "", "work").unwrap();
        let path = create_file(root, "work", "draft").unwrap();
        assert_eq!(path, folder.join("draft.md"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        fs::write(&path, "雪 café").unwrap();
        assert!(create_file(root, "work", "draft").is_err());
        let renamed = rename_entry(root, "work/draft.md", "chapter.md").unwrap();
        assert_eq!(renamed, folder.join("chapter.md"));
        assert!(!path.exists());
        let copy = duplicate_file(root, "work/chapter.md").unwrap();
        let second = duplicate_file(root, "work/chapter.md").unwrap();
        assert_eq!(copy, folder.join("chapter-copy.md"));
        assert_eq!(second, folder.join("chapter-copy-2.md"));
        assert_eq!(fs::read_to_string(copy).unwrap(), "雪 café");
        assert!(rename_entry(root, "work/chapter.md", "chapter-copy.md").is_err());
        delete_entry(root, "work/chapter-copy.md").unwrap();
        delete_entry(root, "work").unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn rejects_traversal_and_protects_immutable_files() {
        let workspace = Workspace::fixture();
        let root = &workspace.0;
        for name in ["", "../escape", "a/b", "a\\b", ".."] {
            assert!(create_file(root, "", name).is_err());
            assert!(create_folder(root, "", name).is_err());
            assert!(rename_entry(root, "seeds/demo/NEXT.md", name).is_err());
        }
        for parent in [
            "voice/gold",
            "voice/holdouts",
            "voice/corpus",
            "voice/corpus/gold",
            "voice/transcripts",
            "voice/sessions",
            "voice/sessions/2026-11-23",
        ] {
            assert!(create_file(root, parent, "new").is_err(), "{parent}");
            assert!(create_folder(root, parent, "new").is_err(), "{parent}");
            assert!(!root.join(parent).join("new.md").exists());
            assert!(!root.join(parent).join("new").exists());
        }
        for parent in ["voice/corpus", "voice/transcripts"] {
            let opened_subfolder = root.join(parent);
            assert!(create_file(&opened_subfolder, "", "new").is_err(), "{parent}");
            assert!(create_folder(&opened_subfolder, "", "new").is_err(), "{parent}");
        }
        assert!(delete_entry(&root.join("voice/corpus"), "gold/a.md").is_err());
        assert!(create_file(root, "voice/drafts", "new").is_ok());
        assert!(create_folder(root, "", "transcripts").is_ok());
        assert!(create_file(root, "transcripts", "ordinary").is_ok());
        assert!(create_file(root, "../escape", "draft").is_err());
        assert!(delete_entry(root, "").is_err());
        assert!(delete_entry(root, "seeds/../voice/manifest.json").is_err());
        assert!(delete_entry(root, "voice/manifest.json").is_err());
        assert!(rename_entry(root, "voice/manifest.json", "renamed.json").is_err());
        assert!(duplicate_file(root, "voice/manifest.json").is_err());
        assert!(delete_entry(root, "voice").is_err());
        for path in [
            "voice/voice-guide.md",
            "voice/gold/a.md",
            "voice/holdouts/b.markdown",
            "voice/corpus/gold/a.md",
            "voice/transcripts/a.txt",
            "voice/gold",
            "voice/holdouts",
            "voice/corpus",
            "voice/transcripts",
            "voice/sessions/2026-11-23/before.md",
            "voice/sessions/2026-11-23/after.md",
            "voice/sessions/2026-11-23/notes.md",
            "voice/sessions/2026-11-23",
        ] {
            assert!(delete_entry(root, path).is_err(), "{path}");
            assert!(rename_entry(root, path, "renamed").is_err(), "{path}");
            if root.join(path).is_file() {
                assert!(duplicate_file(root, path).is_err(), "{path}");
            }
            assert!(root.join(path).exists(), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn declines_paths_through_external_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = Workspace::fixture();
        let outside = Workspace::new();
        outside.write("draft.md", "outside");
        symlink(&outside.0, workspace.0.join("alias")).unwrap();
        for result in [
            create_file(&workspace.0, "alias", "leak"),
            create_folder(&workspace.0, "alias", "outside"),
            rename_entry(&workspace.0, "alias/draft.md", "outside.md"),
            duplicate_file(&workspace.0, "alias/draft.md"),
        ] {
            assert!(result.is_err());
        }
        assert!(delete_entry(&workspace.0, "alias/draft.md").is_err());
        assert_eq!(
            fs::read_to_string(outside.0.join("draft.md")).unwrap(),
            "outside"
        );
        assert!(!scan_workspace(&workspace.0, None, true).contains("alias"));
    }
}
