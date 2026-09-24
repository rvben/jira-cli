# jira

[![CI](https://github.com/rvben/jira-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/jira-cli/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/rvben/jira-cli/graph/badge.svg)](https://codecov.io/gh/rvben/jira-cli)

A fast, friendly Jira CLI for Jira Cloud and Jira Data Center / Server, built
to feel natural for people and predictable for agents.

- **Auto-JSON** when stdout is not a TTY, so you can pipe it anywhere and get structured data
- **`jira doctor`** verifies configuration, the site's deployment (Cloud or Data Center, with the matching auth and API version), identity, project access, and write safety in one command
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

Run `jira auth login` (or the shorter `jira init`) for guided setup. Enter your
Cloud subdomain, your site's address, or paste any link from Jira; setup asks the
site whether it runs Jira Cloud or Data Center, so there is no deployment type or
API version to choose. It offers to open the token page, discovers the Cloud ID required by scoped
tokens, hides token entry, verifies the account and its project access, and stores
the token in your operating-system keychain. Existing profile values are reused
safely. If no OS credential service is available, setup offers an explicit
protected-file fallback rather than silently weakening storage.

A scoped Cloud token needs these scopes:

| Access | Scopes |
|--------|--------|
| Read-only | `read:jira-work`, `read:jira-user` |
| Read-write | the read-only scopes plus `write:jira-work` |
| Boards and sprints, added | `read:board-scope:jira-software`, `read:project:jira`, `read:issue-details:jira`, `read:sprint:jira-software`, plus `write:sprint:jira-software` to move issues into sprints |

Jira's board and sprint API accepts only the granular scopes in the last row, so
a token with just the classic scopes works for every command except `boards` and
`sprints`. Setup prints the list for the access you choose, and
`jira init --json` returns it as `cloudTokenScopes`.

For Jira Data Center, setup can create a dedicated PAT through Jira's official
API using a one-time password or existing PAT. That bootstrap credential is
never saved. When no terminal is available, `jira init --json` returns setup
instructions, and `jira init --json --host <site>` also asks the site whether it
runs Cloud or Data Center and returns the one page where its token is created.
CI can use the environment variables below directly.

To save a profile without prompts, pipe the token to `jira auth login --with-token`:

```bash
jira auth login --with-token --host acme --email me@example.com < token.txt
jira --profile dc auth login --with-token --host jira.example.com --read-only < pat.txt
jira --profile work auth login --with-token < new-token.txt   # rotate: only the token changes
```

It identifies the site's deployment the same way, verifies the token and project
access, and saves nothing if a check fails. Settings you leave out come from the
profile being replaced; its email and token kind only while the site stays the
same, since they belong to that site's account. A new Cloud profile expects a scoped token; pass
`--token-kind classic` for one created without scopes. `--credential-store file`
keeps the token in the protected config file where no OS keychain is available.

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
jira issues list --fields key,summary,parent,epic

# Show
jira issues show MYAPP-123

# Discover the create screen before choosing fields
jira issues create-meta -p MYAPP             # available issue types
jira issues create-meta -p MYAPP -t Story    # required fields, defaults, options, epic support

# Create
jira issues create --project MYAPP --summary "Fix login bug" --type Bug
jira issues create --project MYAPP --summary "Add dark mode" --type Story \
  --description "Users want a dark mode option." --priority High --assignee me
jira issues create --project MYAPP --summary "Write unit tests" --type Subtask \
  --parent MYAPP-42                           # creates a subtask
jira issues create --project MYAPP --summary "Add dark mode" --type Story \
  --epic MYAPP-10 --priority Medium           # resolves this project's fields

# Preview normalized fields without making changes
jira issues create -p MYAPP -t Story -s "Example" --priority Medium --dry-run
jira issues update MYAPP-123 --priority Medium --dry-run
jira issues move MYAPP-123 --sprint active --dry-run

# Update
jira issues update MYAPP-123 --summary "Updated title"
jira issues update MYAPP-123 --priority Low --assignee me
jira issues update MYAPP-123 --field customfield_10016=5
jira issues update MYAPP-123 --epic MYAPP-10
jira issues update MYAPP-123 --clear-epic
jira issues update MYAPP-123 --type Story     # Task -> Story, same hierarchy level
jira issues update MYAPP-123 --assignee none

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
jira issues move MYAPP-123 --sprint active --board 42

# Bulk operations (use --dry-run to preview)
jira issues bulk-transition --jql 'project = MYAPP AND status = "To Do"' --to "In Progress"
jira issues bulk-transition --jql 'project = MYAPP AND status = "To Do"' --to "In Progress" --dry-run
jira issues bulk-assign --jql 'project = MYAPP AND sprint in openSprints()' --assignee me
```

`--epic KEY` resolves the instance's Epic Link custom field or native `parent`
field using create metadata (edit metadata for updates). `--parent KEY` also
uses epic linkage when its target is an Epic; other targets retain normal
parent semantics. Metadata validates the parent/type hierarchy before creation
and lists the project's subtask types when needed. `--epic` and `--parent`
cannot be combined. `issues update --clear-epic` removes epic membership,
resolving the same native or custom field. It conflicts with `--epic` and
explicit relationship overrides. Subtasks belong under a Story or Task, not directly under
an epic. Jira's Cloud and Server/DC metadata formats are supported, including
[older Server metadata](https://developer.atlassian.com/server/jira/platform/jira-rest-api-examples/).

Issue JSON from `issues show`, `issues list`, `issues mine` and `search`
includes `parent` (the direct parent's key, summary and type, or `null`) and
`epic` (the key of the epic the issue belongs to directly, or `null`). On Cloud
`epic` is the parent when that parent sits at the epic hierarchy level. On Data
Center it is the Epic Link field, so a subtask created directly under an epic
shows that epic as `parent` with `epic: null`. `--fields` rejects names that are
not output fields and lists the valid ones.

`issues update --type` changes an issue's type by name or ID within one
hierarchy level, for example Task to Story. Jira applies it only when both
types share a workflow and field configuration. Changing a subtask into a
standard issue (or back), or moving across hierarchy levels such as Story to
Epic, is refused: the Jira edit API cannot do it, so use **More > Move** in the
Jira web UI. On Data Center the flag needs Jira 9.10.0 or later, because older
releases accept an incompatible type change and leave the issue in an invalid
workflow state. Data Center has no hierarchy levels, so Jira Software is asked
whether the issue is an epic, and a target type counts as an epic when its
create screen carries the Epic Name field. A renamed epic type whose create
screen lacks Epic Name is therefore not recognized as a target. Combined with
`--epic`, epic eligibility is checked against the new type.

Priority matching uses the allowed values for the project and issue type, or
the existing issue's edit metadata. Exact names and IDs take precedence, then
case-insensitive names, labels with a numeric rank removed (`Medium` matches
`3 - Medium`), and unique prefixes (`Med`). Invalid or ambiguous input lists
the valid choices without creating or updating an issue. Creation also resolves
issue type names case-insensitively or by ID. Omitting `--priority` keeps Jira's
default. If metadata endpoints are unavailable, names are passed through to
Jira and validation errors include guidance; explicit `--field` values retain
their override behavior. Epic linkage requires a discoverable field or native
Cloud parent support.

Components and fix versions accept exact names, IDs, case-insensitive names,
and unique prefixes from create/edit metadata. Invalid or ambiguous values list
valid options before writing. Required fields without a server default must be
provided when creating an issue. Updates leave omitted fields untouched and
reject explicit clears of required fields. `--field ID=VALUE` can supply required
custom values. Unavailable metadata endpoints retain the existing pass-through
behavior; authentication and server failures are still reported.

`--assignee none` and `--assignee unassign` explicitly leave a new issue unassigned
or clear an existing assignee. Omitting the flag preserves Jira's create default
or the existing assignee. These aliases also work with `issues assign`.

On `issues create` and `issues move`, sprint names and `active` are scoped to the
issue project's Scrum boards. `--board ID` overrides this scope. Exact names
win over substring matches; multiple matches list sprint and board IDs instead
of picking one. Numeric sprint IDs identify a sprint directly; if `--board` is
also supplied, membership on that board is checked.

Creation resolves the sprint and checks that it is active or future before
creating the issue. If the subsequent move fails, the CLI exits with code `8`
and error kind `partial_success`. JSON stderr includes `error.details.key`,
`url`, `created`, `sprintId`, `sprintMoved`, and `recoveryCommand`. The error is
not retryable as a whole: use the returned `jira issues move KEY --sprint ID`
command to finish the operation. Rerunning `issues create` would create a duplicate.

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

### Write previews and bulk outcomes

Create and update previews include the exact normalized `fields` payload;
move previews identify the resolved sprint. `steps` lists the planned writes
in order, including the sprint move after creation. Previews share the real
write's normalization and preflight validation. Jira can still reject a later
write because permissions, workflow or plugin validators, or server state
changed. `metadata` and `warnings` identify checks that could not be completed.

`issues create-meta` lists types when `--type` is omitted. With a type it
returns the create screen's field definitions, including required fields,
defaults and allowed values, plus epic support. `fields: null` means the fields
were not requested or could not be discovered; warnings distinguish the
latter. Epic support describes evidence from that screen; when metadata is
unavailable, a write can still attempt the documented instance fallback.
Unsupported create-meta endpoints return error details with `reason: "unsupported"`
after confirming the project exists. `allowedValues: null` means unknown, while `[]` means no allowed values.

Transitions accept an exact ID, a case-insensitive action name, or a
case-insensitive destination status, in that order. Ambiguity is an error;
the structured error details list candidates and a discovery command.

Bulk commands emit a summary even for zero matches. Any per-issue failure
returns exit 9 (`bulk_failure`); stdout still contains the full summary.
`total = succeeded + ready + failed + notAttempted`, where `ready` counts
dry-run entries that passed available checks and `succeeded` counts completed
writes. Assignment previews resolve the assignee but do not check per-issue
assignability; transition previews resolve the transition for each issue. Issue-local
failures do not discard or replay completed work. Authentication/permission,
rate-limit, network, and server failures stop further requests and mark the
remaining issues `notAttempted`. Each failure includes `errorKind` and
`retryable`, plus the failed `phase` (`lookup` or `write`). Write timeouts,
connection failures, and server errors are marked `outcome: "unknown"`: the
server may have applied the write. These count under `failed` because success
was not confirmed; check issue state before retrying them. Never blindly rerun a partially completed bulk command.

### Boards and sprints

```sh
jira boards list
jira boards list --project MYAPP
jira sprints list --project MYAPP            # current (active) sprints
jira sprints list --project MYAPP --state active,future
jira sprints list --board "MYAPP board"
jira sprints list --board 42 --state all
```

`--project` filters boards on the server. For sprint listing, a project and
board filter intersect; a numeric board ID without a project is fetched
directly. Kanban boards are skipped in broad discovery and rejected when
they are the only boards selected by a filter. Unknown board types are queried;
boards Jira explicitly reports as not supporting sprints are skipped, with
messages in the top-level `warnings` array. Other errors still fail the command. Shared sprints appear once, with every matching board in
`boards`; `total` counts unique sprints. The primary `boardId`/`boardName` is the
origin board when matched, otherwise the lowest matched ID. Results and board
context are ordered by ID. Repeat `--state` or comma-separate its values;
`all` disables the state filter.

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
jira doctor                  # verify config, deployment, auth, projects, and write safety
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

Argument types, enum values, `conflicts_with`, and `requires` reflect the CLI
parser. Commands with previews publish their conditional contract under
`x-dry-run`: `arg`, `effects`, and `output_fields`. Those fields describe the
preview; the command's ordinary `output_fields` describe the real result.
Create, update, and move also publish a complete `stdout_schema` covering both
results. Transitions are declared non-idempotent because repeating a workflow
action can trigger additional effects.
Bulk commands also declare `x-output-on-error` so consumers know to parse
stdout when the exit code is nonzero.

### Read-only mode

Set `JIRA_READ_ONLY=1` to block every command that writes to Jira. The CLI returns exit code 2 with a structured error for any blocked command, before it opens a connection. This is useful when giving an AI agent read access to Jira without the risk of unintended modifications.

`--dry-run` remains available for create, update, move, and both bulk commands in read-only mode. These commands resolve and validate inputs without writing to Jira; the HTTP client also blocks write requests as a second check.

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
| 7 | Conflict, including a target file that already exists |
| 8 | Issue created, but sprint assignment failed; use the recovery command in the error details |
| 9 | Bulk operation has failures; parse the complete per-issue summary on stdout |

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
