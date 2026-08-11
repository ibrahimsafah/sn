# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

## What this project is

`sn` is a single-binary Rust CLI wrapping ServiceNow's REST APIs: Table, Change Management, Attachment, CMDB, Import Set, Service Catalog, Identification & Reconciliation, CICD (App Repository, Update Sets, ATF), Aggregate, Performance Analytics, and two undocumented schema-discovery endpoints. Built for LLM agents: stable JSON on stdout, structured JSON errors on stderr, deterministic exit codes, no interactive surprises unless opted into (`sn init`). Commands that *can* prompt (`sn init`, `sn profile add`) only ever do so on a TTY — with a non-terminal stdin they fail naming the missing flag rather than blocking on a read nobody will answer.

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
  lib.rs            → pub mod {amb, body, cli, client, config, error, observability, output, output_table, query} — register new modules here
  amb.rs            → AMB/Bayeux websocket client (record watchers): channel encoding, handshake, long-poll, TLS
  error.rs          → Error enum (5 variants), exit_code(), to_stderr_json()
  output.rs         → emit_value (JSON), emit_jsonl (JSONL), emit_error (stderr)
  output_table.rs   → write_table (renders JSON as a comfy-table columnar view for `--output table`)
  config.rs         → Config/Credentials TOML types, load/save, resolve_profile(); OAuth types (AuthMethod, OAuthConfig, OAuthGrant, TokenSet) + token persistence
  client.rs         → reqwest blocking client (proxy/TLS), Auth enum (Basic/Bearer/None), Paginator iterator
  oauth.rs          → OAuth 2.0 for SSO: PKCE, loopback redirect server, token exchange (authorization_code/refresh/client_credentials), ensure_access_token()
  query.rs          → ListQuery/GetQuery/WriteQuery/DeleteQuery → Vec<(String,String)>
  body.rs           → --data / --field parsing into serde_json::Value
  observability.rs  → global AtomicU8 verbosity, log helpers (set_level called in main; log_request/response/body wired into client.rs)
  cli/
    mod.rs          → Cli struct, GlobalFlags, all Subcommand enums + arg structs
    init.rs         → sn init (onboarding wizard: profile setup + verification, and ALWAYS claims default_profile); a thin policy layer over profile.rs's core
    auth.rs         → sn auth login/logout/status/refresh (OAuth session commands; login runs the flow for a configured oauth profile, no config mutation) + whoami (authenticated-identity read, shared with profile.rs)
    profile.rs      → sn profile add/list/show/remove/use + the shared profile-writing core (ProfileInput, resolve_name/resolve_input, save_and_verify) used by both `add` and `init`
    table.rs        → sn table list/get/create/update/delete + shared helpers
    watch.rs        → sn watch table/count/activity/channel (live record watchers; streams JSONL)
    schema.rs       → sn schema tables/columns/choices (undocumented SN endpoints)
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
    user.rs         → sn user me (authenticated user via gs.getUserName())
    ping.rs         → sn ping (auth + latency + ServiceNow build version)
    open_record.rs  → sn open <table> <sys_id> (opens the form in the browser)
    raw.rs          → sn raw <method> <path> (REST passthrough for unmodeled endpoints; --header, --query, --data/--field)
    graphql.rs      → sn graphql <query> (GraphQL passthrough: POST /api/now/graphql; in-band errors → exit 2 with the array in sn_error)
    journal.rs      → sn journal <table> <sys_id> (comments/work notes parsed into entries; record-stream source by default, sys_journal_field rows via --source table)
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
| `graphql` | `POST /api/now/graphql` | one endpoint, the whole merged schema: scripted namespaces + generated `GlideRecord_Query`/`GlideRecord_Mutation`/`GlideAggregateRecord_Query` (a field per table). Failure is in-band — HTTP 200 with an `errors` array, sometimes beside partial `data` — so the command maps non-empty `errors` to exit 2 (full array in `sn_error`) and still writes partial `data` to stdout; success unwraps `data` |
| `watch` (undocumented) | `wss://<instance>/amb` (Bayeux/CometD) | cookie-auth only — `Authorization` is ignored; needs an `Origin` header and a session minted by a prior HTTP call. See "Record watchers" below |

### CICD async pattern

