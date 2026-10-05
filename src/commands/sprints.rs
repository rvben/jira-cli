use crate::api::{ApiError, EstimationSource, Issue, JiraClient, PointFields, Sprint};
use crate::commands::issues::{insert_story_points, render_issue_table, user_to_json};
use crate::output::OutputConfig;
use serde_json::json;
use std::collections::BTreeMap;

/// Project and board filters intersect. Shared sprints appear once, with every
/// matching board, ordered by sprint ID for stable machine-readable output.
pub async fn list(
    client: &JiraClient,
    out: &OutputConfig,
    board: Option<&str>,
    state: Option<&str>,
    project: Option<&str>,
) -> Result<(), ApiError> {
    let boards = match (board.and_then(|b| b.parse::<u64>().ok()), project) {
        (Some(id), None) => vec![client.get_board(id).await?],
        _ => client.list_boards_for_project(project).await?,
    };
    if boards.is_empty()
        && let Some(project) = project
    {
        client.get_project(project).await?;
    }
    let target_boards: Vec<_> = boards
        .iter()
        .filter(|b| match board {
            None => true,
            Some(input) => match input.parse::<u64>() {
                Ok(id) => b.id == id,
                Err(_) => b.name.to_lowercase().contains(&input.to_lowercase()),
            },
        })
        .collect();
    if let Some(board) = board
        && target_boards.is_empty()
    {
        return Err(ApiError::NotFound(format!(
            "No board matching {:?}{}",
            board,
            project
                .map(|p| format!(" in project {p}"))
                .unwrap_or_default()
        )));
    }
    let mut all: BTreeMap<u64, (Sprint, BTreeMap<u64, String>)> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut supported = 0;
    if board.is_some() && target_boards.iter().all(|b| !b.may_support_sprints()) {
        let details = target_boards
            .iter()
            .map(|b| format!("board {} {:?} ({})", b.id, b.name, b.board_type))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ApiError::InvalidInput(format!(
            "{details} does not support sprints; choose a Scrum board"
        )));
    }
    for board_info in target_boards {
        if !board_info.may_support_sprints() {
            warnings.push(format!(
                "Board {} {:?} ({}) does not support sprints and was skipped",
                board_info.id, board_info.name, board_info.board_type
            ));
            continue;
        }
        let Some(sprints) = client
            .list_sprints_for_discovery(board_info.id, state)
            .await?
        else {
            warnings.push(format!(
                "Board {} {:?} ({}) does not support sprints and was skipped",
                board_info.id, board_info.name, board_info.board_type
            ));
            continue;
        };
        supported += 1;
        for sprint in sprints {
            all.entry(sprint.id)
                .or_insert_with(|| (sprint, BTreeMap::new()))
                .1
                .insert(board_info.id, board_info.name.clone());
        }
    }
    if board.is_some() && supported == 0 {
        return Err(ApiError::InvalidInput(format!(
            "{}; choose a Scrum board",
            warnings.join("; ")
        )));
    }
    let results: Vec<_> = all.values().map(|(sprint, boards)| {
        let primary = sprint.origin_board_id.and_then(|id| boards.get_key_value(&id))
            .or_else(|| boards.first_key_value()).expect("sprint has a board");
        json!({
            "id": sprint.id, "name": sprint.name, "state": sprint.state,
            "boardId": primary.0, "boardName": primary.1,
            "boards": boards.iter().map(|(id, name)| json!({"id":id,"name":name})).collect::<Vec<_>>(),
            "startDate": sprint.start_date, "endDate": sprint.end_date, "completeDate": sprint.complete_date,
        })
    }).collect();
    if out.json {
        out.print_result(
            &json!({"total":results.len(),"sprints":results,"warnings":warnings}),
            "",
        );
    } else if all.is_empty() {
        out.print_message("No sprints found.");
    } else {
        for (sprint, boards) in all.values() {
            let context = boards
                .iter()
                .map(|(id, name)| format!("{name} ({id})"))
                .collect::<Vec<_>>()
                .join(", ");
            let dates = match sprint.state.as_str() {
                "active" => format!(
                    "{} → {}",
                    sprint.start_date.as_deref().unwrap_or("?"),
                    sprint.end_date.as_deref().unwrap_or("?")
                ),
                "closed" => format!(
                    "completed {}",
                    sprint.complete_date.as_deref().unwrap_or("?")
                ),
                _ => sprint.end_date.as_deref().unwrap_or("-").to_owned(),
            };
            println!(
                "{:>6}  {:<8}  {:<35}  {}  [{}]",
                sprint.id, sprint.state, sprint.name, dates, context
            );
        }
    }
    if !out.json {
        for warning in warnings {
            out.print_message(&warning);
        }
    }
    Ok(())
}

