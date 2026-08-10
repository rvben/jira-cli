use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::api::ApiError;
use crate::api::AuthType;
use crate::output::OutputConfig;

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProfileConfig {
    pub host: Option<String>,
    pub email: Option<String>,
    pub token: Option<String>,
    pub auth_type: Option<String>,
    pub api_version: Option<u8>,
    pub read_only: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    default: ProfileConfig,
    #[serde(default)]
    profiles: BTreeMap<String, ProfileConfig>,
    host: Option<String>,
    email: Option<String>,
    token: Option<String>,
    auth_type: Option<String>,
    api_version: Option<u8>,
    read_only: Option<bool>,
}

impl RawConfig {
    fn default_profile(&self) -> ProfileConfig {
        ProfileConfig {
            host: self.default.host.clone().or_else(|| self.host.clone()),
            email: self.default.email.clone().or_else(|| self.email.clone()),
            token: self.default.token.clone().or_else(|| self.token.clone()),
            auth_type: self
                .default
                .auth_type
                .clone()
                .or_else(|| self.auth_type.clone()),
            api_version: self.default.api_version.or(self.api_version),
            read_only: self.default.read_only.or(self.read_only),
        }
    }
}

/// Resolved credentials for a single profile.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub email: String,
    pub token: String,
    pub auth_type: AuthType,
    pub api_version: u8,
    pub read_only: bool,
}

impl Config {
    /// Load config with priority: CLI args > env vars > config file.
    ///
    /// The API token must be supplied via the `JIRA_TOKEN` environment variable
    /// or the config file — not via a CLI flag, to avoid leaking it in process
    /// argument lists visible to other users.
    pub fn load(
        host_arg: Option<String>,
        email_arg: Option<String>,
        profile_arg: Option<String>,
    ) -> Result<Self, ApiError> {
        let file_profile = load_file_profile(profile_arg.as_deref())?;

        let host = normalize_value(host_arg)
            .or_else(|| env_var("JIRA_HOST"))
            .or_else(|| normalize_value(file_profile.host))
            .ok_or_else(|| {
                ApiError::InvalidInput(
                    "No Jira host configured. Set JIRA_HOST or run `jira config init`.".into(),
                )
            })?;

        let token = env_var("JIRA_TOKEN")
            .or_else(|| normalize_value(file_profile.token.clone()))
            .ok_or_else(|| {
                ApiError::InvalidInput(
                    "No API token configured. Set JIRA_TOKEN or run `jira config init`.".into(),
                )
            })?;

        let auth_type = env_var("JIRA_AUTH_TYPE")
            .as_deref()
            .map(|v| {
                if v.eq_ignore_ascii_case("pat") {
                    AuthType::Pat
                } else {
                    AuthType::Basic
                }
            })
            .or_else(|| {
                file_profile.auth_type.as_deref().map(|v| {
                    if v.eq_ignore_ascii_case("pat") {
                        AuthType::Pat
                    } else {
                        AuthType::Basic
                    }
                })
            })
            .unwrap_or_default();

        let api_version = env_var("JIRA_API_VERSION")
            .and_then(|v| v.parse::<u8>().ok())
            .or(file_profile.api_version)
            .unwrap_or(3);

        // Email is required for Basic auth; PAT auth uses a token only.
        let email = normalize_value(email_arg)
            .or_else(|| env_var("JIRA_EMAIL"))
            .or_else(|| normalize_value(file_profile.email));

        let email = match auth_type {
            AuthType::Basic => email.ok_or_else(|| {
                ApiError::InvalidInput(
                    "No email configured. Set JIRA_EMAIL or run `jira config init`.".into(),
                )
            })?,
            AuthType::Pat => email.unwrap_or_default(),
        };

        let read_only = env_var("JIRA_READ_ONLY")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .or(file_profile.read_only)
            .unwrap_or(false);

        Ok(Self {
            host,
            email,
            token,
            auth_type,
            api_version,
            read_only,
        })
    }
}

/// Render the set of selectable profile names. An empty set is named
/// explicitly, so a config with no named profiles never produces a message
/// ending in a bare `Available:` that reads as a truncated list.
fn format_available(names: &[&str]) -> String {
    if names.is_empty() {
        "none defined".to_string()
    } else {
        names.join(", ")
    }
}

fn config_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("jira")
        .join("config.toml")
}

pub fn schema_config_path() -> String {
    config_path().display().to_string()
}