CICD operations (`app`, `updateset`, `atf`) are async: they return a `progress_id` and run in the background on the instance. Prefer `--wait` — it blocks (polling `GET /api/sn_cicd/progress/{id}` every 2s) until the operation completes, then emits the final progress result. `--wait-timeout <SECS>` (requires `--wait`) bounds the wait; on expiry the command exits 3 with a pointer to `sn progress`. Without `--wait`, the command returns the initial progress object immediately; poll in-flight operations with `sn progress <id>`. Progress responses carry `status` — a numeric **string**, not an enum name: `"0"` pending, `"1"` running, `"2"` successful, `"3"` failed, `"4"` cancelled — plus `status_message`/`status_detail` and `percent_complete` (snake_case). The id is at `links.progress.id`; there is no top-level `progress_id` in the response (that name is only the CLI's positional arg). All groups share one tail — `cli/progress.rs::finish_cicd` (progress-link extraction + polling + emission via `write_response`, so `--output table` works under `--wait`); new async commands must route through it, not open-code the wait.

### Record watchers (AMB websocket)

`sn watch` streams live record changes over ServiceNow's **AMB** (Asynchronous Message Bus) — Bayeux/CometD over a websocket at `wss://<instance>/amb`. This is the transport behind the UI's own record watchers. It is undocumented; everything below was established against a live instance, and most of it is not guessable from a browser packet capture.

**The quirk: you cannot open the socket with the profile's credentials.** `/amb` ignores the `Authorization` header entirely and authenticates **by session cookie alone**. So a watcher must first make one ordinary authenticated HTTP request purely to make the instance mint a session (`Client::session_cookies()`, which reuses `sn ping`'s `sys_user` read), then carry `JSESSIONID`/`glide_user_route`/`glide_session_store` onto the upgrade. Because that request goes out through the normal `Client`, **Basic and OAuth profiles both work with no special handling** — a bearer-authenticated call mints the identical cookies, and AMB accepts them.

Two failure modes both *look like success*, and a naive client falls into both:

1. **`Origin` is mandatory.** Without it the upgrade is `403 Forbidden` no matter how good the cookies are. Browsers set it implicitly, so it appears in no capture of the UI.
2. **The `101` upgrade proves nothing about auth.** An *unauthenticated* socket also gets `101 Switching Protocols`, a `successful: true` handshake and a real `clientId`; it only comes apart at `/meta/subscribe`, which fails with the actively misleading `404::message_deleted`. The one honest signal is the handshake's `ext["glide.session.status"]` — `session.logged.in` vs `session.invalidated`. `amb::Amb::connect` checks it and raises an auth error (exit 4), because the alternative is a client that reports "connected" and then silently receives nothing forever.

Protocol notes:

- **`/meta/connect` is the long-poll, not a heartbeat.** Events only flow while a connect is outstanding: the server holds it ~30s, responds, and the client must immediately re-issue it or the stream goes quiet. This is why the watcher is a plain blocking read loop — `tungstenite`, deliberately *not* `tokio-tungstenite`; no async runtime is warranted.
- **Channel names** are `/rw/{default,count2}/<table>/<b64(query)>`, where the base64 is *standard* (not base64url) with only the padding rewritten: `==` → `--`, `=` → `-`.
- **Event payloads carry the changed fields *with* their new values.** An event gives `operation` (`insert`/`update`/`delete`), `changes` (the changed field names), and a `record` holding every field named in `changes` as a `{display_value, value}` pair, plus five `sys_*` audit columns (`sys_created_by/on`, `sys_updated_by/on`, `sys_mod_count`). On an insert `changes` lists every populated field, so `record` *is* the whole new row. `sn watch table` emits this as-is, with no API call. **Through 0.9.1 this doc and the code claimed the opposite** ("a `record` of `sys_*` columns only — never what a field changed *to*"), which is what put hydration on by default; it was never true, and 0.10.0 flipped the default after checking the wire.
- **What an event omits is any field that did *not* change**, so a watch on `state` carries no `number`, no `assigned_to`. `--hydrate` opts into one Table API GET per event, **replacing** `record` with the whole row (`--fields`/`--display-value` narrow that fetch and require it). A hydrated row is the row as of the *fetch*, not the event, so a record written twice quickly can hydrate the first event with the second's values — the event's own record has no such skew. A delete carries no `record` at all (under `--hydrate`, `record: null` without a fetch), and a hydration failure sets `hydrate_error` rather than killing the stream.
- **`changes` includes derived fields** — writing `urgency` reports `priority` as changed too, because ServiceNow recomputes it.
- **Inserts list every populated field** in `changes`; **deletes carry `changes: []`**, so an `--on-change` filter never matches a delete.
- **`count2` emits a delta, not a total** — `{"count": "+1"}` / `{"count": "-1"}`, as strings. Seed from `sn aggregate <TABLE> --count --query <ENCODED_QUERY>` (same query as the watch) and accumulate.
- **AMB has no server-side operation or field filter**, so `--operation` / `--on-change` filter client-side, *before* hydration (a rejected event must not cost an API call, count against `--max-events`, or reset `--idle-timeout`).

