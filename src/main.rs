#![recursion_limit = "256"]

use jira_cli::api::{ApiError, IssueDraft, IssueUpdate, JiraClient};
use jira_cli::commands;
use jira_cli::config::Config;
use jira_cli::output::{OutputConfig, exit_codes, machine_readable_errors};

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::io::IsTerminal;

/// Parse a comma-separated `--fields` argument into a list of field names.
fn parse_fields_arg(s: &str) -> Vec<String> {
    s.split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

fn parse_field(s: &str) -> Result<(String, serde_json::Value), String> {
    let (key, raw) = s
        .split_once('=')
        .ok_or_else(|| format!("field must be in key=value format, got: {s}"))?;
    // Try to parse as JSON (handles numbers, booleans, objects, arrays).
    // Fall back to a plain string.
    let value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()));
    Ok((key.to_string(), value))
}

/// Parse a repeated CLI string-array arg with the project's three-state sentinel:
/// - empty input → `None` (leave field untouched)
/// - single `"none"` value → `Some(Vec::new())` (clear field)
/// - otherwise → `Some(refs)` (replace field with these values)
///
/// To set a literal value of `"none"`, bypass the sentinel via `--field <name>=[..]`.
fn parse_vec_update_arg(values: &[String]) -> Option<Vec<&str>> {
    match values {
        [] => None,
        [v] if v == "none" => Some(Vec::new()),
        _ => Some(values.iter().map(String::as_str).collect()),
    }
}

/// Convert a `Vec<String>` of CLI-repeated values into an `Option<Vec<&str>>`.
/// `None` if empty, `Some(refs)` otherwise. The caller then `as_deref()`s into
/// `Option<&[&str]>` for the API layer.
fn vec_to_opt_refs(values: &[String]) -> Option<Vec<&str>> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().map(String::as_str).collect())
    }
}

#[derive(Parser)]
#[command(
    name = "jira",
    version,
    about = "Fast, friendly Jira from your terminal, built for humans and agents",
    after_help = "Get started:\n  jira init              Configure and verify an account\n  jira doctor            Check the complete connection\n  jira issues mine       See what needs your attention\n  jira schema --command 'issues list'\n                         Inspect one command for automation",
    arg_required_else_help = true
)]
struct Cli {
    /// Atlassian domain (e.g. mycompany.atlassian.net) [env: JIRA_HOST]
    #[arg(long, env = "JIRA_HOST")]
    host: Option<String>,

    /// Account email [env: JIRA_EMAIL]
    #[arg(long, env = "JIRA_EMAIL")]
    email: Option<String>,

    /// Config profile to use [env: JIRA_PROFILE]
    #[arg(long, env = "JIRA_PROFILE")]
    profile: Option<String>,

    /// Output format: auto (default), text, or json
    #[arg(long = "output", short = 'o', global = true, default_value = "auto")]
    output: String,

    /// Output as JSON (alias for --output=json)
    #[arg(long, global = true, hide = true)]
    json: bool,

    /// Suppress non-data output (counts, confirmations)
    #[arg(long, global = true)]
    quiet: bool,

    /// Disable ANSI color even on a terminal
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage issues
    #[command(subcommand, visible_alias = "issue")]
    Issues(Box<IssuesCommand>),

    /// List projects
    #[command(subcommand, visible_alias = "project", arg_required_else_help = true)]
    Projects(ProjectsCommand),

    /// Search issues with JQL
    Search {
        /// JQL query string
        jql: String,

        /// Maximum number of results
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Skip the first N results (for pagination)
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Fetch all pages (overrides --limit and --offset)
        #[arg(long)]
        all: bool,

        /// Comma-separated list of fields to include in output (JSON mode only)
        #[arg(long)]
        fields: Option<String>,
    },

    /// Search for users by name or email
    #[command(subcommand, visible_alias = "user", arg_required_else_help = true)]
    Users(UsersCommand),

    /// List boards
    #[command(subcommand, visible_alias = "board", arg_required_else_help = true)]
    Boards(BoardsCommand),

    /// List sprints
    #[command(subcommand, visible_alias = "sprint", arg_required_else_help = true)]
    Sprints(SprintsCommand),

    /// Show the currently authenticated user
    Myself,

    /// Verify configuration, authentication, project access, and write safety
    Doctor,

    /// Manage configuration
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Bootstrap config and API token setup (alias for `config init`)
    Init,

    /// List available fields (system and custom)
    #[command(subcommand, visible_alias = "field", arg_required_else_help = true)]
    Fields(FieldsCommand),

    /// Dump commands and arguments as JSON for agent introspection
    Schema {
        /// Return only one command, such as "issues list"
        #[arg(long)]
        command: Option<String>,
    },

    /// Describe offline-safe CLI capabilities
    Capabilities,

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
        /// Install completions for supported shells (bash, zsh, fish)
        #[arg(long)]
        install: bool,
    },
}

#[derive(Subcommand)]
enum IssuesCommand {
    /// List issues
    List {
        /// Filter by project key
        #[arg(short, long)]
        project: Option<String>,

        /// Filter by status (e.g. "In Progress", "Done")
        #[arg(short, long)]
        status: Option<String>,

        /// Filter by assignee (use "me" for current user)
        #[arg(short, long)]
        assignee: Option<String>,

        /// Filter by issue type (e.g. Bug, Story, Task)
        #[arg(short = 't', long = "type")]
        issue_type: Option<String>,

        /// Filter by sprint name or use "active" for open sprints
        #[arg(long)]
        sprint: Option<String>,

        /// Filter by component (can be specified multiple times)
        #[arg(long)]
        components: Vec<String>,

        /// Filter by label (can be specified multiple times)
        #[arg(long)]
        labels: Vec<String>,

        /// Filter by fix version (can be specified multiple times)
        #[arg(long)]
        fix_versions: Vec<String>,

        /// Additional JQL to append
        #[arg(long)]
        jql: Option<String>,

        /// Maximum number of results
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Skip the first N results (for pagination)
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Fetch all pages (overrides --limit and --offset)
        #[arg(long)]
        all: bool,

        /// Comma-separated list of fields to include in output (JSON mode only)
        #[arg(long)]
        fields: Option<String>,
    },

    /// List issues assigned to you
    Mine {
        /// Filter by project key
        #[arg(short, long)]
        project: Option<String>,

        /// Filter by status (e.g. "In Progress", "Done")
        #[arg(short, long)]
        status: Option<String>,

        /// Filter by issue type (e.g. Bug, Story, Task)
        #[arg(short = 't', long)]
        issue_type: Option<String>,

        /// Filter by sprint name or use "active" for open sprints
        #[arg(long)]
        sprint: Option<String>,

        /// Maximum number of results
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Fetch all pages (overrides --limit)
        #[arg(long)]
        all: bool,

        /// Comma-separated list of fields to include in output (JSON mode only)
        #[arg(long)]
        fields: Option<String>,
    },

    /// List comments on an issue
    Comments {
        /// Issue key (e.g. PROJ-123)
        key: String,
    },

    /// Show a single issue in detail
    Show {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Open the issue in your default browser
        #[arg(long)]
        open: bool,
    },

    /// Create a new issue
    Create {
        /// Project key
        #[arg(short, long)]
        project: String,

        /// Issue type (e.g. Bug, Story, Task)
        #[arg(short = 't', long = "type", default_value = "Task")]
        issue_type: String,

        /// Issue summary
        #[arg(short, long)]
        summary: String,

        /// Issue description (plain text; newlines become separate paragraphs)
        #[arg(short, long)]
        description: Option<String>,

        /// Priority (e.g. High, Medium, Low)
        #[arg(long)]
        priority: Option<String>,

        /// Labels to apply (can be specified multiple times)
        #[arg(long)]
        labels: Vec<String>,

        /// Components to attach (can be specified multiple times)
        #[arg(long)]
        components: Vec<String>,

        /// Fix versions to set (can be specified multiple times)
        #[arg(long)]
        fix_versions: Vec<String>,

        /// Assign to this account ID (use "me" for yourself)
        #[arg(long)]
        assignee: Option<String>,

        /// Add to a sprint (sprint ID, name substring, or "active")
        #[arg(long)]
        sprint: Option<String>,

        /// Parent issue key (creates a subtask or child issue)
        #[arg(long)]
        parent: Option<String>,

        /// Custom field values as key=value pairs (e.g. --field customfield_10016=5)
        #[arg(long, value_parser = parse_field)]
        field: Vec<(String, serde_json::Value)>,
    },

    /// Update fields on an existing issue
    Update {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// New summary text
        #[arg(long)]
        summary: Option<String>,

        /// New description (plain text)
        #[arg(long)]
        description: Option<String>,

        /// New priority (e.g. High, Medium, Low)
        #[arg(long)]
        priority: Option<String>,

        /// Components to set (replaces existing; use "none" alone to clear)
        #[arg(long)]
        components: Vec<String>,

        /// Fix versions to set (replaces existing; use "none" alone to clear)
        #[arg(long)]
        fix_versions: Vec<String>,

        /// Labels to set (replaces existing; use "none" alone to clear)
        #[arg(long)]
        labels: Vec<String>,

        /// Assign to this account ID (use "me" for yourself, "none" to unassign)
        #[arg(long)]
        assignee: Option<String>,

        /// Custom field values as key=value pairs (e.g. --field customfield_10016=5)
        #[arg(long, value_parser = parse_field)]
        field: Vec<(String, serde_json::Value)>,
    },

    /// Move an issue to a sprint
    Move {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Sprint ID, sprint name substring, or "active"
        #[arg(long)]
        sprint: String,
    },

    /// Add a comment to an issue
    Comment {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Comment body (plain text)
        #[arg(short, long)]
        body: String,
    },

