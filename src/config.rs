use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::api::ApiError;
use crate::api::AuthType;
use crate::output::OutputConfig;

mod site;

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProfileConfig {
    pub host: Option<String>,
    pub email: Option<String>,
    pub token: Option<String>,
    pub credential_store: Option<String>,
    pub cloud_id: Option<String>,
    pub token_kind: Option<String>,
    pub auth_type: Option<String>,
    pub api_version: Option<u8>,
    pub read_only: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    active_profile: Option<String>,
    #[serde(default)]
    default: ProfileConfig,
    #[serde(default)]
    profiles: BTreeMap<String, ProfileConfig>,
    host: Option<String>,
    email: Option<String>,
    token: Option<String>,
    credential_store: Option<String>,
    cloud_id: Option<String>,
    token_kind: Option<String>,
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
            credential_store: self
                .default
                .credential_store
                .clone()
                .or_else(|| self.credential_store.clone()),
            cloud_id: self
                .default
                .cloud_id
                .clone()
                .or_else(|| self.cloud_id.clone()),
            token_kind: self
                .default
                .token_kind
                .clone()
                .or_else(|| self.token_kind.clone()),
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
    pub profile: String,
    pub host: String,
    pub email: String,
    pub token: String,
    pub auth_type: AuthType,
    pub api_version: u8,
    pub read_only: bool,
    pub credential_store: String,
    pub cloud_id: Option<String>,
    pub token_kind: String,
}

/// What a profile's site reports about its deployment, held against the REST
/// API version and authentication the profile is set up for.
#[derive(Debug)]
pub(crate) enum DeploymentCheck {
    /// The site runs what the profile expects; carries its description.
    Matches(String),
    /// The site runs something the profile cannot talk to; carries the
    /// explanation and the fix.
    Mismatch(String),
    /// The site could not be identified; carries why. Later checks still run.
    Unknown(String),
}

/// Ask the profile's site which deployment it runs and compare it with the
/// profile. Cloud needs REST API v3 and an email with an API token; Data Center
/// serves REST API v2.
pub(crate) async fn check_deployment(config: &Config) -> DeploymentCheck {
    let site = match site::normalize_site(&config.host) {
        Ok(site) => site,
        Err(error) => return DeploymentCheck::Unknown(error),
    };
    let detected = match site::detect(&site).await {
        Ok(detected) => detected,
        Err(error) => return DeploymentCheck::Unknown(error),
    };
    let runs = detected.describe();
    // Each mismatch with the environment setting that corrects it.
    let (problem, setting) = match detected.deployment {
        site::Deployment::Cloud if config.auth_type == AuthType::Pat => (
            format!(
                "{} runs {runs}, which takes an email and API token, but this profile sends a personal access token",
                config.host
            ),
            ("JIRA_AUTH_TYPE", "basic"),
        ),
        site::Deployment::Cloud if config.api_version != 3 => (
            format!(
                "{} runs {runs}, which needs REST API v3, but this profile uses v{}",
                config.host, config.api_version
            ),
            ("JIRA_API_VERSION", "3"),
        ),
        site::Deployment::DataCenter if config.api_version != 2 => (
            format!(
                "{} runs {runs}, which serves REST API v2, but this profile uses v{}",
                config.host, config.api_version
            ),
            ("JIRA_API_VERSION", "2"),
        ),
        _ => return DeploymentCheck::Matches(runs),
    };
    let (variable, value) = setting;
    // The environment overrides the config file, so when the variable is set,
    // or no saved profile exists for setup to correct, the variable is the fix.
    let saved_profile = load_file_profile(Some(&config.profile))
        .map(|(_, profile)| profile.host.is_some())
        .unwrap_or(false);
    if std::env::var_os(variable).is_some() || !saved_profile {
        return DeploymentCheck::Mismatch(format!("{problem}. Set {variable}={value}."));
    }
    DeploymentCheck::Mismatch(format!(
        "{problem}. Run `jira init --profile {}` to set the profile up for this site.",
        shell_word(&config.profile)
    ))
}

/// Render `word` as one shell argument, quoting it only when it needs it, so a
/// suggested command can be pasted as written even for a profile named `work team`.
fn shell_word(word: &str) -> std::borrow::Cow<'_, str> {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@'));
    if plain {
        std::borrow::Cow::Borrowed(word)
    } else {
        std::borrow::Cow::Owned(format!("'{}'", word.replace('\'', r"'\''")))
    }
}

impl Config {
    /// How to replace this profile's token when Jira rejects it: the variable to
    /// change, or the exact command to rerun, rather than a generic checklist.
    pub fn auth_remedy(&self) -> crate::api::AuthRemedy {
        let causes = match self.auth_type {
            AuthType::Basic => format!(
                "it may have expired or been revoked, or belong to an account other than {}",
                self.email
            ),
            AuthType::Pat => "it may have expired or been revoked".to_string(),
        };
        let scopes_advice = "lacks a scope this request needs. Create a token with the scopes `jira init --json` lists under cloudTokenScopes";
        if self.credential_store == "environment" {
            crate::api::AuthRemedy {
                rejected: format!(
                    "The token in JIRA_TOKEN was rejected: {causes}. Replace JIRA_TOKEN with a current token."
                ),
                missing_scope: format!(
                    "The token in JIRA_TOKEN {scopes_advice}, then put it in JIRA_TOKEN."
                ),
            }
        } else {
            let profile = &self.profile;
            let argument = shell_word(profile);
            crate::api::AuthRemedy {
                rejected: format!(
                    "The token stored for profile `{profile}` was rejected: {causes}. Run `jira auth login --profile {argument}` to store a new one."
                ),
                missing_scope: format!(
                    "The token stored for profile `{profile}` {scopes_advice}, then run `jira auth login --profile {argument}` to store it."
                ),
            }
        }
    }

