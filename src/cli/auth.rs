use crate::cli::GlobalFlags;
use crate::cli::kernel::{build_client, build_profile, write_response};
use crate::client::Client;
use crate::config::{
    AuthMethod, OAuthGrant, ResolvedProfile, clear_oauth_tokens, config_path, load_config_from,
    now_unix, resolve_profile_name, save_oauth_tokens,
};
use crate::error::{Error, Result};
use crate::oauth;
use crate::observability::log_note;
use serde_json::{Value, json};

/// Pure session command: run the OAuth flow for an already-configured OAuth
/// profile and cache the tokens. Configuration lives in `sn profile add` /
/// `sn init`; this touches only the token cache.
pub fn login(global: &GlobalFlags) -> Result<()> {
    let config = load_config_from(&config_path()?)?;
    let name = resolve_profile_name(global.profile.as_deref(), &config)?;
    let grant = match config.profiles.get(&name) {
        Some(pc) if matches!(pc.auth, AuthMethod::Oauth) && pc.oauth.is_some() => {
            pc.oauth.as_ref().map(|o| o.grant).unwrap_or_default()
        }
        _ => {
            return Err(Error::Usage(format!(
                "profile '{name}' does not use oauth; run `sn init`"
            )));
        }
    };

    let (profile, user) = complete_oauth_login(global, &name, grant)?;
    let out = json!({
        "ok": true,
        "profile": name,
        "instance": profile.instance,
        "auth": "oauth",
        "grant": grant.as_str(),
        "user": user,
    });
    write_response(global, &out)
}

/// Run the OAuth flow for an already-persisted profile named `name`, cache the
/// resulting tokens, and verify them against the instance. Returns the resolved
/// profile and the authenticated user_name (if the verify call surfaced one).
/// Shared by `sn profile login` and `sn init`'s OAuth branch.
pub(crate) fn complete_oauth_login(
    global: &GlobalFlags,
    name: &str,
    grant: OAuthGrant,
) -> Result<(ResolvedProfile, Option<String>)> {
    // Scope resolution to this specific profile, independent of which profile
    // happens to be the global default.
    let mut scoped = global.clone();
    scoped.profile = Some(name.to_string());

    let profile = build_profile(&scoped)?;
    let tokens = match grant {
        OAuthGrant::AuthorizationCode => oauth::login_authorization_code(&profile, scoped.timeout)?,
        OAuthGrant::ClientCredentials => {
            let client = oauth::build_token_client(&profile, scoped.timeout)?;
            let o = profile
                .oauth
                .as_ref()
                .ok_or_else(|| Error::Config("oauth config missing after save".into()))?;
            oauth::client_credentials(&client, o)?
        }
    };
    save_oauth_tokens(name, &tokens)?;

    // Verify the freshly-issued token against the instance.
    let profile = build_profile(&scoped)?;
    let client = build_client(&profile, scoped.timeout)?;
    let user = whoami(&client)?;
    Ok((profile, user))
}

/// Undocumented, and the instance's own answer to "who is this request from" —
/// it names the caller directly instead of asking a table to filter itself down
/// to them. Present on current releases; absent (404) on old ones, which is what
/// the `sys_user` fallback below is for.
pub(crate) const CURRENT_USER_PATH: &str = "/api/now/ui/user/current_user";

/// Value of [`Identity::source`] when the answer came from [`CURRENT_USER_PATH`].
pub(crate) const SOURCE_CURRENT_USER: &str = "ui/user/current_user";
/// Value of [`Identity::source`] when the answer came from the `sys_user` fallback.
pub(crate) const SOURCE_SYS_USER: &str = "sys_user";

/// Who the instance says the caller is. `source` records which endpoint said so,
/// because the two are not equally trustworthy and a caller auditing an identity
/// needs to know which one answered.
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub user_name: String,
    pub sys_id: Option<String>,
    pub display_name: Option<String>,
    pub source: &'static str,
}

/// Ask the instance who the current credentials actually authenticate as, and
/// in doing so prove that they authenticate at all.
///
/// Primary source is [`CURRENT_USER_PATH`], which names the caller server-side
/// with no query to misparse and no `sys_user` read ACL to satisfy. The
/// `gs.getUserName()` table read stays on as the fallback because that endpoint
/// is undocumented, so the fallback is load-bearing rather than decorative.
///
/// A successful read with no user back is not an error — the credentials worked,
/// the instance just didn't name them — so the identity is optional. What is
/// never acceptable is naming the *wrong* user: every path here either produces
/// an identity it can defend or produces `None`.
pub(crate) fn identify(client: &Client) -> Result<Option<Identity>> {
    match identify_via_current_user(client)? {
        Some(id) => Ok(Some(id)),
        None => identify_via_sys_user(client),
    }
}