    /// Move an issue to a new status
    Transition {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Target status name or transition ID
        #[arg(long)]
        to: String,
    },

    /// List available transitions for an issue
    ListTransitions {
        /// Issue key (e.g. PROJ-123)
        key: String,
    },

    /// Assign an issue to a user
    Assign {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Account ID, "me" for yourself, or "none" to unassign
        #[arg(long)]
        assignee: String,
    },

    /// List available issue link types
    LinkTypes,

    /// Link this issue to another issue
    Link {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Key of the issue to link to
        #[arg(long)]
        to: String,

        /// Link type name (e.g. "Blocks", "Duplicate", "Relates")
        #[arg(long, default_value = "Relates")]
        link_type: String,
    },

    /// Remove a link between issues by link ID
    Unlink {
        /// Link ID (shown in `issues show` output and JSON)
        link_id: String,
    },

    /// Log work (time) on an issue
    LogWork {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// Time spent (e.g. 2h, 30m, 1d 4h)
        #[arg(short, long)]
        time: String,

        /// Comment describing the work done
        #[arg(short, long)]
        comment: Option<String>,

        /// When the work was started (ISO-8601, e.g. 2024-01-15T09:00:00.000+0000)
        #[arg(long)]
        started: Option<String>,
    },

    /// List attachments on an issue
    Attachments {
        /// Issue key (e.g. PROJ-123)
        key: String,
    },

    /// Attach one or more local files to an issue
    Attach {
        /// Issue key (e.g. PROJ-123)
        key: String,

        /// File to upload (can be specified multiple times)
        #[arg(short, long, required = true)]
        file: Vec<std::path::PathBuf>,
    },

    /// Download an attachment by ID
    DownloadAttachment {
        /// Attachment ID (shown in `issues attachments` output)
        id: String,

        /// Directory to write the file into (created if missing)
        #[arg(long, default_value = ".")]
        dir: std::path::PathBuf,

        /// Overwrite the target file if it already exists
        #[arg(long)]
        force: bool,
    },

    /// Delete an attachment by ID
    DeleteAttachment {
        /// Attachment ID (shown in `issues attachments` output)
        id: String,
    },

    /// Transition all issues matching a JQL query to a new status
    BulkTransition {
        /// JQL query selecting the issues to transition
        #[arg(long)]
        jql: String,

        /// Target status name or transition ID
        #[arg(long)]
        to: String,

        /// Preview without making any changes
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt (required when stdin is not a terminal)
        #[arg(long)]
        yes: bool,
    },

    /// Assign all issues matching a JQL query to a user
    BulkAssign {
        /// JQL query selecting the issues to assign
        #[arg(long)]
        jql: String,

        /// Account ID, "me" for yourself, or "none" to unassign
        #[arg(long)]
        assignee: String,

        /// Preview without making any changes
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt (required when stdin is not a terminal)
        #[arg(long)]
        yes: bool,
    },

    /// Catch bare issue keys: `jira issue PROJ-123` → `jira issues show PROJ-123`
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand)]
enum ProjectsCommand {
    /// List accessible projects
    List,
    /// Show details for a single project
    Show {
        /// Project key (e.g. PROJ)
        key: String,
    },
    /// List components for a project
    Components {
        /// Project key (e.g. PROJ)
        key: String,
    },
    /// List versions for a project
    Versions {
        /// Project key (e.g. PROJ)
        key: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show current config (token masked)
    Show,
    /// Print example config file and token instructions
    Init,
    /// Remove a profile from the config file
    Remove {
        /// Profile name to remove (use "default" for the default profile)
        profile: String,
    },
}

#[derive(Subcommand)]
enum UsersCommand {
    /// Search for users by name or email
    Search {
        /// Name, username, or email fragment to search for
        query: String,
    },
}

#[derive(Subcommand)]
enum BoardsCommand {
    /// List all boards
    List,
}

#[derive(Subcommand)]
enum SprintsCommand {
    /// List sprints, optionally filtered by board and/or state
    List {
        /// Board name or ID (lists all boards if omitted)
        #[arg(long)]
        board: Option<String>,

        /// Filter by state: active (default), closed, future, or all
        #[arg(long, default_value = "active")]
        state: String,
    },
}

#[derive(Subcommand)]
enum FieldsCommand {
    /// List all fields with their IDs and types
    List {
        /// Show only custom fields
        #[arg(long)]
        custom: bool,
    },
}

/// Restore the default disposition of `SIGPIPE`, which Rust replaces with
/// `SIG_IGN` before `main` runs. Under `SIG_IGN` a closed downstream turns every
/// write into `EPIPE`, and `println!` panics on that: `jira issues list | head`
/// exits 101 with a backtrace, which is neither a declared exit code nor what a
/// shell pipeline expects. With the default restored the process is terminated
/// by the signal, the way every other tool in a pipeline is.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // SAFETY: this is a plain FFI call with no invariants for the caller to
    // uphold. `SIG_DFL` is the disposition the process is given at exec time.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

#[tokio::main]
async fn main() {
    restore_default_sigpipe();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Clap handles usage errors (unrecognized subcommands, bad flags, etc.)
            // before any Rust code runs, so the output mode has to be recovered
            // from the raw arguments.
            let msg = e.to_string();
            if e.exit_code() == exit_codes::SUCCESS {
                // `--help` and `--version` surface as clap errors that exit 0.
                // They are successful output, not failures, so they carry no
                // error envelope: an agent treating any envelope as a failure
                // would otherwise read a successful `--help` as a broken one.
                e.print().unwrap_or_else(|_| println!("{msg}"));
            } else if machine_readable_errors(
                std::env::args().skip(1),
                std::io::stdout().is_terminal(),
            ) {
                // The envelope carries clap's full message, including its
                // "did you mean" hint, so emitting it alone loses nothing and
                // leaves stderr parseable in one piece.
                jira_cli::output::print_error_envelope(jira_cli::output::INVALID_INPUT.kind, &msg);
            } else {
                e.print().unwrap_or_else(|_| eprintln!("{msg}"));
            }
            std::process::exit(e.exit_code());
        }
    };
    jira_cli::output::set_no_color(cli.no_color);
    let text_mode = cli.output == "text";
    let json_mode = cli.json || cli.output == "json";
    let out = OutputConfig::new(json_mode, text_mode, cli.quiet);

    let result = run(cli, out).await;

    if let Err(ref e) = result {
        // One rendering per mode, so stderr is either wholly parseable as the
        // error envelope or wholly prose. Emitting both leaves `2>&1 | jq`
        // choking on the prose line.
        let contract = jira_cli::output::contract_for_dyn(e.as_ref());
        let message = e.to_string();
        if out.json {
            jira_cli::output::print_error_envelope(contract.kind, &message);
        } else {
            eprintln!("Error: {message}");
        }
        std::process::exit(contract.exit_code);
    }
}