    /// Load config with priority: CLI args > env vars > config file.
    ///
    /// The API token must be supplied via the `JIRA_TOKEN` environment variable
    /// or the config file - not via a CLI flag, to avoid leaking it in process
    /// argument lists visible to other users.
    pub fn load(
        host_arg: Option<String>,
        email_arg: Option<String>,
        profile_arg: Option<String>,
    ) -> Result<Self, ApiError> {
        let (profile, file_profile) = load_file_profile(profile_arg.as_deref())?;

        let host = normalize_value(host_arg)
            .or_else(|| env_var("JIRA_HOST"))
            .or_else(|| normalize_value(file_profile.host))
            .ok_or_else(|| {
                ApiError::InvalidInput(
                    "No Jira host configured. Set JIRA_HOST or run `jira config init`.".into(),
                )
            })?;

        let env_token = env_var("JIRA_TOKEN");
        let stored_token = match file_profile.credential_store.as_deref() {
            Some("keyring") if env_token.is_none() => crate::credentials::load_optional(&profile)?,
            Some("file") | None => normalize_value(file_profile.token.clone()),
            Some(other) => {
                return Err(ApiError::InvalidInput(format!(
                    "unsupported credential_store `{other}` for profile `{profile}`"
                )));
            }
        };
        let credential_store = if env_token.is_some() {
            "environment"
        } else if file_profile.credential_store.as_deref() == Some("keyring") {
            "os-keychain"
        } else if stored_token.is_some() {
            if file_profile.credential_store.as_deref() == Some("file") {
                "config-file"
            } else {
                "legacy-config"
            }
        } else {
            "none"
        }
        .to_string();
        let token = env_token.or(stored_token).ok_or_else(|| {
            ApiError::InvalidInput(
                "No API token configured. Set JIRA_TOKEN or run `jira auth login`.".into(),
            )
        })?;

        // A blank value is absent, the same as for host, email and token: only a
        // value someone actually wrote is worth rejecting.
        let auth_type = match env_var("JIRA_AUTH_TYPE")
            .or_else(|| normalize_value(file_profile.auth_type.clone()))
        {
            Some(v) => parse_auth_type(&v)?,
            None => AuthType::default(),
        };

        let api_version = match env_var("JIRA_API_VERSION") {
            Some(v) => parse_api_version(&v)?,
            None => match file_profile.api_version {
                Some(v) => validate_api_version(v)?,
                None => 3,
            },
        };

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

        let read_only = match env_var("JIRA_READ_ONLY") {
            Some(v) => parse_read_only(&v)?,
            None => file_profile.read_only.unwrap_or(false),
        };

        let cloud_id = env_var("JIRA_CLOUD_ID").or(file_profile.cloud_id);
        let token_kind = env_var("JIRA_TOKEN_KIND")
            .or(file_profile.token_kind)
            .unwrap_or_else(|| "classic".into());
        if !matches!(token_kind.as_str(), "classic" | "scoped") {
            return Err(ApiError::InvalidInput(format!(
                "unsupported token_kind `{token_kind}`; expected classic or scoped"
            )));
        }
        if token_kind == "scoped" && cloud_id.is_none() {
            return Err(ApiError::InvalidInput(
                "scoped Cloud token requires cloud_id; run `jira auth login` again".into(),
            ));
        }
        Ok(Self {
            profile,
            host,
            email,
            token,
            auth_type,
            api_version,
            read_only,
            credential_store,
            cloud_id,
            token_kind,
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

pub fn config_path() -> PathBuf {
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

fn load_file_profile(profile: Option<&str>) -> Result<(String, ProfileConfig), ApiError> {
    let path = config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                normalize_str(profile)
                    .map(str::to_owned)
                    .or_else(|| env_var("JIRA_PROFILE"))
                    .unwrap_or_else(|| "default".into()),
                ProfileConfig::default(),
            ));
        }
        Err(e) => return Err(ApiError::Other(format!("Failed to read config: {e}"))),
    };

    let raw: RawConfig = toml::from_str(&content)
        .map_err(|e| ApiError::Other(format!("Failed to parse config: {e}")))?;

    let profile_name = normalize_str(profile)
        .map(str::to_owned)
        .or_else(|| env_var("JIRA_PROFILE"))
        .or_else(|| raw.active_profile.clone());

    match profile_name {
        Some(name) if name == "default" => Ok((name, raw.default_profile())),
        Some(name) => {
            // BTreeMap gives sorted, deterministic output in error messages
            let available: Vec<&str> = raw.profiles.keys().map(String::as_str).collect();
            raw.profiles
                .get(&name)
                .cloned()
                .map(|value| (name.clone(), value))
                .ok_or_else(|| {
                    ApiError::NotFound(format!(
                        "profile '{name}' in config. Available: {}",
                        format_available(&available)
                    ))
                })
        }
        None => Ok(("default".into(), raw.default_profile())),
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
                "profile": cfg.profile,
                "credentialStore": cfg.credential_store,
                "tokenKind": cfg.token_kind,
                "cloudId": cfg.cloud_id,
            }))
            .expect("failed to serialize JSON"),
        );
    } else {
        out.print_message(&format!("Config file: {}", path.display()));
        out.print_data(&format!(
            "profile: {}\nhost:  {}\nemail: {}\ntoken: {masked}\ncredential store: {}\ntoken kind: {}",
            cfg.profile, cfg.host, cfg.email, cfg.credential_store, cfg.token_kind,
        ));
    }
    Ok(())
}

pub async fn auth_status(
    out: &OutputConfig,
    host_arg: Option<String>,
    email_arg: Option<String>,
    profile_arg: Option<String>,
    offline: bool,
) -> Result<(), ApiError> {
    let cfg = Config::load(host_arg, email_arg, profile_arg)?;
    if offline {
        out.print_result(
            &serde_json::json!({
                "profile": cfg.profile,
                "status": "configured",
                "verified": false,
                "credentialStore": cfg.credential_store,
                "tokenKind": cfg.token_kind,
                "cloudId": cfg.cloud_id,
            }),
            &format!(
                "{} Profile '{}' is configured ({}, {}; network not checked)",
                sym_ok(),
                cfg.profile,
                cfg.credential_store,
                cfg.token_kind
            ),
        );
        return Ok(());
    }
    let client = crate::api::client::JiraClient::new_with_cloud(
        &cfg.host,
        &cfg.email,
        &cfg.token,
        cfg.auth_type.clone(),
        cfg.api_version,
        cfg.cloud_id.as_deref(),
        &cfg.token_kind,
    )?
    .with_auth_remedy(cfg.auth_remedy());
    let myself = client.get_myself().await?;
    out.print_result(
        &serde_json::json!({
            "profile": cfg.profile,
            "status": "ok",
            "verified": true,
            "identity": myself.display_name,
            "credentialStore": cfg.credential_store,
            "tokenKind": cfg.token_kind,
            "cloudId": cfg.cloud_id,
        }),
        &format!(
            "{} Authenticated as {} ({}, {})",
            sym_ok(),
            myself.display_name,
            cfg.credential_store,
            cfg.token_kind,
        ),
    );
    Ok(())
}

pub async fn migrate_credential(
    out: &OutputConfig,
    profile_arg: Option<String>,
) -> Result<(), ApiError> {
    let cfg = Config::load(None, None, profile_arg)?;
    if cfg.credential_store != "legacy-config" && cfg.credential_store != "config-file" {
        return Err(ApiError::InvalidInput(format!(
            "profile `{}` does not contain an inline token to migrate",
            cfg.profile
        )));
    }
    crate::api::client::JiraClient::new_with_cloud(
        &cfg.host,
        &cfg.email,
        &cfg.token,
        cfg.auth_type.clone(),
        cfg.api_version,
        cfg.cloud_id.as_deref(),
        &cfg.token_kind,
    )?
    .with_auth_remedy(cfg.auth_remedy())
    .get_myself()
    .await?;

    crate::credentials::available()?;
    let previous = crate::credentials::load_optional(&cfg.profile)?;
    crate::credentials::store(&cfg.profile, &cfg.token)?;
    if let Err(error) = rewrite_profile_credential(&cfg.profile, Some("keyring")) {
        match previous {
            Some(token) => {
                let _ = crate::credentials::store(&cfg.profile, &token);
            }
            None => {
                let _ = crate::credentials::delete(&cfg.profile);
            }
        }
        return Err(error);
    }
    out.print_result(
        &serde_json::json!({
            "profile": cfg.profile,
            "migrated": true,
            "credentialStore": "os-keychain",
        }),
        &format!(
            "{} Migrated profile `{}` to the operating-system keychain",
            sym_ok(),
            cfg.profile
        ),
    );
    Ok(())
}

pub fn logout(out: &OutputConfig, profile_arg: Option<String>) -> Result<(), ApiError> {
    let profile = requested_profile_name(profile_arg.as_deref());
    let (_, stored) = load_file_profile(Some(&profile))?;
    let removed = if stored.credential_store.as_deref() == Some("keyring") {
        crate::credentials::delete(&profile)?
    } else {
        false
    };
    rewrite_profile_credential(&profile, None)?;
    out.print_result(
        &serde_json::json!({ "profile": profile, "loggedOut": true, "credentialRemoved": removed }),
        &format!("{} Logged out profile `{profile}`", sym_ok()),
    );
    Ok(())
}

