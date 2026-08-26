use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static NO_COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_no_color(disabled: bool) {
    NO_COLOR.store(disabled, Ordering::Relaxed);
}

/// Whether to use colored output (only when stdout is a terminal).
pub fn use_color() -> bool {
    !NO_COLOR.load(Ordering::Relaxed)
        && std::env::var_os("NO_COLOR").is_none()
        && std::io::stdout().is_terminal()
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
    /// Request conflicts with the current state of the resource.
    pub const CONFLICT: i32 = 7;
}

/// One failure mode of the CLI, in the form an agent consumes it.
///
/// This table is the single source of truth for the error contract: `jira
/// schema` renders it, the stderr envelope emits one of its `kind` values, and
/// the process exits with its `exit_code`. Declaring a kind here that no
/// `ApiError` can produce would promise agents a branch that never runs, so
/// every entry is pinned to a reachable variant by
/// `every_declared_error_kind_is_reachable`.
pub struct ErrorContract {
    pub kind: &'static str,
    pub exit_code: i32,
    /// Whether retrying the identical command can plausibly succeed.
    pub retryable: bool,
    pub description: &'static str,
}

pub static AUTH: ErrorContract = ErrorContract {
    kind: "auth",
    exit_code: exit_codes::AUTH_ERROR,
    retryable: false,
    description: "Authentication failed - bad or missing credentials",
};
pub static NOT_FOUND: ErrorContract = ErrorContract {
    kind: "not_found",
    exit_code: exit_codes::NOT_FOUND,
    retryable: false,
    description: "Requested resource does not exist",
};
pub static INVALID_INPUT: ErrorContract = ErrorContract {
    kind: "invalid_input",
    exit_code: exit_codes::INPUT_ERROR,
    retryable: false,
    description: "Bad user input or config error",
};
pub static CONFIRMATION_REQUIRED: ErrorContract = ErrorContract {
    kind: "confirmation_required",
    exit_code: exit_codes::INPUT_ERROR,
    retryable: false,
    description: "Destructive operation requires explicit confirmation (--yes)",
};
pub static RATE_LIMIT: ErrorContract = ErrorContract {
    kind: "rate_limit",
    exit_code: exit_codes::RATE_LIMIT,
    retryable: true,
    description: "Rate limited by Jira - wait and retry",
};
pub static API_ERROR: ErrorContract = ErrorContract {
    kind: "api_error",
    exit_code: exit_codes::API_ERROR,
    retryable: false,
    description: "Non-2xx response from the Jira API",
};
pub static UNEXPECTED_ERROR: ErrorContract = ErrorContract {
    kind: "unexpected_error",
    exit_code: exit_codes::GENERAL_ERROR,
    retryable: false,
    description: "Unexpected or unclassified error",
};
pub static CONFLICT: ErrorContract = ErrorContract {
    kind: "conflict",
    exit_code: exit_codes::CONFLICT,
    retryable: false,
    description: "Request conflicts with the current state of the resource - resolve the conflict before retrying",
};

/// Every failure mode the CLI can report, in schema declaration order.
///
/// New entries append, so an agent that indexed into this array keeps seeing
/// the same kinds at the same positions.
pub static ALL_ERRORS: &[&ErrorContract] = &[
    &AUTH,
    &NOT_FOUND,
    &INVALID_INPUT,
    &CONFIRMATION_REQUIRED,
    &RATE_LIMIT,
    &API_ERROR,
    &UNEXPECTED_ERROR,
    &CONFLICT,
];

/// The contract row describing how this error is reported.
pub fn contract_for(err: &crate::api::ApiError) -> &'static ErrorContract {
    use crate::api::ApiError;
    match err {
        ApiError::Auth(_) => &AUTH,
        ApiError::NotFound(_) => &NOT_FOUND,
        ApiError::InvalidInput(_) => &INVALID_INPUT,
        ApiError::ConfirmationRequired(_) => &CONFIRMATION_REQUIRED,
        ApiError::RateLimit => &RATE_LIMIT,
        ApiError::Conflict(_) => &CONFLICT,
        ApiError::Api { .. } => &API_ERROR,
        ApiError::Http(_) | ApiError::Other(_) => &UNEXPECTED_ERROR,
    }
}

