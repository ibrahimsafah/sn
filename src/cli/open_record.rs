use crate::cli::kernel::{build_profile, write_response};
use crate::cli::GlobalFlags;
use crate::client::normalize_base_url;
use crate::error::{Error, Result};
use serde_json::json;

#[derive(clap::Args, Debug)]
pub struct OpenArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// sys_id of the record.
    pub sys_id: String,
    /// Print the URL to stdout instead of opening a browser.
    #[arg(long)]
    pub print_url: bool,
}

pub fn run(global: &GlobalFlags, args: OpenArgs) -> Result<()> {
    let profile = build_profile(global)?;
    // Profiles store the bare host, so the scheme has to be put back on — a
    // scheme-less "acme.service-now.com/nav_to.do?..." is not a URL a browser
    // will open, and it's what every profile made the documented way produces.
    let instance = normalize_base_url(&profile.instance);
    let url = format!(
        "{instance}/nav_to.do?uri=%2F{table}.do%3Fsys_id%3D{sys_id}",
        table = args.table,
        sys_id = args.sys_id,
    );

    if args.print_url {
        println!("{url}");
        return Ok(());
    }

    webbrowser::open(&url).map_err(|e| Error::Transport(format!("open browser: {e}")))?;

    let out = json!({ "opened": true, "url": url });
    write_response(global, &out)
}
