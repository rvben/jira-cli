use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const ENV_LOCK_FILE: &str = "jira-cli-test-env.lock";
const ENV_LOCK_TIMEOUT: Duration = Duration::from_secs(15);
const STALE_LOCK_AGE: Duration = Duration::from_secs(60);

pub struct ProcessEnvLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl ProcessEnvLock {
    pub fn acquire() -> io::Result<Self> {
        let path = std::env::temp_dir().join(ENV_LOCK_FILE);
        let deadline = Instant::now() + ENV_LOCK_TIMEOUT;

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Self { path, _file: file }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    clear_stale_lock(&path);
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("timed out waiting for env lock at {}", path.display()),
                        ));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Drop for ProcessEnvLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct EnvVarGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    pub fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var(name).ok();
        // SAFETY: tests acquire ProcessEnvLock before mutating process environment.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }

    pub fn unset(name: &'static str) -> Self {
        let previous = std::env::var(name).ok();
        // SAFETY: tests acquire ProcessEnvLock before mutating process environment.
        unsafe { std::env::remove_var(name) };
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => {
                // SAFETY: tests acquire ProcessEnvLock before mutating process environment.
                unsafe { std::env::set_var(self.name, value) };
            }
            None => {
                // SAFETY: tests acquire ProcessEnvLock before mutating process environment.
                unsafe { std::env::remove_var(self.name) };
            }
        }
    }
}

pub fn set_config_dir_env(path: &Path) -> EnvVarGuard {
    EnvVarGuard::set(config_dir_env_name(), path.to_string_lossy().as_ref())
}

pub fn unset_config_dir_env() -> EnvVarGuard {
    EnvVarGuard::unset(config_dir_env_name())
}

pub fn write_config(dir: &Path, body: &str) -> io::Result<PathBuf> {
    let path = dir.join("jira").join("config.toml");
    fs::create_dir_all(path.parent().unwrap_or(dir))?;
    fs::write(&path, body)?;
    Ok(path)
}

/// The environment variable config discovery reads to locate the config
/// directory on this platform.
///
/// Tests that spawn the binary set it on the child rather than on themselves,
/// which is why this is public: handing the child its own environment means
/// they need no `ProcessEnvLock` and cannot inherit a real token from the
/// developer's shell.
pub fn config_dir_env_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "APPDATA"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "XDG_CONFIG_HOME"
    }
}

fn clear_stale_lock(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return;
    };

    if age >= STALE_LOCK_AGE {
        let _ = fs::remove_file(path);
    }
}

/// Prepare `cmd` to run `jira` in isolation: its config directory is
/// `config_dir`, and no `JIRA_*` variable from the environment running the
/// tests reaches it, so a developer's own settings cannot change a result.
/// Variables the test sets on `cmd` afterwards still apply.
pub fn isolate_command<'a>(
    cmd: &'a mut std::process::Command,
    config_dir: &Path,
) -> &'a mut std::process::Command {
    remove_jira_vars(cmd, std::env::vars_os().map(|(name, _)| name));
    cmd.env(config_dir_env_name(), config_dir)
}

fn remove_jira_vars(
    cmd: &mut std::process::Command,
    names: impl Iterator<Item = std::ffi::OsString>,
) {
    for name in names {
        if name.to_string_lossy().starts_with("JIRA_") {
            cmd.env_remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn only_jira_variables_are_removed() {
        let mut cmd = std::process::Command::new("jira");
        let inherited = ["JIRA_THEME", "JIRA_SOMETHING_NEW", "HOME", "NOT_JIRA_HOST"];
        remove_jira_vars(&mut cmd, inherited.iter().map(OsString::from));
        let removed: Vec<&OsStr> = cmd
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(removed, ["JIRA_SOMETHING_NEW", "JIRA_THEME"]);
    }

    #[test]
    fn variables_set_afterwards_still_apply() {
        let mut cmd = std::process::Command::new("jira");
        remove_jira_vars(&mut cmd, [OsString::from("JIRA_HOST")].into_iter());
        cmd.env("JIRA_HOST", "example.atlassian.net");
        let host = cmd.get_envs().find(|(name, _)| *name == "JIRA_HOST");
        assert_eq!(
            host,
            Some((
                OsStr::new("JIRA_HOST"),
                Some(OsStr::new("example.atlassian.net"))
            ))
        );
    }
}
