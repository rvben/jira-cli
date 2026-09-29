//! Interactive Jira workbench. Network work stays off the input loop so the
//! terminal remains responsive while Jira is slow or unavailable.
mod editor;
mod view;

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio::task::JoinHandle;

use crate::api::{ApiError, Issue, JiraClient, SearchResponse, Transition};
use crate::config::Config;
use editor::Editor;

const PAGE_SIZE: usize = 30;

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> Result<Self, io::Error> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Mine,
    Sprint,
    Recent,
    Search,
}

#[derive(Clone)]
enum Request {
    List {
        jql: String,
        offset: usize,
    },
    Detail(String),
    Transitions(String),
    Transition {
        key: String,
        id: String,
        name: String,
    },
    Comment {
        key: String,
        body: String,
    },
}
enum Response {
    List(SearchResponse),
    Detail(Box<Issue>),
    Transitions(Vec<Transition>),
    Written,
}
struct Job {
    request: Request,
    task: JoinHandle<Result<Response, String>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
enum Overlay {
    #[default]
    None,
    Input {
        kind: InputKind,
        editor: Editor,
        target: Option<String>,
    },
    ReviewComment {
        target: String,
        body: String,
        scroll: u16,
    },
    Transitions {
        key: String,
        items: Vec<Transition>,
        selected: usize,
    },
    Help,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Search,
    Filter,
    Project,
    Comment,
}

struct App {
    profile: String,
    host: String,
    project: Option<String>,
    read_only: bool,
    color: bool,
    scope: Scope,
    query: String,
    filter: String,
    issues: Vec<Issue>,
    selected: usize,
    detail: Option<Issue>,
    show_detail: bool,
    detail_scroll: u16,
    detail_max_scroll: u16,
    is_last: bool,
    next_offset: usize,
    loading: bool,
    writing: bool,
    loading_transitions: bool,
    notice: String,
    error: Option<String>,
    drafts: HashMap<String, String>,
    write_uncertain: Option<String>,
    success: bool,
    reselect_key: Option<String>,
    overlay: Overlay,
}
impl App {
    fn new(cfg: &Config, project: Option<&str>, color: bool) -> Self {
        Self {
            profile: cfg.profile.clone(),
            host: cfg.host.clone(),
            project: project.map(str::to_owned),
            read_only: cfg.read_only,
            color,
            scope: Scope::Mine,
            query: String::new(),
            filter: String::new(),
            issues: Vec::new(),
            selected: 0,
            detail: None,
            show_detail: false,
            detail_scroll: 0,
            detail_max_scroll: 0,
            is_last: true,
            next_offset: 0,
            loading: false,
            writing: false,
            loading_transitions: false,
            notice: "Your assigned issues · press ? for keys".into(),
            error: None,
            drafts: HashMap::new(),
            write_uncertain: None,
            success: false,
            reselect_key: None,
            overlay: Overlay::None,
        }
    }
    fn jql(&self) -> String {
        if self.scope == Scope::Search {
            return self.query.clone();
        }
        let base = match self.scope {
            Scope::Mine => "assignee = currentUser()",
            Scope::Sprint => "sprint in openSprints()",
            Scope::Recent => "",
            Scope::Search => unreachable!(),
        };
        let project = self
            .project
            .as_ref()
            .map(|p| format!("project = \"{}\"", p.replace('"', "\\\"")))
            .unwrap_or_default();
        match (base.is_empty(), project.is_empty()) {
            (false, false) => format!("{base} AND {project} ORDER BY updated DESC"),
            (false, true) => format!("{base} ORDER BY updated DESC"),
            (true, false) => format!("{project} ORDER BY updated DESC"),
            (true, true) => "ORDER BY updated DESC".into(),
        }
    }
    fn list_request(&self, offset: usize) -> Request {
        Request::List {
            jql: self.jql(),
            offset,
        }
    }
    fn selected_issue(&self) -> Option<&Issue> {
        self.issues.get(self.selected)
    }
    fn detail_request(&self) -> Option<Request> {
        self.selected_issue()
            .map(|i| Request::Detail(i.key.clone()))
    }
    fn visible_count(&self) -> usize {
        self.issues.iter().filter(|i| self.matches(i)).count()
    }
    fn matches(&self, issue: &Issue) -> bool {
        let f = self.filter.trim();
        f.is_empty()
            || [
                issue.key.as_str(),
                issue.summary(),
                issue.status(),
                issue.assignee(),
                issue.issue_type(),
            ]
            .iter()
            .any(|s| s.to_lowercase().contains(&f.to_lowercase()))
    }
    fn visible_indices(&self) -> Vec<usize> {
        self.issues
            .iter()
            .enumerate()
            .filter(|(_, i)| self.matches(i))
            .map(|(idx, _)| idx)
            .collect()
    }
    fn move_selection(&mut self, delta: isize) -> Option<Request> {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return None;
        }
        let pos = visible
            .iter()
            .position(|&idx| idx == self.selected)
            .unwrap_or(0);
        let next = (pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        if self.selected == visible[next] {
            return None;
        }
        self.selected = visible[next];
        self.detail = None;
        self.show_detail = false;
        self.detail_scroll = 0;
        self.detail_max_scroll = 0;
        self.detail_request()
    }
    fn set_scope(&mut self, scope: Scope) -> Request {
        self.scope = scope;
        self.selected = 0;
        self.issues.clear();
        self.detail = None;
        self.show_detail = false;
        self.detail_scroll = 0;
        self.detail_max_scroll = 0;
        self.is_last = true;
        self.next_offset = 0;
        self.filter.clear();
        self.error = None;
        self.reselect_key = None;
        self.list_request(0)
    }
    fn complete(&mut self, request: &Request, result: Result<Response, String>) -> Option<Request> {
        self.loading = false;
        self.writing = false;
        self.loading_transitions = false;
        let response = match result {
            Ok(r) => {
                self.error = None;
                r
            }
            Err(e) => {
                self.error = Some(clean(&e));
                if let Request::Comment { key, body } = request {
                    self.drafts.insert(key.clone(), body.clone());
                    self.write_uncertain = Some(key.clone());
                    self.notice = "Comment result unknown · r inspect before another write".into();
                } else if let Request::Transition { key, .. } = request {
                    self.write_uncertain = Some(key.clone());
                    self.notice =
                        "Transition result unknown · r inspect before another write".into();
                } else {
                    self.notice = "Request failed · r retry".into();
                }
                return None;
            }
        };
        match (request, response) {
            (Request::List { offset, .. }, Response::List(page)) => {
                if *offset == 0 {
                    self.issues.clear();
                    self.selected = 0;
                    self.detail = None;
                }
                let added = page.issues.len();
                self.next_offset = *offset + added;
                for issue in page.issues {
                    if !self.issues.iter().any(|old| old.key == issue.key) {
                        self.issues.push(issue);
                    }
                }
                self.is_last = page.is_last || added == 0;
                if *offset == 0
                    && let Some(key) = self.reselect_key.take()
                {
                    if let Some(index) = self.issues.iter().position(|issue| issue.key == key) {
                        self.selected = index;
                    } else {
                        self.notice = format!("{key} updated · no longer in this view");
                    }
                } else if !self.success {
                    self.notice = format!(
                        "{} issues loaded{}",
                        self.issues.len(),
                        if self.is_last { "" } else { " · n more" }
                    );
                }
                if *offset == 0 {
                    return self.detail_request();
                }
            }
            (Request::Detail(key), Response::Detail(issue)) => {
                if self.selected_issue().is_some_and(|i| i.key == *key) {
                    if let Some(row) = self.issues.iter_mut().find(|row| row.key == issue.key) {
                        *row = *issue.clone();
                    }
                    self.detail = Some(*issue);
                    self.detail_scroll = 0;
                    self.detail_max_scroll = 0;
                    if self.write_uncertain.as_deref() == Some(key) {
                        self.write_uncertain = None;
                        self.notice =
                            format!("{key} refreshed · inspect its state before retrying");
                    } else if !self.success {
                        self.notice = format!("{key} is up to date");
                    }
                }
            }
            (Request::Transitions(key), Response::Transitions(items)) => {
                if self.selected_issue().is_some_and(|i| i.key == *key) {
                    if items.is_empty() {
                        self.notice = "No transitions available for this issue".into();
                    } else {
                        self.overlay = Overlay::Transitions {
                            key: key.clone(),
                            items,
                            selected: 0,
                        };
                    }
                }
            }
            (Request::Transition { key, name, .. }, Response::Written) => {
                self.overlay = Overlay::None;
                self.notice = format!("{key} moved via {name}");
                self.success = true;
                self.reselect_key = Some(key.clone());
                return Some(self.list_request(0));
            }
            (Request::Comment { key, .. }, Response::Written) => {
                self.overlay = Overlay::None;
                self.notice = format!("Comment added to {key}");
                self.drafts.remove(key);
                self.success = true;
                return Some(Request::Detail(key.clone()));
            }
            _ => {}
        }
        None
    }
    fn key(&mut self, key: KeyEvent) -> Action {
        // A cancelled HTTP write can still commit on the server. Keep the
        // request alive until its result is known before accepting another key.
        if self.writing {
            return Action::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.loading_transitions {
            return match key.code {
                KeyCode::Esc => {
                    self.loading_transitions = false;
                    self.loading = false;
                    self.notice = "Transition menu cancelled".into();
                    Action::Cancel
                }
                KeyCode::Char('q') => Action::Quit,
                _ => {
                    self.notice = "Loading transitions · Esc cancels".into();
                    Action::None
                }
            };
        }
        if self.write_uncertain.is_some()
            && !matches!(
                key.code,
                KeyCode::Char('r' | 'o' | '?' | 'q') | KeyCode::Esc
            )
        {
            self.notice = "Write result unknown · r refresh this issue before continuing".into();
            return Action::None;
        }
        self.success = false;
        match std::mem::take(&mut self.overlay) {
            Overlay::Help => {
                return Action::None;
            }
            Overlay::Input {
                kind,
                mut editor,
                target,
            } => {
                if key.code == KeyCode::Esc {
                    if kind == InputKind::Comment
                        && !editor.text().trim().is_empty()
                        && let Some(target) = target
                    {
                        self.drafts.insert(target, editor.into_text());
                        self.notice = "Comment draft kept for this session".into();
                    }
                    return Action::None;
                }
                let submit = match kind {
                    InputKind::Comment => {
                        key.code == KeyCode::Char('s')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.code == KeyCode::Enter
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                    }
                    _ => key.code == KeyCode::Enter,
                };
                if submit {
                    let value = editor.text().trim().to_string();
                    match kind {
                        InputKind::Search if !value.is_empty() => {
                            self.query = value;
                            return Action::Fetch(self.set_scope(Scope::Search));
                        }
                        InputKind::Filter => {
                            self.filter = value;
                            if let Some(idx) = self.visible_indices().first() {
                                self.selected = *idx;
                                self.detail = None;
                                return self.detail_request().map_or(Action::None, Action::Fetch);
                            }
                            return Action::None;
                        }
                        InputKind::Project => {
                            if !value.is_empty() && !valid_project_key(&value) {
                                self.error = Some(
                                    "Project key must contain only letters, digits, or _".into(),
                                );
                                self.overlay = Overlay::Input {
                                    kind,
                                    editor,
                                    target,
                                };
                                return Action::None;
                            }
                            self.project = if value.is_empty() {
                                None
                            } else {
                                Some(value.to_ascii_uppercase())
                            };
                            return Action::Fetch(self.set_scope(self.scope));
                        }
                        InputKind::Comment if !value.is_empty() => {
                            let target = target.expect("comment input has a target");
                            self.drafts.insert(target.clone(), value.clone());
                            self.overlay = Overlay::ReviewComment {
                                target,
                                body: value,
                                scroll: 0,
                            };
                            return Action::None;
                        }
                        _ => {}
                    }
                }
                editor.key(key);
                self.error = None;
                self.overlay = Overlay::Input {
                    kind,
                    editor,
                    target,
                };
                return Action::None;
            }
            Overlay::ReviewComment {
                target,
                body,
                mut scroll,
            } => {
                match key.code {
                    KeyCode::Esc => {
                        self.overlay = Overlay::Input {
                            kind: InputKind::Comment,
                            editor: Editor::new(body, true),
                            target: Some(target),
                        };
                    }
                    KeyCode::PageDown | KeyCode::Down => {
                        scroll = scroll.saturating_add(5);
                        self.overlay = Overlay::ReviewComment {
                            target,
                            body,
                            scroll,
                        };
                    }
                    KeyCode::PageUp | KeyCode::Up => {
                        scroll = scroll.saturating_sub(5);
                        self.overlay = Overlay::ReviewComment {
                            target,
                            body,
                            scroll,
                        };
                    }
                    KeyCode::Enter => {
                        return Action::Fetch(Request::Comment { key: target, body });
                    }
                    _ => {
                        self.overlay = Overlay::ReviewComment {
                            target,
                            body,
                            scroll,
                        };
                    }
                }
                return Action::None;
            }
            Overlay::Transitions {
                key: issue_key,
                items,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Esc => return Action::None,
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(items.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some(transition) = items.get(selected) {
                            let request = Request::Transition {
                                key: issue_key,
                                id: transition.id.clone(),
                                name: transition.name.clone(),
                            };
                            return Action::Fetch(request);
                        }
                    }
                    _ => {}
                }
                self.overlay = Overlay::Transitions {
                    key: issue_key,
                    items,
                    selected,
                };
                return Action::None;
            }
            Overlay::None => {}
        }
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                Action::None
            }
            KeyCode::Char('1') => Action::Fetch(self.set_scope(Scope::Mine)),
            KeyCode::Char('2') => Action::Fetch(self.set_scope(Scope::Sprint)),
            KeyCode::Char('3') => Action::Fetch(self.set_scope(Scope::Recent)),
            KeyCode::Char('4') | KeyCode::Char('/') => {
                self.overlay = Overlay::Input {
                    kind: InputKind::Search,
                    editor: Editor::new(self.query.clone(), false),
                    target: None,
                };
                Action::None
            }
            KeyCode::Char('f') => {
                self.overlay = Overlay::Input {
                    kind: InputKind::Filter,
                    editor: Editor::new(self.filter.clone(), false),
                    target: None,
                };
                Action::None
            }
            KeyCode::Char('p') if self.scope != Scope::Search => {
                self.overlay = Overlay::Input {
                    kind: InputKind::Project,
                    editor: Editor::new(self.project.clone().unwrap_or_default(), false),
                    target: None,
                };
                Action::None
            }
            KeyCode::Char('p') => {
                self.notice = "Project scope applies to preset views; edit JQL here".into();
                Action::None
            }
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                Action::None
            }
            KeyCode::Esc if self.show_detail => {
                self.show_detail = false;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1).map_or(Action::None, Action::Fetch)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1).map_or(Action::None, Action::Fetch)
            }
            KeyCode::PageDown => {
                self.detail_scroll = self
                    .detail_scroll
                    .saturating_add(8)
                    .min(self.detail_max_scroll);
                Action::None
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(8);
                Action::None
            }
            KeyCode::End => {
                self.detail_scroll = self.detail_max_scroll;
                Action::None
            }
            KeyCode::Home => {
                self.detail_scroll = 0;
                Action::None
            }
            KeyCode::Char('n') if !self.is_last && !self.loading => {
                Action::Fetch(self.list_request(self.next_offset))
            }
            KeyCode::Char('r') => {
                if let Some(key) = self.write_uncertain.clone() {
                    if let Some(index) = self.issues.iter().position(|issue| issue.key == key) {
                        self.selected = index;
                    }
                    Action::Fetch(Request::Detail(key))
                } else {
                    Action::Fetch(self.list_request(0))
                }
            }
            KeyCode::Enter => {
                self.show_detail = true;
                if self.detail.is_some() {
                    Action::None
                } else {
                    self.detail_request().map_or(Action::None, Action::Fetch)
                }
            }
            KeyCode::Char('t') if self.write_uncertain.is_some() => {
                self.notice = "Refresh the issue before another write".into();
                Action::None
            }
            KeyCode::Char('t') if !self.read_only => self
                .selected_issue()
                .map(|i| Action::Fetch(Request::Transitions(i.key.clone())))
                .unwrap_or(Action::None),
            KeyCode::Char('c') if self.write_uncertain.is_some() => {
                self.notice = "Refresh the issue before another write".into();
                Action::None
            }
            KeyCode::Char('c') if !self.read_only && self.selected_issue().is_some() => {
                let key = self.selected_issue().unwrap().key.clone();
                self.overlay = Overlay::Input {
                    kind: InputKind::Comment,
                    editor: Editor::new(self.drafts.remove(&key).unwrap_or_default(), true),
                    target: Some(key),
                };
                Action::None
            }
            KeyCode::Char('t' | 'c') if self.read_only => {
                self.notice = "This profile is read-only; Jira write actions are disabled".into();
                Action::None
            }
            KeyCode::Char('o') => self
                .selected_issue()
                .map(|i| Action::Open(i.key.clone()))
                .unwrap_or(Action::None),
            _ => Action::None,
        }
    }

    fn paste(&mut self, value: &str) {
        if let Overlay::Input { editor, .. } = &mut self.overlay {
            editor.paste(value);
        }
    }
}
enum Action {
    None,
    Quit,
    Cancel,
    Fetch(Request),
    Open(String),
}

