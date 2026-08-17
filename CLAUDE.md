# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

## What this project is

`sn` is a single-binary Rust CLI wrapping ServiceNow's REST APIs: Table, Change Management, Attachment, CMDB, Import Set, Service Catalog, Identification & Reconciliation, CICD (App Repository, Update Sets, ATF), Aggregate, Performance Analytics, and several undocumented endpoints (schema discovery, the REST API Explorer's catalogue, the two that name the caller). Built for LLM agents: stable JSON on stdout, structured JSON errors on stderr, deterministic exit codes, no interactive surprises unless opted into (`sn init`). Commands that *can* prompt (`sn init`, `sn profile add`) only ever do so on a TTY — with a non-terminal stdin they fail naming the missing flag rather than blocking on a read nobody will answer.

## Build, test, lint

```bash
cargo build                    # dev build
cargo build --release          # release (stripped, LTO)
cargo test --workspace         # unit + integration
cargo test --lib query::       # one module
cargo test --test pagination   # one integration file
cargo clippy --all-targets --all-features -- -D warnings   # must pass before commit
cargo fmt --all -- --check
```

Integration tests use `wiremock` to mock ServiceNow and `assert_cmd` to drive the compiled binary. Tests calling `reqwest::blocking::Client` inside `#[tokio::test]` **must** wrap both client construction and method calls in `tokio::task::spawn_blocking`, or the blocking runtime panics on drop inside an async context.

## Architecture

### Module layout

```
src/
  main.rs           → parse Cli, set verbosity, dispatch, map Error → ExitCode
  lib.rs            → pub mod {amb, body, cli, client, config, error, oauth, observability, output, output_table, query} — register new modules here
  amb.rs            → AMB/Bayeux websocket client (record watchers): channel encoding, handshake, long-poll, TLS
  error.rs          → Error enum (7 variants: Usage/Config/Api/Auth/Transport/BrokenPipe/Instance), NO_HTTP_STATUS, exit_code(), to_stderr_json()
  output.rs         → write_value (batched JSON), write_jsonl_line (one record, flushed), emit_value/emit_error (raw writers)
  output_table.rs   → write_table (renders JSON as a comfy-table columnar view for `--output table`)
  config.rs         → Config/Credentials TOML types, load/save, resolve_profile(); OAuth types (AuthMethod, OAuthConfig, OAuthGrant, TokenSet) + token persistence
  client.rs         → reqwest blocking client (proxy/TLS), Auth enum (Basic/Bearer/None), Paginator iterator
  oauth.rs          → OAuth 2.0 for SSO: PKCE, loopback redirect server, token exchange (authorization_code/refresh/client_credentials), ensure_access_token()
  query.rs          → ListQuery/GetQuery/WriteQuery/DeleteQuery → Vec<(String,String)>
  body.rs           → --data / --field parsing into serde_json::Value
  observability.rs  → global AtomicU8 verbosity, log helpers (set_level called in main; log_request/response/body wired into client.rs)
  cli/
    mod.rs          → Cli struct, GlobalFlags, all Subcommand enums + arg structs
    kernel.rs       → the shared command kernel: connect()/emit(), build_profile/build_client/build_client_with_headers, write_response/unwrap_or_raw/take_field/bool_opt, confirm_destructive/confirm_delete
    args.rs         → shared clap arg vocabulary, flattened into the command modules' arg structs: BodyArgs (--data/--field), WaitArgs (--wait/--wait-timeout), DisplayValueOpt, SetLimit/Paging (const-generic per-site defaults), DisplayValueArg. Flatten position is help order; a site whose help text differs keeps its own declaration
    init.rs         → sn init (onboarding wizard: profile setup + verification, and ALWAYS claims default_profile); a thin policy layer over profile.rs's core
    auth.rs         → sn auth login/logout/status/refresh (OAuth session commands; login runs the flow for a configured oauth profile, no config mutation) + whoami (authenticated-identity read, shared with profile.rs)
    profile.rs      → sn profile add/list/show/remove/use + the shared profile-writing core (ProfileInput, resolve_name/resolve_input, save_and_verify) used by both `add` and `init`
    table.rs        → sn table list/get/create/update/delete
    watch.rs        → sn watch <table> (live record watcher; streams JSONL)
    schema.rs       → sn schema tables/columns/choices (undocumented SN endpoints)
    api.rs          → sn api list/search/spec (instance API discovery over the REST API Explorer's undocumented doc endpoints)
    introspect.rs   → sn introspect (dumps clap command tree as JSON; see "The introspect contract")
    progress.rs     → sn progress + finish_cicd (poll async CICD ops)
    app.rs          → sn app install/publish/rollback (App Repository)
    update_set.rs   → sn updateset create/retrieve/preview/commit/commit-multiple/back-out
    atf.rs          → sn atf run/results (Automated Test Framework)
    aggregate.rs    → sn aggregate (server-side stats/counts/averages)
    scores.rs       → sn scores list/favorite/unfavorite (Performance Analytics)
    change.rs       → sn change list/get/create/update/delete + task/ci/conflict/nextstates/approvals/risk/schedule/models/templates
    attachment.rs   → sn attachment list/get/upload/download/delete (binary)
    cmdb.rs         → sn cmdb list/get/create/update/meta + relation add/delete
    import.rs       → sn import create/bulk/get (staging-table imports)
    catalog.rs      → sn catalog list/get/categories/items/order/cart/checkout/wishlist
    identify.rs     → sn identify create-update/query + enhanced variants (CI reconciliation)
    user.rs         → sn user me (authenticated user: `ui/user/current_user` names the sys_id, then a direct `sys_user/{sys_id}` read; the scripted query is only the fallback)
    ping.rs         → sn ping (auth + latency + build version + the instance's own account of who the caller is)
    open_record.rs  → sn open <table> <sys_id> (opens the form in the browser)
    raw.rs          → sn raw <method> <path> (REST passthrough for unmodeled endpoints; --header, --query, --data/--field)
    graphql.rs      → sn graphql <query> (GraphQL passthrough: POST /api/now/graphql; in-band errors → exit 2 with the array in sn_error)
    record_ref.rs   → the `table:identifier` record-reference vocabulary (parse is pure argv/exit 1; number→sys_id resolution is one Table API lookup with the limit-2 canary/exit 2) plus the hardcoded number-prefix map (ITSM + SIR). Every `(table, sys_id)` positional pair routes through `parse_pair`, `attachment upload` through `parse_flag_pair`
    get_record.rs   → sn get <ref> (composite read: record + catalog variables + parsed journal; one GraphQL document resolves the ref and carries the journal streams, the record body is a Table API GET — GraphQL has no all-fields selection — and variables reuse variables.rs's joins probe-free)
    journal.rs      → sn journal <table> <sys_id> (comments/work notes parsed into entries; record-stream source by default, sys_journal_field rows via --source table)
    variables.rs    → sn variables get/set (catalog variables on a record; set pre-validates names, writes via the undocumented sn_sc variables PUT — the one path open to itil — and re-reads to verify, since the endpoint 200s on silently-skipped names)
    context.rs      → sn context [scope|updateset] (instance-side session context: the application scope + update set the caller's tracked writes are captured under; read is one GraphQL round trip resolving the sys_user_preference rows server-side via javascript:gs.getPreference terms, writes go through the concoursepicker PUT plus a direct apps.current_app preference write — the picker's application PUT alone never moves the scope over REST — and verify by re-read)
    completion.rs   → sn completion <shell> (clap_complete)
```

