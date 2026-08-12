# Changelog

## Unreleased

### Added

- **`sn api list|search|spec`** — the instance's own REST API catalogue, so "is there an API
  for X?" stops being the one question that still sends a caller to a browser. `sn api list`
  summarizes every namespace with API and endpoint counts (90 namespaces, 342 APIs, 2,351
  endpoints on the reference PDI); `-n now` lists that namespace's 169 APIs; `sn api search
  attachment` returns the 43 matching endpoints, each with method and route — enough to call
  it with `sn raw`; `sn api spec "Table API"` exports the OpenAPI 3 document, and
  `--format yaml` its YAML serialization (9,556 bytes for that API).

  The endpoints behind it are the REST API Explorer's own (`/api/now/doc/namespaces`,
  `/doc/services`, `/doc/oas_3`): undocumented, unversioned, and reachable with ordinary
  credentials. `oas_3` answers `Accept: application/json` with **406** — measured live, it
  serves `application/octet-stream` whatever `?format=` says — so the group builds its client
  through the header plumbing `sn raw` already owns and sends `Accept: */*`, which the other
  two endpoints answer with JSON regardless. `--format yaml` is written to stdout verbatim,
  as `sn attachment download` does, with a trailing newline added if the instance omits one;
  everything else emits through `write_response`, so `--output table` renders the summaries
  as columns and `--output raw` prints the catalogue endpoint's own response (472 KB on a
  PDI) for piping to jq.

  `spec` takes a bare name and resolves the namespace itself, since `oas_3` matches `name`
  exactly and 404s otherwise: an exact case-insensitive hit on the catalogue title wins, a
  substring must be unique, and an ambiguous one lists the candidates rather than exporting
  the first.

  Two failure modes are diagnosed rather than guessed at. An unknown namespace comes back as
  `{"result":{}}` with HTTP 200, not a 404 — read one way that was a usage error for `list`
  and read the other an empty array with exit 0 for `search`, so a typo was
  indistinguishable from "no matches". Both now route through one check that names near
  matches (`no namespace 'nowe' on this instance — did you mean 'now'?`), which is also what
  tells a misspelling apart from a real namespace that publishes no REST API — the reference
  instance declares 94 and 90 of them appear in the catalogue. And a 404 from `oas_3` is
  explained by first probing
  `/api/now/doc/namespaces` (1.5 KB against 460 KB for the catalogue): only *its* 404
  licenses "may be absent on older releases". The `oas_3` 404 body already names the cause
  exactly (`API Table API xyz not found in namespace now`), so rewriting every 404 into a
  release-age diagnosis was replacing good information with the rarest possible cause — it
  told a user with a typo that their instance was too old.

- **`sn variables get|set <TABLE> <SYS_ID>`** — catalog variables on a record, read and
  written with verification. Reads walk whichever join holds the values
  (`sc_item_option` via `sc_item_option_mtom` for an RITM; `question_answer` for a
  record producer's target record) and emit `[{name, label, value}]` sorted by name.
  Writes go through the undocumented `PUT
  /api/sn_sc/servicecatalog/variables/{table_name}/{sys_id}` — established live as the
  only write path open to non-admin roles (direct Table API writes to `sc_item_option`
  are 403 for a plain `itil` user; the endpoint gates on `canWrite()` of the parent
  record). The endpoint's failure mode is silence — unknown names, wrong case, or a
  record that does not own the variables return 200 with nothing written — so `set`
  validates names against a pre-flight read (unknown → exit 1 listing the pool, before
  any write) and re-reads after the PUT: success reports `{updated: {name: {from, to}},
  unchanged}`, a value that did not persist is exit 2 naming the keys. An `sc_task`
  target is resolved to its `request_item` (the task does not own the RITM's variable
  pool; a direct write would be skipped silently), reported as `resolved_from`.
  Multi-row variable sets are unsupported.

- **`sn journal <TABLE> <SYS_ID>`** — comments and work notes for one record, parsed
  into structured entries (`[{created_on, author, element, label, text}]`, newest
  first). Journal entries live in `sys_journal_field`, but that table is ACL-locked
  for non-admin roles — measured live, an `itil` query returns the row count and zero
  rows. So the default `--source record` reads the record's *rendered* journal stream
  over GraphQL (readable by any role that can read the record) and parses it back into
  entries; `--source table` opts into exact `sys_journal_field` rows — UTC timestamps,
  usernames — and when rows exist but ACLs remove them all, the error names the cause
  and points back at `--source record` instead of returning an empty array. `--comments`
  / `--work-notes` filter by type (and narrow what is fetched), `--limit N` keeps the
  newest N, `--raw` emits the unparsed stream. Tables missing the combined
  `comments_and_work_notes` column fall back to the single journal columns
  automatically. Record-source timestamps are rendered in the caller's timezone and
  date format; writes need no new verb (`sn table update <t> <id> --field
  work_notes=...`).

- **`sn graphql <QUERY>`** — run a GraphQL document against `POST /api/now/graphql`,
  which serves the instance's whole GraphQL surface: the scripted namespaces plus the
  generated `GlideRecord_Query` / `GlideRecord_Mutation` / `GlideAggregateRecord_Query`
  namespaces (a query field and CRUD mutations per table, with per-field display values,
  inline choice lists, ACL-evaluated metadata, and dot-walking through references). The
  document comes inline, from `@file`, or `@-` (stdin); `--var k=v` sets a string
  variable (repeatable, first `=` splits), `--variables '{...}'` supplies a whole JSON
  object for typed values (`--var` wins on conflict), and `--operation` selects from a
  multi-operation document.

  The point over `sn raw`: GraphQL reports failure **in-band** — HTTP 200 with an
  `errors` array, sometimes alongside partial `data` — which would read as success
  under the exit-code contract. `sn graphql` maps a response with errors to exit 2,
  putting the first message in the stderr envelope and the full array under `sn_error`,
  while any partial `data` still reaches stdout first. On success, stdout gets `data`
  unwrapped, the GraphQL analogue of stripping `{"result": ...}`; `--output raw` keeps
  the whole response body.

### Changed

- **`--setLimit` is accepted wherever `--setlimit` is, and `--help` now names the
  alternatives.** Long flags match case-sensitively, so the camelCase spelling was rejected —
  and the flag is named after GlideRecord's `setLimit()`, so it is the spelling a ServiceNow
  developer already has in muscle memory. `--limit`, `--sysparm-limit` and `--page-size` were
  *already* accepted at every record-cap site but invisible, because clap prints only an
  argument's canonical name; each flag's help text now lists them. A genuine typo still gets
  clap's suggestion, and `sn introspect` already published `aliases`, so nothing about the
  machine-readable contract changes.

- **`--all` can no longer be combined with `--output raw` or `--output table`.** Both were
  accepted and then ignored — `sn table list incident --all --output table` streamed JSONL
  like any other `--all`. Both are now usage errors (exit 1) naming the conflict. Table
  cannot size a column without seeing every row, so its message points at `--array`, which
  buffers and already renders; raw means "keep the envelope", and the paginator flattens
  every page's envelope into a record stream, so under `--all` there is no envelope left to
  keep — in the `--array` form either.

  The check reads nothing but `--output` and `--array`, so it now runs *before*
  `build_profile` and `build_client`. Running it after handed a pure argv mistake to the
  IdP on an OAuth profile: the token round trip came first, so an unreachable instance
  answered `--all --output table` with exit 3 and a token-endpoint URL, and the exit-1 usage
  error naming `--array` never printed.

- **Six destructive commands now refuse to run without `--yes` when stdin is not a
  terminal**: `catalog cart-remove`, `catalog cart-empty`, `change conflict remove`,
  `profile remove`, `updateset back-out`, and `app rollback`. On a terminal they prompt. The
  refusal names the operation and its target rather than only the flag to add — `back out
  update set abc requires --yes when stdin is not a terminal`, `remove profile prod and its
  stored credentials requires --yes when stdin is not a terminal`.

  Four of them had no gate at all, and were missed because the shared guard's prompt was
  hardcoded `Delete {what}?` — a phrasing that only fits a row deletion, so anything shaped
  differently had nowhere to plug in. `catalog cart-empty` is one DELETE that discards a
  cart nothing can restore; `profile remove` erases the only copy of a password or an OAuth
  refresh token.

  `updateset back-out` and `app rollback` are gated deliberately, though both are
  recoverable in principle and a human types them on purpose. This CLI's stated audience is
  LLM agents, and both are exactly the shape an agent fires by accident: one flag,
  instance-wide effect, asynchronous, no second confirmation anywhere downstream. A back-out
  reverts every record its set applied; a rollback replaces an installed app across the
  instance without undoing what the newer version wrote. Neither has an undo of its own, and
  the cost of being wrong is asymmetric — `--yes` is one token in a script, while an
  unintended back-out is a recovery project.

- **Batch output is buffered; streamed output is flushed per record.** stdout is a
  `LineWriter` with a ~1 KiB buffer, and compact JSON has no newline to flush on, so a large
  `--array` payload left in thousands of writes. Batch emissions now go through a 64 KiB
  `BufWriter` with an explicit, propagated flush — `BufWriter::drop` throws a failing flush
  away, which would turn a closed pipe or a full disk into silent truncation with exit 0.
  Streaming emissions still flush after every record, and that is not an optimization to be
  reclaimed later: a JSONL line sitting in a buffer is indistinguishable from a hung process
  to whatever is reading the pipe, so `sn watch | jq` would look frozen. The split lives in
  `output.rs`'s functions now rather than in a comment at the call sites, which is where it
  was getting lost.

  Per-record deep clones go with it. Unwrapping `{"result": ...}` moves the subtree instead
  of copying it — once per command everywhere except `sn watch --hydrate`, where it is once
  per event — `sn schema tables --output raw` was cloning the instance's entire table list,
  and a `--wait` poll loop deep-cloned the whole progress response every 2s to read one
  `status` string off it (a 30-minute wait made ~900 copies and kept one).

### Fixed

- **`sn cmdb create` and `sn cmdb update` send the IRE envelope the CMDB Instance API
  requires.** They sent a flat body, which that API cannot accept under any input: PATCH came
  back as a raw Java NPE (HTTP 500, `"attributes" is null`) and POST as a 400 naming a null
  data source, so `--field` never worked on `cmdb` at all — `build_body` only ever produces a
  flat object. Every write now goes out as `{"attributes": {...}, "source": "..."}`.

  **A CLI write therefore stamps `discovery_source`**, from the new `--source`, which
  defaults to `"Manual Entry"`. A CLI write literally is a manual entry, so that is the
  truthful value rather than a convenience, and it makes the `--field` happy path work with
  no ceremony. Name a real source only when standing in for it: the IRE reconciles by source,
  so borrowing a discovery tool's name hands the record that tool's precedence and lets its
  next run silently overwrite a hand edit. A bad source still fails server-side, and the
  instance's `INVALID_INPUT_DATA` detail — which names the valid choices from
  `cmdb_ci.discovery_source` — reaches the caller unchanged.

  Attribute values go out as strings. The API casts each to `java.lang.String`, so
  `--field cpu_count=8` — the headline case, and the `--field operational_status=2` the docs
  advertise — answered HTTP 500 `class java.lang.Integer cannot be cast to class
  java.lang.String` and wrote nothing. Numbers and booleans are stringified; objects and
  arrays are refused up front with a usage error naming the attribute; `null` is left alone,
  since Java casts it fine and the API answers 200, so it stays usable for clearing a field.
  The rule lives in the cmdb envelope rather than in `--field` parsing because the constraint
  is this API's — the Table API takes JSON numbers happily — and it applies inside a
  hand-written envelope too, since passthrough is about the body's shape, not an opt-out of a
  cast the server performs regardless.

  A body whose `attributes` is a JSON **object** is treated as an envelope the caller wrote
  themselves and passes through untouched — also the only way to send
  `inbound_relations`/`outbound_relations` on create. Keying off the key's presence instead
  would have been wrong: 718 CMDB classes on a stock Zurich instance carry a real column
  literally named `attributes`, every one of them String or Field List, so a real field value
  is never an object on the wire and `--field` cannot produce one at all.

  A flat body's top-level `source` is refused as ambiguous (exit 1). It used to be demoted
  into `attributes`, where the API dropped it and stamped the record with the flag's or the
  default provenance instead — exit 0, wrong provenance, nothing said. Lifting it would be
  wrong too: `source` is a real String column on `cmdb_ci_service_discovered` and
  `cmdb_ci_service_calculated`, so lifting would swallow a legitimate field write on those
  classes. Both readings lose something silently, so the caller is asked which they meant;
  write the column with an explicit envelope. `--source` alongside a `source` in an envelope
  is the same usage error.

- **`--output table` is honored by `aggregate`, `scores list`, `scores favorite`, and
  `open`.** All four accepted the flag and ignored it, emitting through `emit_value` +
  `format_from_flags` and bypassing `write_response` — the one place `OutputMode::Table`
  reaches the table renderer. `format_from_flags` is private to `cli/table.rs` now, so the
  bypass is a compile error rather than a convention anyone can forget.

- **`sn attachment download` streams instead of buffering the whole file.** `download_file`
  collected the entire response body into a `Vec<u8>` before the handler wrote it anywhere,
  so peak memory tracked attachment size and a file larger than RAM could not be downloaded
  at all. The body is now drained through a fixed 64 KiB buffer: measured against a loopback
  server, 800 MB downloads at 19 MB peak RSS.

  **`--timeout` becomes a per-read idle timeout for downloads**, which reading the body
  through `Read` rather than `Response::bytes()` fixes by itself. reqwest's blocking client
  enforces its timeout by wrapping whichever future the caller is parked on: `bytes()` parks
  on the whole body, so the 30s default covered the entire transfer and killed large
  downloads that were transferring perfectly well, while `read` parks on one chunk.
  Slow-but-alive now completes; stalled still dies. No non-download request's timeout
  semantics move, and the header/connect phase stays bounded as before.

  `--out` writes through a hidden staging file in the destination's own directory and renames
  on success, so a failed download leaves a pre-existing file untouched. A truncated file
  sitting under the name the caller asked for is indistinguishable from a complete one, so
  the failure has to be loud — nothing at the destination — rather than silent; the rename is
  same-filesystem, hence atomic. The staging name is capped at `NAME_MAX` — its pid+nanos
  decoration adds ~35 bytes, which made a 240-character destination the OS accepts fail with
  `ENAMETOOLONG` — and Ctrl-C unlinks it and exits 130 rather than stranding another hidden
  `.part` file on every retry. stdout has no such option: bytes on a pipe
  cannot be recalled, so a mid-stream failure is exit 3 with an envelope naming how many
  truncated bytes already went out.

- **`--wait` honors `--output raw`.** It hardcoded the default output mode, and it looked for
  the progress link only at the top level — which under raw sits one level down inside the
  untouched envelope. So `--output raw --wait` was a silent no-op that emitted the initial,
  unpolled response as if it were final.

- **`status_code` is omitted, not `0`, when the failure carried no HTTP status.** A failed
  CICD `--wait` published `"status_code": 0` — a status no HTTP response carries, for an
  operation that failed inside a 200. `sn user me`'s dropped-query guard had the mirror-image
  defect, raising `status: 200` for a request that never failed at the HTTP layer. Agents
  branch on that field, so a status describing something other than a real HTTP error is
  worse than no status: the key is absent in both cases now. Exit codes are unchanged — both
  are still 2, because the instance did fail to serve the request.

- **`scores unfavorite`, `profile use` and `profile remove` emit JSON.** `unfavorite` printed
  nothing while `favorite` printed a body from the same endpoint; a bodyless 204 parses to
  `null`, which is nothing to branch on, so it now names the operation (`{"ok": true,
  "uuid": ...}`). `profile use` reports `{"ok": true, "profile": ..., "default": true}`, and
  `profile remove` reports `{"ok": true, "profile": ..., "removed": ..., "wasDefault": ...}`,
  plus a `next` naming `sn profile use` when the removed profile was the default and every
  later command would otherwise have nothing to resolve. `removed: false` with exit 0 for a
  profile that was not there, so the result is idempotent.

- **`sn init` names flags `init` actually accepts.** On a non-TTY without `--password` it
  pointed at `--password-stdin`, a flag `init` does not have, because the shared
  profile-writing core speaks `sn profile add`'s vocabulary. A `Caller` threaded through that
  core makes every missing-field message name a flag of the command that was invoked.

- **`sn watch` marks the gap left by a reconnect.** AMB has no replay. When an established
  session dropped, the supervisor reconnected and resubscribed correctly, but every change
  during the outage was lost and nothing in the JSONL stream said so — the only trace was a
  `-d` note on stderr, so a consumer could not tell a quiet table from a lost one, and anyone
  treating a watch as a complete feed was wrong with no way to find out. A successful
  resubscribe after a drop now writes one synthetic line,
  `{"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}`.

  It is keyed rather than shaped like an event. It carries no `operation` and no `changes` —
  the two fields `--operation`/`--on-change` match on, and the two a consumer's own jq
  predicate is most likely to test — because a marker that looked like an event would be
  dropped by exactly the pipelines that most need it. It is not an event either: it does not
  count against `--max-events` and does not reset the idle clock. And there is one marker per
  gap, not per attempt: the gap is announced, and only then cleared, after the resubscribe
  succeeds, so the reported downtime spans the whole outage however many attempts it took to
  close.

- **`sn watch --idle-timeout` measures subscribed time.** The clock started at process start,
  before the session mint and handshake, and ran through every outage — so it counted time
  the watcher was not listening. Two silent wrong answers, both exit 0 with a stream that
  just looks quiet: an instance slower to mint a session than the timeout subscribed and then
  returned without ever polling, and any outage longer than the timeout — guaranteed once
  backoff caps at 60s — made the reconnect marker the last line of the stream, so the hole it
  had just announced was never covered. Silence still accumulates across sessions, so a
  socket flapping faster than the timeout cannot hold a silent watcher open; connecting is
  simply not idleness. The forgiven interval and the marker's `downtime_ms` are one
  measurement, so they cannot disagree.

- **`sn ping` and `sn user me` no longer identify the caller with a scripted query that can
  silently vanish.** Both asked `sys_user` who we are, filtered by
  `user_name=javascript:gs.getUserName()`. An instance that cannot evaluate that term drops
  the filter, the surviving `sysparm_limit=1` returns whichever row sorts first, and the CLI
  reported a stranger with full confidence — measured on a PDI, a bogus term returns
  `survey.user` and `lucius.bagnoli`, neither of them the caller. It fails open.

  `sn ping` now probes `/api/now/sg/impersonation/session`: one request that proves the
  credentials authenticate *and* names the caller, with no query to misparse and no
  `sys_user` read ACL to satisfy. `sn user me` asks `/api/now/ui/user/current_user` for the
  caller's sys_id and reads `sys_user/{sys_id}` directly, so no `javascript:` term goes on
  the wire at all and the failure cannot happen rather than merely being detected. Both
  endpoints are undocumented, so the scripted read stays as the last rung — and it asks for
  `sysparm_limit=2` now, because `user_name` is unique, so a second row proves the filter was
  dropped and the rows are arbitrary. That case reports no user (exit 2) instead of handing
  back somebody else's record.

  Falling back is triggered by HTTP 404 and nothing else. A 401/403 is a real auth verdict
  and stays exit 4 — a ping that shopped around for an endpoint willing to answer would
  eventually report `ok` for dead credentials. A 5xx is an instance fault and says nothing
  about whether the path exists. A 200 carrying HTML (a session redirect) surfaces as a parse
  failure and propagates, since the fallback would be redirected identically. And "the
  primary worked" means a 200 whose body actually names a user.

  **`sn ping`'s `username` is now the identity the instance asserts, in preference to the
  configured one.** When the two disagree the profile is what is wrong, and echoing the
  config back verifies nothing; the configured name remains the last resort, reported as
  `identity_source: "profile"`. Seven fields join the output — `identity_source`,
  `user_sys_id`, `user_display_name`, `admin`, `can_impersonate`, `impersonating`,
  `original_user` — unconditionally rather than only under `--verbose`, because stdout is a
  machine contract here and a shape that changes with verbosity is worse for the agents that
  consume it. `impersonating` is true only when two present, non-blank names differ; a blank
  one stays null, meaning unknown, rather than being read as a real name that happens to
  differ. The session response also carries `SessionToken`, a live CSRF credential: it is
  copied nowhere, and response-body logging is suppressed for exactly that request so `-ddd`
  cannot print it either. The request count is unchanged at two, since the build version
  still needs its own `sys_properties` read.

- **Concurrent `sn` invocations no longer lose each other's config writes.** Every
  load-modify-save in the config module raced any other invocation: twelve parallel
  `sn profile add`s left **one** profile on disk, and eleven of them exited 0 printing
  success JSON for a profile that no longer existed. `save_profile`, `remove`, `set_default`
  and the verification rollback now run their entire read-modify-write under an exclusive
  advisory lock, and `sn profile add`'s refusal to clobber is re-checked inside it (the
  pre-prompt check has to stay — nobody should type a password that gets discarded — but on
  its own it leaves a window as wide as the prompt).

  The lock lives on a new `.sn.lock` sidecar in the config directory, never on the config
  files themselves: those get replaced by rename, so a lock on one of them is a lock on an
  inode the next writer never opens and nobody would ever block. One lock covers the whole
  directory rather than one per file — the two files are written as a pair, and a single lock
  has no ordering and so no deadlock — and acquisition is re-entrant per thread, because
  flock(2) locks an open file description and a nested acquire would otherwise block the
  process against itself. Contention is bounded by a **10s timeout** that then fails with
  exit 1 naming the lock file, so an agent gets a diagnosable error instead of an invocation
  that never returns.

  Readers take no lock — every invocation reads the config, and contending would trade a rare
  lost update for a common stall — so the write order pairs with theirs: adding writes
  `credentials.toml` first and `config.toml` second, which makes "config entry with no
  credentials", i.e. `Auth::Basic("", "")` and a baffling 401, an arithmetically impossible
  interleaving rather than an unlikely one. Removal goes the other way for the same reason.

  Verification rollback no longer replays a whole-file snapshot. Verification is a network
  round trip, so the rollback lands seconds after the write, and writing both files back
  wholesale erased any profile a sibling invocation had added meanwhile — one dead profile
  traded for a live one. Only the displaced entry is restored, and `default_profile` reverts
  only if this run is still the one holding it. Verification itself stays outside the lock:
  it can involve a browser and a human.

- **Parallel OAuth refreshes are single-flight.** `ensure_access_token` re-checks staleness
  *inside* the lock and skips the refresh if a sibling already landed a fresh token.
  Duplicate refreshes were not just wasted round trips: under refresh-token rotation the
  loser presents a refresh token the IdP already consumed, gets a 401, and exits 4 asking for
  re-login moments after a good token was minted. The cached-and-valid fast path still takes
  no lock and does no I/O; the refresh path holds the lock across the token round trip and so
  carries a budget derived from the HTTP timeout, or a slow IdP would turn every parallel
  invocation into a spurious lock timeout.

- **A crash mid-write can no longer leave a torn config file.** `fs::write` truncates in
  place. Contents now go to a temp file in the same directory (same directory because rename
  is only atomic within one filesystem), are `sync_all`'d, and are renamed over the target;
  on Unix the containing directory is fsynced after the rename, so the guarantee covers the
  directory entry and not only the contents.

### Security

- **`credentials.toml` is created 0600 by `open(2)` itself, closing a world-readable
  window.** `save_credentials_to` wrote the file and *then* chmod'd it, so a brand-new
  `credentials.toml` — with a password or an OAuth refresh token already inside — sat at the
  umask default, normally 0644, until the chmod landed. The atomic write path creates its
  temp file 0600 at open and renames it into place, so there is no chmod and therefore no
  window.

- **`config.toml` is now 0600 as well** — it was 0644. It names the instances and OAuth
  clients a user talks to, and nothing else needs to read it. Because rename carries the temp
  file's mode across, the first write from this release also repairs a `config.toml` an older
  release left at 0644.

### Dependencies

- **`fs4` 1.1** (`sync` feature only), for the config-directory advisory lock. It is the
  maintained fork of the dead `fs2` and wraps `flock(2)`/`LockFileEx` behind one safe API,
  where reaching for `rustix`/`windows-sys` directly would mean unsafe FFI here. MIT OR
  Apache-2.0, crates.io as its only source, the same 1.75 MSRV this crate declares, and no
  transitive crate we were not already building. std's `File::lock` would have cost MSRV
  1.89.

- **`tempfile` moves from dev-dependencies to dependencies**, since the atomic config write
  uses it at runtime.

## 0.11.0 (2026-08-04)

Two threads run through this release. The command surface gets the ergonomics a caller
expects: the read verb is optional on `table` and `cmdb`, every argument is documented,
`--help` is organized rather than exhaustive, and one write verb that never did what its
name implied is gone. And `sn introspect` — the machine-readable face of that surface —
becomes something you can generate schemas from: 73% smaller, free of the phantom nodes
that made up most of it, and carrying the argument constraints a generator needs.

### Breaking changes

- **`sn table replace` and `sn cmdb replace` are gone. Use `update`.** They issued PUT
  where `update` issues PATCH, and ServiceNow treats both as partial updates — so the
  two verbs did the same thing while implying they did not. Measured on a live instance
  by writing one field to two identical records, one per verb: both moved exactly the
  field written plus the same three derived columns (`sys_mod_count`, `sys_updated_on`,
  `activity_due`), and PUT blanked none of the ten fields it omitted. Same result on
  `/api/now/cmdb/instance`. Anything calling `replace` should call `update` with the
  same body; no other change is needed.

- **`--display-value` now defaults to `true`.** Reference and choice fields come back as
  readable labels (`"Software"`, `"In Progress"`) instead of sys_ids and numeric codes.
  Applies to `table`, `change`, `aggregate`, and `scores`; `sn watch` is unaffected,
  since its `--display-value` only narrows a `--hydrate` fetch.

  Two consequences worth knowing. Dates and times are also rewritten, into the calling
  user's timezone and locale format (`2026-08-04 14:22:01` becomes `08/04/2026 10:22:01
  AM`), and a display-formatted date cannot be fed back into an encoded query. And a
  reference field yields a name rather than the sys_id you would need to chain into
  another call. Pass **`--display-value false`** for the old raw output, or
  **`--display-value all`** for both (each field becomes a `{value, display_value}`
  object).

- **`sn introspect` emits the global flags once at the root, not on every command.** The
  root gains `global_args`; each node's `args` now holds only that command's own
  arguments. **A command's effective flags are its own `args` plus the root's
  `global_args`** — non-global arguments do not propagate, so there is no ancestor chain
  to walk. Anything generating per-command schemas needs to union the two.

  clap propagates the 11 globals onto every node, and that repetition was most of what
  the tree contained: of 0.10.0's 1,852 argument entries, 1,353 were those same 11 flags
  repeated and another 124 were `--help`/`--version` stubs — 80% of the output, leaving
  375 entries that described anything command-specific. Together with the phantom-node
  fix below, the tree drops from 486 KB to 129 KB.

### Added

- **The read verb may be omitted on `table` and `cmdb`.** `sn table incident <sys_id>` is
  `get`, `sn table incident` is `list`, and the same for `sn cmdb <class> [sys_id]`. Both
  groups map to a REST path that is already `{noun}/{id}`, so for a read the verb is
  implied by the method. (Reported as #23, where an agent wrote exactly this form and got
  `unrecognized subcommand 'sc_cat_item'` with nothing to act on.)

  Only `get` and `list` are ever inferred — never a write — and the choice is decided, not
  guessed: `get` requires a second positional and `list` rejects one, so exactly one of
  them can parse any given argv. A misspelled verb is still a misspelled verb:
  `sn table lst incident` keeps clap's `tip: a similar subcommand exists: 'list'` rather
  than fetching from a table named `lst`. Groups with non-CRUD subcommands (`change`,
  `catalog`) are excluded, since there the first token is not reliably a noun.

- **`sn raw -H/--header 'Name: Value'`** — send request headers on the REST passthrough,
  curl-style and repeatable. Endpoints that need one (`X-no-response-body`, Scripted REST
  API custom headers, `Accept` negotiation, `X-UserToken`) were previously unreachable,
  since `sn raw` could only vary method, path, query, and body.

  Caller headers are applied last, after the client's own, so they win — `Content-Type`
  included. Repeating a name sends it twice. A malformed header is a usage error (exit 1),
  not a panic.

  `Authorization` is refused: a profile is the whole unit of identity in this CLI, and a
  credential passed in argv is visible to `ps` and to shell history. Configure identity
  with `sn init` or `sn profile add`.

  Both directions stay JSON regardless of the headers set. Responses are parsed as JSON,
  so `Accept: text/xml` fetches XML and then fails to parse; `--data`/`--field` always
  serialize JSON, so `Content-Type: application/xml` only mislabels a JSON body.

- **`-p` as a short alias for `--profile`.** Global, so `sn -p prod table list incident`
  and `sn table list incident -p prod` are equivalent.

- **`sn introspect` emits the constraints a schema generator needs.** Each `args[]` entry
  gains `conflicts_with` (the arg ids that cannot be combined with it — `--data` alongside
  `--field` is exit 1), `value_name` (the `SECS`/`URL`/`PATH` placeholder `--help` shows),
  and `help_heading` (`Global options`, `Advanced options`, or `null` for a command's
  working set). The root gains `version`, so a consumer caching generated schemas can
  invalidate them on an upgrade.

  One relation stays out of reach: clap keeps `requires` private, so `--wait-timeout`
  requiring `--wait` is still only prose in that flag's `help`.

### Fixed

- **`unrecognized subcommand` now names the subcommands that exist.** clap only offers a
  suggestion when the bad token is within edit distance of a real one, which a table name
  never is — so `sn change sc_cat_item abc` printed a usage line and nothing else. The
  error now appends `` `sn change` subcommands: list, get, create, … `` for every group.

- **`sn introspect` no longer emits clap's generated `help` subcommand.** clap generates
  one at every level that has subcommands, and each mirrors its whole subtree as
  argument-less stubs — **274 of 0.10.0's 397 nodes were phantoms.** Most of the tree
  described nothing callable, and every real command had a same-named twin: `sn help table
  list` sat beside `sn table list` with an empty `args`, indistinguishable to a schema
  generator. Any recursive walk — including the one `docs/agent-guide.md` prescribed —
  saw mostly phantoms. The tree is now 121 nodes, all of them real.

  `--help` and `--version` are dropped from `args[]` too. They exit before any handler
  runs, so there is nothing for a generated tool to call. The filter keys on clap's
  *action*, not the argument name, because `sn app install --version <VERSION>` is a real
  value-taking option that a name-based filter would have deleted.

- **`sn introspect` honors the global output flags it accepts.** `--pretty`, `--compact`,
  and `--output table` all parsed and were then discarded, because the command hardcoded
  its own formatter instead of routing through the shared `write_response`. It also built
  its tree from `Cli::command()` rather than `cli::command()`, so it described a command
  tree whose usage lines differed from the one `--help` renders.

### Changed

- **`--help` is organized instead of exhaustive.** The 11 global flags used to be
  interleaved with each command's own — clap gives every argument container its own
  display-order counter and merges them, which zippered `--profile`/`--output`/`--pretty`
  through the middle of `--query`/`--fields`/`--setlimit`. They now sit in their own
  `Global options` section below the command's flags, on all 120 subcommands. Raw
  `sysparm_*` passthroughs most callers never touch (`--view`, `--query-category`,
  `--query-no-domain`, `--no-count`, `--suppress-pagination-header`,
  `--exclude-reference-link`) moved to `Advanced options`. `sn scores list` additionally
  splits its score-series flags into `Score data options`.

- **Every flag and positional now has help text.** 123 arguments across 54 commands had
  none — `sn table get --help` listed `<TABLE>`, `<SYS_ID>` and five flags with blank
  descriptions. All 362 command-specific arguments and all 11 globals are documented now,
  and `sn introspect` is the audit: a test walks the tree and fails on any null `help`.

- **Usage lines put `[OPTIONS]` last**: `sn table get <TABLE> <SYS_ID> [OPTIONS]`, which
  is the order people type. Parsing is unchanged — flags are still accepted in any
  position, before or after positionals and before or after the subcommand.

## 0.10.0 (2026-07-14)

### Breaking changes

- **`sn watch table` no longer reads the record for each event.** An AMB event already
  carries the fields that changed *and their new values*: `record` holds every field
  named in `changes` as a `{display_value, value}` pair, plus five `sys_*` audit columns.
  0.9.1 fetched the record anyway — one Table API read per event — and overwrote `record`
  with the result. The event's own record is now emitted as-is, and no API call is made.

  Pass **`--hydrate`** for the old behavior: one Table API read per event, replacing
  `record` with the whole row. Use it when you need fields that did *not* change. An event
  about `state` carries no `number` and no `assigned_to`, because they were not written.

  `--fields` and `--display-value` now require `--hydrate`, since they only affect that
  fetch; accepting them without it would silently do nothing. `--no-hydrate` is still
  accepted and does nothing, as it now describes the default.

  Note that a hydrated record is the row as of the *fetch*, not as of the event. A record
  written twice in quick succession can hydrate the first event with the second event's
  values. The event's own record has no such skew.

### Fixed

- **The docs claimed AMB events carry no field values.** They do, and always did — the
  claim ("its payload carries only `sys_*` columns, never what they changed to") was in
  the README, both skills, and `watch.rs` itself, and it is what put hydration on by
  default in 0.9.1. Corrected everywhere, verified against a live instance.

## 0.9.1 (2026-07-13)

Adds `sn watch`: live record watchers. Record changes are streamed over ServiceNow's
AMB websocket as they happen, as JSONL on stdout, one event per line.

### Added

- **`sn watch table <TABLE> --query <ENCODED_QUERY>`** — stream changes to every record
  matching the query. `--sys-id <SYS_ID>` watches a single record.
- **`sn watch count <TABLE> --query <ENCODED_QUERY>`** — stream changes to the number of
  records matching the query.
- **`sn watch activity <SYS_ID>`** — stream a record's comments, work notes and field
  changes.
- **`sn watch channel <CHANNEL>`** — subscribe to a raw AMB channel, for channels the
  CLI does not model.
- **`--operation insert|update|delete`** and **`--on-change <FIELDS>`** filter the
  stream. **`--max-events <N>`**, **`--duration <SECS>`** and **`--idle-timeout <SECS>`**
  bound it. Ctrl-C exits 0.

### Notes

- `sn watch count` reports a delta, not a total (`{"count": "+1"}`). Seed it with
  `sn aggregate <TABLE> --count --query <ENCODED_QUERY>` and accumulate.
- Proxies are not supported. The websocket is opened directly rather than through the
  HTTP client, so a profile with a proxy configured exits 1 instead of connecting around
  it. `--insecure` and `--ca-cert` are honored.

## 0.9.0 (2026-07-12)

Creating a profile was only ever possible through `sn init` — a wizard that prompts
for whatever you left out, reports its result to a human on stderr, and claims
`default_profile` when none is set. Scripting it meant hoping you'd passed enough
flags to keep it from blocking on a read that would never be answered. This release
splits the job in two: `sn init` stays the onboarding wizard, and `sn profile add`
becomes the scriptable half.

### Added

- **`sn profile add [NAME]`** — register a profile without the wizard. It emits JSON
  on stdout, and **never prompts when stdin is not a terminal**: a missing field is
  exit 1 naming the flag that supplies it, so it cannot hang a pipeline. It refuses
  to overwrite an existing profile (exit 1; `--force` opts in), and it leaves
  `default_profile` alone — `--set-default`, or `sn profile use`, does that
  deliberately. `--non-interactive` forces the fail-fast behavior on a terminal too.
- **`sn profile add --password-stdin` / `--client-secret-stdin`** pipe a secret in
  rather than passing it on the command line, where `ps` and shell history can see
  it. (`sn init` has neither; it prompts.)
- **`sn profile add --no-verify`** registers a profile without any network call, for
  air-gapped provisioning or config management that runs before the instance is
  reachable.

### Breaking changes

- **`sn init` now always claims `default_profile`.** It previously set it only when
  no default existed, which made "set up my connection" quietly do nothing to a
  machine that already had one. Onboarding onto a profile now means using it. Use
  `sn profile add` to register an instance *without* repointing your commands.

### Fixed

- **Login reported the wrong person.** `sn auth login` and `sn init --auth oauth`
  named the authenticated user by reading `sys_user` with `sysparm_limit=1` — which
  returns whichever row happens to sort first, an arbitrary account that was never
  the caller. The identity now comes from `gs.getUserName()` server-side, the way
  `sn user me` always did.
- **A profile that fails verification is no longer left on disk.** `sn init` wrote
  the config files first and checked the credentials second, so a typo'd password
  left a broken identity behind — and, on a machine with no default yet, made it the
  default. Both commands now roll the write back, so a failed `add`/`init` leaves no
  profile and changes no default.
- **`sn init` no longer invents a bogus instance.** With a non-terminal stdin the
  instance prompt read EOF as an empty answer, which `normalize_instance` then turned
  into the bare suffix `.service-now.com` — a *non-empty* string, so the
  `instance is required` guard never fired and was in effect dead code. A scripted
  `sn init --username u --password p` therefore wrote a profile named `default`
  pointing at `https://.service-now.com`, made it the default, and only *then* failed
  resolving it. Missing fields now name themselves (exit 1), an empty instance stays
  empty, and nothing is written.
- **`sn attachment download` panicked on every invocation.** Exit 101, flag or no
  flag, since the command shipped. Its local `--output <PATH>` (a string) collided
  with the CLI-wide `--output default|raw|table` (an enum): clap merges arguments by
  id, so the local definition shadowed the global one's type and the parser then
  tried to read an `OutputMode` out of a `String`. **The destination flag is now
  `--out` / `-o`** — `--output` keeps its CLI-wide meaning everywhere. Nothing
  exercised `attachment download`, so a total crash went unnoticed across releases;
  it has tests now.
- **`sn open` emitted a URL with no scheme.** Profiles store the bare host, and
  `open` interpolated it straight into the link, producing
  `acme.service-now.com/nav_to.do?...` — which no browser will open. It now goes
  through the same `normalize_base_url` the HTTP client uses. This affected every
  profile created the documented way; nothing exercised `sn open`, so it went
  unnoticed. There are tests now.
- **`sn progress -d` printed no percentage.** ServiceNow sends `percent_complete` as
  a JSON string on some operations and a number on others; the code only read the
  string form and silently skipped the rest.
- **`sn ping` printed an empty username on OAuth profiles.** An OAuth profile stores
  no username — the identity is in the token — so `ping` reported `""`. It now asks
  the instance. Basic profiles still report their configured username, which is what
  proves a stray environment variable didn't swap the credentials out.

### Docs

An adversarial pass over the docs — every claim executed against the compiled
binary and a live instance — found six response shapes that were **invented**, and
that would silently mislead any agent trusting them. All corrected, with the real
shapes captured verbatim:

- `schema tables` puts the table name in **`value`**, not `name` — `jq -r '.[].name'`
  returned `null` for every row.
- `schema columns` has no `choice_field` and no `default_value`; the default is
  `default`, and a choice column is `type: "choice"` with its options inlined in a
  `choices[]` array.
- `aggregate --group-by` returns an **array**, and `groupby_fields` is a *sibling* of
  `stats`, not a member — the documented `jq '.stats.groupby_fields[]'` matched
  nothing.
- `change` returns every field as a **`{display_value, value}` pair**, so `.number` is
  an object, not a string. `change nextstates` returns an object keyed by
  `available_states`/`state_label`, not a list of `{value, label}`.
- `cmdb get` nests the CI's fields under **`.attributes`**.
- `scores list` returns `direction`/`frequency` as **integer codes**; the words live
  in `direction_label`/`frequency_label`.
- `introspect` emits a **recursive tree** (`{name, about, args[], subcommands[]}`).
  There is no `.commands[]`, and the documented `jq` recipe failed outright.
- The `--wait` recipe read the command's stdout on its failure branch, where stdout
  is **empty** (the progress object goes to stderr), and matched on `status_label` —
  a verbatim ServiceNow string that varies by instance, which is how you write a poll
  loop that never terminates. Branch on the exit code and the numeric `status`.

### Internal

- `sn init` and `sn profile add` share one profile-writing core in `cli/profile.rs`
  (`resolve_name` → `resolve_input` → `save_and_verify`) and differ only in policy,
  so the two paths cannot drift. The authenticated-identity read is likewise shared
  (`auth::whoami`).

## 0.8.0 (2026-07-11)

An adversarial review of the docs — checked against the compiled CLI, ServiceNow's
official API docs, the published release assets, and a live instance — drove this
release: every documented claim is now either verified true or fixed, plus two
classes of code defects the review surfaced.

### Breaking changes

- **Every destructive `delete` now requires confirmation.** `change delete`,
  `change task delete`, `attachment delete`, and `cmdb relation delete` gain the
  guard `table delete` already had: a `[y/N]` prompt on a TTY, and a required
  `--yes`/`-y` when stdin is not a terminal (exit 1 with a usage error instead of
  deleting silently). Scripts calling these commands must add `--yes`.

### Added

- **Single-letter short flags** on the highest-traffic parameters: `-q`
  (`--query`), `-f` (`--fields`), `-D` (`--data`), `-F` (`--field`). Capitals
  mirror curl's `-d`/`-F` mnemonics; lowercase `-d` is the verbosity ladder and
  `-f` belongs to `--fields`.
- **`.claude-plugin/marketplace.json`** — the repo is now its own Claude Code
  plugin marketplace, so the documented install flow works as written:
  `claude plugin marketplace add tehubersheezy/servicenow-cli`, then
  `claude plugin install sn`.

### Fixed

- **Verbose logging no longer leaks secrets.** `-ddd` printed OAuth
  token-endpoint responses — live access and refresh tokens — in cleartext;
  token values are now masked (metadata like `token_type` / `expires_in` stays
  readable). `-dd` masked only `Authorization`; it now also masks `Set-Cookie`
  session tokens, and the mask label no longer misstates the auth scheme on
  OAuth profiles.
- **Docs no longer claim PUT blanks omitted fields.** `replace` was documented
  as "full overwrite — omitted fields are blanked"; ServiceNow actually applies
  PUT as a partial update (verified against a live instance and the official
  Table API docs). The docs now say so and explain how to clear a field
  explicitly.
- **The `cmdb relation add` example payload was unusable** — bare
  `type`/`target` keys; the API requires them wrapped in
  `outbound_relations`/`inbound_relations`. Fixed in the README, agent guide,
  and both skills.
- **The documented Claude-plugin install command didn't exist**
  (`claude plugin install --dir`); replaced with the real marketplace flow.
- Documentation gaps closed: the stderr envelope's `sn_error` field, the global
  `--timeout`, the Parameters table's missing rows
  (`--suppress-pagination-header`, `--query-category`) and per-command
  `--setlimit` defaults, the `attachment download` `--output` file-path
  double-meaning, and the TOC's missing Shell completions entry.

## 0.7.1 (2026-07-08)

### Fixes

- **`updateset create` sent the wrong query parameter.** It posted `name=…`, but
  the CICD Update Set API's required parameter is `update_set_name` (verified
  against the official docs on the australia/zurich/yokohama families). ServiceNow
  ignores unknown query params, so the required name never arrived. The `--name`
  flag is unchanged (with `--update-set-name` as an alias); only the wire parameter
  was corrected.
- **`updateset retrieve` ignored its source selectors.** The flags sent
  `source_id` / `source_instance_id`, but the API expects `update_source_id` /
  `update_source_instance_id` — so the selectors were silently dropped and retrieve
  always fell back to ServiceNow's own source resolution. Flags renamed to
  `--update-source-id` / `--update-source-instance-id` and the wire parameters
  corrected. (These flags never functioned before, so nothing that worked breaks.)
- **`sn ping` now honors `--output table`.** It emitted JSON regardless of the
  output mode; its final emission routes through `write_response` like every other
  command, so `--output table`/`--pretty`/`--compact` all apply.

### Changed

- **`-v` prints the version** (with `-V` kept as an alias); the verbose logging
  ladder moves to **`-d` / `-dd` / `-ddd`** (long form `--verbose`). This reverses
  the 0.6.1 `-v`-is-verbose choice in favor of the more common version-on-`-v`
  convention.
- **`sn init --auth oauth` registers a public PKCE client by default** and no longer
  prompts for a client secret on the interactive authorization-code flow. Pass
  `--client-secret` explicitly for a confidential authorization-code client;
  `client_credentials` still requires one.

## 0.7.0 (2026-07-08)

A coherence pass on authentication and profile handling. A profile is now the
single unit of identity: commands either **manage** profiles (`sn init`,
`sn profile *`) or **use** exactly one (`--profile` > `default_profile`). Nothing
mixes stored profile state with per-invocation argv fragments anymore.

### Breaking changes

- **Removed the `--instance-override`, `--username`, and `--password` global
  flags.** They grafted argv fragments onto a stored profile's identity, producing
  chimeras — half from disk, half from the command line. On an OAuth profile,
  `--instance-override` redirected the token endpoint, sending the refresh token
  and client secret to an arbitrary host. Change identity by editing the profile
  (`sn init`) or selecting another (`--profile`).
- **Removed the phantom `"default"` profile fallback.** With no `--profile` and no
  `default_profile`, `sn` used to invent a profile named `"default"` that nobody
  created, surfacing errors about a phantom. It now fails fast: `no profile
  selected; pass --profile <name> or run \`sn init\``.
- **`sn auth login` is now a pure session command with no flags.** It previously
  doubled as a second, partial `sn init` — writing `client_id`/`grant`/
  `redirect_uri`, force-converting a profile to OAuth, and able to persist an empty
  instance while minting tokens against an `--instance-override` host. It now
  resolves the selected profile, requires `auth = "oauth"` with an `[oauth]` block
  (a basic profile errors with `does not use oauth; run \`sn init\``), runs the flow
  with the stored grant, and caches tokens. Configure OAuth via `sn init --auth
  oauth`.
- **Removed `sn auth test`.** Use `sn ping` — it verifies auth and adds latency and
  the ServiceNow build version.
- **Empty/whitespace `instance` is rejected** instead of silently producing a
  scheme-only `https://` base URL.

### Added

- **`SN_CONFIG_DIR`** — points directly at the directory holding `config.toml` and
  `credentials.toml` (no `sn` subdirectory appended), overriding the platform-native
  location on every OS. This is the cross-platform config-isolation mechanism,
  superseding the Linux-only `XDG_CONFIG_HOME` hack; config-dependent integration
  tests are no longer `#[cfg(target_os = "linux")]`-gated.
- **Richer `sn profile list` / `sn profile show`.** `list` reports each profile's
  `auth` method and a `default` marker; `show` surfaces the auth method and, for
  OAuth profiles, the client_id, grant, redirect_uri, pkce, and token state
  (`loggedIn`/`hasRefreshToken`/`expiresAt`) with all secret material redacted.

### Changed

- **`sn auth login` / `logout` / `refresh` now emit success JSON to stdout**
  (joining `status`), honoring the machine contract; all four also honor
  `--output`/`--pretty`/`--compact`.
- **Re-running `sn init` over an existing profile is non-destructive** — it merges
  onto the stored profile, clears only the secrets of the auth method being switched
  away from, and preserves `proxy_username`/`proxy_password`.

### Migration

| Old | New |
|---|---|
| `sn --instance-override URL --username U --password P table list …` | `sn init` a profile once, then `sn --profile NAME table list …` |
| `sn auth login --client-id … --grant … --instance …` | `sn init --auth oauth …`, then `sn auth login` |
| `sn auth test` | `sn ping` |
| relying on the implicit `"default"` profile | `sn profile use NAME` (sets `default_profile`) or pass `--profile` |
| `XDG_CONFIG_HOME` (Linux-only) for config isolation | `SN_CONFIG_DIR` (all platforms) |

## 0.6.1 (2026-07-04)

### Fixes

- **Exit-code contract at the CLI edge.** Clap parse errors (unknown flags,
  missing args) are now intercepted via `try_parse` so usage mistakes exit `1`
  — clap's default `2` is reserved for API errors — and emit the JSON error
  envelope on stderr when stderr is piped. `--help`/`--version` still exit `0`.
- **`-v` is now `--verbose`** (as the help text always claimed); `--version`
  moves to `-V` per clap convention.
- **`import bulk`** accepts the README-documented bare JSON array and wraps it
  as `{"records": [...]}` for `insertMultiple`; pre-wrapped objects still pass
  through unchanged.
- **`introspect`** builds the clap command before describing it, so boolean
  flags no longer report `takes_value: true` with `["true","false"]` (which led
  agents to emit `--all true`); adds `positional`, `repeatable`, and
  `default_values` fields.
- **`--wait-timeout <SECS>`** now bounds the CICD poll loop (exit `3` on
  expiry); all eight async CICD call sites route their final emission through a
  shared `finish_cicd`, so `--output table` works under `--wait`.
- **`-vvv` body logging** truncates on a char boundary instead of panicking
  mid-UTF-8 sequence.

### OAuth

- `sn init`'s OAuth branch prompts for the client secret immediately after the
  client id, and skips the redirect-URI prompt under `client_credentials`.
- **OAuth scope removed entirely** (flag, config field, request parameter).
  ServiceNow grants scopes through the Application Registry entry an admin
  configures, so a client-requested scope granted nothing and only invited
  misconfiguration. Existing `config.toml` files with a leftover `scope=` line
  still parse (serde ignores unknown keys).

### Dependencies

- Bump `quinn-proto` to 0.11.15 (RUSTSEC-2026-0185).

### CI

- The security workflow's `cargo audit` job now installs a prebuilt cargo-audit
  binary (via `taiki-e/install-action`) instead of compiling it from source,
  which had been failing intermittently on crates.io index fetches.

### Docs

- README gains a table of contents, an at-a-glance command block, and an
  OAuth / SSO setup section documenting `sn auth login/status/refresh/logout`.

## 0.6.0 (2026-06-16)

### OAuth 2.0 / SSO authentication

- Profiles can now authenticate via OAuth 2.0 (`auth = "oauth"`) in addition to
  HTTP Basic — the supported path for instances fronted by external SSO
  (Okta/Azure AD/ADFS), where a human's password lives in the IdP and Basic auth
  cannot work.
- Two grants:
  - **`authorization_code`** — interactive browser flow with a loopback redirect
    server (RFC 8252) and PKCE S256 by default. The redirect URI defaults to
    `http://localhost:8400/callback` and must be registered exactly in
    ServiceNow's Application Registry.
  - **`client_credentials`** — non-interactive service-to-service tokens.
- New commands:
  - **`sn auth login`** — configure OAuth, run the flow, cache tokens, and verify
    (`--client-id`, `--client-secret`, `--redirect-uri`, `--scope`, `--grant`,
    `--no-pkce`, `--instance`).
  - **`sn auth status`** — show the resolved auth method and token expiry.
  - **`sn auth refresh`** — force a token refresh now.
  - **`sn auth logout`** — discard cached tokens.
- **`sn init`** now offers `basic` or `oauth` setup interactively (and via the
  same flags), so a profile can be stood up end to end in one command.
- Tokens are refreshed (or minted, for client-credentials) transparently before
  every request; new tokens are persisted automatically. Non-secret OAuth config
  lives in `config.toml`; the client secret and tokens live in
  `credentials.toml` (chmod 0600 on Unix).

Backward compatible: existing `config.toml` files without an `auth` field
continue to behave as `basic` profiles.

## 0.4.1 (2026-04-25)

### Fixes

- **Release pipeline** (v0.4.0 was tagged but never published).
  - `wix/main.wxs` was regenerated after the repo rename so the MSI's
    `ARPHELPLINK` ("More info") points at the new
    `tehubersheezy/servicenow-cli` URL. `dist plan` rejected v0.4.0 because
    the WXS template hadn't been refreshed alongside `Cargo.toml`'s
    `homepage` field.
  - ARM64 Windows builds now run on a native Windows runner
    (`windows-latest`) via `[dist.github-custom-runners]`. The default
    Linux runner couldn't cross-compile `ring` because its build script
    emits MSVC `/imsvc` flags that clang on Linux rejects.

## 0.4.0 (2026-04-25)

### New commands

- **`sn user me`** — returns the currently authenticated user's record. Resolves
  the identity via `gs.getUserName()`, so it works regardless of auth method
  (basic auth, OAuth, etc.).
- **`sn ping`** — one-shot health check. Returns auth status, instance URL,
  username, end-to-end latency in ms, and the ServiceNow build name/tag if
  reachable. Useful as the first thing to run when something breaks.
- **`sn open <table> <sys_id>`** — opens the ServiceNow web UI form for a record
  in the default browser via `nav_to.do?uri=...`. Pass `--print-url` to print
  the URL to stdout instead of launching a browser.
- **`sn raw <method> <path>`** — generic REST passthrough for endpoints that
  aren't yet modeled as typed commands. Accepts arbitrary methods (case
  insensitive), `--query key=value` (repeatable), and the same `--data` /
  `--field` body sources as the typed commands. Response is emitted exactly as
  ServiceNow returns it (no envelope unwrapping). The escape hatch for the long
  tail of ServiceNow's API surface.
- **`sn completion <shell>`** — generate tab-completion scripts for `bash`,
  `zsh`, `fish`, `powershell`, and `elvish` via `clap_complete`.

### New output mode

- **`--output table`** — render JSON results as a human-readable columnar table
  using `comfy-table`. Suitable for interactive browsing; for scripts and
  pipelines, leave the default JSON output. Single objects render as a
  two-column key/value table; arrays of objects render as a wide table with the
  union of keys as headers; empty arrays render as `(no records)`.

### Internal

- New shared helper `cli::table::write_response(global, value)` centralizes
  output dispatch so each command's emit site is a one-liner. All read/write
  command call sites now route through it instead of constructing
  `emit_value(...)` chains.
- Six new modules: `src/cli/{user,ping,open_record,raw,completion}.rs` and
  `src/output_table.rs`.
- New deps: `clap_complete = "4"`, `webbrowser = "1"`, `comfy-table = "7"`.

## 0.3.3 (2026-04-25)

### Distribution

- **Windows MSI installers.** The release pipeline now builds signed-ready
  `.msi` installers for both x86_64 and ARM64 Windows
  (`sn-x86_64-pc-windows-msvc.msi`, `sn-aarch64-pc-windows-msvc.msi`).
  Suitable for unattended deployment via SCCM/Intune/Group Policy:
  `msiexec /i sn-...msi /qn`.
- **ARM64 Windows binary.** Native build for Surface Pro X and Copilot+ PCs
  (`sn-aarch64-pc-windows-msvc.zip`), avoiding x86 emulation overhead.

### Internal

- Added `authors` field and `[package.metadata.wix]` GUIDs to `Cargo.toml`
  (required for stable MSI upgrade behavior across releases).
- Added `wix/main.wxs` (cargo-wix's MSI definition template, generated by
  `dist init`).

## 0.3.2 (2026-04-25)

### Distribution

- **Homebrew tap.** `sn` is now installable via Homebrew:

  ```bash
  brew install tehubersheezy/sn/sn
  ```

  The release workflow auto-publishes the cargo-dist-generated formula
  to [tehubersheezy/homebrew-sn](https://github.com/tehubersheezy/homebrew-sn)
  on every tagged release.

## 0.3.1 (2026-04-24)

### Documentation

- All write subcommands (`table`, `cmdb`, `catalog`, `change`, `import`,
  `identify`) now show consistent `--data` and `--field` help text covering
  the `@file` and `@-` (stdin) idioms. The binary always supported these,
  but only `sn table create` documented them — every other write command
  was mute, leading users to invent shell-quoting workarounds for
  multi-line content.
- `sn --help` now ends with a `BODY INPUT` reference and three concrete
  examples covering multi-line file bodies, file-backed field values
  (`--field description=@notes.md`), and stdin-piped input
  (`jq … | sn … --data @-`).

### Tests

- Added integration tests pinning `sn table update --data @file.json` and
  `sn table update --field name=@file.txt` so the multi-line write paths
  stay regression-tested.

## 0.3.0 (2026-04-23)

### Breaking

- `-v` is now the short flag for `--version` (was `--verbose`). Use `--verbose`
  (or `-vv`, `-vvv`) for verbosity levels. Scripts relying on `sn -v <cmd>` for
  verbose output must switch to `sn --verbose <cmd>`.

### Improvements

- **Observability is live.** `--verbose` logs `METHOD url` + elapsed ms to
  stderr. `-vv` adds response headers. `-vvv` adds request/response bodies
  (truncated). The logger functions existed previously but were never wired in.
- **HTTP error bodies no longer disappear.** Non-JSON 5xx responses (proxy
  errors, WAF blocks, upstream HTML) now surface the first 500 chars of the
  body as `detail` in the error envelope instead of collapsing to `HTTP 502`.
- **Broken-pipe handling.** `sn … | head` exits 0 silently instead of exit 1
  with a `{"error": {"message": "stdout: ..."}}` envelope on stderr.
- **`sn init` respects global proxy/TLS flags.** `sn init --proxy … --insecure
  --ca-cert …` now uses those settings for credential verification *and*
  persists them to the saved profile so future invocations pick them up.

### Internal

- Single `Client::request` method replaces four near-duplicate HTTP verb
  methods.
- Per-command arg structs (`*Args`, `*Sub`) now live alongside their handler
  modules; `cli/mod.rs` is a ~240-line entry point + re-exports (was 1,477).
- Unused `url` crate dependency removed.

## 0.1.0 (2026-04-22)

Initial release.

### Command groups

- **table** — CRUD on any ServiceNow table (list, get, create, update, replace, delete)
- **schema** — schema discovery (tables, columns, choices)
- **aggregate** — server-side stats (count, sum, avg, min, max, group-by)
- **change** — Change Management (normal/emergency/standard, tasks, CIs, conflicts, approvals, risk, schedule, models, templates)
- **attachment** — file upload/download (binary support)
- **cmdb** — CMDB Instance + Meta (CRUD, relations, class metadata)
- **import** — Import Set (single/bulk insert, retrieve)
- **catalog** — Service Catalog (browse, order, cart workflow, wishlist)
- **identify** — Identification & Reconciliation (CI create/update/query, enhanced variants)
- **app** — App Repository (install, publish, rollback)
- **updateset** — Update Set lifecycle (create, retrieve, preview, commit, back-out)
- **atf** — Automated Test Framework (run suites, get results)
- **scores** — Performance Analytics scorecards (list, favorite, unfavorite)
- **progress** — poll async CICD operations
- **introspect** — dump command tree as JSON

### Features

- Named profiles with config/credentials split (chmod 600)
- `--wait` flag for async CICD operations (auto-polls progress)
- Auto-pagination with `--all` (JSONL or `--array`)
- Proxy and TLS support (HTTP/HTTPS/SOCKS5, custom CA certs)
- Claude Code plugin for agent integration
