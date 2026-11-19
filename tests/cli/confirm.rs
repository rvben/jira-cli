//! The confirmation gate: commands whose writes cannot be undone, or whose
//! reach is a query, ask on a terminal and refuse without one.
//!
//! The terminal tests run the binary on a pseudo-terminal, the way a person
//! runs it: stdin and stderr on the terminal, stdout piped so the result stays
//! machine-readable.

use super::usability::{search_issues, transitions};
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const QUESTION_END: &str = "[y/N] ";
const DEADLINE: Duration = Duration::from_secs(30);

/// A `jira` process whose stdin, and optionally stderr, is a terminal.
struct Session {
    child: Child,
    terminal: std::fs::File,
    screen: Arc<Mutex<Vec<u8>>>,
    reader: std::thread::JoinHandle<()>,
    stdout: std::thread::JoinHandle<Vec<u8>>,
}

struct Finished {
    code: Option<i32>,
    stdout: Vec<u8>,
    screen: String,
}

impl Session {
    /// Run against the mock server at `uri` with stdin and stderr on the terminal.
    fn at(uri: &str, args: &[&str]) -> Self {
        Self::start_with(&TempDir::new().unwrap(), Some(uri), args, true)
    }

    fn start_with(
        dir: &TempDir,
        host: Option<&str>,
        args: &[&str],
        stderr_on_terminal: bool,
    ) -> Self {
        let (mut controller, mut device) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut controller,
                &mut device,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty succeeded, so both descriptors are open and owned here.
        let (terminal, device) = unsafe {
            (
                std::fs::File::from_raw_fd(controller),
                OwnedFd::from_raw_fd(device),
            )
        };

        let mut cmd = jira_cmd(dir);
        cmd.args(args)
            .stdin(Stdio::from(device.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(if stderr_on_terminal {
                Stdio::from(device.try_clone().unwrap())
            } else {
                Stdio::null()
            });
        if let Some(host) = host {
            cmd.env("JIRA_HOST", host)
                .env("JIRA_EMAIL", "test@example.com")
                .env("JIRA_TOKEN", "test-token");
        }
        let mut child = cmd.spawn().unwrap();
        // Only the child may hold the terminal, so reading it ends when the child exits.
        drop(cmd);
        drop(device);

        let screen = Arc::new(Mutex::new(Vec::new()));
        let mut reader = terminal.try_clone().unwrap();
        let sink = Arc::clone(&screen);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            while let Ok(n @ 1..) = reader.read(&mut chunk) {
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        let mut pipe = child.stdout.take().unwrap();
        let stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        });
        Session {
            child,
            terminal,
            screen,
            reader,
            stdout,
        }
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned()
    }

    /// The question once it is on the terminal, without its `[y/N]` ending.
    fn question(&mut self) -> String {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if let Some(end) = screen.find(QUESTION_END) {
                return screen[..end].trim_end().to_owned();
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("exited ({status}) without asking:\n{screen}");
            }
            assert!(
                start.elapsed() < DEADLINE,
                "no question within {DEADLINE:?}:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Type `keys` at the terminal, as a person would.
    fn answer(&mut self, keys: &[u8]) {
        self.terminal.write_all(keys).unwrap();
    }

    fn finish(mut self) -> Finished {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > DEADLINE {
                self.child.kill().ok();
                panic!("still running after {DEADLINE:?}:\n{}", self.screen());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // The terminal reads end once the child, its only other holder, is
        // gone, so everything it wrote is on screen after this.
        let Session {
            reader,
            screen,
            stdout,
            ..
        } = self;
        reader.join().unwrap();
        Finished {
            code: status.code(),
            stdout: stdout.join().unwrap(),
            screen: String::from_utf8_lossy(&screen.lock().unwrap()).into_owned(),
        }
    }
}

/// Run `session` off the async runtime so the mock server keeps answering.
async fn on_terminal<T: Send + 'static>(session: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(session).await.unwrap()
}

async fn writes(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() != "GET" && !r.url.path().ends_with("/search/jql"))
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect()
}

async fn transition_to_done(server: &MockServer, keys: &[&str]) {
    for key in keys {
        transitions(
            server,
            key,
            json!([{"id":"31","name":"Done","to":{"name":"Done"}}]),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(format!("/rest/api/3/issue/{key}/transitions")))
            .respond_with(ResponseTemplate::new(204))
            .mount(server)
            .await;
    }
}

const BULK_DONE: &[&str] = &[
    "issues",
    "bulk-transition",
    "--jql",
    "project = PROJ",
    "--to",
    "Done",
];

/// The declined run exits non-zero with a kind of its own, sends nothing, and
/// leaves stdout empty for whoever reads it.
fn assert_declined(finished: &Finished) {
    assert_eq!(
        finished.code,
        Some(exit_codes::INPUT_ERROR),
        "{}",
        finished.screen
    );
    assert!(
        finished.screen.contains("confirmation_declined"),
        "{}",
        finished.screen
    );
    assert!(finished.stdout.is_empty(), "{:?}", finished.stdout);
}

#[tokio::test]
async fn bulk_transition_asks_once_and_writes_what_it_asked_about() {
    let server = MockServer::start().await;
    search_issues(&server, 2).await;
    transition_to_done(&server, &["PROJ-1", "PROJ-2"]).await;
    let uri = server.uri();
    let (question, finished) = on_terminal(move || {
        let mut session = Session::at(&uri, BULK_DONE);
        let question = session.question();
        session.answer(b"y\n");
        (question, session.finish())
    })
    .await;
    assert_eq!(
        question,
        r#"Transition 2 issues (PROJ-1, PROJ-2) to "Done"?"#
    );
    assert_eq!(finished.code, Some(0), "{}", finished.screen);
    let data: Value = serde_json::from_slice(&finished.stdout).unwrap();
    assert_eq!(
        data["succeeded"], 2,
        "stdout stays the plain result: {data}"
    );
    assert_eq!(
        writes(&server).await,
        [
            "POST /rest/api/3/issue/PROJ-1/transitions",
            "POST /rest/api/3/issue/PROJ-2/transitions"
        ]
    );
}

/// Everything the transition would write is in the question, not only the
/// target status.
#[tokio::test]
async fn bulk_transition_names_the_fields_and_comment_it_writes() {
    let server = MockServer::start().await;
    search_issues(&server, 1).await;
    let uri = server.uri();
    let question = on_terminal(move || {
        let mut args = BULK_DONE.to_vec();
        args.extend(["--resolution", "Fixed", "--comment", "Released"]);
        let mut session = Session::at(&uri, &args);
        let question = session.question();
        session.answer(b"n\n");
        assert_declined(&session.finish());
        question
    })
    .await;
    assert_eq!(
        question,
        r#"Transition 1 issue (PROJ-1) to "Done", setting resolution=Fixed, and comment on each?"#
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn any_answer_but_yes_sends_nothing() {
    for keys in [&b"n\n"[..], b"\n", b"yess\n", b"\x04"] {
        let server = MockServer::start().await;
        search_issues(&server, 2).await;
        transition_to_done(&server, &["PROJ-1", "PROJ-2"]).await;
        let uri = server.uri();
        let finished = on_terminal(move || {
            let mut session = Session::at(&uri, BULK_DONE);
            session.question();
            session.answer(keys);
            session.finish()
        })
        .await;
        assert_declined(&finished);
        assert!(writes(&server).await.is_empty(), "{keys:?}");
    }
}

/// A selection that grows between the question and the answer must not grow
/// the write: the issues written are the issues the person agreed to.
#[tokio::test]
async fn the_selection_is_fetched_once_before_asking() {
    let server = MockServer::start().await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(move |_: &wiremock::Request| {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let keys: &[&str] = if n == 0 {
                &["PROJ-1", "PROJ-2"]
            } else {
                &["PROJ-1", "PROJ-2", "PROJ-3"]
            };
            let issues: Vec<Value> = keys
                .iter()
                .map(|key| {
                    let mut issue = full_issue();
                    issue["key"] = json!(key);
                    issue["id"] = json!(key.replace("PROJ-", "1000"));
                    issue
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"issues": issues, "isLast": true}))
        })
        .mount(&server)
        .await;
    transition_to_done(&server, &["PROJ-1", "PROJ-2", "PROJ-3"]).await;
    let uri = server.uri();
    let finished = on_terminal(move || {
        let mut session = Session::at(&uri, BULK_DONE);
        session.question();
        session.answer(b"y\n");
        session.finish()
    })
    .await;
    assert_eq!(finished.code, Some(0), "{}", finished.screen);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(writes(&server).await.len(), 2);
}

#[tokio::test]
async fn an_empty_selection_completes_without_asking() {
    let server = MockServer::start().await;
    search_issues(&server, 0).await;
    let uri = server.uri();
    let finished = on_terminal(move || Session::at(&uri, BULK_DONE).finish()).await;
    assert_eq!(finished.code, Some(0), "{}", finished.screen);
    assert!(
        !finished.screen.contains(QUESTION_END),
        "{}",
        finished.screen
    );
}

#[tokio::test]
async fn bulk_assign_names_the_resolved_account() {
    let server = MockServer::start().await;
    search_issues(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accountId": "acc-1", "displayName": "Me"
        })))
        .mount(&server)
        .await;
    let uri = server.uri();
    let question = on_terminal(move || {
        let mut session = Session::at(
            &uri,
            &[
                "issues",
                "bulk-assign",
                "--jql",
                "project = PROJ",
                "--assignee",
                "me",
            ],
        );
        let question = session.question();
        session.answer(b"n\n");
        assert_declined(&session.finish());
        question
    })
    .await;
    assert_eq!(
        question,
        r#"Assign 1 issue (PROJ-1) to "me" (account acc-1)?"#
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn issues_delete_asks_after_checking_and_names_the_subtasks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "PROJ-1",
            "fields": {"summary": "Parent", "issuetype": {"name": "Task"},
                "subtasks": [{"key": "PROJ-5"}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/mypermissions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permissions": {"DELETE_ISSUES": {"havePermission": true}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    let (question, finished) = on_terminal(move || {
        let mut session = Session::at(&uri, &["issues", "delete", "PROJ-1", "--delete-subtasks"]);
        let question = session.question();
        session.answer(b"yes\n");
        (question, session.finish())
    })
    .await;
    assert_eq!(
        question,
        "Permanently delete 1 issue (PROJ-1) and every subtask under it, now 1 subtask (PROJ-5)? Deleted issues cannot be restored."
    );
    assert_eq!(finished.code, Some(0), "{}", finished.screen);
    assert_eq!(writes(&server).await, ["DELETE /rest/api/3/issue/PROJ-1"]);
}

#[tokio::test]
async fn delete_attachment_names_the_file_before_deleting() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attachment_json(
            "10001",
            "report.pdf",
            2048,
        )))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    let question = on_terminal(move || {
        let mut session = Session::at(&uri, &["issues", "delete-attachment", "10001"]);
        let question = session.question();
        session.answer(b"n\n");
        assert_declined(&session.finish());
        question
    })
    .await;
    assert_eq!(
        question,
        "Permanently delete attachment 10001: report.pdf (2.0 KB, by Alice)? It cannot be restored."
    );
    assert!(writes(&server).await.is_empty());
}

