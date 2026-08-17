//! End-to-end tests for `sn profile add`, driving the compiled binary with
//! `assert_cmd`. Each invocation gets its own config dir via `SN_CONFIG_DIR`, so
//! no global env mutation and no `serial` needed.
//!
//! assert_cmd runs the binary with a **non-TTY stdin**, so every test here
//! exercises the non-interactive branch — which is exactly the branch a script
//! or an agent hits. The prompting branch is unreachable from here (as it is for
//! `sn init`), so it is covered by passing every flag instead.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use std::path::Path;
use wiremock::matchers::{
    basic_auth, body_string_contains, header, method, path as wm_path, query_param,
};
use wiremock::{Mock, ResponseTemplate};

fn stderr_envelope(out: &assert_cmd::assert::Assert) -> Value {
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
        panic!("stderr is not the JSON error envelope ({e}): {stderr}");
    })
}

fn load_config(dir: &Path) -> sn::config::Config {
    sn::config::load_config_from(&dir.join("config.toml")).unwrap()
}

fn load_creds(dir: &Path) -> sn::config::Credentials {
    sn::config::load_credentials_from(&dir.join("credentials.toml")).unwrap()
}

/// A mock instance that accepts `u`/`p` on the verification read.
async fn verifying_server() -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .and(basic_auth("u", "p"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"user_name": "u"}]})),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "current_thread")]
async fn add_writes_profile_verifies_and_never_leaks_the_password() {
    let server = verifying_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "t",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "p",
            ])
            .assert()
            .success();

        // Success JSON is part of the machine contract: stdout, and nothing on
        // stderr (unlike `sn init`, which talks to a human).
        let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["profile"], "t");
        assert_eq!(v["auth"], "basic");
        assert_eq!(v["verified"], true);
        assert_eq!(v["user"], "u");
        assert!(
            out.get_output().stderr.is_empty(),
            "expected empty stderr, got: {}",
            String::from_utf8_lossy(&out.get_output().stderr)
        );

        // The password must never be echoed back.
        assert!(!text.contains("\"p\""), "password leaked:\n{text}");

        // Nothing was selected before, so `add` — which never claims the default
        // — has to say how to make this profile usable.
        assert_eq!(v["default"], false);
        assert_eq!(v["next"], "sn profile use t");

        let cred = &load_creds(&dir).profiles["t"];
        assert_eq!(cred.username, "u");
        assert_eq!(cred.password, "p");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn add_reports_the_authenticated_user_not_an_arbitrary_row() {
    let server = wiremock::MockServer::start().await;

    // Only `gs.getUserName()` names the caller. A bare `sysparm_limit=1` read of
    // sys_user returns whichever row sorts first — some unrelated account — and
    // reporting *that* as "the user you authenticated as" is how you end up
    // staring at a stranger's name after a successful login.
    //
    // Both queries are mocked, and the wrong one answers with a stranger, so this
    // fails loudly on a regression instead of merely missing a mock.
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "user_name=javascript:gs.getUserName()",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"user_name": "the.caller"}]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"user_name": "some.stranger"}]})),
        )
        .with_priority(2)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "t",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "p",
            ])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["user"], "the.caller");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn add_does_not_touch_the_default_profile() {
    let server = verifying_server().await;
    let tmp = write_profiles(
        "a",
        &[ProfileSpec {
            name: "a",
            instance: "https://example.invalid",
            username: "a-u",
            password: "a-p",
        }],
    );
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "b",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "p",
            ])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["default"], false);

        // This is the whole point of `add` vs `init`: adding a profile must not
        // silently repoint every subsequent command at it.
        let cfg = load_config(&dir);
        assert_eq!(cfg.default_profile.as_deref(), Some("a"));
        assert!(cfg.profiles.contains_key("a"));
        assert!(cfg.profiles.contains_key("b"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn add_set_default_flag_claims_the_default() {
    let server = verifying_server().await;
    let tmp = write_profiles(
        "a",
        &[ProfileSpec {
            name: "a",
            instance: "https://example.invalid",
            username: "a-u",
            password: "a-p",
        }],
    );
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "b",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "p",
                "--set-default",
            ])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["default"], true);
        assert_eq!(load_config(&dir).default_profile.as_deref(), Some("b"));
    })
    .await
    .unwrap();
}

