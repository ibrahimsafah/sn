//! The machine-facing output contract: which shape a command's final value
//! takes, which flag combinations are refused instead of quietly ignored, and
//! which keys the stderr envelope is allowed to carry.
//!
//! Every case here shipped broken at least once by *accepting* a flag and doing
//! nothing with it, which no test can catch by asserting on a success exit code
//! alone — so each test pins the rendered bytes or the missing key.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn stdout_of(out: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

fn stderr_envelope(out: &assert_cmd::assert::Assert) -> Value {
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
        panic!("stderr is not the JSON error envelope ({e}): {stderr}");
    })
}

fn profile_dir(instance: &str) -> tempfile::TempDir {
    write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance,
            username: "u",
            password: "p",
        }],
    )
}

/// comfy-table's UTF8_FULL preset; its absence means the value went out as JSON.
fn is_rendered_table(stdout: &str) -> bool {
    stdout.contains('│') && stdout.contains('─')
}

// ── --output table reaches every command ─────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn aggregate_honors_output_table() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"stats": {"count": "42"}}
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--output", "table", "aggregate", "incident", "--count"])
            .assert()
            .success();
        let stdout = stdout_of(&out);
        assert!(is_rendered_table(&stdout), "expected a table:\n{stdout}");
        assert!(stdout.contains("stats"), "{stdout}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn scores_list_honors_output_table() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/pa/scorecards"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"name": "Open incidents", "value": "42"}]
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--output", "table", "scores", "list"])
            .assert()
            .success();
        let stdout = stdout_of(&out);
        assert!(is_rendered_table(&stdout), "expected a table:\n{stdout}");
        assert!(stdout.contains("Open incidents"), "{stdout}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn scores_favorite_honors_output_table() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/pa/scorecards"))
        .and(query_param("sysparm_uuid", "uuid-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"uuid": "uuid-1", "favorite": "true"}
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--output", "table", "scores", "favorite", "uuid-1"])
            .assert()
            .success();
        let stdout = stdout_of(&out);
        assert!(is_rendered_table(&stdout), "expected a table:\n{stdout}");
    })
    .await
    .unwrap();
}

// ── favorite/unfavorite report symmetrically ─────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn scores_unfavorite_emits_the_response_body() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/now/pa/scorecards"))
        .and(query_param("sysparm_uuid", "uuid-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"uuid": "uuid-1", "favorite": "false"}
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--compact", "scores", "unfavorite", "uuid-1"])
            .assert()
            .success();
        let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
        assert_eq!(v["favorite"], "false");
    })
    .await
    .unwrap();
}

/// The endpoint may answer with no body at all. `null` on stdout is nothing a
/// caller can branch on, so the command names what it did instead.
#[tokio::test(flavor = "current_thread")]
async fn scores_unfavorite_emits_a_result_for_an_empty_body() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/now/pa/scorecards"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--compact", "scores", "unfavorite", "uuid-1"])
            .assert()
            .success();
        let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["uuid"], "uuid-1");
    })
    .await
    .unwrap();
}

// ── --all refuses the output modes it cannot honor ───────────────────────────

#[test]
fn table_list_all_rejects_output_table() {
    // No server: the combination is refused before any request goes out.
    let tmp = profile_dir("http://127.0.0.1:1");
    let out = sn_cmd(tmp.path())
        .args(["--output", "table", "table", "list", "incident", "--all"])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--array"), "message was: {msg}");
}

/// "Before any request goes out" has to hold for an OAuth profile too, and that
/// is the case the basic-profile test above cannot see: `build_client` mints a
/// token, so a guard sitting below it makes an unreachable instance or an IdP
/// outage answer a pure argv mistake with a transport error (exit 3) and never
/// prints the exit-1 usage error naming `--array`.
///
/// The profile's cached token is already expired and the token endpoint is a
/// closed port, so any refresh attempt is a guaranteed, immediate exit 3.
#[test]
fn table_list_all_rejects_output_table_before_minting_an_oauth_token() {
    let expired = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 60;
    let tmp = common::write_oauth_profile("oauth", "http://127.0.0.1:1", "cid", expired);
    let out = sn_cmd(tmp.path())
        .args(["--output", "table", "table", "list", "incident", "--all"])
        .assert()
        .code(1);
    let msg = stderr_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--array"), "message was: {msg}");
    assert!(
        !msg.contains("oauth_token.do"),
        "the guard ran after the token round-trip: {msg}"
    );
}

#[test]
fn table_list_all_rejects_output_raw() {
    let tmp = profile_dir("http://127.0.0.1:1");
    for args in [
        vec!["--output", "raw", "table", "list", "incident", "--all"],
        // `--array` buffers, but the envelope the paginator dropped is gone in
        // that form too, so raw is refused there as well.
        vec![
            "--output", "raw", "table", "list", "incident", "--all", "--array",
        ],
    ] {
        let out = sn_cmd(tmp.path()).args(&args).assert().code(1);
        let msg = stderr_envelope(&out)["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains("--output raw"), "message was: {msg}");
    }
}

/// The way out that the `--output table` error points at must actually work.
#[tokio::test(flavor = "current_thread")]
async fn table_list_all_array_renders_a_table() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"number": "INC0001"}, {"number": "INC0002"}]
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args([
                "--output", "table", "table", "list", "incident", "--all", "--array",
            ])
            .assert()
            .success();
        let stdout = stdout_of(&out);
        assert!(is_rendered_table(&stdout), "expected a table:\n{stdout}");
        assert!(stdout.contains("INC0002"), "{stdout}");
    })
    .await
    .unwrap();
}

