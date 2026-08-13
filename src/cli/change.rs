use crate::body::EmptyBody;
use crate::cli::kernel::{confirm_delete, confirm_destructive, connect, emit};
use crate::cli::{BodyArgs, DisplayValueOpt, GlobalFlags, Paging, SetLimit, ADVANCED};
use crate::error::{Error, Result};
use clap::{Subcommand, ValueEnum};

#[derive(Subcommand, Debug)]
pub enum ChangeSub {
    /// List change requests.
    List(ChangeListArgs),
    /// Get a change request by sys_id.
    Get(ChangeGetArgs),
    /// Create a change request.
    Create(ChangeCreateArgs),
    /// Update (PATCH) a change request.
    Update(ChangeUpdateArgs),
    /// Delete a change request.
    Delete(ChangeDeleteArgs),
    /// Get valid next states for a change.
    Nextstates(ChangeSysIdArg),
    /// Update approval state on a change.
    Approvals(ChangeApprovalsArgs),
    /// Update the risk assessment of a change.
    Risk(ChangeRiskArgs),
    /// Get the schedule for a change.
    Schedule(ChangeSysIdArg),
    /// Change task operations.
    Task {
        #[command(subcommand)]
        sub: ChangeTaskSub,
    },
    /// CI relationship operations on a change.
    Ci {
        #[command(subcommand)]
        sub: ChangeCiSub,
    },
    /// Conflict operations on a change.
    Conflict {
        #[command(subcommand)]
        sub: ChangeConflictSub,
    },
    /// List change models.
    Models(ChangeOptionalIdArg),
    /// List standard change templates.
    Templates(ChangeOptionalIdArg),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ChangeType {
    Normal,
    Emergency,
    Standard,
}

#[derive(clap::Args, Debug)]
pub struct ChangeListArgs {
    /// Filter by change type.
    #[arg(long, value_enum)]
    pub r#type: Option<ChangeType>,
    /// Encoded query, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub paging: Paging<1000>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(long, alias = "sysparm-exclude-reference-link", help_heading = ADVANCED)]
    pub exclude_reference_link: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct ChangeGetArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    /// Get a specific change type (uses type-specific endpoint).
    #[arg(long, value_enum)]
    pub r#type: Option<ChangeType>,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(long, alias = "sysparm-exclude-reference-link", help_heading = ADVANCED)]
    pub exclude_reference_link: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct ChangeCreateArgs {
    /// Change type: normal, emergency, or standard.
    #[arg(long, value_enum, default_value_t = ChangeType::Normal)]
    pub r#type: ChangeType,
    /// Standard change template sys_id (required for --type standard).
    #[arg(long)]
    pub template: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
}

#[derive(clap::Args, Debug)]
pub struct ChangeUpdateArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    /// Change type: normal, emergency, or standard.
    #[arg(long, value_enum)]
    pub r#type: Option<ChangeType>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
}

#[derive(clap::Args, Debug)]
pub struct ChangeDeleteArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    /// Change type: normal, emergency, or standard.
    #[arg(long, value_enum)]
    pub r#type: Option<ChangeType>,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(clap::Args, Debug)]
pub struct ChangeSysIdArg {
    /// sys_id of the change request.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct ChangeOptionalIdArg {
    /// sys_id to fetch; omit to list all.
    pub sys_id: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct ChangeApprovalsArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(clap::Args, Debug)]
pub struct ChangeRiskArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(Subcommand, Debug)]
pub enum ChangeTaskSub {
    /// List tasks for a change.
    List(ChangeTaskListArgs),
    /// Get a specific change task.
    Get(ChangeTaskGetArgs),
    /// Create a task on a change.
    Create(ChangeTaskCreateArgs),
    /// Update a change task (PATCH).
    Update(ChangeTaskUpdateArgs),
    /// Delete a change task.
    Delete(ChangeTaskDeleteArgs),
}

#[derive(clap::Args, Debug)]
pub struct ChangeTaskListArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub limit: SetLimit<100>,
}

#[derive(clap::Args, Debug)]
pub struct ChangeTaskGetArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    /// sys_id of the change task.
    pub task_sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct ChangeTaskCreateArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(clap::Args, Debug)]
pub struct ChangeTaskUpdateArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    /// sys_id of the change task.
    pub task_sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(clap::Args, Debug)]
pub struct ChangeTaskDeleteArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    /// sys_id of the change task.
    pub task_sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Subcommand, Debug)]
pub enum ChangeCiSub {
    /// List CIs associated with a change.
    List(ChangeSysIdArg),
    /// Add a CI to a change.
    Add(ChangeCiAddArgs),
}