pub fn schema_config_path_description() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Resolved at runtime to %APPDATA%\\jira\\config.toml by default."
    }

    #[cfg(not(target_os = "windows"))]
    {
        "Resolved at runtime to $XDG_CONFIG_HOME/jira/config.toml when set, otherwise ~/.config/jira/config.toml."
    }
}

pub fn recommended_permissions(path: &std::path::Path) -> String {
    #[cfg(target_os = "windows")]
    {
        format!(
            "Store this file in your per-user AppData directory ({}) and keep it out of shared folders; Windows applies per-user ACLs there by default.",
            path.display()
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        format!("chmod 600 {}", path.display())
    }
}

pub fn schema_recommended_permissions_example() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Keep the file in your per-user %APPDATA% directory and out of shared folders."
    }

    #[cfg(not(target_os = "windows"))]
    {
        "chmod 600 /path/to/config.toml"
    }
}

/// The `dcPatInstructions` value `init --json` prints when no host is known.
///
/// Rendered by the same function the command uses, so the schema example cannot
/// drift from the URL a Data Center user is actually handed.
pub fn schema_dc_pat_url_example() -> String {
    dc_pat_url(None)
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    }
}

fn load_file_profile(profile: Option<&str>) -> Result<ProfileConfig, ApiError> {
    let path = config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ProfileConfig::default()),
        Err(e) => return Err(ApiError::Other(format!("Failed to read config: {e}"))),
    };

    let raw: RawConfig = toml::from_str(&content)
        .map_err(|e| ApiError::Other(format!("Failed to parse config: {e}")))?;

    let profile_name = normalize_str(profile)
        .map(str::to_owned)
        .or_else(|| env_var("JIRA_PROFILE"));

    match profile_name {
        Some(name) => {
            // BTreeMap gives sorted, deterministic output in error messages
            let available: Vec<&str> = raw.profiles.keys().map(String::as_str).collect();
            raw.profiles.get(&name).cloned().ok_or_else(|| {
                ApiError::NotFound(format!(
                    "profile '{name}' in config. Available: {}",
                    format_available(&available)
                ))
            })
        }
        None => Ok(raw.default_profile()),
    }
}

/// Print the config file path and current resolved values (masking the token).
pub fn show(
    out: &OutputConfig,
    host_arg: Option<String>,
    email_arg: Option<String>,
    profile_arg: Option<String>,
) -> Result<(), ApiError> {
    let path = config_path();
    let cfg = Config::load(host_arg, email_arg, profile_arg)?;
    let masked = mask_token(&cfg.token);

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "configPath": path,
                "host": cfg.host,
                "email": cfg.email,
                "tokenMasked": masked,
            }))
            .expect("failed to serialize JSON"),
        );
    } else {
        out.print_message(&format!("Config file: {}", path.display()));
        out.print_data(&format!(
            "host:  {}\nemail: {}\ntoken: {masked}",
            cfg.host, cfg.email
        ));
    }
    Ok(())
}

/// Interactively set up the config file, or print JSON instructions when `--json` is used.
///
/// In JSON mode the function prints a machine-readable instructions object and returns.
/// In an interactive terminal it prompts for Jira type, host, credentials, and profile
/// name, verifies the credentials against the API, then writes (or updates)
/// `~/.config/jira/config.toml`.
pub async fn init(out: &OutputConfig, host: Option<&str>) {
    if out.json {
        init_json(out, host);
        return;
    }

    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        out.print_message(
            "Run `jira init` in an interactive terminal to configure credentials, \
             or use `jira init --json` for setup instructions.",
        );
        return;
    }

    if let Err(e) = init_interactive(host).await {
        eprintln!("{} {e}", sym_fail());
        std::process::exit(crate::output::exit_codes::GENERAL_ERROR);
    }
}

/// The example config `jira init --json` prints, and the same value `jira schema`
/// shows as the shape of that field.
///
/// One source, because these were two hand-maintained copies and the schema's had
/// already fallen behind: it showed neither `auth_type` nor `api_version`, so the
/// Data Center profile a reader needs in order to use a PAT was invisible there.
pub fn schema_example_config() -> serde_json::Value {
    serde_json::json!({
        "default": {
            "host": "mycompany.atlassian.net",
            "email": "me@example.com",
            "token": "your-api-token",
            "auth_type": "basic",
            "api_version": 3,
        },
        "profiles": {
            "work": {
                "host": "work.atlassian.net",
                "email": "me@work.com",
                "token": "work-token",
            },
            "datacenter": {
                "host": "jira.mycompany.com",
                "token": "your-personal-access-token",
                "auth_type": "pat",
                "api_version": 2,
            }
        }
    })
}

