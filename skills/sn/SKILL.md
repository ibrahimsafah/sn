---
name: sn
description: Run ServiceNow operations through the `sn` CLI — read and write incidents, changes, problems, requests, CMDB CIs, catalog items, attachments, journal comments, update sets and ATF runs against the user's instance. Use whenever the user says sn, ServiceNow or SNOW, names an instance, pastes a record number (INC…, CHG…, RITM…, PRB…, TASK…), asks to look up or file or update a ticket, wants live record updates, or asks a question whose answer lives in a ServiceNow table.
allowed-tools: Bash(sn *)
---

# sn — ServiceNow from the command line

JSON on stdout, JSON errors on stderr, deterministic exit codes, and it never prompts when
stdin isn't a terminal. `sn <group> --help` is accurate and complete — read it for syntax.
This file covers only what the binary can't tell you: how the *instance* misleads you.

## The one dangerous silent failure

**A query term ServiceNow cannot parse is dropped, and you get every row in the table** —
correctly formatted, exit 0, no warning. A typo'd field name doesn't narrow your query, it
widens it.

So after a filtered read, and before any decision or write that depends on it, prove the
filter did something. `sn aggregate <table> --count` is one request and no rows:

```bash
sn aggregate incident --count                                        # 70  baseline
sn aggregate incident --count -q "assigned_to.name=Abel Tuter"       # 0   term survived
sn aggregate incident --count -q "assigned_two.name=Abel Tuter"      # 70  typo, dropped
```

**A zero needs the same proof from the other side.** "Nothing matched" and "my query broke"
look identical, so show the query *can* match — confirm the entity exists, or run the same
shape against a value you know is there.

`sn change list` gives you this for free: its last array element is `__meta`, naming the
query actually run and the terms discarded.

```bash
sn change list --type normal -q "assigned_two=x^state=-5" | jq -c '.[-1].__meta'
# {"encodedQuery":"state=-5","fields":{"applied":["state"],"ignored":["assigned_two"]}}
```

**Trust the CLI's own defenses rather than working around them.** `sn variables set` refuses
unknown names before writing, `sn cmdb` stringifies numeric attributes for you, `sn ping`
/ `sn user me` fail closed rather than naming a stranger, and a `table:number` record
reference that cannot be resolved errors instead of matching an arbitrary row. Those traps
are already shut.

## Shape traps — where the obvious jq returns nothing

Silent: `jq` prints empty and exits 0, so a wrong path reads as "no data".

| Command | The obvious path | The real path |
|---|---|---|
| `schema tables` | `.name` (always null) | **`.value`** |
| `schema columns` | `.default_value`, `.choice_field` | **`.default`**, and `type=="choice"` with options in **`.choices[]`** |
| `change *` | `.number` | **`.number.value`** — the Change API wraps *every* field as `{display_value, value}`; `state.value` is a float (`-5.0`) |
| `change nextstates` | a list of `{value,label}` | three keys: `available_states`, `state_label`, **`state_transitions`** (conditions + `transition_available`) |
| `change list` | every element is a record | the **last** is `{"__meta": …}`, so `length` is off by one |
| `cmdb get` | `.name` | **`.attributes.name`** — top level is only `attributes` + relations |
| `sn get` | `.number` | **`.record.number`** — top level is `{table, sys_id, record, variables, journal}` |
| `aggregate --group-by` | `.stats.groupby_fields` | top level becomes an **array**; `groupby_fields` is a **sibling** of `stats` |
| `aggregate --sum-fields` | `.stats.sum` | **`.stats.sum.<field>`** — sum/avg/min/max nest per field |
| any count | a number | a **string**: `"70"` |
| `watch` | every line is an event | a line with **`sn_watch`** is a gap marker |

## Exit codes, and the one that lies

`0` ok · `1` usage/config · `2` instance refused or couldn't answer · `3` network/transport ·
`4` auth. Branch on the code first, parse stdout second. `sn_error` on stderr carries
ServiceNow's own error body — read it and self-correct instead of retrying blind.

**Exit 4 does not mean "log in again."** ServiceNow answers a *row-level ACL denial* with 403,
so a perfectly good credential missing a role exits 4. Split them with one call: if `sn ping`
exits 0, the credential is fine and a role is missing — re-authenticating will never fix it,
and you should say so rather than loop. A 401, or a failing `ping`, is the credential.

`status_code` may be absent on exit 2 (a failure reported inside a 200). Test for the key,
don't default it.

## Values that don't round-trip

`--display-value` **defaults to `true`** on `table`, `change`, `aggregate` and `scores`, so you
get labels — and dates localized to the caller's timezone. A localized date fed back into
`--query` will not match. When a value will be *used* rather than shown, read it with
`--display-value false`.

## Working as an agent

- **Ask the instance before guessing a field name.** `sn schema tables --filter X` →
  `sn schema columns X --writable` → `sn schema choices X <field>`. A guessed name is how you
  land in the dropped-term case above.
- **Bound every watch** (`--max-events` / `--duration` / `--idle-timeout`), and note it
  requires `-q`; there is no bare "watch this table" form.
- **`updateset back-out` and `app rollback` deserve a human.** One flag, instance-wide,
  asynchronous, no undo of their own — say what will be reverted before firing.
- **Pipe secrets** (`--password-stdin`, `--client-secret-stdin`); argv is visible to `ps`.
- **Prefer `sn profile add` over `sn init`** — it emits JSON, never prompts off a TTY, and
  leaves `default_profile` alone.
- **`authorization_code` OAuth is not agent-safe** — `sn auth login` opens a browser and blocks
  on a human. `client_credentials` is headless. Data commands never open a browser.
- **Journal has no write verb**: add a note with
  `sn table update incident <sys_id> --field work_notes="..."`.

## Where to go next

| File | Covers |
|---|---|
| `references/shapes-queries.md` | encoded-query hazards, `^OR`/`^NQ` precedence, `INSTANCEOF`, dates, aggregates |
| `references/change.md` | Change Management: routing by type, state transitions, tasks, conflicts |
| `references/watch.md` | live AMB streams: event anatomy, gap markers, `--on-change` caveats |
| `references/cicd.md` | `app`/`updateset`/`atf`/`progress` — the async `--wait` contract |

Other surfaces are well covered by `--help`; a few notes worth having anyway: `sn api search
<term>` discovers what endpoints the instance actually publishes (use it before hand-writing
`sn raw`); `sn graphql` fails **in band** — HTTP 200 with an `errors` array, mapped to exit 2
with partial `data` still on stdout; `sn attachment download --out` stages and renames, so a
failed download never leaves a truncated file, and reports `{"path","size"}`; `sn identify
query` shows what the IRE *would* match before `create-update` writes; `sn catalog
item-variables` names what an order must carry, and the cart is server-side state that
survives a failed run.
