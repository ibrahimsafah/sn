//! `sn gr` — compiled GraphQL reads: dot-walked fields in one round trip.
//!
//! The one thing `GlideRecord_Query` does that the Table API cannot is
//! traverse references with real values: incident → `caller_id._reference` →
//! `manager._reference` → `email` in one request, where REST costs one GET
//! per hop per record. This command compiles friendly flags into that
//! document so nobody has to hand-write the `{value, displayValue}` leaf
//! convention or the `_reference` nesting — `sn graphql` stays underneath as
//! the passthrough, exactly like `sn raw` under the modeled commands.
//!
//! Compilation needs no dictionary lookup and no schema cache: in a path
//! `a.b.c` every non-terminal segment *must* be a reference for dot-walking
//! to mean anything, so `_reference` insertion is decided by position alone.
//! A path that guessed wrong fails server-side with a `FieldUndefined`
//! validation error naming `_reference`, which is mapped to a message naming
//! the real mistake (dot-walking through a non-reference field).
//!
//! Results are flattened back to the dotted keys the caller typed
//! (`{"caller_id.manager.email": …}`), so output looks like `sn table list`
//! and pipes the same way; the GraphQL nesting never reaches stdout except
//! under `--output raw`, which keeps the whole response envelope.
//!
//! `-q` inherits the platform-wide hazard every query surface here has: a
//! term the server cannot parse is silently dropped and rows come back
//! unfiltered. There is nothing generic to canary against on an arbitrary
//! query (unlike the `number=`-unique lookups in `record_ref.rs`), so
//! `--count` beside a filtered read is the cheap insurance.

use crate::cli::graphql::{errors_to_api_error, execute, graphql_errors};
use crate::cli::journal::undefined_field;
use crate::cli::kernel::{connect, write_response};
use crate::cli::{DisplayValueArg, DisplayValueOpt, GlobalFlags, OutputMode, Paging};
use crate::error::{Error, Result};
use serde_json::{Map, Value, json};

#[derive(clap::Args, Debug)]
pub struct GrArgs {
    /// Table to query (e.g. incident).
    #[arg(value_name = "TABLE")]
    pub table: String,
    /// Comma-separated field paths, dot-walked through reference fields:
    /// `number,caller_id.manager.email`. Required unless --count.
    #[arg(
        long,
        short = 'f',
        required_unless_present = "count",
        conflicts_with = "count"
    )]
    pub fields: Option<String>,
    /// Encoded query (GraphQL queryConditions). ORDERBY/ORDERBYDESC clauses
    /// are honored here, unlike some REST list endpoints.
    #[arg(long, short = 'q')]
    pub query: Option<String>,
    /// Emit only the matching row count, as {"count": N}.
    #[arg(long)]
    pub count: bool,
    #[command(flatten)]
    pub paging: Paging<100>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
}

pub fn run(global: &GlobalFlags, args: GrArgs) -> Result<()> {
    // Pure argv checks precede connect(), per the guard-ordering rule: a
    // typo'd field must not cost an OAuth token mint.
    validate_ident(&args.table, "table name")?;
    let paths = match args.fields.as_deref() {
        Some(spec) => parse_fields(spec)?,
        None => Vec::new(),
    };
    let mode = args
        .display_value
        .display_value
        .unwrap_or(DisplayValueArg::True);

    let selection = if args.count {
        "_rowCount".to_string()
    } else {
        format!(
            "_results {{ {}}}",
            render_selection(&build_tree(&paths), leaf_selection(mode))
        )
    };
    // --count still sends a pagination limit (of 1): _rowCount is the total
    // match count regardless of the page, measured live in get_record.rs's
    // canary, and rows we won't read shouldn't be built server-side.
    let (limit, offset) = if args.count {
        (1, 0)
    } else {
        (args.paging.limit.setlimit, args.paging.offset.unwrap_or(0))
    };
    let doc = build_document(&args.table, &selection, args.query.is_some(), limit, offset);

    let client = connect(global)?;
    let vars = args.query.as_ref().map(|q| json!({ "qc": q }));
    let resp = execute(&client, &doc, vars, None)?;

    let errors = graphql_errors(&resp);
    if !errors.is_empty() {
        return Err(map_errors(errors, &args.table));
    }
    if global.output == OutputMode::Raw {
        return write_response(global, &resp);
    }

    // The table name is a validated identifier (no '/' or '~'), so the JSON
    // pointer cannot be misdirected by it.
    let container = resp
        .pointer(&format!("/data/GlideRecord_Query/{}", args.table))
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::Instance {
            message: format!(
                "GraphQL response carried no result for table '{}'",
                args.table
            ),
            detail: Some("the query succeeded but the reply does not answer it".into()),
        })?;

    if args.count {
        let n = container
            .get("_rowCount")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::Instance {
                message: format!(
                    "GraphQL response carried no _rowCount for table '{}'",
                    args.table
                ),
                detail: None,
            })?;
        return write_response(global, &json!({ "count": n }));
    }

    let records: Vec<Value> = container
        .get("_results")
        .and_then(Value::as_array)
        .map(|rs| rs.iter().map(|r| flatten(r, &paths, mode)).collect())
        .unwrap_or_default();
    write_response(global, &Value::Array(records))
}