### Key data flow

1. `main.rs` parses `Cli` (clap derive), sets verbosity, destructures `Cli { global, command }`; each handler receives `&GlobalFlags` + its typed args struct.
2. `build_profile(&GlobalFlags)` resolves instance + credentials — a profile is the whole unit of identity (`--profile` > `default_profile`; no per-field argv overrides).
3. `build_client(&profile, timeout)` builds a reqwest blocking client (auth, proxy, TLS).
4. Query structs (`ListQuery`, etc.) convert friendly flags → `sysparm_*` pairs.
5. Responses unwrap `{"result": ...}` by default (`--output raw` keeps the envelope); errors go to stderr as `{"error": {message, detail?, status_code?, transaction_id?, sn_error?}}`.

### Exit codes

`0` success, `1` usage/config, `2` API 4xx/5xx (non-auth), `3` network/transport, `4` auth (401/403). Clap parse errors are intercepted in `main.rs` (`handle_clap_error`) to honor this contract: exit 1, JSON error envelope on stderr when stderr is not a TTY (clap's human text when it is). `--help`/`--version` still exit 0.

**`status_code` reports what HTTP said, and is omitted rather than invented.** A failure that never carried an HTTP status must not publish one — in particular never `0`, which no response can carry. Two distinct mechanisms produce a statusless exit 2, and they are deliberately *not* collapsed into one variant because keeping one would have to lie about the other's cause:

- **`Error::Api` with the `NO_HTTP_STATUS` sentinel** (`0`, in `error.rs`) — the HTTP call succeeded, the *operation* inside it failed. Three producers: a `--wait` on a CICD op whose progress ends `"3"` (failed) or `"4"` (cancelled), an AMB subscribe refusal (`amb.rs` — the only HTTP exchange there was the `101` upgrade, and the refusal is an in-band Bayeux reply), and a record-reference number that resolves to nothing (`record_ref.rs`/`get_record.rs` — the lookup 200'd with zero rows, and fabricating a 404 would claim an HTTP verdict that never happened; `sn get`'s message also hedges "(or not readable by this profile)" because GraphQL row ACLs make absent and unreadable the same bytes). A sentinel rather than `Option<u16>` because every other `Error::Api` site has a real status and would become `Some(..)` noise; `to_stderr_json` drops the key and `Display` drops the `" (404)"` suffix.
- **`Error::Instance { message, detail }`** — an HTTP 200 whose *body* does not answer the question asked, so it must not be handed back as if it did. `sn user me`'s scripted fallback returning two rows (proof the `javascript:` term was silently dropped and the rows are strangers) is the case, and record-reference number resolution trips it the same way: `number=` is unique where it exists, so a second row under `sysparm_limit=2` (or `_rowCount > 1` in `sn get`'s GraphQL document) proves the term was dropped and the rows are unfiltered.

Both exit 2 — this is still the instance failing to serve the request — and both are pinned by tests in `error.rs`.

**`status_code: 200` is not this, and is correct where it appears.** `graphql.rs`, `journal.rs` and `variables.rs` detect failures ServiceNow reports *in band* inside a genuinely successful 200, and they build `Error::Api { status: 200 }` on purpose — the HTTP status really was 200, and hiding it would misdescribe the exchange. The rule is only that a status must not be *fabricated*; reporting a real one is never wrong. So `status_code` is diagnostic, and the exit code is what a caller branches on.

### REST endpoint map

Base paths and quirks not obvious from the module list:

| Group | Base path(s) | Notes |
|---|---|---|
| `change` | `/api/sn_chg_rest/change` + `/normal`, `/emergency`, `/standard` | `--type` routes to the sub-path; standard requires `--template <id>`; nested `/task`, `/ci`, `/conflict` + state ops (nextstates, approvals, risk, schedule) |
| `catalog` | `/api/sn_sc/servicecatalog` | full cart flow (browse → add → checkout) or direct `order` (bypasses cart); item-variables endpoint exposes required form fields |
| `cmdb` | `/api/now/cmdb/instance/{className}` (CRUD + relations); `/api/now/cmdb/meta/{className}` (schema) | className is a positional arg |
| `import` | `/api/now/import/{stagingTableName}` | single create + bulk via `insertMultiple`; staging table is positional |
| `identify` | `/api/now/identifyreconcile` | POST-only; CI create/update + read-only query; enhanced variants accept `--options`; payload via `--data` |
| schema (undocumented) | `GET /api/now/doc/table/schema` (tables); `GET /api/now/ui/meta/{table}` (columns/choices/refs) | not in SN's OpenAPI specs; may 404 on very old instances |
| `api` (undocumented) | `GET /api/now/doc` (whole catalogue); `/api/now/doc/services?namespace=` (one namespace, same shape); `/api/now/doc/oas_3` (OpenAPI 3 export); `/api/now/doc/namespaces` (flat name array) | the REST API Explorer's own endpoints, unversioned. **`oas_3` answers `Accept: application/json` with 406** ("Supported response media types for this service are: [application/octet-stream]") — measured live, and it serves octet-stream whatever `?format=` says — so the group builds its client through `build_client_with_headers` with `Accept: */*`; the doc/services endpoints return JSON under `*/*` too, so one client serves all three verbs. `namespaces` is the family's cheap probe (1,494 bytes against 472,842 for the catalogue on the reference instance): only *its* 404 licenses "absent on older releases", and everything else promotes the endpoint's own `detail`, which names the bad argument precisely. It also spell-checks `--namespace`, because `services` answers an unknown namespace with `{"result":{}}` under HTTP 200 — a typo and a real scope publishing no REST API are the same bytes. `GET /api/now/doc/{httpMethod}/{route}` is deliberately unwired: every spelling probed answered 400. `spec --format yaml` is the one response written straight to stdout (the JSON emitter cannot carry it), with a trailing newline added when the instance omits one, and it **buffers rather than streams** — the opposite of `sn attachment download`, on purpose: a spec is a bounded document (9,556 bytes for the Table API; 460 KB for the whole catalogue) and the JSON arm already holds one parsed in memory, so buffering keeps the newline fixup a question about the last byte rather than about writer state |
| identity (undocumented) | `GET /api/now/ui/user/current_user` (names the caller: `user_name`, `user_sys_id`, `user_display_name`); `GET /api/now/sg/impersonation/session` (`CurrentUser`, `OriginalUser`, `admin`, `CanImpersonate`) | the instance's own answer to "who is this request from", with no query to misparse and no `sys_user` read ACL to satisfy. **`sg/impersonation/session` is not `result`-wrapped** — it is a bare object, and a shape that is (a proxy's JSON, a future release) fails the `CurrentUser` check and falls back rather than reporting a name. Its response also carries `SessionToken`, a **live CSRF credential**: it is copied into no output field, and `ping::get_without_body_log` drops response-body logging for exactly that request so `-ddd` cannot print it either. Only a 404 earns the `sys_user` fallback — 401/403 is a real auth verdict (exit 4) and a ping that shopped around for an endpoint willing to answer would report `ok` for dead credentials |
| `graphql` | `POST /api/now/graphql` | one endpoint, the whole merged schema: scripted namespaces + generated `GlideRecord_Query`/`GlideRecord_Mutation`/`GlideAggregateRecord_Query` (a field per table). Failure is in-band — HTTP 200 with an `errors` array, sometimes beside partial `data` — so the command maps non-empty `errors` to exit 2 (full array in `sn_error`) and still writes partial `data` to stdout; success unwraps `data` |
| `watch` (undocumented) | `wss://<instance>/amb` (Bayeux/CometD) | cookie-auth only — `Authorization` is ignored; needs an `Origin` header and a session minted by a prior HTTP call. See "Record watchers" below |
| `context` (undocumented) | `PUT /api/now/ui/concoursepicker/{application,updateset}` (writes); `POST /api/now/graphql` (the read) | the UI pickers' endpoints. The application PUT takes `{"app_id"}` (its sibling takes `{"sysId"}` — they do not share a body shape) and **never writes `apps.current_app` over REST** — it only stages the per-scope update-set preferences — so `context scope` writes the preference row directly after it. The picker GETs are avoided on the read path because `GET …/updateset` has side effects (it heals a scope/update-set preference mismatch); the GraphQL read mirrors that reconciliation in code (`source`, `preference_stale`) instead. `sys_update_set.state` stores `"in progress"` **with a space**; `state=in_progress` silently matches nothing. Row ACLs make this an admin/delegated-developer surface — an `itil` caller gets empty results, which the canary aliases turn into an explicit error, and every `javascript:`-term alias caps at 2 rows so a dropped term (unfiltered results) is detected rather than reported as the answer |