fn init_json(out: &OutputConfig, host: Option<&str>) {
    let path = config_path();
    let path_resolution = schema_config_path_description();
    let permission_advice = recommended_permissions(&path);
    let example = schema_example_config();

    const CLOUD_TOKEN_URL: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";
    let pat_url = dc_pat_url(host);

    out.print_data(
        &serde_json::to_string_pretty(&serde_json::json!({
            "configPath": path,
            "pathResolution": path_resolution,
            "configExists": path.exists(),
            "tokenInstructions": CLOUD_TOKEN_URL,
            "dcPatInstructions": pat_url,
            "recommendedPermissions": permission_advice,
            "example": example,
        }))
        .expect("failed to serialize JSON"),
    );
}

async fn init_interactive(prefill_host: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let sep = sym_dim("──────────────");
    eprintln!("Jira CLI Setup");
    eprintln!("{sep}");

    let path = config_path();

    // Decide what to do: first run, update an existing profile, or add a new one.
    //
    // `target_name` holds the profile name to write:
    //   Some(name) — already known (first run → "default"; update → chosen name)
    //   None       — "add new" path, ask for name after credentials
    let (target_name, existing): (Option<String>, Option<ProfileConfig>) = if path.exists() {
        let profiles = list_profile_names(&path)?;

        // Show the config path and each profile with its host so the user knows
        // what exists before deciding whether to update or add.
        eprintln!();
        eprintln!(
            "  {} {}",
            sym_dim("Config:"),
            sym_dim(&path.display().to_string())
        );
        eprintln!();
        eprintln!("  {}:", sym_dim("Profiles"));
        for name in &profiles {
            let host = read_raw_profile(&path, name)
                .ok()
                .and_then(|p| p.host)
                .unwrap_or_default();
            eprintln!("    {} {}  {}", sym_dim("•"), name, sym_dim(&host));
        }
        eprintln!();

        let action = prompt("Action", "[update/add]", Some("update"))?;
        eprintln!();

        if !action.trim().eq_ignore_ascii_case("add") {
            let default = profiles.first().map(String::as_str).unwrap_or("default");
            let raw = if profiles.len() > 1 {
                prompt("Profile", "", Some(default))?
            } else {
                default.to_owned()
            };
            let name = if raw.trim().is_empty() {
                default.to_owned()
            } else {
                raw.trim().to_owned()
            };
            let cfg = read_raw_profile(&path, &name)?;
            if profiles.len() > 1 {
                eprintln!();
            }
            (Some(name), Some(cfg))
        } else {
            (None, None)
        }
    } else {
        // First run: silently use "default", no need to ask.
        eprintln!();
        (Some("default".to_owned()), None)
    };

    // Instance type — derive from existing config, or ask.
    let is_cloud = if let Some(ref p) = existing {
        p.auth_type.as_deref() != Some("pat")
    } else {
        let t = prompt("Type", sym_dim("[cloud/dc]").as_str(), Some("cloud"))?;
        eprintln!();
        !t.trim().eq_ignore_ascii_case("dc")
    };

    // Host
    let host = if is_cloud {
        let default_sub = existing
            .as_ref()
            .and_then(|p| p.host.clone())
            .as_deref()
            .or(prefill_host)
            .map(|h| h.trim_end_matches(".atlassian.net").to_owned());
        let raw = prompt_required("Subdomain", "", default_sub.as_deref())?;
        let sub = raw.trim().trim_end_matches(".atlassian.net");
        format!("{sub}.atlassian.net")
    } else {
        let default = existing
            .as_ref()
            .and_then(|p| p.host.clone())
            .or_else(|| prefill_host.map(str::to_owned));
        prompt_required("Host", "", default.as_deref())?
    };

    // Credentials
    let (email, token, auth_type, api_version): (Option<String>, String, &str, u8) = if is_cloud {
        const CLOUD_URL: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";
        let default_email = existing.as_ref().and_then(|p| p.email.clone());
        let email = prompt_required("Email", "", default_email.as_deref())?;
        eprintln!("  {}", sym_dim(&format!("→ {CLOUD_URL}")));
        let token_hint = if existing.as_ref().and_then(|p| p.token.as_ref()).is_some() {
            "(Enter to keep)"
        } else {
            ""
        };
        let raw = prompt("Token", token_hint, None)?;
        let token = if raw.trim().is_empty() {
            existing
                .as_ref()
                .and_then(|p| p.token.clone())
                .ok_or("No existing token — please enter a token.")?
        } else {
            raw
        };
        (Some(email), token, "basic", 3)
    } else {
        let pat_url = dc_pat_url(Some(&host));
        eprintln!("  {}", sym_dim(&format!("→ {pat_url}")));
        let token_hint = if existing.as_ref().and_then(|p| p.token.as_ref()).is_some() {
            "(Enter to keep)"
        } else {
            ""
        };
        let raw = prompt("Token", token_hint, None)?;
        let token = if raw.trim().is_empty() {
            existing
                .as_ref()
                .and_then(|p| p.token.clone())
                .ok_or("No existing token — please enter a token.")?
        } else {
            raw
        };
        let default_ver = existing
            .as_ref()
            .and_then(|p| p.api_version.map(|v| v.to_string()))
            .unwrap_or_else(|| "2".to_owned());
        let ver_str = prompt("API version", "", Some(&default_ver))?;
        let api_version: u8 = ver_str.trim().parse().unwrap_or(2);
        (None, token, "pat", api_version)
    };

    // Verify credentials against the API before writing anything.
    use std::io::Write;
    eprintln!();
    eprint!("  Verifying credentials...");
    std::io::stderr().flush().ok();

    let auth_type_enum = if auth_type == "pat" {
        AuthType::Pat
    } else {
        AuthType::Basic
    };

    let verified = match crate::api::client::JiraClient::new(
        &host,
        email.as_deref().unwrap_or(""),
        &token,
        auth_type_enum,
        api_version,
    ) {
        Err(e) => {
            eprintln!(" {} {e}", sym_fail());
            return Err(e.into());
        }
        Ok(client) => match client.get_myself().await {
            Ok(myself) => {
                eprintln!(" {} Authenticated as {}", sym_ok(), myself.display_name);
                true
            }
            Err(e) => {
                eprintln!(" {} {e}", sym_fail());
                eprintln!();
                let save = prompt("Save config anyway?", sym_dim("[y/N]").as_str(), Some("n"))?;
                save.trim().eq_ignore_ascii_case("y")
            }
        },
    };

    if !verified {
        eprintln!();
        eprintln!("{sep}");
        return Ok(());
    }

    // Profile name — ask only when adding a new named profile.
    let profile_name = match target_name {
        Some(name) => name,
        None => {
            eprintln!();
            let raw = prompt_required("Profile name", "", Some("default"))?;
            if raw.trim().is_empty() {
                "default".to_owned()
            } else {
                raw.trim().to_owned()
            }
        }
    };

    // Write config
    write_profile_to_config(
        &path,
        &profile_name,
        &host,
        email.as_deref(),
        &token,
        auth_type,
        api_version,
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    eprintln!();
    eprintln!("  {} Config written to {}", sym_ok(), path.display());
    eprintln!("{sep}");
    if profile_name == "default" {
        eprintln!("  Run: jira projects list");
    } else {
        eprintln!("  Run: jira --profile {profile_name} projects list");
    }
    eprintln!();

    Ok(())
}

/// List all profile names present in the config file (default first, then named profiles).
fn list_profile_names(path: &std::path::Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&content)?;
    let table = doc.as_table().ok_or("config is not a TOML table")?;

    let mut names = Vec::new();
    if table.contains_key("default") {
        names.push("default".to_owned());
    }
    if let Some(profiles) = table.get("profiles").and_then(toml::Value::as_table) {
        for name in profiles.keys() {
            names.push(name.clone());
        }
    }
    Ok(names)
}

