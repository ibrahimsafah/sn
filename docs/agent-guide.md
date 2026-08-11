# `sn` agent usage guide

One-time read for an LLM agent that reads, creates, updates, and deletes
ServiceNow records via `sn`. Assume zero prior ServiceNow knowledge — every
operation below is runnable from a cold start after `sn init`.

`sn` is a Rust CLI wrapping ServiceNow's REST APIs: Table, Change Management,
Attachment, CMDB (Instance + Meta), Import Set, Service Catalog, Identification
& Reconciliation, CICD (App Repository, Update Sets, ATF), Aggregate,
Performance Analytics, and two schema-discovery endpoints. It emits JSON on
stdout, structured JSON errors on stderr, and stable exit codes.

## Output, errors & exit codes (read first)

**stdout is always JSON** — pretty-printed on a TTY, compact when piped. The
default shape is **unwrapped**: `sn` strips ServiceNow's `{"result": ...}`
envelope. `list` and `schema` commands return an array; `get`/`create`/`update`
return one record object.

Three output modes via `--output`:
- `default` — unwrapped JSON (above).
- `raw` — preserves the full `{"result": ...}` envelope.
- `table` — columnar text table for humans (don't parse it).

```bash
sn table get incident abc123 --output raw
# {"result":{"sys_id":"abc123","number":"INC0010001","short_description":"Mail server down"}}
```

Non-obvious shapes worth knowing:

| Command | stdout |
|---|---|
| `table delete`, `change delete`, `attachment delete` | empty (exit code signals success) |
| `profile use`, `profile remove` | empty (exit code signals success) |
| `init` | empty stdout — it reports to a **human on stderr**. Use `profile add` if you need JSON. |
| `attachment download` | raw bytes, or `{"path","size"}` with `--out <file>` |
| `aggregate` | `{"stats":{...}}` — but with `--group-by`, an **array** of `{groupby_fields,stats}` |
| `app` / `updateset` / `atf run` | progress object with `status_label` + `links.progress.id` |
| `progress` | `{status_label, percent_complete, status_message}` |
| `ping` | `{ok, profile, instance, username, latency_ms, build_name, build_tag}` |

**stderr is always a JSON error object on any non-zero exit:**

```json
{
  "error": {
    "message": "Record not found",
    "detail": "No record with sys_id 'abc123' in table 'incident'",
    "status_code": 404,
    "transaction_id": "3f4ab12c8d0001",
    "sn_error": {"message": "No Record found", "detail": "ACL restricts the record retrieval"}
  }
}
```

`sn_error` is ServiceNow's original payload verbatim (null for transport/CLI
errors — check `.error.message` first). `transaction_id` is SN's correlation id,
useful for support requests.

**Exit codes — branch on these before parsing stdout:**

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Usage / config / parse error (bad flags, unreadable file, malformed JSON, mixing `--data` + `--field`, bad proxy URL / CA file) |
| 2 | API error — ServiceNow 4xx/5xx other than auth (400 bad table, 404 not-found, 403 ACL, 429 rate-limit, 5xx) |
| 3 | Network / transport (DNS, connection refused, timeout, TLS handshake, proxy unreachable) |
| 4 | Auth (401, or 403 from the login itself) |

Code 4 means "credentials are wrong"; code 2 with `status_code: 403` means
"authenticated, but ACL forbids it." Recommended agent pattern:

```bash
out=$(sn table get incident "$sysid" 2>/tmp/sn.err)
case $? in
  0) jq -r '.short_description' <<<"$out" ;;
  2) [ "$(jq -r '.error.status_code' /tmp/sn.err)" = 404 ] && exit 0   # not found — nothing to do
     jq -r '.error.message' /tmp/sn.err >&2; exit 1 ;;
  3) echo "transport failure — check connectivity" >&2; exit 1 ;;
  4) echo "auth failed — OAuth: 'sn auth login'; basic: re-add the profile" >&2; exit 1 ;;
esac
```

**Verbose debugging** (stderr only; never required to parse output):

| Flag | Adds to stderr |
|---|---|
| `-d` | method, URL, status, elapsed per request |
| `-dd` | + response headers |
| `-ddd` | + request/response bodies (Authorization/Set-Cookie headers and OAuth token values masked to `****`) |

Turn on `-d` after an exit 2/3 to see the exact URL built — a sysparm typo
producing a malformed path is a common 404 cause. Verbose text is free-form and
may change between versions; only the stderr error object is structured.

## Setup & profiles

```bash
sn init                    # interactive wizard: prompts, then claims default_profile
sn profile add prod ...    # scriptable: adds a profile, leaves default_profile alone
sn profile list            # also: show <name> / use <name> / remove <name>
sn ping                    # verify auth + latency + build version (the health check)
```

**`sn profile add` is the one to reach for.** It is the agent-safe half of the pair:
it emits JSON on stdout, and it **never prompts when stdin is not a terminal** — a
missing field is exit 1 naming the flag, not a blocked read. `sn init` is a human
wizard that also takes over `default_profile`, which is rarely what you want when
adding an instance to an existing setup.

```bash
sn profile add ci --instance dev12345 --username svc --password-stdin < secret.txt
# → {"auth":"basic","default":false,"instance":"dev12345.service-now.com","next":"sn profile use ci",
#    "ok":true,"profile":"ci","user":"svc","verified":true}
```

Keys come back sorted. `"next"` appears only when it has something to tell you — here, that no
default profile is selected yet, so this one needs `sn profile use ci` or an explicit `--profile ci`.

Pipe secrets in rather than passing `--password` / `--client-secret`, which any
process can read out of `ps` and which land in shell history.

`add` always checks the credentials against the instance, and **a profile that
fails the check is not written at all** — you never inherit a half-configured
identity that breaks somewhere confusing later. Its contract:

| situation | exit | effect |
|---|---|---|
| ok | 0 | profile written, `"verified":true` |
| profile already exists | 1 | nothing written — pass `--force` |
| required flag missing (no TTY) | 1 | nothing written — message names the flag |
| credentials rejected | 4 | **nothing written** |
| `--no-verify` | 0 | written unverified, no network call |

`--set-default` also makes it the default; otherwise use `sn profile use <name>`,
or pass `--profile <name>` per command. When no default profile exists, `add` says
so in a `"next"` field.

Secrets go to `credentials.toml` (chmod 600 on Unix; the per-user `%APPDATA%` ACL
on Windows) and non-secret config to `config.toml`, under `~/.config/sn/` (Linux),
`~/Library/Application Support/sn/` (macOS), or `%APPDATA%\sn\` (Windows).
`SN_CONFIG_DIR` relocates that directory (used as-is — no `sn` subdir appended);
it's the supported way to sandbox `sn` for CI or ephemeral sessions:

```bash
SN_CONFIG_DIR=/tmp/sn-sandbox sn profile add ci --instance dev12345 \
  --username svc --password-stdin < secret.txt
SN_CONFIG_DIR=/tmp/sn-sandbox sn --profile ci table list incident --limit 1
```

**OAuth.** `sn profile add --auth oauth --grant client_credentials` works headlessly
(the token is minted and verified). The default `authorization_code` grant needs a
browser, so there is nothing an agent can verify: `add` refuses on a non-TTY rather
than save an untested profile. Register it with `--no-verify` and have a human run
`sn auth login --profile <name>`. Session state is `sn auth status` / `refresh` /
`logout`; tokens then refresh transparently on every command.

**Profile selection** (highest precedence first): `--profile <name>` →
`default_profile` in `config.toml` → error (`no profile selected`, exit 1). There
is no implicit fallback.

`-p` is a short alias for `--profile`. Both are global, so either may appear
before or after the subcommand:

```bash
sn --profile prod table list incident --limit 5
sn table list incident --limit 5 -p prod          # identical
```

A profile is the single unit of identity — instance URL + credentials together.
There is no way to graft a one-off instance/username/password onto a command,
and there are no env vars for credentials or profile selection: configure a
profile with `sn profile add` (or `sn init`). Only proxy/TLS and the config *directory* are
env-overridable (precedence: CLI flag > env var > per-profile config field):

| CLI flag | Env var | Effect |
|---|---|---|
| `--proxy <URL>` | `SN_PROXY` | HTTP/HTTPS/SOCKS5 proxy |
| `--no-proxy` | `SN_NO_PROXY` | bypass proxy (env is comma-separated hosts) |
| `--insecure` | `SN_INSECURE=1` | skip TLS cert verification (off by default) |
| `--ca-cert <PATH>` | `SN_CA_CERT` | custom CA for the instance |
| `--proxy-ca-cert <PATH>` | `SN_PROXY_CA_CERT` | custom CA for the proxy |

Proxy auth and the same settings can also live per-profile in the config files
(`proxy`, `no_proxy`, `insecure`, `ca_cert`, `proxy_ca_cert`, `proxy_username`,
`proxy_password`).

## Discovery flow

When you don't know a table's schema, discover it before writing.

```bash
sn schema tables --filter incident        # fuzzy match name or label
```
```json
[{"value":"incident","label":"Incident","rawLabel":"Incident","reference":false,"sequence":-1,
  "image":"","missing":false,"selected":false,"used":false}]
```

⚠️ **The table name is `value`, not `name`.** This endpoint returns a picker-style
list, so `jq -r '.[].name'` yields `null` for every row. Use `jq -r '.[].value'`.
There is no `super_class`, `is_extendable`, or `sys_id` here.

```bash
sn schema columns incident --writable      # mandatory fields, types, references
```
```json
[
  {"name":"short_description","type":"string","internal_type":"string","max_length":160,
   "mandatory":true,"read_only":false,"default":"","label":"Short description"},
  {"name":"caller_id","type":"reference","mandatory":false,"reference":"sys_user",
   "reference_display_field":"name","label":"Caller"},
  {"name":"state","type":"choice","internal_type":"integer","mandatory":true,"default":"1",
   "choice_type":"dropdown","choices":[{"value":"1","label":"New"},{"value":"2","label":"In Progress"}]}
]
```

⚠️ There is **no `choice_field`** and **no `default_value`**. The default is `default`.
A choice column is one whose **`type` is `"choice"`** — note its `internal_type` may
still be `"integer"` — and its options are inlined in a **`choices[]`** array, so you
often don't need a second `schema choices` call. `reference` is *absent* on
non-reference columns rather than `null`.

`columns` filters: `--writable` (excludes read-only), `--mandatory`,
`--filter <substr>` (name or label), `--references-only`, `--choices-only`,
`--type <type>` (e.g. `string`, `integer`, `reference`).

```bash
sn schema choices incident state           # valid values for a choice field
```
```json
[{"value":"1","label":"New"},{"value":"2","label":"In Progress"},{"value":"6","label":"Resolved"},{"value":"7","label":"Closed"}]
```

The numeric `value` is what you send to write APIs; the `label` is what
`--display-value true` returns on reads. Now write with confidence:

```bash
sn table create incident --field short_description="server down" --field state=2 --field priority=1
```

(Example values throughout are illustrative; real values depend on your instance.)

## Reading records (`list`, `get`)

```bash
# List with a cap, filter, and column projection
sn table list incident --query "active=true^priority=1" --fields "number,short_description,state" --setlimit 10
```
```json
[{"number":"INC0010001","short_description":"Mail server down","state":"In Progress"}]
```

`state` reads as a label, not `"2"`, because `--display-value` defaults to
`true` — see [Display values](#display-values) before you feed any of it back.

`--limit` aliases `--setlimit` (SN's `sysparm_limit`); the default is 1000 per
page on `table list`, `change list`, and `cmdb list`, and 100 on `change task
list`, `attachment list`, `catalog categories`, and `catalog items`. Drop it low
(`--setlimit 5`) for exploration.

```bash
# Get one record by sys_id (get takes a sys_id only — no --query)
sn table get incident a1b2c3d4e5f6
```

To find one record by criteria, use `list --limit 1 --query "..."` and read `[0]`.

**The read verb may be omitted on `table` and `cmdb`.** Both map to a REST path
that is already `{noun}/{id}`, so for a read the verb is implied by whether you
supplied an id:

```bash
sn table incident a1b2c3d4e5f6      # = sn table get incident a1b2c3d4e5f6
sn table incident                   # = sn table list incident
sn cmdb cmdb_ci_server ci001        # = sn cmdb get cmdb_ci_server ci001
```

Only `get` and `list` are ever inferred — **never a write** — and the choice is
decided rather than guessed: `get` requires a second positional and `list`
rejects one, so at most one of them can parse any given argv. A *near-miss* of a
real verb stays a typo: `sn table lst incident` returns
`tip: a similar subcommand exists: 'list'` rather than reading a table named
`lst`.

**The flip side, worth knowing when debugging:** a first token that is *not*
close to any real verb is taken as a table name, because that is exactly what
the shorthand is for. So `sn table bogus-verb incident` is not a usage error —
it reads a table called `bogus-verb` and fails with ServiceNow's
table-not-found (exit **2**), not `unrecognized subcommand` (exit 1). If you
get an unexpected "invalid table" on `table` or `cmdb`, check whether you
misspelled the *verb*.

Groups whose first token isn't reliably a noun (`change`, `catalog`) are
excluded from the shorthand, and an unrecognized subcommand there lists the
valid ones:

```
$ sn change sc_cat_item abc
error: unrecognized subcommand 'sc_cat_item'
  `sn change` subcommands: list, get, create, update, delete, nextstates,
  approvals, risk, schedule, task, ci, conflict, models, templates