### CICD async pattern

CICD operations (`app`, `updateset`, `atf`) are async: they return a `progress_id` and run in the background on the instance. Prefer `--wait` — it blocks (polling `GET /api/sn_cicd/progress/{id}` every 2s) until the operation completes, then emits the final progress result. `--wait-timeout <SECS>` (requires `--wait`) bounds the wait; on expiry the command exits 3 with a pointer to `sn progress`. Without `--wait`, the command returns the initial progress object immediately; poll in-flight operations with `sn progress <id>`. Progress responses carry `status` — a numeric **string**, not an enum name: `"0"` pending, `"1"` running, `"2"` successful, `"3"` failed, `"4"` cancelled — plus `status_message`/`status_detail` and `percent_complete` (snake_case). The id is at `links.progress.id`; there is no top-level `progress_id` in the response (that name is only the CLI's positional arg). All groups share one tail — `cli/progress.rs::finish_cicd` (progress-link extraction + polling + emission via `write_response`, so `--output table` works under `--wait`); new async commands must route through it, not open-code the wait. `--wait` honors `--output raw` rather than hardcoding `Default`: it used to emit the unwrapped result *and* look for the progress link only at the top level, which under raw is inside the envelope — so `--output raw --wait` was a silent no-op that emitted the initial, unpolled response. A wait that ends in a failed operation raises `Error::Api` with `NO_HTTP_STATUS`, so no `status_code` is published for a failure reported inside a 200 (see "Exit codes").

### Record watchers (AMB websocket)

`sn watch` streams live record changes over ServiceNow's **AMB** (Asynchronous Message Bus) — Bayeux/CometD over a websocket at `wss://<instance>/amb`. This is the transport behind the UI's own record watchers. It is undocumented; everything below was established against a live instance, and most of it is not guessable from a browser packet capture.

**The quirk: you cannot open the socket with the profile's credentials.** `/amb` ignores the `Authorization` header entirely and authenticates **by session cookie alone**. So a watcher must first make one ordinary authenticated HTTP request purely to make the instance mint a session (`Client::session_cookies()`, which reuses `sn ping`'s `sys_user` read), then carry `JSESSIONID`/`glide_user_route`/`glide_session_store` onto the upgrade. Because that request goes out through the normal `Client`, **Basic and OAuth profiles both work with no special handling** — a bearer-authenticated call mints the identical cookies, and AMB accepts them.

Three failure modes all *look like success*, and a naive client falls into all of them:

1. **`Origin` is mandatory.** Without it the upgrade is `403 Forbidden` no matter how good the cookies are. Browsers set it implicitly, so it appears in no capture of the UI.
2. **The `101` upgrade proves nothing about auth.** An *unauthenticated* socket also gets `101 Switching Protocols`, a `successful: true` handshake and a real `clientId`; it only comes apart at `/meta/subscribe`, which fails with the actively misleading `404::message_deleted`. The one honest signal is the handshake's `ext["glide.session.status"]` — `session.logged.in` vs `session.invalidated`. `amb::Amb::connect` checks it and raises an auth error (exit 4), because the alternative is a client that reports "connected" and then silently receives nothing forever.
3. **The session dies under a healthy socket.** The instance reaps a watcher's HTTP session ~60–120s after it was minted (measured on a PDI; not idle expiry — the countdown showed ~89 min remaining — and neither AMB traffic, nor delivered events, nor `--hydrate`'s HTTP GETs prevent it, even though connect replies acknowledge the touches). The reaped session keeps answering `/meta/connect` with `successful: true` and keeps accepting re-arms; it just never delivers another event. The primary defense is **preemptive rotation**: `--session-rotate` (default 45s, under the measured ~60s reap floor) builds a make-before-break replacement — connected and subscribed *before* the old session is touched — then drains the old socket and swaps, deduplicating the seam where one write arrives on both sessions (`Stream::duplicate`, keyed `sys_id|operation|sys_mod_count`; a real re-write increments `sys_mod_count`, so only the same write's second copy matches). A routine reap therefore opens no gap and writes no marker. The fallback is detection: `ext["glide.session.status"]` is present on **every connect reply**, and `Amb::poll` raises a recoverable transport error the moment it stops saying `session.logged.in` (checked only at handshake before 0.12.1, which left a watch permanently, silently dead at the first reap while exiting 0) — the supervisor then reconnects on fresh cookies (the `connect` closure re-mints per attempt) and marks the gap. A marker thus means a genuine failure, not scheduled server behavior.

Protocol notes:

- **`/meta/connect` is the long-poll, not a heartbeat.** Events only flow while a connect is outstanding: the server holds it ~30s, responds, and the client must immediately re-issue it or the stream goes quiet. This is why the watcher is a plain blocking read loop — `tungstenite`, deliberately *not* `tokio-tungstenite`; no async runtime is warranted.
- **Channel names** are `/rw/default/<table>/<b64(query)>`, where the base64 is *standard* (not base64url) with only the padding rewritten: `==` → `--`, `=` → `-`. (The wire also offers `/rw/count2/…` count-delta channels and `/activity/events/<sys_id>`; the `count`/`activity`/`channel` verbs that exposed them were removed in 0.12.1 — `activity` measured delivering zero events, and a delta stream that dies mid-watch accumulates a silently wrong total.)
- **Event payloads carry the changed fields *with* their new values.** An event gives `operation` (`insert`/`update`/`delete`), `changes` (the changed field names), and a `record` holding every field named in `changes` as a `{display_value, value}` pair, plus five `sys_*` audit columns (`sys_created_by/on`, `sys_updated_by/on`, `sys_mod_count`). On an insert `changes` lists every populated field, so `record` *is* the whole new row. `sn watch` emits this as-is, with no API call. **Through 0.9.1 this doc and the code claimed the opposite** ("a `record` of `sys_*` columns only — never what a field changed *to*"), which is what put hydration on by default; it was never true, and 0.10.0 flipped the default after checking the wire.
- **What an event omits is any field that did *not* change**, so a watch on `state` carries no `number`, no `assigned_to`. `--hydrate` opts into one Table API GET per event, **replacing** `record` with the whole row (`--fields`/`--display-value` narrow that fetch and require it). A hydrated row is the row as of the *fetch*, not the event, so a record written twice quickly can hydrate the first event with the second's values — the event's own record has no such skew. A delete carries no `record` at all (under `--hydrate`, `record: null` without a fetch), and a hydration failure sets `hydrate_error` rather than killing the stream.
- **`changes` includes derived fields** — writing `urgency` reports `priority` as changed too, because ServiceNow recomputes it.
- **Inserts list every populated field** in `changes`; **deletes carry `changes: []`**, so an `--on-change` filter never matches a delete.
- **AMB has no server-side operation or field filter**, so `--operation` / `--on-change` filter client-side, *before* hydration (a rejected event must not cost an API call, count against `--max-events`, or reset `--idle-timeout`).
- **AMB has no replay and no cursor**, so a subscription starts at "now" and everything that changed during an outage is gone. A watch is a best-effort feed, not a log; anything that must be complete has to be reconciled against the table over the outage window.

Operational shape: output is JSONL on stdout, flushed per line (an unflushed pipe would look frozen). `--max-events`/`--duration`/`--idle-timeout` bound the stream so it is usable from a script; Ctrl-C sends `/meta/disconnect` and exits 0. Connect failures **fail fast** (a session that never established will not establish on retry); only an *established* session that drops earns reconnect-with-backoff (2s→60s, 10 attempts, reset after 60s of health).

**Not every line is an event.** A successful resubscribe after a drop writes one synthetic meta line naming the hole it just left:

```json
{"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}
```

Three properties are load-bearing, and `Stream` (`cli/watch.rs`) exists to make them checkable in isolation — its `event`/`meta` split is the rule's home, not a comment at the call sites:

- **It is keyed, not shaped like an event.** `sn_watch` appears on nothing else, and the marker carries neither `operation` nor `changes` — the two fields `--operation`/`--on-change` match on, and the two a consumer's own jq predicate is most likely to test. A marker that looked like an event would be dropped by exactly the pipelines that most need it.
- **It is not counted as an event**: it does not spend `--max-events` and does not reset the `--idle-timeout` clock. Otherwise a flapping connection would spend the caller's event budget on its own chatter and keep alive a watcher that has delivered nothing.
- **One marker per gap, not per attempt.** The gap is cleared only after the resubscribe succeeds, so an attempt that dies first leaves the original drop time standing and the reported `downtime_ms` spans the whole outage.

**`--idle-timeout` measures subscribed time only.** The clock starts at the first successful subscribe, and `Stream::resume` pushes it forward by each interval spent off the channel — the same measurement the marker reports as `downtime_ms`, so the two cannot disagree. Nothing could arrive while there was no channel to arrive on, so that time is not idleness. The old wall-clock reading (process start, running through every outage) gave two silent wrong answers, both exit 0 with a stream that just looks quiet: an instance slower to mint a session than the timeout subscribed and returned without ever polling, and any outage longer than the timeout — guaranteed once backoff caps at 60s — made the marker the *last* line of the stream, so the hole it had just announced was never covered. Forgiving is deliberately weaker than resetting per session: silence accumulates across sessions, so a socket flapping faster than the timeout still cannot hold a silent watcher open.

Transport gaps: the socket is opened directly rather than through reqwest, so it does not inherit its settings. `--insecure` and `--ca-cert` are carried across explicitly (`amb::TlsOptions`, rustls with the `ring` provider — the one reqwest already links; a custom CA is *added* to the built-in roots, never swapped for them). **Proxies are not supported** and are refused loudly rather than silently bypassed, since ignoring one would send the session cookie outside the caller's sanctioned egress path.

### Client binary methods

`client.rs` adds three methods beyond the JSON HTTP verbs, for the Attachment API:
- `upload_file(path, query, body: Vec<u8>, content_type)` — POST raw binary with a custom Content-Type.
- `download_file(path) -> Download` — GET binary with the **body unread**; the caller drains it with `Download::copy_to(sink)` through a fixed 64 KiB buffer (`DOWNLOAD_BUFFER_BYTES`), so peak memory is independent of file size (measured: 800 MB at 19 MB peak RSS). The *status* is still resolved eagerly, and that split is the point — a 404 or 401 must fail before the caller touches the filesystem, so a failed download never creates a file to clean up. `copy_to`'s `DownloadError` names the failing side: `Source` is a transport error (exit 3) leaving the destination undefined, `Sink` is local and belongs to the caller, the only party that knows what the destination is.
- `delete_json(path, query) -> Value` — DELETE expecting a JSON body (vs `delete()` returning `()`).

**Reading the body through `Read` changes what `--timeout` means for a download, and that is deliberate.** reqwest's blocking client keeps its timeout on the blocking side only (the inner async client is built without one) and enforces it by wrapping whichever future the caller is parked on. `Response::bytes()` parks on the *whole body*, so the 30s cap covered the entire transfer and a large-but-perfectly-healthy download died at the cap; `Read::read` parks on *one chunk*, so the same 30s becomes a **per-read idle timeout** — slow-but-alive completes, a stalled socket still dies 30s after the last byte. The header/connect phase is unchanged (one bounded wait in reqwest's `execute_request`) and **no non-download request's timeout semantics moved**. `read_timeout` was the obvious primitive, but reqwest 0.12.28 exposes it only on the async builder.

`sn attachment download --out <PATH>` stages into a dot-prefixed `.{base}.sn{pid}-{nanos}.part` in the destination's **own** directory and renames on success (same filesystem, hence atomic), because a truncated file sitting under the name the caller asked for is indistinguishable from a complete one — the failure has to be loud. A pre-existing destination therefore survives a failed download untouched, which is the property staging exists for and what its test asserts (`!dest.exists()` also passes for code that writes straight through and unlinks). The staging name is capped at `NAME_MAX` by truncating the readable base on a char boundary; uniqueness lives in pid+nanos. Ctrl-C unlinks the staging file and exits 130 — without that, every retry stranded another hidden `.part`. stdout has no such option: bytes on a pipe cannot be recalled, so a mid-stream failure is exit 3 with an envelope naming how many truncated bytes already went out.

`Client` also carries an `extra_headers: HeaderMap` for `sn raw --header`, applied **last** in `send()` — after auth and after the `Content-Type` that `request()` sets on bodied calls. Merging caller headers into `ClientBuilder`'s defaults instead is silently overridden for exactly `Content-Type`, since reqwest ranks request-level headers above client defaults; the request goes out under the old value with no error. `RequestBuilder::headers` replaces rather than appends, so a caller's `Content-Type` supplants ours instead of arriving as a second value. `build_client_with_headers` (in `cli/kernel.rs`) is a separate entry point so no other call site changed. `Authorization` never reaches here — `raw` rejects it at parse time, because a profile is the whole unit of identity and a credential in argv is visible to `ps`.

### The introspect contract

`sn introspect` is the machine-readable face of the command tree — MCP and function-call schema generators consume it — so its **output shape is a contract**, and changing it is a breaking change for anything caching generated schemas. Emit through `cli::command()` (not `Cli::command()`) then `build()`, and through `write_response` like every other command.

