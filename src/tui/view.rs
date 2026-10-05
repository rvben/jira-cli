use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{App, InputKind, Overlay, Scope, clean};

struct Theme {
    ink: Color,
    muted: Color,
    line: Color,
    accent: Color,
    focus: Color,
    surface: Color,
}
impl Theme {
    fn new(color: bool) -> Self {
        if color {
            Self {
                ink: Color::Rgb(225, 235, 239),
                muted: Color::Rgb(143, 165, 174),
                line: Color::Rgb(47, 69, 77),
                accent: Color::Rgb(106, 224, 199),
                focus: Color::Rgb(26, 61, 64),
                surface: Color::Rgb(15, 32, 39),
            }
        } else {
            Self {
                ink: Color::White,
                muted: Color::Gray,
                line: Color::DarkGray,
                accent: Color::White,
                focus: Color::DarkGray,
                surface: Color::Black,
            }
        }
    }
    fn text(&self) -> Style {
        Style::default().fg(self.ink).bg(self.surface)
    }
    fn muted(&self) -> Style {
        self.text().fg(self.muted)
    }
    fn accent(&self) -> Style {
        self.text().fg(self.accent).add_modifier(Modifier::BOLD)
    }
    fn status(&self, status: &str) -> Style {
        let normalized = status.to_ascii_lowercase();
        if normalized.contains("done")
            || normalized.contains("closed")
            || normalized.contains("resolved")
        {
            self.text().fg(if self.accent == Color::White {
                Color::White
            } else {
                Color::Rgb(132, 215, 161)
            })
        } else if normalized.contains("progress") || normalized.contains("review") {
            self.text().fg(if self.accent == Color::White {
                Color::White
            } else {
                Color::Rgb(131, 189, 250)
            })
        } else if normalized.contains("block") {
            self.text().fg(if self.accent == Color::White {
                Color::White
            } else {
                Color::Rgb(244, 190, 113)
            })
        } else {
            self.muted()
        }
    }
    fn panel(&self) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.line))
            .style(self.text())
    }
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let t = Theme::new(app.color);
    let area = frame.area();
    frame.render_widget(Block::default().style(t.text()), area);
    if area.width < 42 || area.height < 12 {
        frame.render_widget(
            Paragraph::new(
                "JIRA  /  WORKBENCH\n\nEnlarge the terminal to at least 42 × 12.\n\nq  quit",
            )
            .style(t.text()),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    header(frame, rows[0], app, &t);
    tabs(frame, rows[1], app, &t);
    if area.width >= 100 {
        let columns = Layout::horizontal([Constraint::Percentage(43), Constraint::Percentage(57)])
            .split(rows[2]);
        list(frame, columns[0], app, &t);
        detail(frame, columns[1], app, &t);
    } else if area.width >= 68 && area.height >= 22 && !app.show_detail {
        let panes = Layout::vertical([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(rows[2]);
        list(frame, panes[0], app, &t);
        detail(frame, panes[1], app, &t);
    } else if app.show_detail {
        detail(frame, rows[2], app, &t);
    } else {
        list(frame, rows[2], app, &t);
    }
    footer(frame, rows[3], app, &t);
    overlay(frame, app, &t);
}

fn header(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let site = clean(&app.host);
    let profile = clean(&app.profile);
    let context = if app.scope == Scope::Search {
        "Custom JQL".to_string()
    } else {
        app.project
            .as_deref()
            .map(|p| format!("Project {p}"))
            .unwrap_or_else(|| "All projects".into())
    };
    let brand = if area.width < 70 {
        "  JIRA"
    } else {
        "  JIRA  /  WORKBENCH"
    };
    let location = if area.width < 65 {
        format!("  {profile}  /  {context}")
    } else {
        format!("  {site}  /  {profile}  /  {context}")
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(brand, t.accent()),
                Span::styled(if app.read_only { "   READ ONLY" } else { "" }, t.muted()),
            ]),
            Line::styled(fit(&location, area.width as usize), t.muted()),
        ])
        .style(t.text()),
        area,
    );
}
fn tabs(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let choices = if area.width < 80 {
        [
            (Scope::Mine, "1 Mine"),
            (Scope::Sprint, "2 Sprint"),
            (Scope::Recent, "3 Recent"),
            (Scope::Search, "4 JQL"),
        ]
    } else {
        [
            (Scope::Mine, "1  My work"),
            (Scope::Sprint, "2  Current sprint"),
            (Scope::Recent, "3  Recent"),
            (Scope::Search, "4  Search"),
        ]
    };
    let mut spans = vec![Span::raw("  ")];
    for (scope, label) in choices {
        let style = if app.scope == scope {
            t.accent().bg(t.focus)
        } else {
            t.muted()
        };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw(if area.width < 80 { "" } else { "  " }));
    }
    let context = match app.scope {
        Scope::Mine => "Assigned to you · newest updates first".to_string(),
        Scope::Sprint => {
            if let Some(project) = &app.project {
                format!("Active sprints in {project} · all assignees")
            } else {
                "Active sprints across your accessible projects".into()
            }
        }
        Scope::Recent => "Recently updated · all assignees".into(),
        Scope::Search => format!("JQL  ·  {}", clean(&app.query)),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(spans),
            Line::styled(
                format!("  {}", fit(&context, area.width.saturating_sub(2) as usize)),
                t.muted(),
            ),
        ])
        .style(t.text()),
        area,
    );
}
fn list(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let title = format!(
        " ISSUES  ·  {} / {} loaded{}{} ",
        app.visible_count(),
        app.issues.len(),
        if app.is_last {
            ""
        } else {
            " · more available"
        },
        if app.error.is_some() {
            " · stale"
        } else if app.loading {
            " · updating"
        } else {
            ""
        },
    );
    let block = t.panel().title(Span::styled(title, t.accent()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if app.issues.is_empty() {
        let copy = if app.loading {
            "Loading issues from Jira…"
        } else if app.error.is_some() {
            "Could not load issues. Press r to retry."
        } else if app.scope == Scope::Search {
            "No issues matched this JQL. Press / to edit the query."
        } else if app.scope == Scope::Sprint {
            "No issues in an active sprint here. Try another project or JQL search (/)."
        } else {
            "No issues here. Try Recent (3) or search Jira (/)."
        };
        frame.render_widget(
            Paragraph::new(format!("\n  {copy}"))
                .style(t.muted())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let visible = app.visible_indices();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new("\n  No rows match this filter. Press f to change it, or Esc to clear.")
                .style(t.muted())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let items: Vec<ListItem> = visible
        .iter()
        .map(|&idx| {
            let issue = &app.issues[idx];
            let key = clean(&issue.key);
            let status = clean(issue.status());
            let summary = clean(issue.summary());
            let mut head = vec![
                Span::styled(format!(" {key:<14}"), t.accent()),
                Span::styled(status.clone(), t.status(&status)),
            ];
            if let Some(Some(points)) = &issue.story_points {
                head.push(Span::styled(format!("  {points} pts"), t.muted()));
            }
            ListItem::new(vec![
                Line::from(head),
                Line::styled(format!("  {summary}"), t.text()),
            ])
        })
        .collect();
    let mut state = ListState::default();
    state.select(visible.iter().position(|&idx| idx == app.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(t.text().bg(t.focus))
            .highlight_symbol("▌"),
        inner,
        &mut state,
    );
}
fn detail(frame: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let block = t.panel().title(Span::styled(" INSPECTOR ", t.accent()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(issue) = app.detail.as_ref().or_else(|| app.selected_issue()) else {
        app.detail_max_scroll = 0;
        frame.render_widget(
            Paragraph::new("\n  Select an issue to inspect it.").style(t.muted()),
            inner,
        );
        return;
    };
    if !app.matches(issue) {
        app.detail_max_scroll = 0;
        frame.render_widget(
            Paragraph::new("\n  No issues match this filter.").style(t.muted()),
            inner,
        );
        return;
    }
    let mut lines = vec![
        Line::from(Span::styled(format!("  {}", clean(&issue.key)), t.accent())),
        Line::from(Span::styled(
            format!("  {}", clean(issue.summary())),
            t.text().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    field(&mut lines, "STATUS", issue.status(), t);
    field(&mut lines, "TYPE", issue.issue_type(), t);
    field(&mut lines, "PRIORITY", issue.priority(), t);
    field(&mut lines, "ASSIGNEE", issue.assignee(), t);
    if let Some(points) = &issue.story_points {
        let points = points
            .as_ref()
            .map_or_else(|| "-".to_owned(), ToString::to_string);
        field(&mut lines, "POINTS", &points, t);
    }
    if let Some(reporter) = &issue.fields.reporter {
        field(&mut lines, "REPORTER", &reporter.display_name, t);
    }
    if let Some(parent) = &issue.fields.parent {
        field(&mut lines, "PARENT", &parent.key, t);
    }
    if let Some(labels) = &issue.fields.labels
        && !labels.is_empty()
    {
        field(&mut lines, "LABELS", &labels.join(", "), t);
    }
    if let Some(components) = &issue.fields.components
        && !components.is_empty()
    {
        field(
            &mut lines,
            "COMPONENTS",
            &components
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            t,
        );
    }
    if let Some(versions) = &issue.fields.fix_versions
        && !versions.is_empty()
    {
        field(
            &mut lines,
            "FIX VERSION",
            &versions
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            t,
        );
    }
    if let Some(updated) = &issue.fields.updated {
        field(
            &mut lines,
            "UPDATED",
            &updated
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " "),
            t,
        );
    }
    if app.detail.as_ref().is_some() {
        let description = clean_multiline(&issue.description_text());
        lines.push(Line::raw(""));
        lines.push(Line::styled("  DESCRIPTION", t.accent()));
        if description.trim().is_empty() {
            lines.push(Line::styled("  No description", t.muted()));
        } else {
            for line in description.lines() {
                lines.push(Line::styled(format!("  {line}"), t.text()));
            }
        }
        if let Some(comments) = &issue.fields.comment
            && !comments.comments.is_empty()
        {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("  COMMENTS  ·  {}", comments.total),
                t.accent(),
            ));
            for comment in comments.comments.iter().rev().take(5) {
                lines.push(Line::styled(
                    format!(
                        "  {}  ·  {}",
                        clean(&comment.author.display_name),
                        comment.created.chars().take(10).collect::<String>()
                    ),
                    t.muted(),
                ));
                for line in clean_multiline(&comment.body_text()).lines() {
                    lines.push(Line::styled(format!("  {line}"), t.text()));
                }
                lines.push(Line::raw(""));
            }
        }
    } else {
        lines.push(Line::styled("  Loading full details…", t.muted()));
    }
    let paragraph = Paragraph::new(lines)
        .style(t.text())
        .wrap(Wrap { trim: false });
    let content_rows = paragraph.line_count(inner.width);
    app.detail_max_scroll = content_rows
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    app.detail_scroll = app.detail_scroll.min(app.detail_max_scroll);
    frame.render_widget(paragraph.scroll((app.detail_scroll, 0)), inner);
}
fn field(lines: &mut Vec<Line<'static>>, label: &str, value: &str, t: &Theme) {
    lines.push(Line::from(vec![
        Span::styled(format!("  {label:<11}"), t.muted()),
        Span::styled(clean(value), t.text()),
    ]));
}
fn footer(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let message = if let Some(error) = &app.error {
        format!("{} · {error}", app.notice)
    } else {
        app.notice.clone()
    };
    let filter = if app.filter.is_empty() {
        String::new()
    } else {
        format!("  ·  filter: {}", clean(&app.filter))
    };
    let keys = if area.width < 65 {
        if app.show_detail {
            "Esc list  PgUp/PgDn scroll  ? keys"
        } else {
            "j/k move  Enter open  ? keys  q quit"
        }
    } else if area.width < 100 {
        if app.show_detail {
            if app.read_only {
                "Esc list  PgUp/PgDn scroll  o browser  ? keys  q quit"
            } else {
                "Esc list  PgUp/PgDn scroll  t status  c comment  ? keys  q quit"
            }
        } else {
            "j/k move  Enter detail  p project  / search  n more  ? keys  q quit"
        }
    } else if app.read_only {
        "j/k move  Enter detail  p project  / JQL  f filter  n more  r refresh  o browser  ? help  q quit"
    } else {
        "j/k move  Enter detail  p project  / JQL  f filter  n more  r refresh  t status  c comment  o browser  ? help  q quit"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!(
                    "  {}",
                    fit(
                        &format!("{message}{filter}"),
                        area.width.saturating_sub(2) as usize
                    )
                ),
                if app.error.is_some() {
                    t.status("blocked")
                } else if app.success {
                    t.status("done")
                } else {
                    t.muted()
                },
            ),
            Line::styled(
                format!("  {}", fit(keys, area.width.saturating_sub(2) as usize)),
                t.muted(),
            ),
        ])
        .style(t.text()),
        area,
    );
}
fn overlay(frame: &mut Frame, app: &App, t: &Theme) {
    if matches!(app.overlay, Overlay::None) {
        return;
    }
    frame.render_widget(Clear, frame.area());
    frame.render_widget(Block::default().style(t.text()), frame.area());
    match &app.overlay {
        Overlay::None => {}
        Overlay::Input {
            kind,
            editor,
            target,
        } => input_overlay(frame, app, t, *kind, editor, target.as_deref()),
        Overlay::Help => {
            let compact = frame.area().width < 70 || frame.area().height < 18;
            let lines = if compact {
                vec![
                    "1 Mine  2 Sprint  3 Recent  4 Search",
                    "j/k move  Enter detail  Esc list",
                    "/ JQL  f filter  p project",
                    "n more  r refresh  o browser",
                    "PgUp/PgDn scroll  q quit",
                    if app.read_only {
                        "Read-only profile"
                    } else {
                        "t status  c comment"
                    },
                ]
            } else {
                vec![
                    "VIEWS",
                    "1 My work    2 Active sprints    3 Recent    4 Search",
                    "",
                    "BROWSE",
                    "j/k or arrows  move     Enter  inspect     Esc  list",
                    "/  JQL search     f  filter loaded issues     p  project scope",
                    "n  next page     r  refresh     o  open in browser",
                    "PgUp/PgDn  scroll details     q  quit",
                    "",
                    if app.read_only {
                        "READ ONLY  ·  write actions unavailable"
                    } else {
                        "WRITE  ·  t transition  ·  c comment  ·  Ctrl+S review"
                    },
                ]
            };
            popup_text(
                frame,
                t,
                "KEYBOARD",
                lines.iter().map(|line| (*line).to_owned()).collect(),
                if compact { 9 } else { 14 },
            );
        }
        Overlay::ReviewComment {
            target,
            body,
            scroll,
        } => {
            let area = centered(
                frame.area(),
                82,
                frame.area().height.saturating_sub(4).min(20),
            );
            frame.render_widget(Clear, area);
            let block = t
                .panel()
                .title(Span::styled(" REVIEW COMMENT ", t.accent()));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            if inner.height < 3 {
                return;
            }
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "  Post to {target}?\n  Review the text before sending."
                ))
                .style(t.muted()),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(clean_multiline(&format!(
                    "  {}",
                    body.replace('\n', "\n  ")
                )))
                .style(t.text())
                .scroll((*scroll, 0))
                .wrap(Wrap { trim: false }),
                rows[1],
            );
            frame.render_widget(
                Paragraph::new("  Enter post  ·  Esc edit  ·  PgUp/PgDn scroll").style(t.accent()),
                rows[2],
            );
        }
        Overlay::Transitions {
            key,
            items,
            selected,
        } => {
            let area = centered(frame.area(), 72, (items.len() as u16 + 6).min(19));
            frame.render_widget(Clear, area);
            let block = t
                .panel()
                .title(Span::styled(" TRANSITION ISSUE ", t.accent()));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            if inner.height < 3 {
                return;
            }
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(format!("  {key}  ·  choose a workflow action")).style(t.muted()),
                rows[0],
            );
            let visible = rows[1].height as usize;
            let start = selected
                .saturating_sub(visible.saturating_sub(1) / 2)
                .min(items.len().saturating_sub(visible));
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(index, item)| {
                    let destination = item.to.as_ref().map(|to| to.name.as_str()).unwrap_or("");
                    Line::styled(
                        format!(
                            "  {}  {}  →  {}",
                            if index == *selected { "▌" } else { " " },
                            clean(&item.name),
                            clean(destination)
                        ),
                        if index == *selected {
                            t.accent().bg(t.focus)
                        } else {
                            t.text()
                        },
                    )
                })
                .collect();
            frame.render_widget(Paragraph::new(lines).style(t.text()), rows[1]);
            frame.render_widget(
                Paragraph::new("  Enter apply  ·  Esc cancel  ·  ↑/↓ choose").style(t.muted()),
                rows[2],
            );
        }
    }
}