/// Read a single profile's raw values from the config file for use as pre-fill defaults.
fn read_raw_profile(
    path: &std::path::Path,
    name: &str,
) -> Result<ProfileConfig, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let raw: RawConfig = toml::from_str(&content)?;
    if name == "default" {
        Ok(raw.default_profile())
    } else {
        Ok(raw.profiles.get(name).cloned().unwrap_or_default())
    }
}

/// Print `? Label  hint [default]: ` and read a line from stdin.
///
/// `hint` is shown dimmed between the label and the default bracket; pass `""` to omit it.
/// Returns the default string when the user presses Enter without typing.
fn prompt(label: &str, hint: &str, default: Option<&str>) -> Result<String, std::io::Error> {
    use std::io::{self, Write};
    let hint_part = if hint.is_empty() {
        String::new()
    } else {
        format!("  {hint}")
    };
    let default_part = match default {
        Some(d) if !d.is_empty() => format!(" [{d}]"),
        _ => String::new(),
    };
    eprint!("{} {label}{hint_part}{default_part}: ", sym_q());
    io::stderr().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim().to_owned();
    if trimmed.is_empty() {
        Ok(default.unwrap_or("").to_owned())
    } else {
        Ok(trimmed)
    }
}

/// Like `prompt` but re-prompts until the user provides a non-empty value.
fn prompt_required(
    label: &str,
    hint: &str,
    default: Option<&str>,
) -> Result<String, std::io::Error> {
    loop {
        let value = prompt(label, hint, default)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        eprintln!("  {} {label} is required.", sym_fail());
    }
}