async fn run(cli: Cli, out: OutputConfig) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Schema { command } => {
            print_schema(command.as_deref())?;
            return Ok(());
        }

        Command::Capabilities => {
            let capabilities = serde_json::json!({
                "name": "jira",
                "version": env!("CARGO_PKG_VERSION"),
                "clispec": "0.3",
                "output": ["text", "json"],
                "features": ["doctor", "command-scoped schema", "pagination", "field selection", "read-only guard"]
            });
            if out.json {
                println!("{}", serde_json::to_string_pretty(&capabilities)?);
            } else {
                println!(
                    "jira {} - clispec 0.3; text/json output, pagination, field selection, read-only guard",
                    env!("CARGO_PKG_VERSION")
                );
            }
            return Ok(());
        }

        Command::Completions { shell, install } => {
            handle_completions(shell, install, &out)?;
            return Ok(());
        }

        Command::Init => {
            jira_cli::config::init(&out, cli.host.as_deref()).await?;
            return Ok(());
        }

        Command::Config(cmd) => {
            match cmd {
                ConfigCommand::Show => {
                    jira_cli::config::show(&out, cli.host, cli.email, cli.profile)?;
                }
                ConfigCommand::Init => {
                    jira_cli::config::init(&out, cli.host.as_deref()).await?;
                }
                ConfigCommand::Remove { profile } => {
                    jira_cli::config::remove_profile(&out, &profile)?;
                }
            }
            return Ok(());
        }

        _ => {}
    }

    let cfg = Config::load(cli.host, cli.email, cli.profile)?;

    if cfg.read_only {
        let is_write = matches!(
            &cli.command,
            Command::Issues(cmd) if matches!(
                cmd.as_ref(),
                IssuesCommand::Create { .. }
                    | IssuesCommand::Update { .. }
                    | IssuesCommand::Move { .. }
                    | IssuesCommand::Comment { .. }
                    | IssuesCommand::Transition { .. }
                    | IssuesCommand::Assign { .. }
                    | IssuesCommand::Link { .. }
                    | IssuesCommand::Unlink { .. }
                    | IssuesCommand::LogWork { .. }
                    | IssuesCommand::Attach { .. }
                    | IssuesCommand::DeleteAttachment { .. }
                    | IssuesCommand::BulkTransition { .. }
                    | IssuesCommand::BulkAssign { .. }
            )
        );
        if is_write {
            return Err(ApiError::InvalidInput(
                "read-only mode is enabled (unset JIRA_READ_ONLY or remove read_only from config to allow writes)".into(),
            )
            .into());
        }
    }

    let client = JiraClient::new(
        &cfg.host,
        &cfg.email,
        &cfg.token,
        cfg.auth_type.clone(),
        cfg.api_version,
    )?;

    match cli.command {
        Command::Issues(cmd) => match *cmd {
            IssuesCommand::List {
                project,
                status,
                assignee,
                issue_type,
                sprint,
                components,
                labels,
                fix_versions,
                jql,
                limit,
                offset,
                all,
                fields,
            } => {
                let parsed_components = vec_to_opt_refs(&components);
                let parsed_labels = vec_to_opt_refs(&labels);
                let parsed_fix_versions = vec_to_opt_refs(&fix_versions);
                let filters = commands::issues::ListFilters {
                    project: project.as_deref(),
                    status: status.as_deref(),
                    assignee: assignee.as_deref(),
                    issue_type: issue_type.as_deref(),
                    sprint: sprint.as_deref(),
                    components: parsed_components.as_deref(),
                    labels: parsed_labels.as_deref(),
                    fix_versions: parsed_fix_versions.as_deref(),
                    jql_extra: jql.as_deref(),
                };
                let field_filter = fields.as_deref().map(parse_fields_arg);
                commands::issues::list(
                    &client,
                    &out,
                    filters,
                    limit,
                    offset,
                    all,
                    field_filter.as_deref(),
                )
                .await?
            }
            IssuesCommand::Mine {
                project,
                status,
                issue_type,
                sprint,
                limit,
                all,
                fields,
            } => {
                let filters = commands::issues::ListFilters {
                    project: project.as_deref(),
                    status: status.as_deref(),
                    issue_type: issue_type.as_deref(),
                    sprint: sprint.as_deref(),
                    ..Default::default()
                };
                let field_filter = fields.as_deref().map(parse_fields_arg);
                commands::issues::mine(&client, &out, filters, limit, all, field_filter.as_deref())
                    .await?
            }
            IssuesCommand::Comments { key } => {
                commands::issues::comments(&client, &out, &key).await?
            }
            IssuesCommand::Show { key, open } => {
                commands::issues::show(&client, &out, &key, open).await?
            }
            IssuesCommand::Create {
                project,
                issue_type,
                summary,
                description,
                priority,
                labels,
                components,
                fix_versions,
                assignee,
                sprint,
                parent,
                field,
            } => {
                let parsed_labels = vec_to_opt_refs(&labels);
                let parsed_components = vec_to_opt_refs(&components);
                let parsed_fix_versions = vec_to_opt_refs(&fix_versions);
                let assignee_str = match assignee.as_deref() {
                    Some("me") => {
                        let me = client.get_myself().await?;
                        Some(me.account_id)
                    }
                    Some(id) => Some(id.to_string()),
                    None => None,
                };
                let draft = IssueDraft {
                    project_key: &project,
                    issue_type: &issue_type,
                    summary: &summary,
                    description: description.as_deref(),
                    priority: priority.as_deref(),
                    labels: parsed_labels.as_deref(),
                    components: parsed_components.as_deref(),
                    fix_versions: parsed_fix_versions.as_deref(),
                    assignee: assignee_str.as_deref(),
                    parent: parent.as_deref(),
                };
                commands::issues::create(&client, &out, &draft, sprint.as_deref(), &field).await?
            }
            IssuesCommand::Update {
                key,
                summary,
                description,
                priority,
                components,
                fix_versions,
                labels,
                assignee,
                field,
            } => {
                let parsed_components = parse_vec_update_arg(&components);
                let parsed_fix_versions = parse_vec_update_arg(&fix_versions);
                let parsed_labels = parse_vec_update_arg(&labels);

                let resolved_assignee =
                    commands::issues::resolve_assignee_arg(&client, assignee.as_deref()).await?;
                let assignee_ref: Option<Option<&str>> =
                    resolved_assignee.as_ref().map(|inner| inner.as_deref());

                let update = IssueUpdate {
                    summary: summary.as_deref(),
                    description: description.as_deref(),
                    priority: priority.as_deref(),
                    components: parsed_components.as_deref(),
                    fix_versions: parsed_fix_versions.as_deref(),
                    labels: parsed_labels.as_deref(),
                    assignee: assignee_ref,
                };
                commands::issues::update(&client, &out, &key, &update, &field).await?
            }
            IssuesCommand::Move { key, sprint } => {
                commands::issues::move_to_sprint(&client, &out, &key, &sprint).await?
            }
            IssuesCommand::Comment { key, body } => {
                commands::issues::comment(&client, &out, &key, &body).await?
            }
            IssuesCommand::Transition { key, to } => {
                commands::issues::transition(&client, &out, &key, &to).await?
            }
            IssuesCommand::ListTransitions { key } => {
                commands::issues::list_transitions(&client, &out, &key).await?
            }
            IssuesCommand::Assign { key, assignee } => {
                commands::issues::assign(&client, &out, &key, &assignee).await?
            }
            IssuesCommand::LinkTypes => commands::issues::link_types(&client, &out).await?,
            IssuesCommand::Link { key, to, link_type } => {
                commands::issues::link(&client, &out, &key, &to, &link_type).await?
            }
            IssuesCommand::Unlink { link_id } => {
                commands::issues::unlink(&client, &out, &link_id).await?
            }
            IssuesCommand::LogWork {
                key,
                time,
                comment,
                started,
            } => {
                commands::issues::log_work(
                    &client,
                    &out,
                    &key,
                    &time,
                    comment.as_deref(),
                    started.as_deref(),
                )
                .await?
            }
            IssuesCommand::Attachments { key } => {
                commands::issues::attachments(&client, &out, &key).await?
            }
            IssuesCommand::Attach { key, file } => {
                commands::issues::attach(&client, &out, &key, &file).await?
            }
            IssuesCommand::DownloadAttachment { id, dir, force } => {
                commands::issues::download_attachment(&client, &out, &id, &dir, force).await?
            }
            IssuesCommand::DeleteAttachment { id } => {
                commands::issues::delete_attachment(&client, &out, &id).await?
            }
            IssuesCommand::BulkTransition {
                jql,
                to,
                dry_run,
                yes,
            } => {
                if !yes && !dry_run && !std::io::stdin().is_terminal() {
                    return Err(jira_cli::api::ApiError::ConfirmationRequired(
                        "bulk-transition requires --yes when stdin is not a terminal".into(),
                    )
                    .into());
                }
                commands::issues::bulk_transition(&client, &out, &jql, &to, dry_run).await?
            }
            IssuesCommand::BulkAssign {
                jql,
                assignee,
                dry_run,
                yes,
            } => {
                if !yes && !dry_run && !std::io::stdin().is_terminal() {
                    return Err(jira_cli::api::ApiError::ConfirmationRequired(
                        "bulk-assign requires --yes when stdin is not a terminal".into(),
                    )
                    .into());
                }
                commands::issues::bulk_assign(&client, &out, &jql, &assignee, dry_run).await?
            }
            IssuesCommand::External(args) => {
                let key = args
                    .first()
                    .ok_or_else(|| ApiError::InvalidInput("missing issue key".into()))?;
                let open = args.iter().any(|a| a == "--open");
                commands::issues::show(&client, &out, key, open).await?
            }
        },

        Command::Projects(cmd) => match cmd {
            ProjectsCommand::List => commands::projects::list(&client, &out).await?,
            ProjectsCommand::Show { key } => commands::projects::show(&client, &out, &key).await?,
            ProjectsCommand::Components { key } => {
                commands::projects::components(&client, &out, &key).await?
            }
            ProjectsCommand::Versions { key } => {
                commands::projects::versions(&client, &out, &key).await?
            }
        },

        Command::Users(cmd) => match cmd {
            UsersCommand::Search { query } => {
                commands::users::search(&client, &out, &query).await?
            }
        },

        Command::Boards(cmd) => match cmd {
            BoardsCommand::List => commands::boards::list(&client, &out).await?,
        },

        Command::Sprints(cmd) => match cmd {
            SprintsCommand::List { board, state } => {
                // "all" is a special token meaning no state filter.
                let state_filter = if state == "all" {
                    None
                } else {
                    Some(state.as_str())
                };
                commands::sprints::list(&client, &out, board.as_deref(), state_filter).await?
            }
        },

        Command::Search {
            jql,
            limit,
            offset,
            all,
            fields,
        } => {
            let field_filter = fields.as_deref().map(parse_fields_arg);
            commands::search::run(
                &client,
                &out,
                &jql,
                limit,
                offset,
                all,
                field_filter.as_deref(),
            )
            .await?
        }

        Command::Myself => commands::myself::show(&client, &out).await?,

        Command::Doctor => commands::doctor::run(&client, &cfg, &out).await?,

        Command::Fields(cmd) => match cmd {
            FieldsCommand::List { custom } => commands::fields::list(&client, &out, custom).await?,
        },

        // Already handled above
        Command::Schema { .. }
        | Command::Capabilities
        | Command::Completions { .. }
        | Command::Config(_)
        | Command::Init => {}
    }

    Ok(())
}

fn print_schema(command: Option<&str>) -> Result<(), ApiError> {
    let schema = match command {
        Some(path) => command_schema_json(path)?,
        None => schema_json(),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("failed to serialize schema")
    );
    Ok(())
}

fn command_schema_json(path: &str) -> Result<serde_json::Value, ApiError> {
    let normalized = path.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalized.strip_prefix("jira ").unwrap_or(&normalized);
    if normalized.is_empty() || normalized == "jira" {
        return Err(ApiError::InvalidInput(
            "--command cannot be empty; try `jira schema --command 'issues list'`".into(),
        ));
    }

    let schema = schema_json();
    let command = schema["commands"]
        .as_array()
        .and_then(|commands| commands.iter().find(|entry| entry["name"] == normalized))
        .cloned()
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "command '{path}'; inspect available commands with `jira schema`"
            ))
        })?;

    let mut compact = command
        .as_object()
        .cloned()
        .expect("schema commands are objects");
    compact.insert("cli".into(), serde_json::json!(schema["name"]));
    compact.insert("version".into(), serde_json::json!(schema["version"]));
    compact.insert("clispec".into(), serde_json::json!(schema["clispec"]));
    compact.insert(
        "global_args".into(),
        serde_json::json!(schema["global_args"]),
    );
    compact.insert("errors".into(), serde_json::json!(schema["errors"]));
    compact.insert("output".into(), serde_json::json!(schema["output"]));
    Ok(serde_json::Value::Object(compact))
}

