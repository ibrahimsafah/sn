//! End-to-end CLI tests for the `sn profile` OAuth session commands, driving the compiled
//! binary with `assert_cmd`. Each invocation gets its own config dir (set
//! per-process via `SN_CONFIG_DIR`, so no global env mutation and no `serial`
//! needed) — cross-platform, no XDG/Linux gating required.

mod common;

use common::{sn_cmd, write_oauth_profile, write_profiles, ProfileSpec};
use serde_json::Value;
use sn::config::now_unix;
use std::path::Path;
use wiremock::matchers::{body_string_contains, header, method, path as wm_path};
use wiremock::{Mock, ResponseTemplate};

fn load_creds(dir: &Path) -> sn::config::Credentials {
    sn::config::load_credentials_from(&dir.join("credentials.toml")).unwrap()
}

#[test]
fn auth_status_reports_oauth_profile() {
    let tmp = write_oauth_profile(
        "cli",
        "https://example.invalid",
        "cid",
        (now_unix() + 3600) as i64,
    );

    let out = sn_cmd(tmp.path())
        .args(["--profile", "cli", "profile", "status"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["auth"], "oauth");
    assert_eq!(v["loggedIn"], true);
    assert_eq!(v["hasRefreshToken"], true);
    assert_eq!(v["grant"], "authorization_code");
}

#[test]
fn auth_login_rejects_basic_profile() {
    let tmp = write_profiles(
        "basic",
        &[ProfileSpec {
            name: "basic",
            instance: "https://example.invalid",
            username: "admin",
            password: "pw",
        }],
    );

    let out = sn_cmd(tmp.path())
        .args(["--profile", "basic", "profile", "login"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(
        stderr.contains("does not use oauth"),
        "expected oauth usage error, got: {stderr}"
    );
}

#[test]
fn auth_logout_clears_tokens() {
    let tmp = write_oauth_profile(
        "cli",
        "https://example.invalid",
        "cid",
        (now_unix() + 3600) as i64,
    );

    let out = sn_cmd(tmp.path())
        .args(["--profile", "cli", "profile", "logout"])
        .assert()
        .success();

    // Success JSON is part of the machine contract: it must land on stdout.
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["profile"], "cli");
    assert_eq!(v["loggedOut"], true);
    assert!(
        out.get_output().stderr.is_empty(),
        "expected empty stderr, got: {}",
        String::from_utf8_lossy(&out.get_output().stderr)
    );

    let creds = load_creds(tmp.path());
    assert!(creds.profiles["cli"].oauth_tokens.is_none());
}

#[test]
fn auth_group_is_gone() {
    let tmp = write_profiles(
        "basic",
        &[ProfileSpec {
            name: "basic",
            instance: "https://example.invalid",
            username: "admin",
            password: "pw",
        }],
    );

    // The `auth` group was merged into `sn profile`; the old spelling must be a
    // usage error, not a silent alias.
    let out = sn_cmd(tmp.path())
        .args(["--profile", "basic", "auth", "login"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(
        stderr.contains("unrecognized subcommand"),
        "expected unrecognized subcommand, got: {stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auth_refresh_rotates_and_persists_token() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(wm_path("/oauth_token.do"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "NEW_AT",
            "refresh_token": "NEW_RT",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let tmp = write_oauth_profile("cli", &server.uri(), "cid", (now_unix() + 3600) as i64);
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args(["--profile", "cli", "--timeout", "30", "profile", "refresh"])
            .assert()
            .success();

        // Success JSON is part of the machine contract: it must land on stdout.
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["profile"], "cli");
        assert_eq!(v["refreshed"], true);
        assert!(v["expiresAt"].is_number(), "expected expiresAt, got: {v}");
        assert!(
            out.get_output().stderr.is_empty(),
            "expected empty stderr, got: {}",
            String::from_utf8_lossy(&out.get_output().stderr)
        );
    })
    .await
    .unwrap();

    let creds = load_creds(tmp.path());
    assert_eq!(
        creds.profiles["cli"]
            .oauth_tokens
            .as_ref()
            .unwrap()
            .access_token,
        "NEW_AT"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn expired_token_auto_refreshes_then_calls_api() {
    let server = wiremock::MockServer::start().await;
    // Stale token forces build_client to refresh first…
    Mock::given(method("POST"))
        .and(wm_path("/oauth_token.do"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "NEW_AT",
            "refresh_token": "NEW_RT",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    // …then the API call must carry the freshly-minted bearer.
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .and(header("authorization", "Bearer NEW_AT"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": [{"user_name": "alice"}]})),
        )
        .mount(&server)
        .await;

    let tmp = write_oauth_profile("cli", &server.uri(), "cid", (now_unix() as i64) - 10); // expired
    let dir = tmp.path().to_path_buf();

    let out = tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args([
                "--profile",
                "cli",
                "--timeout",
                "30",
                "table",
                "list",
                "sys_user",
                "--limit",
                "1",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(out.status.success(), "command failed: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("alice"),
        "expected record in stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
