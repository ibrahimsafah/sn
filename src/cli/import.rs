use crate::body::{build_body, BodyInput};
use crate::cli::kernel::{connect, emit};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum ImportSub {
    /// Insert a single record into a staging table.
    Create(ImportCreateArgs),
    /// Insert multiple records into a staging table.
    Bulk(ImportBulkArgs),
    /// Retrieve an import set record.
    Get(ImportGetArgs),
}

#[derive(clap::Args, Debug)]
pub struct ImportCreateArgs {
    /// Staging table name.
    pub staging_table: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct ImportBulkArgs {
    /// Staging table name.
    pub staging_table: String,
    /// JSON array of records (or `{"records": [...]}`), @file, or @- for stdin.
    #[arg(long, short = 'D', required = true)]
    pub data: String,
}

#[derive(clap::Args, Debug)]
pub struct ImportGetArgs {
    /// Staging table name.
    pub staging_table: String,
    /// sys_id of the import set record.
    pub sys_id: String,
}

pub fn create(global: &GlobalFlags, args: ImportCreateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/now/import/{}", args.staging_table);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

pub fn bulk(global: &GlobalFlags, args: ImportBulkArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/now/import/{}/insertMultiple", args.staging_table);
    // insertMultiple expects {"records": [...]}; accept the documented bare
    // array and wrap it, or pass a pre-wrapped object through as-is.
    let body = match crate::body::parse_data_value(&args.data)? {
        Value::Array(records) => serde_json::json!({ "records": records }),
        obj @ Value::Object(_) => obj,
        _ => {
            return Err(Error::Usage(
                "--data must be a JSON array of records or {\"records\": [...]}".into(),
            ))
        }
    };
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

pub fn get(global: &GlobalFlags, args: ImportGetArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/now/import/{}/{}", args.staging_table, args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}