Operational shape: output is JSONL on stdout, flushed per event (an unflushed pipe would look frozen). `--max-events`/`--duration`/`--idle-timeout` bound the stream so it is usable from a script; Ctrl-C sends `/meta/disconnect` and exits 0. Connect failures **fail fast** (a session that never established will not establish on retry); only an *established* session that drops earns reconnect-with-backoff (2s→60s, 10 attempts, reset after 60s of health).

Transport gaps: the socket is opened directly rather than through reqwest, so it does not inherit its settings. `--insecure` and `--ca-cert` are carried across explicitly (`amb::TlsOptions`, rustls with the `ring` provider — the one reqwest already links; a custom CA is *added* to the built-in roots, never swapped for them). **Proxies are not supported** and are refused loudly rather than silently bypassed, since ignoring one would send the session cookie outside the caller's sanctioned egress path.

### Client binary methods

`client.rs` adds three methods beyond the JSON HTTP verbs, for the Attachment API:
- `upload_file(path, query, body: Vec<u8>, content_type)` — POST raw binary with a custom Content-Type.
- `download_file(path) -> (Vec<u8>, Option<String>)` — GET binary, returns bytes + Content-Type.
- `delete_json(path, query) -> Value` — DELETE expecting a JSON body (vs `delete()` returning `()`).

`Client` also carries an `extra_headers: HeaderMap` for `sn raw --header`, applied **last** in `send()` — after auth and after the `Content-Type` that `request()` sets on bodied calls. Merging caller headers into `ClientBuilder`'s defaults instead is silently overridden for exactly `Content-Type`, since reqwest ranks request-level headers above client defaults; the request goes out under the old value with no error. `RequestBuilder::headers` replaces rather than appends, so a caller's `Content-Type` supplants ours instead of arriving as a second value. `build_client_with_headers` (in `cli/table.rs`) is a separate entry point so no other call site changed. `Authorization` never reaches here — `raw` rejects it at parse time, because a profile is the whole unit of identity and a credential in argv is visible to `ps`.

### The introspect contract

`sn introspect` is the machine-readable face of the command tree — MCP and function-call schema generators consume it — so its **output shape is a contract**, and changing it is a breaking change for anything caching generated schemas. Emit through `cli::command()` (not `Cli::command()`) then `build()`, and through `write_response` like every other command.

- **Nodes** are `{name, about, args[], subcommands[]}`, recursive. The root additionally carries `version` (from `CARGO_PKG_VERSION`) and `global_args`.
- **A command's effective flags are its own `args` plus the root's `global_args`.** clap propagates the 11 globals onto every node; serializing them there was 1,480 of 1,852 argument entries — three quarters of a 481 KB tree. Partition with `Arg::is_global_set()`. Non-global args do not propagate, so there is no ancestor chain to walk.
- **`build()` is required *and* is the trap.** It finalizes actions, defaults, and value names (without it every arg looks like it takes a value), but it also generates the `help` subcommand, which mirrors the whole tree as argument-less stubs. Emitting that put 274 of 391 nodes there and gave every real command a same-named twin. `describe_command` filters `help` and hidden subcommands back out.
- **Drop `--help`/`--version` by clap *action*, never by name.** `sn app install --version <VERSION>` is a real value-taking option that a name-based filter deletes.
- **`conflicts_with` comes from the command** (`Command::get_arg_conflicts_with`), which is why `describe_arg` takes one; it is sorted so the tree is byte-stable across runs. Its inverse is unavailable: clap keeps `requires` `pub(crate)`, so `--wait-timeout` needing `--wait` survives only as prose in that flag's help. Don't go looking for the getter.

Guards live in `tests/introspect.rs` (no node named `help`, unique command paths, globals not duplicated, builtins gone but `app install --version` intact). Note that `tests/cli_surface.rs::every_argument_has_help_text` walks this tree: it chains the root's `global_args`, because checking `args` alone would silently stop covering 11 flags rather than fail.

Four places document this schema by hand and must move together: `.claude/skills/sn.md`, `skills/sn/SKILL.md`, `docs/agent-guide.md`, and `README.md`.

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

Verification is on by default and `save_and_verify` **rolls the config files back** when it fails — a profile that can't authenticate must never survive on disk, or it fails later somewhere confusing. `sn profile add --no-verify` is the one deliberate escape hatch (air-gapped provisioning; registering an `authorization_code` profile a human will later `sn auth login` to); `sn init` has no such flag and always verifies. Prefer the `-stdin` secret flags over argv, which `ps` and shell history can see.

