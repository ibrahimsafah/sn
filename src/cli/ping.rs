use crate::cli::GlobalFlags;
use crate::cli::auth::{Identity, identify, non_empty};
use crate::cli::kernel::{build_client, build_profile, write_response};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::observability;
use serde_json::{Value, json};
use std::time::Instant;

/// Undocumented. The only endpoint that reports the caller's *privilege* and
/// impersonation state alongside their name, and it does so in one request that
/// simultaneously proves the credentials authenticate — exactly what a ping is
/// for. Absent (404) on instances that predate it, hence the fallback chain.
const SESSION_PATH: &str = "/api/now/sg/impersonation/session";

/// [`crate::cli::auth::Identity::source`] value for [`SESSION_PATH`].
const SOURCE_SESSION: &str = "sg/impersonation/session";
/// [`crate::cli::auth::Identity::source`] value when no endpoint named the
/// caller and the reported username is the one the profile was configured with.
const SOURCE_PROFILE: &str = "profile";

/// What [`SESSION_PATH`] tells us, minus the credential it also hands out.
struct Session {
    user_name: String,
    original_user: Option<String>,
    admin: Option<bool>,
    can_impersonate: Option<bool>,
}

pub fn run(global: &GlobalFlags) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let started = Instant::now();
    let session = probe_session(&client)?;
    let latency_ms = started.elapsed().as_millis() as u64;

    let mut admin = Value::Null;
    let mut can_impersonate = Value::Null;
    let mut impersonating = Value::Null;
    let mut original_user = Value::Null;

    let identity = match session {
        Some(s) => {
            admin = s.admin.map_or(Value::Null, Value::from);
            can_impersonate = s.can_impersonate.map_or(Value::Null, Value::from);
            // Only meaningful when the endpoint reported both names, and
            // `probe_session` guarantees each is a real (non-blank) name — so
            // `impersonating` is true only when two present names differ. A
            // missing or blank OriginalUser stays null: unknown, not "no".
            if let Some(orig) = &s.original_user {
                impersonating = Value::from(orig != &s.user_name);
                original_user = Value::from(orig.clone());
            }
            // This endpoint names the caller but does not identify the record:
            // no sys_id, no display name. `identity_source` in the output says
            // which endpoint answered, so a null here is legible rather than
            // ambiguous.
            Some(Identity {
                user_name: s.user_name,
                sys_id: None,
                display_name: None,
                source: SOURCE_SESSION,
            })
        }
        // The session endpoint is missing on this instance. `identify` both
        // names the caller and re-proves the credentials, so auth is still
        // verified before ping reports `ok`.
        None => identify(&client)?,
    };

    // Report who the *instance* says we are, in preference to who the profile
    // claims. When they disagree the profile is the one that is wrong, and a
    // ping that echoes the config back has verified nothing about identity.
    // The configured username is the last resort, for an instance that
    // authenticated the credentials without naming them.
    let (username, identity_source, user_sys_id, user_display_name) = match identity {
        Some(i) => (
            i.user_name,
            Value::from(i.source),
            i.sys_id.map_or(Value::Null, Value::from),
            i.display_name.map_or(Value::Null, Value::from),
        ),
        None if !profile.username.is_empty() => (
            profile.username.clone(),
            Value::from(SOURCE_PROFILE),
            Value::Null,
            Value::Null,
        ),
        None => (String::new(), Value::Null, Value::Null, Value::Null),
    };

    let (build_name, build_tag) = match client.get(
        "/api/now/table/sys_properties",
        &[
            (
                "sysparm_query".into(),
                "name=glide.buildname^ORname=glide.buildtag".into(),
            ),
            ("sysparm_fields".into(), "name,value".into()),
            ("sysparm_limit".into(), "2".into()),
        ],
    ) {
        Ok(v) => extract_build(&v),
        Err(_) => (Value::Null, Value::Null),
    };

    let out = json!({
        "ok": true,
        "profile": profile.name,
        "instance": profile.instance,
        "username": username,
        "latency_ms": latency_ms,
        "build_name": build_name,
        "build_tag": build_tag,
        "identity_source": identity_source,
        "user_sys_id": user_sys_id,
        "user_display_name": user_display_name,
        "admin": admin,
        "can_impersonate": can_impersonate,
        "impersonating": impersonating,
        "original_user": original_user,
    });

    write_response(global, &out)
}

/// Ask [`SESSION_PATH`] who we are. `Ok(None)` means "this instance can't
/// answer" and the caller should fall back; it never means "nobody".
fn probe_session(client: &Client) -> Result<Option<Session>> {
    let v = match get_without_body_log(client, SESSION_PATH) {
        Ok(v) => v,
        // Only a *missing* endpoint earns the fallback. 401/403 is a real auth
        // verdict and has to stay exit 4 — a ping that shopped around for an
        // endpoint willing to answer would report `ok` for dead credentials.
        // A 5xx is an instance fault, not evidence the path is unsupported.
        Err(Error::Api { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };

    // This response is a bare object, not the `{"result": …}` envelope almost
    // every other ServiceNow endpoint uses. Anything else — including a future
    // release that wraps it — fails the `CurrentUser` check below and falls back
    // rather than reporting a name it cannot locate.
    let Some(user_name) = non_empty(v.get("CurrentUser")) else {
        observability::log_note(
            "impersonation session response did not name a current user; falling back",
        );
        return Ok(None);
    };

    // Everything here is copied field by field. `SessionToken` — a live CSRF
    // credential for this session — is deliberately not among them, and must
    // never be: it is worthless to a ping and dangerous in a transcript.
    Ok(Some(Session {
        user_name,
        // Same `non_empty` guard as `CurrentUser`, and for the same reason: a
        // blank `OriginalUser` taken at face value differs from the current user
        // and would be reported as an impersonated session that isn't happening.
        // An impersonation claim is an accusation; it needs two real names.
        original_user: non_empty(v.get("OriginalUser")),
        admin: v.get("admin").and_then(Value::as_bool),
        can_impersonate: v.get("CanImpersonate").and_then(Value::as_bool),
    }))
}

/// `GET path` with `-ddd` response-body logging suppressed for this one request.
///
/// [`SESSION_PATH`]'s response carries `SessionToken`, and `Client` logs response
/// bodies verbatim at `-ddd`. Stripping the token from stdout is not enough if
/// the debug log prints it anyway — so the body log is dropped wholesale here.
/// Request line, status and headers still log, which is what `-ddd` is actually
/// used for on this call. Verbosity is process-global but the client is
/// blocking and single-threaded, so the window is this request and no other.
fn get_without_body_log(client: &Client, path: &str) -> Result<Value> {
    struct Restore(u8);
    impl Drop for Restore {
        fn drop(&mut self) {
            observability::set_level(self.0);
        }
    }
    let saved = observability::level();
    let _restore = Restore(saved);
    if saved >= 3 {
        observability::set_level(2);
    }
    client.get(path, &[])
}

fn extract_build(v: &Value) -> (Value, Value) {
    let mut name = Value::Null;
    let mut tag = Value::Null;
    let rows = v
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    for row in rows {
        let prop = row.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let val = row.get("value").cloned().unwrap_or(Value::Null);
        match prop {
            "glide.buildname" => name = val,
            "glide.buildtag" => tag = val,
            _ => {}
        }
    }
    (name, tag)
}
