//! OAuth 2.0 support for SSO-fronted ServiceNow instances.
//!
//! Two grant types are implemented:
//!   * **Authorization Code** (with PKCE + a loopback redirect) — the
//!     interactive flow used when ServiceNow delegates login to an external
//!     IdP (Okta, Azure AD, ADFS, …). A human's password lives in the IdP, not
//!     in ServiceNow, so HTTP Basic / the OAuth password grant cannot work;
//!     this browser flow is the supported path.
//!   * **Client Credentials** — a non-interactive service-to-service flow for
//!     automation/CI.
//!
//! ServiceNow OAuth endpoints (relative to the instance base URL):
//!   * `GET  /oauth_auth.do`  — authorization endpoint (issues the `code`)
//!   * `POST /oauth_token.do` — token endpoint (code/refresh/client_credentials)

use crate::client::{Auth, Client};
use crate::config::{self, OAuthGrant, ResolvedOauth, ResolvedProfile, TokenSet};
use crate::error::{Error, Result};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Refresh an access token this many seconds before it actually expires, so an
/// in-flight request never races the expiry boundary.
const REFRESH_SKEW_SECS: u64 = 60;

/// How long `sn profile login` waits for the SSO redirect before giving up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

const URL_SAFE: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ---------------------------------------------------------------------------
// PKCE + state
// ---------------------------------------------------------------------------

/// A PKCE pair: the secret `verifier` (kept by the CLI) and the public
/// `challenge` (sent to the authorization endpoint).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| Error::Transport(format!("rng failure: {e}")))?;
    Ok(buf)
}

fn random_urlsafe(n: usize) -> Result<String> {
    Ok(URL_SAFE.encode(random_bytes(n)?))
}

/// Generate a PKCE verifier (43-char base64url) and its S256 challenge.
pub fn generate_pkce() -> Result<Pkce> {
    let verifier = random_urlsafe(32)?;
    Ok(Pkce {
        challenge: pkce_challenge(&verifier),
        verifier,
    })
}

/// Compute the S256 code challenge for a given verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE.encode(hasher.finalize())
}

/// A random anti-CSRF `state` token.
pub fn random_state() -> Result<String> {
    random_urlsafe(16)
}

// ---------------------------------------------------------------------------
// Authorization URL
// ---------------------------------------------------------------------------

/// Build the authorization-endpoint URL the user's browser is sent to.
pub fn authorize_url(
    base_url: &str,
    o: &ResolvedOauth,
    state: &str,
    pkce_challenge: Option<&str>,
) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("{base_url}{}", o.auth_path))
        .map_err(|e| Error::Config(format!("invalid authorize URL: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &o.client_id);
        q.append_pair("redirect_uri", &o.redirect_uri);
        q.append_pair("state", state);
        if let Some(c) = pkce_challenge {
            q.append_pair("code_challenge", c);
            q.append_pair("code_challenge_method", "S256");
        }
    }
    Ok(url.to_string())
}

// ---------------------------------------------------------------------------
// Loopback redirect listener (RFC 8252)
// ---------------------------------------------------------------------------

/// Result of parsing one inbound HTTP request to the loopback server.
enum Inbound {
    /// The redirect we were waiting for, carrying `?code=…&state=…`.
    Callback { code: String, state: String },
    /// The IdP/ServiceNow returned `?error=…` instead of a code.
    AuthError(String),
    /// Some other path (e.g. `/favicon.ico`) — ignore and keep waiting.
    Ignored,
}

/// Bind a transient HTTP server to the loopback redirect URI, wait for the
/// browser redirect, validate `state`, and return the authorization `code`.
pub fn run_loopback(redirect_uri: &str, expected_state: &str) -> Result<String> {
    let url = reqwest::Url::parse(redirect_uri)
        .map_err(|e| Error::Config(format!("invalid redirect_uri '{redirect_uri}': {e}")))?;
    let port = url.port().ok_or_else(|| {
        Error::Config(format!("redirect_uri '{redirect_uri}' must include a port"))
    })?;
    let expected_path = url.path().to_string();

    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        Error::Config(format!(
            "cannot bind loopback redirect on 127.0.0.1:{port}: {e}. Is another `sn profile login` running, or is the port taken?"
        ))
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::Transport(format!("listener config: {e}")))?;

    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => match handle_stream(stream, &expected_path)? {
                Inbound::Callback { code, state } => {
                    if state != expected_state {
                        return Err(Error::Auth {
                            status: 401,
                            message: "OAuth state mismatch — possible CSRF; aborting".into(),
                            transaction_id: None,
                        });
                    }
                    return Ok(code);
                }
                Inbound::AuthError(e) => {
                    return Err(Error::Auth {
                        status: 401,
                        message: format!("authorization failed: {e}"),
                        transaction_id: None,
                    });
                }
                Inbound::Ignored => continue,
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Error::Transport(
                        "timed out waiting for the OAuth redirect (5 min)".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(Error::Transport(format!("accept loopback: {e}"))),
        }
    }
}

