use crate::cli::kernel::{connect, emit, unwrap_or_raw};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AtfSub {
    /// Run an ATF test suite.
    Run(AtfRunArgs),
    /// Get results for an ATF test suite run.
    Results(AtfResultsArgs),
}

#[derive(clap::Args, Debug)]
pub struct AtfRunArgs {
    /// sys_id of the test suite.
    #[arg(long)]
    pub suite_id: Option<String>,
    /// Name of the test suite.
    #[arg(long)]
    pub suite_name: Option<String>,
    /// Browser name (e.g. `chrome`).
    #[arg(long)]
    pub browser_name: Option<String>,
    /// Browser version.
    #[arg(long)]
    pub browser_version: Option<String>,
    /// OS name.
    #[arg(long)]
    pub os_name: Option<String>,
    /// OS version.
    #[arg(long)]
    pub os_version: Option<String>,
    /// Run tests in cloud browser.
    #[arg(long)]
    pub run_in_cloud: bool,
    /// Record performance metrics during the run.
    #[arg(long)]
    pub performance_run: bool,
    /// Block until the operation completes (polls progress API).
    #[arg(long)]
    pub wait: bool,
    /// Give up on --wait after this many seconds (exit 3). Default: no limit.
    #[arg(long, value_name = "SECS", requires = "wait")]
    pub wait_timeout: Option<u64>,
}

#[derive(clap::Args, Debug)]
pub struct AtfResultsArgs {
    /// Test suite result sys_id.
    pub result_id: String,
}

pub fn run(global: &GlobalFlags, args: AtfRunArgs) -> Result<()> {
    if args.suite_id.is_none() && args.suite_name.is_none() {
        return Err(Error::Usage(
            "either --suite-id or --suite-name is required".into(),
        ));
    }
    let client = connect(global)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.suite_id {
        query.push(("test_suite_sys_id".into(), v));
    }
    if let Some(v) = args.suite_name {
        query.push(("test_suite_name".into(), v));
    }
    if let Some(v) = args.browser_name {
        query.push(("browser_name".into(), v));
    }
    if let Some(v) = args.browser_version {
        query.push(("browser_version".into(), v));
    }
    if let Some(v) = args.os_name {
        query.push(("os_name".into(), v));
    }
    if let Some(v) = args.os_version {
        query.push(("os_version".into(), v));
    }
    if args.run_in_cloud {
        query.push(("run_in_cloud".into(), "true".into()));
    }
    if args.performance_run {
        query.push(("performance_run".into(), "true".into()));
    }
    let resp = client.post("/api/sn_cicd/testsuite/run", &query, &serde_json::json!({}))?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::progress::finish_cicd(global, &client, out, args.wait, args.wait_timeout)
}

pub fn results(global: &GlobalFlags, args: AtfResultsArgs) -> Result<()> {
    let client = connect(global)?;
    let path = format!("/api/sn_cicd/testsuite/results/{}", args.result_id);
    let resp = client.get(&path, &[])?;
    emit(global, resp)
}
