//! Self-verification must never name the wrong user.
//!
//! `sn ping` / `sn user me` / `sn profile add`'s verification all answer "who
//! are these credentials?". The old answer came from one `sys_user` read
//! filtered by a `gs.getUserName()` scripted query, which fails **open**: an
//! instance that cannot evaluate the term silently drops it, the surviving
//! `sysparm_limit` returns whichever rows sort first, and the CLI confidently
//! reports a stranger.
//!
//! These tests pin the replacement: two undocumented endpoints that name the
//! caller directly, a narrowly-triggered fallback chain behind them, and — at
//! every rung — a refusal to report a name that cannot be defended.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION_PATH: &str = "/api/now/sg/impersonation/session";
const CURRENT_USER_PATH: &str = "/api/now/ui/user/current_user";
const SYS_USER_PATH: &str = "/api/now/table/sys_user";

/// A live CSRF credential in the impersonation-session response. Any test that
/// mounts that endpoint uses this exact string so a leak is greppable.
const LEAKY_SESSION_TOKEN: &str = "LEAKYSESSIONTOKEN0xC5RF";

fn session_body(current: &str, original: &str) -> Value {
    // Deliberately NOT wrapped in a `result` envelope — the real endpoint
    // returns a bare object, unlike almost every other ServiceNow path.
    json!({
        "CanImpersonate": true,
        "CurrentUser": current,
        "InactivityTimeout": 90,
        "OriginalUser": original,
        "SessionToken": LEAKY_SESSION_TOKEN,
        "admin": true,
    })
}

fn current_user_body(user: &str) -> Value {
    json!({"result": {
        "user_avatar": null,
        "user_display_name": "Abey Ahmad",
        "user_initials": "AA",
        "user_name": user,
        "user_sys_id": "218517762f7903107efd1d707fa4e3de",
    }})
}

async fn mount(server: &MockServer, p: &str, status: u16, body: Value, calls: u64) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .expect(calls)
        .mount(server)
        .await;
}

/// Mount the `sys_user` fallback read, asserting it asks for **two** rows: one
/// is what makes the dropped-term failure undetectable.
async fn mount_sys_user(server: &MockServer, rows: Value, calls: u64) {
    Mock::given(method("GET"))
        .and(path(SYS_USER_PATH))
        .and(query_param(
            "sysparm_query",
            "user_name=javascript:gs.getUserName()",
        ))
        .and(query_param("sysparm_limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": rows})))
        .expect(calls)
        .mount(server)
        .await;
}

/// Run `sn <args>` against `uri` with a profile whose configured username is
/// `real_user`, and return (exit code, stdout, stderr).
fn run(uri: &str, args: &[&str]) -> (i32, String, String) {
    let tmp = write_profiles(
        "p1",
        &[ProfileSpec {
            name: "p1",
            instance: uri,
            username: "real_user",
            password: "p",
        }],
    );
    let out = sn_cmd(tmp.path()).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ping_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("ping emits JSON on stdout")
}

