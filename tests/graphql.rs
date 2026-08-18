mod common;

use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(server_uri: &str) -> tempfile::TempDir {
    common::write_profiles(
        "test",
        &[common::ProfileSpec {
            name: "test",
            instance: server_uri,
            username: "u",
            password: "p",
        }],
    )
}

#[tokio::test(flavor = "current_thread")]
async fn success_unwraps_data() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains("GlideRecord_Query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"GlideRecord_Query": {"incident": {"_rowCount": 1}}}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "graphql",
                "query { GlideRecord_Query { incident { _rowCount } } }",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        // `data` is unwrapped, like the REST result envelope.
        assert!(s.contains("\"GlideRecord_Query\""), "stdout was: {s}");
        assert!(!s.contains("\"data\""), "stdout was: {s}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn output_raw_keeps_the_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"x": 1}})))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "--output", "raw", "graphql", "query { x }"])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(s.contains("\"data\""), "stdout was: {s}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn graphql_errors_exit_2_despite_http_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{
                "message": "Validation error (FieldUndefined@[x]) : Field 'x' is undefined",
                "errorType": "ValidationError"
            }]
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["graphql", "query { x }"])
            .assert()
            .code(2);
        let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&err).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("FieldUndefined")
        );
        // The full errors array rides in sn_error for programmatic consumers.
        assert_eq!(v["error"]["sn_error"][0]["errorType"], "ValidationError");
        assert_eq!(v["error"]["status_code"], 200);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn partial_data_reaches_stdout_alongside_the_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"good": {"_rowCount": 2}},
            "errors": [{"message": "partial failure"}]
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "graphql", "query { good bad }"])
            .assert()
            .code(2);
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(s.contains("\"good\""), "stdout was: {s}");
        let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(err.contains("partial failure"), "stderr was: {err}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn variables_and_vars_merge_into_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_partial_json(json!({
            "variables": {"id": "abc123", "limit": 5},
            "operationName": "Get"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"ok": true}})))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "graphql",
                "query Get($id: String!, $limit: Int) { x }",
                "--variables",
                r#"{"id": "old", "limit": 5}"#,
                "--var",
                "id=abc123",
                "--operation",
                "Get",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn query_from_stdin() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains("from_stdin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"ok": true}})))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args(["graphql", "@-"])
            .write_stdin("query { from_stdin }")
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn http_auth_failure_still_exits_4() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "User Not Authenticated"}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args(["graphql", "query { x }"])
            .assert()
            .code(4);
    })
    .await
    .unwrap();
}