### OAuth / SSO authentication

A profile authenticates via `auth = "basic"` (default) or `auth = "oauth"` in its `config.toml` entry. OAuth serves SSO-fronted instances (Okta/Azure AD/ADFS) where the password lives in the IdP, so Basic and the password grant can't work.

- **Split by secrecy:** non-secret OAuth config (client_id, redirect_uri, endpoint overrides, grant, pkce) lives in `config.toml` under `[profiles.<name>.oauth]`; the client secret and cached tokens live in `credentials.toml` (`chmod 0600`), mirroring the username/password split.
- **Two grants:** `authorization_code` (interactive browser flow, loopback redirect server per RFC 8252, PKCE S256; registered as a **public client** — `sn init` neither needs nor prompts for a secret; pass `--client-secret` only for a confidential client) and `client_credentials` (non-interactive, confidential, requires a secret). The loopback `redirect_uri` (default `http://localhost:8400/callback`) **must be registered exactly** in the Application Registry.
- **Endpoints:** authorization `GET /oauth_auth.do`, token `POST /oauth_token.do` (overridable per profile).
- **Commands:** `sn auth login` (pure session command, no flags: resolves the selected profile, requires `auth = "oauth"` with an `[oauth]` block — a basic profile errors `does not use oauth; run \`sn init\`` — runs the flow with the stored grant, caches tokens), plus `sn auth status`/`refresh`/`logout`. All emit success JSON to **stdout**; OAuth profiles are configured via `sn init --auth oauth` or `sn profile add --auth oauth`.
- **Headless setup:** `client_credentials` mints a token without a browser, so `sn profile add` verifies it like any other credential. `authorization_code` cannot be verified without one — `sn profile add` refuses on a non-TTY rather than persist an untested profile, and points at `--no-verify` + `sn auth login`.
- **Whose identity is it:** `auth::whoami` resolves the authenticated user via `sysparm_query=user_name=javascript:gs.getUserName()`. A bare `sysparm_limit=1` read of `sys_user` returns whichever row sorts first — an arbitrary account, not the caller — so never use it to report "who you logged in as" (that bug shipped for several releases).
- **Transparent refresh:** `build_client` (in `cli/table.rs`) calls `oauth::ensure_access_token` before every request for OAuth profiles — returns a cached token, refreshes a stale one via the refresh token (or mints a fresh one for client_credentials), persists new tokens. Call sites need no changes; the `Client` is auth-agnostic (one `Auth` enum `Basic`/`Bearer`/`None`, applied in `send()`).

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

### Config file locations

Resolved by `config::config_dir()`. A non-empty `SN_CONFIG_DIR` is used as-is — the documented cross-platform override pointing **directly** at the directory holding `config.toml`/`credentials.toml` (no `sn` subdir appended). Otherwise `directories::ProjectDirs::from("", "", "sn")` gives the platform-native path: Linux `~/.config/sn/`, macOS `~/Library/Application Support/sn/`, Windows `%APPDATA%\sn\`.

`credentials.toml` is `chmod 0600` on Unix. `SN_CONFIG_DIR` is the cross-platform config-isolation mechanism for tests, so config-dependent integration tests are not `#[cfg(target_os = "linux")]`-gated. Point it at a temp dir containing the seeded config files (never a `/sn` subdir).

### Pagination (--all)

`client.paginate()` returns a `Paginator` iterator that follows `Link: rel="next"` headers. Default output is JSONL (one record per line); `--array` buffers into a JSON array. Page size is whatever `--setlimit` sets (default 1000); `--max-records` caps total output (default 100,000; 0 = unlimited).

## Conventions

