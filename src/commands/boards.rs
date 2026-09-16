use crate::api::{ApiError, JiraClient};
use crate::output::OutputConfig;

/// List all Jira Agile boards.
pub async fn list(
    client: &JiraClient,
    out: &OutputConfig,
    project: Option<&str>,
) -> Result<(), ApiError> {
    let boards = client.list_boards_for_project(project).await?;
    if boards.is_empty()
        && let Some(project) = project
    {
        client.get_project(project).await?;
    }

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "total": boards.len(),
                "boards": boards.iter().map(|b| serde_json::json!({
                    "id": b.id,
                    "name": b.name,
                    "type": b.board_type,
                })).collect::<Vec<_>>(),
            }))
            .expect("failed to serialize JSON"),
        );
        return Ok(());
    }

    if boards.is_empty() {
        out.print_message("No boards found.");
        return Ok(());
    }

    for b in &boards {
        println!("{:>6}  {:<30}  {}", b.id, b.name, b.board_type);
    }
    Ok(())
}
