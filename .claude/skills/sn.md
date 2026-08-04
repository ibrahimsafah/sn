---
name: sn
description: Use when the user asks about ServiceNow data, incidents, change requests, problems, CIs, attachments, CMDB, service catalog, import sets, or any SNOW/SN operations. Also use when user says "sn", "servicenow", or references a ServiceNow instance, CICD operations (app install/publish/rollback, update sets, ATF tests), aggregate statistics, Performance Analytics scorecards, CI reconciliation, or watching records change in real time (record watchers / live updates / AMB websocket).
---

# sn — ServiceNow CLI

Single binary wrapping ServiceNow's REST APIs (one per section below). Machine contract: JSON on stdout, JSON errors on stderr, deterministic exit codes, no interactive surprises.

Install: `brew install tehubersheezy/sn/sn` or https://github.com/tehubersheezy/servicenow-cli

## Setup & profiles

```bash
sn profile add prod --instance X --username Y --password-stdin < secret.txt   # AGENT-SAFE: never prompts
sn init                                          # human wizard: prompts, and CLAIMS default_profile
sn ping                                          # verify connectivity + credentials + build version
sn -p prod table list incident                  # pick profile per command (-p = --profile)
sn profile list                                  # also: add <name> / show <name> / use <name> / remove <name>
```

**Use `sn profile add`, not `sn init`.** It emits JSON on stdout and never prompts when stdin isn't a TTY (a missing field is exit 1 naming the flag, not a hang). It also leaves `default_profile` alone — `sn init` takes it over. Pipe secrets via `--password-stdin` / `--client-secret-stdin`; `--password` is visible in `ps` and shell history.

`add` verifies the credentials against the instance and **writes nothing if they're rejected** (exit 4), so you never inherit a broken profile. Exit 1 if the profile exists (`--force` to overwrite) or a flag is missing. `--no-verify` skips the network; `--set-default` also makes it the default.

