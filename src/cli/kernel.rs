//! The command kernel: the ingress ([`connect`]) and egress ([`emit`]) every
//! leaf handler shares, plus the guards ([`confirm_destructive`]) between them.
//! A separate module rather than a corner of `cli/table.rs` so the kernel is
//! structural, not opt-in: while these lived in the `sn table` module, the
//! `--output table` bypasses accumulated precisely because reaching around
//! them was as easy as importing something else.

use crate::cli::{GlobalFlags, OutputMode};
use crate::client::{Auth, Client};
use crate::config::{
    AuthMethod, ProfileResolverInputs, ResolvedProfile, config_path, credentials_path,
    load_config_from, load_credentials_from, resolve_profile,
};
use crate::error::{Error, Result};
use crate::output::{Format, ResolvedFormat};
use is_terminal::IsTerminal;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::time::Duration;

/// [`build_profile`] → [`build_client`] fused: the preamble of every command
/// that talks to the instance and needs nothing from the profile but a client.
/// Call sites that read the resolved profile itself (`open` for the instance
/// URL, `watch` for its transport check, the OAuth flows, `ping`) keep the
/// two-step form.
pub(crate) fn connect(global: &GlobalFlags) -> Result<Client> {
    let profile = build_profile(global)?;
    build_client(&profile, global.timeout)
}

pub(crate) fn build_profile(global: &GlobalFlags) -> Result<ResolvedProfile> {
    let config = load_config_from(&config_path()?)?;
    let creds = load_credentials_from(&credentials_path()?)?;
    let env_proxy = std::env::var("SN_PROXY").ok();
    let env_no_proxy = std::env::var("SN_NO_PROXY").ok();
    let env_insecure = std::env::var("SN_INSECURE").ok();
    let env_ca_cert = std::env::var("SN_CA_CERT").ok();
    let env_proxy_ca_cert = std::env::var("SN_PROXY_CA_CERT").ok();
    resolve_profile(ProfileResolverInputs {
        cli_profile: global.profile.as_deref(),
        cli_proxy: global.proxy.as_deref(),
        env_proxy: env_proxy.as_deref(),
        cli_no_proxy: global.no_proxy,
        env_no_proxy: env_no_proxy.as_deref(),
        cli_insecure: global.insecure,
        env_insecure: env_insecure.as_deref(),
        cli_ca_cert: global.ca_cert.as_deref(),
        env_ca_cert: env_ca_cert.as_deref(),
        cli_proxy_ca_cert: global.proxy_ca_cert.as_deref(),
        env_proxy_ca_cert: env_proxy_ca_cert.as_deref(),
        config: &config,
        credentials: &creds,
    })
}

pub(crate) fn build_client(profile: &ResolvedProfile, timeout: Option<u64>) -> Result<Client> {
    build_client_with_headers(profile, timeout, HeaderMap::new())
}

/// [`build_client`] plus caller-supplied request headers (`sn raw --header`),
/// which the client applies after auth and `Content-Type` so they win. A
/// separate entry point rather than a third parameter on `build_client`: only
/// `raw` has headers to pass, and every other call site stays untouched.
pub(crate) fn build_client_with_headers(
    profile: &ResolvedProfile,
    timeout: Option<u64>,
    extra_headers: HeaderMap,
) -> Result<Client> {
    let mut b = Client::builder()
        .extra_headers(extra_headers)
        .proxy(profile.proxy.clone())
        .no_proxy(profile.no_proxy.clone())
        .insecure(profile.insecure)
        .ca_cert(profile.ca_cert.clone())
        .proxy_ca_cert(profile.proxy_ca_cert.clone())
        .proxy_auth(
            profile.proxy_username.clone(),
            profile.proxy_password.clone(),
        );
    // OAuth profiles attach a bearer token, refreshing (or minting, for
    // client-credentials) it transparently and persisting any new tokens.
    // Basic profiles fall through to the builder's default username/password.
    if matches!(profile.auth_method, AuthMethod::Oauth) {
        let token = crate::oauth::ensure_access_token(profile, timeout)?;
        b = b.auth(Auth::Bearer { token });
    }
    if let Some(secs) = timeout {
        b = b.timeout(Duration::from_secs(secs));
    }
    b.build(profile)
}

pub(crate) fn bool_opt(b: bool) -> Option<bool> {
    if b { Some(true) } else { None }
}

