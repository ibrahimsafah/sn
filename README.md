# sn

[![CI](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/ci.yml)
[![Security](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/security.yml/badge.svg)](https://github.com/tehubersheezy/servicenow-cli/actions/workflows/security.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/tehubersheezy/servicenow-cli/badge)](https://scorecard.dev/viewer/?uri=github.com/tehubersheezy/servicenow-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Latest release](https://img.shields.io/github/v/release/tehubersheezy/servicenow-cli?display_name=tag&sort=semver)](https://github.com/tehubersheezy/servicenow-cli/releases/latest)

**`sn` is a command-line tool for ServiceNow.** It talks to your instance over its REST
APIs, so anything you'd normally do by clicking through the web UI — looking up incidents,
creating change requests, uploading attachments, moving update sets, running ATF tests —
you can do from the terminal, script in a pipeline, or hand to an AI coding agent.

It's a single fast binary with nothing to install on the instance, and every command
follows one contract: clean JSON on stdout, structured errors on stderr, exit codes that
mean something, and no prompts unless you asked for them.

## Quickstart

Install (macOS/Linux — see [the setup guide](docs/setup.md#installation) for Windows,
shell installers, and pre-built binaries):

```bash
brew install tehubersheezy/sn/sn
```

Connect to an instance — `sn init` asks for the instance URL and credentials, verifies
them, and saves a profile:

```bash
sn init
sn ping        # confirm it works: auth, latency, instance version
```

Then start reading and writing:

```bash
sn table list incident --query "active=true" --setlimit 5     # five open incidents
sn table incident <sys_id>                                    # one record
sn schema columns incident --writable                         # what can I set?
sn table create incident -F short_description="Disk full on prod-db-01"
sn watch incident --query "active=true"                       # stream changes, live
```

Output is JSON, so everything pipes into `jq`; add `--output table` for a human-readable
columnar view while exploring.

## What it can do

Each group is a family of subcommands — the [usage guide](docs/usage.md) walks through all
of them with examples.

| Commands | What they cover |
|---|---|
| [`sn table`](docs/usage.md#reading-records) | Read, create, update, and delete records in any table |
| [`sn watch`](docs/usage.md#watching-records-live) | Stream record changes live over the AMB websocket |
| [`sn schema`](docs/usage.md#schema-discovery) | Discover tables, columns, and choice values on an unfamiliar instance |
| [`sn journal`](docs/usage.md#journal-comments-and-work-notes) | Comments and work notes, parsed into structured entries |
| [`sn aggregate`](docs/usage.md#aggregate-queries) | Server-side counts, sums, and averages — no record fetching |
| [`sn graphql`](docs/usage.md#graphql) | Run GraphQL documents, with in-band errors mapped to real exit codes |
| [`sn gr`](docs/usage.md#dot-walked-reads-sn-gr) | Read records with dot-walked reference fields, one round trip, no GraphQL to write |
| [`sn change`](docs/usage.md#change-management) | Change requests: normal/emergency/standard, tasks, CIs, conflicts, approvals |
| [`sn attachment`](docs/usage.md#attachments) | Upload and download files on any record |
| [`sn cmdb`](docs/usage.md#cmdb) | Configuration Items: CRUD, class schema, relationships |
| [`sn import`](docs/usage.md#import-sets) | Load staging tables for transform-based imports |
| [`sn catalog`](docs/usage.md#service-catalog) | Browse the Service Catalog and place orders |
| [`sn variables`](docs/agent-guide.md#catalog-variables-variables) | Catalog variables on a record: read them, write them with verification |
| [`sn identify`](docs/usage.md#identification--reconciliation) | CI identification and reconciliation |
| [`sn app` / `sn updateset` / `sn atf`](docs/usage.md#cicd-operations) | CICD: install/publish apps, move update sets, run ATF suites |
| [`sn scores`](docs/usage.md#performance-analytics-scorecards) | Performance Analytics scorecards |
| [`sn api`](docs/usage.md#api-discovery) | Ask the instance which REST APIs it publishes, down to the OpenAPI spec |
| [`sn raw`](docs/usage.md#raw-rest-passthrough) | Passthrough for any REST endpoint not modeled above |
| [`sn ping` / `sn open` / …](docs/usage.md#inspect-and-connect) | Health checks, opening records in the browser, shell completions |

Scripting against it is predictable by design: exit codes are deterministic (`0` success,
`1` usage/config, `2` API error, `3` network, `4` auth), errors are structured JSON on
stderr, and `--all` streams paginated results as JSONL. The full rules live in the
[output contract](docs/usage.md#output-contract).

## Built for AI agents

The same contract that makes `sn` scriptable makes it safe to hand to a coding agent:
Claude (or any LLM) can discover schema before writing, verify a change actually landed,
stream record changes while a script runs, and read failures as data instead of prose.

- The repo ships as a **Claude Code plugin** with the CLI pre-approved — see
  [agent integration](docs/agent-integration.md).
- `sn introspect` emits the whole command tree as JSON, for generating MCP tool
  definitions or function-call schemas.
- The [agent usage guide](docs/agent-guide.md) is a self-contained playbook written to be
  dropped into an agent's context.

## What's new

**0.13.0:** records get addresses. Every `(table, sys_id)` pair now also takes one
`table:identifier` token — `sn table get incident:INC0010001` — and `sn get <REF>` reads
a record with its catalog variables and parsed journal in one command (a bare
`sn get INC0010001` works for the standard prefixes). `sn context` shows — and switches —
the application scope and update set the session's tracked writes are captured under.
`sn watch` now rotates its AMB session before the instance can reap it, so a routine reap
opens no gap. Three breaking changes: `sn auth` is merged into `sn profile`
(`sn profile login/logout/status/refresh`), `sn watch` targets with `-q` alone
(`--sys-id`, `--hydrate`, `--fields`, `--display-value` are gone), and building from
source needs Rust 1.88. Details in the [changelog](CHANGELOG.md).

**0.12.0:** four new command groups. [`sn api`](docs/usage.md#api-discovery) asks the
instance which REST APIs it actually publishes — every namespace, every endpoint with its
method and route, and any API's OpenAPI 3 document — so finding an endpoint no longer
means opening a browser; [`sn variables`](docs/agent-guide.md#catalog-variables-variables)
reads and writes catalog variables on a record, re-reading after every write because that
endpoint answers `200` for names it silently skipped;
[`sn journal`](docs/usage.md#journal-comments-and-work-notes) parses comments and work
notes into structured entries; and [`sn graphql`](docs/usage.md#graphql) runs GraphQL
documents with in-band errors mapped to the exit-code contract.
[`sn cmdb create`/`update`](docs/usage.md#cmdb) now send the envelope the CMDB Instance
API requires, so `--field` on a CI works at all for the first time (a CLI write stamps
`discovery_source`, `"Manual Entry"` unless you pass `--source`), and `sn ping` reports
the identity the instance asserts rather than whichever `sys_user` row sorted first —
which also means `sn ping`'s `username` can change if your config names someone the
instance disagrees with. Three changes can break a script: that `username` semantic;
`--all` no longer combines with `--output raw` or `--output table` (exit 1 — for table,
buffer with [`--array`](docs/usage.md#pagination); for raw, drop `--all` and page with
`--offset`/`--setlimit`); and six destructive commands — `profile remove`,
`catalog cart-empty`, `catalog cart-remove`, `change conflict remove`,
`updateset back-out`, `app rollback` — refuse to run without `--yes` when stdin is not a
terminal. Details in the [changelog](CHANGELOG.md).

## Documentation

| Doc | Contents |
|---|---|
| [Setup guide](docs/setup.md) | Installation, `sn init`, profiles, OAuth/SSO, config files, proxy and TLS |
| [Usage guide](docs/usage.md) | Every command group with examples, the output contract, exit codes, parameters, debugging |
| [Agent integration](docs/agent-integration.md) | The Claude Code plugin and `sn introspect` |
| [Agent usage guide](docs/agent-guide.md) | The full playbook for LLM agents driving the CLI |
| [Changelog](CHANGELOG.md) | Release history |

## License

MIT