```

### Display values

**`--display-value` defaults to `true`.** Reads come back as readable labels —
`state` is `"In Progress"`, not `"2"`; `assigned_to` is `"Abel Tuter"`, not a
sys_id. This changed in 0.11.0; before that the default was `false`.

| Value | Effect | Use when |
|---|---|---|
| `true` (default) | display labels | reading, reporting, showing a human |
| `false` | raw values and sys_ids | feeding a value back into a query or a write |
| `all` | both — each field becomes `{"value","display_value"}` | you need to show one and send the other |

```bash
sn table get incident a1b2c3d4e5f6 --display-value all
# ... "state":{"value":"2","display_value":"In Progress"}, "priority":{"value":"1","display_value":"1 - Critical"}
```

**Two consequences that will bite a script.** Dates are also rewritten, into the
calling user's timezone and locale — `2026-08-04 14:22:01` comes back as
`08/04/2026 10:22:01 AM`, which **will not parse back into an encoded query**.
And a reference field yields a name where you needed the sys_id to chain into
the next call.

So: **anything you intend to send back to ServiceNow must be read with
`--display-value false`** (or `all`, reading the `value` side). Never echo a
`display_value` into an update or a `--query`.

```bash
sn table get incident a1b2c3d4e5f6 --display-value false     # raw, round-trippable
sn table list incident --query "sys_created_on>2026-08-01" --display-value false --fields sys_id
```

The default applies to `table`, `change`, `aggregate`, and `scores`. `sn watch`
is unaffected — there `--display-value` only narrows a `--hydrate` fetch.

### Pagination & bulk processing

ServiceNow caps any single response. `--all` follows the `Link: rel="next"`
header and streams **every** matching record as JSONL — one object per line, so
you can pipe to `jq -c` without buffering the whole set:

```bash
sn table list incident --query "active=true" --all
sn table list incident --query "active=true" --all --array            # one JSON array instead (buffers in memory)
sn table list incident --query "active=true" --all --max-records 1000 # safety cap
sn table list incident --query "active=true" --all --setlimit 5000    # larger per-call batches
```

`--setlimit` is the per-API-call batch size under `--all`; `--offset` is ignored
in `--all` mode. Don't compute offsets by hand. For a single manual page, use
`--setlimit`+`--offset` without `--all`. Processing JSONL:

```bash
sn table list incident --query "active=true^priority=1" --all | jq -r '.number'           # extract a field
sn table list incident --all | jq -c 'select(.short_description|test("mail";"i"))'         # client-side filter
sn table list incident --all | jq -s 'group_by(.state)|map({state:.[0].state,count:length})' # group + count
sn table list incident --query "state=6^ORstate=7" --all | jq -r '.sys_id' \
  | while read -r sid; do sn table update incident "$sid" --field active=false; done        # stream into updates