fn schema_json() -> serde_json::Value {
    use std::collections::{HashMap, HashSet};

    let config_path = jira_cli::config::schema_config_path();
    let config_path_description = jira_cli::config::schema_config_path_description();
    let permission_advice = jira_cli::config::schema_recommended_permissions_example();

    let dc_pat_instructions = jira_cli::config::schema_dc_pat_url_example();

    // Keys here and in `init_fields` below describe the same object and are kept
    // in step by `schema_init_json_shape_and_output_fields_describe_the_same_keys`.
    let init_shape = serde_json::json!({
        "configPath": "/path/to/config.toml",
        "pathResolution": config_path_description,
        "tokenInstructions": "https://id.atlassian.com/manage-profile/security/api-tokens",
        "dcPatInstructions": dc_pat_instructions,
        "configExists": false,
        "recommendedPermissions": permission_advice,
        "example": jira_cli::config::schema_example_config()
    });

    let init_fields = serde_json::json!([
        {"name": "configPath", "type": "string"},
        {"name": "pathResolution", "type": "string", "description": "How that path is resolved on this platform"},
        {"name": "configExists", "type": "boolean"},
        {"name": "tokenInstructions", "type": "string", "description": "Where to create a Jira Cloud API token"},
        {"name": "dcPatInstructions", "type": "string", "description": "Where to create a Personal Access Token on Jira Data Center/Server"},
        {"name": "recommendedPermissions", "type": "string"},
        {"name": "example", "type": "object", "description": "A complete example config file", "fields": [
            {"name": "default", "type": "object", "description": "The profile used when none is named", "fields": [
                {"name": "host", "type": "string"},
                {"name": "email", "type": "string", "description": "Omit for `pat` auth, which Jira Data Center uses"},
                {"name": "token", "type": "string"},
                {"name": "auth_type", "type": "string", "description": "\"basic\" for Jira Cloud, \"pat\" for a Data Center personal access token"},
                {"name": "api_version", "type": "integer", "description": "3 for Jira Cloud, 2 for Data Center"},
                {"name": "read_only", "type": "boolean", "description": "Block commands that write to Jira"}
            ]},
            {"name": "profiles", "type": "object", "description": "Keyed by profile name, so the keys are chosen by the user rather than fixed. Each value has the same shape as `default`"}
        ]}
    ]);

    // Mutating flag per command path.
    let mutating: HashMap<&str, bool> = [
        ("issues list", false),
        ("issues mine", false),
        ("issues comments", false),
        ("issues show", false),
        ("issues create", true),
        ("issues update", true),
        ("issues move", true),
        ("issues comment", true),
        ("issues transition", true),
        ("issues list-transitions", false),
        ("issues assign", true),
        ("issues link-types", false),
        ("issues link", true),
        ("issues unlink", true),
        ("issues log-work", true),
        ("issues attachments", false),
        ("issues attach", true),
        // Writes a file and creates its parent directories, and with --force
        // overwrites an existing one. Mutating tracks persistent state of any
        // kind, local included, the same way `config init` does; the read-only
        // guard is a narrower question about writing to Jira.
        ("issues download-attachment", true),
        ("issues delete-attachment", true),
        ("issues bulk-transition", true),
        ("issues bulk-assign", true),
        ("projects list", false),
        ("projects show", false),
        ("projects components", false),
        ("projects versions", false),
        ("search", false),
        ("users search", false),
        ("boards list", false),
        ("sprints list", false),
        ("myself", false),
        ("doctor", false),
        ("fields list", false),
        ("config show", false),
        ("config init", true),
        ("config remove", true),
        ("init", true),
        ("schema", false),
        ("capabilities", false),
        ("completions", false),
    ]
    .into_iter()
    .collect();

    // Output fields per command path.
    // `type` vocabulary: string, integer, boolean, object, string[], object[].
    // `nullable` means the key is always present but its value may be JSON null.
    // `optional` means the key may be absent entirely.
    let user_fields = serde_json::json!([
        {"name": "displayName", "type": "string"},
        {"name": "accountId", "type": "string", "nullable": true}
    ]);
    let version_fields = serde_json::json!([
        {"name": "id", "type": "string"},
        {"name": "name", "type": "string"},
        {"name": "description", "type": "string", "nullable": true},
        {"name": "released", "type": "boolean", "nullable": true},
        {"name": "archived", "type": "boolean", "nullable": true},
        {"name": "releaseDate", "type": "string", "nullable": true}
    ]);
    let comment_fields = serde_json::json!([
        {"name": "id", "type": "string"},
        {"name": "author", "type": "object", "fields": user_fields},
        {"name": "body", "type": "string"},
        {"name": "created", "type": "string"},
        {"name": "updated", "type": "string", "nullable": true}
    ]);
    let link_type_fields = serde_json::json!([
        {"name": "id", "type": "string"},
        {"name": "name", "type": "string"},
        {"name": "inward", "type": "string", "description": "Phrasing from the inward side, e.g. \"is blocked by\""},
        {"name": "outward", "type": "string", "description": "Phrasing from the outward side, e.g. \"blocks\""}
    ]);
    let linked_issue_fields = serde_json::json!([
        {"name": "key", "type": "string"},
        {"name": "summary", "type": "string"},
        {"name": "status", "type": "string"}
    ]);
    // Element shape shared by `issues list`, `issues mine` and `search`.
    let issue_summary_fields = serde_json::json!([
        {"name": "key", "type": "string", "description": "Issue key (e.g. PROJ-123)"},
        {"name": "id", "type": "string", "description": "Internal Jira ID"},
        {"name": "url", "type": "string", "description": "Browser URL for the issue"},
        {"name": "summary", "type": "string"},
        {"name": "status", "type": "string"},
        {"name": "assignee", "type": "object", "nullable": true, "fields": user_fields, "description": "Null when the issue is unassigned"},
        {"name": "priority", "type": "string", "nullable": true, "description": "Null when the project does not set a priority"},
        {"name": "type", "type": "string"},
        {"name": "created", "type": "string", "nullable": true},
        {"name": "updated", "type": "string", "nullable": true}
    ]);
    let output_fields: HashMap<&str, serde_json::Value> = [
        ("issues list", issue_summary_fields.clone()),
        ("issues mine", issue_summary_fields.clone()),
        ("search", issue_summary_fields),
        (
            "issues show",
            serde_json::json!([
                {"name": "key", "type": "string"},
                {"name": "id", "type": "string"},
                {"name": "url", "type": "string"},
                {"name": "summary", "type": "string"},
                {"name": "status", "type": "string"},
                {"name": "type", "type": "string"},
                {"name": "priority", "type": "string", "nullable": true},
                {"name": "description", "type": "string", "nullable": true, "description": "Plain text extracted from the Atlassian Document Format body; null when the issue has no description"},
                {"name": "assignee", "type": "object", "nullable": true, "fields": user_fields},
                {"name": "reporter", "type": "object", "nullable": true, "fields": user_fields},
                {"name": "labels", "type": "string[]", "nullable": true},
                {"name": "components", "type": "object[]", "nullable": true, "fields": [
                    {"name": "id", "type": "string"},
                    {"name": "name", "type": "string"},
                    {"name": "description", "type": "string", "nullable": true}
                ]},
                {"name": "fixVersions", "type": "object[]", "nullable": true, "fields": version_fields},
                {"name": "affectedVersions", "type": "object[]", "nullable": true, "fields": version_fields},
                {"name": "comments", "type": "object[]", "fields": comment_fields, "description": "Already included here - no separate `issues comments` call is needed"},
                {"name": "issueLinks", "type": "object[]", "description": "Already included here - no separate call is needed", "fields": [
                    {"name": "id", "type": "string"},
                    {"name": "sentence", "type": "string", "description": "Human-readable form, e.g. \"PROJ-1 blocks PROJ-2\""},
                    {"name": "type", "type": "object", "fields": link_type_fields},
                    {"name": "outwardIssue", "type": "object", "nullable": true, "fields": linked_issue_fields.clone()},
                    {"name": "inwardIssue", "type": "object", "nullable": true, "fields": linked_issue_fields}
                ]},
                {"name": "created", "type": "string", "nullable": true},
                {"name": "updated", "type": "string", "nullable": true}
            ]),
        ),
        ("issues comments", comment_fields),
        (
            "issues create",
            serde_json::json!([
                {"name": "key", "type": "string"},
                {"name": "id", "type": "string"},
                {"name": "url", "type": "string"},
                {"name": "parent", "type": "string", "optional": true, "description": "Present only when --parent was passed"},
                {"name": "sprintId", "type": "integer", "optional": true, "description": "Present only when --sprint was passed"},
                {"name": "sprintName", "type": "string", "optional": true, "description": "Present only when --sprint was passed"}
            ]),
        ),
        (
            "issues update",
            serde_json::json!([
                {"name": "key", "type": "string"},
                {"name": "updated", "type": "boolean"}
            ]),
        ),
        (
            "issues move",
            serde_json::json!([
                {"name": "issue", "type": "string"},
                {"name": "sprintId", "type": "integer"},
                {"name": "sprintName", "type": "string"}
            ]),
        ),
        (
            "issues comment",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "issue", "type": "string"},
                {"name": "url", "type": "string"},
                {"name": "author", "type": "string"},
                {"name": "created", "type": "string"}
            ]),
        ),
        (
            "issues transition",
            serde_json::json!([
                {"name": "issue", "type": "string"},
                {"name": "transition", "type": "string"},
                {"name": "status", "type": "string"},
                {"name": "id", "type": "string"}
            ]),
        ),
        (
            "issues list-transitions",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "to", "type": "object", "nullable": true, "description": "Target status", "fields": [
                    {"name": "name", "type": "string"},
                    {"name": "statusCategory", "type": "object", "nullable": true, "fields": [
                        {"name": "key", "type": "string", "description": "Workflow category, e.g. \"new\", \"indeterminate\", \"done\""},
                        {"name": "name", "type": "string"}
                    ]}
                ]}
            ]),
        ),
        (
            "issues assign",
            serde_json::json!([
                {"name": "issue", "type": "string"},
                {"name": "accountId", "type": "string", "nullable": true, "description": "Null when the issue was unassigned"}
            ]),
        ),
        (
            "issues link-types",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "inward", "type": "string"},
                {"name": "outward", "type": "string"}
            ]),
        ),
        (
            "issues link",
            serde_json::json!([
                {"name": "from", "type": "string"},
                {"name": "to", "type": "string"},
                {"name": "type", "type": "string"}
            ]),
        ),
        (
            "issues unlink",
            serde_json::json!([
                {"name": "linkId", "type": "string"}
            ]),
        ),
        (
            "issues log-work",
            serde_json::json!([
                {"name": "id", "type": "string", "description": "Worklog entry ID"},
                {"name": "issue", "type": "string"},
                {"name": "timeSpent", "type": "string", "description": "As Jira normalised it, e.g. \"1h 30m\""},
                {"name": "timeSpentSeconds", "type": "integer"},
                {"name": "author", "type": "string"},
                {"name": "started", "type": "string"},
                {"name": "created", "type": "string"}
            ]),
        ),
        (
            "issues attachments",
            serde_json::json!([
                {"name": "id", "type": "string", "description": "Attachment ID"},
                {"name": "filename", "type": "string"},
                {"name": "size", "type": "integer", "description": "Size in bytes"},
                {"name": "mimeType", "type": "string", "nullable": true},
                {"name": "author", "type": "string", "nullable": true, "description": "Display name of the uploader; null when Jira reports none"},
                {"name": "created", "type": "string"}
            ]),
        ),
        (
            "issues attach",
            serde_json::json!([
                {"name": "issue", "type": "string"},
                {"name": "id", "type": "string", "description": "Attachment ID"},
                {"name": "filename", "type": "string"},
                {"name": "size", "type": "integer", "description": "Size in bytes"},
                {"name": "mimeType", "type": "string", "nullable": true},
                {"name": "author", "type": "string", "nullable": true},
                {"name": "created", "type": "string"}
            ]),
        ),
        (
            "issues download-attachment",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "filename", "type": "string"},
                {"name": "path", "type": "string", "description": "Local path the file was written to"},
                {"name": "size", "type": "integer", "description": "Size in bytes"}
            ]),
        ),
        (
            "issues delete-attachment",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "deleted", "type": "boolean"}
            ]),
        ),
        (
            "issues bulk-transition",
            serde_json::json!([
                {"name": "dryRun", "type": "boolean"},
                {"name": "total", "type": "integer", "description": "Issues the JQL query matched"},
                {"name": "succeeded", "type": "integer", "description": "Always 0 on a --dry-run, which changes nothing"},
                {"name": "failed", "type": "integer"},
                {"name": "issues", "type": "object[]", "description": "One entry per matched issue", "fields": [
                    {"name": "key", "type": "string"},
                    {"name": "from", "type": "string", "optional": true, "description": "Status before the transition; present only on a successful transition"},
                    {"name": "to", "type": "string", "optional": true},
                    {"name": "action", "type": "string", "optional": true, "description": "Present only on a --dry-run entry"},
                    {"name": "ok", "type": "boolean", "optional": true, "description": "Absent on a --dry-run entry, which changes nothing"},
                    {"name": "error", "type": "string", "optional": true, "description": "Present only when ok is false"}
                ]}
            ]),
        ),
        (
            "issues bulk-assign",
            serde_json::json!([
                {"name": "dryRun", "type": "boolean"},
                {"name": "total", "type": "integer", "description": "Issues the JQL query matched"},
                {"name": "succeeded", "type": "integer", "description": "Always 0 on a --dry-run, which changes nothing"},
                {"name": "failed", "type": "integer"},
                {"name": "issues", "type": "object[]", "description": "One entry per matched issue", "fields": [
                    {"name": "key", "type": "string"},
                    {"name": "currentAssignee", "type": "string", "nullable": true, "optional": true, "description": "Present only on a --dry-run entry; null when the issue is unassigned"},
                    {"name": "action", "type": "string", "optional": true, "description": "Present only on a --dry-run entry"},
                    {"name": "to", "type": "string", "optional": true},
                    {"name": "assignee", "type": "string", "optional": true, "description": "Present only on a successful assignment"},
                    {"name": "ok", "type": "boolean", "optional": true, "description": "Absent on a --dry-run entry, which changes nothing"},
                    {"name": "error", "type": "string", "optional": true, "description": "Present only when ok is false"}
                ]}
            ]),
        ),
        (
            "projects list",
            serde_json::json!([
                {"name": "key", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "id", "type": "string"},
                {"name": "type", "type": "string"}
            ]),
        ),
        (
            "projects show",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "key", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "type", "type": "string", "nullable": true}
            ]),
        ),
        (
            "projects components",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "description", "type": "string", "nullable": true}
            ]),
        ),
        ("projects versions", version_fields),
        (
            "users search",
            serde_json::json!([
                {"name": "accountId", "type": "string"},
                {"name": "displayName", "type": "string"},
                {"name": "email", "type": "string", "nullable": true, "description": "Null when the account keeps its email private"}
            ]),
        ),
        (
            "boards list",
            serde_json::json!([
                {"name": "id", "type": "integer"},
                {"name": "name", "type": "string"},
                {"name": "type", "type": "string"}
            ]),
        ),
        (
            "sprints list",
            serde_json::json!([
                {"name": "id", "type": "integer"},
                {"name": "name", "type": "string"},
                {"name": "state", "type": "string"},
                {"name": "boardId", "type": "integer"},
                {"name": "boardName", "type": "string"},
                {"name": "startDate", "type": "string", "nullable": true},
                {"name": "endDate", "type": "string", "nullable": true},
                {"name": "completeDate", "type": "string", "nullable": true, "description": "Set once the sprint is closed"}
            ]),
        ),
        (
            "myself",
            serde_json::json!([
                {"name": "accountId", "type": "string"},
                {"name": "displayName", "type": "string"},
                {"name": "email", "type": "string", "nullable": true, "description": "Null when the account keeps its email private"}
            ]),
        ),
        (
            "doctor",
            serde_json::json!([
                {"name": "ok", "type": "boolean"},
                {"name": "instance", "type": "string", "optional": true},
                {"name": "user", "type": "object", "optional": true, "fields": [
                    {"name": "accountId", "type": "string"},
                    {"name": "displayName", "type": "string"},
                    {"name": "email", "type": "string", "nullable": true}
                ]},
                {"name": "projectCount", "type": "integer", "optional": true},
                {"name": "checks", "type": "object[]", "fields": [
                    {"name": "name", "type": "string"},
                    {"name": "ok", "type": "boolean"},
                    {"name": "detail", "type": "string"}
                ]}
            ]),
        ),
        (
            "fields list",
            serde_json::json!([
                {"name": "id", "type": "string"},
                {"name": "name", "type": "string"},
                {"name": "custom", "type": "boolean"},
                {"name": "type", "type": "string", "nullable": true, "description": "Null for fields Jira reports without a schema"}
            ]),
        ),
        (
            "config show",
            serde_json::json!([
                {"name": "configPath", "type": "string"},
                {"name": "host", "type": "string"},
                {"name": "email", "type": "string"},
                {"name": "tokenMasked", "type": "string"}
            ]),
        ),
        ("config init", init_fields.clone()),
        (
            "config remove",
            serde_json::json!([
                {"name": "profile", "type": "string"},
                {"name": "removed", "type": "boolean"}
            ]),
        ),
        ("init", init_fields),
        ("schema", serde_json::json!([])),
        (
            "capabilities",
            serde_json::json!([
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "clispec", "type": "string"},
                {"name": "output", "type": "string[]"},
                {"name": "features", "type": "string[]"}
            ]),
        ),
        ("completions", serde_json::json!([])),
    ]
    .into_iter()
    .collect();

    // Annotations for extra info (json_shape and alias_for).
    let annotations: HashMap<&str, serde_json::Value> = [
        (
            "config init",
            serde_json::json!({ "json_shape": init_shape.clone() }),
        ),
        (
            "init",
            serde_json::json!({ "alias_for": "config init", "json_shape": init_shape }),
        ),
    ]
    .into_iter()
    .collect();

    // Global arg IDs excluded from per-command arg lists.
    let global_ids: HashSet<&str> = [
        "json", "output", "quiet", "no_color", "host", "email", "profile",
    ]
    .iter()
    .copied()
    .collect();

    let root = Cli::command();
    let commands = walk_commands(
        &root,
        &[],
        &annotations,
        &global_ids,
        &mutating,
        &output_fields,
    );

    let mut schema = serde_json::json!({
        "$schema": "https://clispec.dev/schema/v0.3.json",
        "clispec": "0.3",
        "name": "jira",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "CLI for Jira - optimized for humans and agents",
        "global_args": [
            {"name": "--output", "type": "string", "required": false, "default": "auto", "description": "Output format: auto (default), text, or json", "enum": ["auto", "text", "json"]},
            {"name": "--quiet", "type": "boolean", "required": false, "description": "Suppress non-data output"},
            {"name": "--no-color", "type": "boolean", "required": false, "description": "Disable ANSI color"},
            {"name": "--host", "type": "string", "required": false, "description": "Atlassian domain (overrides config/env)"},
            {"name": "--email", "type": "string", "required": false, "description": "Account email (overrides config/env)"},
            {"name": "--profile", "type": "string", "required": false, "description": "Config profile to use"},
        ],
        // Rendered from the one error contract table the binary itself uses, so
        // the schema cannot declare a kind the CLI never emits.
        "errors": jira_cli::output::ALL_ERRORS
            .iter()
            .map(|e| serde_json::json!({
                "kind": e.kind,
                "exit_code": e.exit_code,
                "retryable": e.retryable,
                "description": e.description,
            }))
            .collect::<Vec<_>>(),
        "auth": {
            "note": format!(
                "Provide host and email via CLI flags, environment variables, or the config file at {config_path}. Provide the API token via JIRA_TOKEN or that config file."
            ),
            "token_instructions": "https://id.atlassian.com/manage-profile/security/api-tokens",
            "required_fields": ["host", "token"],
            "email_note": "email is required for basic auth (Jira Cloud) but not for pat auth (Jira Data Center/Server)",
            "config_file": {
                "path": config_path,
                "description": config_path_description,
                "profile_selector": { "flag": "--profile", "env": "JIRA_PROFILE" }
            },
            "resolution_order": {
                "host": ["--host", "JIRA_HOST", "config profile/default host"],
                "email": ["--email", "JIRA_EMAIL", "config profile/default email"],
                "token": ["JIRA_TOKEN", "config profile/default token"],
                "auth_type": ["JIRA_AUTH_TYPE", "config profile/default auth_type"],
                "api_version": ["JIRA_API_VERSION", "config profile/default api_version"]
            },
            "env": [
                { "name": "JIRA_HOST", "description": "Atlassian domain override", "required": false },
                { "name": "JIRA_EMAIL", "description": "Account email (not required when auth_type=pat)", "required": false },
                { "name": "JIRA_TOKEN", "description": "API token (env/config only)", "required": false },
                { "name": "JIRA_PROFILE", "description": "Config profile", "required": false },
                { "name": "JIRA_AUTH_TYPE", "description": "Authentication type: basic (default, Jira Cloud) or pat (Personal Access Token, Jira Data Center/Server)", "required": false },
                { "name": "JIRA_API_VERSION", "description": "Jira REST API version: 3 (default, Cloud) or 2 (Data Center/Server)", "required": false }
            ]
        },
        // The guard exists so a Jira account can be handed to an agent without
        // write access, which is worth nothing if the agent cannot see that it
        // is on or which commands it stops.
        "read_only": {
            "description": "Blocks every command that writes to Jira. Local-only writes (the config file, downloaded attachments) are unaffected.",
            "env": "JIRA_READ_ONLY",
            "config_key": "read_only",
            "values_on": jira_cli::config::TRUTHY,
            "values_off": jira_cli::config::FALSY,
            "unrecognized_value": "rejected with invalid_input rather than treated as off",
            "error": { "kind": "invalid_input", "exit_code": jira_cli::output::exit_codes::INPUT_ERROR },
            "blocked_commands": READ_ONLY_BLOCKED_COMMANDS,
        },
        "diagnostics": {
            "description": "Opt-in detail for troubleshooting. Unlike read_only, an unrecognised value is read as off rather than rejected, so a typo here never fails a command.",
            "env": [
                {
                    "name": "JIRA_DEBUG_HTTP",
                    "description": "Include the raw Jira response body in API error messages, for when the default summary is ambiguous.",
                    "values_on": jira_cli::config::TRUTHY,
                }
            ]
        },
        "commands": commands,
        "output": {"tty": "text", "piped": "json"}
    });
    enrich_v0_3(&mut schema);
    schema
}

