---
name: sn
description: Use when the user asks about ServiceNow data, incidents, change requests, problems, CIs, attachments, CMDB, service catalog, import sets, or any SNOW/SN operations. Also use when user says "sn", "servicenow", or references a ServiceNow instance, comments/work notes on a record, CICD operations (app install/publish/rollback, update sets, ATF tests), aggregate statistics, Performance Analytics scorecards, CI reconciliation, GraphQL queries, which REST APIs an instance publishes (API discovery / OpenAPI specs), or watching records change in real time (record watchers / live updates / AMB websocket).
---

# sn — ServiceNow CLI

Single binary wrapping ServiceNow's REST APIs (one per section below). Machine contract: JSON on stdout, JSON errors on stderr, deterministic exit codes, no interactive surprises.

Install: `brew install tehubersheezy/sn/sn` or https://github.com/tehubersheezy/servicenow-cli

## Setup & profiles

```bash
sn profile add prod --instance X --username Y --password-stdin < secret.txt   # AGENT-SAFE: never prompts
sn init                                          # human wizard: prompts, and CLAIMS default_profile
sn ping                                          # verify connectivity + credentials, and who the instance says you are
sn -p prod table list incident                  # pick profile per command (-p = --profile)
sn profile list                                  # also: add <name> / show <name> / use <name>
sn profile remove old --yes                      # --yes required off a TTY (see Destructive commands)
```

**Use `sn profile add`, not `sn init`.** It emits JSON on stdout and never prompts when stdin isn't a TTY (a missing field is exit 1 naming the flag, not a hang). It also leaves `default_profile` alone — `sn init` takes it over. Pipe secrets via `--password-stdin` / `--client-secret-stdin`; `--password` is visible in `ps` and shell history.

`add` verifies the credentials against the instance and **writes nothing if they're rejected** (exit 4), so you never inherit a broken profile. Exit 1 if the profile exists (`--force` to overwrite) or a flag is missing. `--no-verify` skips the network; `--set-default` also makes it the default.

Profile selection: `--profile`/`-p` > `default_profile` in config > error (no implicit fallback). `SN_CONFIG_DIR` overrides the config dir (`config.toml`/`credentials.toml`). No env var sets credentials or selects a profile (proxy/TLS env vars excepted — see Proxy & TLS).

Both files are `0600` and are replaced atomically under a lock on a `.sn.lock` sidecar, so parallel invocations that write profiles or refresh OAuth tokens serialize instead of clobbering each other. A writer that waits more than 10s for the lock fails exit 1 naming the holder's file. Readers take no lock.

## OAuth / SSO

For instances behind an external IdP (Okta, Azure AD, ADFS) basic auth can't work — use OAuth (`auth = "oauth"`). Configure via `sn profile add --auth oauth`; `sn auth login` runs the flow and caches tokens. The default `authorization_code` flow is public PKCE (no secret); `client_credentials` is non-interactive and needs a secret.

```bash
# client_credentials is headless: `add` mints and verifies the token itself. AGENT-SAFE.
sn profile add svc --auth oauth --grant client_credentials --client-id <id> --client-secret-stdin < secret.txt

# authorization_code NEEDS A BROWSER, so `add` refuses to save it unverified off a TTY.
# Register it, then get a human to log in:
sn profile add sso --auth oauth --client-id <id> --no-verify
sn auth login    # runs flow: OPENS A BROWSER, blocks up to 300s on human login — not agent-safe
sn auth status   # token state (never prints secrets)
sn auth refresh  # force refresh
sn auth logout   # drop cached tokens
```

Data commands never open a browser — tokens refresh transparently. A missing/expired token fails exit 4: mint one via `client_credentials` if a secret is configured, else ask the human to `sn auth login`.

## Output, exit codes & flags