/// One requested field path: the dotted key the caller typed (which is also
/// the output key) and its segments.
#[derive(Debug, PartialEq)]
struct FieldPath {
    key: String,
    segments: Vec<String>,
}

/// A node in the merged selection tree. `requested` marks a path ending here
/// (so the leaf `{value, displayValue}` selection is emitted even when the
/// node also carries a `_reference` block for deeper paths).
#[derive(Debug)]
struct Node {
    name: String,
    requested: bool,
    children: Vec<Node>,
}

/// Parse `--fields` into deduplicated paths, validating every segment. The
/// segments are interpolated into the GraphQL document, so this is also the
/// injection guard: only identifier characters survive.
fn parse_fields(spec: &str) -> Result<Vec<FieldPath>> {
    let mut paths: Vec<FieldPath> = Vec::new();
    for raw in spec.split(',') {
        let key = raw.trim();
        if key.is_empty() {
            return Err(Error::Usage("--fields has an empty entry".into()));
        }
        let segments: Vec<String> = key.split('.').map(str::to_string).collect();
        for seg in &segments {
            validate_ident(seg, &format!("field segment in '{key}'"))?;
        }
        if !paths.iter().any(|p| p.key == key) {
            paths.push(FieldPath {
                key: key.to_string(),
                segments,
            });
        }
    }
    Ok(paths)
}

/// A ServiceNow identifier: leading letter or underscore, then letters,
/// digits, underscores. Anything else is refused before it can reach the
/// interpolated GraphQL document.
fn validate_ident(s: &str, what: &str) -> Result<()> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "invalid {what}: '{s}' (expected letters, digits and underscores)"
        )))
    }
}

/// Merge the paths into one selection tree, so `caller_id.name` and
/// `caller_id.email` select `caller_id { _reference { … } }` once.
fn build_tree(paths: &[FieldPath]) -> Vec<Node> {
    let mut roots = Vec::new();
    for p in paths {
        insert(&mut roots, &p.segments);
    }
    roots
}

