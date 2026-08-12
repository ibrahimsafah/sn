//! `sn cmdb create/update` must send the CMDB Instance API's IRE envelope
//! (`{"attributes": {...}, "source": "..."}`). A flat body 500s with a raw Java
//! NPE and an envelope without `source` 400s, so these tests pin the exact
//! request body rather than just the status code.

mod common;

use serde_json::json;
use wiremock::matchers::{body_json, method, path};
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

const CI: &str = "0f4ac6c4b750230096c3e4f6ee11a9fe";

fn created() -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({ "result": { "sys_id": CI } }))
}

fn updated() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "result": { "sys_id": CI } }))
}

#[tokio::test(flavor = "current_thread")]
async fn create_with_fields_wraps_them_in_attributes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(json!({
            "attributes": {"name": "web01", "cpu_count": 8},
            "source": "Manual Entry",
        })))
        .respond_with(created())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--field",
                "name=web01",
                "--field",
                "cpu_count=8",
                "--source",
                "Manual Entry",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn create_without_source_defaults_to_manual_entry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(json!({
            "attributes": {"name": "web01"},
            "source": "Manual Entry",
        })))
        .respond_with(created())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--field",
                "name=web01",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn create_with_flat_data_wraps_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(json!({
            "attributes": {"name": "web01", "short_description": "x"},
            "source": "Other Automated",
        })))
        .respond_with(created())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--data",
                r#"{"name":"web01","short_description":"x"}"#,
                "--source",
                "Other Automated",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// An explicit envelope is the caller saying they know the shape — including
/// the relation arrays, which have no flat spelling.
#[tokio::test(flavor = "current_thread")]
async fn create_with_an_explicit_envelope_passes_through_untouched() {
    let server = MockServer::start().await;
    let envelope = json!({
        "attributes": {"name": "web01"},
        "source": "ServiceNow",
        "outbound_relations": [{"target": "abc123", "type": "def456"}],
    });
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(envelope.clone()))
        .respond_with(created())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--data",
                &envelope.to_string(),
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn update_with_fields_wraps_them_in_attributes() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/now/cmdb/instance/cmdb_ci_linux_server/{CI}"
        )))
        .and(body_json(json!({
            "attributes": {"short_description": "x"},
            "source": "Manual Entry",
        })))
        .respond_with(updated())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "update",
                "cmdb_ci_linux_server",
                CI,
                "--field",
                "short_description=x",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn update_with_flat_data_wraps_it() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/now/cmdb/instance/cmdb_ci_linux_server/{CI}"
        )))
        .and(body_json(json!({
            "attributes": {"short_description": "x"},
            "source": "Manual via IRE",
        })))
        .respond_with(updated())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "update",
                "cmdb_ci_linux_server",
                CI,
                "--data",
                r#"{"short_description":"x"}"#,
                "--source",
                "Manual via IRE",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn update_with_an_explicit_envelope_passes_through_untouched() {
    let server = MockServer::start().await;
    let envelope = json!({"attributes": {"short_description": "x"}, "source": "Altiris"});
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/now/cmdb/instance/cmdb_ci_linux_server/{CI}"
        )))
        .and(body_json(envelope.clone()))
        .respond_with(updated())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "update",
                "cmdb_ci_linux_server",
                CI,
                "--data",
                &envelope.to_string(),
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// 718 CMDB classes on a stock instance carry a real String column named
/// `attributes`. A scalar there is a field to write, not an envelope.
#[tokio::test(flavor = "current_thread")]
async fn a_scalar_attributes_field_is_wrapped_like_any_other() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/api/now/cmdb/instance/cmdb_ci_storage_pool/{CI}"
        )))
        .and(body_json(json!({
            "attributes": {"attributes": "raid=6"},
            "source": "Manual Entry",
        })))
        .respond_with(updated())
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "update",
                "cmdb_ci_storage_pool",
                CI,
                "--field",
                "attributes=raid=6",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn source_in_both_the_body_and_the_flag_is_a_usage_error() {
    let server = MockServer::start().await;
    // The whole point: an ambiguous provenance must not reach the CMDB.
    Mock::given(method("POST"))
        .respond_with(created())
        .expect(0)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--data",
                r#"{"attributes":{"name":"web01"},"source":"ServiceNow"}"#,
                "--source",
                "Manual Entry",
            ])
            .assert()
            .code(1);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("given twice"), "stderr: {stderr}");
        assert!(stderr.contains("ServiceNow"), "stderr: {stderr}");
        assert!(stderr.contains("Manual Entry"), "stderr: {stderr}");
    })
    .await
    .unwrap();
}

/// A bad `source` still fails server-side; the choice-value hint in the
/// response must reach the caller instead of being swallowed.
#[tokio::test(flavor = "current_thread")]
async fn a_rejected_source_surfaces_the_instance_hint() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/cmdb/instance/cmdb_ci_linux_server/{CI}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "message": "Invalid input data",
                "detail": "INVALID_INPUT_DATA: In payload invalid data source [Nonesuch] exist. You need to provide a valid choice value from field [discovery_source] in table [cmdb_ci]."
            },
            "status": "failure"
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "cmdb",
                "update",
                "cmdb_ci_linux_server",
                CI,
                "--field",
                "short_description=x",
                "--source",
                "Nonesuch",
            ])
            .assert()
            .code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(v["error"]["status_code"], 400);
        assert!(
            v["error"]["detail"]
                .as_str()
                .unwrap()
                .contains("discovery_source"),
            "the valid-choice hint must survive: {stderr}"
        );
    })
    .await
    .unwrap();
}
