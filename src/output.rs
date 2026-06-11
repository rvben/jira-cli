use std::io::IsTerminal;

/// Whether to use colored output (only when stdout is a terminal).
pub fn use_color() -> bool {
    std::io::stdout().is_terminal()
}

/// Format a URL as a clickable OSC 8 hyperlink in terminals that support it.
///
/// Modern terminals (iTerm2, Ghostty, Warp, VTE-based) render this as a
/// clickable link. Falls back to the bare URL when not on a color TTY.
pub fn hyperlink(url: &str) -> String {
    if use_color() {
        format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\")
    } else {
        url.to_string()
    }
}

/// Output configuration for agent-friendly CLI design.
///
/// Supports TTY detection (auto-JSON when piped), quiet mode,
/// and structured JSON output for all commands including mutations.
#[derive(Clone, Copy)]
pub struct OutputConfig {
    pub json: bool,
    pub quiet: bool,
}

impl OutputConfig {
    pub fn new(json_flag: bool, text_flag: bool, quiet: bool) -> Self {
        let json = if text_flag {
            false
        } else {
            json_flag || !std::io::stdout().is_terminal()
        };
        Self { json, quiet }
    }

    /// Print data to stdout (tables or JSON). Always shown.
    pub fn print_data(&self, data: &str) {
        println!("{data}");
    }

    /// Print an informational message to stderr. Suppressed by --quiet.
    pub fn print_message(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }

    /// Print the result of a mutation command.
    ///
    /// In JSON mode: prints structured JSON to stdout.
    /// In human mode: prints the human message to stdout (not stderr),
    /// since mutation results are data the caller may want to capture.
    pub fn print_result(&self, json_value: &serde_json::Value, human_message: &str) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(json_value).expect("failed to serialize JSON")
            );
        } else {
            println!("{human_message}");
        }
    }
}

/// Write a structured error envelope as the last line of stderr.
///
/// Consumers can parse this JSON to branch on `error.kind` without
/// parsing free-form error text.
pub fn print_error_envelope(kind: &str, message: &str) {
    let envelope = serde_json::json!({
        "error": {
            "kind": kind,
            "message": message
        }
    });
    eprintln!(
        "{}",
        serde_json::to_string(&envelope).unwrap_or_else(|_| {
            r#"{"error":{"kind":"unexpected_error","message":"serialization failed"}}"#.into()
        })
    );
}

/// Exit codes for agent-friendly error handling.
/// Agents can branch on specific failure modes without parsing error text.
pub mod exit_codes {
    /// Command succeeded.
    pub const SUCCESS: i32 = 0;
    /// General / unexpected error.
    pub const GENERAL_ERROR: i32 = 1;
    /// Bad user input or config error (wrong key format, missing config, etc.).
    pub const INPUT_ERROR: i32 = 2;
    /// Authentication failed (bad or missing token).
    pub const AUTH_ERROR: i32 = 3;
    /// Resource not found.
    pub const NOT_FOUND: i32 = 4;
    /// Jira API returned a non-2xx error.
    pub const API_ERROR: i32 = 5;
    /// Rate limited by Jira.
    pub const RATE_LIMIT: i32 = 6;
}

/// Map an error to a specific exit code by downcasting to ApiError.
pub fn exit_code_for_error(err: &(dyn std::error::Error + 'static)) -> i32 {
    if let Some(api_err) = err.downcast_ref::<crate::api::ApiError>() {
        match api_err {
            crate::api::ApiError::Auth(_) => exit_codes::AUTH_ERROR,
            crate::api::ApiError::NotFound(_) => exit_codes::NOT_FOUND,
            crate::api::ApiError::InvalidInput(_) => exit_codes::INPUT_ERROR,
            crate::api::ApiError::RateLimit => exit_codes::RATE_LIMIT,
            crate::api::ApiError::Api { .. } => exit_codes::API_ERROR,
            crate::api::ApiError::Http(_) | crate::api::ApiError::Other(_) => {
                exit_codes::GENERAL_ERROR
            }
        }
    } else {
        exit_codes::GENERAL_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;

    #[test]
    fn exit_code_for_auth_error() {
        let err = ApiError::Auth("bad token".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::AUTH_ERROR);
    }

    #[test]
    fn exit_code_for_not_found() {
        let err = ApiError::NotFound("PROJ-123".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::NOT_FOUND);
    }

    #[test]
    fn exit_code_for_invalid_input() {
        let err = ApiError::InvalidInput("bad key format".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::INPUT_ERROR);
    }

    #[test]
    fn exit_code_for_rate_limit() {
        let err = ApiError::RateLimit;
        assert_eq!(exit_code_for_error(&err), exit_codes::RATE_LIMIT);
    }

    #[test]
    fn exit_code_for_api_error() {
        let err = ApiError::Api {
            status: 500,
            message: "Internal Server Error".into(),
        };
        assert_eq!(exit_code_for_error(&err), exit_codes::API_ERROR);
    }

    #[test]
    fn exit_code_for_other_error() {
        let err = ApiError::Other("something".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::GENERAL_ERROR);
    }

    #[test]
    fn exit_code_for_http_error_is_general() {
        // Build a reqwest::Error without a network call
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reqwest_err = rt.block_on(async {
            reqwest::Client::new()
                .get("http://127.0.0.1:1")
                .send()
                .await
                .unwrap_err()
        });
        let err = ApiError::Http(reqwest_err);
        assert_eq!(exit_code_for_error(&err), exit_codes::GENERAL_ERROR);
    }

    #[test]
    fn exit_code_for_non_api_error_is_general() {
        let err: Box<dyn std::error::Error> = "plain string error".into();
        assert_eq!(exit_code_for_error(err.as_ref()), exit_codes::GENERAL_ERROR);
    }

    #[test]
    fn print_result_json_mode_prints_structured_output() {
        // Exercises the json=true branch of print_result without crashing
        let out = OutputConfig {
            json: true,
            quiet: true,
        };
        out.print_result(&serde_json::json!({"key": "PROJ-1"}), "Created PROJ-1");
    }

    #[test]
    fn print_result_human_mode_uses_human_message() {
        let out = OutputConfig {
            json: false,
            quiet: true,
        };
        out.print_result(&serde_json::json!({"key": "PROJ-1"}), "Created PROJ-1");
    }

    #[test]
    fn print_message_suppressed_in_quiet_mode() {
        let out = OutputConfig {
            json: false,
            quiet: true,
        };
        out.print_message("this should be suppressed");
    }

    #[test]
    fn print_message_emits_in_non_quiet_mode() {
        let out = OutputConfig {
            json: false,
            quiet: false,
        };
        out.print_message("this goes to stderr");
    }

    #[test]
    fn hyperlink_without_tty_returns_bare_url() {
        // Tests always run without a TTY, so use_color() is false
        let url = "https://example.atlassian.net/browse/PROJ-1";
        assert_eq!(hyperlink(url), url);
    }
}