fn two_profiles(dir: &TempDir) {
    write_config(
        dir.path(),
        "[default]\nhost = \"a.example.com\"\ntoken = \"t\"\ncredential_store = \"file\"\n\
         [profiles.work]\nhost = \"b.example.com\"\ntoken = \"t\"\ncredential_store = \"file\"\n",
    )
    .unwrap();
}

fn config_text(dir: &TempDir) -> String {
    std::fs::read_to_string(dir.path().join("jira").join("config.toml")).unwrap()
}

#[test]
fn profile_removal_asks_on_a_terminal() {
    for command in ["profile", "config"] {
        let dir = TempDir::new().unwrap();
        two_profiles(&dir);
        let mut session = Session::start_with(&dir, None, &[command, "remove", "work"], true);
        assert_eq!(session.question(), "Remove profile 'work'?");
        session.answer(b"n\n");
        assert_declined(&session.finish());
        assert!(config_text(&dir).contains("[profiles.work]"), "{command}");

        let mut session = Session::start_with(&dir, None, &[command, "remove", "work"], true);
        session.question();
        session.answer(b"y\n");
        let finished = session.finish();
        assert_eq!(finished.code, Some(0), "{}", finished.screen);
        assert!(!config_text(&dir).contains("[profiles.work]"), "{command}");
    }
}

