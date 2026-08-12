use crate::cli::table::{build_client, build_profile, confirm_destructive, unwrap_or_raw};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AppSub {
    /// Install an application from the app repository.
    Install(AppInstallArgs),
    /// Publish an application to the app repository.
    Publish(AppPublishArgs),
    /// Roll back an application to a previous version.
    Rollback(AppRollbackArgs),
}

#[derive(clap::Args, Debug)]
pub struct AppInstallArgs {
    /// sys_id of the application.
    #[arg(long)]
    pub sys_id: Option<String>,
    /// Application scope (e.g. `x_acme_myapp`).
    #[arg(long)]
    pub scope: Option<String>,
    /// Version to install.
    #[arg(long)]
    pub version: Option<String>,
    /// Automatically upgrade the base application if needed.
    #[arg(long)]
    pub auto_upgrade_base_app: bool,
    /// Version of the base application to use.
    #[arg(long)]
    pub base_app_version: Option<String>,
    /// Block until the operation completes (polls progress API).
    #[arg(long)]
    pub wait: bool,
    /// Give up on --wait after this many seconds (exit 3). Default: no limit.
    #[arg(long, value_name = "SECS", requires = "wait")]
    pub wait_timeout: Option<u64>,
}

#[derive(clap::Args, Debug)]
pub struct AppPublishArgs {
    /// sys_id of the application.
    #[arg(long)]
    pub sys_id: Option<String>,
    /// Application scope (e.g. `x_acme_myapp`).
    #[arg(long)]
    pub scope: Option<String>,
    /// Version to publish.
    #[arg(long)]
    pub version: Option<String>,
    /// Developer notes for this publish.
    #[arg(long)]
    pub dev_notes: Option<String>,
    /// Block until the operation completes (polls progress API).
    #[arg(long)]
    pub wait: bool,
    /// Give up on --wait after this many seconds (exit 3). Default: no limit.
    #[arg(long, value_name = "SECS", requires = "wait")]
    pub wait_timeout: Option<u64>,
}

#[derive(clap::Args, Debug)]
pub struct AppRollbackArgs {
    /// sys_id of the application.
    #[arg(long)]
    pub sys_id: Option<String>,
    /// Application scope (e.g. `x_acme_myapp`).
    #[arg(long)]
    pub scope: Option<String>,
    /// Version to roll back to (required).
    #[arg(long, required = true)]
    pub version: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Block until the operation completes (polls progress API).
    #[arg(long)]
    pub wait: bool,
    /// Give up on --wait after this many seconds (exit 3). Default: no limit.
    #[arg(long, value_name = "SECS", requires = "wait")]
    pub wait_timeout: Option<u64>,
}

pub fn install(global: &GlobalFlags, args: AppInstallArgs) -> Result<()> {
    if args.sys_id.is_none() && args.scope.is_none() {
        return Err(Error::Usage(
            "either --sys-id or --scope is required".into(),
        ));
    }
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.sys_id {
        query.push(("sys_id".into(), v));
    }
    if let Some(v) = args.scope {
        query.push(("scope".into(), v));
    }
    if let Some(v) = args.version {
        query.push(("version".into(), v));
    }
    if args.auto_upgrade_base_app {
        query.push(("auto_upgrade_base_app".into(), "true".into()));
    }
    if let Some(v) = args.base_app_version {
        query.push(("base_app_version".into(), v));
    }
    let resp = client.post(
        "/api/sn_cicd/app_repo/install",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait, args.wait_timeout)
}

pub fn publish(global: &GlobalFlags, args: AppPublishArgs) -> Result<()> {
    if args.sys_id.is_none() && args.scope.is_none() {
        return Err(Error::Usage(
            "either --sys-id or --scope is required".into(),
        ));
    }
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.sys_id {
        query.push(("sys_id".into(), v));
    }
    if let Some(v) = args.scope {
        query.push(("scope".into(), v));
    }
    if let Some(v) = args.version {
        query.push(("version".into(), v));
    }
    if let Some(v) = args.dev_notes {
        query.push(("dev_notes".into(), v));
    }
    let resp = client.post(
        "/api/sn_cicd/app_repo/publish",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait, args.wait_timeout)
}

pub fn rollback(global: &GlobalFlags, args: AppRollbackArgs) -> Result<()> {
    let target = match (args.sys_id.as_deref(), args.scope.as_deref()) {
        (Some(id), _) => format!("app {id}"),
        (None, Some(scope)) => format!("app {scope}"),
        (None, None) => {
            return Err(Error::Usage(
                "either --sys-id or --scope is required".into(),
            ))
        }
    };
    // Replaces the installed application on the whole instance with an older
    // build; whatever the newer version wrote is not rolled back with it.
    confirm_destructive(
        args.yes,
        "roll back",
        &format!("{target} to version {}", args.version),
    )?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.sys_id {
        query.push(("sys_id".into(), v));
    }
    if let Some(v) = args.scope {
        query.push(("scope".into(), v));
    }
    query.push(("version".into(), args.version));
    let resp = client.post(
        "/api/sn_cicd/app_repo/rollback",
        &query,
        &serde_json::json!({}),
    )?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait, args.wait_timeout)
}