- **stdout**: unwrapped JSON — `list`=array, `get`/`create`/`update`=object, `delete`=empty.
- **stderr** (when piped): `{"error": {"message", "detail?", "status_code?", "transaction_id?", "sn_error?"}}`. `sn_error` carries ServiceNow's own error body — read it to self-correct (bad field, missing role). On a TTY, usage errors print clap's human text instead.
- **Exit codes**: `0` ok · `1` usage/config (incl. bad flags, a missing `--yes`, a config-lock timeout) · `2` API error · `3` network/transport (incl. `--wait-timeout` expiry) · `4` auth. Branch on exit code first, parse stdout second.
- **Exit 4 is not always "log in again."** It is any 401/403, and ServiceNow answers a *row-level* ACL denial with 403 — an `itil` profile running `sn table create sys_user --field user_name=x` gets exit 4 / `"status_code": 403` on credentials that authenticate perfectly, and `sn ping` on that same profile still exits 0. So check `sn ping` before re-authenticating: `ok` plus a 403 on one table or one row is a missing role, and neither `sn auth login` nor a new password fixes it. A 401, or a ping that fails, is the credential.
- **`status_code` can be absent on exit 2.** The key is omitted, never `0` and never `null`, when the failure carried no HTTP status: an operation ServiceNow reports as failed *inside* a 200 (a `--wait` that ends in `status` `"3"`), or a 200 whose body doesn't answer the question (`sn user me` when the instance dropped the query term). Test for the key's presence, don't default it.
- **`--output default|raw|table`**: `default` = unwrapped JSON (pipe to jq); `raw` = full `{"result": ...}` envelope; `table` = human columns (interactive only, don't pipe). `table` and `raw` are honored everywhere, `aggregate`/`scores`/`open` and `--wait` included; neither can be combined with `--all` (see Pagination).

Flags (global unless noted; every `sysparm_*` has a friendly name + raw `--sysparm-*` alias):

- `--display-value true|false|all` (list/get) — **defaults to `true`**: choice/reference fields
  come back as labels, and dates in the caller's timezone/locale (which will not round-trip into
  a query). `false` for raw values, `all` for both.
- `--setlimit N` (list; aliases `--limit`, `--page-size`) — max records. Default 1000 on
  `table`/`change`/`cmdb` list, 100 on `change task`, `attachment`, `catalog categories`/`items`
- `--input-display-value` (writes) — set fields by display value
- `--timeout SECS` (default 30) · `--pretty`/`--compact` (default: pretty on TTY, compact when piped)
- `-d`/`-dd`/`-ddd` — log requests / +headers / +bodies to stderr (auth headers, cookies, and OAuth tokens masked); `-v`/`-V` = version
- `--data` and `--field` are mutually exclusive on writes (exit 1)
- `--timeout` caps the whole request everywhere except `sn attachment download`, where it becomes a **per-read idle timeout**: a slow-but-alive transfer completes, a stalled one still dies

## Destructive commands need `--yes`

Eleven commands prompt on a TTY and, when stdin is not a terminal, **refuse before sending anything**: exit 1, `{"error":{"message":"<verb> <target> requires --yes when stdin is not a terminal"}}`. An agent's stdin is never a terminal, so `-y`/`--yes` is mandatory:

`table delete` · `change delete` · `change task delete` · `change conflict remove` · `attachment delete` · `cmdb relation delete` · `catalog cart-remove` · `catalog cart-empty` · `profile remove` · `updateset back-out` · `app rollback`

The last two are gated even though a human types them deliberately: one flag, instance-wide effect, asynchronous, no second confirmation downstream, and no undo of their own. A back-out reverts every record its set applied; a rollback replaces an installed app without rolling back what the newer version wrote.

## Discovery flow (when you don't know the schema)

```bash
sn schema tables --filter incident          # 1. find the table
sn schema columns incident --writable       # 2. writable fields
sn schema choices incident state            # 3. valid values for a choice field
sn table create incident --field short_description="x" --field state=2   # 4. write
```