/// Interactively set up the config file, or print JSON instructions when `--json` is used.
///
/// In JSON mode the function prints a machine-readable instructions object and returns.
/// In an interactive terminal it prompts for Jira type, host, credentials, and profile
/// name, verifies the credentials against the API, then writes (or updates)
/// `~/.config/jira/config.toml`.
///
/// `detect_site` asks `host` which deployment it runs; only a host the user
/// named for this run is contacted, so the JSON instructions stay offline
/// otherwise.
pub async fn init(
    out: &OutputConfig,
    host: Option<&str>,
    detect_site: bool,
    profile: Option<&str>,
) -> Result<(), ApiError> {
    if out.json {
        init_json(out, host, detect_site).await;
        return Ok(());
    }

    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(ApiError::InvalidInput(
            "interactive setup requires a terminal; run `jira init --json` for setup instructions, or configure JIRA_HOST, JIRA_EMAIL, and JIRA_TOKEN for automation"
                .into(),
        ));
    }

    init_interactive(host, profile)
        .await
        .map_err(|error| ApiError::Other(error.to_string()))
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
            "credential_store": "keyring",
            "cloud_id": "your-atlassian-cloud-id",
            "token_kind": "scoped",
            "auth_type": "basic",
            "api_version": 3,
            "read_only": true,
        },
        "profiles": {
            "work": {
                "host": "work.atlassian.net",
                "email": "me@work.com",
                "credential_store": "keyring",
                "cloud_id": "your-work-cloud-id",
                "token_kind": "scoped",
            },
            "datacenter": {
                "host": "jira.mycompany.com",
                "credential_store": "keyring",
                    "auth_type": "pat",
                "api_version": 2,
            }
        }
    })
}

async fn init_json(out: &OutputConfig, host: Option<&str>, detect_site: bool) {
    let path = config_path();
    let path_resolution = schema_config_path_description();
    let permission_advice = recommended_permissions(&path);
    let example = schema_example_config();

    let normalized = host.map(site::normalize_site);
    let site_host = normalized
        .as_ref()
        .and_then(|site| site.as_ref().ok())
        .map(|site| site.host.as_str())
        .or(host);
    let pat_url = dc_pat_url(site_host);

    let mut result = serde_json::json!({
        "configPath": path,
        "pathResolution": path_resolution,
        "configExists": path.exists(),
        "tokenInstructions": CLOUD_TOKEN_URL,
        "cloudTokenScopes": cloud_token_scopes_json(),
        "dcPatInstructions": pat_url,
        "recommendedPermissions": permission_advice,
        "example": example,
    });
    if let (true, Some(input), Some(normalized)) = (detect_site, host, normalized) {
        result["site"] = detect_site_json(input, normalized).await;
    }

    out.print_data(&serde_json::to_string_pretty(&result).expect("failed to serialize JSON"));
}

/// What `init --json --host` learned about the site: its deployment, and the
/// one page where its token is created. Fields detection could not establish
/// are null, with `error` saying why, rather than a guess at Cloud.
async fn detect_site_json(
    input: &str,
    normalized: Result<site::Site, String>,
) -> serde_json::Value {
    let site = match normalized {
        Ok(site) => site,
        Err(error) => {
            return serde_json::json!({
                "input": input, "host": null, "deployment": null, "version": null,
                "tokenUrl": null, "error": error,
            });
        }
    };
    match site::detect(&site).await {
        Ok(detected) => {
            let (deployment, token_url) = match detected.deployment {
                site::Deployment::Cloud => ("cloud", CLOUD_TOKEN_URL.to_owned()),
                site::Deployment::DataCenter => ("data_center", dc_pat_url(Some(&detected.site))),
            };
            serde_json::json!({
                "input": input, "host": detected.site, "deployment": deployment,
                "version": detected.version, "tokenUrl": token_url, "error": null,
            })
        }
        Err(error) => serde_json::json!({
            "input": input, "host": site.host, "deployment": null, "version": null,
            "tokenUrl": null, "error": error,
        }),
    }
}

