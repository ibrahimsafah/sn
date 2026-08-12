//! `sn variables` — catalog variables on a record, read and written with
//! verification.
//!
//! Variable storage is split: an sc_req_item's values live in `sc_item_option`
//! rows joined through `sc_item_option_mtom`; a record produced by a record
//! producer (incident, change_request, …) keeps its answers in
//! `question_answer` keyed by `table_name`/`table_sys_id`. Reads here walk
//! whichever join applies.
//!
//! Writes go through `PUT /api/sn_sc/servicecatalog/variables/{table}/{sys_id}`
//! — an undocumented scripted REST resource, but the only write path open to
//! non-admin roles: direct Table API writes to `sc_item_option` are 403 for a
//! plain `itil` user, while this endpoint gates on `canWrite()` of the parent
//! record. Established live against a PDI as both admin and `itil`; the
//! operation script (sys_ws_operation "Update Variables") reads the body as a
//! flat `{name: value}` map and **silently skips** any name the record's
//! variable pool does not contain — wrong case, wrong record, wrong shape all
//! return 200 with nothing written. `set` therefore pre-validates names
//! against a read of the pool (unknown name → usage error before any write)
//! and re-reads after the PUT, failing with exit 2 when a requested value did
//! not persist.
//!
//! A child `sc_task` does not own its RITM's variables (`gr.variables` on the
//! task resolves nothing, so every key would be silently skipped); both verbs
//! resolve an sc_task to its `request_item` first, and `set` reports the
//! resolution in its output.
//!
//! Multi-row variable sets (`sc_multi_row_question_answer`) are out of scope:
//! they store row JSON, not per-variable values.

use crate::body::{build_body, BodyInput};
use crate::cli::journal::{validate_identifier, validate_sys_id};
use crate::cli::table::{build_client, build_profile, write_response};
use crate::cli::GlobalFlags;
use crate::client::Client;
use crate::error::{Error, Result};
use clap::Subcommand;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const PUT_BASE: &str = "/api/sn_sc/servicecatalog/variables";

#[derive(Subcommand, Debug)]
pub enum VariablesSub {
    /// Read a record's variables as `[{name, label, value}]`.
    Get(VariablesGetArgs),
    /// Write variables and verify they persisted (PUT /api/sn_sc/servicecatalog/variables).
    Set(VariablesSetArgs),
}