fn insert(nodes: &mut Vec<Node>, segments: &[String]) {
    let (first, rest) = segments.split_first().expect("paths are non-empty");
    let idx = match nodes.iter().position(|n| n.name == *first) {
        Some(i) => i,
        None => {
            nodes.push(Node {
                name: first.clone(),
                requested: false,
                children: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    if rest.is_empty() {
        nodes[idx].requested = true;
    } else {
        insert(&mut nodes[idx].children, rest);
    }
}

/// The leaf selection `--display-value` asks for.
fn leaf_selection(mode: DisplayValueArg) -> &'static str {
    match mode {
        DisplayValueArg::True => "displayValue",
        DisplayValueArg::False => "value",
        DisplayValueArg::All => "value displayValue",
    }
}

/// Render the tree as a GraphQL selection set. A non-terminal segment is a
/// reference by construction, so it nests through `_reference`; a node that
/// is both requested and traversed carries the leaf selection *and* the
/// `_reference` block.
fn render_selection(nodes: &[Node], leaf: &str) -> String {
    let mut s = String::new();
    for n in nodes {
        s.push_str(&n.name);
        s.push_str(" { ");
        if n.requested || n.children.is_empty() {
            s.push_str(leaf);
            s.push(' ');
        }
        if !n.children.is_empty() {
            s.push_str("_reference { ");
            s.push_str(&render_selection(&n.children, leaf));
            s.push_str("} ");
        }
        s.push_str("} ");
    }
    s
}

/// The whole document. The query rides in as the `$qc` variable — never
/// interpolated, so encoded-query contents can't break out of the document —
/// and the argument is omitted entirely when no `-q` was given.
fn build_document(
    table: &str,
    selection: &str,
    has_query: bool,
    limit: u32,
    offset: u32,
) -> String {
    let (header, condition) = if has_query {
        ("query ($qc: String!) ", "queryConditions: $qc, ")
    } else {
        ("", "")
    };
    format!(
        "{header}{{ GlideRecord_Query {{ {table}({condition}pagination: {{ limit: {limit}, \
         offset: {offset} }}) {{ {selection} }} }} }}"
    )
}

/// Flatten one `_results` element back to the dotted keys the caller typed.
fn flatten(record: &Value, paths: &[FieldPath], mode: DisplayValueArg) -> Value {
    let mut out = Map::new();
    for p in paths {
        out.insert(p.key.clone(), extract(record, &p.segments, mode));
    }
    Value::Object(out)
}

/// Walk one path through the nested response. A null anywhere along the way
/// (empty reference, unreadable row under ACL) yields null for the whole
/// key — dot-walking semantics.
fn extract(record: &Value, segments: &[String], mode: DisplayValueArg) -> Value {
    let mut cur = record;
    for (i, seg) in segments.iter().enumerate() {
        let Some(v) = cur.get(seg).filter(|v| !v.is_null()) else {
            return Value::Null;
        };
        if i + 1 == segments.len() {
            return leaf_value(v, mode);
        }
        match v.get("_reference").filter(|r| !r.is_null()) {
            Some(r) => cur = r,
            None => return Value::Null,
        }
    }
    Value::Null
}

/// One leaf per `--display-value`: the display value, the raw value, or —
/// for `all` — both under the Table API's `{display_value, value}` spelling,
/// so the three modes mirror what `sn table list` emits.
fn leaf_value(v: &Value, mode: DisplayValueArg) -> Value {
    let take = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    match mode {
        DisplayValueArg::True => take("displayValue"),
        DisplayValueArg::False => take("value"),
        DisplayValueArg::All => json!({
            "display_value": take("displayValue"),
            "value": take("value"),
        }),
    }
}

/// Map a non-empty `errors` array to the CLI error, naming the two failure
/// shapes the compiler can cause before falling back to the passthrough
/// mapping. The HTTP status genuinely was 200 in all of them.
///
/// A table with no GraphQL query field arrives in two spellings, so both are
/// checked. `sn get` documented `FieldUndefined` naming the table, but the
/// reference instance (measured live 2026-08-31) instead fails validation on
/// the unknown field's *arguments* first — `UnknownArgument@`
/// `[GlideRecord_Query/not_a_table]) : Unknown field argument 'pagination'` —
/// which, passed through raw, blames `pagination` for a typo'd table. The
/// error path ending at the table level (`@[GlideRecord_Query/<table>])`) is
/// what both spellings share; a bad *leaf* field's path is always deeper
/// (`…/<table>/_results/<field>/…`), so it cannot match this.
fn map_errors(errors: Vec<Value>, table: &str) -> Error {
    let table_path = format!("@[GlideRecord_Query/{table}])");
    let at_table_level = errors.iter().any(|e| {
        e.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(&table_path))
    });
    if at_table_level || undefined_field(&errors, table) {
        let detail = errors
            .first()
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map(String::from);
        return Error::Api {
            status: 200,
            message: format!("table '{table}' is not queryable via GraphQL"),
            detail,
            transaction_id: None,
            sn_error: Some(Value::Array(errors)),
        };
    }
    if undefined_field(&errors, "_reference") {
        return Error::Api {
            status: 200,
            message: "a --fields path dot-walks through a non-reference field".into(),
            detail: Some(
                "only reference fields can be dot-walked; the validation error in sn_error \
                 names the field's actual type"
                    .into(),
            ),
            transaction_id: None,
            sn_error: Some(Value::Array(errors)),
        };
    }
    errors_to_api_error(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(specs: &[&str]) -> Vec<FieldPath> {
        parse_fields(&specs.join(",")).unwrap()
    }

    #[test]
    fn parse_fields_dedups_and_trims() {
        let p = parse_fields(" number , number,caller_id.email ").unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].key, "number");
        assert_eq!(p[1].segments, vec!["caller_id", "email"]);
    }

    #[test]
    fn parse_fields_rejects_bad_input() {
        for bad in ["", "a,,b", "a b", "a.{ x }", "1st", "a..b"] {
            assert!(
                matches!(parse_fields(bad), Err(Error::Usage(_))),
                "{bad:?} should be a usage error"
            );
        }
    }

    #[test]
    fn table_name_is_validated() {
        assert!(validate_ident("incident", "table name").is_ok());
        assert!(validate_ident("u_custom_2", "table name").is_ok());
        for bad in ["", "bad table", "x(y)", "9lives"] {
            assert!(validate_ident(bad, "table name").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn shared_prefixes_merge_into_one_reference_block() {
        let sel = render_selection(
            &build_tree(&paths(&["caller_id.name", "caller_id.email"])),
            "displayValue",
        );
        assert_eq!(
            sel,
            "caller_id { _reference { name { displayValue } email { displayValue } } } "
        );
    }

    #[test]
    fn requested_and_traversed_node_carries_both_selections() {
        let sel = render_selection(
            &build_tree(&paths(&["caller_id", "caller_id.email"])),
            "value",
        );
        assert_eq!(sel, "caller_id { value _reference { email { value } } } ");
    }

    #[test]
    fn document_with_query_uses_a_variable() {
        let doc = build_document("incident", "_results { number { value } }", true, 10, 5);
        assert_eq!(
            doc,
            "query ($qc: String!) { GlideRecord_Query { incident(queryConditions: $qc, \
             pagination: { limit: 10, offset: 5 }) { _results { number { value } } } } }"
        );
    }

    #[test]
    fn document_without_query_omits_the_argument() {
        let doc = build_document("incident", "_rowCount", false, 1, 0);
        assert_eq!(
            doc,
            "{ GlideRecord_Query { incident(pagination: { limit: 1, offset: 0 }) \
             { _rowCount } } }"
        );
    }

    #[test]
    fn flatten_walks_references_and_nulls_broken_paths() {
        let record = json!({
            "number": {"displayValue": "INC0001", "value": "INC0001"},
            "caller_id": {
                "displayValue": "Beth Anglin",
                "_reference": {
                    "email": {"displayValue": "beth@example.com", "value": "beth@example.com"},
                    "manager": {"_reference": null}
                }
            }
        });
        let ps = paths(&[
            "number",
            "caller_id",
            "caller_id.email",
            "caller_id.manager.name",
            "missing.field",
        ]);
        let flat = flatten(&record, &ps, DisplayValueArg::True);
        assert_eq!(flat["number"], "INC0001");
        assert_eq!(flat["caller_id"], "Beth Anglin");
        assert_eq!(flat["caller_id.email"], "beth@example.com");
        assert_eq!(flat["caller_id.manager.name"], Value::Null);
        assert_eq!(flat["missing.field"], Value::Null);
    }

    #[test]
    fn display_value_all_mirrors_the_table_api_shape() {
        let record = json!({"number": {"displayValue": "INC0001", "value": "x"}});
        let flat = flatten(&record, &paths(&["number"]), DisplayValueArg::All);
        assert_eq!(
            flat["number"],
            json!({"display_value": "INC0001", "value": "x"})
        );
    }

    #[test]
    fn unqueryable_table_and_bad_dotwalk_get_named() {
        let table_err = map_errors(
            vec![json!({"message":
                "Validation error (FieldUndefined@[GlideRecord_Query/foo]) : Field 'foo' in type 'GlideRecord_Query' is undefined"})],
            "foo",
        );
        match table_err {
            Error::Api {
                status, message, ..
            } => {
                assert_eq!(status, 200);
                assert!(message.contains("not queryable"), "{message}");
            }
            other => panic!("expected Api error, got {other:?}"),
        }

        // The reference instance's actual spelling (measured live): validation
        // fails on the unknown field's arguments, never naming the field.
        let live_err = map_errors(
            vec![json!({"message":
                "Validation error (UnknownArgument@[GlideRecord_Query/not_a_table]) : Unknown field argument 'pagination'"})],
            "not_a_table",
        );
        match live_err {
            Error::Api {
                message, detail, ..
            } => {
                assert!(message.contains("not queryable"), "{message}");
                assert!(detail.unwrap().contains("UnknownArgument"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }

        let walk_err = map_errors(
            vec![json!({"message":
                "Validation error (FieldUndefined) : Field '_reference' in type 'GlideStringField' is undefined"})],
            "incident",
        );
        match walk_err {
            Error::Api { message, .. } => {
                assert!(message.contains("non-reference field"), "{message}");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
