use super::{ApiError, JiraClient, Sprint, validate_issue_key};
use std::collections::{BTreeMap, BTreeSet};

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
            if let Some(board_id) = board
                && !self
                    .list_sprints(board_id, None)
                    .await?
                    .iter()
                    .any(|s| s.id == id)
            {
                return Err(ApiError::InvalidInput(format!(
                    "Sprint {id} is not on board {board_id}"
                )));
            }
            return Ok(sprint);
        }

        let board_ids = match board {
            Some(id) => vec![id],
            None => self
                .list_boards_for_project(project)
                .await?
                .into_iter()
                .filter(|b| b.may_support_sprints())
                .map(|b| b.id)
                .collect::<Vec<_>>(),
        };
        if board_ids.is_empty() {
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
        for board_id in board_ids {
            let state = if active { Some("active") } else { None };
            let sprints = if board.is_none() {
                self.list_sprints_for_discovery(board_id, state)
                    .await?
                    .unwrap_or_default()
            } else {
                self.list_sprints(board_id, state).await?
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
                        .map(u64::to_string)
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
}