#[test]
fn add_refuses_to_clobber_an_existing_profile() {
    let tmp = write_profiles(
        "a",
        &[ProfileSpec {
            name: "a",
            instance: "https://example.invalid",
            username: "a-u",
            password: "a-p",
        }],
    );

    // Unroutable instance: the guard must fire before any network call.
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "a",
            "--instance",
            "http://127.0.0.1:1",
            "--username",
            "new-u",
            "--password",
            "new-p",
        ])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("already exists"), "message was: {msg}");
    assert!(msg.contains("--force"), "message was: {msg}");

    // The identity someone else may be relying on is untouched.
    let cred = &load_creds(tmp.path()).profiles["a"];
    assert_eq!(cred.username, "a-u");
    assert_eq!(cred.password, "a-p");
    assert_eq!(
        load_config(tmp.path()).profiles["a"].instance,
        "https://example.invalid"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn add_force_overwrites_but_preserves_proxy_credentials() {
    let server = verifying_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    // Seed a profile carrying proxy credentials — a field `add` cannot configure
    // and therefore must never destroy.
    let mut cfg = sn::config::Config {
        default_profile: Some("t".into()),
        ..Default::default()
    };
    cfg.profiles.insert(
        "t".into(),
        sn::config::ProfileConfig {
            instance: "old.example.com".into(),
            ..Default::default()
        },
    );
    let mut creds = sn::config::Credentials::default();
    creds.profiles.insert(
        "t".into(),
        sn::config::ProfileCredentials {
            username: "old-u".into(),
            password: "old-p".into(),
            proxy_username: Some("proxy-user".into()),
            proxy_password: Some("proxy-pass".into()),
            ..Default::default()
        },
    );
    sn::config::save_config_to(&dir.join("config.toml"), &cfg).unwrap();
    sn::config::save_credentials_to(&dir.join("credentials.toml"), &creds).unwrap();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "t",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "p",
                "--force",
            ])
            .assert()
            .success();

        let cred = &load_creds(&dir).profiles["t"];
        assert_eq!(cred.username, "u");
        assert_eq!(cred.password, "p");
        assert_eq!(cred.proxy_username.as_deref(), Some("proxy-user"));
        assert_eq!(cred.proxy_password.as_deref(), Some("proxy-pass"));
    })
    .await
    .unwrap();
}

#[test]
fn add_names_the_missing_flag_instead_of_hanging() {
    let tmp = tempfile::tempdir().unwrap();

    // No --instance, and stdin is not a terminal. `sn init` would silently read
    // EOF and invent an empty instance; `add` must name the flag and exit 1.
    let out = sn_cmd(tmp.path())
        .args(["profile", "add", "t", "--username", "u", "--password", "p"])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--instance"), "message was: {msg}");
}

#[test]
fn add_names_the_missing_password_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "t",
            "--instance",
            "http://127.0.0.1:1",
            "--username",
            "u",
        ])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--password-stdin"), "message was: {msg}");
}

#[tokio::test(flavor = "current_thread")]
async fn add_reads_the_password_from_stdin() {
    let server = wiremock::MockServer::start().await;
    // The secret arrives with a trailing newline from the shell; only that may be
    // stripped, so the credential the instance sees is exactly `s3cret`.
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .and(basic_auth("u", "s3cret"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"user_name": "u"}]})),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "t",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password-stdin",
            ])
            .write_stdin("s3cret\n")
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["verified"], true);
        assert_eq!(load_creds(&dir).profiles["t"].password, "s3cret");
    })
    .await
    .unwrap();
}

