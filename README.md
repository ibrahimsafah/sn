# sn

[![CI](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/ci.yml)
[![Security](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/security.yml/badge.svg)](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/security.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/tehubersheezy/servicenow-cli/badge)](https://scorecard.dev/viewer/?uri=github.com/tehubersheezy/servicenow-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Latest release](https://img.shields.io/github/v/release/tehubersheezy/servicenow-cli?display_name=tag&sort=semver)](https://github.com/tehubersheezy/servicenow-cli/releases/latest)

A fast, single-binary CLI for ServiceNow, built for LLM agents and human operators alike.

It wraps ServiceNow's REST APIs — Table, Change Management, Attachment, CMDB, Import Set, Service Catalog, Identification & Reconciliation, CICD, Aggregate, Performance Analytics, and schema discovery — behind one predictable interface: stable JSON on stdout, structured errors on stderr, and deterministic exit codes.

```bash
sn init                                      # connect to an instance
sn table list incident --setlimit 5          # read records as JSON
sn table create incident --field short_description="Disk full on prod-db-01"
sn ping                                       # auth + latency health check
```

> ### 🆕 What's new in 0.10.0 — `sn watch`
>
> **[`sn watch`](#watching-records-live)** streams record changes over ServiceNow's AMB
> websocket as they happen, as JSONL on stdout, one event per line. Each event carries the
> fields that changed and their new values.
>
> ```bash
> sn watch table incident --query "priority=1^active=true" --max-events 5
> sn watch table incident --sys-id <SYS_ID> --on-change state --duration 60
> ```
>
> Filter with `--operation` and `--on-change`; bound the stream with `--max-events`,
> `--duration` and `--idle-timeout`. Add `--hydrate` to also fetch the fields that
> *didn't* change.
>
> Full [changelog](CHANGELOG.md).

## Contents

- [Installation](#installation)
- [Setup](#setup)
  - [Basic auth](#basic-auth)
  - [Non-interactive setup (CI, containers, agents)](#non-interactive-setup-ci-containers-agents)
  - [OAuth / SSO](#oauth--sso)
- [Usage](#usage)
  - [Reading records](#reading-records)
  - [Writing records](#writing-records)
  - [Pagination](#pagination)
  - [Watching records (live)](#watching-records-live)
  - [Schema discovery](#schema-discovery)
  - [Aggregate queries](#aggregate-queries)
  - [Change Management](#change-management)
  - [Attachments](#attachments)
  - [CMDB](#cmdb)
  - [Import Sets](#import-sets)
  - [Service Catalog](#service-catalog)
  - [Identification & Reconciliation](#identification--reconciliation)
  - [CICD operations](#cicd-operations)
  - [Performance Analytics scorecards](#performance-analytics-scorecards)
  - [Inspect and connect](#inspect-and-connect)
  - [Open a record in the web UI](#open-a-record-in-the-web-ui)
  - [Raw REST passthrough](#raw-rest-passthrough)
  - [Human-readable table output](#human-readable-table-output)
  - [Shell completions](#shell-completions)
  - [Agent integration](#agent-integration)
- [Output contract](#output-contract)
  - [Exit codes](#exit-codes)
- [Parameters](#parameters)
- [Configuration](#configuration)
  - [Environment variables](#environment-variables)
- [Proxy and TLS](#proxy-and-tls)
- [Debugging](#debugging)
- [License](#license)

## Installation

### Homebrew (macOS / Linux)

```bash
brew install tehubersheezy/sn/sn
# or: brew tap tehubersheezy/sn && brew install sn   (upgrade later with: brew upgrade sn)
```

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tehubersheezy/servicenow-cli/releases/latest/download/sn-installer.sh | sh
```

### Windows (MSI or PowerShell)

Download `sn-x86_64-pc-windows-msvc.msi` (64-bit Intel/AMD) or `sn-aarch64-pc-windows-msvc.msi` (ARM64 — Surface Pro X, Copilot+ PCs) from the [latest release](https://github.com/tehubersheezy/servicenow-cli/releases/latest) and double-click. For unattended/SCCM/Intune deployment use `msiexec /i sn-x86_64-pc-windows-msvc.msi /qn`. Or install via PowerShell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/tehubersheezy/servicenow-cli/releases/latest/download/sn-installer.ps1 | iex"
```

### Pre-built binaries

Download from [Releases](https://github.com/tehubersheezy/servicenow-cli/releases): Linux (x86_64, ARM64), macOS (Intel, Apple Silicon), and Windows (x86_64, ARM64) — the latter as a portable `.zip` (no install) or `.msi` installer.

## Setup

`sn` supports **basic auth** (username + password) for most instances, and **OAuth / SSO** for instances fronted by an external identity provider (Okta, Azure AD, ADFS), where the password lives in the IdP and basic auth cannot work.

### Basic auth

```bash
sn init
# Profile name [default]:
# Instance (e.g. 'dev380385' or 'https://acme.service-now.com'): mycompany.service-now.com
# Auth method (basic/oauth) [basic]:
# Username: admin
# Password: ********
# profile 'default' saved and verified (mycompany.service-now.com).
# 'default' is now the default profile.
```

`sn init` is the onboarding command: it sets up a profile **and makes it the default**. To add
further instances without disturbing which profile your commands currently use, reach for
`sn profile add`:

```bash
sn profile add prod --instance prod.service-now.com --username svc-user --auth basic   # prompts for the password
sn --profile prod table list incident --setlimit 5
sn profile use prod                  # make it the default, when you're ready
```

(Omit `--auth basic` and it prompts for the auth method too. Any field you don't pass, it asks for —
on a terminal. Off one, it fails naming the flag instead. See below.)

### Non-interactive setup (CI, containers, agents)

`sn profile add` is built to be scripted. It never prompts when stdin isn't a terminal — it fails
naming the flag it needed — so it cannot hang a pipeline. Pipe the password in rather than passing
`--password`, which is visible in `ps` output and shell history:

```bash
sn profile add ci --instance acme.service-now.com --username svc-user --password-stdin < secret.txt
# → {"auth":"basic","default":false,"instance":"acme.service-now.com","next":"sn profile use ci",
#    "ok":true,"profile":"ci","user":"svc-user","verified":true}
```

Keys come back sorted. `user` is the identity the instance resolved the credentials to — worth
asserting on in CI, since it catches a service account being silently swapped out.

It always checks the credentials against the instance, and **a profile that fails the check is not
written at all** — no half-configured identity to trip over later. Pass `--no-verify` to register a
profile without touching the network (air-gapped provisioning, or config management that runs
before the instance is reachable).

`"next"` appears only when there's something to do about it — above, that no default profile is
selected yet, so `ci` needs `sn profile use ci` or an explicit `--profile ci`.

`add` creates; it will not silently overwrite an identity you or a teammate may be relying on:

| | |
|---|---|
| profile already exists | exit 1 — pass `--force` to overwrite |
| required flag missing, no TTY | exit 1, naming the flag |
| credentials rejected | exit 4, nothing written |
| `--non-interactive` | never prompt, even on a terminal — fail naming the flag |
| `--set-default` | also make it the default (otherwise `add` leaves it alone) |

### OAuth / SSO

Configure the profile with `sn init --auth oauth` (or `sn profile add --auth oauth`), then run the
flow with `sn auth login`:

```bash
# Authorization-code + PKCE (default): a PUBLIC client — no secret needed or prompted for.
sn init --profile sso --auth oauth --instance acme.service-now.com --client-id <id>

# Non-interactive server-to-server: client_credentials is a CONFIDENTIAL client and needs a secret
# (prompted if --client-secret is omitted; --client-secret-stdin keeps it out of `ps`).
sn profile add svc --auth oauth --instance acme.service-now.com \
  --grant client_credentials --client-id <id> --client-secret-stdin < secret.txt

sn --profile sso auth login          # run the OAuth flow, cache tokens
```

The two grants differ in whether they can be set up headlessly. `client_credentials` mints a token
without a browser, so `sn profile add` verifies it like any other credential. `authorization_code`
**requires** a browser, so there is nothing for `sn profile add` to test on a machine that has none:
it refuses rather than save an untested profile. Pass `--no-verify` to register it anyway, then have
a human run `sn auth login`.

**One-time admin setup** (if the instance has no registry entry yet): **System OAuth → Application Registry → New → "Create an OAuth API endpoint for external clients"**; set the redirect URL to `http://localhost:8400/callback` — which must match `--redirect-uri` **exactly** — and copy the client ID. For the default authorization-code flow, enable **Public Client / PKCE required** so no secret is needed; only `client_credentials` needs the generated secret.

After login, tokens refresh transparently. Manage the session with `sn auth status` (method + token expiry), `sn auth refresh`, and `sn auth logout`. The client ID and redirect URI live in `config.toml`; the secret and tokens in `credentials.toml` (chmod 600).

Verify either auth method at any time with `sn ping`.

## Usage

### Reading records

```bash
# List incidents (default: up to 1000 records)
sn table list incident

# Filter, select fields, limit
sn table list incident --query "active=true^priority=1" \
  --fields "number,short_description,state" --setlimit 10

# One record. Reference and choice fields come back as readable labels by default;
# --display-value false returns raw sys_ids and codes, all returns both.
sn table get incident <sys_id>
sn table get incident <sys_id> --display-value false

# The read verb is optional on table and cmdb — these mirror the REST path:
sn table incident <sys_id>          # same as: sn table get incident <sys_id>
sn table incident                   # same as: sn table list incident
sn cmdb cmdb_ci_server <sys_id>     # same as: sn cmdb get cmdb_ci_server <sys_id>
```

Only `get` and `list` are ever inferred, never a write, and only when the choice is
unambiguous — `get` needs a second positional and `list` refuses one. A misspelled verb
stays an error: `sn table lst incident` still tells you it meant `list`.

Note that `--display-value true` (the default) also renders dates in the calling user's
timezone and locale format, and a display-formatted date cannot be fed back into an
encoded query. Use `--display-value false` when a value has to round-trip.

### Writing records

`create` and `update` take either `--data` (`-D`) or `--field` (`-F`), mutually exclusive:

- `--data` / `-D '<json>'` — inline JSON object (`@file.json` reads a file, `@-` reads stdin)
- `--field` / `-F key=value` — repeatable key/value pairs (`key=@file` reads the value from a file)

```bash
# Key/value pairs, or inline JSON, or piped from another tool
sn table create incident --field short_description="Disk full on prod-db-01" --field urgency=2
sn table create incident --data '{"short_description":"Server down","priority":"1"}'
echo '{"short_description":"from pipe"}' | sn table create incident --data @-

# update = PATCH, and it is a partial update: omitted fields keep their values.
# To clear one, set it explicitly (e.g. --field description="").
sn table update incident <sys_id> --field state=2
sn table update incident <sys_id> --data @record.json

# Delete
sn table delete incident <sys_id> --yes
```

### Pagination

```bash
# Stream every match as JSONL (one record per line)
sn table list incident --query "active=true" --all

# ...or buffer into one JSON array; cap the total with --max-records
sn table list incident --all --array --max-records 5000

# Pipe to jq
sn table list incident --all | jq -r '.number'
```

### Watching records (live)

`sn watch` streams record changes as they happen, over ServiceNow's AMB websocket. Output is **JSONL on stdout**: one event per line, flushed as it arrives.

```bash
# Stream changes to matching records. Bound the stream, or it runs until you stop it.
sn watch table incident --query "priority=1^active=true" --max-events 5
sn watch table incident --sys-id <SYS_ID> --duration 60      # stop after 60s
sn watch table incident --query "active=true" --idle-timeout 30   # stop after 30s of quiet

# Narrow it down
sn watch table incident --query "active=true" --operation insert          # only new records
sn watch table incident --query "active=true" --on-change state,priority  # only these fields

# Other channels
sn watch count incident --query "active=true"   # how many records match
sn watch activity <SYS_ID>                      # comments, work notes, field changes
sn watch channel '/uxbannerannouncements'       # raw AMB channel (escape hatch)
```

**An event carries the fields that changed, with their new values.** `record` holds every field named in `changes` as a `{display_value, value}` pair, plus a few `sys_*` audit columns. This is what you get by default, with no extra API call:

```jsonc
{"table_name":"incident","sys_id":"1c74…","display_value":"INC0008001",
 "operation":"update","changes":["urgency","priority"],
 "changes_with_users":{"urgency":"abeyahmad"},
 "record":{"urgency":{"display_value":"1 - High","value":"1"},      // ← the new value
           "priority":{"display_value":"3 - Moderate","value":"3"}, // ← derived, recomputed
           "sys_updated_by":{"display_value":"abeyahmad","value":"abeyahmad"}}}
```

What an event does **not** carry is any field that *didn't* change: an event about `urgency` has no `number` and no `assigned_to`, because nobody wrote them. **`--hydrate`** fetches the whole row for each event (one Table API read) and puts it in `record` instead — reach for it when you need fields nobody touched. `--fields` narrows that fetch and `--display-value` resolves references; both require `--hydrate`. Note a hydrated row is current as of the *fetch*, not the event.

Worth knowing:

- **`changes` includes derived fields.** Writing `urgency` also reports `priority`, because ServiceNow recomputes it.
- **Inserts list every populated field** in `changes`, so an insert's `record` is the whole new row. **Deletes carry `changes: []`**, so `--on-change` never matches a delete — and no `record`, since there is nothing left to report (under `--hydrate` a delete emits `record: null` instead of attempting a doomed fetch).
- **`sn watch count` reports a delta, not a total** (`{"count": "+1"}`). Seed from `sn aggregate <TABLE> --count` and accumulate.
- Ctrl-C exits 0. Works with both basic and OAuth/SSO profiles.
- `--insecure` and `--ca-cert` are honored. **Proxies are not supported**: a profile with a proxy configured exits 1 rather than connecting around it.

### Schema discovery

Explore an unfamiliar instance:

```bash
sn schema tables --filter incident        # find tables by keyword
sn schema columns incident --writable     # writable columns for a table
sn schema choices incident state          # valid values for a choice field
```

### Aggregate queries

Server-side statistics, without fetching individual records:

```bash
# Count records grouped by state, with readable labels
sn aggregate incident --count --group-by state --display-value true

# Average a field, filtered
sn aggregate incident --avg-fields reassignment_count --query "active=true"

# Several aggregations in one call
sn aggregate incident --sum-fields reassignment_count --min-fields priority --max-fields priority
```

### Change Management

Normal, emergency, and standard change requests across their lifecycle:

```bash
# List; create (standard changes require --template); update; delete
sn change list --type normal --query "state=1" --setlimit 10
sn change create --type normal --field short_description="DB migration" --field category=software
sn change create --type standard --template <template_sys_id> --field short_description="Routine patching"
sn change update <sys_id> --field state=2
sn change delete <sys_id> --yes

# Workflow helpers
sn change nextstates <sys_id>                          # valid next states
sn change approvals <sys_id> --field approval="approved"
sn change risk <sys_id> --data '{"risk_value":"moderate"}'
sn change schedule <sys_id>
sn change models                                       # change models
sn change templates                                    # standard-change templates
```

#### Change tasks, CIs, and conflicts

```bash
# Tasks
sn change task list <change_sys_id>
sn change task create <change_sys_id> --field short_description="Pre-check"
sn change task update <change_sys_id> <task_sys_id> --field state=2
sn change task delete <change_sys_id> <task_sys_id> --yes

# CIs and conflicts
sn change ci add <change_sys_id> --data '{"cmdb_ci_sys_id":"<ci_id>"}'
sn change conflict get <sys_id>
sn change conflict remove <sys_id>
```

### Attachments

Files on any record:

```bash
sn attachment list --query "table_name=incident"
sn attachment get <sys_id>

# Upload a file (optionally override its name and content type)
sn attachment upload --table incident --record <record_sys_id> --file ./screenshot.png
sn attachment upload --table incident --record <record_sys_id> --file ./data.csv \
  --file-name "export_2026.csv" --content-type text/csv

# Download to a file, or to stdout for piping
sn attachment download <sys_id> --out ./downloaded.png   # -o also works
sn attachment download <sys_id> | gzip > backup.gz

sn attachment delete <sys_id> --yes
```

### CMDB

CRUD and relationships on Configuration Items of any class:

```bash
sn cmdb list cmdb_ci_server --query "operational_status=1" --setlimit 20
sn cmdb get cmdb_ci_server <sys_id>                                     # includes relations
sn cmdb create cmdb_ci_server --field name=web-server-01 --field ip_address=10.0.1.50
sn cmdb update cmdb_ci_server <sys_id> --field operational_status=2     # PATCH
sn cmdb meta cmdb_ci_server                                             # class schema

# Relations
sn cmdb relation add cmdb_ci_server <sys_id> --data '{"outbound_relations":[{"type":"<cmdb_rel_type_sys_id>","target":"<target_ci_sys_id>"}]}'
sn cmdb relation delete cmdb_ci_server <sys_id> <rel_sys_id> --yes
```

### Import Sets

Insert into staging tables for transform-based imports:

```bash
sn import create u_staging_table --field u_name="Server-01" --field u_ip="10.0.1.1"
sn import bulk u_staging_table --data '[{"u_name":"Server-01"},{"u_name":"Server-02"}]'
sn import get u_staging_table <sys_id>
```

### Service Catalog

Browse catalogs and items, then order directly or through the cart:

```bash
# Browse
sn catalog list
sn catalog categories <catalog_sys_id>
sn catalog items --text "laptop" --catalog <catalog_id>
sn catalog item <item_sys_id>
sn catalog item-variables <item_sys_id>       # form fields required to order

# Order immediately (bypasses the cart)
sn catalog order <item_sys_id> --data '{"sysparm_quantity":"1"}'

# ...or work the cart (cart-update / cart-remove / cart-empty also available)
sn catalog add-to-cart <item_sys_id> --data '{"sysparm_quantity":"1"}'
sn catalog cart
sn catalog checkout
sn catalog submit-order

sn catalog wishlist
```

### Identification & Reconciliation

Create, update, or identify CIs through the reconciliation engine. Each call takes an `items` payload:

```bash
# Create or update
sn identify create-update --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01","ip_address":"10.0.1.1"}}]}'

# Identify only, without modifying anything
sn identify query --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01"}}]}'

# Enhanced variants add --data-source and --options (partial payload/commit)
sn identify create-update-enhanced --data @payload.json \
  --data-source "discovery" --options "partial_payload:true,partial_commits:true"
sn identify query-enhanced --data @query.json --data-source "discovery"
```

### CICD operations

`app`, `updateset`, and `atf run` are asynchronous — they return a progress object and run in the background on the instance. Add `--wait` to block until the operation finishes and emit the final result, and `--wait-timeout <SECS>` to bound that wait (on expiry `sn` exits 3 with a pointer to `sn progress`). Without `--wait`, take the id from `links.progress.id` and poll manually with `sn progress <id>`.

```bash
# App Repository lifecycle
sn app install  --scope x_myapp --version 1.2.0 --wait
sn app publish  --scope x_myapp --version 1.3.0 --dev-notes "Bug fixes" --wait
sn app rollback --scope x_myapp --version 1.1.0 --wait

# Update sets
sn updateset create --name "My Changes" --description "Sprint 42 work"
sn updateset retrieve --update-set-id <id> --auto-preview
sn updateset preview <remote_update_set_id> --wait
sn updateset commit  <remote_update_set_id> --wait
sn updateset commit-multiple --ids id1,id2,id3
sn updateset back-out --update-set-id <id> --wait

# ATF suites
sn atf run --suite-name "Regression Suite" --wait --wait-timeout 900
sn atf results <result_id>

# Poll an operation already in flight
sn progress <progress_id>
```

### Performance Analytics scorecards

```bash
# List scorecards (paged and sorted)
sn scores list --per-page 20 --sort-by VALUE --sort-dir DESC

# Historical scores for one indicator
sn scores list --uuid <indicator_id> --include-scores --from 2026-01-01 --to 2026-04-01

sn scores favorite <uuid>
sn scores unfavorite <uuid>
```

### Inspect and connect

```bash
# Latency + auth + ServiceNow build version — one-shot health check (either auth method)
sn ping
# {"build_name":"Vancouver","build_tag":"glide-vancouver-...","instance":"acme.service-now.com",
#  "latency_ms":134,"ok":true,"profile":"prod","username":"admin"}

# The authenticated user, resolved via gs.getUserName()
sn user me
```

### Open a record in the web UI

```bash
sn open incident <sys_id>                # any table; opens the form in your default browser
sn open incident <sys_id> --print-url    # print the URL instead of opening it
```

### Raw REST passthrough

An escape hatch for endpoints not yet modeled as typed commands — returned exactly as ServiceNow sends it, no envelope unwrapping:

```bash
sn raw GET /api/now/v2/table/incident -q sysparm_limit=5 -q sysparm_query=active=true
sn raw POST /api/now/table/incident --data '{"short_description":"From sn raw"}'
sn raw PATCH /api/now/table/incident/abc123 --field state=2
sn raw DELETE /api/now/table/incident/abc123
```

### Human-readable table output

Most read commands accept `--output table` for columns instead of JSON — for interactive browsing; keep the default JSON for scripts and pipelines (don't pipe it):

```bash
sn table list incident --setlimit 5 --output table
sn schema columns incident --writable --output table
```

### Shell completions

```bash
# zsh — write to a dir on your fpath, then enable compinit
mkdir -p ~/.zsh/completions
sn completion zsh > ~/.zsh/completions/_sn
# add these two lines to ~/.zshrc (once), then restart your shell:
#   fpath=(~/.zsh/completions $fpath)
#   autoload -Uz compinit && compinit

# bash (requires the bash-completion package)
sn completion bash > ~/.local/share/bash-completion/completions/sn

# fish
sn completion fish > ~/.config/fish/completions/sn.fish
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`. The `${fpath[1]}` shortcut some tools suggest fails when that directory doesn't exist (common on Apple Silicon Homebrew) — the dir-on-fpath recipe above is portable.

### Agent integration

`sn` ships as a Claude Code plugin (plugin name `sn`, in `.claude-plugin/`) that pre-approves `Bash(sn *)` so Claude runs `sn` commands without per-call prompts. This repo is its own marketplace:

```bash
claude plugin marketplace add tehubersheezy/servicenow-cli   # or a local clone path
claude plugin install sn
```

In a clone of this repo, the skill at `.claude/skills/sn.md` is picked up automatically — invoke with `/sn`.

`sn introspect` dumps the full command tree as JSON — for auto-generating MCP tool definitions or function-call schemas:

```bash
sn introspect | jq '.subcommands[] | {name, about}'

# Flags that cannot be combined, across the whole tree:
sn introspect | jq '[.. | objects | select(.conflicts_with? // [] | length > 0)
                     | {name, conflicts_with}] | unique'
```

Each `args[]` entry carries `name`, `long`, `short`, `help`, `help_heading`, `required`, `takes_value`, `value_name`, `positional`, `repeatable`, `aliases`, `default_values`, `possible_values`, and `conflicts_with`. `--help` and `--version` are omitted — they exit before any handler runs — and nothing named `help` appears in the tree.

The root carries two extra keys: `version` (the binary that produced the tree) and `global_args` (the 11 flags clap propagates to every command). **A command's effective flags are its own `args` plus the root's `global_args`.** They live at the root because serializing them onto all 121 nodes was three quarters of the output:

```bash
# Everything `table list` accepts:
sn introspect | jq '[.global_args[], (.subcommands[] | select(.name=="table")
                     | .subcommands[] | select(.name=="list") | .args[])] | map(.name)'
```

One relation is missing and cannot be added: clap keeps `requires` private, so `--wait-timeout` requiring `--wait` shows up only as prose in that flag's `help`.

## Output contract

Commands emit JSON on stdout by a few consistent rules:

- `list` / `schema tables` / `columns` / `choices` → a JSON array (JSONL with `--all`).
- `get` / `create` / `update` → the single record object (`cmdb get` includes relations).
- `delete` → nothing.
- `aggregate` → a stats object; `scores` → scorecard records.
- Async CICD (`app`, `updateset`, `atf run`, `progress`) → a progress object carrying `status` — a numeric **string**, not a word: `"0"` pending, `"1"` running, `"2"` successful, `"3"` failed, `"4"` cancelled — alongside `status_message`, `percent_complete`, and the operation's id at `links.progress.id`.
- `attachment download` → raw bytes (or `{"path","size"}` metadata JSON when you pass `--out <file>`). The destination flag is `--out`/`-o`; `--output` is reserved CLI-wide for the output *mode*.

Across every command:

- `--output raw` preserves ServiceNow's `{"result": ...}` envelope; `--output table` renders columns (interactive only).
- Output is pretty-printed on a TTY, compact when piped — override with `--pretty` / `--compact`.
- Errors always go to stderr: `{"error": {"message", "detail?", "status_code?", "transaction_id?", "sn_error?"}}` — `sn_error` carries ServiceNow's raw error object.
- `--timeout <SECS>` bounds every request (default 30s).

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Usage or config error |
| 2 | API error (4xx/5xx, non-auth) |
| 3 | Network / transport error |
| 4 | Auth error (401/403) |

## Parameters

Every `sysparm_*` parameter has both a friendly name and a raw alias; `--query` and `--fields` also have short flags:

| Friendly | Short | Alias | Values |
|---|---|---|---|
| `--query` | `-q` | `--sysparm-query` | Encoded query string |
| `--fields` | `-f` | `--sysparm-fields` | Comma-separated field list |
| `--setlimit` |  | `--limit`, `--sysparm-limit`, `--page-size` | Max records returned (default 1000 for table list; 100 for attachment list) |
| `--offset` |  | `--sysparm-offset` | Starting offset |
| `--display-value` |  | `--sysparm-display-value` | `true` (default), `false`, `all` |
| `--exclude-reference-link` |  | `--sysparm-exclude-reference-link` | Flag (presence ⇒ true) |
| `--view` |  | `--sysparm-view` | Named UI view |
| `--input-display-value` |  | `--sysparm-input-display-value` | Flag (presence ⇒ true; writes) |
| `--suppress-auto-sys-field` |  | `--sysparm-suppress-auto-sys-field` | Flag (presence ⇒ true; writes) |
| `--suppress-pagination-header` |  | `--sysparm-suppress-pagination-header` | Flag (presence ⇒ true) |
| `--query-category` |  | `--sysparm-query-category` | Index-selection hint (string) |
| `--query-no-domain` |  | `--sysparm-query-no-domain` | Flag (presence ⇒ true) |
| `--no-count` |  | `--sysparm-no-count` | Flag (presence ⇒ true) |
| `--output` |  | (CLI only) | `default` (unwrapped JSON), `raw` (full envelope), or `table` (columnar — interactive only) |

## Configuration

Credentials use a two-file, AWS CLI-style split:

| File | Contains | Location (Linux) |
|---|---|---|
| `config.toml` | Instance URLs, default profile, non-secret OAuth config | `~/.config/sn/` |
| `credentials.toml` | Usernames, passwords, secrets, cached tokens (chmod 600) | `~/.config/sn/` |

macOS uses `~/Library/Application Support/sn/` and Windows `%APPDATA%\sn\`.

A **profile** is the single unit of identity. Create one with `sn init` (which also makes it the default) or `sn profile add` (which does not), select it with `--profile NAME` (`-p NAME`) or `sn profile use NAME`; resolution is `--profile` > `default_profile` > a clear error. No env var or flag overrides an individual profile field.

Point `sn` at a different config directory (for testing or sandboxing) with `SN_CONFIG_DIR`.

### Environment variables

| Env var | Description |
|---|---|
| `SN_CONFIG_DIR` | Override the config directory. Points **directly** at the folder holding `config.toml` and `credentials.toml` (no `sn` subdirectory appended). Cross-platform; when unset, the platform-native location is used. |
| `SN_PROXY` | HTTP/HTTPS/SOCKS5 proxy URL |
| `SN_NO_PROXY` | Comma-separated hosts to bypass the proxy |
| `SN_INSECURE=1` | Disable TLS certificate verification |
| `SN_CA_CERT` | Path to a custom CA cert for ServiceNow |
| `SN_PROXY_CA_CERT` | Path to a custom CA cert for the proxy |

```bash
SN_PROXY=http://proxy:8080 sn table list incident
SN_INSECURE=1 sn table list incident    # skip cert verification
```

There are deliberately no environment variables for credential values or profile selection — use profiles (`sn init`, `sn profile add`, `--profile`) instead. To keep a secret off the command line in a script, pipe it in with `sn profile add --password-stdin` / `--client-secret-stdin`.

## Proxy and TLS

Route through a proxy or adjust TLS per invocation:

```bash
sn --proxy http://proxy.corp:8080 table list incident   # also socks5://proxy:1080
sn --no-proxy table list incident                        # bypass a configured proxy for one call
sn --insecure table list incident                        # skip cert verification (dev/self-signed certs)
sn --ca-cert /path/to/ca.pem table list incident         # custom CA certificate
```

Any of these can live in a profile — non-secrets in `config.toml`, proxy credentials in `credentials.toml`:

```toml
# config.toml
[profiles.dev]
instance = "dev.example.com"
proxy = "http://proxy.corp:8080"
no_proxy = "localhost,127.0.0.1"
insecure = false
ca_cert = "/etc/ssl/custom-ca.pem"
proxy_ca_cert = "/etc/ssl/proxy-ca.pem"

# credentials.toml
[profiles.dev]
proxy_username = "proxy-user"
proxy_password = "proxy-pass"
```

Precedence for every proxy/TLS setting: CLI flag > env var (`SN_PROXY`, `SN_INSECURE=1`, …) > profile config.

`--insecure` is the exception: it is a logical OR across all three sources, not a chain. TLS verification is disabled if **any** of the flag, `SN_INSECURE`, or the profile's `insecure = true` says so — there is no way to turn it back *on* for one invocation of a profile that has it set. That's deliberate (a footgun should not be quietly re-armed by a stale config), but it means the only way to undo `insecure = true` is to edit the profile.

## Debugging

```bash
sn -d   table list incident     # HTTP method, URL, status
sn -dd  table list incident     # + response headers
sn -ddd table list incident     # + request/response bodies (auth headers, cookies, OAuth tokens masked)
sn -v                           # print version (-V also works)
```

## License

MIT
