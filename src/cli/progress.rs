use crate::cli::table::{build_client, build_profile, unwrap_or_raw};
use crate::cli::{GlobalFlags, OutputMode};
use crate::client::Client;
use crate::error::{Error, Result};
use serde_json::Value;

#[derive(clap::Args, Debug)]
pub struct ProgressArgs {
    /// Progress ID returned by app/updateset/atf operations.
    pub progress_id: String,
}

pub fn run(global: &GlobalFlags, args: ProgressArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/sn_cicd/progress/{}", args.progress_id);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

/// Shared tail of every async CICD command (`app`, `updateset`, `atf`): if
/// `--wait` was passed and the response carries a progress link, poll it to
/// completion (bounded by `--wait-timeout`) and emit the final progress
/// result; otherwise emit the initial response. Routing the final emission
/// through `write_response` keeps `--output table` working under `--wait`.
pub(crate) fn finish_cicd(
    global: &GlobalFlags,
    client: &Client,
    out: Value,
    wait: bool,
    wait_timeout: Option<u64>,
) -> Result<()> {
    if wait {
        // Callers hand `out` over already shaped by `--output`, so under `raw`
        // the progress link sits one level down inside the untouched envelope.
        // Looking only at the top level made `--wait` a no-op there: no link
        // found, no polling, initial response emitted as if it were final.
        let body = out.get("result").unwrap_or(&out);
        if let Some(progress_id) = body
            .get("links")
            .and_then(|l| l.get("progress"))
            .and_then(|p| p.get("id"))
            .and_then(|id| id.as_str())
        {
            let final_result = wait_for_completion(client, progress_id, global, wait_timeout)?;
            return crate::cli::table::write_response(global, &final_result);
        }
    }
    crate::cli::table::write_response(global, &out)
}

/// Poll `GET /api/sn_cicd/progress/{progress_id}` in a loop until the operation
/// reaches a terminal state (Successful, Failed, or Cancelled) and return the
/// final result value, shaped by `global.output` like any other response.
/// `wait_timeout` bounds the total wait in seconds; `None` waits indefinitely.
///
/// Status codes:
/// - "0" = Pending, "1" = Running, "2" = Successful, "3" = Failed, "4" = Cancelled
pub(crate) fn wait_for_completion(
    client: &Client,
    progress_id: &str,
    global: &GlobalFlags,
    wait_timeout: Option<u64>,
) -> Result<Value> {
    let path = format!("/api/sn_cicd/progress/{}", progress_id);
    let deadline =
        wait_timeout.map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    loop {
        let resp = client.get(&path, &[])?;
        // Poll decisions always read the unwrapped body — `status` lives inside
        // the `result` envelope — but what the caller finally sees is theirs to
        // choose, so `--output raw` still hands back the envelope it asked for.
        let result = unwrap_or_raw(resp.clone(), OutputMode::Default);

        let status = result.get("status").and_then(|s| s.as_str()).unwrap_or("1");

        match status {
            "2" => return Ok(unwrap_or_raw(resp, global.output)),
            "3" | "4" => {
                let msg = result
                    .get("status_message")
                    .and_then(|s| s.as_str())
                    .unwrap_or("operation failed");
                // The HTTP call succeeded; the *operation* failed. There is no
                // HTTP status to report, and the envelope must omit the key
                // rather than invent one.
                return Err(Error::Api {
                    status: crate::error::NO_HTTP_STATUS,
                    message: msg.to_string(),
                    detail: result
                        .get("status_detail")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                    transaction_id: None,
                    sn_error: Some(result),
                });
            }
            _ => {
                if global.verbose > 0 {
                    // ServiceNow sends percent_complete as a JSON string ("100") on
                    // some operations and a number (0) on others, so `.as_str()`
                    // alone silently printed nothing half the time.
                    let pct = result.get("percent_complete").and_then(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .or_else(|| v.as_f64().map(|n| n.to_string()))
                    });
                    if let Some(pct) = pct {
                        eprintln!("sn: progress {pct}%");
                    }
                }
                if let Some(d) = deadline {
                    if std::time::Instant::now() >= d {
                        return Err(Error::Transport(format!(
                            "--wait timed out after {}s; operation still running, poll with `sn progress {}`",
                            wait_timeout.unwrap_or_default(),
                            progress_id
                        )));
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}