/// One sprint with its story point and issue totals, as the board's sprint
/// views count them: the board's filter scopes the issues, the board's
/// estimation field supplies the points, and subtasks are left out. Totals
/// reflect each issue's current estimate and status.
pub async fn show(
    client: &JiraClient,
    out: &OutputConfig,
    sprint: &str,
    board: Option<u64>,
    project: Option<&str>,
) -> Result<(), ApiError> {
    let sprint = client.resolve_sprint_scoped(sprint, project, board).await?;
    let board_id = board.or(sprint.origin_board_id).ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "Sprint {} {:?} reports no origin board; pass --board <ID>",
            sprint.id, sprint.name
        ))
    })?;
    let board = client.get_board(board_id).await?;
    let estimation = client.board_estimation(&board).await?;
    let mut warnings = Vec::new();
    let points_fields: &[String] = match &estimation.points {
        PointFields::Read(fields) => {
            if estimation.source != EstimationSource::Board {
                warnings.push(format!(
                    "Board {} {:?} reports no estimation statistic; story points are read from {}",
                    board.id,
                    board.name,
                    fields.join(", ")
                ));
            }
            fields
        }
        PointFields::Unavailable(reason) => {
            warnings.push(format!("{reason}; story point totals are unavailable"));
            &[]
        }
    };
    let issues = client
        .sprint_issues(board.id, sprint.id, points_fields)
        .await?;
    let totals = SprintTotals::count(&issues);
    let points = matches!(estimation.points, PointFields::Read(_));

    if out.json {
        let issue_json: Vec<_> = issues
            .iter()
            .map(|issue| {
                let mut json = json!({
                    "key": issue.key,
                    "url": client.browse_url(&issue.key),
                    "summary": issue.summary(),
                    "status": issue.status(),
                    "statusCategory": issue.fields.status.status_category.as_ref().map(|c| c.key.as_str()),
                    "type": issue.issue_type(),
                    "subtask": issue.fields.issuetype.is_subtask(),
                    "assignee": user_to_json(issue.fields.assignee.as_ref()),
                    "priority": issue.fields.priority.as_ref().map(|p| p.name.as_str()),
                });
                insert_story_points(&mut json, issue);
                json
            })
            .collect();
        let fields = match &estimation.points {
            PointFields::Read(fields) => fields.clone(),
            PointFields::Unavailable(_) => Vec::new(),
        };
        out.print_result(
            &json!({
                "id": sprint.id,
                "name": sprint.name,
                "state": sprint.state,
                "goal": sprint.goal.as_deref().filter(|g| !g.trim().is_empty()),
                "startDate": sprint.start_date,
                "endDate": sprint.end_date,
                "completeDate": sprint.complete_date,
                "board": {"id": board.id, "name": board.name},
                "estimation": {
                    "source": estimation.source.as_str(),
                    "fieldIds": fields,
                    "fieldName": estimation.field_name,
                },
                "totals": totals.to_json(points),
                "issues": issue_json,
                "warnings": warnings,
            }),
            "",
        );
        return Ok(());
    }

    let dates = match (&sprint.start_date, &sprint.end_date) {
        (None, None) => String::new(),
        (start, end) => format!(
            "  {} → {}",
            start.as_deref().unwrap_or("?"),
            end.as_deref().unwrap_or("?")
        ),
    };
    println!(
        "Sprint {} {:?} ({}){dates}",
        sprint.id, sprint.name, sprint.state
    );
    println!("Board:    {} {:?}", board.id, board.name);
    if let Some(goal) = sprint.goal.as_deref().filter(|g| !g.trim().is_empty()) {
        println!("Goal:     {goal}");
    }
    if points {
        let label = estimation
            .field_name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        println!(
            "Points:   {}{label}",
            totals.summary(|bucket| points_json(bucket.points).to_string())
        );
    }
    let mut issues_line = totals.summary(|bucket| bucket.issues.to_string());
    if points {
        issues_line.push_str(&format!(", {} unestimated", totals.unestimated));
    }
    println!("Issues:   {issues_line}");
    if totals.subtasks > 0 {
        println!(
            "Subtasks: {} listed below, not counted in the totals",
            totals.subtasks
        );
    }
    println!();
    render_issue_table(&issues, out);
    for warning in warnings {
        out.print_message(&warning);
    }
    Ok(())
}

/// Issues and story points in one status category.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Bucket {
    issues: usize,
    points: f64,
}

impl Bucket {
    fn add(&mut self, other: Bucket) {
        self.issues += other.issues;
        self.points += other.points;
    }
}

/// A sprint's issues counted by status category, subtasks excluded.
#[derive(Debug, Default, PartialEq)]
struct SprintTotals {
    to_do: Bucket,
    in_progress: Bucket,
    done: Bucket,
    /// Issues whose status Jira returned without a known category.
    uncategorized: Bucket,
    unestimated: usize,
    subtasks: usize,
}

impl SprintTotals {
    /// Subtasks are left out because their estimates roll up into their
    /// parent's in Jira's sprint views. An issue type that does not say
    /// whether it is a subtask is counted.
    fn count(issues: &[Issue]) -> Self {
        let mut totals = Self::default();
        for issue in issues {
            if issue.fields.issuetype.is_subtask() == Some(true) {
                totals.subtasks += 1;
                continue;
            }
            let points = match &issue.story_points {
                Some(Some(points)) => points.as_f64().unwrap_or(0.0),
                Some(None) => {
                    totals.unestimated += 1;
                    0.0
                }
                None => 0.0,
            };
            let category = issue
                .fields
                .status
                .status_category
                .as_ref()
                .map(|c| c.key.as_str());
            let bucket = match category {
                Some("new") => &mut totals.to_do,
                Some("indeterminate") => &mut totals.in_progress,
                Some("done") => &mut totals.done,
                _ => &mut totals.uncategorized,
            };
            bucket.add(Bucket { issues: 1, points });
        }
        totals
    }