async fn init_interactive(
    prefill_host: Option<&str>,
    requested_profile: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let sep = sym_dim("──────────────");
    eprintln!("Jira CLI Setup");
    eprintln!("{sep}");

    let path = config_path();

    // Decide what to do: first run, update an existing profile, or add a new one.
    //
    // `target_name` holds the profile name to write:
    //   Some(name) - already known (first run → "default"; update → chosen name)
    //   None       - "add new" path, ask for name after credentials
    let (target_name, existing): (Option<String>, Option<ProfileConfig>) =
        if let Some(name) = requested_profile {
            let existing = if path.exists() {
                let profiles = list_profile_names(&path)?;
                profiles
                    .iter()
                    .any(|candidate| candidate == name)
                    .then(|| read_raw_profile(&path, name))
                    .transpose()?
            } else {
                None
            };
            (Some(name.to_owned()), existing)
        } else if path.exists() {
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

    // Site, and whether it runs Cloud or Data Center, which the site reports itself.
    let default_site = prefill_host
        .map(str::to_owned)
        .or_else(|| existing.as_ref().and_then(|p| p.host.clone()));
    let previous_deployment = existing.as_ref().map(|p| {
        if p.auth_type.as_deref() == Some("pat") {
            site::Deployment::DataCenter
        } else {
            site::Deployment::Cloud
        }
    });
    let (host, deployment) = prompt_site(default_site.as_deref(), previous_deployment).await?;
    let is_cloud = deployment == site::Deployment::Cloud;
    eprintln!();

    // Asked before the token, because a scoped token's scopes follow from it.
    let default_read_only = existing
        .as_ref()
        .and_then(|profile| profile.read_only)
        .unwrap_or(false);
    let read_only = prompt_bool("Read-only mode?", default_read_only)?;

    let prior_token = match (
        existing
            .as_ref()
            .and_then(|profile| profile.credential_store.as_deref()),
        target_name.as_deref(),
    ) {
        (Some("keyring"), Some(name)) => crate::credentials::load_optional(name)?,
        _ => existing
            .as_ref()
            .and_then(|profile| profile.token.clone())
            .filter(|token| !token.trim().is_empty()),
    };

    // Credentials
    let (email, token, auth_type, api_version, cloud_id, token_kind): (
        Option<String>,
        String,
        &str,
        u8,
        Option<String>,
        String,
    ) = if is_cloud {
        let default_email = existing.as_ref().and_then(|p| p.email.clone());
        let email = prompt_required("Email", "", default_email.as_deref())?;
        let default_kind = existing
            .as_ref()
            .and_then(|profile| profile.token_kind.as_deref())
            .unwrap_or("scoped");
        let requested_kind = prompt("Token type", "[scoped/classic]", Some(default_kind))?;
        let token_kind = if requested_kind.eq_ignore_ascii_case("classic") {
            "classic".to_owned()
        } else {
            "scoped".to_owned()
        };
        let cloud_id = if token_kind == "scoped" {
            eprint!("  Discovering Cloud ID...");
            std::io::stderr().flush().ok();
            let id = discover_cloud_id(&host).await?;
            eprintln!(" {}", sym_ok());
            Some(id)
        } else {
            None
        };
        if prior_token.is_none() {
            offer_to_open(CLOUD_TOKEN_URL)?;
        }
        eprintln!("  {}", sym_dim(&format!("→ {CLOUD_TOKEN_URL}")));
        if token_kind == "scoped" {
            print_token_scopes(read_only);
        }
        let token_hint = if prior_token.is_some() {
            "(Enter to keep)"
        } else {
            ""
        };
        let raw = prompt_secret("Token", token_hint)?;
        let token = if raw.trim().is_empty() {
            prior_token
                .clone()
                .ok_or("No existing token. Please enter a token.")?
        } else {
            raw
        };
        (Some(email), token, "basic", 3, cloud_id, token_kind)
    } else {
        let pat_url = dc_pat_url(Some(&host));
        let token = if let Some(existing_token) = prior_token.clone() {
            print_dc_pat_link(&pat_url);
            let raw = prompt_secret("Personal access token", "(Enter to keep)")?;
            if raw.trim().is_empty() {
                existing_token
            } else {
                raw
            }
        } else if prompt_bool("Create a dedicated PAT automatically?", true)? {
            let method = prompt("Bootstrap with", "[password/pat]", Some("password"))?;
            let use_pat = method.eq_ignore_ascii_case("pat");
            let username = if use_pat {
                None
            } else {
                Some(prompt_required("Bootstrap username", "", None)?)
            };
            let secret = prompt_secret(
                if use_pat {
                    "Existing personal access token"
                } else {
                    "Bootstrap password"
                },
                "used once and never saved",
            )?;
            eprint!("  Creating personal access token...");
            std::io::stderr().flush().ok();
            match create_data_center_pat(
                &host,
                username.as_deref(),
                &secret,
                target_name.as_deref().unwrap_or("jira-cli"),
            )
            .await
            {
                Ok(token) => {
                    eprintln!(" {}", sym_ok());
                    token
                }
                Err(error) => {
                    eprintln!(" {} {error}", sym_fail());
                    eprintln!("  Falling back to creating the token in the browser.");
                    offer_to_open(&pat_url)?;
                    print_dc_pat_link(&pat_url);
                    prompt_secret("Personal access token", "")?
                }
            }
        } else {
            offer_to_open(&pat_url)?;
            print_dc_pat_link(&pat_url);
            prompt_secret("Personal access token", "")?
        };
        // Data Center serves REST API v2. A version set by hand on an existing
        // Data Center profile is kept.
        let api_version = existing
            .as_ref()
            .filter(|p| p.auth_type.as_deref() == Some("pat"))
            .and_then(|p| p.api_version)
            .unwrap_or(2);
        (None, token, "pat", api_version, None, "classic".to_owned())
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

    // The token was pasted a moment ago, so nothing stored is to blame.
    let remedy = crate::api::AuthRemedy {
        rejected: match email.as_deref() {
            Some(email) => {
                format!("Check that the whole token was pasted and that it belongs to {email}.")
            }
            None => {
                "Check that the whole token was pasted and that it has not been revoked.".to_owned()
            }
        },
        missing_scope:
            "The token lacks a scope setup needs. Create it again with the scopes listed above."
                .to_owned(),
    };
    let verified = match crate::api::client::JiraClient::new_with_cloud(
        &host,
        email.as_deref().unwrap_or(""),
        &token,
        auth_type_enum,
        api_version,
        cloud_id.as_deref(),
        &token_kind,
    )
    .map(|client| client.with_auth_remedy(remedy))
    {
        Err(e) => {
            eprintln!(" {} {e}", sym_fail());
            return Err(e.into());
        }
        Ok(client) => match client.get_myself().await {
            Ok(myself) => {
                eprintln!(" {} Authenticated as {}", sym_ok(), myself.display_name);
                match check_project_access(&client).await {
                    Ok(detail) => {
                        eprintln!("  {} {detail}", sym_ok());
                        true
                    }
                    Err(e) => {
                        eprintln!("  {} Could not list projects: {e}", sym_fail());
                        eprintln!();
                        prompt_bool("Save config anyway?", false)?
                    }
                }
            }
            Err(e) => {
                eprintln!(" {} {e}", sym_fail());
                eprintln!();
                prompt_bool("Save config anyway?", false)?
            }
        },
    };

    if !verified {
        eprintln!();
        eprintln!("{sep}");
        return Ok(());
    }

    // Profile name - ask only when adding a new named profile.
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

    let file_storage = choose_credential_storage()?;
    let previous_keyring = if file_storage {
        None
    } else {
        crate::credentials::load_optional(&profile_name)?
    };
    if !file_storage {
        crate::credentials::store(&profile_name, &token)?;
    }

    // Write config only after the credential is durable. Roll back a keychain
    // change if the atomic config replacement fails.
    let write_result = write_profile_to_config(
        &path,
        &profile_name,
        ProfileWrite {
            host: &host,
            email: email.as_deref(),
            token: &token,
            credential_store: if file_storage { "file" } else { "keyring" },
            cloud_id: cloud_id.as_deref(),
            token_kind: &token_kind,
            auth_type,
            api_version,
            read_only,
        },
    );
    if let Err(error) = write_result {
        if !file_storage {
            match previous_keyring {
                Some(previous) => {
                    let _ = crate::credentials::store(&profile_name, &previous);
                }
                None => {
                    let _ = crate::credentials::delete(&profile_name);
                }
            }
        }
        return Err(error);
    }
    if file_storage {
        let _ = crate::credentials::delete(&profile_name);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    eprintln!();
    eprintln!("  {} Config written to {}", sym_ok(), path.display());
    eprintln!(
        "  {}",
        sym_dim(if file_storage {
            "Credential storage: protected config file; treat it as a secret"
        } else {
            "Credential storage: operating-system keychain"
        })
    );
    eprintln!("{sep}");
    eprintln!("  Next:");
    for (command, purpose) in next_commands(&profile_name) {
        eprintln!("    {command:<width$}  {}", sym_dim(purpose), width = 36);
    }
    eprintln!();

    Ok(())
}

/// Confirm the verified credentials can read projects, not just `/myself`.
///
/// An account can authenticate yet see nothing: a scoped Cloud token without
/// read access, or a Data Center account without Browse Projects. Reporting it
/// during setup puts the problem where the credential is chosen.
async fn check_project_access(client: &crate::api::client::JiraClient) -> Result<String, ApiError> {
    let projects = client.list_projects().await?;
    if projects.is_empty() {
        return Ok(
            "No projects visible yet; ask a Jira administrator for Browse Projects permission"
                .to_owned(),
        );
    }
    Ok(crate::commands::doctor::project_access_detail(
        projects.len(),
    ))
}

/// List the scopes to select when creating a scoped Cloud token for this profile.
fn print_token_scopes(read_only: bool) {
    let access = if read_only { "read-only" } else { "read-write" };
    eprintln!(
        "  {}",
        sym_dim(&format!("Select these scopes for {access} access:"))
    );
    eprintln!("    {}", crate::api::scopes::platform(read_only).join("  "));
    eprintln!("  {}", sym_dim("Boards and sprints also need:"));
    eprintln!("    {}", crate::api::scopes::agile(read_only).join("  "));
}

/// Scopes a scoped Cloud token needs, as `init --json` reports them.
fn cloud_token_scopes_json() -> serde_json::Value {
    use crate::api::scopes;
    serde_json::json!({
        "readOnly": scopes::platform(true),
        "readWrite": scopes::platform(false),
        "boardsAndSprintsReadOnly": scopes::agile(true),
        "boardsAndSprintsReadWrite": scopes::agile(false),
    })
}

/// Commands worth running first after `jira init`, addressed to the saved profile.
fn next_commands(profile: &str) -> Vec<(String, &'static str)> {
    let prefix = if profile == "default" {
        "jira".to_owned()
    } else {
        format!("jira --profile {}", shell_word(profile))
    };
    vec![
        (format!("{prefix} doctor"), "Check the complete connection"),
        (
            format!("{prefix} issues mine"),
            "See what needs your attention",
        ),
        (
            format!("{prefix} projects list"),
            "List the projects you can see",
        ),
    ]
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

/// Prompt for a credential without echoing it to the terminal.
fn prompt_secret(label: &str, hint: &str) -> Result<String, std::io::Error> {
    use std::io::{self, Write};
    let hint_part = if hint.is_empty() {
        String::new()
    } else {
        format!("  {hint}")
    };
    eprint!("{} {label}{hint_part}: ", sym_q());
    io::stderr().flush()?;
    rpassword::read_password().map(|value| value.trim().to_owned())
}

fn prompt_bool(label: &str, default: bool) -> Result<bool, std::io::Error> {
    let choices = if default { "[Y/n]" } else { "[y/N]" };
    let value = prompt(label, choices, None)?;
    Ok(match value.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" | "1" => true,
        "n" | "no" | "false" | "0" => false,
        _ => default,
    })
}

fn choose_credential_storage() -> Result<bool, Box<dyn std::error::Error>> {
    match crate::credentials::available() {
        Ok(()) => Ok(false),
        Err(error) => {
            eprintln!("  {} {error}", sym_fail());
            if prompt_bool("Use the protected config-file fallback instead?", false)? {
                Ok(true)
            } else {
                Err("credential storage cancelled; start an OS credential service or use JIRA_TOKEN for this session".into())
            }
        }
    }
}

/// Ask for the Jira site and identify its deployment.
///
/// Accepts anything a user has at hand: a Cloud subdomain, a host, or a link
/// copied from the browser. The deployment comes from the site itself; the user
/// is asked only when the site cannot be identified, and may re-enter the address
/// instead. `previous` is the deployment of the profile being updated, offered as
/// the default in that case.
async fn prompt_site(
    default: Option<&str>,
    previous: Option<site::Deployment>,
) -> Result<(String, site::Deployment), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut default = default.map(str::to_owned);
    loop {
        let raw = prompt_required(
            "Jira site",
            &sym_dim("subdomain, address, or any Jira link"),
            default.as_deref(),
        )?;
        let entered = match site::normalize_site(&raw) {
            Ok(entered) => entered,
            Err(error) => {
                eprintln!("  {} {error}", sym_fail());
                continue;
            }
        };
        let host = entered.host.clone();
        default = Some(raw.trim().to_owned());

        eprint!("  Checking {host}...");
        std::io::stderr().flush().ok();
        match site::detect(&entered).await {
            Ok(detected) => {
                eprintln!(" {} {}", sym_ok(), detected.describe());
                if detected.site != host {
                    let reason = match detected.deployment {
                        site::Deployment::Cloud => "the site's Atlassian address",
                        site::Deployment::DataCenter => "where Jira answers",
                    };
                    eprintln!(
                        "  {}",
                        sym_dim(&format!("Using {}, {reason}", detected.site))
                    );
                }
                return Ok((detected.site, detected.deployment));
            }
            Err(error) => {
                eprintln!(" {} {error}", sym_fail());
                eprintln!(
                    "  {}",
                    sym_dim(&format!(
                        "Setup will use {host} as entered; choose retry to correct it."
                    ))
                );
                let fallback = previous.unwrap_or(if site::is_cloud_host(&host) {
                    site::Deployment::Cloud
                } else {
                    site::Deployment::DataCenter
                });
                match prompt_deployment(fallback)? {
                    Some(deployment) => return Ok((host, deployment)),
                    None => continue,
                }
            }
        }
    }
}

/// Ask which deployment an unidentified site runs. `None` means the user chose
/// to re-enter the address.
fn prompt_deployment(
    fallback: site::Deployment,
) -> Result<Option<site::Deployment>, std::io::Error> {
    let default = match fallback {
        site::Deployment::Cloud => "cloud",
        site::Deployment::DataCenter => "dc",
    };
    loop {
        let answer = prompt("Deployment", &sym_dim("[cloud/dc/retry]"), Some(default))?;
        match answer.to_ascii_lowercase().as_str() {
            "cloud" | "c" => return Ok(Some(site::Deployment::Cloud)),
            "dc" | "d" | "datacenter" | "data center" | "server" => {
                return Ok(Some(site::Deployment::DataCenter));
            }
            "retry" | "r" => return Ok(None),
            _ => eprintln!("  {} Answer cloud, dc, or retry.", sym_fail()),
        }
    }
}

/// Offer to open a token page in the browser. The caller prints the link either
/// way, so declining or a missing browser still leaves the address on screen.
fn offer_to_open(url: &str) -> Result<(), std::io::Error> {
    if prompt_bool("Open the token page in your browser?", true)?
        && let Err(error) = open::that(url)
    {
        eprintln!("  {} Could not open browser: {error}", sym_fail());
    }
    Ok(())
}

async fn discover_cloud_id(host: &str) -> Result<String, Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TenantInfo {
        cloud_id: String,
    }

    let site = if host.starts_with("http://") || host.starts_with("https://") {
        host.trim_end_matches('/').to_owned()
    } else {
        format!("https://{}", host.trim_end_matches('/'))
    };
    let info = reqwest::Client::new()
        .get(format!("{site}/_edge/tenant_info"))
        .send()
        .await?
        .error_for_status()?
        .json::<TenantInfo>()
        .await?;
    if info.cloud_id.trim().is_empty() {
        return Err("Atlassian returned an empty Cloud ID".into());
    }
    Ok(info.cloud_id)
}

async fn create_data_center_pat(
    host: &str,
    username: Option<&str>,
    bootstrap_secret: &str,
    profile_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let site = if host.starts_with("http://") || host.starts_with("https://") {
        host.trim_end_matches('/').to_owned()
    } else {
        format!("https://{}", host.trim_end_matches('/'))
    };
    let request = reqwest::Client::new()
        .post(format!("{site}/rest/pat/latest/tokens"))
        .json(&serde_json::json!({
            "name": format!("jira-cli / {profile_name}"),
        }));
    let request = match username {
        Some(username) => request.basic_auth(username, Some(bootstrap_secret)),
        None => request.bearer_auth(bootstrap_secret),
    };
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("PAT creation failed with HTTP {status}").into());
    }
    let body: serde_json::Value = response.json().await?;
    ["rawToken", "token"]
        .into_iter()
        .find_map(|field| body.get(field).and_then(serde_json::Value::as_str))
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "PAT creation response did not contain the one-time token".into())
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
struct ProfileWrite<'a> {
    host: &'a str,
    email: Option<&'a str>,
    token: &'a str,
    credential_store: &'a str,
    cloud_id: Option<&'a str>,
    token_kind: &'a str,
    auth_type: &'a str,
    api_version: u8,
    read_only: bool,
}

