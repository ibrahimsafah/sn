//! End-to-end tests for API-key authentication (`auth = "apikey"`): the
//! `x-sn-apikey` header on the wire, profile creation/verification through
//! `sn profile add`, and the secrecy of the stored key. Driven through the
//! compiled binary with `assert_cmd`; each invocation gets its own config dir
//! via `SN_CONFIG_DIR`.

mod common;

use common::{sn_cmd, write_apikey_profile};
use serde_json::{Value, json};
use std::path::Path;
use wiremock::matchers::{header, method, path as wm_path};
use wiremock::{Mock, ResponseTemplate};

fn load_config(dir: &Path) -> sn::config::Config {
    sn::config::load_config_from(&dir.join("config.toml")).unwrap()
}

fn load_creds(dir: &Path) -> sn::config::Credentials {
    sn::config::load_credentials_from(&dir.join("credentials.toml")).unwrap()
}

/// Matches only requests that carry no `Authorization` header at all — an
/// API-key request must not also present basic/bearer credentials.
struct NoAuthorizationHeader;

impl wiremock::Match for NoAuthorizationHeader {
    fn matches(&self, request: &wiremock::Request) -> bool {
        !request.headers.contains_key("authorization")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn requests_carry_the_api_key_header_and_no_authorization() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/incident"))
        .and(header("x-sn-apikey", "KEY123"))
        .and(NoAuthorizationHeader)
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": "a"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let tmp = write_apikey_profile("keyed", &server.uri(), "KEY123");
    let dir = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(&dir)
            .args(["table", "list", "incident", "--setlimit", "1"])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v[0]["sys_id"], "a");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn add_writes_apikey_profile_verifies_and_never_leaks_the_key() {
    let server = wiremock::MockServer::start().await;
    // Verification lands on the identity endpoint with the key attached.
    Mock::given(method("GET"))
        .and(wm_path("/api/now/ui/user/current_user"))
        .and(header("x-sn-apikey", "SEKRET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"user_name": "api.user"}})),
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
                "keyed",
                "--instance",
                &uri,
                "--auth",
                "apikey",
                "--api-key",
                "SEKRET",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(!stdout.contains("SEKRET"), "key leaked to stdout: {stdout}");
        let v: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["auth"], "apikey");
        assert_eq!(v["verified"], true);
        assert_eq!(v["user"], "api.user");

        let cfg = load_config(&dir);
        assert_eq!(cfg.profiles["keyed"].auth, sn::config::AuthMethod::Apikey);
        let creds = load_creds(&dir);
        assert_eq!(creds.profiles["keyed"].api_key.as_deref(), Some("SEKRET"));
        assert!(creds.profiles["keyed"].password.is_empty());

        // `profile show` reports the method without reporting the secret.
        let out = sn_cmd(&dir)
            .args(["profile", "show", "keyed"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(!stdout.contains("SEKRET"), "key leaked by show: {stdout}");
        let v: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(v["auth"], "apikey");
        assert_eq!(v["hasApiKey"], true);
    })
    .await
    .unwrap();
}

#[test]
fn add_without_a_key_fails_naming_the_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "keyed",
            "--instance",
            "https://example.invalid",
            "--auth",
            "apikey",
            "--no-verify",
        ])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(
        stderr.contains("--api-key"),
        "error must name the missing flag, got: {stderr}"
    );
    // Nothing may survive a failed add.
    assert!(
        !tmp.path().join("config.toml").exists() || {
            !load_config(tmp.path()).profiles.contains_key("keyed")
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_verification_rolls_the_profile_back() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "User Not Authenticated"}})),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args([
                "profile",
                "add",
                "keyed",
                "--instance",
                &uri,
                "--auth",
                "apikey",
                "--api-key",
                "WRONG",
            ])
            .assert()
            .failure()
            .code(4);
        // The bad profile must not survive on disk.
        assert!(!load_config(&dir).profiles.contains_key("keyed"));
        assert!(!load_creds(&dir).profiles.contains_key("keyed"));
    })
    .await
    .unwrap();
}

#[test]
fn status_reports_apikey_profile() {
    let tmp = write_apikey_profile("keyed", "https://example.invalid", "KEY123");
    let out = sn_cmd(tmp.path())
        .args(["profile", "status"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["auth"], "apikey");
    assert_eq!(v["hasApiKey"], true);
    assert!(v.get("username").is_none());
}

#[test]
fn switching_to_basic_clears_the_stored_key() {
    let tmp = write_apikey_profile("keyed", "https://example.invalid", "KEY123");
    sn_cmd(tmp.path())
        .args([
            "profile",
            "add",
            "keyed",
            "--instance",
            "https://example.invalid",
            "--auth",
            "basic",
            "--username",
            "u",
            "--password",
            "p",
            "--force",
            "--no-verify",
        ])
        .assert()
        .success();
    let creds = load_creds(tmp.path());
    assert!(creds.profiles["keyed"].api_key.is_none());
    assert_eq!(creds.profiles["keyed"].username, "u");
    assert_eq!(
        load_config(tmp.path()).profiles["keyed"].auth,
        sn::config::AuthMethod::Basic
    );
}
