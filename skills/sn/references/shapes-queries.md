# Queries and aggregate shapes

`sn <group> --help` has the syntax. This covers the parts that fail quietly.

## Precedence: `^OR` and `^NQ` are not symmetric

This produces wrong results rather than errors:

- A trailing `^` term after an `^OR` group applies to the **whole group**.
- A trailing `^` term after `^NQ` applies **only to the last segment**.

So `active=true^ORstate=2^priority=1` filters everything by `priority=1`, while
`active=true^NQstate=2^priority=1` leaves the first segment unfiltered. If you use `^NQ` for
keyset iteration, that unfiltered segment can return the same rows forever. Prefer `^OR`, and
verify with a count.

## Dot-walking is the usual reason a term vanishes

```bash
sn table list incident --query "caller_id.name=Abel Tuter"
sn table list incident --query "cmdb_ci.location.city=Cary"
```

The **first** segment must be a real reference field on this table, or the whole term is
dropped and you get every row. Confirm before trusting it:

```bash
sn schema columns incident | jq -r '.[] | select(.type=="reference") | .name'
```

The *read* side of dot-walking is `sn gr` — `sn gr incident -f number,caller_id.manager.email`
fetches dot-walked values in one round trip. The same rule governs its paths: every
non-terminal segment must be a reference, though there a wrong segment errors instead of
widening the result.

## Filtering a base table by class

```bash
sn table list cmdb_ci --query "sys_class_nameINSTANCEOFcmdb_ci_server"
```

Filtering a base table by a *child's* field silently drops the term — the column doesn't exist
on the base table. Query the child table directly instead.

## Dates

Server-side date functions run as `javascript:` terms:

```bash
sn table list incident --query "sys_created_on>javascript:gs.daysAgoStart(7)"
sn table list incident --query "opened_atONToday@javascript:gs.beginningOfToday()@javascript:gs.endOfToday()"
```

An instance that can't evaluate one drops it like any other term. And remember a *displayed*
date won't round-trip back into a query — read with `--display-value false` when the value is
going into a filter.

## Aggregate: the cheapest question you can ask

One request, no rows, no pagination — which is what makes it the right tool for verifying a
filter as well as for reporting.

```bash
sn aggregate incident --count
sn aggregate incident --count --query "active=true"
sn aggregate incident --count --group-by state
sn aggregate incident --sum-fields reassignment_count --min-fields priority
```

Two shapes that make the obvious `jq` return nothing:

- **`--group-by` flips the top level to an array**, and `groupby_fields` is a **sibling** of
  `stats`, not inside it:
  ```bash
  sn aggregate incident --count --group-by assignment_group -q "active=true" \
    | jq -r '.[] | "\(.groupby_fields[0].value // "(none)")\t\(.stats.count)"'
  ```
  The empty-label bucket is real — records with no value for the grouped field. Surface it
  rather than dropping it; it is often the largest group.
- **`sum`/`avg`/`min`/`max` nest per field**: `.stats.sum.reassignment_count`, not `.stats.sum`.

**Counts come back as JSON strings** (`"70"`). Coerce with `jq 'tonumber'` before comparing.

## Pagination

For "how many are there", don't paginate at all — `sn aggregate <table> --count` answers in one
request. The `--all`/`--array`/`--output` interactions are self-naming exit-1 errors.