// ── Color / symbol helpers ──────────────────────────────────────────────────

fn sym_q() -> String {
    if crate::output::use_color() {
        use owo_colors::OwoColorize;
        "?".green().bold().to_string()
    } else {
        "?".to_owned()
    }
}

fn sym_ok() -> String {
    if crate::output::use_color() {
        use owo_colors::OwoColorize;
        "✔".green().to_string()
    } else {
        "✔".to_owned()
    }
}

fn sym_fail() -> String {
    if crate::output::use_color() {
        use owo_colors::OwoColorize;
        "✖".red().to_string()
    } else {
        "✖".to_owned()
    }
}

fn sym_dim(s: &str) -> String {
    if crate::output::use_color() {
        use owo_colors::OwoColorize;
        s.dimmed().to_string()
    } else {
        s.to_owned()
    }
}

/// Write or update a single profile section in the config file.
///
/// If the file already exists its other sections are preserved; only the target
/// profile section is created or replaced. The parent directory is created if needed.
fn write_profile_to_config(
    path: &std::path::Path,
    profile_name: &str,
    host: &str,
    email: Option<&str>,
    token: &str,
    auth_type: &str,
    api_version: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut doc: toml::Value = if existing.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&existing)?
    };

    let root = doc.as_table_mut().expect("config is a TOML table");

    let mut section = toml::map::Map::new();
    section.insert("host".to_owned(), toml::Value::String(host.to_owned()));
    if let Some(e) = email {
        section.insert("email".to_owned(), toml::Value::String(e.to_owned()));
    }
    section.insert("token".to_owned(), toml::Value::String(token.to_owned()));
    if auth_type != "basic" {
        section.insert(
            "auth_type".to_owned(),
            toml::Value::String(auth_type.to_owned()),
        );
        section.insert(
            "api_version".to_owned(),
            toml::Value::Integer(i64::from(api_version)),
        );
    }

    if profile_name == "default" {
        root.insert("default".to_owned(), toml::Value::Table(section));
    } else {
        let profiles = root
            .entry("profiles")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        profiles
            .as_table_mut()
            .expect("profiles is a TOML table")
            .insert(profile_name.to_owned(), toml::Value::Table(section));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&doc)?)?;

    Ok(())
}

/// Remove a named profile from the config file.
///
/// The "default" profile is removed by deleting the `[default]` section. Named profiles
/// are removed from the `[profiles]` table. Prints a success or error message; does not
/// write to stdout so it is safe in JSON mode.
pub fn remove_profile(out: &OutputConfig, profile_name: &str) -> Result<(), ApiError> {
    let path = config_path();

    if !path.exists() {
        return Err(ApiError::NotFound(format!(
            "config file at {}",
            path.display()
        )));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| ApiError::Other(format!("Failed to read config: {e}")))?;
    let mut doc: toml::Value = toml::from_str(&content)
        .map_err(|e| ApiError::Other(format!("Failed to parse config: {e}")))?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| ApiError::Other("config is not a TOML table".to_string()))?;

    let removed = if profile_name == "default" {
        root.remove("default").is_some()
    } else {
        root.get_mut("profiles")
            .and_then(toml::Value::as_table_mut)
            .and_then(|t| t.remove(profile_name))
            .is_some()
    };

    if !removed {
        return Err(ApiError::NotFound(format!(
            "profile '{profile_name}' in config. Available: {}",
            format_available(&removable_profiles(root))
        )));
    }

    let serialized = toml::to_string_pretty(&doc)
        .map_err(|e| ApiError::Other(format!("Failed to serialize config: {e}")))?;
    std::fs::write(&path, serialized)
        .map_err(|e| ApiError::Other(format!("Failed to write config: {e}")))?;

    out.print_result(
        &serde_json::json!({ "profile": profile_name, "removed": true }),
        &format!("{} Removed profile '{profile_name}'", sym_ok()),
    );
    Ok(())
}

