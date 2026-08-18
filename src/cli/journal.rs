//! `sn journal` — comments and work notes for one record, parsed into entries.
//!
//! Journal entries live in `sys_journal_field` (one row per entry), but that
//! table is ACL-locked for non-admin roles: a query as `itil` returns the row
//! *count* and zero rows. What every role that can read the record *can* read
//! is the record's own journal columns (`comments`, `work_notes`,
//! `comments_and_work_notes`), whose displayValue is the rendered stream —
//! every entry, prefixed `<timestamp> - <author> (<field label>)`, entries
//! separated by a blank line. Established live against a Zurich PDI as both
//! admin and a bare `itil` user; see docs/graphql.md.
//!
//! So the default source is `record`: fetch the rendered stream over GraphQL
//! and parse it back into structured entries. `--source table` opts into the
//! `sys_journal_field` rows instead — true per-entry rows with UTC timestamps
//! and usernames — for profiles that can read them; when rows exist but ACLs
//! filter them all, the error says so and points back at `--source record`.
//!
//! Parsing caveats, both inherent to the rendered stream: timestamps are in
//! the calling user's timezone and date format (the raw table stores UTC), and
//! a note whose *text* contains a line shaped exactly like an entry header
//! will split the entry there.

use crate::cli::GlobalFlags;
use crate::cli::graphql::{errors_to_api_error, execute, graphql_errors};
use crate::cli::kernel::{connect, write_response};
use crate::cli::record_ref;
use crate::client::Client;
use crate::error::{Error, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(clap::Args, Debug)]
pub struct JournalArgs {
    /// Table the record lives in (e.g. `incident`), or a combined
    /// `table:sys_id` / `table:number` reference (e.g. `incident:INC0010001`).
    pub table: String,
    /// sys_id of the record. Omit when TABLE is a `table:id` reference.
    pub sys_id: Option<String>,
    /// Only customer-visible comments.
    #[arg(long, conflicts_with = "work_notes")]
    pub comments: bool,
    /// Only work notes.
    #[arg(long = "work-notes", conflicts_with = "comments")]
    pub work_notes: bool,
    /// Where to read from. `record` parses the record's rendered journal
    /// columns (works for any role that can read the record; timestamps in the
    /// caller's timezone). `table` reads sys_journal_field rows (UTC
    /// timestamps, usernames — but ACL-locked to admin-ish roles by default).
    #[arg(long, value_enum, default_value_t = JournalSource::Record)]
    pub source: JournalSource,
    /// Keep only the newest N entries. With --source table the cap is applied
    /// server-side; otherwise after parsing.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Emit the unparsed rendered stream as a JSON string (record source only).
    #[arg(long, conflicts_with_all = ["limit", "source"])]
    pub raw: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq, Default)]
#[value(rename_all = "lowercase")]
pub enum JournalSource {
    #[default]
    Record,
    Table,
}

/// One parsed journal entry, newest first in the output array. `element` is
/// the journal field's column name; from the record source it is derived from
/// the rendered label (and omitted when the label matches neither journal
/// field), from the table source it is exact and `label` is absent.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Entry {
    created_on: String,
    author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    text: String,
}

pub fn run(global: &GlobalFlags, args: JournalArgs) -> Result<()> {
    let r = record_ref::parse_pair(&args.table, args.sys_id.as_deref(), "table")?;

    let client = connect(global)?;
    let sys_id = r.resolve(&client)?;

    let filter = if args.comments {
        Some("comments")
    } else if args.work_notes {
        Some("work_notes")
    } else {
        None
    };

    let out = match args.source {
        JournalSource::Record => {
            let streams = fetch_record_streams(&client, &r.table, &sys_id, filter)?;
            if args.raw {
                let combined: Vec<String> = streams.into_iter().map(|(_, s)| s).collect();
                Value::String(combined.join(""))
            } else {
                let mut entries = Vec::new();
                for (_, stream) in &streams {
                    entries.extend(parse_stream(stream)?);
                }
                sort_newest_first(&mut entries);
                if let Some(n) = args.limit {
                    entries.truncate(n);
                }
                serde_json::to_value(entries).expect("entries serialize")
            }
        }
        JournalSource::Table => {
            let entries = fetch_table_rows(&client, &sys_id, filter, args.limit)?;
            serde_json::to_value(entries).expect("entries serialize")
        }
    };
    write_response(global, &out)
}

