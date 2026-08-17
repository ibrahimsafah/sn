# CICD — the async contract

`app`, `updateset` and `atf` operations run in the background on the instance. `--help` has the
verbs; this is the part that breaks scripts.

## Branch on the exit code, never on `status_label`

`--wait` polls every 2s and exits **0 only when the operation actually succeeded**:

| Outcome | Exit | stdout |
|---|---|---|
| Succeeded | `0` | the final progress result |
| Operation failed | `2` | **empty** — progress object is on stderr under `.error.sn_error` |
| `--wait-timeout` expired | `3` | **empty** — pointer to `sn progress <id>` |

So reading stdout on a failure branch gets you nothing. A failed operation carries **no
`status_code`**: the HTTP call succeeded, the operation didn't.

`status_label` is ServiceNow's verbatim string and varies by instance — "Successful",
"Complete", "Succeeded". Matching on it is how you write a poll loop that never terminates.

## Polling manually

Key off the numeric `status`, which is a **string** holding a digit:

| `status` | Meaning |
|---|---|
| `"0"` | pending |
| `"1"` | running |
| `"2"` | successful |
| `"3"` | failed |
| `"4"` | cancelled |

Alongside it: `status_message`, `status_detail`, `percent_complete` (snake_case). **The progress
id lives at `links.progress.id`** — there is no top-level `progress_id` in the response, despite
that being the CLI's argument name.

```bash
id=$(sn app install --scope x_myapp --version 1.2.0 | jq -r '.links.progress.id')
sn progress "$id"
```

Prefer `--wait` with a `--wait-timeout` when you can: one command, and the timeout bounds a
stall instead of hanging. `--wait` honors `--output raw`.

## The two that deserve a human

Both require `--yes`, and unlike a row delete they warrant confirming intent first:

- **`updateset back-out` reverts every record its update set applied** — not the set, the
  records.
- **`app rollback` replaces an installed app** without rolling back what the newer version
  wrote to data.

One flag, instance-wide, asynchronous, no second confirmation downstream. An unintended
back-out is a recovery project.
