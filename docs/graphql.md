# GraphQL additions — design notes

Proposed `sn` features built on ServiceNow's GraphQL endpoint. Everything in
this document was verified live against a Zurich PDI (dev421992, 2026-08-11)
— including the ACL behavior, which was tested as both admin and a
freshly-minted `itil` user.

## Background: the endpoint

One endpoint serves everything: `POST /api/now/graphql` (basic auth works —
same credentials as the Table API). The merged schema contains two kinds of
surface:

- **Scripted namespaces** (`now`, `global`, `sn*` — ~28 of them): hand-written
  APIs for chat, Playbook, Flow Designer tooling. Nothing generally useful to
  this CLI.
- **Generated namespaces**: `GlideRecord_Query`, `GlideRecord_Mutation`,
  `GlideAggregateRecord_Query` — auto-derived from the data dictionary.
  Every table (~6,200 on a stock PDI) gets a query field, insert/update/delete
  mutations, and aggregates. This is the surface worth wrapping.

Full introspection of the merged schema is ~94 MB and takes ~2 minutes —
never introspect at runtime. Also note graphql-java's "good faith
introspection" guard: a query that mentions `__Type.fields` (or similar meta
fields) more than once is rejected with `BadFaithIntrospection`.

## What GraphQL does that the Table API can't

These are the capability gaps that justify the work; each maps to a proposed
feature below.

1. **Many tables / many queries in one request** — aliases allow the same
   table under different conditions; reads and aggregates mix in one document.
2. **Total count with the page** — `_rowCount` returns the full match count
   alongside `pagination: {limit: N}` results. The Table API has no total; the
   workaround is a second Aggregate API call.
3. **Field metadata inline, ACL-evaluated for the caller** — every column is
   a typed wrapper carrying `label`, `internalType`, `isMandatory`, and live
   `canRead`/`canWrite`/`canCreate` verdicts, per field, per record, per user.
4. **Choice lists in context** — choice columns expose
   `_choices { value displayValue }`, evaluated against the record (dependent
   choices resolve correctly). `sys_choice` gives you the static dump.
5. **Table-level capability discovery** — `_table_metadata { label plural
   canRead canWrite canCreate canDelete auditWanted }`, again evaluated for
   the calling user. "Can I create incidents?" without trying and catching 403.
6. **Structured dot-walking** — reference columns expose `_reference`, which
   opens the full results type of the target table: any fields, their
   metadata, further references. REST's `sysparm_fields=caller_id.name` gives
   flat strings with no nesting control.
7. **Per-field display values** — select `value`, `displayValue`, or both per
   column. `sysparm_display_value` is all-or-nothing per request.
8. **Journal access that survives itil ACLs** — see `sn journal` below.

## Journal fields: the ACL asymmetry (verified)

Comments and work notes live in `sys_journal_field` (one row per entry). The
access model is inverted from what you'd expect:

| Route | admin | itil |
|---|---|---|
| `sys_journal_field` via GraphQL or Table API | full rows | **blocked** — `_rowCount` leaks the count but `_results` comes back empty (row ACL filtering) |
| `GlideAggregateRecord_Query` on `sys_journal_field` | works | **denied** ("Insufficient rights to query records") |
| `comments` / `work_notes` / `comments_and_work_notes` columns on the record | full stream | **full stream** |

The record columns return the *rendered* stream — all entries concatenated,
each prefixed `YYYY-MM-DD HH:MM:SS - Author Name (Comments|Work notes)`,
entries separated by blank lines. Two caveats: timestamps are rendered in the
calling user's timezone (the raw table stores UTC), and it's a display string,
not rows — parse it (header regex:
`^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} - (.+) \((Comments|Work notes)\)$`).

## Proposed additions

### 1. `sn graphql` — first-class query runner

`src/cli/graphql.rs`, modeled on `raw.rs`. Query from positional arg, `@file`,
or stdin; `--var key=value` for GraphQL variables; unwraps `data` to stdout.

Why `raw` isn't enough: GraphQL returns **HTTP 200 with an `errors` array**
(sometimes alongside partial `data`), which silently defeats the
"branch on exit code first" contract. The dedicated command maps a non-empty
`errors` array to the stderr JSON error shape and exit code 2, and decides a
policy for partial results (propose: `data` to stdout, `errors` to stderr,
exit 2 — callers that want partials can still read stdout).

Request body shape: `{"query": "...", "variables": {...}}`.

### 2. `sn journal <table> <sys_id>` — structured comments/work notes

The itil-safe route: read the record's `comments_and_work_notes` displayValue
via GraphQL, parse the rendered stream into JSON entries
(`[{created_on, author, type, text}]`). Flags:

