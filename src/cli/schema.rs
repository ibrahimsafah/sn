use crate::cli::table::{build_client, build_profile, take_field};
use crate::cli::{GlobalFlags, OutputMode};
use crate::error::{Error, Result};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum SchemaSub {
    /// List every table on the instance.
    Tables(SchemaTablesArgs),
    /// List a table's columns, with type, mandatory and reference metadata.
    Columns(SchemaColumnsArgs),
    /// List the choice values available for one column.
    Choices(SchemaChoicesArgs),
}

#[derive(clap::Args, Debug, Default)]
pub struct SchemaTablesArgs {
    /// Case-insensitive substring filter on table name or label.
    #[arg(long)]
    pub filter: Option<String>,
    /// Only tables that are referenced by another table.
    #[arg(long)]
    pub reference_only: bool,
}

#[derive(clap::Args, Debug)]
pub struct SchemaColumnsArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Case-insensitive substring filter on column name or label.
    #[arg(long)]
    pub filter: Option<String>,
    /// Only columns of this internal type (e.g. `string`, `reference`).
    #[arg(long, value_name = "TYPE")]
    pub r#type: Option<String>,
    /// Only mandatory columns.
    #[arg(long)]
    pub mandatory: bool,
    /// Only columns the caller can write.
    #[arg(long)]
    pub writable: bool,
    /// Only columns backed by a choice list.
    #[arg(long)]
    pub choices_only: bool,
    /// Only reference columns.
    #[arg(long)]
    pub references_only: bool,
}

#[derive(clap::Args, Debug, Default)]
pub struct SchemaChoicesArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Column to read the choice list from (e.g. `state`).
    pub field: String,
}

pub fn tables(global: &GlobalFlags, args: SchemaTablesArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let resp = client.get("/api/now/doc/table/schema", &[])?;
    // Every table on the instance comes back here — thousands of objects — so
    // this unwraps by moving rather than cloning the array out of the envelope.
    let list = match global.output {
        OutputMode::Raw => resp,
        _ => match resp {
            // A response with no `result` at all is emitted whole rather than
            // as `[]`, which would report an empty instance to the caller.
            Value::Object(mut m) => match m.remove("result") {
                Some(Value::Array(a)) => Value::Array(filter_tables(a, &args)),
                Some(other) => other,
                None => Value::Object(m),
            },
            other => other,
        },
    };
    crate::cli::table::write_response(global, &list)
}

fn filter_tables(items: Vec<Value>, args: &SchemaTablesArgs) -> Vec<Value> {
    let needle = args.filter.as_deref().map(str::to_lowercase);
    items
        .into_iter()
        .filter(|t| {
            if args.reference_only
                && !t
                    .get("reference")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                return false;
            }
            if let Some(n) = &needle {
                let label = t
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let value = t
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !label.contains(n) && !value.contains(n) {
                    return false;
                }
            }
            true
        })
        .collect()
}

pub fn columns(global: &GlobalFlags, args: SchemaColumnsArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/ui/meta/{}", args.table);
    let resp = client.get(&path, &[])?;
    let list = match global.output {
        OutputMode::Raw => resp,
        OutputMode::Default | OutputMode::Table => {
            let cols = take_field(resp, "result")
                .and_then(|r| take_field(r, "columns"))
                .unwrap_or(Value::Object(serde_json::Map::new()));
            Value::Array(filter_columns(cols, &args))
        }
    };
    crate::cli::table::write_response(global, &list)
}

fn filter_columns(cols: Value, args: &SchemaColumnsArgs) -> Vec<Value> {
    let cols_obj = match cols {
        Value::Object(m) => m,
        _ => return vec![],
    };
    cols_obj
        .into_iter()
        .map(|(name, mut v)| {
            if let Value::Object(ref mut m) = v {
                m.insert("name".into(), Value::String(name));
            }
            v
        })
        .filter(|v| keep_column(v, args))
        .collect()
}

fn keep_column(col: &Value, args: &SchemaColumnsArgs) -> bool {
    let getb = |k: &str| col.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let gets = |k: &str| col.get(k).and_then(|v| v.as_str()).unwrap_or("");
    if args.mandatory && !getb("mandatory") {
        return false;
    }
    if args.writable && getb("read_only") {
        return false;
    }
    if args.choices_only
        && col
            .get("choices")
            .and_then(|v| v.as_array())
            .map_or(true, |a| a.is_empty())
    {
        return false;
    }
    if args.references_only && gets("type") != "reference" {
        return false;
    }
    if let Some(t) = args.r#type.as_deref() {
        if !gets("type").eq_ignore_ascii_case(t) {
            return false;
        }
    }
    if let Some(n) = args.filter.as_deref().map(str::to_lowercase) {
        let name = gets("name").to_lowercase();
        let label = gets("label").to_lowercase();
        if !name.contains(&n) && !label.contains(&n) {
            return false;
        }
    }
    true
}

pub fn choices(global: &GlobalFlags, args: SchemaChoicesArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/ui/meta/{}", args.table);
    let resp = client.get(&path, &[])?;
    let out = match global.output {
        OutputMode::Raw => resp,
        OutputMode::Default | OutputMode::Table => take_field(resp, "result")
            .and_then(|r| take_field(r, "columns"))
            .and_then(|c| take_field(c, &args.field))
            .and_then(|f| take_field(f, "choices"))
            .ok_or_else(|| {
                Error::Usage(format!(
                    "no choices found on field '{}' in table '{}'",
                    args.field, args.table
                ))
            })?,
    };
    crate::cli::table::write_response(global, &out)
}
