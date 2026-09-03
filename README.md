# jira

[![CI](https://github.com/rvben/jira-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/jira-cli/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/rvben/jira-cli/graph/badge.svg)](https://codecov.io/gh/rvben/jira-cli)

A fast, friendly Jira CLI for Jira Cloud and Jira Data Center / Server, built
to feel natural for people and predictable for agents.

- **Auto-JSON** when stdout is not a TTY, so you can pipe it anywhere and get structured data
- **`jira doctor`** verifies configuration, identity, project access, and write safety in one command
- **Command-scoped schema** gives agents one complete command contract without loading the full tree
- **Structured exit codes**, so agents can branch on auth failures, rate limits, not-found, and input errors without parsing text
- **Clean stdout/stderr split**: data on stdout, messages on stderr, `--quiet` suppresses all non-data output

```
$ jira issues list --project MYAPP --status "In Progress"
Key        Status       Assignee  Type  Summary
MYAPP-42   In Progress  Alice     Bug   Fix login redirect loop
MYAPP-38   In Progress  Bob       Task  Update password reset flow

$ jira issues list --project MYAPP --json
{"items": [...], "total": 2, "startAt": 0, "maxResults": 50}
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

Run `jira auth login` (or the shorter `jira init`) for guided setup. It opens
Atlassian's token page when useful, discovers the Cloud ID required by scoped
tokens, hides token entry, verifies the account, and stores the token in your
operating-system keychain. Existing profile values are reused safely. If no OS
credential service is available, setup offers an explicit protected-file
fallback rather than silently weakening storage.

For Jira Data Center, setup can create a dedicated PAT through Jira's official
API using a one-time password or existing PAT. That bootstrap credential is
never saved. When no terminal is available, `jira init --json` returns setup
instructions; CI can use the environment variables below directly.

**Default locations:**

| Platform | Path |
|----------|------|
| Linux / macOS | `~/.config/jira/config.toml` (or `$XDG_CONFIG_HOME/jira/config.toml`) |

```toml
[default]
host  = "mycompany.atlassian.net"
email = "me@example.com"
credential_store = "keyring"
cloud_id = "your-atlassian-cloud-id"
token_kind = "scoped"
expires_at = "2026-11-24"
read_only = true
```

Get a Jira Cloud API token at: https://id.atlassian.com/manage-profile/security/api-tokens

Run `jira auth status` to verify the active credential, `jira auth status
--offline` to inspect local credential state without a request, and `jira
doctor` for the complete connection. `jira config show` displays resolved
settings and the credential source; `jira config path` prints the backing file.
Existing configs with inline tokens remain readable; move one into the keychain
with `jira auth migrate`.

### Environment variables

All credentials can be set via environment variables, which is useful for CI and scripts:

| Variable | Description |
|----------|-------------|
| `JIRA_HOST` | Atlassian domain (e.g. `mycompany.atlassian.net`) |
| `JIRA_EMAIL` | Account email |
| `JIRA_TOKEN` | API token or Personal Access Token |
| `JIRA_PROFILE` | Config profile name |
| `JIRA_AUTH_TYPE` | `basic` (default) or `pat` |
| `JIRA_API_VERSION` | `3` (Cloud, default) or `2` (Data Center / Server) |
| `JIRA_CLOUD_ID` | Atlassian Cloud ID required by a scoped token |
| `JIRA_TOKEN_KIND` | `scoped` or `classic` |
| `JIRA_READ_ONLY` | Block write operations. On: `1`, `true`, `yes`, `on`. Off: `0`, `false`, `no`, `off`. Any other value is an error, not "off" |
| `JIRA_DEBUG_HTTP` | Include the raw Jira response body in API error messages (`1`, `true`, `yes`, `on`). Useful when the default summary is ambiguous. |

Values are matched case-insensitively. `JIRA_AUTH_TYPE` and `JIRA_API_VERSION` reject anything outside the values listed above rather than falling back to the default, so a typo surfaces as a config error instead of an unexplained authentication failure.

### Multiple profiles

```toml
[default]
host  = "mycompany.atlassian.net"
email = "me@example.com"
credential_store = "keyring"
cloud_id = "your-atlassian-cloud-id"
token_kind = "scoped"

[profiles.dc]
host        = "jira.corp.com"
credential_store = "keyring"
auth_type   = "pat"
api_version = 2
```

Switch with `--profile dc` or `JIRA_PROFILE=dc jira <command>`.

### Jira Data Center / Server (PAT auth)

Data Center uses Personal Access Tokens instead of email + API token:

```toml
[default]
host        = "jira.corp.com"
credential_store = "keyring"
auth_type   = "pat"
api_version = 2
```

Email is not required for PAT auth. `jira auth login` can create and save a
dedicated PAT automatically, or open the manual token page:
`https://<your-host>/secure/ViewProfile.jspa`. From there, choose
**Personal access tokens**. Jira's direct selected-tab URL varies by release.

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
jira issues download-attachment 10042 --dir ./downloads --force  # overwrite an existing file
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
jira init                    # guided setup that verifies credentials before saving
jira doctor                  # verify config, auth, projects, and write safety
jira doctor --offline        # inspect configuration without contacting Jira
jira auth status             # verify the selected credential
jira auth status --offline   # inspect local credential state only
jira profile list            # list profiles and show the active one
jira profile use work        # make work the active profile
jira profile remove old --yes
jira config show             # resolved credentials (token masked)
jira config path             # resolved config file location
jira config init             # same as jira init
```

## Agent use

Use `jira schema --command` when an agent knows which operation it needs. The
compact response includes that command's arguments, effects, pagination,
output fields, global flags, and error contract. Use the full `jira schema`
document only for discovery across the complete command tree.

```sh
jira schema --command 'issues list'
jira schema --command 'issues transition'
jira schema | jq '.commands[] | .name'
```

### Read-only mode

Set `JIRA_READ_ONLY=1` to block every command that writes to Jira. The CLI returns exit code 2 with a structured error for any blocked command, before it opens a connection. This is useful when giving an AI agent read access to Jira without the risk of unintended modifications.

The guard covers writes to Jira, not writes to your disk: `jira init`, `jira config init`, `jira config remove` and `jira issues download-attachment` still work, because they change local files only.

`jira schema` lists the blocked commands under `read_only.blocked_commands`, so an agent can see what it is allowed to do without trying:

```sh
jira schema | jq '.read_only'
```

A value the CLI does not recognise (`JIRA_READ_ONLY=enabled`, or a typo) is rejected as a config error rather than read as "off", so a mis-set guard fails loudly instead of quietly allowing writes.

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
| 7 | Target already exists (pass `--force` to overwrite) |

A downstream that stops reading, as in `jira issues list | head -5`, terminates
the CLI with `SIGPIPE` rather than any of these codes. That is what every other
member of a pipeline does, and shells report it as 141.

## Output flags

| Flag | Effect |
|------|--------|
| `--json` | Force JSON output (auto when stdout is not a TTY) |
| `--quiet` | Suppress counts, confirmations, and status messages |
| `--no-color` | Disable ANSI color (`NO_COLOR` is also honored) |

These flags are available on every command. `--json` is a compatibility alias for `--output json`; use `--output text` to force human-readable output in a pipeline.

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

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