- **Nodes** are `{name, about, args[], subcommands[]}`, recursive. The root additionally carries `version` (from `CARGO_PKG_VERSION`) and `global_args`.
- **A command's effective flags are its own `args` plus the root's `global_args`.** clap propagates the 11 globals onto every node; serializing them there was 1,480 of 1,852 argument entries — three quarters of a 481 KB tree. Partition with `Arg::is_global_set()`. Non-global args do not propagate, so there is no ancestor chain to walk.
- **`build()` is required *and* is the trap.** It finalizes actions, defaults, and value names (without it every arg looks like it takes a value), but it also generates the `help` subcommand, which mirrors the whole tree as argument-less stubs. Emitting that put 274 of 391 nodes there and gave every real command a same-named twin. `describe_command` filters `help` and hidden subcommands back out.
- **Drop `--help`/`--version` by clap *action*, never by name.** `sn app install --version <VERSION>` is a real value-taking option that a name-based filter deletes.
- **`conflicts_with` comes from the command** (`Command::get_arg_conflicts_with`), which is why `describe_arg` takes one; it is sorted so the tree is byte-stable across runs. Its inverse is unavailable: clap keeps `requires` `pub(crate)`, so `--wait-timeout` needing `--wait` survives only as prose in that flag's help. Don't go looking for the getter.

Guards live in `tests/introspect.rs` (no node named `help`, unique command paths, globals not duplicated, builtins gone but `app install --version` intact). Note that `tests/cli_surface.rs::every_argument_has_help_text` walks this tree: it chains the root's `global_args`, because checking `args` alone would silently stop covering 11 flags rather than fail.

Two places document this schema by hand and must move together: `docs/agent-guide.md` and `docs/agent-integration.md` (the README links there but carries no schema detail itself). The agent skills used to restate it and deliberately no longer do — anything `sn introspect --help` and these docs already cover was cut from them, because a hand-copy of a machine-readable contract is a second source of truth that silently goes stale.

### Profile resolution precedence

A profile is the single unit of identity: commands either manage profiles (`sn init`, `sn profile *`) or use exactly one. Selection is `--profile` flag > `default_profile` in config.toml; if neither resolves, `resolve_profile_name` (in `config.rs`) returns `Error::Config("no profile selected; pass --profile <name> or run \`sn init\`")` (exit 1). There are **no** per-field argv overrides — change identity by rewriting the profile (`sn init`, or `sn profile add --force`) or selecting a different one via `--profile`. A profile with an empty/whitespace `instance` is rejected. There are deliberately no env vars for credential values or profile selection; proxy/TLS env vars (`SN_PROXY` etc.) and the `SN_CONFIG_DIR` config-directory override (see below) still exist.

### `sn init` vs `sn profile add`

Both write a profile through **one shared core** in `cli/profile.rs` (`resolve_name` → `resolve_input` → `save_and_verify`); they differ only in the three policies layered on top, so the two can't drift:

| | `sn init` | `sn profile add` |
|---|---|---|
| role | onboarding wizard | scriptable creator |
| `default_profile` | **always** claims it | **never** touches it (unless `--set-default`) |
| existing profile | upserts | exit 1 unless `--force` |
| result reported as | human text on stderr | JSON on stdout, empty stderr |
| secret input | prompt only | `--password-stdin` / `--client-secret-stdin` (or a prompt) |
| skip verification | can't | `--no-verify` |

(Prompts themselves go to **stdout** — `print!` in `profile::ask` — for both. They only ever fire on a TTY, so scripted stdout stays clean JSON.)

The shared core speaks `profile add`'s vocabulary, so a `Caller` (`Init` / `ProfileAdd`) is threaded through `resolve_name`/`resolve_input`: every missing-field message must name a flag the *invoked* command actually has. Without it `sn init` on a non-TTY pointed at `--password-stdin`, which `init` does not accept.

Verification is on by default and `save_and_verify` **rolls the config files back** when it fails — a profile that can't authenticate must never survive on disk, or it fails later somewhere confusing. `sn profile add --no-verify` is the one deliberate escape hatch (air-gapped provisioning; registering an `authorization_code` profile a human will later `sn auth login` to); `sn init` has no such flag and always verifies. Prefer the `-stdin` secret flags over argv, which `ps` and shell history can see.

### OAuth / SSO authentication

A profile authenticates via `auth = "basic"` (default) or `auth = "oauth"` in its `config.toml` entry. OAuth serves SSO-fronted instances (Okta/Azure AD/ADFS) where the password lives in the IdP, so Basic and the password grant can't work.

- **Split by secrecy:** non-secret OAuth config (client_id, redirect_uri, endpoint overrides, grant, pkce) lives in `config.toml` under `[profiles.<name>.oauth]`; the client secret and cached tokens live in `credentials.toml` (0600), mirroring the username/password split.
- **Two grants:** `authorization_code` (interactive browser flow, loopback redirect server per RFC 8252, PKCE S256; registered as a **public client** — `sn init` neither needs nor prompts for a secret; pass `--client-secret` only for a confidential client) and `client_credentials` (non-interactive, confidential, requires a secret). The loopback `redirect_uri` (default `http://localhost:8400/callback`) **must be registered exactly** in the Application Registry.
- **Endpoints:** authorization `GET /oauth_auth.do`, token `POST /oauth_token.do` (overridable per profile).
- **Commands:** `sn auth login` (pure session command, no flags: resolves the selected profile, requires `auth = "oauth"` with an `[oauth]` block — a basic profile errors `does not use oauth; run \`sn init\`` — runs the flow with the stored grant, caches tokens), plus `sn auth status`/`refresh`/`logout`. All emit success JSON to **stdout**; OAuth profiles are configured via `sn init --auth oauth` or `sn profile add --auth oauth`.
- **Headless setup:** `client_credentials` mints a token without a browser, so `sn profile add` verifies it like any other credential. `authorization_code` cannot be verified without one — `sn profile add` refuses on a non-TTY rather than persist an untested profile, and points at `--no-verify` + `sn auth login`.
- **Whose identity is it:** `auth::identify` (which `whoami`, `sn ping` and `sn profile add`/`sn init` verification all sit on) asks `/api/now/ui/user/current_user` first and only falls back to the `user_name=javascript:gs.getUserName()` read of `sys_user`. A bare `sysparm_limit=1` read of that table returns whichever row sorts first — an arbitrary account, not the caller — so never use it to report "who you logged in as" (that bug shipped for several releases, and then came back through a different door: an instance that cannot evaluate the scripted term **silently drops it**, leaving exactly that bare limit. Measured on a PDI, a bogus term returns `survey.user` and `lucius.bagnoli`, neither of them the caller). The fallback therefore asks for `sysparm_limit=2`: `user_name` is unique, so an evaluated filter matches at most one row and a second row is proof the term is gone — that case reports no user (or, in `sn user me`, `Error::Instance`) rather than a stranger. Every path here either produces an identity it can defend or produces `None`; a report of who you are must fail closed.
- **Transparent refresh:** `build_client` (in `cli/kernel.rs`) calls `oauth::ensure_access_token` before every request for OAuth profiles — returns a cached token, refreshes a stale one via the refresh token (or mints a fresh one for client_credentials), persists new tokens. Call sites need no changes; the `Client` is auth-agnostic (one `Auth` enum `Basic`/`Bearer`/`None`, applied in `send()`).

### Proxy and TLS

Same precedence as profile fields: CLI flag > env var > profile config file.