```

### Encoded query syntax

`--query` takes a ServiceNow "encoded query." Build incrementally — run with
`--limit 1` first to sanity-check syntax, then widen.

| Operator | Meaning | Example |
|---|---|---|
| `=` / `!=` | equals / not equals | `state=2`, `state!=7` |
| `>` `>=` `<` `<=` | numeric/date compare | `priority<=2` |
| `LIKE` / `STARTSWITH` / `ENDSWITH` | contains / prefix / suffix | `short_descriptionLIKEmail` |
| `IN` / `NOT IN` | value in / not in comma list | `stateIN1,2,3` |
| `ISEMPTY` / `ISNOTEMPTY` | null check | `assigned_toISEMPTY` |
| `^` / `^OR` / `^NQ` | AND / OR / new query (OR across groups) | `active=true^priority=1` |
| `ORDERBY` / `ORDERBYDESC` | ascending / descending sort | `ORDERBYDESCsys_created_on` |

```bash
# Priority 1 or 2, active, newest first
sn table list incident --query "active=true^priority=1^ORpriority=2^ORDERBYDESCsys_created_on" --limit 20
# Assigned to a user (sys_id) or unassigned
sn table list incident --query "assigned_to=6816f79c...^ORassigned_toISEMPTY"
```

## Writing records (`create`, `update`, `delete`)

**Body input** — two mutually exclusive ways (mixing them is exit 1):
- `--field name=value` (repeatable): cleanest for a few fields, no JSON
  escaping. Values are sent as strings; ServiceNow coerces per column type.
- `--data`: full JSON payload — needed for nested objects, arrays, or explicit
  nulls. Accepts inline JSON, `@file`, or `@-` (stdin).

```bash
sn table create incident --field short_description="Server CPU spike" --field caller_id=6816f79c... --field urgency=2
sn table create incident --data '{"short_description":"Printer jam in 3B","urgency":"3"}'
sn table create incident --data @body.json
jq -n '{short_description:"from pipe",urgency:"3"}' | sn table create incident --data @-
```

**`update` = PATCH** — only the named fields change; everything else is
untouched. Almost always what you want:

```bash
sn table update incident c7d8e9f0a1b2 --field state=2 --field work_notes="Investigating"
```

`update` is the **only** write verb. There is no `replace`: it issued PUT where
`update` issues PATCH, but ServiceNow applies both as partial updates — omitted
fields keep their values and nothing is blanked — so the two did the same thing
while implying they did not. It was removed in 0.11.0; `sn table replace` now
exits 1. Anything that called it should call `update` with the same body.

To clear a field, send it explicitly empty: `--field description=""`.

**`delete`** returns exit 0 with empty stdout. Non-interactive runs must pass
`--yes` — without it, a non-TTY invocation exits 1 with a usage error (a TTY
gets a `[y/N]` prompt). The same guard covers `change delete`, `change task
delete`, `attachment delete`, and `cmdb relation delete`:

```bash
sn table delete incident c7d8e9f0a1b2 --yes
```

**Writing by display value:** if you have a label ("In Progress") instead of a
raw value ("2"), add `--input-display-value` so ServiceNow resolves labels on
input. Resolution can be ambiguous (two users named "Alice"); prefer raw sys_ids
for references.

```bash
sn table update incident c7d8e9f0a1b2 --input-display-value --field state="In Progress"
```

On writes, `--fields` narrows only the *response*, never the request body.

## Shared parameter reference

Friendly flags map to ServiceNow `sysparm_*` params; both names work. These
apply across `table` and most other command groups.

| Friendly flag | sysparm | Applies to | Notes |
|---|---|---|---|
| `--query <EQ>` | `sysparm_query` | list | Encoded query |
| `--fields <csv>` | `sysparm_fields` | list/get/create/update | Columns to return |
| `--setlimit <N>` | `sysparm_limit` | list | Max/page; default 1000 (`table`/`change`/`cmdb` list) or 100 (`change task`, `attachment`, `catalog categories`/`items`). Aliases `--limit`, `--page-size` |
| `--offset <N>` | `sysparm_offset` | list | Page offset |
| `--display-value <false\|true\|all>` | `sysparm_display_value` | list/get/create/update | See Display values |
| `--input-display-value` | `sysparm_input_display_value` | create/update | Resolve labels in request body |
| `--exclude-reference-link` | `sysparm_exclude_reference_link` | list/get/create/update | Drop `link` URL from references |
| `--view <name>` | `sysparm_view` | list/get | Named form/list view |
| `--query-no-domain` | `sysparm_query_no_domain` | list/get/update/delete | Cross-domain if authorized |
| `--no-count` / `--suppress-pagination-header` | `sysparm_no_count` / `sysparm_suppress_pagination_header` | list | Skip count query (faster on big tables) |
| `--suppress-auto-sys-field` | `sysparm_suppress_auto_sys_field` | create/update | Skip system-field auto-gen |
| `--all` / `--array` / `--max-records <N>` | (CLI only) | list | Auto-paginate / array output / cap |
| `--query-category <cat>` | `sysparm_query_category` | list | Index selection |
| `--output`, `--profile`, `-d`/`-dd`/`-ddd` | (CLI only) | all | See relevant sections |
| `--yes` / `-y` | (CLI only) | **`delete` subcommands only** — not global | Skip the confirmation; required on a non-TTY |

## Aggregate

`sn aggregate` → `GET /api/now/stats/{table}`: server-side count/sum/avg/min/max
in one round trip, instead of paginating and counting client-side.

```bash
sn aggregate incident --count                       # ungrouped → ONE object
```
```json
{"stats":{"count":"142"}}
```

```bash
sn aggregate incident --count --group-by state      # grouped → an ARRAY, one entry per group
```
```json
[
  {"groupby_fields":[{"field":"state","value":"1"}],"stats":{"count":"15"}},
  {"groupby_fields":[{"field":"state","value":"2"}],"stats":{"count":"20"}},
  {"groupby_fields":[{"field":"state","value":"7"}],"stats":{"count":"27"}}
]
```

⚠️ **`--group-by` changes the top-level type from object to array**, and
`groupby_fields` is a **sibling** of `stats`, not a member of it. The count for a
group lives at `.stats.count`, and the group's value at
`.groupby_fields[0].value` — so `jq '.stats.groupby_fields[]'` returns nothing.
To read groups:

```bash
sn aggregate incident --count --group-by state \
  | jq -r '.[] | "\(.groupby_fields[0].value)\t\(.stats.count)"'
```

`sum`/`avg`/`min`/`max` nest **per field** rather than being scalars:
`{"stats":{"sum":{"reassignment_count":"24"},"min":{"priority":"1"}}}`.

```bash
# Combine aggregations and filter server-side
sn aggregate incident --sum-fields reassignment_count --min-fields priority --max-fields priority --query "active=true"
```

Flags: `--count`, `--group-by <csv>`, `--avg-fields`/`--sum-fields`/
`--min-fields`/`--max-fields <csv>`, `--query <EQ>`, `--having <expr>`,
`--order-by <csv>`, `--display-value`.

## Change Management

`sn change` wraps `/api/sn_chg_rest/change`. Three types — **normal**,
**emergency**, **standard**; `--type` targets a type-specific endpoint (omit for
the generic one). Standard changes **require** `--template`.

⚠️ **Unlike the Table API, the Change API returns every field as a
`{display_value, value}` pair.** `.number` is an *object*, not a string:

```json
{"number":{"display_value":"CHG0000024","value":"CHG0000024"},
 "state":{"display_value":"Closed","value":3.0}}