/// Private on purpose: `write_response` is the single place a command's final
/// value reaches stdout, and it is the only thing that routes `--output table`.
/// Every module that reached for this instead silently ignored that flag, so
/// keeping it unreachable makes the convention a compile error to break.
fn format_from_flags(g: &GlobalFlags) -> ResolvedFormat {
    if g.pretty {
        Format::Pretty.resolve()
    } else if g.compact {
        Format::Compact.resolve()
    } else {
        Format::Auto.resolve()
    }
}

/// [`unwrap_or_raw`] → [`write_response`] fused: the postamble of every command
/// whose final value is the response itself. Takes `resp` by value so the
/// envelope is moved out of, never cloned — a call site that still holds the
/// response afterwards is reshaping it and belongs on the underlying pair.
pub(crate) fn emit(global: &GlobalFlags, resp: Value) -> Result<()> {
    let out = unwrap_or_raw(resp, global.output);
    write_response(global, &out)
}

/// Unwrap the `{"result": ...}` envelope unless `--output raw` asked to keep it.
///
/// Takes `v` by value and moves the subtree out: this runs on every command's
/// response, and cloning it allocated once per string and map in the tree.
pub(crate) fn unwrap_or_raw(v: Value, mode: OutputMode) -> Value {
    match mode {
        OutputMode::Raw => v,
        OutputMode::Default | OutputMode::Table => match v {
            Value::Object(mut m) => match m.remove("result") {
                Some(r) => r,
                None => Value::Object(m),
            },
            other => other,
        },
    }
}

/// Take one key out of a JSON object by value, for the same reason
/// [`unwrap_or_raw`] moves: the caller owns the response and is about to drop
/// the rest of it. `None` for a missing key or a non-object.
pub(crate) fn take_field(v: Value, key: &str) -> Option<Value> {
    match v {
        Value::Object(mut m) => m.remove(key),
        _ => None,
    }
}

/// Write a response value to stdout in whichever shape the global `--output` flag selects:
/// JSON (`default`/`raw`) or human-readable columnar (`table`). Centralizes the OutputMode
/// dispatch so each command's call site stays a one-liner.
pub(crate) fn write_response(global: &GlobalFlags, value: &Value) -> Result<()> {
    if matches!(global.output, OutputMode::Table) {
        crate::output_table::write_table(value)
    } else {
        crate::output::write_value(value, format_from_flags(global))
    }
}

/// Gate a destructive operation behind a confirmation, shared by every command
/// that removes or undoes instance state. With `--yes` it is a no-op; otherwise
/// it refuses to proceed on a non-interactive stdin (exit 1) and, on a TTY,
/// prompts `{Verb} {what}? [y/N]:` and aborts unless the answer is affirmative.
///
/// `verb` is a lowercase imperative — `delete`, `remove`, `empty`, `back out`,
/// `roll back` — and appears verbatim in the non-TTY refusal, so the message
/// names the operation the caller actually asked for. It was hardcoded to
/// "delete" until 0.12.0, which is why the commands that undo rather than
/// delete never got a gate: the only phrasing on offer was a lie. Match it to
/// the command's own name (`sn catalog cart-remove` refuses with "remove", not
/// "delete"), or a caller grepping stderr for the operation they ran finds a
/// word that appears nowhere in their argv.
///
/// `what` names the target — `incident/abc123`, `relation r1 on
/// cmdb_ci_server/x`, `profile prod and its stored credentials` — and is part
/// of the refusal too, not just the prompt: the non-TTY path is the one a
/// script reads, and "requires --yes" alone says which flag to add but not what
/// it would have been added to.
pub(crate) fn confirm_destructive(yes: bool, verb: &str, what: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Usage(format!(
            "{verb} {what} requires --yes when stdin is not a terminal"
        )));
    }
    let mut chars = verb.chars();
    let verb = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    eprint!("{verb} {what}? [y/N]: ");
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .map_err(|e| Error::Usage(format!("read stdin: {e}")))?;
    if !matches!(s.trim(), "y" | "Y" | "yes" | "YES") {
        return Err(Error::Usage("aborted".into()));
    }
    Ok(())
}

/// [`confirm_destructive`] for the `delete` verb — the shape every `delete`
/// command wants, and the one `attachment`/`cmdb` still call by this name.
pub(crate) fn confirm_delete(yes: bool, what: &str) -> Result<()> {
    confirm_destructive(yes, "delete", what)
}