- `--comments` / `--work-notes` — filter by type (also narrows the fetched
  column, which matters: `comments` and `work_notes` are separate columns).
- `--raw` — the unparsed stream string.
- `--source record|table` (default `record`): `table` queries
  `sys_journal_field` directly for true per-entry rows with UTC timestamps and
  usernames — for profiles whose ACLs allow it. No `auto` mode: predictable
  request counts beat cleverness for an agent-facing CLI, and the ACL-filtered
  case errors with a pointer to `--source record`.

Writes need no new surface — `sn table update <t> <id> --field work_notes=…`
already lands a journal entry via REST.

### 3. `sn table list --with-total`

Route the read through GraphQL to return `_rowCount` (total matches) alongside
the page. Output shape must remain an array by default, so propose the total
goes to a wrapper only under the flag: `{"total": N, "results": [...]}` —
flag-gated, so no existing consumer breaks.

### 4. Multi-get batching

`sn table get incident <id> <id> <id>` (today `get` takes exactly one sys_id)
compiled into one GraphQL document via aliases:

```graphql
query { GlideRecord_Query {
  a: incident(sys_id: "…") { _results { … } }
  b: incident(sys_id: "…") { _results { … } }
} }
```

N round trips → 1. Table and column names are `[a-z0-9_]+`; validate before
splicing into query text — they cross a syntax boundary.

### 5. ACL-aware schema commands

- `sn schema table-meta <table>` → `_table_metadata` verdicts for the calling
  profile. Agent pre-flight: know `canCreate` before attempting the insert.
- `sn schema columns <table> --live` / `sn schema choices <table> <col> --live`
  → field-wrapper metadata (`label`, `internalType`, `isMandatory`, `canWrite`,
  `_choices`) read off one sampled record (`pagination: {limit: 1}`).
  Per-user, record-context answers instead of the static `sys_dictionary` /
  `sys_choice` dump. **Caveat**: field-level metadata hangs off `_results`
  rows — an empty or fully ACL-filtered table yields nothing; fall back to the
  REST path and say so on stderr.

### 6. `--expand` on `table get`/`list`

`--expand caller_id=name,email,manager.name` → nested `_reference` selection,
emitting real nested JSON objects instead of REST's flattened dot-walk strings.

## Non-goals

- **Writes over GraphQL.** Verified gotcha: a *successful* `update_incident`
  can return `_rowCount: null, _results: null` — you cannot confirm a write
  from the mutation response. REST PATCH echoes the updated record reliably.
  Principle: reads go GraphQL where it wins; writes stay REST.
- **GraphQL subscriptions.** Every generated table type has a `_subscription`
  field, but `sn watch` already rides the same AMB channel natively with
  reconnect/backoff/hydration policy. The GraphQL wrapper adds nothing.
- **CICD / Performance Analytics.** No GraphQL surface exists — they are
  procedural APIs, not tables. (Their *state* tables — `sys_update_set`,
  `sys_atf_test_result`, `pa_scores` — are readable like any other table.)

## Reference: request/response shapes

Get one record (the `sn graphql` runner should make this trivial to emit):

```json
{
  "query": "query ($id: String!) { GlideRecord_Query { incident(sys_id: $id) { _results { number { value } short_description { value } state { value displayValue } assigned_to { value displayValue _reference { email { value } } } } } } }",
  "variables": { "id": "47a91e3c2f8acf107efd1d707fa4e387" }
}
```

List with conditions + total:

```graphql
query { GlideRecord_Query {
  incident(queryConditions: "active=true^ORDERBYDESCsys_updated_on",
           pagination: { limit: 5 }) {
    _rowCount
    _results { number { value } state { displayValue } }
} } }
```

`queryConditions` is the same encoded-query syntax as `sysparm_query`,
`ORDERBY`/`ORDERBYDESC` included. Aggregate:

```graphql
query { GlideAggregateRecord_Query(
    tableName: "incident", queryConditions: "active=true",
    groupBy: ["state"]) {
  totalCount
  aggregates { groupBy { field value displayValue } count }
} }
```

(Also available per group: `avg` / `min` / `max` / `sum` / `countDistinct`;
root args include `having`, `orderBy`, `groupPagination`.)

Schema discovery on a live record:

```graphql
query { GlideRecord_Query { incident(pagination: { limit: 1 }) {
  _table_metadata { label plural canCreate canWrite canDelete }
  _results {
    state { label internalType isMandatory canWrite
            _choices { value displayValue } }
    assigned_to { label referenceTableName }
} } } }
```