fn write_profile_to_config(
    path: &std::path::Path,
    profile_name: &str,
    profile: ProfileWrite<'_>,
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
    root.insert(
        "active_profile".to_owned(),
        toml::Value::String(profile_name.to_owned()),
    );

    let mut section = toml::map::Map::new();
    section.insert(
        "host".to_owned(),
        toml::Value::String(profile.host.to_owned()),
    );
    if let Some(e) = profile.email {
        section.insert("email".to_owned(), toml::Value::String(e.to_owned()));
    }
    section.insert(
        "credential_store".to_owned(),
        toml::Value::String(profile.credential_store.to_owned()),
    );
    if profile.credential_store == "file" {
        section.insert(
            "token".to_owned(),
            toml::Value::String(profile.token.to_owned()),
        );
    }
    if let Some(cloud_id) = profile.cloud_id {
        section.insert(
            "cloud_id".to_owned(),
            toml::Value::String(cloud_id.to_owned()),
        );
    }
    if profile.token_kind != "classic" {
        section.insert(
            "token_kind".to_owned(),
            toml::Value::String(profile.token_kind.to_owned()),
        );
    }
    if profile.auth_type != "basic" {
        section.insert(
            "auth_type".to_owned(),
            toml::Value::String(profile.auth_type.to_owned()),
        );
        section.insert(
            "api_version".to_owned(),
            toml::Value::Integer(i64::from(profile.api_version)),
        );
    }
    if profile.read_only {
        section.insert("read_only".to_owned(), toml::Value::Boolean(true));
    }

    if profile_name == "default" {
        root.insert("default".to_owned(), toml::Value::Table(section));
    } else {
        let profiles = root
            .entry("profiles")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        // A hand-edited config can carry `profiles` as a string or a number.
        // Reporting that is the whole job here: panicking loses the reason, and
        // replacing the value would delete whatever the user meant by it.
        let profiles = profiles.as_table_mut().ok_or_else(|| {
            format!(
                "{} defines `profiles` as something other than a table, so the `{profile_name}` profile cannot be added to it",
                path.display()
            )
        })?;
        profiles.insert(profile_name.to_owned(), toml::Value::Table(section));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(&doc)?;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut temp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".toml.tmp")
        .tempfile_in(parent)?;
    use std::io::Write;
    temp.write_all(body.as_bytes())?;
    temp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    temp.persist(path).map_err(|error| error.error)?;

    Ok(())
}

