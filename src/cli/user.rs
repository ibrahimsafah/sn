use crate::cli::auth::identify_via_current_user;
use crate::cli::table::{build_client, build_profile, take_field};
use crate::cli::{GlobalFlags, OutputMode};
use crate::client::Client;
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

    // Preferred path: let the instance name the record, then read that record by
    // sys_id. No scripted term travels with the request, so the failure this
    // command is most exposed to — a `javascript:` filter the instance silently
    // drops, leaving a bare `sysparm_limit` that returns strangers — cannot
    // happen rather than merely being detected.
    let resp = match identify_via_current_user(&client)? {
        Some(id) => match id.sys_id {
            Some(sys_id) => client.get(&format!("/api/now/table/sys_user/{sys_id}"), &[])?,
            // The endpoint answered but identified no record (an older shape, a
            // stub). A name alone can't be read back safely, so fall back.
            None => scripted_self_read(&client)?,
        },
        // Endpoint absent, or a 200 that named nobody.
        None => scripted_self_read(&client)?,
    };

    let out = if matches!(global.output, OutputMode::Raw) {
        resp
    } else {
        // The direct read returns an object under `result`; the scripted fallback
        // an array of at most one. Take whichever this was — by move, since this
        // is a whole user record and nothing reads `resp` afterwards.
        let record = match take_field(resp, "result") {
            Some(Value::Array(rows)) => rows.into_iter().next(),
            Some(Value::Null) | None => None,
            Some(other) => Some(other),
        };
        record.ok_or_else(|| Error::Instance {
            message: "no user record returned for the authenticated identity".into(),
            detail: None,
        })?
    };
    crate::cli::table::write_response(global, &out)
}

/// Fallback: `sys_user` filtered by a server-side `gs.getUserName()` term, for an
/// instance where [`identify_via_current_user`] could not name the record.
///
/// `sysparm_limit=2`, not 1, for the reason spelled out on
/// `cli::auth::identify_via_sys_user`: ServiceNow silently drops a query term it
/// cannot evaluate, and the surviving `sysparm_limit` then returns whichever rows
/// sort first. `user_name` is unique, so an evaluated filter can match at most one
/// row — a second row is proof the filter is gone and these rows are arbitrary
/// strangers, which is the one thing this command must never hand back as "you".
fn scripted_self_read(client: &Client) -> Result<Value> {
    let query = vec![
        (
            "sysparm_query".into(),
            "user_name=javascript:gs.getUserName()".into(),
        ),
        ("sysparm_limit".into(), "2".into()),
    ];
    let resp = client.get("/api/now/table/sys_user", &query)?;
    if resp["result"].as_array().is_some_and(|rows| rows.len() > 1) {
        // HTTP 200 with a body that does not answer the question asked. Reported
        // as an instance error carrying no status, because there is no HTTP error
        // status here to report and `status_code: 200` on a failure is a lie an
        // agent will branch on.
        return Err(Error::Instance {
            message: "the gs.getUserName() query was not evaluated, so the rows returned are \
                      arbitrary users rather than the caller"
                .into(),
            detail: Some(
                "scripted query evaluation appears to be disabled on this instance; \
                 `sn ping` reports the authenticated identity without one"
                    .into(),
            ),
        });
    }
    Ok(resp)
}