#[derive(clap::Args, Debug)]
pub struct ChangeCiAddArgs {
    /// sys_id of the parent change request.
    pub change_sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

#[derive(Subcommand, Debug)]
pub enum ChangeConflictSub {
    /// Get conflicts for a change.
    Get(ChangeSysIdArg),
    /// Add a conflict to a change.
    Add(ChangeConflictAddArgs),
    /// Remove conflicts from a change.
    Remove(ChangeConflictRemoveArgs),
}

/// `remove` cannot reuse [`ChangeSysIdArg`] like its `get` sibling: that struct
/// is shared with `nextstates`, `schedule` and `ci list`, and a `--yes` flag on
/// a read command is noise that reads as if the read were dangerous.
#[derive(clap::Args, Debug)]
pub struct ChangeConflictRemoveArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(clap::Args, Debug)]
pub struct ChangeConflictAddArgs {
    /// sys_id of the change request.
    pub sys_id: String,
    #[command(flatten)]
    pub body: BodyArgs,
}

fn base_path(ct: Option<ChangeType>) -> &'static str {
    match ct {
        Some(ChangeType::Normal) => "/api/sn_chg_rest/change/normal",
        Some(ChangeType::Emergency) => "/api/sn_chg_rest/change/emergency",
        Some(ChangeType::Standard) => "/api/sn_chg_rest/change/standard",
        None => "/api/sn_chg_rest/change",
    }
}

pub fn list(global: &GlobalFlags, args: ChangeListArgs) -> Result<()> {
    let client = connect(global)?;
    let path = base_path(args.r#type);
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        query.push(("sysparm_query".into(), v));
    }
    if let Some(v) = args.fields {
        query.push(("sysparm_fields".into(), v));
    }
    query.push(("sysparm_limit".into(), args.paging.setlimit().to_string()));
    if let Some(v) = args.paging.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    if let Some(v) = args.display_value.display_value {
        let dv: crate::query::DisplayValue = v.into();
        query.push(("sysparm_display_value".into(), dv.as_str().into()));
    }
    if args.exclude_reference_link {
        query.push(("sysparm_exclude_reference_link".into(), "true".into()));
    }
    if let Some(v) = args.view {
        query.push(("sysparm_view".into(), v));
    }
    let resp = client.get(path, &query)?;
    emit(global, resp)
}

pub fn get(global: &GlobalFlags, args: ChangeGetArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{}/{}", base_path(args.r#type), args.sys_id);
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.fields {
        query.push(("sysparm_fields".into(), v));
    }
    if let Some(v) = args.display_value.display_value {
        let dv: crate::query::DisplayValue = v.into();
        query.push(("sysparm_display_value".into(), dv.as_str().into()));
    }
    if args.exclude_reference_link {
        query.push(("sysparm_exclude_reference_link".into(), "true".into()));
    }
    if let Some(v) = args.view {
        query.push(("sysparm_view".into(), v));
    }
    let resp = client.get(&path, &query)?;
    emit(global, resp)
}

