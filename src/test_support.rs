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