| CLI flag | Env var | config.toml field | Description |
|---|---|---|---|
| `--proxy <URL>` | `SN_PROXY` | `proxy` | HTTP/HTTPS/SOCKS5 proxy URL |
| `--no-proxy` | — | — | Bypass proxy for this invocation |
| — | `SN_NO_PROXY` | `no_proxy` | Comma-separated hosts to bypass |
| `--insecure` | `SN_INSECURE=1` | `insecure` | Disable TLS cert verification (off by default) |
| `--ca-cert <PATH>` | `SN_CA_CERT` | `ca_cert` | Custom CA cert for ServiceNow |
| `--proxy-ca-cert <PATH>` | `SN_PROXY_CA_CERT` | `proxy_ca_cert` | Custom CA cert for the proxy |

Proxy auth is stored per-profile in `credentials.toml` (`proxy_username`, `proxy_password`).

### Config file locations and persistence

Resolved by `config::config_dir()`. A non-empty `SN_CONFIG_DIR` is used as-is — the documented cross-platform override pointing **directly** at the directory holding `config.toml`/`credentials.toml` (no `sn` subdir appended). Otherwise `directories::ProjectDirs::from("", "", "sn")` gives the platform-native path: Linux `~/.config/sn/`, macOS `~/Library/Application Support/sn/`, Windows `%APPDATA%\sn\`.

`SN_CONFIG_DIR` is the cross-platform config-isolation mechanism for tests, so config-dependent integration tests are not `#[cfg(target_os = "linux")]`-gated. Point it at a temp dir containing the seeded config files (never a `/sn` subdir).

**Both files are written atomically at 0600.** `write_atomic` puts the contents in a `tempfile` created **0600 by `open(2)` itself** — never `chmod`'d afterwards, because create-then-chmod is exactly the window this path exists to close: `save_credentials_to` used to write and then chmod, so a brand-new `credentials.toml` sat at the umask default with a password already inside. The temp file is in the **same directory** (`rename(2)` is only atomic within one filesystem, so `/tmp` would be a copy), `sync_all`'d before the rename so the name cannot publish blocks that were never written, and the containing directory is `fsync`'d after it (best-effort, Unix-only) so the rename itself is durable. `config.toml` gets the same treatment and the same **0600** — it was 0644, and it names the instances and OAuth clients a user talks to, and nothing else needs to read it. The rename carries the temp file's mode across, so this also repairs files an older release left at 0644.

**The advisory lock lives on a `.sn.lock` sidecar, never on the config files themselves.** They are replaced by rename, so a lock taken on one of them is a lock on an inode the next writer will never open — every writer would hold "the lock" on a different inode and none would block. Only a file created once and never renamed over has a stable identity to lock.

- **One lock covers the whole directory**, not one per file. The two files are written as a pair, and a single lock lets a writer hold that pair across the whole read → modify → write. It also means there is no lock ordering, and so no deadlock.
- **Acquisition is re-entrant per thread.** `flock(2)` locks an open file *description*, so a second `open` + lock from the same thread contends with the first and would deadlock the process against itself (`update_credentials_at` → `save_credentials_to` is exactly that shape). `HELD_LOCKS` records held directories per thread, keyed on the **canonicalized** path so `cfg` and `./cfg` cannot look like two locks. Contention between threads and processes is untouched — each opens its own description.
- **Contention is bounded by a 10s `LOCK_TIMEOUT`, not an unbounded block**, so an agent gets a diagnosable exit 1 (`timed out … another sn process is holding it`) instead of an invocation that never returns. `with_config_lock_for` takes a caller-chosen budget for the one critical section that is not "parse, mutate, write": `oauth::ensure_access_token` holds the lock across the `/oauth_token.do` round trip, so it passes a budget derived from the HTTP timeout — with the default 10s every parallel invocation would fail with a spurious lock timeout the moment the IdP got slow.
- **Any load → modify → save must go through `config::with_config_lock`.** The `save_*` functions lock only their own write, which does nothing about two processes that each read the old file first: twelve parallel `sn profile add`s left **one** profile on disk while eleven exited 0 reporting success for a profile that no longer existed. `save_profile`/`restore`/`remove`/`set_default` run their whole sequence inside it; the nested `save_*` acquisition is the re-entrant no-op above. Emission stays *outside* the lock so a slow pipe cannot park other writers, and verification stays outside it because it is a network round trip that can involve a browser and a human.
- **Readers take no lock** — every invocation reads the config, and contending would trade a rare lost update for a common stall. Write *ordering* is what keeps them honest instead: adding writes `credentials.toml` then `config.toml`, so a reader that sees the new config entry is guaranteed to see the credentials too (the interleaving that yields `Auth::Basic("", "")` and a baffling 401 is arithmetically impossible, not merely unlikely); removal goes the other way. Removal's residue is one two-`read(2)` window that only a reader-side shared lock would close.

`fs4` provides the lock (maintained fork of the dead fs2; wraps `flock(2)`/`LockFileEx` behind one safe API; std's `File::lock` would have cost MSRV 1.89 — note `lock_dir` spells it `fs4::FileExt::try_lock(&file)` as a trait call so 1.89's inherent method cannot silently shadow it). `tempfile` moved from dev-dependencies to dependencies because the write path now uses it at runtime.

### Pagination (--all)

`client.paginate()` returns a `Paginator` iterator that follows `Link: rel="next"` headers. Default output is JSONL (one record per line); `--array` buffers into a JSON array. Page size is whatever `--setlimit` sets (default 1000); `--max-records` caps total output (default 100,000; 0 = unlimited).

`--all` with `--output table` or `--output raw` is a **usage error (exit 1)**, checked before `build_profile`/`build_client` so a pure argv mistake never reaches the network or an IdP. Table cannot size a column without seeing every row, so its message points at `--array`, which buffers and already renders; raw means "keep the envelope" and the paginator flattens every page's envelope into a record stream, so there is nothing left to keep — in the `--array` form either. Both were accepted and silently ignored before.

## Conventions

