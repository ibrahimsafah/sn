use crate::cli::kernel::{confirm_destructive, connect, emit, unwrap_or_raw};
use crate::cli::{GlobalFlags, WaitArgs};
use crate::error::Result;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum UpdateSetSub {
    /// Create a new Update Set.
    Create(UpdateSetCreateArgs),
    /// Retrieve a remote Update Set into this instance.
    Retrieve(UpdateSetRetrieveArgs),
    /// Preview a retrieved remote Update Set.
    Preview(UpdateSetIdArg),
    /// Commit a previewed remote Update Set.
    Commit(UpdateSetIdArg),
    /// Commit multiple remote Update Sets at once.
    CommitMultiple(UpdateSetCommitMultipleArgs),
    /// Back out (undo) an applied Update Set.
    BackOut(UpdateSetBackOutArgs),
}

#[derive(clap::Args, Debug)]
pub struct UpdateSetCreateArgs {
    /// Name for the new Update Set (required). Maps to the API's
    /// `update_set_name` query parameter.
    #[arg(long, alias = "update-set-name", required = true)]
    pub name: String,
    /// Optional description.
    #[arg(long)]
    pub description: Option<String>,
    /// sys_id to assign to the new Update Set.
    #[arg(long)]
    pub sys_id: Option<String>,
    /// Application scope.
    #[arg(long)]
    pub scope: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct UpdateSetRetrieveArgs {
    /// sys_id of the Update Set to retrieve (required).
    #[arg(long, required = true)]
    pub update_set_id: String,
    /// sys_id of the source's Remote Instance record (sys_update_set_source).
    #[arg(long)]
    pub update_source_id: Option<String>,
    /// Instance ID of the source's Remote Instance (sys_update_set_source).
    #[arg(long)]
    pub update_source_instance_id: Option<String>,
    /// Automatically preview after retrieval.
    #[arg(long)]
    pub auto_preview: bool,
    /// Clean up retrieved set after preview/commit.
    #[arg(long)]
    pub cleanup_retrieved: bool,
    #[command(flatten)]
    pub wait: WaitArgs,
}

/// Shared arg struct for preview and commit (single path param).
#[derive(clap::Args, Debug)]
pub struct UpdateSetIdArg {
    /// Remote Update Set sys_id.
    pub remote_update_set_id: String,
    #[command(flatten)]
    pub wait: WaitArgs,
}

#[derive(clap::Args, Debug)]
pub struct UpdateSetCommitMultipleArgs {
    /// Comma-separated list of remote Update Set sys_ids.
    #[arg(long, required = true)]
    pub ids: String,
    #[command(flatten)]
    pub wait: WaitArgs,
}

#[derive(clap::Args, Debug)]
pub struct UpdateSetBackOutArgs {
    /// sys_id of the Update Set to back out (required).
    #[arg(long, required = true)]
    pub update_set_id: String,
    /// Also roll back any application installs included in the set.
    #[arg(long)]
    pub rollback_installs: bool,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    #[command(flatten)]
    pub wait: WaitArgs,
}

pub fn create(global: &GlobalFlags, args: UpdateSetCreateArgs) -> Result<()> {
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = vec![("update_set_name".into(), args.name)];
    if let Some(v) = args.description {
        query.push(("description".into(), v));
    }
    if let Some(v) = args.sys_id {
        query.push(("sys_id".into(), v));
    }
    if let Some(v) = args.scope {
        query.push(("scope".into(), v));
    }
    let resp = client.post(
        "/api/sn_cicd/update_set/create",
        &query,
        &serde_json::json!({}),
    )?;
    emit(global, resp)
}

pub fn retrieve(global: &GlobalFlags, args: UpdateSetRetrieveArgs) -> Result<()> {
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = vec![("update_set_id".into(), args.update_set_id)];
    if let Some(v) = args.update_source_id {
        query.push(("update_source_id".into(), v));
    }
    if let Some(v) = args.update_source_instance_id {
        query.push(("update_source_instance_id".into(), v));
    }
    if args.auto_preview {
        query.push(("auto_preview".into(), "true".into()));
    }
    if args.cleanup_retrieved {
        query.push(("cleanup_retrieved".into(), "true".into()));
    }
    let resp = client.post(
        "/api/sn_cicd/update_set/retrieve",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait)
}

pub fn preview(global: &GlobalFlags, args: UpdateSetIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!(
        "/api/sn_cicd/update_set/preview/{}",
        args.remote_update_set_id
    );
    let resp = client.post(&path, &[], &serde_json::json!({}))?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait)
}

pub fn commit(global: &GlobalFlags, args: UpdateSetIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!(
        "/api/sn_cicd/update_set/commit/{}",
        args.remote_update_set_id
    );
    let resp = client.post(&path, &[], &serde_json::json!({}))?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait)
}

pub fn commit_multiple(global: &GlobalFlags, args: UpdateSetCommitMultipleArgs) -> Result<()> {
    let client = connect(global)?;
    let query = vec![("remote_update_set_ids".into(), args.ids)];
    let resp = client.post(
        "/api/sn_cicd/update_set/commitMultiple",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait)
}

pub fn back_out(global: &GlobalFlags, args: UpdateSetBackOutArgs) -> Result<()> {
    // Not a delete, but it reverts every record the set applied — instance-wide,
    // asynchronous, and with no undo of its own. Gated for the same reason the
    // deletes are: nothing here asks a second time.
    confirm_destructive(
        args.yes,
        "back out",
        &format!("update set {}", args.update_set_id),
    )?;
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = vec![("update_set_id".into(), args.update_set_id)];
    if args.rollback_installs {
        query.push(("rollback_installs".into(), "true".into()));
    }
    let resp = client.post(
        "/api/sn_cicd/update_set/back_out",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait)
}