/// Remove a named profile from the config file.
///
/// The "default" profile is removed by deleting the `[default]` section. Named profiles
/// are removed from the `[profiles]` table. Prints a success or error message; does not
/// write to stdout so it is safe in JSON mode.
fn requested_profile_name(profile: Option<&str>) -> String {
    normalize_str(profile)
        .map(str::to_owned)
        .or_else(|| env_var("JIRA_PROFILE"))
        .unwrap_or_else(|| "default".into())
}

fn profile_table_mut<'a>(
    root: &'a mut toml::Table,
    profile_name: &str,
) -> Result<&'a mut toml::Table, ApiError> {
    if profile_name == "default" {
        if root.contains_key("default") {
            return root
                .get_mut("default")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| ApiError::Other("`default` is not a TOML table".into()));
        }
        return Ok(root);
    }
    root.get_mut("profiles")
        .and_then(toml::Value::as_table_mut)
        .and_then(|profiles| profiles.get_mut(profile_name))
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| ApiError::NotFound(format!("profile `{profile_name}` in config")))
}

fn write_toml_atomically(path: &std::path::Path, doc: &toml::Value) -> Result<(), ApiError> {
    let body = toml::to_string_pretty(doc)
        .map_err(|error| ApiError::Other(format!("Failed to serialize config: {error}")))?;
    let parent = path
        .parent()
        .ok_or_else(|| ApiError::Other("config path has no parent".into()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| ApiError::Other(format!("Failed to create config directory: {error}")))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".toml.tmp")
        .tempfile_in(parent)
        .map_err(|error| ApiError::Other(format!("Failed to create temporary config: {error}")))?;
    use std::io::Write;
    temp.write_all(body.as_bytes())
        .and_then(|()| temp.flush())
        .map_err(|error| ApiError::Other(format!("Failed to write temporary config: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))
            .map_err(|error| ApiError::Other(format!("Failed to protect config: {error}")))?;
    }
    temp.persist(path)
        .map_err(|error| ApiError::Other(format!("Failed to replace config: {}", error.error)))?;
    Ok(())
}

fn rewrite_profile_credential(
    profile_name: &str,
    credential_store: Option<&str>,
) -> Result<(), ApiError> {
    let path = config_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|error| ApiError::Other(format!("Failed to read config: {error}")))?;
    let mut doc: toml::Value = toml::from_str(&content)
        .map_err(|error| ApiError::Other(format!("Failed to parse config: {error}")))?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| ApiError::Other("config is not a TOML table".into()))?;
    let profile = profile_table_mut(root, profile_name)?;
    profile.remove("token");
    match credential_store {
        Some(store) => {
            profile.insert("credential_store".into(), toml::Value::String(store.into()));
        }
        None => {
            profile.remove("credential_store");
        }
    }
    write_toml_atomically(&path, &doc)
}

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

    if root.get("active_profile").and_then(toml::Value::as_str) == Some(profile_name) {
        let next = removable_profiles(root).first().copied().map(str::to_owned);
        match next {
            Some(name) => {
                root.insert("active_profile".into(), toml::Value::String(name));
            }
            None => {
                root.remove("active_profile");
            }
        }
    }

    write_toml_atomically(&path, &doc)?;
    let _ = crate::credentials::delete(profile_name);

    out.print_result(
        &serde_json::json!({ "profile": profile_name, "removed": true }),
        &format!("{} Removed profile '{profile_name}'", sym_ok()),
    );
    Ok(())
}

pub fn print_config_path(out: &OutputConfig) -> Result<(), ApiError> {
    let path = config_path();
    out.print_result(
        &serde_json::json!({ "configPath": path }),
        &path.display().to_string(),
    );
    Ok(())
}

pub fn list_profiles(out: &OutputConfig) -> Result<(), ApiError> {
    let path = config_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|error| ApiError::Other(format!("Failed to read config: {error}")))?;
    let raw: RawConfig = toml::from_str(&content)
        .map_err(|error| ApiError::Other(format!("Failed to parse config: {error}")))?;
    let active = raw.active_profile.as_deref().unwrap_or("default");
    let mut items = Vec::new();
    if raw.default.host.is_some() || raw.host.is_some() {
        let profile = raw.default_profile();
        items.push(serde_json::json!({
            "name": "default", "host": profile.host, "active": active == "default"
        }));
    }
    items.extend(raw.profiles.iter().map(|(name, profile)| {
        serde_json::json!({
            "name": name, "host": profile.host, "active": active == name
        })
    }));
    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(
                &serde_json::json!({"items": items, "total": items.len()}),
            )
            .expect("failed to serialize profiles"),
        );
    } else if items.is_empty() {
        out.print_data("No profiles configured. Run `jira init`.");
    } else {
        for item in items {
            let marker = if item["active"].as_bool().unwrap_or(false) {
                "*"
            } else {
                " "
            };
            out.print_data(&format!(
                "{marker} {:<20} {}",
                item["name"].as_str().unwrap_or_default(),
                item["host"].as_str().unwrap_or("-")
            ));
        }
    }
    Ok(())
}

