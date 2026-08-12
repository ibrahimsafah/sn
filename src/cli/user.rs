use crate::cli::table::{build_client, build_profile};
use crate::cli::{GlobalFlags, OutputMode};
use crate::error::{Error, Result};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum UserSub {
    /// Show the currently authenticated user record.
    Me,
}

pub fn me(global: &GlobalFlags) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    // `sysparm_limit=2`, not 1, for the reason spelled out on
    // `cli::auth::identify_via_sys_user`: ServiceNow silently drops a query term
    // it cannot evaluate, and the surviving `sysparm_limit` then returns
    // whichever rows sort first. `user_name` is unique, so an evaluated filter
    // can match at most one row — a second row is proof the filter is gone and
    // these rows are arbitrary strangers, which is the one thing this command
    // must never hand back as "you".
    let query = vec![
        (
            "sysparm_query".into(),
            "user_name=javascript:gs.getUserName()".into(),
        ),
        ("sysparm_limit".into(), "2".into()),
    ];
    let resp = client.get("/api/now/table/sys_user", &query)?;
    if resp["result"].as_array().is_some_and(|rows| rows.len() > 1) {
        return Err(Error::Api {
            status: 200,
            message: "instance did not evaluate the gs.getUserName() query, so the rows returned \
                      are arbitrary users rather than the caller"
                .into(),
            detail: Some(
                "scripted query evaluation appears to be disabled on this instance; \
                 `sn ping` reports the authenticated identity without one"
                    .into(),
            ),
            transaction_id: None,
            sn_error: None,
        });
    }
    let out = if matches!(global.output, OutputMode::Raw) {
        resp
    } else {
        resp["result"].get(0).cloned().ok_or_else(|| Error::Api {
            status: 200,
            message: "no user record returned for current auth identity".into(),
            detail: None,
            transaction_id: None,
            sn_error: None,
        })?
    };
    crate::cli::table::write_response(global, &out)
}