fn enrich_v0_3(schema: &mut serde_json::Value) {
    let Some(commands) = schema["commands"].as_array_mut() else {
        return;
    };
    for command in commands {
        let Some(object) = command.as_object_mut() else {
            continue;
        };
        let name = object["name"].as_str().unwrap_or_default().to_string();
        let mutating = object["mutating"].as_bool().unwrap_or(false);
        let non_idempotent = matches!(
            name.as_str(),
            "issues create"
                | "issues comment"
                | "issues link"
                | "issues log-work"
                | "issues attach"
        );
        object.insert(
            "effects".into(),
            serde_json::json!(if !mutating {
                "read_only"
            } else if non_idempotent {
                "non_idempotent"
            } else {
                "idempotent"
            }),
        );

        if name == "completions" {
            object.insert("output_kind".into(), serde_json::json!("opaque"));
            object.insert("media_type".into(), serde_json::json!("text/plain"));
            object.remove("output_fields");
            continue;
        }

        object.insert("cardinality".into(), serde_json::json!("bounded"));
        if matches!(name.as_str(), "issues list" | "search") {
            object.insert("cardinality".into(), serde_json::json!("unbounded"));
            object.insert(
                "pagination".into(),
                serde_json::json!({
                    "style": "offset",
                    "limit_arg": "--limit",
                    "offset_arg": "--offset"
                }),
            );
            object.insert("fields_arg".into(), serde_json::json!("--fields"));
        }
        if name == "capabilities" {
            object.insert(
                "example".into(),
                serde_json::json!({"args": ["capabilities"]}),
            );
            object.insert("cardinality".into(), serde_json::json!("single"));
        }
        if name == "schema" {
            object.insert("cardinality".into(), serde_json::json!("single"));
            object.insert(
                "stdout_schema".into(),
                serde_json::json!({"$ref": "https://clispec.dev/schema/v0.3.json"}),
            );
        }
        if let Some(fields) = object
            .get_mut("output_fields")
            .and_then(serde_json::Value::as_array_mut)
        {
            normalize_output_fields(fields);
        }
        if !object.contains_key("output_fields") && !object.contains_key("stdout_schema") {
            object.insert("stdout_schema".into(), serde_json::json!({}));
        }
    }
}

