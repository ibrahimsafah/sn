//! `sn raw --header` end to end.
//!
//! The interesting cases are the overrides. Headers land in three places —
//! client defaults (`Accept`, `User-Agent`), the per-request `Content-Type` on
//! bodied calls, and auth — and reqwest ranks request-level headers above client
//! defaults. A caller header merged in at build time would therefore be silently
//! ignored for `Content-Type`, the header people most want to set. These tests
//! read back what the server actually received rather than trusting a 200.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile_dir(server: &MockServer) -> tempfile::TempDir {
    write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    )
}

/// Every value the server saw under `name`, in order.
async fn received(server: &MockServer, name: &str) -> Vec<String> {
    let reqs = server.received_requests().await.expect("request recording");
    assert_eq!(reqs.len(), 1, "expected exactly one request: {reqs:?}");
    reqs[0]
        .headers
        .get_all(name)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

fn stderr_envelope(out: &assert_cmd::assert::Assert) -> Value {
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr is not the JSON error envelope ({e}): {stderr}"))
}

#[tokio::test(flavor = "current_thread")]
async fn custom_header_reaches_the_server() {
    let server = MockServer::start().await;
    // The mock only answers when the header arrives, so a 200 is the proof.
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(header("x-no-response-body", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args([
                "raw",
                "GET",
                "/api/now/table/incident",
                "-H",
                "X-no-response-body: true",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn user_accept_overrides_the_client_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args([
                "raw",
                "GET",
                "/api/now/table/incident",
                "--header",
                "Accept: application/json;charset=utf-8",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // Replaced, not appended: the client default `application/json` is gone.
    assert_eq!(
        received(&server, "accept").await,
        vec!["application/json;charset=utf-8"]
    );
    // Defaults the caller did not name are untouched.
    let ua = received(&server, "user-agent").await;
    assert!(ua[0].starts_with("sn/"), "user-agent was {ua:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn user_content_type_overrides_the_per_request_default() {
    // The trap: `Content-Type` is set at request level for bodied calls, so it
    // beats anything merged into the client's default headers.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"result": {"sys_id": "x"}})))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args([
                "raw",
                "POST",
                "/api/now/table/incident",
                "--field",
                "short_description=hi",
                "-H",
                "Content-Type: application/json; charset=utf-8",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();

    assert_eq!(
        received(&server, "content-type").await,
        vec!["application/json; charset=utf-8"],
        "caller Content-Type must replace the one the client sets"
    );
    let reqs = server.received_requests().await.unwrap();
    let body: Value = reqs[0].body_json().unwrap();
    assert_eq!(body["short_description"], "hi");
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_header_names_are_all_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .mount(&server)
        .await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args([
                "raw",
                "GET",
                "/api/now/table/incident",
                "-H",
                "X-Multi: a",
                "-H",
                "X-Multi: b",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();

    assert_eq!(received(&server, "x-multi").await, vec!["a", "b"]);
}

#[tokio::test(flavor = "current_thread")]
async fn authorization_header_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args([
                "raw",
                "GET",
                "/api/now/table/incident",
                "-H",
                "Authorization: Basic c25lYWt5",
            ])
            .assert()
            .code(1);
        let v = stderr_envelope(&out);
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.contains("Authorization"), "message was: {msg}");
        assert!(msg.contains("sn profile add"), "message was: {msg}");
        // The credential must not leak into the error envelope either.
        assert!(!msg.contains("c25lYWt5"), "message was: {msg}");
    })
    .await
    .unwrap();

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "the instance must never be contacted with a rejected header"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_header_is_a_usage_error() {
    let server = MockServer::start().await;
    let tmp = profile_dir(&server);

    tokio::task::spawn_blocking(move || {
        for bad in ["X-Broken", ": empty-name"] {
            let out = sn_cmd(tmp.path())
                .args(["raw", "GET", "/api/now/table/incident", "-H", bad])
                .assert()
                .code(1);
            let v = stderr_envelope(&out);
            assert!(
                v["error"]["message"].as_str().unwrap().contains("--header"),
                "message was: {v}"
            );
        }
    })
    .await
    .unwrap();

    assert!(server.received_requests().await.unwrap().is_empty());
}