    fn total(&self) -> Bucket {
        let mut total = Bucket::default();
        for bucket in [self.to_do, self.in_progress, self.done, self.uncategorized] {
            total.add(bucket);
        }
        total
    }

    fn breakdown(&self, value: impl Fn(Bucket) -> serde_json::Value) -> serde_json::Value {
        json!({
            "total": value(self.total()),
            "toDo": value(self.to_do),
            "inProgress": value(self.in_progress),
            "done": value(self.done),
            "uncategorized": value(self.uncategorized),
        })
    }

    /// `points` false says the board has no story points to total, which
    /// leaves the point figures null rather than zero.
    fn to_json(&self, points: bool) -> serde_json::Value {
        json!({
            "issues": self.breakdown(|b| json!(b.issues)),
            "points": points.then(|| self.breakdown(|b| points_json(b.points))),
            "estimated": points.then(|| self.total().issues - self.unestimated),
            "unestimated": points.then_some(self.unestimated),
            "subtasksExcluded": self.subtasks,
        })
    }

    /// "N total, N to do, N in progress, N done", plus the uncategorized
    /// figure when any issue has no known status category.
    fn summary(&self, value: impl Fn(Bucket) -> String) -> String {
        let mut line = format!(
            "{} total, {} to do, {} in progress, {} done",
            value(self.total()),
            value(self.to_do),
            value(self.in_progress),
            value(self.done)
        );
        if self.uncategorized.issues > 0 {
            line.push_str(&format!(", {} uncategorized", value(self.uncategorized)));
        }
        line
    }
}

/// A point sum as JSON. Floating-point sums of fractional estimates pick up
/// noise in the last digits (0.1 + 0.2), which no estimate carries; rounding
/// to six places drops it. Whole sums print as integers.
fn points_json(points: f64) -> serde_json::Value {
    let points = (points * 1e6).round() / 1e6;
    if points.fract() == 0.0 && points.abs() < 9_007_199_254_740_992.0 {
        json!(points as i64)
    } else {
        json!(points)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(category: Option<&str>, subtask: Option<bool>, points: Option<Option<f64>>) -> Issue {
        let mut status = json!({"name": "Status"});
        if let Some(key) = category {
            status["statusCategory"] = json!({"key": key, "name": key});
        }
        let mut issuetype = json!({"name": "Story"});
        if let Some(subtask) = subtask {
            issuetype["subtask"] = json!(subtask);
        }
        let mut issue: Issue = serde_json::from_value(json!({
            "id": "1", "key": "A-1",
            "fields": {"summary": "s", "status": status, "issuetype": issuetype}
        }))
        .unwrap();
        issue.story_points = points.map(|p| p.map(|p| serde_json::Number::from_f64(p).unwrap()));
        issue
    }

    #[test]
    fn a_status_without_a_known_category_is_counted_as_uncategorized() {
        let totals = SprintTotals::count(&[
            issue(None, Some(false), Some(Some(2.0))),
            issue(Some("undefined"), Some(false), Some(Some(3.0))),
            issue(Some("done"), Some(false), Some(Some(1.0))),
        ]);
        assert_eq!(
            totals.uncategorized,
            Bucket {
                issues: 2,
                points: 5.0
            }
        );
        assert_eq!(
            totals.total(),
            Bucket {
                issues: 3,
                points: 6.0
            }
        );
    }

    #[test]
    fn an_issue_type_that_does_not_say_whether_it_is_a_subtask_is_counted() {
        let totals = SprintTotals::count(&[
            issue(Some("new"), None, Some(Some(1.0))),
            issue(Some("new"), Some(true), Some(Some(4.0))),
        ]);
        assert_eq!(
            totals.to_do,
            Bucket {
                issues: 1,
                points: 1.0
            }
        );
        assert_eq!(totals.subtasks, 1);
    }

    #[test]
    fn unavailable_points_leave_every_point_figure_null() {
        let totals = SprintTotals::count(&[issue(Some("new"), Some(false), None)]);
        let json = totals.to_json(false);
        assert_eq!(json["points"], serde_json::Value::Null);
        assert_eq!(json["estimated"], serde_json::Value::Null);
        assert_eq!(json["unestimated"], serde_json::Value::Null);
        assert_eq!(json["issues"]["toDo"], 1);
    }

    #[test]
    fn point_sums_print_whole_numbers_as_integers_and_drop_float_noise() {
        assert_eq!(points_json(13.0).to_string(), "13");
        assert_eq!(points_json(0.1 + 0.2).to_string(), "0.3");
        assert_eq!(points_json(2.5).to_string(), "2.5");
        assert_eq!(points_json(0.0).to_string(), "0");
    }
}
