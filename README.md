# jira

[![CI](https://github.com/rvben/jira-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/jira-cli/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/rvben/jira-cli/graph/badge.svg)](https://codecov.io/gh/rvben/jira-cli)

An agent-friendly Jira CLI for Jira Cloud and Jira Data Center / Server.

- **Auto-JSON** when stdout is not a TTY — pipe it anywhere, get structured data
- **`jira schema`** dumps every command, flag, and JSON shape as machine-readable JSON for agent introspection
- **Structured exit codes** — agents can branch on auth failures, rate limits, not-found, and input errors without parsing text
- **Clean stdout/stderr split** — data on stdout, messages on stderr, `--quiet` suppresses all non-data output

```
$ jira issues list --project MYAPP --status "In Progress"
KEY          SUMMARY                        STATUS       ASSIGNEE
MYAPP-42     Fix login redirect loop        In Progress  Alice
MYAPP-38     Update password reset flow     In Progress  Bob

$ jira issues list --project MYAPP --json
{"total": 2, "issues": [...]}
```

## Installation

```sh
uv tool install jira-cli-rs
```

Or via Cargo:

```sh
cargo install jira-cli
```

Or build from source:

```sh
git clone https://github.com/rvben/jira-cli
cd jira-cli
make install          # runs check + release build, copies to ~/.local/bin/jira
```

## Configuration

Run `jira init` for a guided setup, or create the config file manually.

**Default locations:**

| Platform | Path |
|----------|------|
| Linux / macOS | `~/.config/jira/config.toml` (or `$XDG_CONFIG_HOME/jira/config.toml`) |

```toml
[default]
host  = "mycompany.atlassian.net"
email = "me@example.com"
token = "your-api-token"
```

Get a Jira Cloud API token at: https://id.atlassian.com/manage-profile/security/api-tokens

```sh
chmod 600 ~/.config/jira/config.toml
```

Run `jira config show` to confirm the resolved path and active credentials (token is masked).

### Environment variables

All credentials can be set via environment variables — useful for CI and scripts:

| Variable | Description |
|----------|-------------|
| `JIRA_HOST` | Atlassian domain (e.g. `mycompany.atlassian.net`) |
| `JIRA_EMAIL` | Account email |
| `JIRA_TOKEN` | API token or Personal Access Token |
| `JIRA_PROFILE` | Config profile name |
| `JIRA_AUTH_TYPE` | `basic` (default) or `pat` |
| `JIRA_API_VERSION` | `3` (Cloud, default) or `2` (Data Center / Server) |
| `JIRA_READ_ONLY` | Block write operations (`1`, `true`, `yes`, `on`) |
| `JIRA_DEBUG_HTTP` | Include the raw Jira response body in API error messages (`1`, `true`, `yes`). Useful when the default summary is ambiguous. |

### Multiple profiles

```toml
[default]
host  = "mycompany.atlassian.net"
email = "me@example.com"
token = "cloud-token"

[profiles.dc]
host        = "jira.corp.com"
token       = "personal-access-token"
auth_type   = "pat"
api_version = 2
```

Switch with `--profile dc` or `JIRA_PROFILE=dc jira <command>`.

### Jira Data Center / Server (PAT auth)

Data Center uses Personal Access Tokens instead of email + API token:

```toml
[default]
host        = "jira.corp.com"
token       = "your-personal-access-token"
auth_type   = "pat"
api_version = 2
```

Email is not required for PAT auth. Get your token at:
`https://<your-host>/secure/ViewProfile.jspa?selectedTab=com.atlassian.pats.pats-plugin:jira-user-personal-access-tokens`

## Usage

### Issues

```sh
# List
jira issues list
jira issues list --project MYAPP --status "In Progress"
jira issues list --project MYAPP --type Bug --assignee me
jira issues list --sprint active
jira issues list --all                        # fetch every page

# Assigned to you
jira issues mine
jira issues mine --project MYAPP --status "To Do"

# Show
jira issues show MYAPP-123

# Create
jira issues create --project MYAPP --summary "Fix login bug" --type Bug
jira issues create --project MYAPP --summary "Add dark mode" --type Story \
  --description "Users want a dark mode option." --priority High --assignee me
jira issues create --project MYAPP --summary "Write unit tests" \
  --parent MYAPP-42                           # creates a subtask

# Update
jira issues update MYAPP-123 --summary "Updated title"
jira issues update MYAPP-123 --priority Low --assignee me
jira issues update MYAPP-123 --field customfield_10016=5

# Transition
jira issues list-transitions MYAPP-123
jira issues transition MYAPP-123 --to "In Review"

# Assign
jira issues assign MYAPP-123 --assignee me
jira issues assign MYAPP-123 --assignee user@example.com

# Comment
jira issues comment MYAPP-123 --body "Deployed to staging."
jira issues comments MYAPP-123

# Log work
jira issues log-work MYAPP-123 --time-spent 2h
jira issues log-work MYAPP-123 --time-spent 30m --comment "Fixed the flaky test"

# Attachments
jira issues attachments MYAPP-123
jira issues attach MYAPP-123 --file ./design.png --file ./spec.pdf
jira issues download-attachment 10042 --dir ./downloads
jira issues delete-attachment 10042

# Links
jira issues link-types
jira issues link MYAPP-123 --to MYAPP-456 --type "Blocks"
jira issues unlink <link-id>

# Move to sprint
jira issues move MYAPP-123 --sprint active
jira issues move MYAPP-123 --sprint "Sprint 14"

# Bulk operations (use --dry-run to preview)
jira issues bulk-transition --jql 'project = MYAPP AND status = "To Do"' --to "In Progress"
jira issues bulk-transition --jql 'project = MYAPP AND status = "To Do"' --to "In Progress" --dry-run
jira issues bulk-assign --jql 'project = MYAPP AND sprint in openSprints()' --assignee me
```

### Projects

```sh
jira projects list
jira projects show MYAPP
```

### Search

```sh
jira search 'project = MYAPP AND sprint in openSprints() ORDER BY priority'
jira search 'assignee = currentUser() AND status != Done' --limit 20
jira search 'project = MYAPP' --all                       # fetch every page
```

### Boards and sprints

```sh
jira boards list
jira sprints list
jira sprints list --board "MYAPP board"
```

### Users and fields

```sh
jira users search --query "alice"
jira fields list
jira fields list --custom                     # custom fields only
```

### Shell completions

```sh
# Install automatically (bash, zsh, fish)
jira completions bash --install
jira completions zsh --install
jira completions fish --install

# Or redirect manually
jira completions zsh > ~/.zsh/completions/_jira
```

### Config

```sh
jira init                    # setup guide with example config and token URLs
jira config show             # resolved credentials (token masked)
jira config init             # same as jira init
```

## Agent use

`jira schema` returns a complete, machine-readable description of all commands, flags, JSON output shapes, auth requirements, and exit codes. AI agents should call this once at the start of a session instead of relying on help text.

```sh
jira schema | jq '.commands[] | .name'
jira schema | jq '.commands[] | select(.name == "issues list")'
```

### Read-only mode

Set `JIRA_READ_ONLY=1` to block all write operations (create, update, transition, comment, assign, etc.). The CLI will return exit code 2 with a clear error message for any blocked command. This is useful when giving an AI agent read access to Jira without the risk of unintended modifications.

You can set it in the config file:

```toml
[default]
read_only = true
```

Or per-profile:

```toml
[profiles.agent]
read_only = true
```

When giving an AI agent access to the CLI, set the env var in the agent's configuration. For example, in Claude Code's `.claude/settings.json`:

```json
{
  "env": {
    "JIRA_READ_ONLY": "1"
  }
}
```

Any agent that supports environment variable configuration can use the same approach.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Unexpected error |
| 2 | Bad input or config error |
| 3 | Authentication failed |
| 4 | Resource not found |
| 5 | Jira API error |
| 6 | Rate limited |

## Output flags

| Flag | Effect |
|------|--------|
| `--json` | Force JSON output (auto when stdout is not a TTY) |
| `--quiet` | Suppress counts, confirmations, and status messages |

Both flags are available on every command.

## Development

```sh
make build          # debug build
make check          # fmt check + clippy + tests (run before committing)
make test           # unit + integration tests (wiremock, no real Jira needed)
make lint           # fmt check + clippy
make fmt            # auto-format
make install        # check + release build + copy to ~/.local/bin/jira
```

### Running e2e tests

The e2e test suite runs against a real Jira instance. A Jira Data Center
instance is required (Data Center license needed):

```sh
make jira-start     # start local Jira via Docker
make jira-wait      # wait until Jira is ready (~2 min on first run)

JIRA_E2E_HOST=http://localhost:8080 \
JIRA_E2E_EMAIL=admin \
JIRA_E2E_TOKEN=mytoken \
JIRA_E2E_PROJECT=TST \
  make test-e2e

make jira-stop
```

All e2e tests tag created issues with `[e2e-auto]` for easy cleanup.

### CI

GitHub Actions runs `fmt → clippy → nextest` on Ubuntu and macOS for every
push and pull request. The workflow is at `.github/workflows/ci.yml`.

## License

MIT
