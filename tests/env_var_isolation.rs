mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{basic_auth, method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

// -----------------------------------------------------------------------------
// Regression suite for the env-var contract.
//
// The user reported: "environment variables are overriding profiles."
// Investigation showed the current code does NOT consult any of:
//   - SN_INSTANCE
//   - SN_INSTANCE_URL
//   - SN_USERNAME
//   - SN_PASSWORD
//   - SN_PROFILE
//   - SN_TIMEOUT
// Those names belonged to a previous design and were intentionally removed.
//
// The ONLY env vars currently consulted are `SN_CONFIG_DIR` (config directory
// selection) plus the proxy/TLS set documented in CLAUDE.md:
//   - SN_PROXY
//   - SN_NO_PROXY
//   - SN_INSECURE
//   - SN_CA_CERT
//   - SN_PROXY_CA_CERT
//
// These tests lock the contract in: setting any of the credential-leaking
// names must NOT influence which instance is reached, which user is
// authenticated as, which profile is selected, or how long the client waits.
// They are intended to fail loudly if any future change re-introduces
// env-var-driven credential resolution.
// -----------------------------------------------------------------------------

/// Seed a single-profile config dir: profile `"test"` (also `default_profile`)
/// with `instance = server_uri`, `username = "real_user"`,
/// `password = "real_pass"`. Returns the temp dir handle (drop = cleanup).
fn seed(server_uri: &str) -> TempDir {
    write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: server_uri,
            username: "real_user",
            password: "real_pass",
        }],
    )
}

/// Mount the standard `sn ping` mock — ping's primary probe, with the given
/// basic-auth pair — expecting exactly `n_calls`. Matching on `basic_auth` is
/// the whole point: a mismatch 404s, ping's fallback chain then also 404s, and
/// the `expect` count fails. Ping's best-effort `sys_properties` lookup 404s
/// harmlessly against wiremock and is ignored.
async fn mount_auth_mock(server: &MockServer, user: &str, pass: &str, n_calls: u64) {
    Mock::given(method("GET"))
        .and(path_matcher("/api/now/sg/impersonation/session"))
        .and(basic_auth(user, pass))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "CurrentUser": "ok",
            "OriginalUser": "ok",
            "admin": false,
            "CanImpersonate": false,
        })))
        .expect(n_calls)
        .mount(server)
        .await;
}

// =============================================================================
// 1. Credential / instance / profile env vars MUST NOT leak.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn sn_username_env_does_not_override_profile() {
    let server = MockServer::start().await;
    // Mock requires the PROFILE credentials. If SN_USERNAME leaked, basic
    // auth would arrive as "hacker" and the mock would not match → 404 → test
    // fails. The `expect(1)` plus `.success()` together prove the override
    // didn't happen.
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_USERNAME", "hacker")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn sn_password_env_does_not_override_profile() {
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_PASSWORD", "wrongpass")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn sn_instance_env_does_not_override_profile() {
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    // If SN_INSTANCE leaked, the binary would resolve traffic toward
    // nonexistent.invalid → DNS failure → exit 3. Success means the env was
    // ignored and the profile's URL was used.
    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_INSTANCE", "http://nonexistent.invalid")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn sn_instance_url_env_does_not_override_profile() {
    // This is the variant the user actually has set in their shell. Same
    // contract as the SN_INSTANCE test above.
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_INSTANCE_URL", "http://nonexistent.invalid")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn sn_profile_env_does_not_override_default_profile() {
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    // Set SN_PROFILE to a name that doesn't exist anywhere in the config. If
    // SN_PROFILE were honored, profile resolution would fail with "no
    // instance configured for profile 'other_profile_name'" (exit 1). It must
    // instead fall through to default_profile = "test" and succeed.
    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_PROFILE", "other_profile_name")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn sn_timeout_env_is_not_consulted() {
    // SN_TIMEOUT is NOT read by the binary. To prove that, we set it to "0"
    // — if it were honored as a `Duration::from_secs(0)`, every request
    // would time out instantly. A successful ping round-trip proves the env
    // var was ignored and the default timeout applied.
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_TIMEOUT", "0")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn all_credential_env_vars_set_to_garbage_still_works() {
    // The "user's actual environment" regression: every removed-design env
    // var is set to something that would break resolution if it were
    // consulted. The binary must still resolve via the on-disk profile.
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_INSTANCE", "http://nonexistent.invalid")
            .env("SN_INSTANCE_URL", "http://also-nonexistent.invalid")
            .env("SN_USERNAME", "hacker")
            .env("SN_PASSWORD", "wrongpass")
            .env("SN_PROFILE", "no_such_profile")
            .env("SN_TIMEOUT", "0")
            .args(["ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}

// =============================================================================
// 2. Proxy/TLS env vars MUST be honored as documented.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn sn_proxy_env_routes_through_proxy() {
    // Negative test: setting SN_PROXY to a closed port forces the request
    // through a dead proxy. If the env var IS honored (the documented
    // behavior), the connection fails at transport layer → exit code 3
    // ("Network/transport error" per CLAUDE.md). If the env var were ignored
    // we'd reach wiremock and the test would erroneously succeed.
    //
    // We deliberately don't try to mock a real HTTP CONNECT proxy: a
    // transport-level failure is a sufficient and unambiguous signal that
    // the env var made it into client construction.
    let server = MockServer::start().await;
    // The mock must NOT be hit — if it is, the proxy was bypassed.
    mount_auth_mock(&server, "real_user", "real_pass", 0).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_PROXY", "http://127.0.0.1:1")
            .args(["ping"])
            .assert()
            .code(3);
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn cli_proxy_flag_overrides_sn_proxy_env() {
    // Set SN_PROXY to a dead address (would fail). Pass --no-proxy on the
    // CLI to clear all proxy config. The request must succeed, which proves
    // the CLI flag overrode the env var.
    let server = MockServer::start().await;
    mount_auth_mock(&server, "real_user", "real_pass", 1).await;
    let uri = server.uri();

    let tmp = seed(&uri);
    let cfg = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&cfg)
            .env("SN_PROXY", "http://127.0.0.1:1")
            .args(["--no-proxy", "ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server);
}
