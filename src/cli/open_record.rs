use crate::cli::GlobalFlags;
use crate::cli::kernel::{build_client, build_profile, write_response};
use crate::cli::record_ref::{self, RefId};
use crate::client::normalize_base_url;
use crate::error::{Error, Result};
use serde_json::json;

#[derive(clap::Args, Debug)]
pub struct OpenArgs {
    /// Table name (e.g. `incident`), or a combined `table:sys_id` /
    /// `table:number` reference (e.g. `incident:INC0010001`).
    pub table: String,
    /// sys_id of the record. Omit when TABLE is a `table:id` reference.
    pub sys_id: Option<String>,
    /// Print the URL to stdout instead of opening a browser.
    #[arg(long)]
    pub print_url: bool,
}

pub fn run(global: &GlobalFlags, args: OpenArgs) -> Result<()> {
    let r = record_ref::parse_pair(&args.table, args.sys_id.as_deref(), "table")?;
    let profile = build_profile(global)?;
    // A sys_id reference builds a URL offline, as this command always has; a
    // number is the one form that needs the instance (one lookup) to name the
    // sys_id the form URL requires.
    let sys_id = match &r.id {
        RefId::SysId(id) => id.clone(),
        RefId::Number(_) => {
            let client = build_client(&profile, global.timeout)?;
            r.resolve(&client)?
        }
    };
    // Profiles store the bare host, so the scheme has to be put back on — a
    // scheme-less "acme.service-now.com/nav_to.do?..." is not a URL a browser
    // will open, and it's what every profile made the documented way produces.
    let instance = normalize_base_url(&profile.instance);
    let url = format!(
        "{instance}/nav_to.do?uri=%2F{table}.do%3Fsys_id%3D{sys_id}",
        table = r.table,
    );

    if args.print_url {
        println!("{url}");
        return Ok(());
    }

    webbrowser::open(&url).map_err(|e| Error::Transport(format!("open browser: {e}")))?;

    let out = json!({ "opened": true, "url": url });
    write_response(global, &out)
}