Profile selection: `--profile`/`-p` > `default_profile` in config > error (no implicit fallback). `SN_CONFIG_DIR` overrides the config dir (`config.toml`/`credentials.toml`). No env var sets credentials or selects a profile (proxy/TLS env vars excepted — see Proxy & TLS).

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
- **Exit codes**: `0` ok · `1` usage/config (incl. bad flags) · `2` API error · `3` network/transport (incl. `--wait-timeout` expiry) · `4` auth (401/403; OAuth → prompt human to `sn auth login`). Branch on exit code first, parse stdout second.
- **`--output default|raw|table`**: `default` = unwrapped JSON (pipe to jq); `raw` = full `{"result": ...}` envelope; `table` = human columns (interactive only, don't pipe).

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
```

## Watch (live record changes)

Streams record changes over ServiceNow's AMB websocket. **JSONL on stdout, one event per line, flushed as it arrives.**

```bash
# Bound the stream, or it runs until interrupted.
sn watch table incident -q "priority=1^active=true" --max-events 5
sn watch table incident --sys-id <SYS_ID> --duration 60      # stop after 60s
sn watch table incident -q "active=true" --idle-timeout 30   # stop after 30s of quiet

sn watch table incident -q "active=true" --operation insert           # only new records
sn watch table incident -q "active=true" --on-change state,priority   # only these fields
sn watch count incident -q "active=true"                              # count deltas
sn watch activity <SYS_ID>                                            # comments / work notes
sn watch channel '/uxbannerannouncements'                             # raw AMB channel
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
- **`sn watch count` emits a delta, not a total**: `{"count":"+1"}` / `{"count":"-1"}` (strings). Seed with `sn aggregate <TABLE> --count` and accumulate.
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
```

## Attachments

```bash
sn attachment list --query "table_name=incident"
sn attachment get <sys_id>
sn attachment upload --table incident --record <record_id> --file ./report.pdf
sn attachment download <sys_id> --out ./file.pdf       # -o too; NOT --output (that's the output MODE)
sn attachment delete <sys_id> --yes
```

## CMDB

```bash
sn cmdb list cmdb_ci_server --query "operational_status=1"
sn cmdb get cmdb_ci_server <sys_id>       # ⚠️ CI fields nest under .attributes — use .attributes.name,
                                          #    NOT .name. Top level is only {attributes,
                                          #    inbound_relations, outbound_relations}.
sn cmdb create cmdb_ci_server --field name=web-01 --field ip_address=10.0.1.1
sn cmdb update cmdb_ci_server <sys_id> --field operational_status=2
sn cmdb meta cmdb_ci_server
sn cmdb relation add cmdb_ci_server <sys_id> --data '{"outbound_relations":[{"type":"<cmdb_rel_type_sys_id>","target":"<target_ci_sys_id>"}]}'
sn cmdb relation delete cmdb_ci_server <sys_id> <rel_sys_id> --yes
```

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

**Branch on the exit code, never on `status_label`.** `--wait` exits 0 only when the operation actually succeeded (`status` `"2"`). A failure is **exit 2 with empty stdout** (the progress object is on stderr under `.error.sn_error`); a timeout is **exit 3, also empty stdout** — so reading the command's stdout on a failure branch gets you nothing. `status_label` is ServiceNow's verbatim string ("Successful"/"Complete"/"Succeeded", varies by instance); matching on it is how you write a poll loop that never ends. When polling manually, key off the numeric `status`: `0` pending, `1` running, `2` successful, `3` failed, `4` cancelled.

```bash
sn app install --scope x_myapp --version 1.2.0 --wait --wait-timeout 900
sn app publish --scope x_myapp --version 1.3.0 --dev-notes "Bug fixes" --wait
sn app rollback --scope x_myapp --version 1.1.0 --wait
sn updateset create --name "Changes" --description "Sprint work"
sn updateset retrieve --update-set-id <id> --auto-preview
sn updateset preview <remote_update_set_id> --wait
sn updateset commit <remote_update_set_id> --wait
sn updateset commit-multiple --ids id1,id2,id3
sn updateset back-out --update-set-id <id> --wait
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
sn ping                                    # auth + latency + build version (JSON)
sn user me                                 # currently authenticated user record
sn open incident <sys_id>                  # open the form in the default browser
sn open incident <sys_id> --print-url      # print the URL instead (for scripts)
sn raw GET /api/now/v2/table/incident -q sysparm_limit=5     # REST passthrough for unmodeled endpoints
sn raw POST /api/now/table/incident --data '{"short_description":"x"}'
sn raw PATCH /api/now/table/incident/<sys_id> --field state=2
sn raw DELETE /api/now/table/incident/<sys_id>
sn raw GET /api/now/table/incident -H 'X-no-response-body: true'   # repeatable; Authorization is rejected
sn completion bash|zsh|fish|powershell|elvish   # zsh: > ~/.zsh/completions/_sn (dir on fpath + compinit)
sn introspect                              # full command tree as JSON (for MCP/tool generation)
```

`raw` emits the response exactly as ServiceNow returns it (no unwrapping); method is case-insensitive. `-H/--header 'Name: Value'` is repeatable and beats the header the client would otherwise send (`Content-Type` included); `Authorization` is rejected — identity comes from the profile. Both directions stay JSON: a non-JSON response fails to parse. `introspect` args carry `takes_value`, `value_name`, `positional`, `repeatable`, `default_values`, `aliases`, `possible_values`, `conflicts_with`, and `help_heading`; flags report `takes_value: false` — pass them bare (`--all`, not `--all true`). `conflicts_with` names the flags that cannot be combined (`--data` with `--field` is exit 1). The root carries `version` and `global_args`: **a command's effective flags are its own `args` plus the root's `global_args`** — the 11 propagated globals are emitted once, not repeated on all 121 nodes. Nothing named `help` appears in the tree, and `--help`/`--version` are omitted from `args[]`.

## Proxy & TLS

```bash
sn --proxy http://proxy:8080 table list incident    # http/https/socks5://
sn --insecure table list incident                   # skip TLS cert verification
sn --ca-cert /path/ca.pem table list incident       # custom CA
sn --no-proxy table list incident                   # bypass configured proxy
```

Env: `SN_PROXY`, `SN_NO_PROXY`, `SN_INSECURE=1`, `SN_CA_CERT`, `SN_PROXY_CA_CERT`. Per-profile in `config.toml`: `proxy`, `no_proxy`, `insecure`, `ca_cert`, `proxy_ca_cert` (proxy auth in `credentials.toml`). Precedence: CLI flag > env var > profile config.