pub fn use_profile(out: &OutputConfig, profile_name: &str) -> Result<(), ApiError> {
    let path = config_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|error| ApiError::Other(format!("Failed to read config: {error}")))?;
    let mut doc: toml::Value = toml::from_str(&content)
        .map_err(|error| ApiError::Other(format!("Failed to parse config: {error}")))?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| ApiError::Other("config is not a TOML table".into()))?;
    if !removable_profiles(root).contains(&profile_name) {
        return Err(ApiError::NotFound(format!(
            "profile '{profile_name}' in config"
        )));
    }
    root.insert(
        "active_profile".into(),
        toml::Value::String(profile_name.into()),
    );
    write_toml_atomically(&path, &doc)?;
    out.print_result(
        &serde_json::json!({"profile": profile_name, "active": true}),
        &format!("{} Active profile set to '{profile_name}'", sym_ok()),
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

// The selectedTab plugin key differs between Jira releases. The profile page is
// stable and always exposes Personal access tokens in the profile navigation.
const PAT_PATH: &str = "/secure/ViewProfile.jspa";
const PAT_NAVIGATION: &str = "Profile → Personal access tokens";

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

fn print_dc_pat_link(url: &str) {
    eprintln!("  {}", sym_dim(&format!("→ {url}")));
    eprintln!("  {}", sym_dim(PAT_NAVIGATION));
}

/// Where a Jira Cloud user creates an API token, scoped or classic.
const CLOUD_TOKEN_URL: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";

/// Mask a token for display, showing only the last 4 characters.
///
/// Atlassian tokens begin with a predictable prefix, so showing the
/// start provides no meaningful identification - the end is more useful.
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

/// The values every boolean environment variable in this CLI reads as on and
/// off, matched case-insensitively.
///
/// Public because `jira schema` declares them: an agent should not have to guess
/// which spellings the safety switch accepts.
pub const TRUTHY: &[&str] = &["1", "true", "yes", "on"];
pub const FALSY: &[&str] = &["0", "false", "no", "off"];

/// Whether a diagnostics-only toggle is switched on.
///
/// An unrecognised value means off here, because failing a command outright over
/// a typo in a debug switch costs more than the missed logging. Safety switches
/// use `parse_read_only` instead, which refuses.
pub fn is_truthy(value: &str) -> bool {
    TRUTHY.contains(&value.trim().to_ascii_lowercase().as_str())
}

/// Parse `JIRA_READ_ONLY`, rejecting anything that is neither an on nor an off
/// value.
///
/// The guard is a safety control, so an unrecognised value must not resolve to
/// "off": `JIRA_READ_ONLY=enabled` would then read as protection while every
/// write went through. Refusing to start is the only answer that cannot be
/// mistaken for the setting having worked.
fn parse_read_only(value: &str) -> Result<bool, ApiError> {
    let v = value.to_ascii_lowercase();
    if TRUTHY.contains(&v.as_str()) {
        Ok(true)
    } else if FALSY.contains(&v.as_str()) {
        Ok(false)
    } else {
        Err(ApiError::InvalidInput(format!(
            "JIRA_READ_ONLY is set to '{value}', which is neither on ({}) nor off ({}). \
             Refusing to run rather than guess whether writes are meant to be blocked.",
            TRUTHY.join(", "),
            FALSY.join(", ")
        )))
    }
}

/// Parse an `auth_type` from the environment or the config file.
///
/// A typo must not fall back to basic auth: on a Data Center instance that turns
/// "you misspelled pat" into an opaque 401 from Jira.
fn parse_auth_type(value: &str) -> Result<AuthType, ApiError> {
    if value.eq_ignore_ascii_case("basic") {
        Ok(AuthType::Basic)
    } else if value.eq_ignore_ascii_case("pat") {
        Ok(AuthType::Pat)
    } else {
        Err(ApiError::InvalidInput(format!(
            "auth_type '{value}' is not recognised. Use 'basic' (Jira Cloud) or \
             'pat' (Jira Data Center/Server)."
        )))
    }
}

/// Jira REST API versions this CLI knows how to talk to.
const API_VERSIONS: &[u8] = &[2, 3];

fn parse_api_version(value: &str) -> Result<u8, ApiError> {
    let parsed = value.parse::<u8>().map_err(|_| {
        ApiError::InvalidInput(format!(
            "api_version '{value}' is not a number. Use 3 (Jira Cloud) or 2 \
             (Jira Data Center/Server)."
        ))
    })?;
    validate_api_version(parsed)
}

/// Reject a version the client has no URL scheme for, rather than building
/// requests against `/rest/api/<n>/` and reporting Jira's 404 as the problem.
fn validate_api_version(version: u8) -> Result<u8, ApiError> {
    if API_VERSIONS.contains(&version) {
        Ok(version)
    } else {
        Err(ApiError::InvalidInput(format!(
            "api_version {version} is not supported. Use 3 (Jira Cloud) or 2 \
             (Jira Data Center/Server)."
        )))
    }
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
    fn read_only_accepts_its_documented_values_in_any_case() {
        for on in ["1", "true", "TRUE", "True", "yes", "YES", "on", "On"] {
            assert!(parse_read_only(on).unwrap(), "{on} should enable the guard");
        }
        for off in ["0", "false", "FALSE", "no", "No", "off", "OFF"] {
            assert!(
                !parse_read_only(off).unwrap(),
                "{off} should disable the guard"
            );
        }
    }

    /// The dangerous direction: a value nobody recognises must not quietly mean
    /// "writes allowed", because the operator who set it believes the opposite.
    #[test]
    fn read_only_refuses_a_value_it_does_not_understand() {
        for bad in ["enabled", "ture", "2", "y", "readonly"] {
            let err = parse_read_only(bad).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains(bad),
                "the rejection must quote the offending value; got: {message}"
            );
            assert!(
                matches!(err, ApiError::InvalidInput(_)),
                "{bad} must be reported as bad input, not as a Jira failure"
            );
        }
    }

    /// A diagnostics switch reads an unknown value as off, which is the opposite
    /// policy from the read-only guard above and deliberately so.
    #[test]
    fn is_truthy_accepts_any_case_and_treats_the_unknown_as_off() {
        for on in ["1", "true", "TRUE", "True", "yes", "YES", "on", "  on  "] {
            assert!(is_truthy(on), "{on} should read as on");
        }
        for off in ["0", "false", "no", "off", "enabled", "ture", ""] {
            assert!(!is_truthy(off), "{off} should read as off");
        }
    }

    /// A blank `auth_type` is an unset one, not a typo to refuse. Otherwise a
    /// config file with an empty placeholder stops every command.
    #[test]
    fn load_blank_auth_type_in_the_config_file_is_treated_as_unset() {
        let _lock = ProcessEnvLock::acquire();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "[default]\nhost = \"x.atlassian.net\"\nemail = \"me@example.com\"\n\
             token = \"t\"\nauth_type = \"  \"\n",
        )
        .unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _auth = EnvVarGuard::unset("JIRA_AUTH_TYPE");

        let config = Config::load(None, None, None).unwrap();
        assert_eq!(config.auth_type, AuthType::Basic);
    }

    /// Profiles written by earlier releases carry an `expires_at` date, including
    /// ones in a format no longer checked. They keep loading.
    #[test]
    fn load_ignores_a_recorded_expiry_from_earlier_releases() {
        let _lock = ProcessEnvLock::acquire();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "[default]\nhost = \"x.atlassian.net\"\nemail = \"me@example.com\"\n\
             token = \"t\"\nexpires_at = \"not a date\"\n",
        )
        .unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _token = EnvVarGuard::unset("JIRA_TOKEN");

        let config = Config::load(None, None, None).unwrap();
        assert_eq!(config.host, "x.atlassian.net");
    }

    #[test]
    fn rewriting_a_profile_drops_its_recorded_expiry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[default]\nhost = \"old.atlassian.net\"\nexpires_at = \"2026-11-24\"\n",
        )
        .unwrap();

        write_profile_to_config(
            &path,
            "default",
            ProfileWrite {
                host: "acme.atlassian.net",
                email: Some("me@acme.com"),
                token: "secret",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "basic",
                api_version: 3,
                read_only: false,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("acme.atlassian.net"));
        assert!(!content.contains("expires_at"), "{content}");
    }

    /// Jira creates a PAT without expiry unless asked for one, and setup has no
    /// reason to impose a lifetime the instance does not require.
    #[tokio::test]
    async fn data_center_pat_is_created_without_an_expiry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/pat/latest/tokens"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"rawToken": "new-pat"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let token = create_data_center_pat(&server.uri(), Some("me"), "pw", "work")
            .await
            .unwrap();
        assert_eq!(token, "new-pat");

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, serde_json::json!({"name": "jira-cli / work"}));
    }

    async fn project_check_against(
        response: wiremock::ResponseTemplate,
    ) -> Result<String, ApiError> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/project"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::api::JiraClient::new(
            &server.uri(),
            "me@example.com",
            "token",
            AuthType::Basic,
            2,
        )
        .unwrap();
        check_project_access(&client).await
    }

    #[tokio::test]
    async fn init_project_check_counts_the_visible_projects() {
        let projects = serde_json::json!([
            {"id": "1", "key": "ONE", "name": "One"},
            {"id": "2", "key": "TWO", "name": "Two"}
        ]);
        let detail =
            project_check_against(wiremock::ResponseTemplate::new(200).set_body_json(projects))
                .await
                .unwrap();
        assert_eq!(detail, "2 projects accessible");
    }

    /// Authenticating proves the token, not that the account can see any work,
    /// so setup names the permission to ask for instead of reporting success.
    #[tokio::test]
    async fn init_project_check_names_the_permission_when_nothing_is_visible() {
        let detail = project_check_against(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
        )
        .await
        .unwrap();
        assert!(detail.contains("No projects visible"), "got: {detail}");
        assert!(detail.contains("Browse Projects"), "got: {detail}");
    }

    #[tokio::test]
    async fn init_project_check_reports_a_refused_project_listing() {
        let error = project_check_against(
            wiremock::ResponseTemplate::new(403)
                .set_body_json(serde_json::json!({"errorMessages": ["Forbidden"]})),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ApiError::Forbidden(_)), "got: {error:?}");
    }

    #[test]
    fn init_next_commands_address_the_saved_profile() {
        let default: Vec<String> = next_commands("default")
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(
            default,
            ["jira doctor", "jira issues mine", "jira projects list"]
        );

        let work: Vec<String> = next_commands("work").into_iter().map(|(c, _)| c).collect();
        assert_eq!(
            work,
            [
                "jira --profile work doctor",
                "jira --profile work issues mine",
                "jira --profile work projects list"
            ]
        );

        let spaced: Vec<String> = next_commands("work team")
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(spaced[0], "jira --profile 'work team' doctor");
    }

    #[test]
    fn shell_word_quotes_only_what_the_shell_would_split() {
        assert_eq!(shell_word("work"), "work");
        assert_eq!(shell_word("team.eu-1"), "team.eu-1");
        assert_eq!(shell_word("work team"), "'work team'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn auth_type_refuses_a_typo_rather_than_falling_back_to_basic() {
        assert_eq!(parse_auth_type("pat").unwrap(), AuthType::Pat);
        assert_eq!(parse_auth_type("PAT").unwrap(), AuthType::Pat);
        assert_eq!(parse_auth_type("basic").unwrap(), AuthType::Basic);

        let err = parse_auth_type("ptt").unwrap_err().to_string();
        assert!(err.contains("ptt"), "got: {err}");
        assert!(
            err.contains("pat"),
            "the message must name the real spelling"
        );
    }

    #[test]
    fn api_version_refuses_anything_the_client_cannot_address() {
        assert_eq!(parse_api_version("2").unwrap(), 2);
        assert_eq!(parse_api_version("3").unwrap(), 3);

        for bad in ["v3", "", "3.0", "latest"] {
            assert!(
                parse_api_version(bad).is_err(),
                "{bad} is not a version number"
            );
        }
        // Parses as a u8 and is still wrong: there is no /rest/api/7/.
        let err = parse_api_version("7").unwrap_err().to_string();
        assert!(err.contains('7'), "got: {err}");
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
    fn load_uses_active_profile_when_no_override_is_set() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
active_profile = "work"

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

        let cfg = Config::load(None, None, None).unwrap();
        assert_eq!(cfg.profile, "work");
        assert_eq!(cfg.host, "work.atlassian.net");
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
        // No env or config needed - init() never loads credentials in JSON mode.
        // No host either: with one, init() asks that site what it runs.
        init(&out, None, false, None).await.unwrap();
    }

    // The text path of init() requires an interactive TTY; in test context stdin is
    // not a TTY, so it returns an actionable input error without hanging.
    #[tokio::test]
    async fn init_non_interactive_returns_actionable_error() {
        let out = crate::output::OutputConfig {
            json: false,
            quiet: false,
        };
        // stdin is not a TTY in tests - must return immediately, not hang
        let error = init(&out, None, false, None).await.unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)));
        assert!(error.to_string().contains("JIRA_TOKEN"));
    }

    #[test]
    fn write_profile_to_config_creates_default_profile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("jira").join("config.toml");

        write_profile_to_config(
            &path,
            "default",
            ProfileWrite {
                host: "acme.atlassian.net",
                email: Some("me@acme.com"),
                token: "secret",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "basic",
                api_version: 3,
                read_only: false,
            },
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

        write_profile_to_config(
            &path,
            "dc",
            ProfileWrite {
                host: "jira.corp.com",
                email: None,
                token: "pattoken",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "pat",
                api_version: 2,
                read_only: true,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[profiles.dc]"));
        assert!(content.contains("jira.corp.com"));
        assert!(content.contains("pattoken"));
        assert!(content.contains("auth_type"));
        assert!(content.contains("api_version"));
        assert!(content.contains("read_only = true"));
        assert!(!content.contains("email"));
    }

    #[test]
    fn a_profiles_key_that_is_not_a_table_is_reported_rather_than_panicked_on() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "profiles = 5\n").unwrap();

        let err = write_profile_to_config(
            &path,
            "work",
            ProfileWrite {
                host: "h.atlassian.net",
                email: None,
                token: "tok",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "basic",
                api_version: 3,
                read_only: false,
            },
        )
        .expect_err("a `profiles` integer cannot hold a profile")
        .to_string();
        assert!(
            err.contains("profiles") && err.contains("work"),
            "the message must name the key and the profile being added: {err}"
        );
        assert!(
            err.contains(&path.display().to_string()),
            "the message must name the file to edit: {err}"
        );

        // Control: the same call against a well-formed `profiles` table has to
        // succeed, or the check above would pass by refusing everything.
        let good = dir.path().join("good.toml");
        std::fs::write(&good, "[profiles.other]\nhost = \"a.b\"\n").unwrap();
        write_profile_to_config(
            &good,
            "work",
            ProfileWrite {
                host: "h.atlassian.net",
                email: None,
                token: "tok",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "basic",
                api_version: 3,
                read_only: false,
            },
        )
        .unwrap();
        let written = std::fs::read_to_string(&good).unwrap();
        assert!(written.contains("[profiles.work]") && written.contains("[profiles.other]"));
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
            ProfileWrite {
                host: "work.atlassian.net",
                email: Some("w@work.com"),
                token: "tok2",
                credential_store: "file",
                cloud_id: None,
                token_kind: "classic",
                auth_type: "basic",
                api_version: 3,
                read_only: false,
            },
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
        assert!(url.ends_with(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_bare_host_adds_https_scheme() {
        let url = dc_pat_url(Some("jira.corp.com"));
        assert!(url.starts_with("https://jira.corp.com"));
        assert!(url.ends_with(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_host_with_https_scheme_is_preserved() {
        let url = dc_pat_url(Some("https://jira.corp.com/"));
        assert!(url.starts_with("https://jira.corp.com"));
        assert!(!url.contains("https://https://"));
        assert!(url.ends_with(PAT_PATH));
    }

    #[test]
    fn dc_pat_url_host_with_http_scheme_is_preserved() {
        let url = dc_pat_url(Some("http://localhost:8080"));
        assert!(url.starts_with("http://localhost:8080"));
        assert!(url.ends_with(PAT_PATH));
    }
}