- Every `sysparm_*` parameter has a friendly flag (e.g. `--query`) and a raw alias (`--sysparm-query`) mapping to the same field, via clap's `alias` attribute in `cli/mod.rs`.
- `update` = PATCH, and it is the *only* write verb. `replace` (PUT) was removed in 0.11.0: measured on a live instance, PUT and PATCH left a record byte-identical, because ServiceNow treats both as partial updates — PUT does not blank omitted fields. Clear a field by setting it explicitly. Don't reintroduce a PUT verb without re-measuring; the redundancy is a ServiceNow behavior, not a CLI choice.
- **`--display-value` defaults to `true`** on `table`, `change`, `aggregate`, `scores` (not `watch`, where it only narrows a `--hydrate` fetch). This also localizes dates into the caller's timezone/locale, and such a date will not round-trip into an encoded query — use `--display-value false` when a value has to be fed back to the API.
- **Help is organized in three tiers, and every arg carries a doc comment.** `GlobalFlags` has `next_help_heading = "Global options"` so the 11 propagated globals don't zipper through each command's own flags (clap gives every `Args` container its own display-order counter and merges them). Raw `sysparm_*` passthroughs nobody reaches for get `help_heading = ADVANCED` (the const in `cli/mod.rs`); the default "Options" section is left as the working set, in declaration order — so **declare fields in order of usefulness**. A new arg without a `///` doc comment is a bug: `sn introspect` is the audit (`args[].help` must be non-null for all of them).
- `cli::command()` wraps `Cli::command()` to rewrite every usage line as `sn <path> <POSITIONALS> [OPTIONS]`; `cli::parse()` is what `main.rs` calls, so errors and `--help` agree. Anything building the command tree (`completion.rs`) must go through `cli::command()`, not `Cli::command()`, or its usage lines will disagree.
- **Record references**: every `(table, sys_id)` positional pair (`table get/update/delete`, `journal`, `open`, `variables`, `cmdb get/update`, `cmdb relation add/delete`) and `attachment upload --record` also accept one combined `table:sys_id` / `table:number` token, through `cli/record_ref.rs`. Parse (exit 1) precedes the confirm gate and `connect`; number resolution (one Table API `number=` lookup, `sysparm_limit=2` canary) runs after `connect` and exits 2. A ref *plus* an explicit second positional is refused, never arbitrated. A destructive confirm names what the caller typed (`RecordRef::Display`: `table/sys_id`, `table:number`) — the gate must not connect to learn a sys_id. `sn get` is the composite read on top (`get_record.rs`): reference or bare hardcoded-prefix number → `{table, sys_id, record, variables, journal}`; its resolution rides the journal GraphQL document instead of the Table API lookup, deliberately a second (equally canaried, equally pinned) path because the batching is the command's point.
- **The read verb is optional on `table` and `cmdb`** (`IMPLIED_VERB_GROUPS`): `sn table incident <sys_id>` → `get`, `sn table incident` → `list`, `sn table incident:INC0010001` → `get`. Implemented in `cli::parse` as a *retry after failure*, not `allow_external_subcommands` — argv is re-parsed with a verb spliced in only when the first parse raised `InvalidSubcommand`. Two properties carry it, and both must survive any change here:
  - **The verb is decided, not guessed.** A `:` in the token marks a record reference (no table name can contain one), so only `get` is offered; a plain token tries `list` before `get`, and `list` rejects a second positional, so a trailing sys_id still lands on `get`. Ordering carries the bare-noun-means-list rule because `get`'s sys_id positional is optional now. Never infer a write.
  - **A typo must not become a shorthand.** clap fills `ContextKind::SuggestedSubcommand` only when the token is within edit distance of a real verb, so a non-empty suggestion aborts the rewrite — that is why `sn table lst incident` still says "did you mean 'list'" instead of GETting a table named `lst`. Don't replace that check with a hand-rolled distance function; it would drift from clap's.
  - Groups with non-CRUD subcommands (`change nextstates`, `catalog checkout`) are excluded on purpose: their first token isn't reliably a noun.
- `InvalidSubcommand` errors append the group's real subcommand list (`main.rs::subcommand_hint` → `cli::valid_subcommands`), because clap names none of them unless the token is a near-miss.
- **Every destructive command is gated by `cli::kernel::confirm_destructive(yes, verb, what)`** (`confirm_delete` is the `delete`-verb shim `attachment`/`cmdb` still call). On a TTY it prompts `<Verb> <what>? [y/N]:` on stderr; with a non-terminal stdin it refuses with ``"<verb> <target> requires --yes when stdin is not a terminal"``.
  - **The verb must be the one the caller typed.** The guard's prompt used to be a hardcoded "Delete {what}?", which fits only a row deletion — which is why `catalog cart-empty`, `catalog cart-remove`, `change conflict remove` and `profile remove` shipped with no gate at all, having nowhere to plug in. `catalog cart-remove` and `change conflict remove` say "remove", not "delete", a word in neither caller's argv. Give a command its own args struct rather than reusing a shared one when the sibling uses are reads (`ChangeSysIdArg` is shared with `nextstates`/`schedule`/`ci list`/`conflict get`, and a `--yes` on a read reads as if the read were dangerous).
  - **Gate before `build_profile`/`build_client`.** The check reads nothing but argv, and running it after authentication hands a pure argv mistake to the IdP: on an OAuth profile `build_client` mints a token first, so an unreachable instance answers with exit 3 and a token-endpoint URL while the exit-1 usage error never prints.
  - `updateset back-out` and `app rollback` are gated too. Both are recoverable in principle and a human types them deliberately, but this CLI's audience is LLM agents and both are the shape an agent fires by accident: one flag, instance-wide effect, asynchronous, no second confirmation downstream. `--yes` is one token in a script; an unintended back-out is a recovery project.
- **`sn cmdb create/update` writes go out as the IRE envelope** `{"attributes": {...}, "source": "..."}` (`cmdb.rs::ire_envelope`) — the CMDB Instance API accepts nothing else (PATCH of a flat body is a raw Java NPE, HTTP 500 `"attributes" is null`; POST is a 400 naming a null data source), so `--field` could never work on `cmdb` at all. A body whose `attributes` is a JSON *object* is passed through as a caller-written envelope (the only way to send `inbound_relations`/`outbound_relations` on create); anything else is wrapped. Keying off the key's *presence* would be wrong: 718 CMDB classes on a stock Zurich instance carry a real column named `attributes`, all String or Field List, so a real field value is never an object on the wire.
  - **Attribute values must be strings.** The API casts each one to `java.lang.String` before it reaches the record, so `--field cpu_count=8` answers HTTP 500 `class java.lang.Integer cannot be cast to class java.lang.String` and writes nothing (likewise `Boolean`, `LinkedHashMap`, `ArrayList`). `ire_envelope` stringifies numbers and booleans, refuses objects and arrays with a usage error naming the attribute, and leaves `null` alone (Java casts it fine, so it stays usable for clearing a field). **This is a CMDB-only constraint and must not be pushed into `body.rs`** — `coerce_field_value` is shared with every other command, and the Table API takes JSON numbers happily. It applies inside a hand-written envelope too: passthrough is about the body's *shape*, not an opt-out of a cast the server performs regardless.
  - `--source` defaults to `"Manual Entry"` (a CLI write literally is a manual entry). `source` given both in the body and as the flag is a usage error, and a **flat** body's top-level `source` is refused as ambiguous rather than guessed: demoting it into `attributes` had the API drop it and stamp the record with the default provenance (exit 0, wrong provenance, nothing said), while lifting it would swallow a legitimate field write on `cmdb_ci_service_discovered`/`cmdb_ci_service_calculated`, which have a real `source` column. Both readings lose something silently, so the caller is asked which they meant. Provenance is what the IRE reconciles on, and a borrowed discovery-source name lets that tool's next run overwrite a hand edit.