/// A config edited while the question is open is not overwritten with the
/// copy read before it.
#[test]
fn profile_removal_keeps_changes_made_while_asking() {
    let dir = TempDir::new().unwrap();
    two_profiles(&dir);
    let mut session = Session::start_with(&dir, None, &["profile", "remove", "work"], true);
    session.question();
    let edited = config_text(&dir) + "\n[profiles.added]\nhost = \"c.example.com\"\n";
    std::fs::write(dir.path().join("jira").join("config.toml"), &edited).unwrap();
    session.answer(b"y\n");
    let finished = session.finish();
    assert_eq!(
        finished.code,
        Some(exit_codes::CONFLICT),
        "{}",
        finished.screen
    );
    assert_eq!(config_text(&dir), edited);
}

/// Every gated command refuses before touching anything when nobody can be
/// asked: stdin piped, as under `output()`.
#[tokio::test]
async fn without_a_terminal_every_gated_command_refuses_before_any_request() {
    let server = MockServer::start().await;
    for args in [
        &["issues", "delete-attachment", "10001"][..],
        &["issues", "delete", "PROJ-1"],
        BULK_DONE,
        &[
            "issues",
            "bulk-assign",
            "--jql",
            "project = PROJ",
            "--assignee",
            "none",
        ],
    ] {
        let output = run_jira_against(&server, args);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{args:?}"
        );
        let envelope = error_envelope(&String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            envelope["error"]["kind"], "confirmation_required",
            "{args:?}"
        );
        assert!(
            envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--yes"),
            "{envelope}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());

    for command in ["profile", "config"] {
        let dir = TempDir::new().unwrap();
        two_profiles(&dir);
        let output = jira_cmd(&dir)
            .args([command, "remove", "work"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{command}"
        );
        let envelope = error_envelope(&String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            envelope["error"]["kind"], "confirmation_required",
            "{command}"
        );
        assert!(config_text(&dir).contains("[profiles.work]"), "{command}");
    }
}

/// A question nobody can see is not consent: with stdin on a terminal but
/// stderr redirected, the command refuses instead of waiting on a hidden prompt.
#[tokio::test]
async fn a_hidden_question_is_never_asked() {
    let server = MockServer::start().await;
    search_issues(&server, 1).await;
    let uri = server.uri();
    let finished = on_terminal(move || {
        Session::start_with(&TempDir::new().unwrap(), Some(&uri), BULK_DONE, false).finish()
    })
    .await;
    assert_eq!(finished.code, Some(exit_codes::INPUT_ERROR));
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// `--yes` answers in advance, so a terminal session runs without a question.
#[tokio::test]
async fn yes_skips_the_question_on_a_terminal() {
    let server = MockServer::start().await;
    search_issues(&server, 1).await;
    transition_to_done(&server, &["PROJ-1"]).await;
    let uri = server.uri();
    let finished = on_terminal(move || {
        let mut args = BULK_DONE.to_vec();
        args.push("--yes");
        Session::at(&uri, &args).finish()
    })
    .await;
    assert_eq!(finished.code, Some(0), "{}", finished.screen);
    assert!(!finished.screen.contains(QUESTION_END));
    assert_eq!(writes(&server).await.len(), 1);
}