/// Fetch the rendered stream(s) off the record. Returns `(column, stream)`
/// pairs (empty stream when the column has no entries).
///
/// Which columns exist varies by table: the task family has all three journal
/// columns, other tables may have one. A missing column fails the *whole*
/// GraphQL document with a FieldUndefined validation error naming it, so this
/// walks a fallback chain: `comments_and_work_notes` → both single columns in
/// one query → whichever single column the error did not name.
fn fetch_record_streams(
    client: &Client,
    table: &str,
    sys_id: &str,
    filter: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let attempts: &[&[&str]] = match filter {
        Some("comments") => &[&["comments"]],
        Some("work_notes") => &[&["work_notes"]],
        _ => &[
            &["comments_and_work_notes"],
            &["comments", "work_notes"],
            &["comments"],
            &["work_notes"],
        ],
        // Filtered fetches have no fallback on purpose: asking for a column
        // the table does not have should say so, not silently switch fields.
    };

    let mut last_missing: Option<String> = None;
    'attempt: for cols in attempts {
        // Skip an attempt that re-requests a column already known missing.
        if let Some(missing) = &last_missing
            && cols.contains(&missing.as_str())
        {
            continue;
        }
        let selection: Vec<String> = cols
            .iter()
            .map(|c| format!("{c} {{ displayValue }}"))
            .collect();
        let query = format!(
            "query ($id: String!) {{ GlideRecord_Query {{ {table}(sys_id: $id) \
             {{ _results {{ {} }} }} }} }}",
            selection.join(" ")
        );
        let resp = execute(client, &query, Some(json!({ "id": sys_id })), None)?;
        let errors = graphql_errors(&resp);
        if !errors.is_empty() {
            if undefined_field(&errors, table) {
                return Err(Error::Api {
                    status: 200,
                    message: format!("table '{table}' is not queryable via GraphQL"),
                    detail: None,
                    transaction_id: None,
                    sn_error: Some(Value::Array(errors)),
                });
            }
            for col in *cols {
                if undefined_field(&errors, col) {
                    last_missing = Some(col.to_string());
                    continue 'attempt;
                }
            }
            return Err(errors_to_api_error(errors));
        }
        let results = resp
            .pointer(&format!("/data/GlideRecord_Query/{table}/_results"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some(record) = results.first() else {
            return Err(Error::Api {
                status: 404,
                message: format!("no {table} record with sys_id {sys_id}"),
                detail: None,
                transaction_id: None,
                sn_error: None,
            });
        };
        return Ok(cols
            .iter()
            .map(|c| {
                let stream = record
                    .pointer(&format!("/{c}/displayValue"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                (c.to_string(), stream)
            })
            .collect());
    }
    Err(Error::Api {
        status: 200,
        message: match filter {
            Some(col) => format!("table '{table}' has no {col} field"),
            None => format!("table '{table}' has no journal fields (comments/work_notes)"),
        },
        detail: None,
        transaction_id: None,
        sn_error: None,
    })
}

/// Read `sys_journal_field` rows directly (newest first). Exact entries with
/// UTC timestamps and usernames — when the profile's ACLs allow it.
fn fetch_table_rows(
    client: &Client,
    sys_id: &str,
    filter: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<Entry>> {
    let mut qc = format!("element_id={sys_id}");
    if let Some(col) = filter {
        qc.push_str(&format!("^element={col}"));
    }
    qc.push_str("^ORDERBYDESCsys_created_on");

    let query = "query ($qc: String!, $limit: Int) { GlideRecord_Query { \
                 sys_journal_field(queryConditions: $qc, pagination: { limit: $limit }) \
                 { _rowCount _results { element { value } value { value } \
                 sys_created_on { value } sys_created_by { value } } } } }";
    let vars = json!({ "qc": qc, "limit": limit.unwrap_or(1000) });
    let resp = execute(client, query, Some(vars), None)?;
    let errors = graphql_errors(&resp);
    if !errors.is_empty() {
        return Err(errors_to_api_error(errors));
    }

    let node = resp
        .pointer("/data/GlideRecord_Query/sys_journal_field")
        .cloned()
        .unwrap_or(Value::Null);
    let row_count = node.get("_rowCount").and_then(Value::as_u64).unwrap_or(0);
    let results = node
        .get("_results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // The ACL signature established live: the count leaks through row-level
    // ACLs, the rows do not. Say what happened instead of returning [].
    if row_count > 0 && results.is_empty() {
        return Err(Error::Api {
            status: 200,
            message: format!(
                "{row_count} journal entries exist but were removed by ACLs \
                 (sys_journal_field is admin-readable by default); try --source record"
            ),
            detail: None,
            transaction_id: None,
            sn_error: None,
        });
    }

    let field = |row: &Value, name: &str| {
        row.pointer(&format!("/{name}/value"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Ok(results
        .iter()
        .map(|row| Entry {
            created_on: field(row, "sys_created_on"),
            author: field(row, "sys_created_by"),
            element: Some(field(row, "element")),
            label: None,
            text: field(row, "value"),
        })
        .collect())
}

/// Whether the errors contain a FieldUndefined validation error naming `name`.
pub(crate) fn undefined_field(errors: &[Value], name: &str) -> bool {
    let needle = format!("Field '{name}'");
    errors.iter().any(|e| {
        e.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("FieldUndefined") && m.contains(&needle))
    })
}

/// Parse a rendered journal stream into entries, in stream order (newest
/// first, as ServiceNow renders it).
///
/// An entry header is `<timestamp> - <author> (<label>)` on its own line; the
/// entry's text is every following line until the next header, with trailing
/// blank lines dropped (the renderer separates entries with one). The
/// timestamp is matched structurally — starts with a digit, contains `:` —
/// not by format, because it is rendered in the caller's date format.
pub(crate) fn parse_stream(stream: &str) -> Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut current: Option<(String, String, String, Vec<String>)> = None;

    for line in stream.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((created_on, author, label)) = parse_header(line) {
            if let Some(entry) = current.take() {
                entries.push(finish_entry(entry));
            }
            current = Some((created_on, author, label, Vec::new()));
        } else if let Some((_, _, _, text)) = current.as_mut() {
            text.push(line.to_string());
        } else if !line.trim().is_empty() {
            return Err(Error::Api {
                status: 200,
                message: "unrecognized journal stream format; re-run with --raw to inspect it"
                    .into(),
                detail: Some(format!("first unparsed line: {line}")),
                transaction_id: None,
                sn_error: None,
            });
        }
    }
    if let Some(entry) = current.take() {
        entries.push(finish_entry(entry));
    }
    Ok(entries)
}

fn finish_entry(
    (created_on, author, label, mut text): (String, String, String, Vec<String>),
) -> Entry {
    while text.last().is_some_and(|l| l.trim().is_empty()) {
        text.pop();
    }
    Entry {
        created_on,
        author,
        element: element_for_label(&label),
        label: Some(label),
        text: text.join("\n"),
    }
}

/// `Some((timestamp, author, label))` when the line is an entry header.
fn parse_header(line: &str) -> Option<(String, String, String)> {
    let line = line.trim_end();
    let rest = line.strip_suffix(')')?;
    let (timestamp, rest) = rest.split_once(" - ")?;
    // The timestamp is locale-formatted, so require shape, not format: it
    // starts with a digit and contains a time separator.
    if !timestamp.starts_with(|c: char| c.is_ascii_digit()) || !timestamp.contains(':') {
        return None;
    }
    let (author, label) = rest.rsplit_once(" (")?;
    if author.is_empty() || label.is_empty() {
        return None;
    }
    Some((timestamp.to_string(), author.to_string(), label.to_string()))
}

/// Best-effort mapping from the rendered field label back to the column name.
/// Labels vary by instance ("Comments", "Additional comments", "Work notes"),
/// so match loosely and omit rather than guess for anything else.
fn element_for_label(label: &str) -> Option<String> {
    let l = label.to_lowercase();
    if l.contains("work note") {
        Some("work_notes".into())
    } else if l.contains("comment") {
        Some("comments".into())
    } else {
        None
    }
}

/// Newest first across merged streams. Only reorders when every timestamp is
/// ISO-shaped (`YYYY-MM-DD …` sorts lexicographically); locale-formatted dates
/// ("08/11/2026") do not sort as strings, so those keep stream order.
pub(crate) fn sort_newest_first(entries: &mut [Entry]) {
    let iso = |e: &Entry| {
        let b = e.created_on.as_bytes();
        b.len() > 10 && b[4] == b'-' && b[7] == b'-'
    };
    if entries.iter().all(iso) {
        entries.sort_by(|a, b| b.created_on.cmp(&a.created_on));
    }
}

pub(crate) fn validate_identifier(name: &str, what: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.starts_with(|c: char| c.is_ascii_lowercase());
    if !ok {
        return Err(Error::Usage(format!(
            "invalid {what} name '{name}': expected lowercase letters, digits and underscores"
        )));
    }
    Ok(())
}

/// sys_ids are 32-char GUIDs in practice, but the platform allows other
/// shapes; reject only what could alter the encoded query it is spliced into.
pub(crate) fn validate_sys_id(sys_id: &str) -> Result<()> {
    let ok = !sys_id.is_empty()
        && sys_id.len() <= 64
        && sys_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        return Err(Error::Usage(format!("invalid sys_id '{sys_id}'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_entries_newest_first() {
        let stream = "2026-08-11 10:40:20 - Abey Ahmad (Work notes)\nchecked the router\n\n\
                      2026-08-10 07:40:58 - Abey Ahmad (Comments)\nuser called back\n\n";
        let entries = parse_stream(stream).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].created_on, "2026-08-11 10:40:20");
        assert_eq!(entries[0].author, "Abey Ahmad");
        assert_eq!(entries[0].element.as_deref(), Some("work_notes"));
        assert_eq!(entries[0].label.as_deref(), Some("Work notes"));
        assert_eq!(entries[0].text, "checked the router");
        assert_eq!(entries[1].element.as_deref(), Some("comments"));
    }

    #[test]
    fn multiline_text_with_internal_blank_lines_survives() {
        let stream = "2026-08-11 10:40:20 - A B (Comments)\nline one\n\nline three\n\n\
                      2026-08-10 07:00:00 - A B (Comments)\nolder\n\n";
        let entries = parse_stream(stream).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "line one\n\nline three");
    }

    #[test]
    fn locale_formatted_timestamps_are_accepted() {
        let stream = "08/11/2026 10:40:20 AM - Abey Ahmad (Additional comments)\nhello\n\n";
        let entries = parse_stream(stream).unwrap();
        assert_eq!(entries[0].created_on, "08/11/2026 10:40:20 AM");
        assert_eq!(entries[0].element.as_deref(), Some("comments"));
    }

    #[test]
    fn author_names_containing_dash_and_parens_parse() {
        let stream = "2026-08-11 10:40:20 - Mary-Jane O'Neil (ext) (Work notes)\nnote\n\n";
        let entries = parse_stream(stream).unwrap();
        // rsplit on " (" keeps the parenthetical inside the author name.
        assert_eq!(entries[0].author, "Mary-Jane O'Neil (ext)");
        assert_eq!(entries[0].label.as_deref(), Some("Work notes"));
    }

    #[test]
    fn empty_stream_yields_no_entries() {
        assert!(parse_stream("").unwrap().is_empty());
        assert!(parse_stream("\n\n").unwrap().is_empty());
    }

    #[test]
    fn garbage_stream_is_an_error_not_silence() {
        let err = parse_stream("this is not a journal stream").unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn unknown_label_omits_element_but_keeps_label() {
        let stream = "2026-08-11 10:40:20 - A B (Approval history)\napproved\n\n";
        let entries = parse_stream(stream).unwrap();
        assert_eq!(entries[0].element, None);
        assert_eq!(entries[0].label.as_deref(), Some("Approval history"));
    }

    #[test]
    fn header_lookalikes_in_text_must_have_timestamp_shape() {
        // "2 - foo (bar)" starts with a digit but has no time separator.
        let stream = "2026-08-11 10:40:20 - A B (Comments)\n2 - foo (bar)\nmore\n\n";
        let entries = parse_stream(stream).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "2 - foo (bar)\nmore");
    }

    #[test]
    fn merged_iso_entries_sort_newest_first() {
        let mut entries = parse_stream(
            "2026-08-10 07:00:00 - A B (Comments)\nolder\n\n\
             2026-08-11 09:00:00 - A B (Comments)\nnewer\n\n",
        )
        .unwrap();
        sort_newest_first(&mut entries);
        assert_eq!(entries[0].text, "newer");
    }

    #[test]
    fn locale_timestamps_keep_stream_order() {
        let mut entries = parse_stream(
            "08/10/2026 07:00:00 AM - A B (Comments)\nfirst\n\n\
             08/11/2026 09:00:00 AM - A B (Comments)\nsecond\n\n",
        )
        .unwrap();
        sort_newest_first(&mut entries);
        assert_eq!(entries[0].text, "first");
    }

    #[test]
    fn identifier_and_sys_id_validation() {
        assert!(validate_identifier("incident", "table").is_ok());
        assert!(validate_identifier("u_custom_2", "table").is_ok());
        for bad in ["", "Incident", "x;drop", "9start"] {
            assert!(validate_identifier(bad, "table").is_err(), "{bad}");
        }
        assert!(validate_sys_id("47a91e3c2f8acf107efd1d707fa4e387").is_ok());
        for bad in ["", "a^b", "a=b", "a,b", &"x".repeat(65)] {
            assert!(validate_sys_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn undefined_field_matches_the_column_name_exactly() {
        let errs = vec![serde_json::json!({
            "message": "Validation error (FieldUndefined@[GlideRecord_Query/incident/_results/comments_and_work_notes]) : Field 'comments_and_work_notes' in type 'GlideRecord_TableResultsType_incident' is undefined",
            "errorType": "ValidationError"
        })];
        assert!(undefined_field(&errs, "comments_and_work_notes"));
        assert!(!undefined_field(&errs, "comments"));
        assert!(!undefined_field(&errs, "incident"));
    }
}