- The shared `pub(crate)` command kernel lives in `cli/kernel.rs`: `connect(global)` (the `build_profile` → `build_client` preamble; keep the two-step form only when the handler reads the resolved profile itself — `open`, `watch`, the OAuth flows, `ping`, and the `build_client_with_headers` callers `raw`/`api`), `emit(global, resp)` (`unwrap_or_raw` → `write_response`, taking `resp` by value so the envelope is moved, not cloned), plus `bool_opt`, `take_field` and `confirm_destructive`/`confirm_delete`. `format_from_flags` is **private** on purpose — it plus `emit_value` was the bypass that let `aggregate`, `scores list`, `scores favorite` and `open` accept `--output table` and ignore it, so the bypass is now a compile error rather than a convention. Nothing is re-exported through `cli::table`, so the old import path cannot come back one convenient `use` at a time.
- Every command's final emission goes through `cli::kernel::write_response(global, &value)` (usually via `emit`), which routes to `output::write_value` (JSON, default/raw) or `output_table::write_table` (`--output table`). `unwrap_or_raw` treats `Table` like `Default` (envelope unwrapped, then rendered).
- **The batch/stream split in `output.rs` is load-bearing, and it lives in the functions rather than in a comment at the call sites** — the call sites are where it kept getting lost. `write_value` batches a finished payload through a 64 KiB `BufWriter` with an explicit, propagated flush (`BufWriter::drop` discards a failing flush, which would turn a closed pipe or a full disk into silent truncation with exit 0). `write_jsonl_line` is the streaming path and **flushes after every record**: a line sitting in a buffer is indistinguishable from a hung process, so `sn watch | jq` would look frozen. **Callers must not flush again.** A second flush is a second syscall on a pipe the reader may already have closed, so `sn watch … | head -1` attributes the broken pipe to the *following* record instead of the one that hit it, and the session skips its `/meta/disconnect` — that is a real merge defect, not a hypothetical: `Stream::write` was flushing on top of `write_jsonl_line`, and the test caught it only after both branches landed. There is deliberately no whole-iterator JSONL helper; one existed, nothing in the binary called it, and its tests read like coverage of the streaming path while pinning dead code.

## Claude Code plugin

This repo is also a Claude Code plugin and its own marketplace (`.claude-plugin/plugin.json` + `marketplace.json` + `skills/sn/`). The skill is a directory — `SKILL.md` plus `references/{shapes-queries,change,watch,cicd}.md` — loaded progressively: `SKILL.md` is always in context when the skill fires, the references only when it points at one. `.claude/skills/sn/` is the in-repo copy (`/sn`); `skills/sn/` is for external distribution and is **identical except for the `allowed-tools: Bash(sn *)` line**, which auto-approves CLI calls for installed users. Keep all ten files in sync; `diff -r` the two trees.

**The skill documents what `--help` cannot, and nothing else.** Measured against a no-skill baseline across ten A/B runs, an agent with only the CLI matched an agent with the old 1,041-line skill on every task, because this CLI's help and its self-naming errors already teach the flags (`sn cmdb create --help` carries the IRE envelope, the Java String cast *and* the `source` ambiguity). What a skill can add is the *instance's* behavior, which no help text can reach: response shapes (`change` wrapping every field, `cmdb` nesting under `.attributes`, the `__meta` element, aggregate's sibling `groupby_fields`), silently dropped query terms, exit 4 meaning a missing role rather than a bad credential, and the CICD status contract. When adding to the skill, the test is: *can the binary teach this at the moment of need?* If yes — help text, a self-naming error, or validation the CLI already performs — leave it out.

## CI/CD

- `ci.yml` — fmt + clippy + test on ubuntu/macos/windows (PRs + pushes to main).
- `security.yml` — `cargo audit` (RustSec) + `cargo deny check all` (license/source/banned-crate policy in `deny.toml`); on dep changes + daily 06:00 UTC.
- `scorecard.yml` — OSSF Scorecard; weekly + push to main; publishes to scorecard.dev.
- `release.yml` — cargo-dist on `v*` tag: builds {x86_64,aarch64}-{linux,macos,windows}, shell/PowerShell/MSI installers, Homebrew formula, and signed provenance attestations (`gh attestation verify`); auto-publishes the formula to `tehubersheezy/homebrew-sn` via `HOMEBREW_TAP_TOKEN`.
- `release-guard.yml` — hooks `Release` via `workflow_run`, so it still runs when every job inside skipped. Fails if any release job was **skipped**, if the tag published fewer than 20 assets, or if the Homebrew formula version doesn't match the tag. Exists because a green `Release` does *not* mean anything shipped (see below).
- `mask-check.yml` — **run before tagging** (`gh workflow run mask-check.yml`). Preflight that pushes the top CHANGELOG section through a job output, the exact path that breaks, and names the offending line if it would be withheld.
- Config: `dist-workspace.toml` (cargo-dist), `wix/main.wxs` (MSI template; stable GUIDs in `[package.metadata.wix]` of `Cargo.toml`), `.github/dependabot.yml` (weekly grouped dep PRs for cargo + github-actions).

### Release notes: state the fix, keep it short

**The top section of `CHANGELOG.md` *is* the GitHub release body.** cargo-dist parses the file and publishes that section as `announcement_github_body` — nothing in `dist-workspace.toml` names the file or reshapes the text. So it is written for someone deciding whether to upgrade, not for a reviewer reconstructing the work.

- **State the fix, not the problem.** "`sn cmdb create` and `sn cmdb update` work" — not "they sent a flat body, which that API cannot accept under any input: PATCH came back as a raw Java NPE…". A user upgrading does not need the defect's history, and leading with it buries the one sentence they came for.
- **One line per entry**, and keep the whole section **under ~4,000 characters**. 0.12.0 shipped at 37,114 and rendered as an unbroken wall; the rewrite says the same things in 3,383.
- **New commands lead**, and they are the one place to spend words: name the verb and describe what it now lets a caller do. Everything else follows — `### Breaking`, then `### Fixed`, then `### Security`. A release is read first for what it adds.
- **Design rationale, live measurements and rejected alternatives do not belong here.** They belong in this file and `docs/`, which is where they already are — the changelog was duplicating them. Keep a number only when the number is the user-facing result ("800 MB at 19 MB peak RSS"), not when it is evidence for a design choice.
- **Never use `## ` inside a release section.** `mask-check.yml` extracts the top section with `awk '/^## /{n++} n==1'`, so a nested `##` truncates what the preflight checks. Subsections are `### `.

### The changelog can silently empty a release

**Never write `Bearer <word>` in `CHANGELOG.md`.** It will publish a release with zero assets while reporting success.

cargo-dist passes the whole dist-manifest — release body included — as the `plan` job's `val` output, and every downstream job gates on `fromJson(needs.plan.outputs.val)`. The runner refuses to set an output containing a masked string, so `val` arrives empty, every `if` evaluates false, and build/host/publish all **skip**. Skipped is not failed, so the run is green. This emptied v0.8.0 three times.

The masked string is not one of your secrets. The runner registers CredScan credential regexes alongside real secrets, and one matches a bearer credential. Measured on the runner 2026-07-14 (see the `mask-forensics` workflow in git history):

| masked | not masked |
|---|---|
| `Bearer auth`, `Bearer xyzzy`, `Bearer 123`, `Bearer x` | `bearer auth` (case-sensitive), `BEARER AUTH` |
| `Bearer` ⏎ `auth` — **a newline is whitespace, so wrapping is no escape** | `Bearer` alone (needs a token after it) |
| `Authorization: Bearer abc123`, and the phrase anywhere inside prose | `Bearer-auth`, `Bearer.auth`, `Bearer_auth`, `Bearer, auth` (needs whitespace) |
| | `Bearers auth`, `MyBearer auth` (word boundaries required) |

So the rule is `\bBearer` + whitespace + any non-whitespace character. Say "bearer token" lowercase, or "OAuth access token", and rely on `mask-check` rather than on remembering this. **This was misdiagnosed for months** as junk prose lines pasted into `HOMEBREW_TAP_TOKEN`; that secret is a clean single line (`mask-check`'s `shape` job proves it every run) and rotating it changes nothing. A multiline secret *is* a real hazard of the same family — each line becomes a repo-wide mask — which is why `shape` still checks.
