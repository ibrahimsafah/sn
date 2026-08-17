# Change Management

`sn change --help` has the verbs. These are the things it doesn't tell you.

## `--type` routes to a different endpoint

It doesn't set a field — `normal`, `emergency` and `standard` are separate API paths. That's
why `standard` can't work without `--template`: a standard change is instantiated from a
pre-approved template. List them with `sn change templates`.

## Every field is wrapped

**The Change API returns each field as `{display_value, value}`** — unlike the Table API, which
returns scalars. This catches everyone:

```bash
sn change get <sys_id> --type normal | jq -r '.number'         # → null
sn change get <sys_id> --type normal | jq -r '.number.value'   # → CHG0030001
```

`state.value` comes back as a **float** (`-5.0`, `3.0`). Coerce before comparing.

## The last array element is not a record

`sn change list` appends a `__meta` entry, so `length` is one more than the number of changes
and a naive map hits a non-record:

```bash
sn change list --type normal --setlimit 3 | jq 'length'     # 4 — three changes plus __meta
```

Drop it with `jq '[.[] | select(has("__meta") | not)]'`.

But read it first, because it reports the query the instance *actually ran* — the dropped-term
check, free:

```bash
sn change list --type normal -q "assigned_two=x^state=-5" | jq -c '.[-1].__meta'
# {"encodedQuery":"state=-5","fields":{"applied":["state"],"ignored":["assigned_two"]}}
```

## `sn change list` cannot sort

`ORDERBY`/`ORDERBYDESC` are discarded — ascending and descending return identical rows, and
`__meta` shows why (`encodedQuery: ""`, `ignored: [""]`). When order matters, go through the
Table API, which honors it:

```bash
sn table list change_request -q "ORDERBYDESCopened_at" --fields "number,state,short_description" --setlimit 5
```

You lose the `{display_value, value}` wrapping that way, which is a simplification.

## `nextstates` before any state write

ServiceNow's change state model is enforced by workflow, so an invalid transition fails in ways
that read like a permission problem. The response has **three** top-level keys:

```json
{"available_states": ["3"],
 "state_label": {"3": "Closed"},
 "state_transitions": [[{"sys_id": "…", "transition_available": "true", "conditions": […]}]]}
```

```bash
sn change nextstates <sys_id> | jq -r '.available_states[] as $s | "\($s)\t\(.state_label[$s])"'
```

`state_transitions` is what to read when a transition is *refused* — it carries the conditions
and a `transition_available` verdict per transition, which is the difference between "you can't"
and knowing why.

## Two more

- **`change task list` defaults to `--setlimit 100`**, not 1000 like its siblings. Pass it
  explicitly if a change might have more.
- **`change conflict remove` clears every recorded conflict at once** — it is not a targeted
  delete, and nothing restores them. Read `conflict get` first and say what's about to go.