```

So `jq -r '.number'` prints a JSON blob, not `CHG0000024` — you want
`jq -r '.number.value'`. Note `state.value` comes back as a **number** (`3.0`),
while the Table API would give you the string `"3"`.

```bash
sn change list --type normal --query "state=1^priority<=2" --setlimit 10
sn change get chg001 --type normal
sn change create --type normal --field short_description="DB migration" --field category=software
sn change create --type standard --template <template_sys_id>
sn change update chg001 --field state=2
sn change delete chg001 --yes
```

**Workflow** — call `nextstates` before changing state to avoid
invalid-transition errors:

```bash
sn change nextstates chg001
# {"available_states":["3"],"state_label":{"3":"Closed"},"state_transitions":[]}
#   ^ an OBJECT, not a list of {value,label}: the legal next states are the strings
#     in .available_states, and .state_label maps each to its display name.
#     e.g.  jq -r '.available_states[] as $s | "\($s)\t\(.state_label[$s])"'
sn change approvals chg001 --field approval="approved"
sn change risk chg001 --data '{"risk_value":"moderate"}'
sn change schedule chg001
sn change models          # browse change models
sn change templates       # browse standard-change templates
```

**Sub-resources** — tasks, affected CIs, conflicts:

```bash
sn change task list <change_sys_id>
sn change task create <change_sys_id> --field short_description="Pre-check"
sn change task update <change_sys_id> <task_sys_id> --field state=2
sn change task delete <change_sys_id> <task_sys_id> --yes
sn change ci list <change_sys_id>
sn change ci add <change_sys_id> --data '{"cmdb_ci_sys_id":"<ci_id>"}'
sn change conflict get <sys_id>          # also: conflict add / conflict remove
```

## Attachments

`sn attachment` wraps `/api/now/attachment` — binary upload/download for any
record. Content type is auto-detected from file extension; override with
`--content-type`.

```bash
sn attachment list --query "table_name=incident" --setlimit 20
sn attachment get att001
sn attachment upload --table incident --record <record_sys_id> --file ./report.pdf
sn attachment download att001 --out ./downloaded.png       # {"path":"./downloaded.png","size":245760}
sn attachment download att001 > file.bin                   # or raw bytes to stdout
sn attachment delete att001 --yes
```

## CMDB

`sn cmdb` combines the Instance API (`/api/now/cmdb/instance/{class}`, CRUD +
relations) and Meta API (`/api/now/cmdb/meta/{class}`, schema). The class name
is always the first positional arg.

```bash
sn cmdb list cmdb_ci_server --query "operational_status=1" --setlimit 10
sn cmdb get cmdb_ci_server ci001                        # includes relations
sn cmdb create cmdb_ci_server --field name=web-server-02 --field ip_address=10.0.1.51
sn cmdb update cmdb_ci_server ci001 --field operational_status=2   # PATCH; the only write verb
sn cmdb meta cmdb_ci_server                             # class schema
sn cmdb relation add cmdb_ci_server ci001 --data '{"outbound_relations":[{"type":"<cmdb_rel_type_sys_id>","target":"<target_ci_sys_id>"}]}'
sn cmdb relation delete cmdb_ci_server ci001 <rel_sys_id> --yes
```

⚠️ **`cmdb get` nests the CI's fields under `attributes`.** The top level has
exactly three keys — `attributes`, `inbound_relations`, `outbound_relations` — so
the CI's name is `.attributes.name`, not `.name`:

```bash
sn cmdb get cmdb_ci_server ci001 | jq -r '.attributes.name'
sn cmdb get cmdb_ci_server ci001 | jq '.outbound_relations[] | {type, target}'
```

## Import Sets

`sn import` wraps `/api/now/import/{stagingTable}` — loads data through transform
maps. The result reports each transform outcome (`status`: `inserted`,
`updated`, `skipped`, or `error`).

```bash
sn import create u_my_staging_table --field u_name="Server-01" --field u_ip="10.0.1.1"
sn import bulk u_my_staging_table --data '[{"u_name":"Server-01","u_ip":"10.0.1.1"},{"u_name":"Server-02","u_ip":"10.0.1.2"}]'
sn import bulk u_my_staging_table --data @records.json
sn import get u_my_staging_table imp001
```

## Service Catalog

`sn catalog` wraps `/api/sn_sc/servicecatalog` — browse, cart, order. Call
`item-variables` before ordering to discover required form fields (those with
`mandatory: true` must be in the order payload).

```bash
# Browse
sn catalog list [--text "IT"]
sn catalog get <catalog_sys_id>
sn catalog categories <catalog_sys_id> [--top-level-only]
sn catalog category <category_sys_id>
sn catalog items --text "laptop" [--category <id>] [--catalog <id>]
sn catalog item <item_sys_id>
sn catalog item-variables <item_sys_id>
```

Two ordering paths — **order now** (immediate) or the **cart workflow**:

```bash
sn catalog order <item_sys_id> --data '{"sysparm_quantity":"1","variables":{"urgency":"high"}}'  # {"request_number":"REQ0010001","request_id":"req001"}