fn handle_stream(stream: TcpStream, expected_path: &str) -> Result<Inbound> {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| Error::Transport(format!("clone stream: {e}")))?,
    );
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| Error::Transport(format!("read redirect request: {e}")))?;

    // Request line: `GET /callback?code=…&state=… HTTP/1.1`
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let inbound = classify_target(target, expected_path);

    let body = match &inbound {
        Inbound::Callback { .. } => {
            "<html><body><h2>Login complete.</h2><p>You can close this tab and return to the terminal.</p></body></html>"
        }
        Inbound::AuthError(_) => {
            "<html><body><h2>Login failed.</h2><p>Return to the terminal for details.</p></body></html>"
        }
        Inbound::Ignored => "<html><body>Not found.</body></html>",
    };
    let status = if matches!(inbound, Inbound::Ignored) {
        "404 Not Found"
    } else {
        "200 OK"
    };
    let mut stream = stream;
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    Ok(inbound)
}

/// Parse the HTTP request target into an `Inbound`. Split out for unit testing.
fn classify_target(target: &str, expected_path: &str) -> Inbound {
    // Prepend a dummy origin so the relative target parses with a query string.
    let Ok(parsed) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
        return Inbound::Ignored;
    };
    if parsed.path() != expected_path {
        return Inbound::Ignored;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" if error.is_some() => {
                error = Some(format!("{}: {v}", error.take().unwrap()));
            }
            _ => {}
        }
    }
    if let Some(e) = error {
        return Inbound::AuthError(e);
    }
    match (code, state) {
        (Some(code), Some(state)) => Inbound::Callback { code, state },
        _ => Inbound::AuthError("redirect missing code/state".into()),
    }
}

// ---------------------------------------------------------------------------
// Token endpoint exchanges
// ---------------------------------------------------------------------------

/// Build an unauthenticated `Client` (proxy/TLS settings preserved) for talking
/// to the OAuth token endpoint.
pub fn build_token_client(profile: &ResolvedProfile, timeout: Option<u64>) -> Result<Client> {
    let mut b = Client::builder()
        .proxy(profile.proxy.clone())
        .no_proxy(profile.no_proxy.clone())
        .insecure(profile.insecure)
        .ca_cert(profile.ca_cert.clone())
        .proxy_ca_cert(profile.proxy_ca_cert.clone())
        .proxy_auth(
            profile.proxy_username.clone(),
            profile.proxy_password.clone(),
        )
        .auth(Auth::None);
    if let Some(secs) = timeout {
        b = b.timeout(Duration::from_secs(secs));
    }
    b.build(profile)
}

fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

/// Parse a `/oauth_token.do` JSON response into a `TokenSet`.
fn parse_token_response(v: &Value) -> Result<TokenSet> {
    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Auth {
            status: 401,
            message: format!("token endpoint returned no access_token: {v}"),
            transaction_id: None,
        })?
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let expires_at = v
        .get("expires_in")
        .and_then(value_as_u64)
        .map(|secs| config::now_unix().saturating_add(secs));
    let token_type = v
        .get("token_type")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at,
        token_type,
    })
}