// -----------------------------------------------------------------------------
// The primary probe.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn ping_names_the_caller_from_the_session_endpoint_without_leaking_its_token() {
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        session_body("abeyahmad", "abeyahmad"),
        1,
    )
    .await;
    // One request answers identity AND proves auth: no fallback may fire.
    mount(&server, CURRENT_USER_PATH, 200, json!({}), 0).await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["-ddd", "ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(v["identity_source"], json!("sg/impersonation/session"));
    assert_eq!(v["admin"], json!(true));
    assert_eq!(v["can_impersonate"], json!(true));
    assert_eq!(v["impersonating"], json!(false));
    assert_eq!(v["original_user"], json!("abeyahmad"));

    // The response carries a live CSRF credential. It must reach neither stdout
    // nor the `-ddd` debug log, which prints response bodies verbatim for every
    // other endpoint.
    assert!(
        !stdout.contains(LEAKY_SESSION_TOKEN),
        "SessionToken leaked to stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains(LEAKY_SESSION_TOKEN),
        "SessionToken leaked to -ddd stderr:\n{stderr}"
    );
    // ...and the suppression is scoped to that one request: -ddd still logged.
    assert!(
        stderr.contains(SESSION_PATH),
        "expected the request itself to still be logged at -ddd:\n{stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ping_reports_an_impersonated_session() {
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        session_body("victim.user", "admin.user"),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    // The effective identity is who you are *now*, not who you signed in as.
    assert_eq!(v["username"], json!("victim.user"));
    assert_eq!(v["original_user"], json!("admin.user"));
    assert_eq!(v["impersonating"], json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn a_blank_original_user_is_not_an_impersonated_session() {
    // A blank `OriginalUser` differs from the current user as a string, so taken
    // at face value it reports an impersonation that is not happening — a
    // confident falsehood about identity, which is the class of bug this whole
    // command exists to remove. Unknown must stay unknown.
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        json!({
            "CanImpersonate": true,
            "CurrentUser": "abeyahmad",
            "OriginalUser": "   ",
            "admin": true,
        }),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(
        v["impersonating"],
        Value::Null,
        "a blank OriginalUser is unknown, not an impersonation:\n{stdout}"
    );
    assert_eq!(v["original_user"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn ping_prefers_the_instance_identity_over_the_configured_username() {
    // The profile says `real_user`; the instance says otherwise. Echoing the
    // config back would verify nothing — the config is the thing under test.
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        session_body("someone.else", "someone.else"),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 0);
    assert_eq!(ping_json(&stdout)["username"], json!("someone.else"));
}

#[tokio::test(flavor = "current_thread")]
async fn ping_still_reports_the_build_version() {
    // Identity moved to a new endpoint; the build fields are an output contract
    // and the `sys_properties` read that supplies them has to survive.
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        session_body("abeyahmad", "abeyahmad"),
        1,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_properties"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [
            {"name": "glide.buildname", "value": "Zurich"},
            {"name": "glide.buildtag", "value": "glide-zurich-06-25-2025"},
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();

    let (code, stdout, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 0);
    let v = ping_json(&stdout);
    assert_eq!(v["build_name"], json!("Zurich"));
    assert_eq!(v["build_tag"], json!("glide-zurich-06-25-2025"));
    assert!(v["latency_ms"].is_u64());
    assert_eq!(v["ok"], json!(true));
}

// -----------------------------------------------------------------------------
// The fallback chain. Both new endpoints are undocumented, so this is
// load-bearing, not decorative.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn ping_falls_back_to_current_user_when_the_session_endpoint_is_absent() {
    let server = MockServer::start().await;
    mount(&server, SESSION_PATH, 404, json!({}), 1).await;
    mount(
        &server,
        CURRENT_USER_PATH,
        200,
        current_user_body("abeyahmad"),
        1,
    )
    .await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(v["identity_source"], json!("ui/user/current_user"));
    assert_eq!(
        v["user_sys_id"],
        json!("218517762f7903107efd1d707fa4e3de"),
        "current_user identifies the record, not just the login name"
    );
    assert_eq!(v["user_display_name"], json!("Abey Ahmad"));
    // Nothing on this path can speak to privilege; it must not guess.
    assert_eq!(v["admin"], Value::Null);
    assert_eq!(v["impersonating"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn ping_falls_back_to_the_scripted_sys_user_read_when_both_are_absent() {
    let server = MockServer::start().await;
    mount(&server, SESSION_PATH, 404, json!({}), 1).await;
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    mount_sys_user(
        &server,
        json!([{"user_name": "abeyahmad", "sys_id": "abc123", "name": "Abey Ahmad"}]),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(v["identity_source"], json!("sys_user"));
    assert_eq!(v["user_sys_id"], json!("abc123"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_session_response_that_names_nobody_falls_back_instead_of_inventing_a_name() {
    // 200, plausible-looking, but no CurrentUser: this is not the endpoint we
    // think it is. Reporting `real_user` (the config) or a blank would both be
    // wrong answers dressed as right ones.
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        200,
        json!({"CanImpersonate": true, "SessionToken": LEAKY_SESSION_TOKEN}),
        1,
    )
    .await;
    mount(
        &server,
        CURRENT_USER_PATH,
        200,
        current_user_body("abeyahmad"),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(v["identity_source"], json!("ui/user/current_user"));
    // A partial answer must not be laundered into a confident one.
    assert_eq!(v["admin"], Value::Null);
    assert_eq!(v["can_impersonate"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn a_current_user_response_without_a_user_name_falls_back() {
    let server = MockServer::start().await;
    mount(&server, SESSION_PATH, 404, json!({}), 1).await;
    mount(
        &server,
        CURRENT_USER_PATH,
        200,
        json!({"result": {"user_avatar": null, "user_initials": "AA"}}),
        1,
    )
    .await;
    mount_sys_user(
        &server,
        json!([{"user_name": "abeyahmad", "sys_id": "abc123"}]),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v = ping_json(&stdout);
    assert_eq!(v["username"], json!("abeyahmad"));
    assert_eq!(v["identity_source"], json!("sys_user"));
}

#[tokio::test(flavor = "current_thread")]
async fn html_on_a_200_is_an_error_rather_than_a_name() {
    // A session redirect turns any endpoint into a login page served as 200
    // text/html. The fallback would be redirected identically, so retrying it
    // buys nothing — fail, don't guess.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SESSION_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Sign in</body></html>")
                .insert_header("content-type", "text/html"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount(&server, CURRENT_USER_PATH, 200, json!({}), 0).await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, stdout, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 3, "an unparseable body is a transport-level failure");
    assert!(stdout.is_empty(), "no identity may be emitted: {stdout}");
}

// -----------------------------------------------------------------------------
// The dropped-scripted-term failure this whole change exists for.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_scripted_term_reports_no_user_rather_than_a_stranger() {
    // Measured live: with the `javascript:` term dropped, `sysparm_limit`
    // returns whichever rows sort first. `user_name` is unique, so a second row
    // is proof the filter is gone — the CLI must decline to name anyone.
    let server = MockServer::start().await;
    mount(&server, SESSION_PATH, 404, json!({}), 1).await;
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    mount_sys_user(
        &server,
        json!([
            {"user_name": "survey.user", "sys_id": "s1"},
            {"user_name": "lucius.bagnoli", "sys_id": "s2"},
        ]),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "the credentials did authenticate: {stderr}");
    let v = ping_json(&stdout);
    assert!(
        !stdout.contains("survey.user") && !stdout.contains("lucius.bagnoli"),
        "an arbitrary row was reported as the caller:\n{stdout}"
    );
    // Falls all the way back to what the profile was configured with, and says so.
    assert_eq!(v["username"], json!("real_user"));
    assert_eq!(v["identity_source"], json!("profile"));
    assert_eq!(v["user_sys_id"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn profile_add_verification_reports_no_user_when_the_term_is_dropped() {
    // Same trap on the profile-creation path: the credentials verified, so the
    // profile is kept — but `user` must be null, not a stranger's name.
    let server = MockServer::start().await;
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    mount_sys_user(
        &server,
        json!([{"user_name": "survey.user"}, {"user_name": "lucius.bagnoli"}]),
        1,
    )
    .await;
    let uri = server.uri();

    tokio::task::spawn_blocking(move || {
        let tmp = tempfile::tempdir().unwrap();
        let out = sn_cmd(tmp.path())
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
            .output()
            .unwrap();
        assert!(out.status.success(), "profile add failed: {out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let v: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(v["user"], Value::Null, "named a stranger: {stdout}");
        assert!(!stdout.contains("survey.user"), "{stdout}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn user_me_refuses_arbitrary_rows_when_the_term_is_dropped() {
    let server = MockServer::start().await;
    // No `current_user` on this instance, so `user me` is on the scripted rung —
    // the one that can be sabotaged.
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    Mock::given(method("GET"))
        .and(path(SYS_USER_PATH))
        .and(query_param("sysparm_limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [
            {"user_name": "survey.user", "sys_id": "s1"},
            {"user_name": "lucius.bagnoli", "sys_id": "s2"},
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["user", "me"]))
        .await
        .unwrap();

    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stdout.is_empty(), "emitted a stranger's record:\n{stdout}");
    assert!(
        stderr.contains("arbitrary"),
        "error must explain the dropped query, got:\n{stderr}"
    );
    // The HTTP call was a 200. There is no HTTP error status to report, and
    // `status_code: 200` on a failure is a fabrication agents branch on — the
    // key must be absent, not present-and-wrong.
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr is a JSON envelope");
    assert!(
        err["error"].get("status_code").is_none(),
        "a 200 response must not be reported as an HTTP failure status:\n{stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn user_me_reports_no_record_without_inventing_a_status() {
    // Same rule on the empty-result path next door: nothing came back, but
    // nothing about that is HTTP 200 either.
    let server = MockServer::start().await;
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    mount_sys_user(&server, json!([]), 1).await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["user", "me"]))
        .await
        .unwrap();

    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stdout.is_empty());
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr is a JSON envelope");
    assert!(
        err["error"].get("status_code").is_none(),
        "no HTTP failure occurred, so no status may be reported:\n{stderr}"
    );
}

// -----------------------------------------------------------------------------
// `sn user me` prefers the endpoint that needs no scripted query at all.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn user_me_reads_the_record_named_by_current_user_with_no_scripted_query() {
    // Detecting the dropped-term failure is second best; not sending a scripted
    // term is first. `current_user` identifies the record, so `user me` reads it
    // by sys_id and the sabotage-able query never leaves the process.
    let server = MockServer::start().await;
    mount(
        &server,
        CURRENT_USER_PATH,
        200,
        current_user_body("abeyahmad"),
        1,
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{SYS_USER_PATH}/218517762f7903107efd1d707fa4e3de"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {
            "user_name": "abeyahmad",
            "sys_id": "218517762f7903107efd1d707fa4e3de",
            "email": "abey@example.com",
        }})))
        .expect(1)
        .mount(&server)
        .await;
    // The scripted list read must not happen at all.
    mount_sys_user(&server, json!([{"user_name": "survey.user"}]), 0).await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["user", "me"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v: Value = serde_json::from_str(&stdout).expect("user me emits JSON");
    assert_eq!(v["user_name"], json!("abeyahmad"));
    assert_eq!(v["email"], json!("abey@example.com"));
    assert_eq!(
        v["sys_id"],
        json!("218517762f7903107efd1d707fa4e3de"),
        "the record read must be the one current_user identified"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn user_me_falls_back_to_the_scripted_read_when_current_user_names_no_record() {
    // `current_user` answered, but with a login name and no `user_sys_id` — there
    // is nothing to read by id, so the scripted rung is still load-bearing.
    let server = MockServer::start().await;
    mount(
        &server,
        CURRENT_USER_PATH,
        200,
        json!({"result": {"user_name": "abeyahmad", "user_display_name": "Abey Ahmad"}}),
        1,
    )
    .await;
    mount_sys_user(
        &server,
        json!([{"user_name": "abeyahmad", "sys_id": "abc123"}]),
        1,
    )
    .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["user", "me"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["user_name"], json!("abeyahmad"));
}

#[tokio::test(flavor = "current_thread")]
async fn user_me_returns_the_single_matched_record() {
    let server = MockServer::start().await;
    mount(&server, CURRENT_USER_PATH, 404, json!({}), 1).await;
    Mock::given(method("GET"))
        .and(path(SYS_USER_PATH))
        .and(query_param(
            "sysparm_query",
            "user_name=javascript:gs.getUserName()",
        ))
        .and(query_param("sysparm_limit", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"user_name": "abeyahmad", "sys_id": "abc123"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();

    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || run(&uri, &["user", "me"]))
        .await
        .unwrap();

    assert_eq!(code, 0, "stderr: {stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["user_name"], json!("abeyahmad"));
}

// -----------------------------------------------------------------------------
// Auth verdicts must not be shopped around.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn a_401_from_the_session_endpoint_is_exit_4_with_no_fallback() {
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        401,
        json!({"error": {"message": "User Not Authenticated"}}),
        1,
    )
    .await;
    // Trying another endpoint after a rejected credential is how a ping ends up
    // reporting `ok` for dead credentials.
    mount(&server, CURRENT_USER_PATH, 200, json!({}), 0).await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, stdout, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 4);
    assert!(stdout.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_403_from_the_session_endpoint_is_exit_4_with_no_fallback() {
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        403,
        json!({"error": {"message": "Insufficient rights"}}),
        1,
    )
    .await;
    mount(&server, CURRENT_USER_PATH, 200, json!({}), 0).await;
    let uri = server.uri();

    let (code, _, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 4);
}

#[tokio::test(flavor = "current_thread")]
async fn a_401_from_current_user_is_exit_4_with_no_sys_user_fallback() {
    let server = MockServer::start().await;
    mount(&server, SESSION_PATH, 404, json!({}), 1).await;
    mount(
        &server,
        CURRENT_USER_PATH,
        401,
        json!({"error": {"message": "User Not Authenticated"}}),
        1,
    )
    .await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, _, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 4);
}

#[tokio::test(flavor = "current_thread")]
async fn a_500_from_the_session_endpoint_does_not_fall_back() {
    // A server fault says nothing about whether the path is supported, and a
    // second endpoint is no more likely to be honest about identity.
    let server = MockServer::start().await;
    mount(
        &server,
        SESSION_PATH,
        500,
        json!({"error": {"message": "boom"}}),
        1,
    )
    .await;
    mount(&server, CURRENT_USER_PATH, 200, json!({}), 0).await;
    mount_sys_user(&server, json!([]), 0).await;
    let uri = server.uri();

    let (code, stdout, _) = tokio::task::spawn_blocking(move || run(&uri, &["ping"]))
        .await
        .unwrap();
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
}
