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
sn watch table incident --query "active=true"                 # stream changes, live
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
| [`sn change`](docs/usage.md#change-management) | Change requests: normal/emergency/standard, tasks, CIs, conflicts, approvals |
| [`sn attachment`](docs/usage.md#attachments) | Upload and download files on any record |
| [`sn cmdb`](docs/usage.md#cmdb) | Configuration Items: CRUD, class schema, relationships |
| [`sn import`](docs/usage.md#import-sets) | Load staging tables for transform-based imports |
| [`sn catalog`](docs/usage.md#service-catalog) | Browse the Service Catalog and place orders |
| [`sn identify`](docs/usage.md#identification--reconciliation) | CI identification and reconciliation |
| [`sn app` / `sn updateset` / `sn atf`](docs/usage.md#cicd-operations) | CICD: install/publish apps, move update sets, run ATF suites |
| [`sn scores`](docs/usage.md#performance-analytics-scorecards) | Performance Analytics scorecards |
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

**Unreleased:** [`sn journal`](docs/usage.md#journal-comments-and-work-notes) reads a
record's comments and work notes as structured entries, and
[`sn graphql`](docs/usage.md#graphql) runs GraphQL documents with in-band errors mapped
to the exit-code contract.

**0.11.0:** the read verb is optional on `table` and `cmdb` (`sn table incident <SYS_ID>`
just works — only reads are ever inferred, never a write), `sn raw` takes request headers,
and `--profile` gets a short `-p`. Three breaking changes: `replace` is gone (use
`update`), `--display-value` defaults to `true`, and `sn introspect` emits global flags
once at the root. Details in the [changelog](CHANGELOG.md).

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