Response-shape gotchas — these bite hard because the obvious `jq` is silently wrong:
- `schema tables` — the table name is **`.value`**, not `.name` (`.name` is null).
- `schema columns` — no `choice_field`/`default_value`. Default is **`default`**; a choice column has `type:"choice"` with its options inlined in **`choices[]`**.

## API discovery (is there an API for X?)

`schema` describes tables; `api` describes **endpoints** — the instance's own REST catalogue, the same one the REST API Explorer reads. Use it before reaching for `sn raw`, and instead of a browser.

```bash
sn api list                              # every namespace with API/endpoint counts
sn api list --namespace now              # the APIs inside one namespace (-n)
sn api search attachment                 # endpoints matching a substring: namespace, api_name, method, route
sn api search cart --method DELETE       # narrow by HTTP method (case-insensitive)
sn api spec "Table API"                  # that API's OpenAPI 3 document (JSON)
sn api spec "Table API" --format yaml    # verbatim YAML on stdout; ignores --pretty/--compact/--output
```

`search` returns one row per endpoint carrying `namespace`, `api_name`, `name`, `version`, `method` and `route` — enough to call it with `sn raw`:

```bash
sn api search "attachment" -m POST | jq -r '.[] | "\(.method) /api\(.route)"'
sn raw GET /api/now/attachment -q sysparm_limit=1
```

- `spec <NAME>` takes the catalogue title, case-insensitively; a unique substring is enough. An ambiguous one is exit 1 listing every candidate as `namespace/title`, and the message names the fix: copy one of them exactly. `-n` only breaks a tie whose candidates span namespaces — it changes nothing for three matches inside `now`, so don't retry with it blind.
- An unknown `--namespace` is exit 1 naming near matches (`sn api list -n nw` → "did you mean 'now'?"), not an empty result. A genuine no-match from `search` is `[]` with exit 0.
- `--output raw` on `list`/`search` prints the catalogue endpoint's own response instead of the summary — several hundred KB, for jq.
- These are the Explorer's undocumented doc endpoints. A 404 is reported with the instance's own reason (bad API name, bad `--version`); only the endpoint family itself being absent is blamed on the release.

## Table CRUD

```bash
sn table list incident --query "active=true^priority=1" --fields "number,state" --setlimit 10
sn table get incident <sys_id>
sn table incident <sys_id>                            # verb optional on table/cmdb: = get
sn table incident                                     # = list (never infers a write)
sn table get incident <sys_id> --display-value false  # raw sys_ids and codes (labels are the default)
sn table create incident --field short_description="x" --field urgency=2
sn table create incident --data @body.json             # or --data '{"key":"val"}'
sn table update incident <sys_id> --field state=6      # PATCH (partial)
sn table delete incident <sys_id> --yes                # --yes required on non-TTY, else clean JSON error exit 1
```

`get` takes no `--query`; filter with `list --query "..." --setlimit 1`.

## Journal (comments & work notes)

```bash
sn journal incident <sys_id>                  # entries newest first: [{created_on, author, element, label, text}]
sn journal incident <sys_id> --comments       # or --work-notes (mutually exclusive)
sn journal incident <sys_id> --limit 5        # newest 5
sn journal incident <sys_id> --raw            # unparsed rendered stream as a JSON string
sn journal incident <sys_id> --source table   # exact sys_journal_field rows (UTC, usernames) — needs table ACL access
```

Default `--source record` parses the record's rendered journal stream (works for any role that can read the record; timestamps in the caller's timezone/format). `sys_journal_field` itself is ACL-locked for non-admin roles — with `--source table` the count leaks but rows come back empty, and the command errors naming the cause instead of emitting `[]`. To *add* an entry: `sn table update incident <sys_id> --field work_notes="..."`.

## Catalog variables (read + verified write)

```bash
sn variables get sc_req_item <sys_id>                        # [{name, label, value}] sorted by name
sn variables get incident <sys_id>                           # record-producer answers (question_answer)
sn variables set sc_req_item <sys_id> --field acrobat=true   # write + verify; repeatable --field, or --data '{...}'
```

