use crate::body::{build_body, BodyInput};
use crate::cli::kernel::{connect, emit};
use crate::cli::GlobalFlags;
use crate::error::Result;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum IdentifySub {
    /// Create or update a CI (POST /api/now/identifyreconcile).
    CreateUpdate(IdentifyArgs),
    /// Identify a CI without modifying (POST /api/now/identifyreconcile/query).
    Query(IdentifyArgs),
    /// Create or update with enhanced options.
    CreateUpdateEnhanced(IdentifyEnhancedArgs),
    /// Identify with enhanced options.
    QueryEnhanced(IdentifyEnhancedArgs),
}

#[derive(clap::Args, Debug)]
pub struct IdentifyArgs {
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Data source identifier.
    #[arg(long)]
    pub data_source: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct IdentifyEnhancedArgs {
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Data source identifier.
    #[arg(long)]
    pub data_source: Option<String>,
    /// Comma-separated key:value options (e.g. `partial_payload:true,partial_commits:true`).
    #[arg(long)]
    pub options: Option<String>,
}

pub fn create_update(global: &GlobalFlags, args: IdentifyArgs) -> Result<()> {
    let client = connect(global)?;
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.data_source {
        query.push(("sysparm_data_source".into(), v));
    }
    let resp = client.post("/api/now/identifyreconcile", &query, &body)?;
    emit(global, resp)
}

pub fn query(global: &GlobalFlags, args: IdentifyArgs) -> Result<()> {
    let client = connect(global)?;
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let mut query_params: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.data_source {
        query_params.push(("sysparm_data_source".into(), v));
    }
    let resp = client.post("/api/now/identifyreconcile/query", &query_params, &body)?;
    emit(global, resp)
}

pub fn create_update_enhanced(global: &GlobalFlags, args: IdentifyEnhancedArgs) -> Result<()> {
    let client = connect(global)?;
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.data_source {
        query.push(("sysparm_data_source".into(), v));
    }
    if let Some(v) = args.options {
        query.push(("options".into(), v));
    }
    let resp = client.post("/api/now/identifyreconcile/enhanced", &query, &body)?;
    emit(global, resp)
}

pub fn query_enhanced(global: &GlobalFlags, args: IdentifyEnhancedArgs) -> Result<()> {
    let client = connect(global)?;
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.data_source {
        query.push(("sysparm_data_source".into(), v));
    }
    if let Some(v) = args.options {
        query.push(("options".into(), v));
    }
    let resp = client.post("/api/now/identifyreconcile/queryEnhanced", &query, &body)?;
    emit(global, resp)
}