fn input_overlay(
    frame: &mut Frame,
    app: &App,
    t: &Theme,
    kind: InputKind,
    editor: &super::editor::Editor,
    target: Option<&str>,
) {
    let height = if kind == InputKind::Comment {
        frame.area().height.saturating_sub(4).min(18)
    } else {
        7
    };
    let area = centered(frame.area(), 82, height);
    frame.render_widget(Clear, area);
    let title = match kind {
        InputKind::Search => "SEARCH  /  JQL",
        InputKind::Filter => "FILTER LOADED ISSUES",
        InputKind::Project => "PROJECT SCOPE",
        InputKind::Comment => "WRITE COMMENT",
    };
    let block = t
        .panel()
        .title(Span::styled(format!(" {title} "), t.accent()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let prompt = if kind == InputKind::Comment {
        format!(
            "  Comment on {} · Enter adds a line",
            target.unwrap_or("issue")
        )
    } else {
        match kind {
            InputKind::Search => "  Jira Query Language",
            InputKind::Filter => "  Match key, summary, status, assignee, or type",
            InputKind::Project => "  Project key · leave blank for all projects",
            InputKind::Comment => unreachable!(),
        }
        .to_string()
    };
    frame.render_widget(Paragraph::new(prompt).style(t.muted()), rows[0]);
    let display = format!("  {}", editor.display().replace('\n', "\n  "));
    let mut paragraph = Paragraph::new(clean_multiline(&display))
        .style(t.text())
        .wrap(Wrap { trim: false });
    if editor.is_multiline() {
        paragraph = paragraph.scroll((
            editor
                .cursor_line()
                .saturating_sub(rows[1].height.saturating_sub(2) as usize) as u16,
            0,
        ));
    } else {
        paragraph = paragraph.scroll((
            0,
            editor
                .cursor_column()
                .saturating_sub(rows[1].width.saturating_sub(7) as usize) as u16,
        ));
    }
    frame.render_widget(paragraph, rows[1]);
    let hint = if kind == InputKind::Comment {
        "  Ctrl+S review  ·  Esc keep draft  ·  paste supported"
    } else {
        "  Enter apply  ·  Esc cancel  ·  Ctrl+U clear"
    };
    frame.render_widget(Paragraph::new(hint).style(t.accent()), rows[2]);
    if let Some(error) = &app.error {
        frame.render_widget(
            Paragraph::new(format!(
                "  {}",
                fit(error, area.width.saturating_sub(4) as usize)
            ))
            .style(t.status("blocked")),
            rows[0],
        );
    }
}

fn popup_text(frame: &mut Frame, t: &Theme, title: &str, lines: Vec<String>, height: u16) {
    let area = centered(frame.area(), 78, height);
    frame.render_widget(Clear, area);
    let block = t
        .panel()
        .title(Span::styled(format!(" {title} "), t.accent()));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .map(|line| Line::styled(format!("  {line}"), t.text()))
                .collect::<Vec<_>>(),
        )
        .block(block),
        area,
    );
}

fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut shortened: String = text.chars().take(width - 1).collect();
    shortened.push('…');
    shortened
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn clean_multiline(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' {
                c
            } else if c.is_control() {
                ' '
            } else {
                c
            }
        })
        .take(20_000)
        .collect()
}