// ── --wait honors --output, and a failed wait has no HTTP status ─────────────

/// Two breaks in one path: the wait hardcoded the default output mode, and the
/// progress link was only looked for at the top level — so under `--output raw`
/// the command emitted the *initial* response, unwaited and unwrapped.
#[tokio::test(flavor = "current_thread")]
async fn wait_keeps_the_envelope_under_output_raw() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/app_repo/install"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"links": {"progress": {"id": "prog1"}}, "status": "0"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/progress/prog1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"status": "2", "status_label": "Successful"}
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args([
                "--compact",
                "--output",
                "raw",
                "app",
                "install",
                "--scope",
                "x_acme",
                "--wait",
            ])
            .assert()
            .success();
        let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
        assert_eq!(
            v["result"]["status_label"], "Successful",
            "raw must keep the envelope AND report the polled result: {v}"
        );
    })
    .await
    .unwrap();
}

/// A CICD operation that fails is reported over HTTP 200, so there is no status
/// code to publish. The key must be **absent** — agents branch on its presence,
/// and the old `0` was a status no HTTP response can carry.
#[tokio::test(flavor = "current_thread")]
async fn failed_wait_omits_status_code_from_the_error_envelope() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/app_repo/install"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"links": {"progress": {"id": "prog2"}}, "status": "0"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/progress/prog2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "status": "3",
                "status_message": "install failed",
                "status_detail": "missing dependency"
            }
        })))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server.uri());
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["app", "install", "--scope", "x_acme", "--wait"])
            .assert()
            .code(2);
        let env = stderr_envelope(&out);
        let err = env["error"].as_object().expect("error object");
        assert!(
            !err.contains_key("status_code"),
            "status_code must be absent, not null or 0: {env}"
        );
        assert_eq!(err["message"], "install failed");
        assert_eq!(err["detail"], "missing dependency");
    })
    .await
    .unwrap();
}

// ── profile mutations report what they did ───────────────────────────────────

#[test]
fn profile_remove_emits_a_json_result() {
    let tmp = write_profiles(
        "alpha",
        &[
            ProfileSpec {
                name: "alpha",
                instance: "alpha.example.com",
                username: "au",
                password: "ap",
            },
            ProfileSpec {
                name: "beta",
                instance: "beta.example.com",
                username: "bu",
                password: "bp",
            },
        ],
    );

    let out = sn_cmd(tmp.path())
        .args(["--compact", "profile", "remove", "alpha", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["profile"], "alpha");
    assert_eq!(v["removed"], true);
    assert_eq!(v["wasDefault"], true);

    // Removing what is not there stays a success, and says so.
    let out = sn_cmd(tmp.path())
        .args(["--compact", "profile", "remove", "alpha", "--yes"])
        .assert()
        .success();
    let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
    assert_eq!(v["removed"], false);
    assert_eq!(v["wasDefault"], false);
}

#[test]
fn profile_use_emits_a_json_result() {
    let tmp = write_profiles(
        "alpha",
        &[
            ProfileSpec {
                name: "alpha",
                instance: "alpha.example.com",
                username: "au",
                password: "ap",
            },
            ProfileSpec {
                name: "beta",
                instance: "beta.example.com",
                username: "bu",
                password: "bp",
            },
        ],
    );

    let out = sn_cmd(tmp.path())
        .args(["--compact", "profile", "use", "beta"])
        .assert()
        .success();
    let v: Value = serde_json::from_str(stdout_of(&out).trim()).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["profile"], "beta");
    assert_eq!(v["default"], true);

    let cfg = sn::config::load_config_from(&tmp.path().join("config.toml")).unwrap();
    assert_eq!(cfg.default_profile.as_deref(), Some("beta"));
}

// ── an error names only flags the invoked command accepts ────────────────────

/// `sn init` shares its implementation with `sn profile add`, which has
/// `--password-stdin` and `--non-interactive`. `init` has neither, so pointing
/// its caller at them was a dead end.
#[test]
fn init_names_a_password_flag_it_actually_accepts() {
    let tmp = tempfile::tempdir().unwrap();
    let out = sn_cmd(tmp.path())
        .args([
            "init",
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
    assert!(msg.contains("--password"), "message was: {msg}");
    assert!(!msg.contains("--password-stdin"), "message was: {msg}");
    assert!(!msg.contains("--non-interactive"), "message was: {msg}");

    // `profile add` does accept both, and must keep naming them.
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
    assert!(msg.contains("--non-interactive"), "message was: {msg}");
}
