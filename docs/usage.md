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

- [API discovery](#api-discovery)
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

`--yes` skips the confirmation, and without a terminal it is required — a non-interactive run
without it exits 1 naming the operation and its target rather than prompting into a void. Every
destructive command carries the same guard, not only the ones spelled `delete`: `table delete`,
`change delete`, `change task delete`, `change conflict remove`, `attachment delete`,
`cmdb relation delete`, `catalog cart-remove`, `catalog cart-empty`, `updateset back-out`,
`app rollback`, and `profile remove`.

## Pagination

```bash
# Stream every match as JSONL (one record per line)
sn table list incident --query "active=true" --all

# ...or buffer into one JSON array; cap the total with --max-records
sn table list incident --all --array --max-records 5000

# Pipe to jq
sn table list incident --all | jq -r '.number'
```

`--all` is JSONL and only JSONL, so it refuses both other output modes with exit 1 rather than
accepting a flag it would ignore. For columns, buffer first — `--all --array --output table`.
`--output raw` has no equivalent: pagination flattens every page's `{"result": ...}` envelope into
a record stream, so there is nothing left to keep; page by hand with `--setlimit`/`--offset` if the
envelope is what you need.

## Watching records (live)

`sn watch` streams record changes as they happen, over ServiceNow's AMB websocket. Output is **JSONL on stdout**, flushed as it arrives: one event per line, plus the occasional supervisor marker described under [Gaps](#gaps-a-watch-is-best-effort-and-says-where-it-broke).

```bash
# Stream changes to matching records. Bound the stream, or it runs until you stop it.
sn watch incident --query "priority=1^active=true" --max-events 5
sn watch incident --sys-id <SYS_ID> --duration 60           # stop after 60s
sn watch incident --query "active=true" --idle-timeout 30   # stop after 30s of quiet

# Narrow it down
sn watch incident --query "active=true" --operation insert          # only new records
sn watch incident --query "active=true" --on-change state,priority  # only these fields
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
- Ctrl-C exits 0. Works with both basic and OAuth/SSO profiles.
- `--insecure` and `--ca-cert` are honored. **Proxies are not supported**: a profile with a proxy configured exits 1 rather than connecting around it.

### Gaps: a watch is best-effort, and says where it broke

AMB has no replay and no cursor. A subscription starts at "now", so every change that happened
while a session was down is gone — no later message carries it and a reconnect cannot ask for it.
What the watcher *can* do is say where the hole is. After an established session drops and the
resubscribe succeeds, it writes one synthetic line:

```json
{"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}
```

Everything between the preceding line and that marker is missing. Without it, a lost feed and a
quiet table look identical.

- **The marker is keyed, not shaped like an event.** `sn_watch` appears on nothing else, and it
  carries neither `operation` nor `changes` — the two fields `--operation`/`--on-change` match on,
  and the two a `jq` predicate is most likely to test. A marker shaped like an event would be
  dropped by exactly the pipelines that most need to see it. Filter with
  `jq 'select(.sn_watch == null)'` if you want events only.
- **It is not an event**: it does not count against `--max-events` and does not reset the
  `--idle-timeout` clock.
- **Markers are routine.** ServiceNow reaps a watcher's HTTP session every minute or two
  regardless of traffic; the watcher detects the reap on its next poll, reconnects on a fresh
  session, and writes a marker. A long watch therefore carries periodic small-`downtime_ms`
  markers — expected, not a sign of trouble. One honest caveat: the reap precedes its detection
  by up to one ~30s long-poll cycle, and that undetected window is not inside `downtime_ms` —
  when completeness matters, reconcile from shortly *before* the reported window, not just
  inside it.
- **One marker per gap, not per attempt.** `downtime_ms` spans the whole outage however many
  reconnects it took; `attempt` is the ordinal of the one that succeeded. A clean run emits no
  marker at all.
- Anything that has to be complete must be reconciled against the table itself over the reported
  window — the marker gives you exactly the interval to re-query.

**`--idle-timeout` measures subscribed time, and only subscribed time.** The clock starts at the
first successful subscribe, not at process start, and every interval spent off the channel is
forgiven — `downtime_ms` is exactly what was forgiven. So `--idle-timeout 3` on an instance that
takes a second to mint a session runs about four seconds, and an outage longer than the timeout
cannot make the marker the last line of the stream. Silence still accumulates *across* sessions,
so a connection flapping faster than the timeout cannot hold a silent watcher open forever.

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
sn change conflict remove <sys_id> --yes   # takes no conflict id: this clears them all
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

Downloads stream through a fixed 64 KiB buffer, so memory does not track file size — 800 MB
measured at 19 MB peak RSS — and there is no attachment too large to fetch. Three details follow
from that:

- **`--timeout` is a per-read idle timeout on a download**, not a cap on the whole transfer (it
  still is on every other command). A slow but healthy transfer runs as long as it needs; a stalled
  one dies `--timeout` seconds after the last byte. The connect and header phase stays bounded, so
  a 404 still fails immediately.
- **`--out` never leaves a truncated file behind.** Bytes are staged in a hidden `.part` file in the
  destination's own directory and renamed into place only once the transfer completes — same
  filesystem, so the rename is atomic. If the download fails, the staging file is removed and a file
  already at that path is left byte-for-byte untouched, so a retry is always safe. Ctrl-C removes the
  staging file and exits **130**.
- **stdout has no undo.** Bytes handed to a pipe cannot be recalled, so a mid-stream failure is exit
  3 with an error naming how many truncated bytes were already written. Prefer `--out` for anything
  large.

## CMDB

CRUD and relationships on Configuration Items of any class:

```bash
sn cmdb list cmdb_ci_server --query "operational_status=1" --setlimit 20
sn cmdb get cmdb_ci_server <sys_id>                                     # includes relations
sn cmdb create cmdb_ci_server --field name=web-server-01 --field ip_address=10.0.1.50
sn cmdb update cmdb_ci_server <sys_id> --field operational_status=2     # PATCH
sn cmdb update cmdb_ci_server <sys_id> --field name=web-01 --source "Other Automated"
sn cmdb meta cmdb_ci_server                                             # class schema

# Relations
sn cmdb relation add cmdb_ci_server <sys_id> --data '{"outbound_relations":[{"type":"<cmdb_rel_type_sys_id>","target":"<target_ci_sys_id>"}]}'
sn cmdb relation delete cmdb_ci_server <sys_id> <rel_sys_id> --yes
```

The CMDB Instance API takes writes in an envelope, `{"attributes": {...}, "source": "..."}`, and
`sn` builds it: give `create`/`update` flat fields exactly as on `table` and they are wrapped for
you. Values go out as strings, because the API casts each attribute to a Java `String` and answers a
JSON number or boolean with an HTTP 500 — so `--field cpu_count=8` sends `"8"`, while an object or
array is refused up front with a usage error. A body whose `attributes` is a JSON object is taken as
an envelope you wrote yourself and passed through unchanged; that is how `inbound_relations` /
`outbound_relations` ride along on a create.

`--source` is the record's provenance and lands in `discovery_source`. It defaults to
`"Manual Entry"` — the truthful value for a CLI write. Name a real discovery source only when
standing in for it: the IRE reconciles by source, so borrowing a tool's name lets that tool's next
run overwrite the record. Valid values are the choices on `cmdb_ci.discovery_source`
(`sn schema choices cmdb_ci discovery_source`). Giving `source` in both a flat body and the flag —
or in a flat body at all, where the API would drop it — is a usage error rather than a silent
preference.

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

# ...or work the cart
sn catalog add-to-cart <item_sys_id> --data '{"sysparm_quantity":"1"}'
sn catalog cart
sn catalog cart-update <cart_item_id> --data '{"sysparm_quantity":"2"}'
sn catalog cart-remove <cart_item_id> --yes   # drops one line
sn catalog cart-empty <cart_sys_id> --yes     # drops the whole cart; nothing restores it
sn catalog checkout
sn catalog submit-order

sn catalog wishlist
```

`cart-remove` and `cart-empty` are gated like a delete: on a terminal they prompt, and without a
terminal they need `--yes` or exit 1.

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
sn app rollback --scope x_myapp --version 1.1.0 --wait --yes

# Update sets
sn updateset create --name "My Changes" --description "Sprint 42 work"
sn updateset retrieve --update-set-id <id> --auto-preview
sn updateset preview <remote_update_set_id> --wait
sn updateset commit  <remote_update_set_id> --wait
sn updateset commit-multiple --ids id1,id2,id3
sn updateset back-out --update-set-id <id> --wait --yes

# ATF suites
sn atf run --suite-name "Regression Suite" --wait --wait-timeout 900
sn atf results <result_id>

# Poll an operation already in flight
sn progress <progress_id>
```

`app rollback` and `updateset back-out` require `--yes` without a terminal, the same guard the
deletes carry: a back-out reverts every record its update set applied, a rollback replaces an
installed app instance-wide, both run asynchronously, and neither has an undo.

`--wait` honors `--output raw` and `--output table` — under raw it used to emit the initial,
unpolled response and never wait at all. A failed operation is exit 2 with the progress object on
stderr under `.error.sn_error`, and no `status_code`, since the instance reported the failure inside
an HTTP 200.

## Performance Analytics scorecards

```bash
# List scorecards (paged and sorted)
sn scores list --per-page 20 --sort-by VALUE --sort-dir DESC

# Historical scores for one indicator
sn scores list --uuid <indicator_id> --include-scores --from 2026-01-01 --to 2026-04-01

sn scores favorite <uuid>
sn scores unfavorite <uuid>
```

## API discovery

`sn schema` answers "what does this table look like?"; `sn api` answers "is there an API for this?"
It reads the same catalogue the instance's REST API Explorer does:

```bash
sn api list                          # every namespace, with API and endpoint counts
sn api list --namespace sn_chg_rest  # the APIs in one namespace
sn api search attachment             # matching endpoints, with method and route
sn api search cart --namespace sn_sc --method POST
sn api spec "Table API"              # the OpenAPI 3 document
sn api spec "Table API" --format yaml > table-api.yaml
```

`search` matches case-insensitively across namespace, API name, route and both descriptions, and
each row carries what a call needs — `route` is relative to `/api`, so
`/now/attachment/{sys_id}` is `sn raw DELETE /api/now/attachment/<sys_id>`. `list` and `search`
summarize; `--output raw` prints the catalogue endpoint's own response instead (several hundred KB)
for piping to `jq`, and `--output table` renders either as columns.

`spec` takes the name `list` reports; a unique case-insensitive substring is enough, and an
ambiguous one exits 1 listing the candidates with their namespaces so `--namespace` can break the
tie. `--format yaml` goes to stdout verbatim and ignores `--pretty`/`--compact`/`--output`.

An unknown `--namespace` is a usage error naming the near miss — the endpoint answers a bad
namespace with `{"result":{}}` and HTTP 200, which would otherwise be indistinguishable from "no
matches". A genuine 404 keeps the instance's own explanation ("Version v99 not found for now/Table
API") instead of being rewritten as a guess about the release.

## Inspect and connect

```bash
# Auth + identity + latency + build — one-shot health check (either auth method)
sn ping
# {"ok":true,"profile":"prod","instance":"acme.service-now.com","username":"admin",
#  "identity_source":"sg/impersonation/session","user_sys_id":null,"user_display_name":null,
#  "admin":true,"can_impersonate":true,"impersonating":false,"original_user":"admin",
#  "latency_ms":134,"build_name":null,"build_tag":null}

# The caller's own sys_user record
sn user me
```

`username` is **the instance's answer, not the configured one** — `sn ping` asks an endpoint that
names the caller, because echoing the profile back verifies nothing about identity and the two
disagree exactly when the profile is wrong. `identity_source` says which endpoint answered
(`sg/impersonation/session`, `ui/user/current_user`, `sys_user`, or `profile` for the configured
name as a last resort). `admin`, `can_impersonate`, `impersonating` and `original_user` come from
the same probe and are `null` when the endpoint that carries them is absent; `impersonating` is
`true` only when two present, non-blank names differ. `build_name`/`build_tag` need their own
`sys_properties` read and are `null` when it returns nothing — a Zurich PDI carries neither
`glide.buildname` nor `glide.buildtag`, so null there means "the instance doesn't publish it", not
a failure.

`sn user me` resolves the caller's sys_id and reads that one record — no `javascript:` term on the
wire, so a term the instance cannot evaluate cannot be silently dropped and leave you holding a
stranger's record. On an instance without that endpoint it falls back to the scripted `sys_user`
read and exits 2 if the filter was evidently dropped.

## Open a record in the web UI

```bash
sn open incident <sys_id>                # any table; opens the form in your default browser
sn open incident <sys_id> --print-url    # print the URL instead of opening it
```

Opening emits `{"opened": true, "url": "..."}` and honors `--output`. `--print-url`
deliberately does not: it writes the bare URL and nothing else, under every
`--output` mode, so `$(sn open … --print-url)` is directly usable in a shell.

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
sn aggregate incident --count --group-by state --output table
sn api search attachment --method DELETE --output table
sn table list incident --query "active=true" --all --array --output table
```

`aggregate`, `scores list`, `scores favorite` and `open` accepted `--output table` and silently
ignored it in earlier releases; all four go through the same renderer as every other command now.
`--all` still refuses it — a table cannot size a column without seeing every row, so buffer with
`--array` as above.

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
- `api list` / `api search` → an array of summary rows; `api spec` → the OpenAPI document (JSON, or YAML verbatim under `--format yaml`).
- `profile use` → `{"ok","profile","default"}`; `profile remove` → `{"ok","profile","removed","wasDefault"}`, with `removed:false` and exit 0 when there was no such profile.
- `scores unfavorite` → the endpoint's body, or `{"ok","uuid"}` when there is none; it used to print nothing at all. `scores favorite` passes the body through as-is, which is `null` on an instance that answers the POST with no content.

Across every command:

- `--output raw` preserves ServiceNow's `{"result": ...}` envelope; `--output table` renders columns (interactive only). A mode a command cannot honor is a usage error, not a silent fallback: `--all` refuses both.
- Output is pretty-printed on a TTY, compact when piped — override with `--pretty` / `--compact`.
- Errors always go to stderr: `{"error": {"message", "detail?", "status_code?", "transaction_id?", "sn_error?"}}` — `sn_error` carries ServiceNow's raw error object. Only `message` is guaranteed; `status_code` is **omitted** when the failure carried no HTTP status (a CICD operation reported as failed inside a 200 under `--wait`, a scripted query the instance dropped) — never a fabricated `0`. It *is* reported as `200` where the HTTP call genuinely succeeded and ServiceNow put the failure in the body (`sn graphql`, `sn journal`, `sn variables set`), so the key says what HTTP said, not whether the command worked: branch on the exit code instead.
- `--timeout <SECS>` bounds every request (default 30s) — except on `attachment download`, where it becomes a per-read idle timeout.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Usage or config error — including a destructive command refused for want of `--yes`, and a config write that could not take the directory lock within 10s |
| 2 | API error (4xx/5xx, non-auth), or a failure the instance reported inside an HTTP 200 |
| 3 | Network / transport error |
| 4 | Auth error — every 401 and every 403 |

Exit 4 is wider than "wrong password": a 403 from an ACL, a field the role cannot write, or an
expired token all land here, and `status_code` is the only thing distinguishing them. There is no
exit 2 with `status_code: 403`. Re-authenticating fixes the 401 case only; if the same call fails
again, the answer is a role or an ACL.

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
