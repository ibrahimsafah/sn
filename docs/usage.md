# Usage guide

Every command group, with examples. Two things hold everywhere and are worth knowing before
anything else:

- **Output is JSON on stdout, errors are JSON on stderr, and the exit code tells you what
  happened.** The full rules are in [Output contract](#output-contract) at the bottom.
- **A command needs a profile** — an instance + credentials saved by `sn init` or
  `sn profile add`. See the [setup guide](setup.md) if you haven't made one.

Working with records:

- [Reading records](#reading-records)
- [Writing records](#writing-records)
- [Pagination](#pagination)
- [Watching records (live)](#watching-records-live)
- [Schema discovery](#schema-discovery)
- [Journal: comments and work notes](#journal-comments-and-work-notes)
- [Aggregate queries](#aggregate-queries)
- [GraphQL](#graphql)

ITSM and platform APIs:

- [Change Management](#change-management)
- [Attachments](#attachments)
- [CMDB](#cmdb)
- [Import Sets](#import-sets)
- [Service Catalog](#service-catalog)
- [Identification & Reconciliation](#identification--reconciliation)
- [CICD operations](#cicd-operations)
- [Performance Analytics scorecards](#performance-analytics-scorecards)

Utilities:

- [Inspect and connect](#inspect-and-connect)
- [Open a record in the web UI](#open-a-record-in-the-web-ui)
- [Raw REST passthrough](#raw-rest-passthrough)
- [Human-readable table output](#human-readable-table-output)
- [Shell completions](#shell-completions)

The contract:

- [Output contract](#output-contract)
- [Exit codes](#exit-codes)
- [Parameters](#parameters)
- [Debugging](#debugging)

## Reading records

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

## Writing records

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

## Pagination

```bash
# Stream every match as JSONL (one record per line)
sn table list incident --query "active=true" --all

# ...or buffer into one JSON array; cap the total with --max-records
sn table list incident --all --array --max-records 5000

# Pipe to jq
sn table list incident --all | jq -r '.number'
```

## Watching records (live)

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
- **`sn watch count` reports a delta, not a total** (`{"count": "+1"}`). Seed from `sn aggregate <TABLE> --count --query <ENCODED_QUERY>` (same query as the watch) and accumulate.
- Ctrl-C exits 0. Works with both basic and OAuth/SSO profiles.
- `--insecure` and `--ca-cert` are honored. **Proxies are not supported**: a profile with a proxy configured exits 1 rather than connecting around it.

## Schema discovery

Explore an unfamiliar instance:

```bash
sn schema tables --filter incident        # find tables by keyword
sn schema columns incident --writable     # writable columns for a table
sn schema choices incident state          # valid values for a choice field
```

## Journal: comments and work notes

Journal entries live in `sys_journal_field`, one row per entry — but that table is
ACL-locked for non-admin roles (the row *count* comes back, the rows don't). What any
role that can read the record *can* read is the record's rendered journal stream.
`sn journal` fetches it over GraphQL and parses it back into structured entries,
newest first:

```bash
sn journal incident <sys_id>                    # all entries: [{created_on, author, element, label, text}]
sn journal incident <sys_id> --comments         # customer-visible comments only
sn journal incident <sys_id> --work-notes       # work notes only
sn journal incident <sys_id> --limit 5          # newest 5
sn journal incident <sys_id> --raw              # the unparsed rendered stream, as a JSON string
sn journal incident <sys_id> --source table     # exact sys_journal_field rows (needs table ACL access)
```

The default `--source record` works for any role that can read the record; its
timestamps are rendered in the calling user's timezone and date format. `--source table`
returns exact rows with UTC timestamps and usernames instead — and when rows exist but
ACLs filter them all, the error says so and points back at `--source record`. Adding an
entry needs no dedicated command: `sn table update incident <sys_id> --field
work_notes="..."` writes one.

## Aggregate queries

Server-side statistics, without fetching individual records:

```bash
# Count records grouped by state, with readable labels
sn aggregate incident --count --group-by state --display-value true

# Average a field, filtered
sn aggregate incident --avg-fields reassignment_count --query "active=true"

# Several aggregations in one call
sn aggregate incident --sum-fields reassignment_count --min-fields priority --max-fields priority
```

## GraphQL

`POST /api/now/graphql` serves ServiceNow's whole GraphQL surface, including the generated `GlideRecord_Query` / `GlideRecord_Mutation` / `GlideAggregateRecord_Query` namespaces — a query field and CRUD mutations for every table, with per-field display values, inline choice lists, ACL-evaluated metadata, and server-side dot-walking through reference fields. `sn graphql` runs a document against it under the profile's auth and the standard output/error contract:

```bash
sn graphql 'query { GlideRecord_Query { incident(queryConditions: "active=true", pagination: { limit: 5 }) { _rowCount _results { number { value } state { displayValue } } } } }'
sn graphql @query.graphql --var id=a1b2c3d4e5f6           # document from a file, one string variable
sn graphql @- --variables '{"limit": 5}' < query.graphql  # document from stdin, typed variables
sn graphql @doc.graphql --operation GetIncident           # pick one operation from a multi-op document
```

On success stdout gets `data` unwrapped — the GraphQL analogue of stripping `{"result": ...}` (`--output raw` keeps the whole response). GraphQL reports failure **in-band**: HTTP 200 with an `errors` array, sometimes alongside partial `data`. A response with errors exits 2 with the first error's message in the stderr envelope and the full array under `sn_error`; any partial `data` still reaches stdout first. `--var k=v` sets a string variable (repeatable; only the first `=` splits, so encoded queries pass through). `--variables` takes a whole JSON object for non-string variables; `--var` entries overlay it.

What GraphQL gives you that the Table API can't: a total match count beside a page
(`_rowCount`), many tables or queries in one request, per-field `canRead`/`canWrite`
verdicts, choice lists resolved in record context (`_choices`), and structured dot-walking
through reference fields (`_reference`). See [graphql.md](graphql.md) for the design notes.

## Change Management

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

### Change tasks, CIs, and conflicts

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

## Attachments

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

## CMDB

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

## Import Sets

Insert into staging tables for transform-based imports:

```bash
sn import create u_staging_table --field u_name="Server-01" --field u_ip="10.0.1.1"
sn import bulk u_staging_table --data '[{"u_name":"Server-01"},{"u_name":"Server-02"}]'
sn import get u_staging_table <sys_id>
```

## Service Catalog

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

## Identification & Reconciliation

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

## CICD operations

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

## Performance Analytics scorecards

```bash
# List scorecards (paged and sorted)
sn scores list --per-page 20 --sort-by VALUE --sort-dir DESC

# Historical scores for one indicator
sn scores list --uuid <indicator_id> --include-scores --from 2026-01-01 --to 2026-04-01

sn scores favorite <uuid>
sn scores unfavorite <uuid>
```

## Inspect and connect

```bash
# Latency + auth + ServiceNow build version — one-shot health check (either auth method)
sn ping
# {"build_name":"Vancouver","build_tag":"glide-vancouver-...","instance":"acme.service-now.com",
#  "latency_ms":134,"ok":true,"profile":"prod","username":"admin"}

# The authenticated user, resolved via gs.getUserName()
sn user me
```

## Open a record in the web UI

```bash
sn open incident <sys_id>                # any table; opens the form in your default browser
sn open incident <sys_id> --print-url    # print the URL instead of opening it
```

## Raw REST passthrough

An escape hatch for endpoints not yet modeled as typed commands — returned exactly as ServiceNow sends it, no envelope unwrapping:

```bash
sn raw GET /api/now/v2/table/incident -q sysparm_limit=5 -q sysparm_query=active=true
sn raw POST /api/now/table/incident --data '{"short_description":"From sn raw"}'
sn raw PATCH /api/now/table/incident/abc123 --field state=2
sn raw DELETE /api/now/table/incident/abc123
sn raw GET /api/now/table/incident -H 'X-no-response-body: true' -H 'X-Trace: 1'
```

## Human-readable table output

Most read commands accept `--output table` for columns instead of JSON — for interactive browsing; keep the default JSON for scripts and pipelines (don't pipe it):

```bash
sn table list incident --setlimit 5 --output table
sn schema columns incident --writable --output table
```

## Shell completions

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

## Output contract

Commands emit JSON on stdout by a few consistent rules:

- `list` / `schema tables` / `columns` / `choices` → a JSON array (JSONL with `--all`).
- `get` / `create` / `update` → the single record object (`cmdb get` includes relations).
- `delete` → nothing.
- `aggregate` → a stats object; `scores` → scorecard records; `journal` → an array of entries.
- `graphql` → the response's `data` value, unwrapped; a non-empty `errors` array means exit 2 with the errors on stderr (partial `data` still reaches stdout).
- Async CICD (`app`, `updateset`, `atf run`, `progress`) → a progress object carrying `status` — a numeric **string**, not a word: `"0"` pending, `"1"` running, `"2"` successful, `"3"` failed, `"4"` cancelled — alongside `status_message`, `percent_complete`, and the operation's id at `links.progress.id`.
- `attachment download` → raw bytes (or `{"path","size"}` metadata JSON when you pass `--out <file>`). The destination flag is `--out`/`-o`; `--output` is reserved CLI-wide for the output *mode*.

Across every command:

- `--output raw` preserves ServiceNow's `{"result": ...}` envelope; `--output table` renders columns (interactive only).
- Output is pretty-printed on a TTY, compact when piped — override with `--pretty` / `--compact`.
- Errors always go to stderr: `{"error": {"message", "detail?", "status_code?", "transaction_id?", "sn_error?"}}` — `sn_error` carries ServiceNow's raw error object.
- `--timeout <SECS>` bounds every request (default 30s).

## Exit codes

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
| `--setlimit` |  | `--limit`, `--sysparm-limit`, `--page-size` | Max records returned. Default 1000 on `table`/`change`/`cmdb` list; 100 on `change task list`, `attachment list`, `catalog categories`, `catalog items` |
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

## Debugging

```bash
sn -d   table list incident     # HTTP method, URL, status
sn -dd  table list incident     # + response headers
sn -ddd table list incident     # + request/response bodies (auth headers, cookies, OAuth tokens masked)
sn -v                           # print version (-V also works)
```
