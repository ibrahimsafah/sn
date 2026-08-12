use crate::cli::table::{build_client, build_profile, take_field};
use crate::cli::{GlobalFlags, OutputMode};
use crate::error::{Error, Result};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum UserSub {
    /// Show the currently authenticated user record.
    Me,
}

pub fn me(global: &GlobalFlags) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let query = vec![
        (
            "sysparm_query".into(),
            "user_name=javascript:gs.getUserName()".into(),
        ),
        ("sysparm_limit".into(), "1".into()),
    ];
    let resp = client.get("/api/now/table/sys_user", &query)?;
    let out = if matches!(global.output, OutputMode::Raw) {
        resp
    } else {
        take_field(resp, "result")
            .and_then(|r| match r {
                Value::Array(a) => a.into_iter().next(),
                _ => None,
            })
            .ok_or_else(|| Error::Api {
                status: 200,
                message: "no user record returned for current auth identity".into(),
                detail: None,
                transaction_id: None,
                sn_error: None,
            })?
    };
    crate::cli::table::write_response(global, &out)
}
