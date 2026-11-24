use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn read_utf8(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn atomic_write_text(path: &Path, content: &str) -> Result<(), String> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let (temp, mut file) = loop {
        let temp = parent.join(format!(".ghostless-tmp-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let result = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        replace(&temp, path)
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result.map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(windows))]
fn replace(temp: &Path, path: &Path) -> std::io::Result<()> { fs::rename(temp, path) }
#[cfg(windows)]
fn replace(temp: &Path, path: &Path) -> std::io::Result<()> {
    match fs::rename(temp, path) {
        Ok(()) => Ok(()),
        Err(_e) if path.is_file() => {
            let backup = temp.with_extension("backup");
            fs::rename(path, &backup)?;
            match fs::rename(temp, path) {
                Ok(()) => { let _ = fs::remove_file(backup); Ok(()) },
                Err(err) => { let _ = fs::rename(backup, path); Err(err) }
            }
        },
        Err(e) => Err(e),
    }
}

pub(crate) fn sha256_text(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }

pub(crate) fn is_contained_in(root: &Path, candidate: &Path) -> bool {
    let Ok(root) = root.canonicalize() else { return false };
    let mut probe = candidate.to_path_buf();
    let mut tail = Vec::new();
    while !probe.exists() {
        let Some(name) = probe.file_name() else { return false };
        tail.push(name.to_owned());
        if !probe.pop() { return false; }
    }
    let Ok(mut resolved) = probe.canonicalize() else { return false };
    for part in tail.iter().rev() { resolved.push(part); }
    // Normalize lexical traversal after resolving the nearest existing parent.
    let mut normalized = PathBuf::new();
    for part in resolved.components() {
        match part { std::path::Component::ParentDir => { normalized.pop(); }, _ => normalized.push(part.as_os_str()) }
    }
    normalized.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_utf8_hash_and_containment() {
        let base = std::env::temp_dir().join(format!("ghostless-io-{}", std::process::id()));
        let file = base.join("child/draft.md");
        atomic_write_text(&file, "雪 café 🖋️").unwrap();
        assert_eq!(read_utf8(&file).unwrap(), "雪 café 🖋️");
        assert_eq!(sha256_text("hello\n"), "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03");
        assert!(is_contained_in(&base, &file));
        assert!(!is_contained_in(&base, &base.join("../escape")));
        assert!(!fs::read_dir(file.parent().unwrap()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ghostless-tmp-")));
        fs::remove_dir_all(base).unwrap();
    }
}
