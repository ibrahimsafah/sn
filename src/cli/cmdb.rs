use crate::body::{build_body, BodyInput};
use crate::cli::table::{build_client, build_profile, confirm_delete, unwrap_or_raw};
use crate::cli::GlobalFlags;
use crate::error::Result;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum CmdbSub {
    /// List CI records for a CMDB class.
    List(CmdbListArgs),
    /// Get a CI record with relations.
    Get(CmdbGetArgs),
    /// Create a CI record.
    Create(CmdbCreateArgs),
    /// Update a CI record (PATCH).
    Update(CmdbUpdateArgs),
    /// Get metadata for a CMDB class.
    Meta(CmdbMetaArgs),
    /// Relation operations on a CI.
    Relation {
        #[command(subcommand)]
        sub: CmdbRelationSub,
    },
}

#[derive(clap::Args, Debug)]
pub struct CmdbListArgs {
    /// CMDB class name (e.g. `cmdb_ci_server`).
    pub class: String,
    /// Encoded query, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Maximum records returned. Maps to sysparm_limit.
    #[arg(long, alias = "sysparm-limit", alias = "limit", default_value_t = 1000)]
    pub setlimit: u32,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbGetArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct CmdbCreateArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbUpdateArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbMetaArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
}

#[derive(Subcommand, Debug)]
pub enum CmdbRelationSub {
    /// Create a relation on a CI.
    Add(CmdbRelationAddArgs),
    /// Delete a relation from a CI.
    Delete(CmdbRelationDeleteArgs),
}

#[derive(clap::Args, Debug)]
pub struct CmdbRelationAddArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbRelationDeleteArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// sys_id of the relation to delete.
    pub rel_sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn list(global: &GlobalFlags, args: CmdbListArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}", args.class);
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        query.push(("sysparm_query".into(), v));
    }
    query.push(("sysparm_limit".into(), args.setlimit.to_string()));
    if let Some(v) = args.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    let resp = client.get(&path, &query)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn get(global: &GlobalFlags, args: CmdbGetArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}/{}", args.class, args.sys_id);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn create(global: &GlobalFlags, args: CmdbCreateArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}", args.class);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn update(global: &GlobalFlags, args: CmdbUpdateArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}/{}", args.class, args.sys_id);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.patch(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn meta(global: &GlobalFlags, args: CmdbMetaArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/meta/{}", args.class);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn relation(global: &GlobalFlags, sub: CmdbRelationSub) -> Result<()> {
    match sub {
        CmdbRelationSub::Add(args) => relation_add(global, args),
        CmdbRelationSub::Delete(args) => relation_delete(global, args),
    }
}

fn relation_add(global: &GlobalFlags, args: CmdbRelationAddArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!(
        "/api/now/cmdb/instance/{}/{}/relation",
        args.class, args.sys_id
    );
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

fn relation_delete(global: &GlobalFlags, args: CmdbRelationDeleteArgs) -> Result<()> {
    confirm_delete(
        args.yes,
        &format!(
            "relation {} on {}/{}",
            args.rel_sys_id, args.class, args.sys_id
        ),
    )?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!(
        "/api/now/cmdb/instance/{}/{}/relation/{}",
        args.class, args.sys_id, args.rel_sys_id
    );
    client.delete(&path, &[])?;
    Ok(())
}
