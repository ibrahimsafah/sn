//! `sn cmdb create/update` must send the CMDB Instance API's IRE envelope
//! (`{"attributes": {...}, "source": "..."}`), with every attribute value a
//! JSON **string**. A flat body 500s with a raw Java NPE, an envelope without
//! `source` 400s, and a non-string attribute value 500s with a Java
//! `ClassCastException` while writing nothing — so these tests pin the exact
//! request body rather than just the status code. Each expectation below is a
//! body measured to be accepted by a live Zurich instance; a mock that accepts
//! a payload the instance rejects would make this whole file meaningless.

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

/// Every attribute value goes out as a JSON string. Measured on Zurich, a bare
/// `8` here is HTTP 500 `class java.lang.Integer cannot be cast to class
/// java.lang.String` and nothing is written; `"8"` succeeds and the record
/// reads back `cpu_count: "8"`. `--field` parses bare digits, `true`/`false`
/// and decimals into JSON scalars, so all three have to be stringified.
#[tokio::test(flavor = "current_thread")]
async fn create_with_fields_wraps_them_in_attributes_as_strings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(json!({
            "attributes": {
                "name": "web01",
                "cpu_count": "8",
                "virtual": "true",
                "cost": "12.5",
            },
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
                "--field",
                "virtual=true",
                "--field",
                "cost=12.5",
                "--source",
                "Manual Entry",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// The documented one-liner (`--field operational_status=2`) 500s if the number
/// reaches the instance as a number.
#[tokio::test(flavor = "current_thread")]
async fn update_stringifies_a_numeric_field() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/cmdb/instance/cmdb_ci_server/{CI}")))
        .and(body_json(json!({
            "attributes": {"operational_status": "2"},
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
                "cmdb_ci_server",
                CI,
                "--field",
                "operational_status=2",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// The envelope passthrough is about the body's *shape*; the cast the API
/// performs on every attribute value applies to a hand-written envelope too.
#[tokio::test(flavor = "current_thread")]
async fn an_explicit_envelope_still_gets_string_attribute_values() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_linux_server"))
        .and(body_json(json!({
            "attributes": {"name": "web01", "cpu_count": "8"},
            "source": "Altiris",
            "outbound_relations": [{"target": "abc123", "type": "def456"}],
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
                r#"{"attributes":{"name":"web01","cpu_count":8},"source":"Altiris",
                    "outbound_relations":[{"target":"abc123","type":"def456"}]}"#,
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// A JSON object or array under `attributes` is refused before the request:
/// the API answers those with the same 500 (`LinkedHashMap`/`ArrayList` cannot
/// be cast to `String`), and serializing one to text would write garbage.
#[tokio::test(flavor = "current_thread")]
async fn a_structured_attribute_value_is_refused_without_a_request() {
    let server = MockServer::start().await;
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
                r#"{"name":"web01","ports":[80,443]}"#,
            ])
            .assert()
            .code(1);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("ports"), "stderr: {stderr}");
        assert!(stderr.contains("string values"), "stderr: {stderr}");
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

/// A flat body's `source` used to be demoted into `attributes`, where the API
/// dropped it and stamped the record with the flag/default provenance instead —
/// measured live as a silent exit 0 writing `discovery_source: "Manual Entry"`
/// while the caller said "Altiris". It is now refused, with or without the flag.
#[tokio::test(flavor = "current_thread")]
async fn a_flat_bodys_source_is_refused_instead_of_demoted() {
    for flag in [None, Some("Manual Entry")] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(created())
            .expect(0)
            .mount(&server)
            .await;
        let server_uri = server.uri();
        tokio::task::spawn_blocking(move || {
            let tmp = profile(&server_uri);
            let mut cmd = common::sn_cmd(tmp.path());
            cmd.args([
                "--compact",
                "cmdb",
                "create",
                "cmdb_ci_linux_server",
                "--data",
                r#"{"name":"zz-probe","source":"Altiris"}"#,
            ]);
            if let Some(flag) = flag {
                cmd.args(["--source", flag]);
            }
            let out = cmd.assert().code(1);
            let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
            assert!(stderr.contains("ambiguous"), "stderr: {stderr}");
            assert!(stderr.contains("Altiris"), "stderr: {stderr}");
        })
        .await
        .unwrap();
    }
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