/// The contract row for any error, falling back to `unexpected_error` for
/// errors that did not originate as an `ApiError`.
pub fn contract_for_dyn(err: &(dyn std::error::Error + 'static)) -> &'static ErrorContract {
    err.downcast_ref::<crate::api::ApiError>()
        .map_or(&UNEXPECTED_ERROR, contract_for)
}

/// Map an error to a specific exit code by downcasting to ApiError.
pub fn exit_code_for_error(err: &(dyn std::error::Error + 'static)) -> i32 {
    contract_for_dyn(err).exit_code
}

/// Whether errors should be machine-readable, decided from the raw command line.
///
/// Only needed when the parser rejected the arguments before `--output` could be
/// resolved. Mirrors `OutputConfig::new`: an explicit text request wins over an
/// explicit json one, and with neither, a non-terminal stdout means a machine is
/// reading. A value after `--` is positional, so the scan stops there.
///
/// This inspects a command line that has already failed to parse, so a literal
/// `--json` sitting in an option's value can steer the format. That only changes
/// how an error is rendered, never whether one occurs.
pub fn machine_readable_errors<I>(args: I, stdout_is_terminal: bool) -> bool
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut explicit_json = false;
    let mut explicit_text = false;
    let mut expecting_value = false;

    for arg in args {
        let arg = arg.as_ref();
        if expecting_value {
            expecting_value = false;
            match arg {
                "json" => explicit_json = true,
                "text" => explicit_text = true,
                _ => {}
            }
            continue;
        }
        match arg {
            "--" => break,
            "--json" => explicit_json = true,
            "-o" | "--output" => expecting_value = true,
            _ => {
                let value = arg
                    .strip_prefix("--output")
                    .or_else(|| arg.strip_prefix("-o"))
                    .map(|rest| rest.strip_prefix('=').unwrap_or(rest));
                match value {
                    Some("json") => explicit_json = true,
                    Some("text") => explicit_text = true,
                    _ => {}
                }
            }
        }
    }

    if explicit_text {
        false
    } else {
        explicit_json || !stdout_is_terminal
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

    /// One `ApiError` per declared kind, proving the kind is producible.
    ///
    /// `Http` is omitted deliberately: it shares `unexpected_error` with
    /// `Other`, and constructing one costs a failed network round trip.
    fn witnesses() -> Vec<ApiError> {
        vec![
            ApiError::Auth("x".into()),
            ApiError::NotFound("x".into()),
            ApiError::InvalidInput("x".into()),
            ApiError::ConfirmationRequired("x".into()),
            ApiError::RateLimit,
            ApiError::Conflict("x".into()),
            ApiError::Api {
                status: 500,
                message: "x".into(),
            },
            ApiError::Other("x".into()),
        ]
    }

    /// The contract `jira schema` publishes and the contract the binary can
    /// actually honour must be the same list.
    ///
    /// Both directions matter. A declared kind with no witness is a branch an
    /// agent writes and never reaches; commit 55711d9 removed one such phantom
    /// (`conflict`) and left another (`confirmation_required`) in place, which
    /// is what this test exists to stop recurring. A witnessed kind that is not
    /// declared is a failure mode an agent is never told about.
    #[test]
    fn every_declared_error_kind_is_reachable() {
        let declared: std::collections::BTreeSet<&str> =
            ALL_ERRORS.iter().map(|e| e.kind).collect();
        let reachable: std::collections::BTreeSet<&str> =
            witnesses().iter().map(|e| contract_for(e).kind).collect();

        assert_eq!(
            declared,
            reachable,
            "schema errors and emittable kinds diverged: \
             declared-but-unreachable {:?}, reachable-but-undeclared {:?}",
            declared.difference(&reachable).collect::<Vec<_>>(),
            reachable.difference(&declared).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn declared_kinds_are_unique() {
        let mut kinds: Vec<&str> = ALL_ERRORS.iter().map(|e| e.kind).collect();
        let before = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(before, kinds.len(), "duplicate error kind in the contract");
    }

    /// Retrying a conflict unchanged reproduces it, so an agent must be told not
    /// to loop on it. Only rate limiting clears on its own.
    #[test]
    fn only_rate_limit_is_retryable() {
        let retryable: Vec<&str> = ALL_ERRORS
            .iter()
            .filter(|e| e.retryable)
            .map(|e| e.kind)
            .collect();
        assert_eq!(retryable, vec!["rate_limit"]);
    }

    #[test]
    fn conflict_maps_to_its_own_exit_code() {
        let err = ApiError::Conflict("issue already resolved".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::CONFLICT);
        assert_eq!(contract_for(&err).kind, "conflict");
    }

    /// Refusing for want of `--yes` keeps the exit code it has always used, so
    /// only the kind becomes more specific and no caller's branch on 2 breaks.
    #[test]
    fn confirmation_required_keeps_the_input_error_exit_code() {
        let err = ApiError::ConfirmationRequired("needs --yes".into());
        assert_eq!(exit_code_for_error(&err), exit_codes::INPUT_ERROR);
        assert_eq!(contract_for(&err).kind, "confirmation_required");
    }

    #[test]
    fn machine_readable_errors_defaults_to_the_stdout_stream() {
        let none: [&str; 0] = [];
        assert!(
            machine_readable_errors(none, false),
            "piped stdout implies a machine reader"
        );
        assert!(
            !machine_readable_errors(none, true),
            "a terminal implies a human"
        );
    }

    #[test]
    fn machine_readable_errors_honours_every_json_spelling() {
        for args in [
            vec!["--json"],
            vec!["-o", "json"],
            vec!["-ojson"],
            vec!["-o=json"],
            vec!["--output", "json"],
            vec!["--output=json"],
        ] {
            assert!(
                machine_readable_errors(args.clone(), true),
                "{args:?} must select the machine-readable rendering"
            );
        }
    }

    #[test]
    fn machine_readable_errors_honours_every_text_spelling() {
        for args in [
            vec!["-o", "text"],
            vec!["-otext"],
            vec!["-o=text"],
            vec!["--output", "text"],
            vec!["--output=text"],
        ] {
            assert!(
                !machine_readable_errors(args.clone(), false),
                "{args:?} must select prose even when stdout is piped"
            );
        }
    }

    /// Mirrors `OutputConfig::new`, where an explicit text request wins.
    #[test]
    fn explicit_text_beats_explicit_json() {
        assert!(!machine_readable_errors(
            ["--json", "--output", "text"],
            false
        ));
        assert!(!machine_readable_errors(
            ["--output", "text", "--json"],
            false
        ));
    }

    /// Everything after `--` is a positional value, not a flag.
    #[test]
    fn machine_readable_errors_stops_at_the_positional_terminator() {
        assert!(!machine_readable_errors(["--", "--json"], true));
    }

    #[test]
    fn machine_readable_errors_ignores_unrelated_arguments() {
        assert!(!machine_readable_errors(
            ["issues", "list", "--project", "json"],
            true
        ));
    }

    #[test]
    fn hyperlink_without_tty_returns_bare_url() {
        // Tests always run without a TTY, so use_color() is false
        let url = "https://example.atlassian.net/browse/PROJ-1";
        assert_eq!(hyperlink(url), url);
    }
}