sn catalog add-to-cart <item_sys_id> --data '{"sysparm_quantity":"1"}'
sn catalog cart                         # view; then cart-update <id> / cart-remove <id> / cart-empty <cart_sys_id>
sn catalog checkout                     # validate
sn catalog submit-order                 # place order
sn catalog wishlist
```

## Identification & Reconciliation

`sn identify` wraps `/api/now/identifyreconcile` — CI create/update through the
reconciliation engine, which decides insert-vs-update from identification rules.
POST-only; all operations take `--data` for the items payload.

```bash
sn identify create-update --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01","ip_address":"10.0.1.1"}}]}'
```
```json
{"items":[{"sysId":"ci001","className":"cmdb_ci_server","operation":"INSERT","identifierEntrySysId":"id001"}]}
```

```bash
sn identify query --data '{"items":[{"className":"cmdb_ci_server","values":{"name":"web-01"}}]}'   # identify without modifying
```

**Enhanced variants** support partial payloads/commits via `--options`
(comma-separated `key:value`) and `--data-source <name>` (tags the audit trail):

```bash
sn identify create-update-enhanced --data @payload.json --data-source "my_discovery" --options "partial_payload:true,partial_commits:true"
sn identify query-enhanced --data @query.json
```

## CICD (app, updateset, atf)

`app`, `updateset`, and `atf run` are **asynchronous** — they return a progress
object with `links.progress.id` immediately. `status` codes: `0` Pending, `1`
Running, `2` Successful, `3` Failed, `4` Cancelled.

**Preferred: `--wait`** blocks until the operation finishes (polling
`GET /api/sn_cicd/progress/{id}` every 2s internally), then emits the final
progress result. `--wait-timeout <SECS>` bounds the wait; on expiry the command
exits 3 pointing you to `sn progress <id>`.

**Branch on the exit code, never on `status_label`.** `--wait` returns 0 *only* when
`status` reaches `2`. A failed operation is **exit 2 with empty stdout** (the progress
object is on stderr, under `.error.sn_error`); a timeout is **exit 3, also empty
stdout**. So reading the command's stdout on a failure branch gets you an empty
string:

```bash
if out=$(sn app install --scope x_myapp --version 1.2.0 --wait --wait-timeout 300 2>/tmp/sn.err); then
  echo "installed"                                    # exit 0 ⇒ status "2", nothing else to check
else
  case $? in
    2) jq -r '.error.message' /tmp/sn.err >&2 ;;      # failed — details in .error.sn_error
    3) echo "timed out; still running — poll: sn progress <id>" >&2 ;;
  esac
  exit 1
fi
```

Final `--wait` stdout on success:
`{"status":"2","status_label":"Successful","status_message":"...","percent_complete":"100"}`.

⚠️ **`status_label` is ServiceNow's own string, passed through verbatim — `sn` never
normalizes it.** It varies by instance and operation ("Successful", "Complete",
"Succeeded"…). Matching on it is how you write a poll loop that never terminates.
The numeric `status` is the contract; the label is for humans.

**Manual polling** (for an operation already in flight) — key off `status`:

```bash
while r=$(sn progress "$id"); do
  case "$(jq -r '.status' <<<"$r")" in
    2)   break ;;                                              # successful
    3|4) jq -r '.status_message' <<<"$r" >&2; exit 1 ;;        # failed / cancelled
    *)   sleep 5 ;;                                            # 0 pending, 1 running
  esac
done
```

**App lifecycle** — install/publish/rollback scoped apps, identified by
`--scope` or `--sys-id`:

```bash
sn app install  --scope x_myapp --version 1.2.0 --wait
sn app publish  --scope x_myapp --version 1.3.0 --dev-notes "Fix approval NPE" --wait
sn app rollback --scope x_myapp --version 1.1.0 --wait          # --version required
```

**Update Set lifecycle** — create → (make changes) → retrieve → preview → commit:

```bash
sn updateset create --name "Sprint 42" --description "ITSM tweaks"            # {"sys_id":"...","state":"in progress"}
sn updateset retrieve --update-set-id <remote_sys_id> --auto-preview --wait   # --auto-preview previews right after retrieval
sn updateset preview <remote_id> --wait
sn updateset commit  <remote_id> --wait
sn updateset commit-multiple --ids id1,id2,id3
sn updateset back-out --update-set-id <sys_id> [--rollback-installs] --wait
```

**ATF** — run a suite by name or id, then fetch detailed results by result sys_id:

```bash
sn atf run --suite-name "Regression Suite" --wait     # or --suite-id; also --browser-name, --run-in-cloud, --performance-run
sn atf results <result_id>                            # {"status":"success","tests_total":38,"tests_passed":38,"tests_failed":0}
```

## Performance Analytics scorecards

`sn scores list` → `GET /api/now/pa/scorecards`. Paginate with
`--per-page`/`--page`.

```bash
sn scores list --per-page 20 --sort-by VALUE --sort-dir DESC
```
```json
[{"uuid":"indicator-uuid-1","name":"MTTR - Incidents","value":4.2,"target":6.0,
  "direction":2,"direction_label":"Minimize","frequency":10,"frequency_label":"Daily"}]