fn normalize_output_fields(fields: &mut [serde_json::Value]) {
    for field in fields {
        let Some(object) = field.as_object_mut() else {
            continue;
        };
        let old_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match old_type {
            "string[]" => {
                object.insert("type".into(), serde_json::json!("array"));
                object.insert("items".into(), serde_json::json!({"type": "string"}));
            }
            "object[]" => {
                object.insert("type".into(), serde_json::json!("array"));
                let nested = object.remove("fields");
                let mut items = serde_json::json!({"type": "object"});
                if let Some(nested) = nested {
                    items["fields"] = nested;
                }
                object.insert("items".into(), items);
            }
            _ => {}
        }
        if let Some(nested) = object
            .get_mut("fields")
            .and_then(serde_json::Value::as_array_mut)
        {
            normalize_output_fields(nested);
        }
    }
}

/// The commands `JIRA_READ_ONLY` refuses, named as `jira schema` names them.
///
/// The guard itself is the `matches!` in `run`, which cannot read this list
/// because it dispatches on enum variants. `tests/cli.rs` closes that gap from
/// the outside: it runs every name here against a mock server under
/// `JIRA_READ_ONLY=1` and fails if any request reaches it, so this list is
/// checked against the binary rather than trusted.
const READ_ONLY_BLOCKED_COMMANDS: &[&str] = &[
    "issues create",
    "issues update",
    "issues move",
    "issues comment",
    "issues transition",
    "issues assign",
    "issues link",
    "issues unlink",
    "issues log-work",
    "issues attach",
    "issues delete-attachment",
    "issues bulk-transition",
    "issues bulk-assign",
];

