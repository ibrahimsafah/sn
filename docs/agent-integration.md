# Agent integration

`sn` is built to be driven by an LLM coding agent as well as by a human. The properties that
make that safe are the same ones that make it good in a shell script:

- **Stable JSON on stdout, structured JSON errors on stderr** — an agent reads failures as
  data, not prose.
- **Deterministic [exit codes](usage.md#exit-codes)** — branch on the code before parsing
  anything: `2` API error, `3` network, `4` auth.
- **No interactive surprises** — nothing prompts unless you opted in (`sn init`); with a
  non-terminal stdin, commands that would prompt fail naming the missing flag instead of
  hanging the pipeline.
- **Discovery built in** — `sn schema` tells an agent what a table looks like before it
  writes; `sn introspect` tells it what the CLI itself looks like.

For the full agent-facing playbook — discovery flow, encoded-query syntax, common mistakes —
see the [agent usage guide](agent-guide.md), which is written to be dropped into an agent's
context.

## Claude Code plugin

The repo ships as a Claude Code plugin (plugin name `sn`, in `.claude-plugin/`) that
pre-approves `Bash(sn *)` so Claude runs `sn` commands without per-call prompts. This repo is
its own marketplace:

```bash
claude plugin marketplace add tehubersheezy/servicenow-cli   # or a local clone path
claude plugin install sn
```

In a clone of this repo, the skill at `.claude/skills/sn.md` is picked up automatically —
invoke with `/sn`.

## `sn introspect`: the machine-readable command tree

`sn introspect` dumps the full command tree as JSON — for auto-generating MCP tool
definitions or function-call schemas:

```bash
sn introspect | jq '.subcommands[] | {name, about}'

# Flags that cannot be combined, across the whole tree:
sn introspect | jq '[.. | objects | select(.conflicts_with? // [] | length > 0)
                     | {name, conflicts_with}] | unique'
```

Each `args[]` entry carries `name`, `long`, `short`, `help`, `help_heading`, `required`, `takes_value`, `value_name`, `positional`, `repeatable`, `aliases`, `default_values`, `possible_values`, and `conflicts_with`. `--help` and `--version` are omitted — they exit before any handler runs — and nothing named `help` appears in the tree.

The root carries two extra keys: `version` (the binary that produced the tree) and `global_args` (the 11 flags clap propagates to every command). **A command's effective flags are its own `args` plus the root's `global_args`.** They live at the root because serializing them onto all 130 nodes was three quarters of the output:

```bash
# Everything `table list` accepts:
sn introspect | jq '[.global_args[], (.subcommands[] | select(.name=="table")
                     | .subcommands[] | select(.name=="list") | .args[])] | map(.name)'
```

One relation is missing and cannot be added: clap keeps `requires` private, so `--wait-timeout` requiring `--wait` shows up only as prose in that flag's `help`.