```

⚠️ `direction` and `frequency` are **integer codes**, not words. The human strings
are in `direction_label` / `frequency_label` — read those, not the numbers.

```bash
sn scores list --uuid <indicator_id> --include-scores --from 2026-01-01 --to 2026-04-01   # historical series
sn scores favorite <uuid>       # or: sn scores unfavorite <uuid>
```

Filters: `--uuid <csv>`, `--favorites`, `--key`, `--target`, `--contains <csv>`,
`--sort-by VALUE|CHANGE|CHANGEPERC|GAP|NAME|DATE|…`, `--sort-dir ASC|DESC`,
`--include-scores` (with `--from`/`--to`), `--include-available-breakdowns`,
`--include-realtime`.

## Utility & extension commands

```bash
sn ping                                  # health check (auth + latency + build); see Setup
sn user me                               # the authenticated user's sys_user record
sn open incident a1b2c3 [--print-url]    # open the record's form in a browser; --print-url prints the URL instead
sn raw GET /api/now/table/incident --query sysparm_limit=5      # REST passthrough for unmodeled endpoints
sn raw POST /api/now/table/incident --data '{"short_description":"via raw"}'
sn raw GET /api/now/table/incident -H 'X-no-response-body: true'   # repeatable request headers
sn graphql 'query { GlideRecord_Query { incident(pagination: {limit: 5}) { _rowCount _results { number { value } } } } }'
sn graphql @query.graphql --var id=a1b2c3 --variables '{"limit": 5}'   # document from file; string + typed variables
sn completion zsh                        # shell completion script (bash|zsh|fish|powershell|elvish) to stdout
sn introspect                            # full command tree as JSON — auto-generate MCP / function-call schemas
```

`sn raw <METHOD> <PATH>` applies the active profile's auth/proxy/TLS and the
standard output/error contract; use it for endpoints `sn` doesn't model.

`sn graphql <QUERY>` runs a GraphQL document against `POST /api/now/graphql` —
the whole surface, including the generated `GlideRecord_Query` /
`GlideRecord_Mutation` / `GlideAggregateRecord_Query` namespaces (a query field
and CRUD mutations for every table; per-field display values, `_choices` on
choice fields, `_table_metadata` ACL verdicts, `_reference` dot-walking,
`_rowCount` totals alongside a page). The document comes inline, from `@file`,
or `@-` (stdin). On success stdout gets `data` unwrapped (`--output raw` keeps
the envelope). GraphQL reports failure **in-band** — HTTP 200 with an `errors`
array, sometimes next to partial `data` — so a response with errors exits 2
with the full array under `sn_error`, and partial `data` still reaches stdout
first. `--var k=v` sets a string variable (repeatable; only the first `=`
splits, so encoded queries pass through). `--variables '{...}'` supplies a JSON
object for non-string variables; `--var` entries overlay it. Prefer variables
over splicing values into the document — a sys_id in `--var` can't break query
syntax.
`sn introspect` emits the whole command tree as **one recursive object** —
`{name, about, args[], subcommands[]}`, with `subcommands` nesting the same shape
all the way down. There is **no** top-level `commands` array (`jq '.commands[]'`
fails with "Cannot iterate over null"), and no `summary`, `flags`, or `exit_codes`
key: the help string is `about`, and every option lives in `args[]`.

```bash
# Flatten the tree to every command and its help text:
sn introspect | jq '[.. | objects | select(has("subcommands")) | {name, about}]'

# What flags does `table list` take?
sn introspect | jq '.subcommands[] | select(.name=="table")
                    | .subcommands[] | select(.name=="list") | .args[].name'
```

Each entry in `args[]` carries `name`, `long`, `short`, `help`, `help_heading`,
`required`, `takes_value`, `value_name`, `positional`, `repeatable`, `aliases`,
`default_values`, `possible_values`, and `conflicts_with` — enough to generate
an MCP tool or function-call schema. The help string is `help`, not `about`;
`about` is the *command's* description. A `takes_value: false` arg is a
valueless switch: emit `--all`, never `--all true`.

The root carries two extra keys: `version` (the binary that produced the tree)
and `global_args` (the 11 flags clap propagates to every command). **A command's
effective flags are its own `args` plus the root's `global_args`** — non-global
args do not propagate, so there is no ancestor chain to walk. They sit at the
root because repeating them on all 121 nodes was three quarters of the output.

```bash
# Everything `table list` accepts:
sn introspect | jq '[.global_args[], (.subcommands[] | select(.name=="table")
                     | .subcommands[] | select(.name=="list") | .args[])]
                    | map(.name)'
```

`conflicts_with` lists the arg ids that cannot be combined with this one, so a
generator never emits `--data` alongside `--field` (exit 1). Its inverse is
**not** available: clap keeps `requires` private, so `--wait-timeout` requiring
`--wait` appears only as prose in that flag's `help`. `help_heading` carries the
tier `--help` renders — `Global options`, `Advanced options`, or `null` for a
command's working set.

Those tiers are the same ones `sn <command> --help` prints, in that order: the
command's own flags first (unheaded, ordered by usefulness), then **Advanced
options** — raw `sysparm_*` passthroughs like `--view`, `--query-category`,
`--query-no-domain`, `--no-count` that most callers never touch — then **Global
options**, the 11 flags every command accepts. When scanning help output for the
flag you want, the first block is almost always the one that matters.

`--help` and `--version` are omitted: they exit before any handler runs, so
there is nothing for a generated tool to call. Nothing named `help` appears in
the tree at all — clap's `help` subcommand mirrors every command as an
argument-less stub, and emitting it gave each real command a same-named twin.

## Common mistakes

- **Echoing a read straight back into a write or a `--query`** → `--display-value`
  defaults to **`true`**, so you are holding `"In Progress"` and a localized date
  like `08/04/2026 10:22:01 AM`, neither of which ServiceNow will accept back.
  Read with `--display-value false` (or `all`, and send the `value` side).
- **Reaching for `replace`** → it was removed in 0.11.0 and exits 1. `update` is
  the only write verb; PUT and PATCH were both partial updates, so the two did
  the same thing. Clear a field explicitly (`--field x=""`).
- **Mixing `--data` and `--field`** → exit 1. Pick one.
- **`--query` on `get`** → `get` takes a sys_id only; use `list --limit 1 --query "..."`.
- **Missing `--yes` on `delete`** in CI/agent contexts → immediate exit 1 usage
  error (non-TTY never prompts). Applies to every `delete` subcommand (`table`,
  `change`, `change task`, `attachment`, `cmdb relation`).
- **Sending a display value as raw** → `--field state="In Progress"` without
  `--input-display-value` fails; send `state=2`.
- **Paginating by hand** → use `--all` (with `--max-records` as a guard rail);
  never compute offsets.
- **Trusting `sn_error` on transport errors (exit 3)** → it's null/absent; check
  `.error.message`.
- **Pulling more than you need** → `--setlimit` defaults to 1000 on the main list commands; lower it for
  exploration.

## Claude Code plugin

`sn` ships as a Claude Code plugin that pre-approves `Bash(sn *)` (no per-call
permission prompts). Repos that clone it load the local skill at
`.claude/skills/sn.md` automatically (invoke with `/sn`). To install it as a
plugin elsewhere, add this repo as a marketplace (`claude plugin marketplace
add tehubersheezy/servicenow-cli`, or a local clone path), then
`claude plugin install sn`.

## Quick reference

```
sn init [--profile NAME]                          sn ping [--profile NAME]
sn profile add NAME --instance X --username Y --password-stdin [--force|--no-verify|--set-default]
sn profile list|show NAME|use NAME|remove NAME    sn auth login|status|refresh|logout
sn user me     sn open TABLE SYS_ID [--print-url]     sn completion SHELL
sn raw METHOD PATH [-q k=v ...] [--data ...|--field k=v ...]
sn graphql QUERY|@FILE|@- [--var K=V ...] [--variables JSON|@FILE|@-] [--operation NAME]
sn introspect  sn progress PROGRESS_ID