#[derive(clap::Args, Debug)]
pub struct VariablesGetArgs {
    /// Table the record lives in (sc_req_item, or a record producer's target
    /// like incident). An sc_task is resolved to its request item.
    pub table: String,
    /// sys_id of the record.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct VariablesSetArgs {
    /// Table the record lives in (sc_req_item, or a record producer's target
    /// like incident). An sc_task is resolved to its request item.
    pub table: String,
    /// sys_id of the record.
    pub sys_id: String,
    /// Body source: inline JSON object of name/value pairs, @file (path), or
    /// @- (stdin). Names are case-sensitive; values are raw (reference →
    /// sys_id, checkbox → true/false).
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file
    /// (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

/// One variable, `get`'s output row. Sorted by name for stable output.
#[derive(Serialize, Debug)]
struct VarRow {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    value: String,
}

pub fn get(global: &GlobalFlags, args: VariablesGetArgs) -> Result<()> {
    validate_identifier(&args.table, "table")?;
    validate_sys_id(&args.sys_id)?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let (table, sys_id, _) = resolve_target(&client, &args.table, &args.sys_id)?;
    let rows = fetch_vars(&client, &table, &sys_id)?;
    write_response(global, &serde_json::to_value(rows).expect("rows serialize"))
}

pub fn set(global: &GlobalFlags, args: VariablesSetArgs) -> Result<()> {
    validate_identifier(&args.table, "table")?;
    validate_sys_id(&args.sys_id)?;
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let requested = body.as_object().expect("build_body returns an object");
    if requested.is_empty() {
        return Err(Error::Usage(
            "no variables to set; the body is empty".into(),
        ));
    }

    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let (table, sys_id, resolved_from) = resolve_target(&client, &args.table, &args.sys_id)?;

    // Pre-flight: the endpoint silently skips unknown names, so reject them
    // here — before anything is written — with the actual pool in the error.
    let before = var_map(&fetch_vars(&client, &table, &sys_id)?);
    let unknown: Vec<&str> = requested
        .keys()
        .filter(|k| !before.contains_key(k.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let available: Vec<&str> = before.keys().map(String::as_str).collect();
        return Err(Error::Usage(format!(
            "unknown variable(s) {} on {table} {sys_id} (names are case-sensitive); \
             available: {}",
            unknown.join(", "),
            available.join(", ")
        )));
    }

    client.put(&format!("{PUT_BASE}/{table}/{sys_id}"), &[], &body)?;

    // Verify: the endpoint returns 200 with an empty body no matter what, so
    // the read-back diff is the only confirmation there is.
    let after = var_map(&fetch_vars(&client, &table, &sys_id)?);
    let mut updated = Map::new();
    let mut unchanged = Map::new();
    let mut failed = Map::new();
    for (name, value) in requested {
        let want = comparable(value);
        let was = before.get(name).cloned().unwrap_or_default();
        let now = after.get(name).cloned().unwrap_or_default();
        if now == want {
            if was == now {
                unchanged.insert(name.clone(), Value::String(now));
            } else {
                updated.insert(name.clone(), json!({ "from": was, "to": now }));
            }
        } else {
            failed.insert(name.clone(), json!({ "requested": want, "stored": now }));
        }
    }
    if !failed.is_empty() {
        let names: Vec<&str> = failed.keys().map(String::as_str).collect();
        return Err(Error::Api {
            status: 200,
            message: format!(
                "variable write did not persist for: {} (the endpoint returned 200 \
                 but the stored value differs; read-only variable or value \
                 normalization)",
                names.join(", ")
            ),
            detail: None,
            transaction_id: None,
            sn_error: Some(Value::Object(failed)),
        });
    }

    let mut out = Map::new();
    out.insert("table".into(), Value::String(table));
    out.insert("sys_id".into(), Value::String(sys_id));
    if let Some(from) = resolved_from {
        out.insert("resolved_from".into(), from);
    }
    out.insert("updated".into(), Value::Object(updated));
    out.insert("unchanged".into(), Value::Object(unchanged));
    write_response(global, &Value::Object(out))
}

/// Follow an sc_task to the sc_req_item that owns the variable pool. Returns
/// `(table, sys_id, resolved_from)`; `resolved_from` names the original task
/// when a hop happened.
fn resolve_target(
    client: &Client,
    table: &str,
    sys_id: &str,
) -> Result<(String, String, Option<Value>)> {
    if table != "sc_task" {
        return Ok((table.to_string(), sys_id.to_string(), None));
    }
    let resp = client.get(
        &format!("/api/now/table/sc_task/{sys_id}"),
        &query_pairs(&[
            ("sysparm_fields", "request_item"),
            ("sysparm_display_value", "false"),
        ]),
    )?;
    let ritm = resp
        .pointer("/result/request_item/value")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ritm.is_empty() {
        return Err(Error::Api {
            status: 200,
            message: format!(
                "sc_task {sys_id} has no request_item; its variable pool cannot be located"
            ),
            detail: None,
            transaction_id: None,
            sn_error: None,
        });
    }
    Ok((
        "sc_req_item".to_string(),
        ritm,
        Some(json!({ "table": "sc_task", "sys_id": sys_id })),
    ))
}

/// Read the record's variable pool from wherever it is stored. An empty pool
/// triggers an existence check so a bad sys_id 404s instead of returning [].
fn fetch_vars(client: &Client, table: &str, sys_id: &str) -> Result<Vec<VarRow>> {
    let (path, query, name_key, label_key, value_key) = if table == "sc_req_item" {
        (
            "/api/now/table/sc_item_option_mtom",
            format!("request_item={sys_id}"),
            "sc_item_option.item_option_new.name",
            "sc_item_option.item_option_new.question_text",
            "sc_item_option.value",
        )
    } else {
        (
            "/api/now/table/question_answer",
            format!("table_name={table}^table_sys_id={sys_id}"),
            "question.name",
            "question.question_text",
            "value",
        )
    };
    let fields = format!("{name_key},{label_key},{value_key}");
    let resp = client.get(
        path,
        &query_pairs(&[
            ("sysparm_query", &query),
            ("sysparm_fields", &fields),
            ("sysparm_display_value", "false"),
            ("sysparm_limit", "1000"),
        ]),
    )?;
    let results = resp
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        // No rows: either the record has no variables or it does not exist.
        // The existence probe makes the difference visible (its 404 propagates).
        client.get(
            &format!("/api/now/table/{table}/{sys_id}"),
            &query_pairs(&[("sysparm_fields", "sys_id")]),
        )?;
        return Ok(Vec::new());
    }
    let text = |row: &Value, key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut rows: Vec<VarRow> = results
        .iter()
        .filter_map(|row| {
            let name = text(row, name_key);
            // Container/layout rows carry no name and are not addressable.
            if name.is_empty() {
                return None;
            }
            let label = text(row, label_key);
            Some(VarRow {
                name,
                label: (!label.is_empty()).then_some(label),
                value: text(row, value_key),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

fn var_map(rows: &[VarRow]) -> BTreeMap<String, String> {
    rows.iter()
        .map(|r| (r.name.clone(), r.value.clone()))
        .collect()
}

/// The string a JSON body value is expected to read back as: ServiceNow
/// stores every variable value as a string, booleans as "true"/"false".
fn comparable(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn query_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparable_matches_storage_formats() {
        assert_eq!(comparable(&json!("x")), "x");
        assert_eq!(comparable(&json!(true)), "true");
        assert_eq!(comparable(&json!(false)), "false");
        assert_eq!(comparable(&json!(42)), "42");
        assert_eq!(comparable(&Value::Null), "");
    }

    #[test]
    fn var_map_keys_by_name() {
        let rows = vec![
            VarRow {
                name: "a".into(),
                label: None,
                value: "1".into(),
            },
            VarRow {
                name: "b".into(),
                label: Some("B".into()),
                value: "".into(),
            },
        ];
        let m = var_map(&rows);
        assert_eq!(m.get("a").unwrap(), "1");
        assert_eq!(m.get("b").unwrap(), "");
    }
}