/// Names `config remove` accepts, in deterministic order: the `default`
/// section when present, then each `[profiles.*]` key.
fn removable_profiles(root: &toml::Table) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    if root.contains_key("default") {
        names.push("default");
    }
    if let Some(profiles) = root.get("profiles").and_then(toml::Value::as_table) {
        names.extend(profiles.keys().map(String::as_str));
    }
    names
}

const PAT_PATH: &str = "/secure/ViewProfile.jspa?selectedTab=com.atlassian.pats.pats-plugin:jira-user-personal-access-tokens";

/// Build the Personal Access Token creation URL for a Jira DC/Server instance.
///
/// When `host` is known the full URL is returned so the user can click it directly.
/// When unknown a placeholder template is returned.
fn dc_pat_url(host: Option<&str>) -> String {
    match host {
        Some(h) => {
            let base = if h.starts_with("http://") || h.starts_with("https://") {
                h.trim_end_matches('/').to_string()
            } else {
                format!("https://{}", h.trim_end_matches('/'))
            };
            format!("{base}{PAT_PATH}")
        }
        None => format!("http://<your-host>{PAT_PATH}"),
    }
}

/// Mask a token for display, showing only the last 4 characters.
///
/// Atlassian tokens begin with a predictable prefix, so showing the
/// start provides no meaningful identification — the end is more useful.
fn mask_token(token: &str) -> String {
    let n = token.chars().count();
    if n > 4 {
        let suffix: String = token.chars().skip(n - 4).collect();
        format!("***{suffix}")
    } else {
        "***".into()
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| normalize_value(Some(value)))
}

fn normalize_value(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_str(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, ProcessEnvLock, set_config_dir_env, write_config};
    use tempfile::TempDir;

    #[test]
    fn mask_token_long() {
        let masked = mask_token("ATATxxx1234abcd");
        assert!(masked.starts_with("***"));
        assert!(masked.ends_with("abcd"));
    }

    #[test]
    fn mask_token_short() {
        assert_eq!(mask_token("abc"), "***");
    }

    #[test]
    fn mask_token_unicode_safe() {
        // Ensure char-based indexing doesn't panic on multi-byte chars
        let token = "token-日本語-end";
        let result = mask_token(token);
        assert!(result.starts_with("***"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn config_path_prefers_xdg_config_home() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());

        assert_eq!(config_path(), dir.path().join("jira").join("config.toml"));
    }

    #[test]
    fn load_ignores_blank_env_vars_and_falls_back_to_file() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "work.atlassian.net"
email = "me@example.com"
token = "secret-token"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "   ");
        let _email = EnvVarGuard::set("JIRA_EMAIL", "");
        let _token = EnvVarGuard::set("JIRA_TOKEN", " ");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.host, "work.atlassian.net");
        assert_eq!(cfg.email, "me@example.com");
        assert_eq!(cfg.token, "secret-token");
    }

    #[test]
    fn load_accepts_documented_default_section() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "example.atlassian.net"
email = "me@example.com"
token = "secret-token"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.host, "example.atlassian.net");
        assert_eq!(cfg.email, "me@example.com");
        assert_eq!(cfg.token, "secret-token");
    }

    #[test]
    fn load_treats_blank_env_vars_as_missing_when_no_file_exists() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "");
        let _email = EnvVarGuard::set("JIRA_EMAIL", "");
        let _token = EnvVarGuard::set("JIRA_TOKEN", "");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = Config::load(None, None, None).unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
        assert!(err.to_string().contains("No Jira host configured"));
    }

    #[test]
    fn permission_guidance_matches_platform() {
        let guidance = recommended_permissions(std::path::Path::new("/tmp/jira/config.toml"));

        #[cfg(target_os = "windows")]
        assert!(guidance.contains("AppData"));

        #[cfg(not(target_os = "windows"))]
        assert!(guidance.starts_with("chmod 600 "));
    }

    // ── Priority: CLI > env > file ─────────────────────────────────────────────

    #[test]
    fn load_env_host_overrides_file() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "file.atlassian.net"
email = "me@example.com"
token = "tok"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "env.atlassian.net");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.host, "env.atlassian.net");
    }

    #[test]
    fn load_cli_host_arg_overrides_env_and_file() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "file.atlassian.net"