- Every `sysparm_*` parameter has a friendly flag (e.g. `--query`) and a raw alias (`--sysparm-query`) mapping to the same field, via clap's `alias` attribute in `cli/mod.rs`.
- `update` = PATCH, and it is the *only* write verb. `replace` (PUT) was removed in 0.11.0: measured on a live instance, PUT and PATCH left a record byte-identical, because ServiceNow treats both as partial updates — PUT does not blank omitted fields. Clear a field by setting it explicitly. Don't reintroduce a PUT verb without re-measuring; the redundancy is a ServiceNow behavior, not a CLI choice.
- **`--display-value` defaults to `true`** on `table`, `change`, `aggregate`, `scores` (not `watch`, where it only narrows a `--hydrate` fetch). This also localizes dates into the caller's timezone/locale, and such a date will not round-trip into an encoded query — use `--display-value false` when a value has to be fed back to the API.
- **Help is organized in three tiers, and every arg carries a doc comment.** `GlobalFlags` has `next_help_heading = "Global options"` so the 11 propagated globals don't zipper through each command's own flags (clap gives every `Args` container its own display-order counter and merges them). Raw `sysparm_*` passthroughs nobody reaches for get `help_heading = ADVANCED` (the const in `cli/mod.rs`); the default "Options" section is left as the working set, in declaration order — so **declare fields in order of usefulness**. A new arg without a `///` doc comment is a bug: `sn introspect` is the audit (`args[].help` must be non-null for all of them).
- `cli::command()` wraps `Cli::command()` to rewrite every usage line as `sn <path> <POSITIONALS> [OPTIONS]`; `cli::parse()` is what `main.rs` calls, so errors and `--help` agree. Anything building the command tree (`completion.rs`) must go through `cli::command()`, not `Cli::command()`, or its usage lines will disagree.
- **The read verb is optional on `table` and `cmdb`** (`IMPLIED_VERB_GROUPS`): `sn table incident <sys_id>` → `get`, `sn table incident` → `list`. Implemented in `cli::parse` as a *retry after failure*, not `allow_external_subcommands` — argv is re-parsed with a verb spliced in only when the first parse raised `InvalidSubcommand`. Two properties carry it, and both must survive any change here:
  - **The verb is decided, not guessed.** `get` requires a second positional and `list` rejects one, so at most one of the two rewrites can parse. Never infer a write.
  - **A typo must not become a shorthand.** clap fills `ContextKind::SuggestedSubcommand` only when the token is within edit distance of a real verb, so a non-empty suggestion aborts the rewrite — that is why `sn table lst incident` still says "did you mean 'list'" instead of GETting a table named `lst`. Don't replace that check with a hand-rolled distance function; it would drift from clap's.
  - Groups with non-CRUD subcommands (`change nextstates`, `catalog checkout`) are excluded on purpose: their first token isn't reliably a noun.
- `InvalidSubcommand` errors append the group's real subcommand list (`main.rs::subcommand_hint` → `cli::valid_subcommands`), because clap names none of them unless the token is a near-miss.
- Shared `pub(crate)` helpers live in `cli/table.rs`: `build_profile`, `build_client`, `bool_opt`, `format_from_flags`, `unwrap_or_raw`, `write_response`.
- Every command's final emission goes through `cli::table::write_response(global, &value)`, which routes to `output::emit_value` (JSON, default/raw) or `output_table::write_table` (`--output table`). `unwrap_or_raw` treats `Table` like `Default` (envelope unwrapped, then rendered).

## Claude Code plugin

This repo is also a Claude Code plugin and its own marketplace (`.claude-plugin/plugin.json` + `marketplace.json` + `skills/sn/SKILL.md`). The plugin skill mirrors `.claude/skills/sn.md` but adds `allowed-tools: Bash(sn *)` for auto-approved CLI access — keep the two files in sync. `.claude/skills/sn.md` is for in-repo use (`/sn`); `skills/sn/SKILL.md` is for external distribution.

## CI/CD

- `ci.yml` — fmt + clippy + test on ubuntu/macos/windows (PRs + pushes to main).
- `security.yml` — `cargo audit` (RustSec) + `cargo deny check all` (license/source/banned-crate policy in `deny.toml`); on dep changes + daily 06:00 UTC.
- `scorecard.yml` — OSSF Scorecard; weekly + push to main; publishes to scorecard.dev.
- `release.yml` — cargo-dist on `v*` tag: builds {x86_64,aarch64}-{linux,macos,windows}, shell/PowerShell/MSI installers, Homebrew formula, and signed provenance attestations (`gh attestation verify`); auto-publishes the formula to `tehubersheezy/homebrew-sn` via `HOMEBREW_TAP_TOKEN`.
- `release-guard.yml` — hooks `Release` via `workflow_run`, so it still runs when every job inside skipped. Fails if any release job was **skipped**, if the tag published fewer than 20 assets, or if the Homebrew formula version doesn't match the tag. Exists because a green `Release` does *not* mean anything shipped (see below).
- `mask-check.yml` — **run before tagging** (`gh workflow run mask-check.yml`). Preflight that pushes the top CHANGELOG section through a job output, the exact path that breaks, and names the offending line if it would be withheld.
- Config: `dist-workspace.toml` (cargo-dist), `wix/main.wxs` (MSI template; stable GUIDs in `[package.metadata.wix]` of `Cargo.toml`), `.github/dependabot.yml` (weekly grouped dep PRs for cargo + github-actions).

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
