use serde_json::Value;
use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) fn cli_command() -> Command {
    if let Some(path) = env::var_os("GHOSTLESS_CLI") {
        return Command::new(path);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // The checkout launcher is a Unix script. On Windows, run the Python
    // package beside this crate while the source tree is still present
    // (a release binary's compile path is gone, so it uses PATH).
    #[cfg(not(windows))]
    {
        let source = root.join("bin/ghostless");
        if source.is_file() {
            return Command::new(source);
        }
    }
    #[cfg(windows)]
    if root.join("lib/ghostless/__main__.py").is_file() {
        return python_module_command(&root);
    }
    Command::new("ghostless")
}

#[cfg(windows)]
pub(crate) fn python_module_command(root: &std::path::Path) -> Command {
    let python = env::var_os("GHOSTLESS_PYTHON").unwrap_or_else(|| "python".into());
    let mut command = Command::new(python);
    command.arg("-m").arg("ghostless");
    let mut paths = vec![root.join("lib")];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    if let Ok(joined) = env::join_paths(paths) {
        command.env("PYTHONPATH", joined);
    }
    command.env("GHOSTLESS_SUITE", root);
    command
}

pub(crate) fn run_cli(args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut command = cli_command();
    command
        .args(args)
        .env("GHOSTLESS_NO_UPDATE", "1")
        // Keep the CLI on UTF-8 stdin/stdout on every OS (Windows defaults to
        // the locale, which would double-encode non-ASCII working text).
        .env("PYTHONUTF8", "1");
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot launch ghostless CLI: {error}"))?;
    if let Some(value) = input {
        child
            .stdin
            .take()
            .ok_or("Cannot open CLI input")?
            .write_all(value.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

pub(crate) fn resolve_project(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return require_project(&path);
    }
    let output = run_cli(&["which"], None)?;
    let reported = output
        .lines()
        .find_map(|line| line.strip_prefix("project="));
    match reported.map(str::trim) {
        Some(value) if !value.is_empty() && value != "none" => {
            require_project(&PathBuf::from(value))
        }
        _ => Err(
            "Pass --project DIR, or run `ghostless init` in the folder where your writing lives."
                .into(),
        ),
    }
}

pub(crate) fn discover_voice_project(workspace: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return require_project(path).ok();
    }
    for ancestor in workspace.ancestors() {
        for candidate in [ancestor.to_path_buf(), ancestor.join("voice")] {
            if manifest_is_ghostless(&candidate) {
                return candidate.canonicalize().ok();
            }
        }
    }
    None
}

pub(crate) fn require_project(path: &std::path::Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!(
            "Project directory does not exist: {}. Run `ghostless init` to create one.",
            path.display()
        ));
    }
    if !path.is_dir() || !manifest_is_ghostless(path) {
        return Err(format!(
            "Not a ghostless project: {}. Run `ghostless init` to create one.",
            path.display()
        ));
    }
    path.canonicalize().map_err(|error| {
        format!(
            "Project directory does not exist: {} ({error})",
            path.display()
        )
    })
}

pub(crate) fn require_draft(path: &std::path::Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("Draft does not exist: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("Draft does not exist: {} ({error})", path.display()))
}

pub(crate) fn manifest_is_ghostless(dir: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    value.get("kind").and_then(Value::as_str) == Some("ghostless")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ENV_LOCK;

    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    struct EnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVar {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            // Tests that call the CLI hold ENV_LOCK, so no other test thread
            // reads these variables while they change.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ghostless-editor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_project(dir: &Path) {
        std::fs::write(dir.join("manifest.json"), "{\"kind\":\"ghostless\"}\n").unwrap();
    }

    #[test]
    fn missing_paths_name_the_path_and_init() {
        let missing = scratch("missing-project");
        std::fs::remove_dir_all(&missing).unwrap();
        let error = require_project(&missing).unwrap_err();
        assert!(
            error.contains("Project directory does not exist"),
            "{error}"
        );
        assert!(error.contains(&missing.display().to_string()), "{error}");
        assert!(error.contains("ghostless init"), "{error}");

        let empty = scratch("not-a-project");
        let error = require_project(&empty).unwrap_err();
        assert!(error.contains("Not a ghostless project"), "{error}");
        assert!(error.contains(&empty.display().to_string()), "{error}");
        assert!(error.contains("ghostless init"), "{error}");

        let draft = missing.join("draft.md");
        let error = require_draft(&draft).unwrap_err();
        assert!(error.contains("Draft does not exist"), "{error}");
        assert!(error.contains(&draft.display().to_string()), "{error}");
    }

    #[test]
    fn resolves_project_from_cwd_env_and_default() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let project = scratch("cwd");
        write_project(&project);
        let elsewhere = scratch("empty");
        let home = scratch("home");
        let data = home.join(".ghostless");
        std::fs::create_dir_all(&data).unwrap();
        let canonical = project.canonicalize().unwrap();

        let _cwd = CwdGuard(std::env::current_dir().unwrap());
        let _clear_project = EnvVar::set("GHOSTLESS_PROJECT", None);
        let _home = EnvVar::set("GHOSTLESS_HOME", Some(data.to_str().unwrap()));

        std::env::set_current_dir(&project).unwrap();
        let resolved = resolve_project(None).expect("cwd project");
        assert_eq!(resolved, canonical);

        std::env::set_current_dir(&elsewhere).unwrap();
        let via_env = {
            let _project_env = EnvVar::set("GHOSTLESS_PROJECT", Some(project.to_str().unwrap()));
            resolve_project(None).expect("GHOSTLESS_PROJECT")
        };
        assert_eq!(via_env, canonical);

        let payload = serde_json::json!({"defaultProject": canonical});
        std::fs::write(
            data.join("config.json"),
            serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();
        let via_default = resolve_project(None).expect("defaultProject");
        assert_eq!(via_default, canonical);
    }
}