#[test]
fn add_no_verify_skips_the_network() {
    let tmp = tempfile::tempdir().unwrap();

    // Port 1 is unroutable: if `--no-verify` did not short-circuit the check this
    // would fail with a transport error (exit 3) instead of succeeding.
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "t",
            "--instance",
            "http://127.0.0.1:1",
            "--username",
            "u",
            "--password",
            "p",
            "--no-verify",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["verified"], false);
    assert!(load_config(tmp.path()).profiles.contains_key("t"));
}

#[tokio::test(flavor = "current_thread")]
async fn add_writes_nothing_when_the_credentials_are_rejected() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        // 401 is an auth failure, not a usage error: exit 4.
        sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "t",
                "--instance",
                &uri,
                "--username",
                "u",
                "--password",
                "wrong",
            ])
            .assert()
            .code(4);

        // The rollback contract: a profile that cannot authenticate must not
        // survive on disk, or it becomes a landmine that fails somewhere
        // confusing later.
        assert!(
            !load_config(&dir).profiles.contains_key("t"),
            "a profile that failed verification was left on disk"
        );
        assert!(!load_creds(&dir).profiles.contains_key("t"));
    })
    .await
    .unwrap();
}

#[test]
fn add_oauth_authorization_code_refuses_when_it_cannot_open_a_browser() {
    let tmp = tempfile::tempdir().unwrap();

    // The browser flow is the only way to test these credentials, and there is no
    // browser here. Saving them unverified would be exactly the silent-landmine
    // case `add` exists to prevent — so it refuses, and points at the two
    // commands that do work.
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "sso",
            "--instance",
            "https://example.invalid",
            "--auth",
            "oauth",
            "--client-id",
            "cid",
        ])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--no-verify"), "message was: {msg}");
    assert!(msg.contains("sn profile login"), "message was: {msg}");
    assert!(!load_config(tmp.path()).profiles.contains_key("sso"));
}

#[test]
fn add_oauth_authorization_code_saves_unverified_with_no_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "sso",
            "--instance",
            "https://example.invalid",
            "--auth",
            "oauth",
            "--client-id",
            "cid",
            "--no-verify",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["auth"], "oauth");
    assert_eq!(v["grant"], "authorization_code");
    assert_eq!(v["verified"], false);
    assert_eq!(v["loggedIn"], false);
    // An OAuth profile with no tokens is useless until someone logs in; say so.
    assert_eq!(v["next"], "sn profile login --profile sso");

    let p = &load_config(tmp.path()).profiles["sso"];
    assert_eq!(p.auth, sn::config::AuthMethod::Oauth);
    assert_eq!(p.oauth.as_ref().unwrap().client_id, "cid");
}

#[tokio::test(flavor = "current_thread")]
async fn add_oauth_client_credentials_mints_a_token_headlessly() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(wm_path("/oauth_token.do"))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "CC_AT",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .and(header("authorization", "Bearer CC_AT"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"user_name": "svc"}]})),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        // client_credentials needs no browser, so `add` can and does test it.
        let out = sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "svc",
                "--instance",
                &uri,
                "--auth",
                "oauth",
                "--grant",
                "client_credentials",
                "--client-id",
                "cid",
                "--client-secret-stdin",
                "--timeout",
                "30",
            ])
            .write_stdin("shh\n")
            .assert()
            .success();

        let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["grant"], "client_credentials");
        assert_eq!(v["verified"], true);
        assert_eq!(v["loggedIn"], true);
        assert_eq!(v["user"], "svc");
        assert!(!text.contains("shh"), "client secret leaked:\n{text}");

        let cred = &load_creds(&dir).profiles["svc"];
        assert_eq!(cred.client_secret.as_deref(), Some("shh"));
        assert_eq!(
            cred.oauth_tokens.as_ref().unwrap().access_token,
            "CC_AT",
            "the minted token should be cached"
        );
    })
    .await
    .unwrap();
}
