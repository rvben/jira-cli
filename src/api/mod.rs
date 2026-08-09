pub mod client;
pub mod types;

pub use client::JiraClient;
pub use types::*;

use std::fmt;

/// Authentication method used when connecting to Jira.
///
/// `Basic` uses HTTP Basic auth with email and API token (Jira Cloud default).
/// `Pat` uses a Bearer token (Personal Access Token), typically for Jira Data Center / Server.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AuthType {
    #[default]
    Basic,
    Pat,
}

#[derive(Debug)]
pub enum ApiError {
    /// Bad credentials or forbidden.
    Auth(String),
    /// Resource not found.
    NotFound(String),
    /// Invalid user input (bad key format, missing required value, etc.).
    InvalidInput(String),
    /// A destructive operation was refused because it was not explicitly
    /// confirmed. Distinct from `InvalidInput` because the command line was
    /// well formed: adding `--yes` makes the identical request succeed.
    ConfirmationRequired(String),
    /// HTTP 429 rate limit.
    RateLimit,
    /// The request conflicts with the current state of something it would
    /// change: an HTTP 409 from Jira, or a local file the command would
    /// overwrite. Retrying unchanged reproduces the conflict, so the caller has
    /// to resolve it first.
    Conflict(String),
    /// Non-2xx response from the Jira API.
    Api { status: u16, message: String },
    /// Network / TLS error.
    Http(reqwest::Error),
    /// Any other error.
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Auth(msg) => write!(
                f,
                "Authentication failed: {msg}\nCheck JIRA_TOKEN or run `jira config show` to verify credentials."
            ),
            ApiError::NotFound(msg) => write!(f, "Not found: {msg}"),
            ApiError::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
            ApiError::ConfirmationRequired(msg) => write!(f, "Confirmation required: {msg}"),
            ApiError::RateLimit => write!(f, "Rate limited by Jira. Please wait and try again."),
            ApiError::Conflict(msg) => write!(f, "Conflict: {msg}"),
            ApiError::Api { status, message } => write!(f, "API error {status}: {message}"),
            ApiError::Http(e) => write!(f, "HTTP error: {e}"),
            ApiError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApiError::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Http(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn auth_error_display_includes_check_guidance() {
        let err = ApiError::Auth("invalid credentials".into());
        let msg = err.to_string();
        assert!(msg.contains("Authentication failed"));
        assert!(msg.contains("invalid credentials"));
        assert!(msg.contains("JIRA_TOKEN"), "should hint at how to fix auth");
    }

    #[test]
    fn not_found_error_display_includes_message() {
        let err = ApiError::NotFound("PROJ-999 not found".into());
        let msg = err.to_string();
        assert!(msg.contains("Not found"));
        assert!(msg.contains("PROJ-999"));
    }

    #[test]
    fn invalid_input_error_display_includes_message() {
        let err = ApiError::InvalidInput("host is required".into());
        let msg = err.to_string();
        assert!(msg.contains("Invalid input"));
        assert!(msg.contains("host is required"));
    }

    #[test]
    fn rate_limit_error_display_is_actionable() {
        let err = ApiError::RateLimit;
        let msg = err.to_string();
        assert!(msg.to_lowercase().contains("rate limit") || msg.contains("Rate limit"));
        assert!(msg.contains("wait"), "should tell user to wait");
    }

    #[test]
    fn api_error_display_includes_status_and_message() {
        let err = ApiError::Api {
            status: 422,
            message: "Field 'foo' is required".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("422"));
        assert!(msg.contains("Field 'foo' is required"));
    }

    #[test]
    fn other_error_display_is_message_verbatim() {
        let err = ApiError::Other("something unexpected".into());
        assert_eq!(err.to_string(), "something unexpected");
    }

    #[test]
    fn http_error_source_is_the_underlying_reqwest_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reqwest_err = rt.block_on(async {
            reqwest::Client::new()
                .get("http://127.0.0.1:1")
                .send()
                .await
                .unwrap_err()
        });
        let api_err = ApiError::Http(reqwest_err);
        assert!(
            api_err.source().is_some(),
            "Http variant must expose its source"
        );
    }

    #[test]
    fn non_http_variants_have_no_error_source() {
        assert!(ApiError::Auth("x".into()).source().is_none());
        assert!(ApiError::NotFound("x".into()).source().is_none());
        assert!(ApiError::InvalidInput("x".into()).source().is_none());
        assert!(
            ApiError::ConfirmationRequired("x".into())
                .source()
                .is_none()
        );
        assert!(ApiError::RateLimit.source().is_none());
        assert!(ApiError::Conflict("x".into()).source().is_none());
        assert!(ApiError::Other("x".into()).source().is_none());
    }

    #[test]
    fn conflict_error_display_includes_message() {
        let err = ApiError::Conflict("issue was edited by someone else".into());
        let msg = err.to_string();
        assert!(msg.contains("Conflict"));
        assert!(msg.contains("issue was edited by someone else"));
    }

    /// The refusal names the flag that resolves it, so the caller does not have
    /// to guess how to proceed.
    #[test]
    fn confirmation_required_display_names_the_remedy() {
        let err = ApiError::ConfirmationRequired("bulk-assign requires --yes".into());
        let msg = err.to_string();
        assert!(msg.contains("Confirmation required"));
        assert!(msg.contains("--yes"));
    }
}