Writes go through the undocumented `PUT /api/sn_sc/servicecatalog/variables/{table}/{sys_id}` — the only write path open to non-admin roles (direct `sc_item_option` writes are 403 for `itil`); it gates on write access to the record itself. The endpoint silently skips unknown variable names (200 with nothing written), so `set` validates names first — case-sensitive; unknown → exit 1 listing the pool, **before** any write — then re-reads after the PUT: a value that did not persist is exit 2, success reports `{updated: {name: {from, to}}, unchanged}`. An `sc_task` is resolved to its RITM automatically (`resolved_from` in the output). Multi-row variable sets are unsupported.

## Encoded query (--query)

The most error-prone part of any invocation:

- `^`=AND, `^OR`=OR, `^NQ`=new top-level query: `active=true^priority=1^ORpriority=2`
- Operators: `=`, `!=`, `>`, `>=`, `<`, `<=`, `IN` (`stateIN1,2,3`), `LIKE` (`short_descriptionLIKEdisk`), `STARTSWITH`, `ENDSWITH`
- Empty checks: `assigned_toISEMPTY`, `assigned_toISNOTEMPTY`
- Dot-walk references: `caller_id.name=Abel Tuter`, `cmdb_ci.location.city=Cary`
- Dates: `sys_created_on>javascript:gs.daysAgoStart(7)`, `opened_atONToday@javascript:gs.beginningOfToday()@javascript:gs.endOfToday()`
- Sort in-query: `ORDERBYDESCsys_created_on`, `ORDERBYnumber`
- Values are raw (no quotes); spaces are fine: `short_descriptionLIKEdisk full`

## Pagination

```bash
sn table list incident --all                     # JSONL stream (one record/line)
sn table list incident --all --array             # single JSON array
sn table list incident --all --max-records 5000  # safety cap (default 100000)
sn table list incident --all | jq -r '.number'   # pipe JSONL through jq
sn table list incident --all --array --output table --max-records 50   # the only way to table a paged read
```

`--all` cannot be combined with `--output raw` or `--output table` — both are exit 1 naming the conflict, not a silently ignored flag. `table` can't size a column without seeing every row, so buffer with `--array` first; `raw` has nothing to keep, since pagination flattens each page's `{"result": ...}` into a record stream (`--array` included). To keep envelopes, drop `--all` and page yourself with `--offset`/`--setlimit`.

## Watch (live record changes)

Streams record changes over ServiceNow's AMB websocket. **JSONL on stdout, one event per line, flushed as it arrives.**

```bash
# Bound the stream, or it runs until interrupted.
sn watch incident -q "priority=1^active=true" --max-events 5
sn watch incident --sys-id <SYS_ID> --duration 60      # stop after 60s
sn watch incident -q "active=true" --idle-timeout 30   # stop after 30s of quiet

sn watch incident -q "active=true" --operation insert           # only new records
sn watch incident -q "active=true" --on-change state,priority   # only these fields
```

**Events carry the changed fields WITH their new values.** `record` holds each field in `changes` as a `{display_value, value}` pair (+ a few `sys_*` audit cols). No API call — this is the default output:

```jsonc
{"table_name":"incident","sys_id":"1c74…","display_value":"INC0008001",
 "operation":"update","changes":["urgency","priority"],
 "changes_with_users":{"urgency":"abeyahmad"},
 "record":{"urgency":{"display_value":"1 - High","value":"1"},        // ← the NEW value
           "priority":{"display_value":"3 - Moderate","value":"3"}}}  // ← derived
```

**It omits fields that did NOT change** — an `urgency` event has no `number`, no `assigned_to`. Add **`--hydrate`** to fetch the whole row (1 Table API GET per event, **replaces** `record`); `-f/--fields` and `--display-value` narrow that fetch and **require** `--hydrate`. A hydrated row is current as of the fetch, not the event.

