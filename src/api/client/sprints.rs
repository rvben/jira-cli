use super::story_points::fill_story_points;
use super::{ApiError, JiraClient, Sprint, validate_issue_key};
use crate::api::types::{
    AgileIssuePage, Board, BoardConfiguration, BoardEstimation, EstimationSource, Issue,
    PointFields, SprintEstimation,
};
use std::collections::{BTreeMap, BTreeSet};

/// The fields a sprint's issue listing needs besides the estimate.
const SPRINT_ISSUE_FIELDS: [&str; 5] = ["summary", "status", "issuetype", "assignee", "priority"];

impl JiraClient {
    /// Resolve a globally unique sprint ID, exact name, substring, or "active".
    pub async fn resolve_sprint(&self, specifier: &str) -> Result<Sprint, ApiError> {
        self.resolve_sprint_scoped(specifier, None, None).await
    }

    /// Names are scoped to a project's sprint-capable boards unless an explicit board
    /// overrides that scope. Numeric sprint IDs are already globally unique;
    /// when a board is supplied, also verify membership on that board.
    pub async fn resolve_sprint_scoped(
        &self,
        specifier: &str,
        project: Option<&str>,
        board: Option<u64>,
    ) -> Result<Sprint, ApiError> {
        let specifier = specifier.trim();
        if specifier.is_empty() || board == Some(0) {
            return Err(ApiError::InvalidInput(
                "Sprint must not be empty and board IDs must be positive".into(),
            ));
        }
        if let Ok(id) = specifier.parse::<u64>() {
            if id == 0 {
                return Err(ApiError::InvalidInput("Sprint IDs must be positive".into()));
            }
            let sprint = self.get_sprint(id).await?;
            if let Some(board_id) = board {
                let sprints = self.list_sprints_for_discovery(board_id, None).await?;
                let Some(sprints) = sprints else {
                    let board = self.get_board(board_id).await?;
                    return Err(ApiError::InvalidInput(format!(
                        "Board {} {:?} ({}) does not support sprints; choose a Scrum board",
                        board.id, board.name, board.board_type
                    )));
                };
                if !sprints.iter().any(|s| s.id == id) {
                    return Err(ApiError::InvalidInput(format!(
                        "Sprint {id} is not on board {board_id}"
                    )));
                }
            }
            return Ok(sprint);
        }

        let board_infos = match board {
            Some(id) => vec![self.get_board(id).await?],
            None => self
                .list_boards_for_project(project)
                .await?
                .into_iter()
                .filter(|b| b.may_support_sprints())
                .collect::<Vec<_>>(),
        };
        if board_infos.is_empty() {
            let scope = project
                .map(|p| format!(" for project {p}"))
                .unwrap_or_default();
            return Err(ApiError::NotFound(format!(
                "No sprint-capable boards found{scope}; use --board <ID> or a numeric --sprint <ID>"
            )));
        }
        let active = specifier.eq_ignore_ascii_case("active");
        let query = specifier.to_lowercase();
        let mut candidates: BTreeMap<u64, (Sprint, BTreeSet<u64>)> = BTreeMap::new();
        for board_info in &board_infos {
            let board_id = board_info.id;
            if !board_info.may_support_sprints() {
                return Err(ApiError::InvalidInput(format!(
                    "Board {board_id} {:?} ({}) does not support sprints; choose a Scrum board",
                    board_info.name, board_info.board_type
                )));
            }
            let state = if active { Some("active") } else { None };
            let sprints = if board.is_none() {
                self.list_sprints_for_discovery(board_id, state)
                    .await?
                    .unwrap_or_default()
            } else {
                self.list_sprints_for_discovery(board_id, state).await?.ok_or_else(|| {
                    ApiError::InvalidInput(format!(
                        "Board {board_id} {:?} ({}) does not support sprints; choose a Scrum board",
                        board_info.name, board_info.board_type
                    ))
                })?
            };
            for sprint in sprints {
                let matches = if active {
                    sprint.state.eq_ignore_ascii_case("active")
                } else {
                    sprint.name.to_lowercase().contains(&query)
                };
                if matches {
                    candidates
                        .entry(sprint.id)
                        .or_insert_with(|| (sprint, BTreeSet::new()))
                        .1
                        .insert(board_id);
                }
            }
        }
        // An exact name wins over substring matches, but duplicate exact names
        // still require an ID. Shared sprints count once, even on multiple boards.
        if !active
            && candidates
                .values()
                .any(|(s, _)| s.name.eq_ignore_ascii_case(specifier))
        {
            candidates.retain(|_, (s, _)| s.name.eq_ignore_ascii_case(specifier));
        }
        if candidates.len() == 1 {
            return Ok(candidates.into_values().next().expect("one candidate").0);
        }
        let scope = match (board, project) {
            (Some(id), _) => format!(" on board {id}"),
            (_, Some(p)) => format!(" for project {p}"),
            _ => String::new(),
        };
        if candidates.is_empty() {
            return Err(ApiError::NotFound(format!(
                "No sprint found matching {specifier:?}{scope}"
            )));
        }
        let choices = candidates
            .values()
            .map(|(s, boards)| {
                format!(
                    "{} ({:?}, boards {})",
                    s.id,
                    s.name,
                    boards
                        .iter()
                        .map(|id| {
                            let info = board_infos.iter().find(|b| b.id == *id);
                            info.map_or_else(
                                || id.to_string(),
                                |b| format!("{} {:?} ({})", b.id, b.name, b.board_type),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Err(ApiError::InvalidInput(format!(
            "Ambiguous sprint {specifier:?}{scope}; candidates: {choices}. Use --sprint <ID> or --board <ID>."
        )))
    }

    /// Resolve the actual project even when the user supplied an old issue key.
    pub async fn issue_project(&self, key: &str) -> Result<String, ApiError> {
        validate_issue_key(key)?;
        let issue: serde_json::Value = self.get(&format!("issue/{key}?fields=project")).await?;
        issue["fields"]["project"]["key"].as_str()
            .or_else(|| issue["fields"]["project"]["id"].as_str())
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Other("Jira did not return the issue's project; supply --board <ID> or a numeric --sprint <ID>".into()))
    }

    /// The field a board's sprints are estimated with.
    ///
    /// The board's configuration names the field Jira's own sprint views sum,
    /// so it settles which field counts even on a site with several. Only when
    /// the board reports no estimation statistic does the profile pin or
    /// field discovery stand in.
    pub async fn board_estimation(&self, board: &Board) -> Result<SprintEstimation, ApiError> {
        let config: BoardConfiguration = self
            .agile_get(&format!("board/{}/configuration", board.id))
            .await?;
        let Some(BoardEstimation { kind, field }) = config.estimation else {
            let fields = self.story_point_fields().await?;
            let source = if self.story_points_pinned() {
                EstimationSource::Profile
            } else {
                EstimationSource::FieldCatalog
            };
            let points = if fields.is_empty() {
                PointFields::Unavailable(format!(
                    "Board {} {:?} reports no estimation statistic and the site has no story points field",
                    board.id, board.name
                ))
            } else {
                PointFields::Read(fields)
            };
            return Ok(SprintEstimation {
                source,
                field_name: None,
                points,
            });
        };
        let field_name = field.as_ref().and_then(|f| f.display_name.clone());
        let points = match field {
            // Time tracking and the other system fields a board can estimate
            // with are not story points; only a custom field carries them.
            Some(field) if kind == "field" && field.field_id.starts_with("customfield_") => {
                PointFields::Read(vec![field.field_id])
            }
            Some(field) => PointFields::Unavailable(format!(
                "Board {} {:?} estimates with {}, not story points",
                board.id,
                board.name,
                field.display_name.as_deref().unwrap_or(&field.field_id)
            )),
            None if kind.eq_ignore_ascii_case("none") => PointFields::Unavailable(format!(
                "Board {} {:?} estimates by issue count, not story points",
                board.id, board.name
            )),
            None => PointFields::Unavailable(format!(
                "Board {} {:?} estimates with {kind:?}, not a story points field",
                board.id, board.name
            )),
        };
        Ok(SprintEstimation {
            source: EstimationSource::Board,
            field_name,
            points,
        })
    }

    /// Every issue in a sprint as the board shows it: the board's filter
    /// scopes the listing, as it does in Jira's sprint views. Story points
    /// are read from `points_fields`.
    pub async fn sprint_issues(
        &self,
        board_id: u64,
        sprint_id: u64,
        points_fields: &[String],
    ) -> Result<Vec<Issue>, ApiError> {
        let fields = SPRINT_ISSUE_FIELDS
            .iter()
            .map(|f| (*f).to_owned())
            .chain(points_fields.iter().cloned())
            .collect::<Vec<_>>()
            .join(",");
        const PAGE: usize = 100;
        let mut issues = Vec::new();
        loop {
            let path = format!(
                "board/{board_id}/sprint/{sprint_id}/issue?startAt={}&maxResults={PAGE}&fields={fields}",
                issues.len()
            );
            let page: AgileIssuePage = self.agile_get(&path).await?;
            let received = page.issues.len();
            issues.extend(page.issues);
            if received == 0 || page.total.is_some_and(|total| issues.len() >= total) {
                break;
            }
        }
        fill_story_points(&mut issues, points_fields)?;
        Ok(issues)
    }
}
