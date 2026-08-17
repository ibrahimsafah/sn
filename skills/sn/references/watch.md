# Watch — live record changes

Streams record changes over ServiceNow's AMB websocket. JSONL on stdout, one event per line,
flushed as it arrives. `sn watch --help` has the flags; this is what the wire does.

`-q` is required — there is no bare "watch this table" form, because a channel is defined by a
query. Always bound the stream, or it runs until interrupted.

```bash
sn watch incident -q "priority=1^active=true" --max-events 5
sn watch incident -q "sys_idISNOTEMPTY" --duration 30     # every row in the table
sn watch incident -q "sys_id=<SYS_ID>" --duration 60      # one record
```

## What an event carries

The changed fields **with their new values** — so the common case needs no API call:

```jsonc
{"table_name":"incident","sys_id":"1c74…","display_value":"INC0008001",
 "operation":"update","changes":["urgency","priority"],
 "record":{"urgency":{"display_value":"1 - High","value":"1"},
           "priority":{"display_value":"3 - Moderate","value":"3"}}}
```

**It omits every field that did not change** — an `urgency` event carries no `number`, no
`assigned_to`. When you need one of those, read the record explicitly:

```bash
sn table get incident <sys_id> --fields "number,assigned_to"
```

Do that per event you care about, not for the stream: each read is an API call, and the row
comes back current as of the read, not the event, so a record written twice quickly answers
with the later write's values.

## Gotchas

- **`changes` includes derived fields.** Writing `urgency` reports `priority` too, because
  ServiceNow recomputes it. Don't infer intent from the field list.
- **Inserts list every populated field**, so an insert's `record` is the whole new row.
- **Deletes carry `changes: []`** and no `record`.
- **`--on-change` can silently drop updates.** It filters on `changes`, and update events have
  been observed arriving with `changes: []` — in which case they're rejected and the stream
  just looks quiet. Treat it as best-effort: if missing an update is unacceptable, take every
  event and filter on `record` yourself.
- **Filters are client-side.** AMB has no server-side filter, so `--operation`/`--on-change`
  are applied locally — a rejected event doesn't spend `--max-events` or reset `--idle-timeout`.
- **No replay, no cursor.** A subscription starts at "now"; anything that changed during an
  outage is gone. A watch is a best-effort feed, not a log.

## Not every line is an event

After a genuine drop and resubscribe, one marker names the hole:

```json
{"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}
```

It deliberately carries no `operation` and no `changes`, so a `jq` predicate testing either
drops it silently — backwards, since this is the one line saying your data has a gap. Filter
events with `select(.sn_watch == null)`, and treat a marker as "re-read the table over
`downtime_ms`". It doesn't spend `--max-events` or reset the idle clock, and there's one per
gap, not per attempt.

Markers should be rare: the watcher preemptively rotates its session every 45s
(`--session-rotate`) with an overlapped, deduplicated handoff, so a routine server-side reap
opens no gap. A marker therefore means something genuinely broke. Detection of a real death can
lag one ~30s poll, so reconcile from shortly *before* the reported window.

`--idle-timeout` measures **subscribed time only** — connecting and reconnecting aren't
idleness, so a short timeout can't race a slow handshake.

## Transport

Works with basic and OAuth profiles alike. **No proxy support** — refused with exit 1 rather
than silently bypassed, since ignoring a proxy would send the session cookie outside the
sanctioned egress path. `--insecure`/`--ca-cert` do work. Ctrl-C exits 0; exit 4 if the profile
can't authenticate, 3 if the socket can't be established.
