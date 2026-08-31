//! `sn gr` — compiled GraphQL reads end to end: document compilation on the
//! wire, response flattening, error mapping, and the argv guards.

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
async fn compiles_dotwalks_and_flattens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        // The compiled selection: shared prefix merged, `_reference` nesting.
        .and(body_string_contains(
            "caller_id { _reference { email { displayValue } } }",
        ))
        // The encoded query rides as a variable, never interpolated.
        .and(body_partial_json(
            json!({"variables": {"qc": "active=true"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"GlideRecord_Query": {"incident": {"_results": [{
                "number": {"displayValue": "INC0001"},
                "caller_id": {"_reference": {"email": {"displayValue": "beth@example.com"}}}
            }]}}}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "gr",
                "incident",
                "-f",
                "number,caller_id.email",
                "-q",
                "active=true",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        // Flattened back to the dotted keys, GraphQL nesting gone.
        assert!(s.contains(r#""caller_id.email":"beth@example.com""#), "{s}");
        assert!(s.contains(r#""number":"INC0001""#), "{s}");
        assert!(!s.contains("_reference"), "{s}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn count_compiles_to_row_count_only() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains("_rowCount"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"GlideRecord_Query": {"incident": {"_rowCount": 67}}}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "gr", "incident", "--count"])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(s.trim(), r#"{"count":67}"#);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn unqueryable_table_is_named_and_exits_2() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{"message":
                "Validation error (UnknownArgument@[GlideRecord_Query/no_such]) : \
                 Unknown field argument 'pagination'"}]
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["gr", "no_such", "-f", "number"])
            .assert()
            .code(2);
        let e = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(e.contains("not queryable via GraphQL"), "{e}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn dotwalk_through_non_reference_is_named() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{"message":
                "Validation error (FieldUndefined) : Field '_reference' in type \
                 'GlideStringField' is undefined"}]
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["gr", "incident", "-f", "short_description.name"])
            .assert()
            .code(2);
        let e = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(e.contains("non-reference field"), "{e}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn output_raw_keeps_the_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"GlideRecord_Query": {"incident": {"_results": []}}}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--output",
                "raw",
                "--compact",
                "gr",
                "incident",
                "-f",
                "number",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(s.contains("\"data\""), "{s}");
        assert!(s.contains("GlideRecord_Query"), "{s}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn argv_guards_fail_before_the_network() {
    // No mock is mounted: a request reaching the server would 404 and the
    // exit code would be 2/3, so exit 1 proves the guard fired first.
    let server = MockServer::start().await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        // Neither --fields nor --count.
        common::sn_cmd(tmp.path())
            .args(["gr", "incident"])
            .assert()
            .code(1);
        // --fields and --count conflict.
        common::sn_cmd(tmp.path())
            .args(["gr", "incident", "-f", "number", "--count"])
            .assert()
            .code(1);
        // A field segment that could break out of the document.
        let out = common::sn_cmd(tmp.path())
            .args(["gr", "incident", "-f", "number { value }"])
            .assert()
            .code(1);
        let e = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(e.contains("invalid field segment"), "{e}");
        // An invalid table name.
        common::sn_cmd(tmp.path())
            .args(["gr", "bad table", "-f", "number"])
            .assert()
            .code(1);
    })
    .await
    .unwrap();
}