email = "me@example.com"
token = "tok"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "env.atlassian.net");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(Some("cli.atlassian.net".into()), None, None).unwrap();
        assert_eq!(cfg.host, "cli.atlassian.net");
    }

    // ── Error cases ────────────────────────────────────────────────────────────

    #[test]
    fn load_missing_token_returns_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "myhost.atlassian.net");
        let _email = EnvVarGuard::set("JIRA_EMAIL", "me@example.com");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = Config::load(None, None, None).unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
        assert!(err.to_string().contains("No API token"));
    }

    #[test]
    fn load_missing_email_for_basic_auth_returns_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "myhost.atlassian.net");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::set("JIRA_TOKEN", "secret");
        let _auth = EnvVarGuard::unset("JIRA_AUTH_TYPE");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = Config::load(None, None, None).unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
        assert!(err.to_string().contains("No email configured"));
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(dir.path(), "host = [invalid toml").unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = Config::load(None, None, None).unwrap_err();
        assert!(matches!(err, ApiError::Other(_)));
        assert!(err.to_string().contains("parse"));
    }

    // ── Auth type ──────────────────────────────────────────────────────────────

    #[test]
    fn load_pat_auth_does_not_require_email() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "jira.corp.com"
token = "my-pat-token"
auth_type = "pat"
api_version = 2
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _auth = EnvVarGuard::unset("JIRA_AUTH_TYPE");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.auth_type, AuthType::Pat);
        assert_eq!(cfg.api_version, 2);
        assert!(cfg.email.is_empty(), "PAT auth sets email to empty string");
    }

    #[test]
    fn load_jira_auth_type_env_pat_overrides_basic() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "jira.corp.com"
email = "me@example.com"
token = "tok"
auth_type = "basic"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _auth = EnvVarGuard::set("JIRA_AUTH_TYPE", "pat");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.auth_type, AuthType::Pat);
    }

    #[test]
    fn load_jira_api_version_env_overrides_default() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::set("JIRA_HOST", "myhost.atlassian.net");
        let _email = EnvVarGuard::set("JIRA_EMAIL", "me@example.com");
        let _token = EnvVarGuard::set("JIRA_TOKEN", "tok");
        let _api_version = EnvVarGuard::set("JIRA_API_VERSION", "2");
        let _auth = EnvVarGuard::unset("JIRA_AUTH_TYPE");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.api_version, 2);
    }

    // ── Profile selection ──────────────────────────────────────────────────────

    #[test]
    fn load_profile_arg_selects_named_section() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "default.atlassian.net"
email = "default@example.com"
token = "default-tok"

[profiles.work]
host = "work.atlassian.net"
email = "me@work.com"
token = "work-tok"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let cfg = Config::load(None, None, Some("work".into())).unwrap();
        assert_eq!(cfg.host, "work.atlassian.net");
        assert_eq!(cfg.email, "me@work.com");
        assert_eq!(cfg.token, "work-tok");
    }

    #[test]
    fn load_jira_profile_env_selects_named_section() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "default.atlassian.net"
email = "default@example.com"
token = "default-tok"

[profiles.staging]
host = "staging.atlassian.net"
email = "me@staging.com"
token = "staging-tok"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::set("JIRA_PROFILE", "staging");

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.host, "staging.atlassian.net");
    }

    #[test]
    fn load_unknown_profile_returns_descriptive_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[profiles.alpha]
host = "alpha.atlassian.net"
email = "me@alpha.com"
token = "alpha-tok"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = Config::load(None, None, Some("nonexistent".into())).unwrap_err();
        assert!(
            matches!(err, ApiError::NotFound(_)),
            "selecting a profile that is not in the config is a not_found condition, \
             not an unexpected error: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent"),
            "error should name the bad profile"
        );
        assert!(
            msg.contains("alpha"),
            "error should list available profiles"
        );
    }

    // ── config::show ───────────────────────────────────────────────────────────

    #[test]
    fn show_json_output_includes_host_and_masked_token() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "show-test.atlassian.net"
email = "me@example.com"
token = "supersecrettoken"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let out = crate::output::OutputConfig::new(true, false, true);
        // Must not error and must produce no error output
        show(&out, None, None, None).unwrap();
    }

    #[test]
    fn show_text_output_renders_without_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[default]