/// Exchange an authorization `code` for tokens.
pub fn exchange_code(
    client: &Client,
    o: &ResolvedOauth,
    code: &str,
    pkce_verifier: Option<&str>,
) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type".into(), "authorization_code".into()),
        ("code".into(), code.to_string()),
        ("redirect_uri".into(), o.redirect_uri.clone()),
        ("client_id".into(), o.client_id.clone()),
    ];
    if let Some(secret) = &o.client_secret {
        form.push(("client_secret".into(), secret.clone()));
    }
    if let Some(v) = pkce_verifier {
        form.push(("code_verifier".into(), v.to_string()));
    }
    parse_token_response(&client.post_form(&o.token_path, &form)?)
}

/// Exchange a refresh token for a fresh access token. The new `TokenSet`
/// inherits the previous refresh token if the response omits one (ServiceNow
/// may or may not rotate it).
pub fn refresh(client: &Client, o: &ResolvedOauth, refresh_token: &str) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh_token.to_string()),
        ("client_id".into(), o.client_id.clone()),
    ];
    if let Some(secret) = &o.client_secret {
        form.push(("client_secret".into(), secret.clone()));
    }
    let mut tokens = parse_token_response(&client.post_form(&o.token_path, &form)?)?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

/// Mint a token via the client-credentials grant (requires a client secret).
pub fn client_credentials(client: &Client, o: &ResolvedOauth) -> Result<TokenSet> {
    let secret = o
        .client_secret
        .as_ref()
        .ok_or_else(|| Error::Config("client_credentials grant requires a client secret".into()))?;
    let form = vec![
        ("grant_type".into(), "client_credentials".into()),
        ("client_id".into(), o.client_id.clone()),
        ("client_secret".into(), secret.clone()),
    ];
    parse_token_response(&client.post_form(&o.token_path, &form)?)
}

// ---------------------------------------------------------------------------
// High-level orchestration
// ---------------------------------------------------------------------------

/// Run the interactive authorization-code (SSO) flow end to end and return the
/// resulting tokens. Opens the user's browser and waits for the redirect.
pub fn login_authorization_code(
    profile: &ResolvedProfile,
    timeout: Option<u64>,
) -> Result<TokenSet> {
    login_authorization_code_with(profile, timeout, |url| {
        eprintln!("Opening browser for SSO login:\n  {url}");
        if webbrowser::open(url).is_err() {
            eprintln!("(could not open a browser automatically — open the URL above manually)");
        }
        Ok(())
    })
}

/// Like [`login_authorization_code`], but with the browser-open step injected so
/// the flow can be exercised end to end in tests without launching a real
/// browser. `open` receives the fully-built authorization URL; the default
/// caller hands it to `webbrowser::open`.
pub fn login_authorization_code_with(
    profile: &ResolvedProfile,
    timeout: Option<u64>,
    open: impl FnOnce(&str) -> Result<()>,
) -> Result<TokenSet> {
    let o = profile
        .oauth
        .as_ref()
        .ok_or_else(|| Error::Config("profile is not configured for oauth".into()))?;
    let client = build_token_client(profile, timeout)?;
    let base = client.base_url().to_string();

    let state = random_state()?;
    let (challenge, verifier) = if o.pkce {
        let p = generate_pkce()?;
        (Some(p.challenge), Some(p.verifier))
    } else {
        (None, None)
    };
    let url = authorize_url(&base, o, &state, challenge.as_deref())?;

    open(&url)?;
    eprintln!("Waiting for the SSO redirect on {} …", o.redirect_uri);

    let code = run_loopback(&o.redirect_uri, &state)?;
    exchange_code(&client, o, &code, verifier.as_deref())
}