sn schema tables [--filter SUBSTR]
sn schema columns TABLE [--writable] [--mandatory] [--filter S] [--references-only] [--choices-only] [--type T]
sn schema choices TABLE COLUMN

# Shared list flags: --query EQ  --fields CSV  --setlimit N(=--limit)  --offset N
#   --display-value false|true|all  --all [--array] [--max-records N]  --output default|raw|table
sn table list TABLE [shared list flags] [--view N] [--query-category C] [--query-no-domain] [--no-count]
sn table get  TABLE SYS_ID [--fields CSV] [--display-value ...] [--view N]
sn table create  TABLE (--data JSON|@FILE|@- | --field K=V ...) [--fields CSV] [--display-value ...] [--input-display-value]
sn table update  TABLE SYS_ID (--data ...|--field K=V ...) [same write flags]     # PATCH — the only write verb
sn table delete  TABLE SYS_ID [--yes] [--query-no-domain]
sn table TABLE [SYS_ID]                                                           # verb optional: = list / = get

sn change list [--type normal|emergency|standard] [shared list flags]
sn change get|update|delete SYS_ID [--type ...] [--yes]     sn change create [--type ...] [--template ID] (--data|--field)
sn change nextstates|schedule SYS_ID                sn change approvals|risk SYS_ID (--data|--field)
sn change models|templates [SYS_ID]
sn change task list|get|create|update|delete CHANGE_SYS_ID [TASK_SYS_ID] (--data|--field) [--yes]
sn change ci list|add CHANGE_SYS_ID (--data|--field)    sn change conflict get|add|remove SYS_ID

sn attachment list [--query EQ] [--setlimit N]      sn attachment get|delete SYS_ID [--yes]
sn attachment upload --table T --record SYS_ID --file PATH [--file-name N] [--content-type MIME]
sn attachment download SYS_ID [--out PATH]

sn cmdb list CLASS [--query EQ] [--setlimit N]       sn cmdb get CLASS SYS_ID      sn cmdb meta CLASS
sn cmdb create|update CLASS [SYS_ID] (--data|--field)   sn cmdb CLASS [SYS_ID]   # verb optional: = list / = get
sn cmdb relation add CLASS SYS_ID (--data|--field)   sn cmdb relation delete CLASS SYS_ID REL_SYS_ID [--yes]

sn import create STAGING_TABLE (--data|--field)     sn import bulk STAGING_TABLE --data JSON|@FILE|@-
sn import get STAGING_TABLE SYS_ID

sn catalog list [--text T]    get|category|item|item-variables SYS_ID    categories CATALOG_SYS_ID [--top-level-only]
sn catalog items [--text T] [--category ID] [--catalog ID]
sn catalog order|add-to-cart ITEM_SYS_ID (--data|--field)
sn catalog cart | cart-update ID | cart-remove ID | cart-empty CART_SYS_ID | checkout | submit-order | wishlist

sn identify create-update|query (--data ...) [--data-source NAME]
sn identify create-update-enhanced|query-enhanced (--data ...) [--data-source NAME] [--options KEY:VAL,...]

sn aggregate TABLE [--count] [--avg-fields|--sum-fields|--min-fields|--max-fields CSV]
             [--group-by CSV] [--query EQ] [--having EXPR] [--order-by CSV] [--display-value ...]

sn app install|publish|rollback [--scope S|--sys-id ID] [--version V] [--dev-notes T] [--wait [--wait-timeout SECS]]
sn updateset create --name N [--description T]      retrieve --update-set-id ID [--auto-preview] [--wait]
sn updateset preview|commit REMOTE_ID [--wait]      commit-multiple --ids CSV
sn updateset back-out --update-set-id ID [--rollback-installs] [--wait]
sn atf run [--suite-id ID|--suite-name N] [--wait]  sn atf results RESULT_ID

sn scores list [--uuid CSV] [--per-page N] [--page N] [--sort-by ...] [--sort-dir ...]
               [--include-scores --from D --to D] [--favorites] [--key]
sn scores favorite|unfavorite UUID

Global flags (any command): --profile NAME  --output default|raw|table  --proxy URL  --no-proxy
  --insecure  --ca-cert PATH  --proxy-ca-cert PATH  --timeout SECS  -d/-dd/-ddd  -v/-V (version)
Env vars (proxy/TLS + config dir only — no credential/profile env vars):
  SN_CONFIG_DIR  SN_PROXY  SN_NO_PROXY  SN_INSECURE=1  SN_CA_CERT  SN_PROXY_CA_CERT
Exit codes: 0 ok   1 usage/config   2 api(4xx/5xx)   3 network   4 auth(401/403)
Error (stderr, all non-zero): {"error":{message,detail,status_code,transaction_id,sn_error}}
```