host = "show-test.atlassian.net"
email = "me@example.com"
token = "supersecrettoken"
"#,
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let out = crate::output::OutputConfig::new(false, false, true);
        show(&out, None, None, None).unwrap();
    }

    // ── config::init ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn init_json_output_includes_example_and_paths() {
        let out = crate::output::OutputConfig::new(true, false, true);
        // No env or config needed — init() never loads credentials in JSON mode
        init(&out, Some("jira.corp.com")).await;
    }

    // The text path of init() requires an interactive TTY; in test context stdin is
    // not a TTY so it prints a short message and returns without hanging.
    #[tokio::test]
    async fn init_non_interactive_prints_message_without_error() {
        let out = crate::output::OutputConfig {
            json: false,
            quiet: false,
        };
        // stdin is not a TTY in tests — must return immediately, not hang
        init(&out, None).await;
    }

    #[test]
    fn write_profile_to_config_creates_default_profile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("jira").join("config.toml");

        write_profile_to_config(
            &path,
            "default",
            "acme.atlassian.net",
            Some("me@acme.com"),
            "secret",
            "basic",
            3,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("acme.atlassian.net"));
        assert!(content.contains("me@acme.com"));
        assert!(content.contains("secret"));
        // basic/v3 are defaults and should not add redundant keys
        assert!(!content.contains("auth_type"));
    }

    #[test]
    fn write_profile_to_config_creates_named_pat_profile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        write_profile_to_config(&path, "dc", "jira.corp.com", None, "pattoken", "pat", 2).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[profiles.dc]"));
        assert!(content.contains("jira.corp.com"));
        assert!(content.contains("pattoken"));
        assert!(content.contains("auth_type"));
        assert!(content.contains("api_version"));
        assert!(!content.contains("email"));
    }

    #[test]
    fn write_profile_to_config_preserves_other_profiles() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        // Write initial config with a default profile
        std::fs::write(
            &path,
            "[default]\nhost = \"first.atlassian.net\"\nemail = \"a@b.com\"\ntoken = \"tok1\"\n",
        )
        .unwrap();

        // Add a second named profile without touching default
        write_profile_to_config(
            &path,
            "work",
            "work.atlassian.net",
            Some("w@work.com"),
            "tok2",
            "basic",
            3,
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("first.atlassian.net"),
            "default profile must be preserved"
        );
        assert!(
            content.contains("work.atlassian.net"),
            "new profile must be written"
        );
    }

    // ── remove_profile ─────────────────────────────────────────────────────────

    #[test]
    fn remove_profile_removes_default_section() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            dir.path(),
            "[default]\nhost = \"acme.atlassian.net\"\nemail = \"me@acme.com\"\ntoken = \"tok\"\n",
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        remove_profile(&OutputConfig::new(true, false, true), "default").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[default]"));
        assert!(!content.contains("acme.atlassian.net"));
    }

    #[test]
    fn remove_profile_removes_named_profile_preserves_others() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            dir.path(),
            "[default]\nhost = \"first.atlassian.net\"\ntoken = \"tok1\"\n\n\
             [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        remove_profile(&OutputConfig::new(true, false, true), "work").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("work.atlassian.net"),
            "work profile must be gone"
        );
        assert!(
            content.contains("first.atlassian.net"),
            "default profile must be preserved"
        );
    }

    #[test]
    fn remove_profile_last_named_profile_leaves_default_intact() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            dir.path(),
            "[default]\nhost = \"acme.atlassian.net\"\ntoken = \"tok\"\n\n\
             [profiles.staging]\nhost = \"staging.atlassian.net\"\ntoken = \"tok2\"\n",
        )
        .unwrap();

        let _config_dir = set_config_dir_env(dir.path());
        remove_profile(&OutputConfig::new(true, false, true), "staging").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("staging.atlassian.net"),
            "staging must be gone"
        );
        assert!(
            content.contains("acme.atlassian.net"),
            "default must be preserved"
        );
    }

    // ── dc_pat_url ─────────────────────────────────────────────────────────────

    #[test]
    fn dc_pat_url_without_host_returns_placeholder() {
        let url = dc_pat_url(None);
        assert!(url.starts_with("http://<your-host>"));
        assert!(url.contains(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_bare_host_adds_https_scheme() {
        let url = dc_pat_url(Some("jira.corp.com"));
        assert!(url.starts_with("https://jira.corp.com"));
        assert!(url.contains(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_host_with_https_scheme_is_preserved() {
        let url = dc_pat_url(Some("https://jira.corp.com/"));
        assert!(url.starts_with("https://jira.corp.com"));
        assert!(!url.contains("https://https://"));
        assert!(url.contains(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_host_with_http_scheme_is_preserved() {
        let url = dc_pat_url(Some("http://localhost:8080"));
        assert!(url.starts_with("http://localhost:8080"));
        assert!(url.contains(PAT_PATH));
    }
}