/// Probe [`CURRENT_USER_PATH`] on its own, without the `sys_user` fallback.
///
/// `Ok(None)` means *this instance cannot answer here* — the endpoint is absent
/// (404) or returned a 200 that names nobody — and the caller should fall back.
/// It never means "no user". Errors that are verdicts rather than absences
/// propagate: 401/403 is a real auth rejection and must stay exit 4; a 5xx is an
/// instance fault and a second endpoint is no more likely to be honest about
/// identity than the first; a 200 carrying HTML (a session redirect) surfaces as
/// a transport-level parse error, and a fallback would be redirected the same
/// way, so retrying only turns one clear failure into two.
///
/// Split out from [`identify`] because `sn user me` wants exactly this rung: the
/// `user_sys_id` it returns lets that command read the caller's record directly,
/// with no scripted query in the request at all.
pub(crate) fn identify_via_current_user(client: &Client) -> Result<Option<Identity>> {
    match client.get(CURRENT_USER_PATH, &[]) {
        Ok(v) => match parse_current_user(&v) {
            Some(id) => Ok(Some(id)),
            // 200 with a body that doesn't name a user means this is not the
            // endpoint we think it is (a proxy's JSON, a stub, a future shape
            // change). Not a reason to invent a name — hand off to the fallback.
            None => {
                log_note("current_user did not name a user; falling back to sys_user");
                Ok(None)
            }
        },
        Err(Error::Api { status: 404, .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Back-compat wrapper: the authenticated user's name, or `None` when the
/// instance authenticated the credentials without naming anyone.
pub(crate) fn whoami(client: &Client) -> Result<Option<String>> {
    Ok(identify(client)?.map(|i| i.user_name))
}

fn parse_current_user(v: &Value) -> Option<Identity> {
    let r = v.get("result")?;
    Some(Identity {
        user_name: non_empty(r.get("user_name"))?,
        sys_id: non_empty(r.get("user_sys_id")),
        display_name: non_empty(r.get("user_display_name")),
        source: SOURCE_CURRENT_USER,
    })
}

/// A trimmed string value, or `None` for absent / non-string / blank.
///
/// Every identity field goes through this. A whitespace-only name is not a name,
/// and treating one as real is how a report of *who you are* becomes a
/// fabrication rather than an absence.
pub(crate) fn non_empty(v: Option<&Value>) -> Option<String> {
    match v.and_then(Value::as_str).map(str::trim) {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

/// Fallback identity read: `sys_user` filtered by a server-side
/// `gs.getUserName()` scripted query.
///
/// The trap this has to survive is that the filter can vanish. A bare
/// `sysparm_limit=1` read of `sys_user` returns whichever row happens to sort
/// first — an arbitrary account, not the caller — and ServiceNow **silently
/// drops** a query term it cannot evaluate (an instance with scripted-query
/// evaluation disabled, a term that fails to parse). The request still returns
/// 200 with rows, so the failure is indistinguishable from success by status
/// alone, and it fails *open*: a plausible wrong name.
///
/// Hence `sysparm_limit=2` rather than 1. `user_name` is unique, so an evaluated
/// filter can match at most one row; a second row proves the term was dropped
/// and the rows are arbitrary. That case reports nothing rather than a name.
/// (An instance holding exactly one user total would defeat the check — no real
/// instance does; `admin`, `guest` and the system accounts ship with it.)
fn identify_via_sys_user(client: &Client) -> Result<Option<Identity>> {
    let v = client.get(
        "/api/now/table/sys_user",
        &[
            (
                "sysparm_query".into(),
                "user_name=javascript:gs.getUserName()".into(),
            ),
            ("sysparm_fields".into(), "user_name,sys_id,name".into()),
            ("sysparm_limit".into(), "2".into()),
        ],
    )?;
    let rows = v["result"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if rows.len() > 1 {
        log_note(
            "sys_user identity query returned multiple rows; the gs.getUserName() term was \
             dropped by the instance, so the rows are arbitrary — reporting no user",
        );
        return Ok(None);
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let Some(user_name) = non_empty(row.get("user_name")) else {
        return Ok(None);
    };
    Ok(Some(Identity {
        user_name,
        sys_id: non_empty(row.get("sys_id")),
        display_name: non_empty(row.get("name")),
        source: SOURCE_SYS_USER,
    }))
}

pub fn logout(global: &GlobalFlags) -> Result<()> {
    let config = load_config_from(&config_path()?)?;
    let name = resolve_profile_name(global.profile.as_deref(), &config)?;
    clear_oauth_tokens(&name)?;
    let out = json!({"ok": true, "profile": name, "loggedOut": true});
    write_response(global, &out)
}

pub fn status(global: &GlobalFlags) -> Result<()> {
    let profile = build_profile(global)?;
    let mut out = json!({
        "profile": profile.name,
        "instance": profile.instance,
        "auth": if matches!(profile.auth_method, AuthMethod::Oauth) { "oauth" } else { "basic" },
    });
    match profile.auth_method {
        AuthMethod::Basic => {
            out["username"] = json!(profile.username);
        }
        AuthMethod::Oauth => {
            if let Some(o) = &profile.oauth {
                out["grant"] = json!(o.grant.as_str());
                out["clientId"] = json!(o.client_id);
                out["redirectUri"] = json!(o.redirect_uri);
                out["loggedIn"] = json!(o.tokens.is_some());
                if let Some(t) = &o.tokens {
                    out["hasRefreshToken"] = json!(t.refresh_token.is_some());
                    if let Some(exp) = t.expires_at {
                        out["expiresAt"] = json!(exp);
                        out["expiresInSecs"] = json!(exp as i64 - now_unix() as i64);
                        out["expired"] = json!(t.is_expired(0));
                    }
                }
            }
        }
    }
    write_response(global, &out)
}

pub fn refresh(global: &GlobalFlags) -> Result<()> {
    let profile = build_profile(global)?;
    if !matches!(profile.auth_method, AuthMethod::Oauth) {
        return Err(Error::Usage(format!(
            "profile '{}' does not use oauth",
            profile.name
        )));
    }
    let tokens = oauth::force_refresh(&profile, global.timeout)?;
    let out = json!({
        "ok": true,
        "profile": profile.name,
        "refreshed": true,
        "expiresAt": tokens.expires_at,
    });
    write_response(global, &out)
}
