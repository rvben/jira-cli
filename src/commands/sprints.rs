use crate::api::{ApiError, JiraClient, Sprint};
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