pub fn create(global: &GlobalFlags, args: ChangeCreateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = match args.r#type {
        ChangeType::Standard => {
            let tmpl = args
                .template
                .ok_or_else(|| Error::Usage("--template is required for --type standard".into()))?;
            format!("/api/sn_chg_rest/change/standard/{tmpl}")
        }
        _ => base_path(Some(args.r#type)).to_string(),
    };
    let body = args.body.build(EmptyBody::Object)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.fields {
        query.push(("sysparm_fields".into(), v));
    }
    if let Some(v) = args.display_value.display_value {
        let dv: crate::query::DisplayValue = v.into();
        query.push(("sysparm_display_value".into(), dv.as_str().into()));
    }
    let resp = client.post(&path, &query, &body)?;
    emit(global, resp)
}

pub fn update(global: &GlobalFlags, args: ChangeUpdateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("{}/{}", base_path(args.r#type), args.sys_id);
    let body = args.body.build(EmptyBody::Reject)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.fields {
        query.push(("sysparm_fields".into(), v));
    }
    if let Some(v) = args.display_value.display_value {
        let dv: crate::query::DisplayValue = v.into();
        query.push(("sysparm_display_value".into(), dv.as_str().into()));
    }
    let resp = client.patch(&path, &query, &body)?;
    emit(global, resp)
}

pub fn delete(global: &GlobalFlags, args: ChangeDeleteArgs) -> Result<()> {
    confirm_delete(args.yes, &format!("change {}", args.sys_id))?;
    let client = connect(global)?;
    let path = format!("{}/{}", base_path(args.r#type), args.sys_id);
    client.delete(&path, &[])?;
    Ok(())
}

pub fn nextstates(global: &GlobalFlags, args: ChangeSysIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/nextstates", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn approvals(global: &GlobalFlags, args: ChangeApprovalsArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/approvals", args.sys_id);
    let body = args.body.build(EmptyBody::Reject)?;
    let resp = client.patch(&path, &[], &body)?;
    emit(global, resp)
}

pub fn risk(global: &GlobalFlags, args: ChangeRiskArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/risk", args.sys_id);
    let body = args.body.build(EmptyBody::Reject)?;
    let resp = client.patch(&path, &[], &body)?;
    emit(global, resp)
}

pub fn schedule(global: &GlobalFlags, args: ChangeSysIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/schedule", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn models(global: &GlobalFlags, args: ChangeOptionalIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = match args.sys_id {
        Some(id) => format!("/api/sn_chg_rest/change/model/{id}"),
        None => "/api/sn_chg_rest/change/model".to_string(),
    };
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn templates(global: &GlobalFlags, args: ChangeOptionalIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = match args.sys_id {
        Some(id) => format!("/api/sn_chg_rest/change/standard/template/{id}"),
        None => "/api/sn_chg_rest/change/standard/template".to_string(),
    };
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

pub fn task(global: &GlobalFlags, sub: ChangeTaskSub) -> Result<()> {
    match sub {
        ChangeTaskSub::List(args) => task_list(global, args),
        ChangeTaskSub::Get(args) => task_get(global, args),
        ChangeTaskSub::Create(args) => task_create(global, args),
        ChangeTaskSub::Update(args) => task_update(global, args),
        ChangeTaskSub::Delete(args) => task_delete(global, args),
    }
}

fn task_list(global: &GlobalFlags, args: ChangeTaskListArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/task", args.change_sys_id);
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.fields {
        query.push(("sysparm_fields".into(), v));
    }
    query.push(("sysparm_limit".into(), args.limit.setlimit.to_string()));
    let resp = client.get(&path, &query)?;
    emit(global, resp)
}

fn task_get(global: &GlobalFlags, args: ChangeTaskGetArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!(
        "/api/sn_chg_rest/change/{}/task/{}",
        args.change_sys_id, args.task_sys_id
    );
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

fn task_create(global: &GlobalFlags, args: ChangeTaskCreateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/task", args.change_sys_id);
    let body = args.body.build(EmptyBody::Object)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

fn task_update(global: &GlobalFlags, args: ChangeTaskUpdateArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!(
        "/api/sn_chg_rest/change/{}/task/{}",
        args.change_sys_id, args.task_sys_id
    );
    let body = args.body.build(EmptyBody::Reject)?;
    let resp = client.patch(&path, &[], &body)?;
    emit(global, resp)
}

fn task_delete(global: &GlobalFlags, args: ChangeTaskDeleteArgs) -> Result<()> {
    confirm_delete(
        args.yes,
        &format!("task {} on change {}", args.task_sys_id, args.change_sys_id),
    )?;
    let client = connect(global)?;
    let path = format!(
        "/api/sn_chg_rest/change/{}/task/{}",
        args.change_sys_id, args.task_sys_id
    );
    client.delete(&path, &[])?;
    Ok(())
}

pub fn ci(global: &GlobalFlags, sub: ChangeCiSub) -> Result<()> {
    match sub {
        ChangeCiSub::List(args) => ci_list(global, args),
        ChangeCiSub::Add(args) => ci_add(global, args),
    }
}

fn ci_list(global: &GlobalFlags, args: ChangeSysIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/ci", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

fn ci_add(global: &GlobalFlags, args: ChangeCiAddArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/ci", args.change_sys_id);
    let body = args.body.build(EmptyBody::Reject)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

pub fn conflict(global: &GlobalFlags, sub: ChangeConflictSub) -> Result<()> {
    match sub {
        ChangeConflictSub::Get(args) => conflict_get(global, args),
        ChangeConflictSub::Add(args) => conflict_add(global, args),
        ChangeConflictSub::Remove(args) => conflict_remove(global, args),
    }
}

fn conflict_get(global: &GlobalFlags, args: ChangeSysIdArg) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/conflict", args.sys_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}

fn conflict_add(global: &GlobalFlags, args: ChangeConflictAddArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/conflict", args.sys_id);
    let body = args.body.build(EmptyBody::Reject)?;
    let resp = client.post(&path, &[], &body)?;
    emit(global, resp)
}

fn conflict_remove(global: &GlobalFlags, args: ChangeConflictRemoveArgs) -> Result<()> {
    // The endpoint takes no conflict id: this clears every conflict recorded
    // against the change, which is why it is gated like `change task delete`.
    // The verb is the command's own — this is `conflict remove`, and a refusal
    // saying "delete" names an operation the caller never typed.
    confirm_destructive(
        args.yes,
        "remove",
        &format!("all conflicts on change {}", args.sys_id),
    )?;
    let client = connect(global)?;
    let path = format!("/api/sn_chg_rest/change/{}/conflict", args.sys_id);
    client.delete(&path, &[])?;
    Ok(())
}