/// Return a valid bearer access token for an OAuth profile, refreshing or
/// minting one as needed and persisting any new tokens. This is the chokepoint
/// `build_client` calls before every API request.
///
/// A refresh is **single-flight across processes**. The naive shape — read the
/// cached token, refresh it over the network, then write — is not merely
/// wasteful when N agent invocations wake up together on one expired token: on
/// an instance with refresh-token rotation it is a hard failure. Each of them
/// presents the same refresh token RT0; the first gets RT1 and persists it, and
/// every other one presents an RT0 the IdP has already consumed, gets a 401, and
/// exits 4 telling the user to log in again — seconds after a perfectly good
/// token was minted. So the whole read → refresh → write runs under the config
/// lock, and the staleness check is repeated *inside* it: whoever arrives second
/// re-reads the file, finds the token a sibling just wrote, and returns it
/// without touching the network at all.
///
/// The cost is that the lock is held across an HTTP round trip, which serializes
/// every concurrent invocation of `sn` behind one token fetch. That is the right
/// trade — the alternative is a real authentication failure, and the wait is
/// bounded by the same timeout the request itself has — but it does mean the
/// usual 10s lock timeout is too tight, so this call passes a budget derived
/// from the HTTP timeout instead (see [`refresh_lock_timeout`]).
///
/// The fast path (cached token still valid) takes no lock and does no I/O, so
/// the common case pays nothing for any of this.
pub fn ensure_access_token(profile: &ResolvedProfile, timeout: Option<u64>) -> Result<String> {
    let o = profile
        .oauth
        .as_ref()
        .ok_or_else(|| Error::Config("oauth profile missing oauth config".into()))?;

    if let Some(tok) = &o.tokens
        && !tok.is_expired(REFRESH_SKEW_SECS)
    {
        return Ok(tok.access_token.clone());
    }

    config::with_config_lock_for(refresh_lock_timeout(timeout), || {
        // Re-read under the lock. `o.tokens` is this invocation's snapshot from
        // before it started waiting; the file is the truth now.
        let cached = config::load_oauth_tokens(&profile.name)?;
        if let Some(tok) = &cached
            && !tok.is_expired(REFRESH_SKEW_SECS)
        {
            return Ok(tok.access_token.clone());
        }

        // Likewise prefer the on-disk refresh token: under rotation, ours may
        // already have been spent and replaced by whoever held the lock last.
        let rt = cached
            .as_ref()
            .and_then(|t| t.refresh_token.clone())
            .or_else(|| o.tokens.as_ref().and_then(|t| t.refresh_token.clone()));
        if let Some(rt) = rt {
            let client = build_token_client(profile, timeout)?;
            let fresh = refresh(&client, o, &rt)?;
            config::save_oauth_tokens(&profile.name, &fresh)?;
            return Ok(fresh.access_token);
        }

        // No cached token (or it expired without a refresh token). The
        // client-credentials grant can mint one non-interactively.
        if matches!(o.grant, OAuthGrant::ClientCredentials) {
            let client = build_token_client(profile, timeout)?;
            let fresh = client_credentials(&client, o)?;
            config::save_oauth_tokens(&profile.name, &fresh)?;
            return Ok(fresh.access_token);
        }

        Err(Error::Auth {
            status: 401,
            message: format!(
                "no valid OAuth token for profile '{}'; run `sn profile login`",
                profile.name
            ),
            transaction_id: None,
        })
    })
}

/// The `Client` builder's default request timeout, mirrored from `client.rs`.
/// Only used to size a lock budget, so drift costs a slightly wrong wait rather
/// than a wrong request.
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Extra slack over the HTTP timeout, so a waiter still out-waits a holder that
/// used its entire request budget before giving up.
const REFRESH_LOCK_SLACK: Duration = Duration::from_secs(10);

/// How long to wait for the config lock when a token refresh may be in flight.
///
/// The lock's usual 10s budget assumes the holder is doing a parse and a small
/// write. Here the holder is blocked on the IdP, so the waiter has to be willing
/// to wait out the holder's own request timeout — otherwise a slow IdP turns
/// every parallel invocation into a spurious "timed out waiting for the config
/// lock" (exit 1), which is a worse failure than the one single-flight fixed.
fn refresh_lock_timeout(timeout: Option<u64>) -> Duration {
    Duration::from_secs(timeout.unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS))
        .saturating_add(REFRESH_LOCK_SLACK)
}

