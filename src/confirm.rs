//! The confirmation gate for commands whose writes cannot be undone or reach
//! many issues at once.
//!
//! `--yes` and `--dry-run` proceed without asking. Otherwise a person has to
//! see the question and answer it, so the command asks only when both stdin
//! and stderr are terminals and refuses before sending anything when either
//! is not. The question goes to stderr so stdout keeps only the result, in
//! whatever format was asked for. Piping `yes` into the command therefore
//! never authorizes a write: that is what `--yes` is for.

use crate::api::ApiError;
use std::io::{BufRead, IsTerminal, Write};

/// How a gated command may proceed, decided before any request is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// `--yes` or `--dry-run`: proceed without asking.
    Given,
    /// A person at a terminal answers once the command knows what it will change.
    Ask,
}

impl Consent {
    /// Refuse with `refusal` when consent was not given and cannot be asked for.
    pub fn resolve(yes: bool, dry_run: bool, refusal: &str) -> Result<Self, ApiError> {
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        Self::resolve_for(yes || dry_run, interactive, refusal)
    }

    fn resolve_for(given: bool, interactive: bool, refusal: &str) -> Result<Self, ApiError> {
        match (given, interactive) {
            (true, _) => Ok(Consent::Given),
            (false, true) => Ok(Consent::Ask),
            (false, false) => Err(ApiError::ConfirmationRequired(refusal.to_owned())),
        }
    }

    /// Ask `question` on the terminal unless consent was already given.
    pub fn confirm(self, question: &str) -> Result<(), ApiError> {
        match self {
            Consent::Given => Ok(()),
            Consent::Ask => ask(
                question,
                &mut std::io::stdin().lock(),
                &mut std::io::stderr(),
            ),
        }
    }
}

/// Write `question` and read one answer. Only `y` or `yes`, in any case,
/// proceeds; anything else, including an empty line or end of input, declines.
pub fn ask(
    question: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), ApiError> {
    let io =
        |error: std::io::Error| ApiError::Other(format!("confirmation prompt failed: {error}"));
    write!(output, "{} [y/N] ", visible(question)).map_err(io)?;
    output.flush().map_err(io)?;
    // Read bytes: an answer that is not UTF-8 is not yes, rather than an error.
    let mut answer = Vec::new();
    if input.read_until(b'\n', &mut answer).map_err(io)? == 0 {
        // End of input leaves the cursor after the question.
        writeln!(output).map_err(io)?;
    }
    match String::from_utf8_lossy(&answer)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "y" | "yes" => Ok(()),
        _ => Err(ApiError::ConfirmationDeclined(
            "not confirmed; nothing was changed".into(),
        )),
    }
}

/// `text` with control and bidirectional formatting characters escaped, so a
/// name taken from Jira, such as an attachment's filename, cannot move the
/// cursor, recolor the terminal or reorder the text, and so change what the
/// question appears to say.
fn visible(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() || is_bidi_format(c) {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    shown
}

/// The Unicode bidirectional marks, embeddings, overrides and isolates.
fn is_bidi_format(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Up to five keys, then how many more: `A-1, A-2, A-3, A-4, A-5 and 7 more`.
pub fn sample(keys: &[&str]) -> String {
    const SHOWN: usize = 5;
    let mut text = keys[..keys.len().min(SHOWN)].join(", ");
    if keys.len() > SHOWN {
        text.push_str(&format!(" and {} more", keys.len() - SHOWN));
    }
    text
}

/// `1 issue`, `3 issues`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(text: &str) -> (Result<(), ApiError>, String) {
        let mut output = Vec::new();
        let result = ask("Delete PROJ-1?", &mut text.as_bytes(), &mut output);
        (result, String::from_utf8(output).unwrap())
    }

    #[test]
    fn yes_in_any_case_proceeds() {
        for text in ["y\n", "Y\n", "yes\n", "YES\n", "  yes  \n", "y"] {
            assert!(answer(text).0.is_ok(), "{text:?}");
        }
    }

    #[test]
    fn anything_else_declines() {
        for text in ["\n", "n\n", "no\n", "yess\n", "ja\n", "", "y es\n"] {
            let (result, _) = answer(text);
            assert!(
                matches!(result, Err(ApiError::ConfirmationDeclined(_))),
                "{text:?} must decline, got {result:?}"
            );
        }
    }

    #[test]
    fn the_question_names_the_default() {
        let (_, written) = answer("n\n");
        assert_eq!(written, "Delete PROJ-1? [y/N] ");
    }

    #[test]
    fn control_characters_in_the_question_are_shown_not_obeyed() {
        let mut output = Vec::new();
        let _ = ask(
            "Delete report.pdf\r\x1b[2KDelete nothing?",
            &mut "n\n".as_bytes(),
            &mut output,
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Delete report.pdf\\r\\u{1b}[2KDelete nothing? [y/N] "
        );
    }

    #[test]
    fn bidirectional_overrides_are_shown_not_obeyed() {
        let mut output = Vec::new();
        let _ = ask(
            "Delete invoice\u{202E}fdp.exe?",
            &mut "n\n".as_bytes(),
            &mut output,
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Delete invoice\\u{202e}fdp.exe? [y/N] "
        );
    }

    #[test]
    fn an_answer_that_is_not_utf8_declines() {
        let mut output = Vec::new();
        let result = ask("Delete PROJ-1?", &mut &b"y\xff\n"[..], &mut output);
        assert!(
            matches!(result, Err(ApiError::ConfirmationDeclined(_))),
            "{result:?}"
        );
    }

    #[test]
    fn end_of_input_ends_the_line() {
        let (_, written) = answer("");
        assert_eq!(written, "Delete PROJ-1? [y/N] \n");
    }

    #[test]
    fn consent_given_never_asks() {
        assert_eq!(
            Consent::resolve_for(true, false, "x").unwrap(),
            Consent::Given
        );
        assert_eq!(
            Consent::resolve_for(true, true, "x").unwrap(),
            Consent::Given
        );
        assert!(Consent::Given.confirm("unreachable").is_ok());
    }

    #[test]
    fn a_terminal_asks_and_anything_else_refuses() {
        assert_eq!(
            Consent::resolve_for(false, true, "x").unwrap(),
            Consent::Ask
        );
        let refused = Consent::resolve_for(false, false, "needs --yes").unwrap_err();
        assert!(
            matches!(&refused, ApiError::ConfirmationRequired(m) if m == "needs --yes"),
            "{refused:?}"
        );
    }

    #[test]
    fn samples_cap_at_five_keys() {
        assert_eq!(sample(&["A-1"]), "A-1");
        assert_eq!(
            sample(&["A-1", "A-2", "A-3", "A-4", "A-5"]),
            "A-1, A-2, A-3, A-4, A-5"
        );
        assert_eq!(
            sample(&["A-1", "A-2", "A-3", "A-4", "A-5", "A-6", "A-7"]),
            "A-1, A-2, A-3, A-4, A-5 and 2 more"
        );
        assert_eq!(count(1, "issue"), "1 issue");
        assert_eq!(count(3, "subtask"), "3 subtasks");
    }
}