⚠️ Gotchas:
- **`changes` includes derived fields** — writing `urgency` also reports `priority` (ServiceNow recomputes it).
- **Inserts list every populated field** (so an insert's `record` is the whole new row); **deletes carry `changes: []`**, so `--on-change` never matches a delete. A delete carries no `record` at all (`record: null` under `--hydrate`).
- **Not every line is an event.** After the socket drops and resubscribes, the stream carries one marker: `{"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}`. AMB has no replay, so this is the only evidence the feed has a hole in it — everything that changed during `downtime_ms` is gone and no later line carries it. A consumer must tolerate it: it has no `operation` and no `changes`, so a `jq` predicate testing either drops it silently. Filter with `select(.sn_watch == null)` if you only want events, and treat its arrival as "re-read the table" if completeness matters. It is not an event: it does not count against `--max-events` and does not reset `--idle-timeout`. One marker per outage, not per attempt.
- **Markers are routine.** ServiceNow reaps a watcher's HTTP session every minute or two regardless of traffic; the watcher detects the reap, reconnects on a fresh session, and writes a marker. A long watch carries periodic small-`downtime_ms` markers — expected, not a sign of trouble. The reap precedes detection by up to one ~30s long-poll cycle and that window is not in `downtime_ms`, so when completeness matters reconcile from shortly before the reported window.
- **`--idle-timeout` measures subscribed time only** — connecting, handshaking and reconnecting are not idleness, so `--idle-timeout 1` cannot race a slow handshake. Silence still accumulates across a reconnect, so a socket flapping faster than the timeout can't hold a silent watcher open.
- Ctrl-C exits 0. Exit 4 if the profile can't authenticate, 3 if the socket can't be established.
- Works with basic **and** OAuth profiles. **No proxy support** (refused with exit 1, not silently bypassed); `--insecure`/`--ca-cert` do work.

## Aggregate

Server-side stats, no record fetch:

```bash
sn aggregate incident --count                       # → {"stats":{"count":"142"}}
sn aggregate incident --count --group-by state      # → an ARRAY, one entry per group
# [{"groupby_fields":[{"field":"state","value":"1"}],"stats":{"count":"15"}}, ...]
sn aggregate incident --sum-fields reassignment_count --min-fields priority
# sum/avg/min/max nest PER FIELD: {"stats":{"sum":{"reassignment_count":"24"}}}
```

⚠️ `--group-by` flips the top level from object to **array**, and `groupby_fields` is a **sibling** of `stats`, not inside it — `jq '.stats.groupby_fields[]'` returns nothing. Use `jq -r '.[] | "\(.groupby_fields[0].value)\t\(.stats.count)"'`.

## Change Management

```bash
sn change list --type normal --query "state=1" --setlimit 10
sn change get <sys_id> --type normal
sn change create --type normal --field short_description="DB migration"
sn change create --type standard --template <template_id>   # standard requires --template
sn change update <sys_id> --field state=2
sn change delete <sys_id> --yes                             # --yes required on non-TTY (like table delete)
```

⚠️ **The Change API returns every field as a `{display_value, value}` pair** — unlike the Table API. `.number` is an OBJECT: use `jq -r '.number.value'`, not `jq -r '.number'`. (`state.value` is a number, e.g. `3.0`.) And `change nextstates` returns `{"available_states":["3"],"state_label":{"3":"Closed"}}` — an object, not a list of `{value,label}`.

```bash
sn change nextstates <sys_id>                               # valid state transitions
sn change approvals <sys_id> --field approval="approved"
sn change risk <sys_id> --data '{"risk_value":"moderate"}'
sn change schedule <sys_id>
sn change models
sn change templates                                        # standard-change templates
sn change task list <change_sys_id>
sn change task create <change_sys_id> --field short_description="Pre-check"
sn change ci list <change_sys_id>
sn change ci add <change_sys_id> --data '{"cmdb_ci_sys_id":"<id>"}'
sn change conflict get <sys_id>
sn change conflict remove <sys_id> --yes                    # gated: clears ALL conflicts on the change
sn change task delete <change_sys_id> <task_sys_id> --yes   # gated
```

## Attachments

```bash
sn attachment list --query "table_name=incident"
sn attachment get <sys_id>
sn attachment upload --table incident --record <record_id> --file ./report.pdf
sn attachment download <sys_id> --out ./file.pdf       # -o too; NOT --output (that's the output MODE)
sn attachment delete <sys_id> --yes
```

`download` streams, so file size doesn't drive memory, and `--timeout` becomes a per-read idle timeout for it alone. **Prefer `--out` for anything large**: it stages into a hidden `.part` file in the destination's own directory and renames on success, so a failed download leaves a pre-existing file at that path untouched and never a truncated one (Ctrl-C unlinks the staging file, exit 130). It reports `{"path","size"}` on success. To stdout there is no such protection — a mid-stream failure is exit 3 with an envelope naming how many truncated bytes already went out.

## CMDB

```bash
sn cmdb list cmdb_ci_server --query "operational_status=1"
sn cmdb get cmdb_ci_server <sys_id>       # ⚠️ CI fields nest under .attributes — use .attributes.name,
                                          #    NOT .name. Top level is only {attributes,
                                          #    inbound_relations, outbound_relations}.
sn cmdb create cmdb_ci_server --field name=web-01 --field ip_address=10.0.1.1
sn cmdb update cmdb_ci_server <sys_id> --field operational_status=2
sn cmdb update cmdb_ci_server <sys_id> --field comments=x --source "Other Automated"   # name a real source
sn cmdb meta cmdb_ci_server
sn cmdb relation add cmdb_ci_server <sys_id> --data '{"outbound_relations":[{"type":"<cmdb_rel_type_sys_id>","target":"<target_ci_sys_id>"}]}'
sn cmdb relation delete cmdb_ci_server <sys_id> <rel_sys_id> --yes
```

**Writes go out in the IRE envelope, `{"attributes": {...}, "source": "..."}`, which the CMDB Instance API demands** — a flat body is an HTTP 500 (`"attributes" is null`) on PATCH and a 400 on POST. `--field`/`--data` take the CI's fields **flat** and the CLI wraps them; you never write `attributes` yourself unless you want relations to ride along on create:

```bash
sn cmdb create cmdb_ci_server --data '{"attributes":{"name":"web-01"},"source":"Manual Entry",
  "outbound_relations":[{"type":"<rel_type_id>","target":"<ci_id>"}]}'   # object `attributes` = passthrough
```

- **`--source` defaults to `"Manual Entry"`**, which lands in `discovery_source`. A CLI write is a manual entry, so leave it alone unless you are genuinely standing in for a tool: the IRE reconciles by source, and borrowing a discovery tool's name lets that tool's next run overwrite the record. Valid values: `sn schema choices cmdb_ci discovery_source`. A bad one is exit 2 with the instance's `INVALID_INPUT_DATA` listing the choices in `detail`.
- **Attribute values go out as strings.** The API casts each to a Java String and answers a JSON number or boolean with an HTTP 500, so `--field cpu_count=8` sends `"8"`. An object or array value is refused up front (exit 1, naming the attribute); `null` is left alone and clears the field.
- **A flat body's top-level `source` is refused** (exit 1) rather than guessed at: to this API `source` is the record's provenance, but it is also a real column on `cmdb_ci_service_discovered`/`cmdb_ci_service_calculated`. Pass provenance as `--source`, or write the column with an explicit envelope: `{"attributes":{"source":"..."},"source":"<provenance>"}`. Giving both `--source` and a body `source` is exit 1 too.

## Import Sets

```bash
sn import create u_staging_table --field u_name=Server-01
sn import bulk u_staging_table --data '[{"u_name":"A"},{"u_name":"B"}]'   # array auto-wrapped as {"records":[...]}
sn import get u_staging_table <sys_id>
```

## Service Catalog

Browse → cart → checkout → submit, or `order` directly:

```bash
sn catalog list
sn catalog items --text "laptop"                 # search items
sn catalog item <sys_id>
sn catalog item-variables <sys_id>               # required form fields
sn catalog order <item_sys_id> --data '{"sysparm_quantity":"1"}'   # order immediately (bypass cart)
sn catalog add-to-cart <item_sys_id>
sn catalog cart
sn catalog cart-update <cart_item_id> --data '{"sysparm_quantity":"2"}'
sn catalog cart-remove <cart_item_id> --yes      # gated: --yes required off a TTY
sn catalog cart-empty <cart_sys_id> --yes        # gated; one DELETE, nothing restores the cart
sn catalog checkout                              # validate
sn catalog submit-order                          # place order
sn catalog wishlist
```

## Identification & Reconciliation

```bash
sn identify create-update --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01"}}]}'
sn identify query --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01"}}]}'
sn identify create-update-enhanced --data @payload.json --data-source "discovery" --options "partial_payload:true"
sn identify query-enhanced --data @query.json
```

## CICD (app / updateset / atf)

Async — `--wait` blocks until done (add `--wait-timeout <SECS>` to bound a stall, exit 3 on expiry). Poll running ops with `sn progress <id>`.

**Branch on the exit code, never on `status_label`.** `--wait` exits 0 only when the operation actually succeeded (`status` `"2"`). A failure is **exit 2 with empty stdout** (the progress object is on stderr under `.error.sn_error`, and the envelope carries **no `status_code`** — the HTTP call succeeded, the operation didn't); a timeout is **exit 3, also empty stdout** — so reading the command's stdout on a failure branch gets you nothing. `status_label` is ServiceNow's verbatim string ("Successful"/"Complete"/"Succeeded", varies by instance); matching on it is how you write a poll loop that never ends. When polling manually, key off the numeric `status`: `0` pending, `1` running, `2` successful, `3` failed, `4` cancelled.

```bash
sn app install --scope x_myapp --version 1.2.0 --wait --wait-timeout 900
sn app publish --scope x_myapp --version 1.3.0 --dev-notes "Bug fixes" --wait
sn app rollback --scope x_myapp --version 1.1.0 --wait --yes   # gated (see Destructive commands)
sn updateset create --name "Changes" --description "Sprint work"
sn updateset retrieve --update-set-id <id> --auto-preview
sn updateset preview <remote_update_set_id> --wait
sn updateset commit <remote_update_set_id> --wait
sn updateset commit-multiple --ids id1,id2,id3
sn updateset back-out --update-set-id <id> --wait --yes         # gated; reverts every record the set applied
sn atf run --suite-name "Regression Suite" --wait --wait-timeout 1800
sn atf results <result_id>
sn progress <progress_id>
```

## Scorecards (Performance Analytics)

```bash
sn scores list --per-page 20 --sort-by VALUE --sort-dir DESC
sn scores list --uuid <indicator_id> --include-scores --from 2026-01-01 --to 2026-04-01
sn scores favorite <uuid>
sn scores unfavorite <uuid>
```

## Utility & escape hatches

```bash
sn ping                                    # auth + identity + latency + build version (JSON)
sn user me                                 # currently authenticated user record
sn open incident <sys_id>                  # open the form in the default browser
sn open incident <sys_id> --print-url      # print the URL instead (for scripts)
sn raw GET /api/now/v2/table/incident -q sysparm_limit=5     # REST passthrough for unmodeled endpoints
sn raw POST /api/now/table/incident --data '{"short_description":"x"}'
sn raw PATCH /api/now/table/incident/<sys_id> --field state=2
sn raw DELETE /api/now/table/incident/<sys_id>
sn raw GET /api/now/table/incident -H 'X-no-response-body: true'   # repeatable; Authorization is rejected
sn graphql 'query { GlideRecord_Query { incident(pagination: {limit: 5}) { _rowCount _results { number { value } } } } }'
sn graphql @query.graphql --var id=<sys_id> --variables '{"limit": 5}' --operation Get
sn completion bash|zsh|fish|powershell|elvish   # zsh: > ~/.zsh/completions/_sn (dir on fpath + compinit)
sn introspect                              # full command tree as JSON (for MCP/tool generation)
```

`ping` is the identity check as well as the liveness check. It reports `ok`, `profile`, `instance`, `latency_ms`, `build_name`/`build_tag` (null when the instance doesn't publish those properties), and the caller as the **instance** names them:

```json
{"ok":true,"profile":"dev","instance":"dev12345.service-now.com","username":"abel.tuter",
 "identity_source":"sg/impersonation/session","user_sys_id":null,"user_display_name":null,
 "admin":false,"can_impersonate":false,"impersonating":false,"original_user":"abel.tuter",
 "latency_ms":351,"build_name":null,"build_tag":null}
```

`username` is what the instance asserts, not the configured one — when the two disagree the profile is what's wrong. `identity_source` says which endpoint answered (`sg/impersonation/session`, `ui/user/current_user`, `sys_user`, or `profile` when nothing named the caller, in which case the value is only an echo of the config). `admin`/`can_impersonate` are always present. `impersonating` is `true` only when two real names differ; `null` means unknown, never "no". Use `admin` to decide whether a privileged write is worth attempting — and see the exit-4 note above before treating a 403 as a login problem. `sn user me` returns the full `sys_user` row, read by sys_id with no scripted query on the wire; if the instance can't identify the caller it is exit 2 rather than a stranger's record.

`graphql` runs a document against `POST /api/now/graphql` — the whole GraphQL surface, including the generated `GlideRecord_Query`/`GlideRecord_Mutation`/`GlideAggregateRecord_Query` per-table namespaces (per-field display values, `_choices` on choice fields, `_table_metadata` ACL verdicts, `_reference` dot-walking, `_rowCount` totals). Success unwraps `data` (`--output raw` keeps the envelope). GraphQL fails **in-band** — HTTP 200 with an `errors` array — so errors exit 2 with the array under `sn_error`, and partial `data` still reaches stdout. `--var k=v` sets a string variable (first `=` splits); `--variables '{...}'` is a JSON object for typed values; `--var` wins on conflict.

`raw` emits the response exactly as ServiceNow returns it (no unwrapping); method is case-insensitive. `-H/--header 'Name: Value'` is repeatable and beats the header the client would otherwise send (`Content-Type` included); `Authorization` is rejected — identity comes from the profile. Both directions stay JSON: a non-JSON response fails to parse. `introspect` args carry `takes_value`, `value_name`, `positional`, `repeatable`, `default_values`, `aliases`, `possible_values`, `conflicts_with`, and `help_heading`; flags report `takes_value: false` — pass them bare (`--all`, not `--all true`). `conflicts_with` names the flags that cannot be combined (`--data` with `--field` is exit 1). The root carries `version` and `global_args`: **a command's effective flags are its own `args` plus the root's `global_args`** — the 11 propagated globals are emitted once, not repeated on all 130 nodes. Nothing named `help` appears in the tree, and `--help`/`--version` are omitted from `args[]`.

## Proxy & TLS

```bash
sn --proxy http://proxy:8080 table list incident    # http/https/socks5://
sn --insecure table list incident                   # skip TLS cert verification
sn --ca-cert /path/ca.pem table list incident       # custom CA
sn --no-proxy table list incident                   # bypass configured proxy
```

Env: `SN_PROXY`, `SN_NO_PROXY`, `SN_INSECURE=1`, `SN_CA_CERT`, `SN_PROXY_CA_CERT`. Per-profile in `config.toml`: `proxy`, `no_proxy`, `insecure`, `ca_cert`, `proxy_ca_cert` (proxy auth in `credentials.toml`). Precedence: CLI flag > env var > profile config.
