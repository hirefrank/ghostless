//! RAII temp directories for editor tests. Drop removes the directory even when
//! a test panics, so failures do not leak `std::env::temp_dir()` entries.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ghostless-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
        Self { path }
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