/// Walk the clap command tree and emit a schema entry for every leaf command.
///
/// Intermediate subcommand groups (e.g. `issues`, `projects`) are not emitted;
/// only leaf commands that perform an action produce an entry. Command names are
/// built as space-joined paths (e.g. `"issues list"`).
fn walk_commands(
    cmd: &clap::Command,
    path: &[String],
    annotations: &std::collections::HashMap<&str, serde_json::Value>,
    global_ids: &std::collections::HashSet<&str>,
    mutating: &std::collections::HashMap<&str, bool>,
    output_fields: &std::collections::HashMap<&str, serde_json::Value>,
) -> Vec<serde_json::Value> {
    let subs: Vec<_> = cmd
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .collect();

    if subs.is_empty() {
        // Leaf command - emit a schema entry.
        let positionals: Vec<_> = cmd.get_arguments().filter(|a| a.is_positional()).collect();
        let flags: Vec<_> = cmd
            .get_arguments()
            .filter(|a| {
                !a.is_positional()
                    && a.get_long() != Some("help")
                    && a.get_long() != Some("version")
                    && !global_ids.contains(a.get_id().as_str())
            })
            .collect();

        let base_path = path.join(" ");

        let mut entry = serde_json::Map::new();
        entry.insert("name".into(), serde_json::json!(base_path));
        entry.insert(
            "description".into(),
            serde_json::json!(cmd.get_about().map(|s| s.to_string()).unwrap_or_default()),
        );

        // mutating field
        let is_mutating = mutating.get(base_path.as_str()).copied().unwrap_or(false);
        entry.insert("mutating".into(), serde_json::json!(is_mutating));

        let ann = annotations.get(base_path.as_str());

        if let Some(alias) = ann.and_then(|a| a.get("alias_for")) {
            entry.insert("alias_for".into(), alias.clone());
        }

        // Merge positionals and flags into a single args array, each with name+type.
        let mut all_args: Vec<serde_json::Value> = Vec::new();

        for a in &positionals {
            let mut arg_obj = serde_json::Map::new();
            arg_obj.insert("name".into(), serde_json::json!(a.get_id().as_str()));
            arg_obj.insert("type".into(), serde_json::json!(arg_type(a)));
            arg_obj.insert("required".into(), serde_json::json!(a.is_required_set()));
            if let Some(help) = a.get_help() {
                arg_obj.insert("description".into(), serde_json::json!(help.to_string()));
            }
            all_args.push(serde_json::Value::Object(arg_obj));
        }

        for a in &flags {
            let long_name = a
                .get_long()
                .map(|l| format!("--{l}"))
                .unwrap_or_else(|| format!("--{}", a.get_id().as_str().replace('_', "-")));
            let mut arg_obj = serde_json::Map::new();
            arg_obj.insert("name".into(), serde_json::json!(long_name));
            if let Some(short) = a.get_short() {
                arg_obj.insert("short".into(), serde_json::json!(format!("-{short}")));
            }
            arg_obj.insert("type".into(), serde_json::json!(arg_type(a)));
            arg_obj.insert("required".into(), serde_json::json!(a.is_required_set()));
            if !a.get_default_values().is_empty() {
                let dv = a.get_default_values()[0].to_string_lossy();
                if let Ok(n) = dv.parse::<i64>() {
                    arg_obj.insert("default".into(), serde_json::json!(n));
                } else {
                    arg_obj.insert("default".into(), serde_json::json!(dv.as_ref()));
                }
            }
            if let Some(help) = a.get_help() {
                let help_str = help.to_string();
                if !help_str.is_empty() {
                    arg_obj.insert("description".into(), serde_json::json!(help_str));
                }
            }
            all_args.push(serde_json::Value::Object(arg_obj));
        }

        entry.insert("args".into(), serde_json::json!(all_args));

        // output_fields
        if let Some(fields) = output_fields.get(base_path.as_str()) {
            entry.insert("output_fields".into(), fields.clone());
        } else {
            entry.insert("output_fields".into(), serde_json::json!([]));
        }

        if let Some(shape) = ann.and_then(|a| a.get("json_shape")) {
            entry.insert("json_shape".into(), shape.clone());
        }

        vec![serde_json::Value::Object(entry)]
    } else {
        // Intermediate group - recurse into subcommands.
        subs.iter()
            .flat_map(|sub| {
                let mut new_path = path.to_vec();
                new_path.push(sub.get_name().to_string());
                walk_commands(
                    sub,
                    &new_path,
                    annotations,
                    global_ids,
                    mutating,
                    output_fields,
                )
            })
            .collect()
    }
}

/// Infer the type string for a clap argument.
fn arg_type(a: &clap::Arg) -> &'static str {
    use clap::ArgAction;
    match a.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse => "boolean",
        ArgAction::Count => "integer",
        ArgAction::Append => "string[]",
        _ => {
            // Numeric-looking IDs (limit/offset style args).
            let id = a.get_id().as_str();
            if id == "limit" || id == "offset" {
                return "integer";
            }
            "string"
        }
    }
}

fn handle_completions(
    shell: Shell,
    install: bool,
    out: &OutputConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    use clap_complete::generate;
    use std::io;

    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();

    if install {
        let (path, mut writer, note) = match shell {
            Shell::Bash => {
                let p = bash_completion_path()?;
                let writer = create_completion_writer(&p)?;
                let note = format!(
                    "Generated completion file at {}. Source it from your shell startup if ~/.bash_completion.d is not loaded automatically.",
                    p.display()
                );
                (p, writer, note)
            }
            Shell::Zsh => {
                let p = zsh_completion_path()?;
                let writer = create_completion_writer(&p)?;
                let note = format!(
                    "Generated completion file at {}. Ensure its parent directory is in `fpath`, then run `autoload -Uz compinit && compinit`.",
                    p.display()
                );
                (p, writer, note)
            }
            Shell::Fish => {
                let p = fish_completion_path()?;
                let writer = create_completion_writer(&p)?;
                let note = format!(
                    "Generated completion file at {}. Fish loads this path automatically.",
                    p.display()
                );
                (p, writer, note)
            }
            Shell::PowerShell => {
                return Err(ApiError::InvalidInput(
                    "`jira completions powershell --install` is not supported. Redirect `jira completions powershell` into your PowerShell profile or completion path manually.".into(),
                )
                .into());
            }
            _ => {
                let shell_name = shell.to_string();
                return Err(ApiError::InvalidInput(format!(
                    "`jira completions {shell_name} --install` is not supported. Redirect `jira completions {shell_name}` into your shell completion path manually."
                ))
                .into());
            }
        };
        generate(shell, &mut cmd, bin_name, &mut writer);
        out.print_message(&note);
        out.print_message(&format!("Completion file path: {}", path.display()));
    } else {
        generate(shell, &mut cmd, bin_name, &mut io::stdout());
    }
    Ok(())
}

fn create_completion_writer(path: &std::path::Path) -> Result<Box<dyn std::io::Write>, ApiError> {
    let parent = path.parent().unwrap_or(path);
    std::fs::create_dir_all(parent)
        .map_err(|e| ApiError::Other(format!("cannot create {}: {e}", parent.display())))?;
    let file = std::fs::File::create(path)
        .map_err(|e| ApiError::Other(format!("cannot write {}: {e}", path.display())))?;
    Ok(Box::new(file) as Box<dyn std::io::Write>)
}

fn home_dir() -> Result<std::path::PathBuf, ApiError> {
    dirs::home_dir().ok_or_else(|| ApiError::Other("cannot determine home directory".into()))
}

fn bash_completion_path() -> Result<std::path::PathBuf, ApiError> {
    Ok(home_dir()?.join(".bash_completion.d").join("jira"))
}

fn zsh_completion_path() -> Result<std::path::PathBuf, ApiError> {
    Ok(home_dir()?.join(".zsh").join("completions").join("_jira"))
}