fn clean(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(500)
        .collect()
}

fn valid_project_key(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn start(client: Arc<JiraClient>, request: Request, app: &mut App) -> Job {
    app.loading = true;
    app.loading_transitions = matches!(&request, Request::Transitions(_));
    app.writing = matches!(
        &request,
        Request::Comment { .. } | Request::Transition { .. }
    );
    app.error = None;
    let progress = match &request {
        Request::List { .. } => "Loading issues…",
        Request::Detail(_) => "Loading issue details…",
        Request::Transitions(_) => "Loading transitions…",
        Request::Transition { .. } => "Moving issue…",
        Request::Comment { .. } => "Posting comment…",
    };
    if !app.success || !matches!(request, Request::Detail(_)) && app.reselect_key.is_none() {
        app.notice = progress.into();
    }
    let task_request = request.clone();
    let task = tokio::spawn(async move {
        let result = match task_request {
            Request::List { jql, offset } => client
                .search(&jql, PAGE_SIZE, offset)
                .await
                .map(Response::List),
            Request::Detail(key) => client
                .get_issue(&key)
                .await
                .map(|issue| Response::Detail(Box::new(issue))),
            Request::Transitions(key) => client
                .get_transitions(&key)
                .await
                .map(Response::Transitions),
            Request::Transition { key, id, .. } => client
                .do_transition(&key, &id)
                .await
                .map(|_| Response::Written),
            Request::Comment { key, body } => client
                .add_comment(&key, &body)
                .await
                .map(|_| Response::Written),
        };
        result.map_err(|e| e.to_string())
    });
    Job { request, task }
}

/// Start the terminal workbench. The caller has already resolved the profile.
pub async fn run(
    client: JiraClient,
    cfg: &Config,
    project: Option<&str>,
    color: bool,
) -> Result<(), ApiError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(ApiError::InvalidInput(
            "`jira tui` needs a terminal; use `jira issues mine` for scripts".into(),
        ));
    }
    if let Some(project) = project
        && !valid_project_key(project)
    {
        return Err(ApiError::InvalidInput(
            "project must be a Jira project key".into(),
        ));
    }
    let client = Arc::new(client);
    let color = color
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    let mut app = App::new(cfg, project, color);
    let _guard = TerminalGuard::enter().map_err(|e| ApiError::Other(e.to_string()))?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|e| ApiError::Other(e.to_string()))?;
    let first = app.list_request(0);
    let mut job = Some(start(client.clone(), first, &mut app));
    loop {
        if job.as_ref().is_some_and(|j| j.task.is_finished()) {
            let mut done = job.take().unwrap();
            let result = (&mut done.task)
                .await
                .unwrap_or_else(|e| Err(format!("Request stopped: {e}")));
            if let Some(next) = app.complete(&done.request, result) {
                job = Some(start(client.clone(), next, &mut app));
            }
        }
        terminal
            .draw(|frame| view::draw(frame, &mut app))
            .map_err(|e| ApiError::Other(e.to_string()))?;
        if event::poll(Duration::from_millis(40)).map_err(|e| ApiError::Other(e.to_string()))? {
            let action = match event::read().map_err(|e| ApiError::Other(e.to_string()))? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    app.key(key)
                }
                Event::Paste(value) => {
                    app.paste(&value);
                    Action::None
                }
                _ => Action::None,
            };
            match action {
                Action::Quit => break,
                Action::Cancel => {
                    drop(job.take());
                }
                Action::None => {}
                Action::Open(key) => {
                    let url = client.browse_url(&key);
                    match open::that(url) {
                        Ok(()) => app.notice = format!("Opened {key} in browser"),
                        Err(e) => app.error = Some(format!("Could not open browser: {e}")),
                    }
                }
                Action::Fetch(request) => {
                    drop(job.take());
                    job = Some(start(client.clone(), request, &mut app));
                }
            }
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::AuthType;
    use ratatui::backend::TestBackend;

    fn app(read_only: bool) -> App {
        let cfg = Config {
            profile: "work".into(),
            host: "jira.example.com".into(),
            email: "me@example.com".into(),
            token: "token".into(),
            auth_type: AuthType::Basic,
            api_version: 3,
            read_only,
            credential_store: "file".into(),
            cloud_id: None,
            token_kind: "classic".into(),
        };
        App::new(&cfg, Some("APP"), false)
    }
    fn issue(key: &str, summary: &str) -> Issue {
        serde_json::from_value(serde_json::json!({
            "id": "1", "key": key,
            "fields": { "summary": summary, "status": { "name": "To Do" }, "issuetype": { "name": "Task" } }
        })).unwrap()
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn scopes_and_filter_keep_selection_on_visible_issue() {
        let mut app = app(false);
        assert_eq!(
            app.jql(),
            "assignee = currentUser() AND project = \"APP\" ORDER BY updated DESC"
        );
        app.set_scope(Scope::Recent);
        assert_eq!(app.jql(), "project = \"APP\" ORDER BY updated DESC");
        assert!(
            matches!(app.key(key(KeyCode::Char('2'))), Action::Fetch(Request::List { jql, offset: 0 }) if jql == "sprint in openSprints() AND project = \"APP\" ORDER BY updated DESC")
        );
        app.project = None;
        assert_eq!(app.jql(), "sprint in openSprints() ORDER BY updated DESC");
        app.issues = vec![issue("APP-1", "Alpha"), issue("APP-2", "Beta")];
        app.filter = "beta".into();
        assert_eq!(app.visible_indices(), vec![1]);
        assert!(matches!(app.move_selection(1), Some(Request::Detail(ref key)) if key == "APP-2"));
    }

    #[test]
    fn comments_require_review_and_read_only_blocks_writes() {
        let mut app = app(false);
        app.issues.push(issue("APP-1", "Task"));
        assert!(matches!(app.key(key(KeyCode::Char('c'))), Action::None));
        assert!(matches!(
            app.overlay,
            Overlay::Input {
                kind: InputKind::Comment,
                ..
            }
        ));
        app.key(key(KeyCode::Char('H')));
        app.key(key(KeyCode::Char('i')));
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(app.overlay, Overlay::ReviewComment { ref body, .. } if body == "Hi"));
        assert!(
            matches!(app.key(key(KeyCode::Enter)), Action::Fetch(Request::Comment { body, .. }) if body == "Hi")
        );
        app.writing = true;
        assert!(matches!(app.key(key(KeyCode::Char('r'))), Action::None));
        app.writing = false;
        let mut readonly = self::app(true);
        readonly.issues.push(issue("APP-1", "Task"));
        readonly.key(key(KeyCode::Char('c')));
        assert!(matches!(readonly.overlay, Overlay::None));
        assert!(readonly.notice.contains("read-only"));
        readonly.key(key(KeyCode::Char('t')));
        assert!(matches!(readonly.overlay, Overlay::None));
    }

    #[test]
    fn writes_keep_the_issue_chosen_when_dialog_opened() {
        let mut app = app(false);
        app.issues = vec![issue("APP-1", "First"), issue("APP-2", "Second")];
        app.key(key(KeyCode::Char('c')));
        app.selected = 1; // A list refresh can change selection while editing.
        app.paste("For the first issue");
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            app.key(key(KeyCode::Enter)),
            Action::Fetch(Request::Comment { key, body })
                if key == "APP-1" && body == "For the first issue"
        ));

        app.overlay = Overlay::Transitions {
            key: "APP-1".into(),
            items: vec![Transition {
                id: "11".into(),
                name: "Done".into(),
                to: None,
            }],
            selected: 0,
        };
        assert!(matches!(
            app.key(key(KeyCode::Enter)),
            Action::Fetch(Request::Transition { key, id, .. })
                if key == "APP-1" && id == "11"
        ));
    }

    #[test]
    fn project_scope_and_uncertain_write_recovery() {
        let mut app = app(false);
        app.issues.push(issue("APP-1", "Task"));
        app.key(key(KeyCode::Char('p')));
        assert!(matches!(
            app.overlay,
            Overlay::Input {
                kind: InputKind::Project,
                ..
            }
        ));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.paste("ops");
        assert!(
            matches!(app.key(key(KeyCode::Enter)), Action::Fetch(Request::List { jql, .. }) if jql.contains("project = \"OPS\""))
        );

        app.issues.push(issue("OPS-1", "Task"));
        let request = Request::Comment {
            key: "OPS-1".into(),
            body: "A note".into(),
        };
        app.selected = 0;
        app.complete(&request, Err("connection closed".into()));
        assert_eq!(app.write_uncertain.as_deref(), Some("OPS-1"));
        assert_eq!(app.drafts.get("OPS-1").map(String::as_str), Some("A note"));
        assert!(matches!(app.key(key(KeyCode::Char('c'))), Action::None));
        assert!(
            matches!(app.key(key(KeyCode::Char('r'))), Action::Fetch(Request::Detail(ref key)) if key == "OPS-1")
        );
        app.complete(
            &Request::Detail("OPS-1".into()),
            Ok(Response::Detail(Box::new(issue("OPS-1", "Task")))),
        );
        assert!(app.write_uncertain.is_none());
        app.key(key(KeyCode::Char('c')));
        assert!(
            matches!(app.overlay, Overlay::Input { kind: InputKind::Comment, ref editor, .. } if editor.text() == "A note")
        );
    }

    #[test]
    fn transition_refreshes_membership_and_preserves_result() {
        let mut app = app(false);
        app.issues.push(issue("APP-1", "Task"));
        let next = app.complete(
            &Request::Transition {
                key: "APP-1".into(),
                id: "11".into(),
                name: "Done".into(),
            },
            Ok(Response::Written),
        );
        assert!(matches!(next, Some(Request::List { offset: 0, .. })));
        assert!(app.success);
        let page = SearchResponse {
            issues: vec![],
            total: Some(0),
            start_at: 0,
            max_results: 30,
            is_last: true,
        };
        let jql = app.jql();
        app.complete(&Request::List { jql, offset: 0 }, Ok(Response::List(page)));
        assert_eq!(app.notice, "APP-1 updated · no longer in this view");
        assert!(app.issues.is_empty());
    }

    #[test]
    fn renders_compact_and_wide_terminals() {
        let mut app = app(false);
        app.issues.push(issue("APP-1", "A sample task"));
        for (width, height) in [(42, 12), (80, 24), (120, 35)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| view::draw(frame, &mut app)).unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(screen.contains(if width < 80 {
                "2 Sprint"
            } else {
                "Current sprint"
            }));
        }
        app.show_detail = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| view::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("INSPECTOR"));
        assert!(screen.contains("APP-1"));
    }

    #[test]
    fn renders_comment_editor_and_review() {
        let mut app = app(false);
        app.issues.push(issue("APP-1", "A sample task"));
        app.overlay = Overlay::Input {
            kind: InputKind::Comment,
            editor: Editor::new("First line\nSecond line".into(), true),
            target: Some("APP-1".into()),
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| view::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("First line"));
        assert!(screen.contains("Second line"));
        app.overlay = Overlay::ReviewComment {
            target: "APP-1".into(),
            body: "First line\nSecond line".into(),
            scroll: 0,
        };
        terminal.draw(|frame| view::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Post to APP-1?"));
    }
}