/// Force a token refresh now (for `sn profile refresh`), persisting the result.
///
/// Locked like [`ensure_access_token`] so it cannot race a refresh happening
/// underneath it, but with no staleness re-check: "force" means force, and the
/// user typed it.
pub fn force_refresh(profile: &ResolvedProfile, timeout: Option<u64>) -> Result<TokenSet> {
    let o = profile
        .oauth
        .as_ref()
        .ok_or_else(|| Error::Config("profile is not configured for oauth".into()))?;
    config::with_config_lock_for(refresh_lock_timeout(timeout), || {
        let client = build_token_client(profile, timeout)?;
        let fresh = if matches!(o.grant, OAuthGrant::ClientCredentials) {
            client_credentials(&client, o)?
        } else {
            let rt = config::load_oauth_tokens(&profile.name)?
                .and_then(|t| t.refresh_token)
                .or_else(|| o.tokens.as_ref().and_then(|t| t.refresh_token.clone()))
                .ok_or_else(|| Error::Auth {
                    status: 401,
                    message: format!(
                        "no refresh token for profile '{}'; run `sn profile login`",
                        profile.name
                    ),
                    transaction_id: None,
                })?;
            refresh(&client, o, &rt)?
        };
        config::save_oauth_tokens(&profile.name, &fresh)?;
        Ok(fresh)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OAuthGrant;

    fn sample_oauth() -> ResolvedOauth {
        ResolvedOauth {
            client_id: "cid".into(),
            client_secret: None,
            redirect_uri: "http://localhost:8400/callback".into(),
            auth_path: "/oauth_auth.do".into(),
            token_path: "/oauth_token.do".into(),
            grant: OAuthGrant::AuthorizationCode,
            pkce: true,
            tokens: None,
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_test_vector() {
        // From RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pkce_roundtrips() {
        let p = generate_pkce().unwrap();
        assert_eq!(pkce_challenge(&p.verifier), p.challenge);
        // Verifier length within the RFC 7636 43..=128 range.
        assert!((43..=128).contains(&p.verifier.len()));
    }

    #[test]
    fn states_are_random_and_urlsafe() {
        let a = random_state().unwrap();
        let b = random_state().unwrap();
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn authorize_url_has_required_params() {
        let o = sample_oauth();
        let url = authorize_url("https://acme.service-now.com", &o, "xyz", Some("chal")).unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        assert_eq!(parsed.path(), "/oauth_auth.do");
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q.get("response_type").unwrap(), "code");
        assert_eq!(q.get("client_id").unwrap(), "cid");
        assert_eq!(
            q.get("redirect_uri").unwrap(),
            "http://localhost:8400/callback"
        );
        assert_eq!(q.get("state").unwrap(), "xyz");
        assert_eq!(q.get("code_challenge").unwrap(), "chal");
        assert_eq!(q.get("code_challenge_method").unwrap(), "S256");
    }

    #[test]
    fn authorize_url_omits_pkce_when_disabled() {
        let o = sample_oauth();
        let url = authorize_url("https://acme.service-now.com", &o, "s", None).unwrap();
        assert!(!url.contains("code_challenge"));
    }

    #[test]
    fn classify_target_extracts_code_and_state() {
        match classify_target("/callback?code=abc&state=xyz", "/callback") {
            Inbound::Callback { code, state } => {
                assert_eq!(code, "abc");
                assert_eq!(state, "xyz");
            }
            _ => panic!("expected callback"),
        }
    }

    #[test]
    fn classify_target_ignores_other_paths() {
        assert!(matches!(
            classify_target("/favicon.ico", "/callback"),
            Inbound::Ignored
        ));
    }

    #[test]
    fn classify_target_surfaces_error() {
        match classify_target("/callback?error=access_denied", "/callback") {
            Inbound::AuthError(e) => assert!(e.contains("access_denied")),
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn parse_token_response_computes_expiry() {
        let before = config::now_unix();
        let v = serde_json::json!({
            "access_token": "AT",
            "refresh_token": "RT",
            "expires_in": 1800,
            "token_type": "Bearer"
        });
        let t = parse_token_response(&v).unwrap();
        assert_eq!(t.access_token, "AT");
        assert_eq!(t.refresh_token.as_deref(), Some("RT"));
        let exp = t.expires_at.unwrap();
        assert!(exp >= before + 1800 && exp <= config::now_unix() + 1800);
    }

    #[test]
    fn parse_token_response_accepts_string_expires_in() {
        let v = serde_json::json!({"access_token": "AT", "expires_in": "3600"});
        let t = parse_token_response(&v).unwrap();
        assert!(t.expires_at.is_some());
    }

    #[test]
    fn parse_token_response_errors_without_access_token() {
        let v = serde_json::json!({"error": "invalid_grant"});
        assert!(parse_token_response(&v).is_err());
    }
}