fn fish_completion_path() -> Result<std::path::PathBuf, ApiError> {
    #[cfg(target_os = "windows")]
    let base = dirs::config_dir().ok_or_else(|| {
        ApiError::Other("cannot determine config directory for fish completions".into())
    })?;

    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or(home_dir()?.join(".config"));

    Ok(base.join("fish").join("completions").join("jira.fish"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jira_cli::api::ApiError;
    use jira_cli::test_support::{
        EnvVarGuard, ProcessEnvLock, set_config_dir_env, unset_config_dir_env,
    };
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    #[test]
    fn parse_vec_update_arg_empty_is_none() {
        assert!(parse_vec_update_arg(&[]).is_none());
    }

    #[test]
    fn parse_vec_update_arg_none_sentinel_clears() {
        let values = vec!["none".to_string()];
        assert_eq!(parse_vec_update_arg(&values), Some(vec![]));
    }

    #[test]
    fn parse_vec_update_arg_values_pass_through() {
        let values = vec!["Backend".to_string(), "API".to_string()];
        assert_eq!(parse_vec_update_arg(&values), Some(vec!["Backend", "API"]));
    }

    #[test]
    fn parse_vec_update_arg_literal_none_at_position_0_with_more_values_does_not_clear() {
        // "none" is only a sentinel when it is the sole value. If accompanied by
        // other values it is treated as a literal string, not a clear instruction.
        let values = vec!["none".to_string(), "Backend".to_string()];
        assert_eq!(parse_vec_update_arg(&values), Some(vec!["none", "Backend"]));
    }

    #[test]
    fn vec_to_opt_refs_empty_is_none() {
        let values: Vec<String> = vec![];
        assert!(vec_to_opt_refs(&values).is_none());
    }

    #[test]
    fn vec_to_opt_refs_passes_through_values() {
        let values = vec!["a".to_string(), "b".to_string()];
        assert_eq!(vec_to_opt_refs(&values), Some(vec!["a", "b"]));
    }

    #[test]
    fn schema_does_not_advertise_nonexistent_token_flag() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let global_args = schema["global_args"].as_array().unwrap();
        assert!(
            !global_args.iter().any(|arg| arg["name"] == "--token"),
            "schema must not invent a --token CLI flag"
        );

        let auth_env = schema["auth"]["env"].as_array().unwrap();
        assert!(
            auth_env.iter().any(|entry| entry["name"] == "JIRA_TOKEN"),
            "schema must still document JIRA_TOKEN as an auth source"
        );
    }

    #[test]
    fn schema_has_clispec_version() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        assert_eq!(schema["clispec"].as_str(), Some("0.3"));
    }

    #[test]
    fn schema_has_global_args_with_type() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let global_args = schema["global_args"].as_array().unwrap();
        assert!(!global_args.is_empty(), "global_args must not be empty");
        for arg in global_args {
            assert!(
                arg["name"].as_str().is_some(),
                "every global_arg needs a name"
            );
            assert!(
                arg["type"].as_str().is_some(),
                "every global_arg needs a type: {arg}"
            );
        }
    }

    #[test]
    fn schema_has_errors_array() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let errors = schema["errors"].as_array().unwrap();
        assert!(!errors.is_empty(), "errors array must not be empty");
        for err in errors {
            assert!(err["kind"].as_str().is_some(), "every error needs a kind");
            assert!(
                err["exit_code"].as_u64().is_some(),
                "every error needs exit_code"
            );
        }
        // Check specific kinds exist
        let kinds: Vec<&str> = errors.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"auth"), "errors must include auth kind");
        assert!(
            kinds.contains(&"not_found"),
            "errors must include not_found kind"
        );
        assert!(
            kinds.contains(&"conflict"),
            "errors must include conflict kind (Principle 5: Idempotent Operations)"
        );
    }

    /// The published schema is the contract table verbatim. Reading it back
    /// this way means a kind can only reach agents by being reachable in the
    /// binary first, which is what `every_declared_error_kind_is_reachable`
    /// enforces on the table itself.
    #[test]
    fn schema_errors_match_the_error_contract() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let errors = schema["errors"].as_array().unwrap();

        assert_eq!(errors.len(), jira_cli::output::ALL_ERRORS.len());
        for (declared, contract) in errors.iter().zip(jira_cli::output::ALL_ERRORS) {
            assert_eq!(declared["kind"], contract.kind);
            assert_eq!(declared["exit_code"], contract.exit_code);
            assert_eq!(declared["retryable"], contract.retryable);
            assert_eq!(declared["description"], contract.description);
        }
    }

    /// Pinned because agents branch on these two numbers, so a reordering of
    /// the contract table must not silently renumber a failure mode.
    #[test]
    fn schema_declares_conflict_as_non_retryable_exit_seven() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let conflict = schema["errors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "conflict")
            .expect("schema must declare the conflict kind");

        assert_eq!(conflict["exit_code"], 7);
        assert_eq!(conflict["retryable"], false);
    }

    #[test]
    fn schema_all_commands_have_mutating() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let commands = schema["commands"].as_array().unwrap();
        for cmd in commands {
            assert!(
                cmd["mutating"].is_boolean(),
                "command '{}' must have mutating bool",
                cmd["name"]
            );
        }
    }

    /// `mutating` answers "does this change persistent state", which includes
    /// the local filesystem: `config init` writes a config file and declares
    /// itself mutating, and `download-attachment` writes a downloaded file. An
    /// agent uses the flag to decide whether a command is safe to run
    /// speculatively, so a command that can overwrite a local file must not
    /// advertise itself as read-only.
    #[test]
    fn schema_marks_commands_that_write_local_files_as_mutating() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let commands = schema["commands"].as_array().unwrap();
        let mutating_of = |name: &str| {
            commands
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("schema must declare '{name}'"))["mutating"]
                .as_bool()
                .unwrap()
        };

        assert!(
            mutating_of("issues download-attachment"),
            "download-attachment creates directories and overwrites files with --force"
        );
        assert!(
            mutating_of("config init"),
            "config init writes a config file; it is the precedent for the rule above"
        );
        assert!(
            !mutating_of("issues attachments"),
            "listing attachments writes nothing"
        );
    }

    #[test]
    fn schema_all_data_commands_declare_output() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let commands = schema["commands"].as_array().unwrap();
        for cmd in commands {
            if cmd["output_kind"] != "opaque" {
                assert!(
                    cmd["output_fields"].is_array() || cmd["stdout_schema"].is_object(),
                    "data command '{}' must declare output_fields or stdout_schema",
                    cmd["name"]
                );
            }
        }
    }

    /// Every output field, at any nesting depth, describes itself with a type
    /// drawn from one closed vocabulary. An agent branches on these strings, so
    /// a one-off spelling is as bad as a missing type: it silently falls through
    /// whatever match the agent wrote.
    #[test]
    fn schema_output_field_types_come_from_the_declared_vocabulary() {
        const TYPES: [&str; 6] = ["string", "integer", "number", "boolean", "object", "array"];

        fn check(fields: &serde_json::Value, path: &str) {
            for field in fields.as_array().unwrap_or_else(|| {
                panic!("{path}: `fields` must be an array; got {fields}");
            }) {
                let name = field["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: every field needs a name; got {field}"));
                let here = format!("{path}.{name}");
                let ty = field["type"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{here}: missing `type`"));
                assert!(
                    TYPES.contains(&ty),
                    "{here}: type '{ty}' is not one of {TYPES:?}"
                );
                for flag in ["nullable", "optional"] {
                    if !field[flag].is_null() {
                        assert!(
                            field[flag].is_boolean(),
                            "{here}: `{flag}` must be a boolean when present, got {}",
                            field[flag]
                        );
                    }
                }
                if !field["fields"].is_null() {
                    assert!(
                        ty == "object",
                        "{here}: only object types can carry nested `fields`, got '{ty}'"
                    );
                    check(&field["fields"], &here);
                }
            }
        }

        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        for cmd in schema["commands"].as_array().unwrap() {
            if cmd["output_kind"] != "opaque" {
                check(
                    &cmd["output_fields"],
                    cmd["name"].as_str().expect("command must have a name"),
                );
            }
        }
    }

    /// `config init` is described twice - once as `output_fields`, once as a
    /// worked `json_shape` example. Two descriptions of one object drift apart
    /// unless something ties them together, and a reader has no way to tell
    /// which of the two is current.
    #[test]
    fn schema_init_json_shape_and_output_fields_describe_the_same_keys() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();

        for name in ["config init", "init"] {
            let cmd = schema["commands"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("schema must declare '{name}'"));

            let declared: BTreeSet<&str> = cmd["output_fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["name"].as_str().unwrap())
                .collect();
            let in_example: BTreeSet<&str> = cmd["json_shape"]
                .as_object()
                .unwrap_or_else(|| panic!("{name} must carry a json_shape example"))
                .keys()
                .map(String::as_str)
                .collect();

            assert_eq!(
                declared, in_example,
                "{name}: output_fields and json_shape must describe the same keys"
            );
        }
    }

    #[test]
    fn schema_all_args_have_type() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let commands = schema["commands"].as_array().unwrap();
        for cmd in commands {
            if let Some(args) = cmd["args"].as_array() {
                for arg in args {
                    assert!(
                        arg["type"].as_str().is_some(),
                        "arg '{}' in command '{}' must have type",
                        arg["name"],
                        cmd["name"]
                    );
                }
            }
        }
    }

    #[test]
    fn schema_auth_describes_runtime_config_path_and_effective_requirements() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let auth = &schema["auth"];

        assert_eq!(
            auth["config_file"]["path"].as_str(),
            Some(jira_cli::config::schema_config_path().as_str())
        );
        assert_eq!(
            auth["config_file"]["description"].as_str(),
            Some(jira_cli::config::schema_config_path_description())
        );
        // email is not required when using PAT auth, so required_fields only
        // lists the fields that are always mandatory.
        assert_eq!(
            auth["required_fields"],
            serde_json::json!(["host", "token"])
        );
        assert!(
            auth["email_note"].as_str().is_some(),
            "schema must explain when email is required"
        );

        let auth_env = auth["env"].as_array().unwrap();
        assert!(
            auth_env.iter().all(|entry| entry["required"] == false),
            "individual env vars are optional auth sources, not mandatory on their own"
        );
    }

    #[test]
    fn schema_config_init_uses_platform_specific_bootstrap_guidance() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let _config_dir = unset_config_dir_env();
        let schema = schema_json();
        let config_init = schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|command| command["name"] == "config init")
            .unwrap();

        assert_eq!(
            config_init["json_shape"]["pathResolution"].as_str(),
            Some(jira_cli::config::schema_config_path_description())
        );
        assert_eq!(
            config_init["json_shape"]["recommendedPermissions"].as_str(),
            Some(jira_cli::config::schema_recommended_permissions_example())
        );
    }

    #[test]
    fn config_show_propagates_invalid_config_as_error() {
        let _env = ProcessEnvLock::acquire().unwrap();
        let dir = TempDir::new().unwrap();
        let _config_dir = set_config_dir_env(dir.path());
        let _host = EnvVarGuard::unset("JIRA_HOST");
        let _email = EnvVarGuard::unset("JIRA_EMAIL");
        let _token = EnvVarGuard::unset("JIRA_TOKEN");
        let _profile = EnvVarGuard::unset("JIRA_PROFILE");

        let err = jira_cli::config::show(&OutputConfig::new(true, false, true), None, None, None)
            .unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
    }

    #[test]
    fn parse_field_number_value() {
        let (key, val) = parse_field("customfield_10106=8").unwrap();
        assert_eq!(key, "customfield_10106");
        assert_eq!(val, serde_json::json!(8));
        assert!(val.is_number());
    }

    #[test]
    fn parse_field_float_value() {
        let (_key, val) = parse_field("customfield_10106=3.5").unwrap();
        assert_eq!(val, serde_json::json!(3.5));
    }

    #[test]
    fn parse_field_bool_value() {
        let (_, val) = parse_field("customfield_foo=true").unwrap();
        assert_eq!(val, serde_json::json!(true));
        let (_, val2) = parse_field("customfield_foo=false").unwrap();
        assert_eq!(val2, serde_json::json!(false));
    }

    #[test]
    fn parse_field_string_value() {
        let (key, val) = parse_field("customfield_10014=PROJ-1").unwrap();
        assert_eq!(key, "customfield_10014");
        assert_eq!(val, serde_json::json!("PROJ-1"));
        assert!(val.is_string());
    }

    #[test]
    fn parse_field_json_object_value() {
        let (_, val) = parse_field(r#"customfield_10080={"id":"10000"}"#).unwrap();
        assert_eq!(val["id"], "10000");
    }

    #[test]
    fn parse_field_json_array_value() {
        let (_, val) = parse_field(r#"labels=["backend","urgent"]"#).unwrap();
        assert_eq!(val[0], "backend");
        assert_eq!(val[1], "urgent");
    }

    #[test]
    fn parse_field_plain_string_with_spaces() {
        // A value that is not valid JSON falls back to a plain string
        let (_, val) = parse_field("summary=hello world").unwrap();
        assert_eq!(val, serde_json::json!("hello world"));
    }

    #[test]
    fn parse_field_missing_equals_returns_error() {
        let err = parse_field("noequalssign").unwrap_err();
        assert!(err.contains("key=value"));
    }

    #[test]
    fn parse_field_value_with_equals_in_it() {
        // split_once ensures only the first '=' splits key from value
        let (key, val) = parse_field("customfield_10014=A=B").unwrap();
        assert_eq!(key, "customfield_10014");
        assert_eq!(val, serde_json::json!("A=B"));
    }
}
