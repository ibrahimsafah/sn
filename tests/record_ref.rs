//! Record references (`table:identifier`) across the CLI, and `sn get`.
//!
//! `sn get` batches resolution + journal into one GraphQL POST and reads the
//! record body over the Table API; every other ref site resolves a number via
//! the Table API `number=` lookup with the `sysparm_limit=2` canary. Both
//! resolution paths and both failure modes (not found, dropped term) are
//! pinned here.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A well-formed sys_id for reference tokens (32 hex chars).
const HEX: &str = "1c741bd70b2322007518478d83673af3";
/// The sys_id number lookups resolve to.
const RESOLVED: &str = "aaaabbbbccccddddeeeeffff00001111";

fn profile_for(server_uri: &str) -> tempfile::TempDir {
    write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: server_uri,
            username: "u",
            password: "p",
        }],
    )
}

/// A profile pointing at an address no request can reach, for tests that must
/// prove a command failed before touching the network.
fn offline_profile() -> tempfile::TempDir {
    profile_for("http://127.0.0.1:9")
}

fn graphql_response(table: &str, results: serde_json::Value, row_count: u64) -> serde_json::Value {
    json!({"data": {"GlideRecord_Query": {table: {"_rowCount": row_count, "_results": results}}}})
}

async fn mount_vars_empty(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/now/table/question_answer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .mount(server)
        .await;
}

// ---------------------------------------------------------------- sn get ---

#[tokio::test(flavor = "current_thread")]
async fn get_by_sys_id_ref_needs_no_number_lookup() {
    let server = MockServer::start().await;
    // The one GraphQL POST carries the sys_id condition — never a number term.
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains(format!("sys_id={HEX}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_response(
            "incident",
            json!([{
                "sys_id": {"value": HEX},
                "comments": {"displayValue":
                    "2026-08-11 10:40:20 - Abey Ahmad (Comments)\nhello there\n\n"},
                "work_notes": null,
            }]),
            1,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{HEX}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": {"sys_id": HEX, "number": "INC0000060"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/question_answer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [
            {"question.name": "env", "question.question_text": "Environment", "value": "prod"},
        ]})))
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "get", &format!("incident:{HEX}")])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["table"], "incident");
        assert_eq!(v["sys_id"], HEX);
        assert_eq!(v["record"]["number"], "INC0000060");
        assert_eq!(v["variables"][0]["name"], "env");
        assert_eq!(v["variables"][0]["value"], "prod");
        assert_eq!(v["journal"][0]["author"], "Abey Ahmad");
        assert_eq!(v["journal"][0]["text"], "hello there");
        assert_eq!(v["journal"][0]["element"], "comments");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn get_by_number_resolves_in_the_graphql_document() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains("number=INC0010001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_response(
            "incident",
            json!([{"sys_id": {"value": RESOLVED}, "comments": null, "work_notes": null}]),
            1,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{RESOLVED}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": {"sys_id": RESOLVED, "number": "INC0010001"}})),
        )
        .mount(&server)
        .await;
    mount_vars_empty(&server).await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "get", "incident:INC0010001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["sys_id"], RESOLVED);
        assert_eq!(v["variables"], json!([]));
        assert_eq!(v["journal"], json!([]));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn get_bare_number_uses_the_prefix_map() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .and(body_string_contains("number=SIR0010001"))
        .and(body_string_contains("sn_si_incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_response(
            "sn_si_incident",
            json!([{"sys_id": {"value": RESOLVED}, "comments": null, "work_notes": null}]),
            1,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sn_si_incident/{RESOLVED}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": RESOLVED}})),
        )
        .mount(&server)
        .await;
    mount_vars_empty(&server).await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "get", "SIR0010001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["table"], "sn_si_incident");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn get_number_not_found_is_exit_2_without_status_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_response(
            "incident",
            json!([]),
            0,
        )))
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd.args(["get", "incident:INC9999999"]).assert().code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let err: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
        let msg = err["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("no incident record with number INC9999999"),
            "{msg}"
        );
        // A failure reported inside an HTTP 200 publishes no status_code.
        assert!(err["error"].get("status_code").is_none(), "{err}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn get_number_canary_trips_on_multiple_matches() {
    // Two rows for a unique-by-design number: the instance dropped the term
    // and returned unfiltered rows. Resolving to either would be wrong.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graphql_response(
            "sys_user_group",
            json!([
                {"sys_id": {"value": "a".repeat(32)}, "comments": null, "work_notes": null},
                {"sys_id": {"value": "b".repeat(32)}, "comments": null, "work_notes": null},
            ]),
            67,
        )))
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd.args(["get", "sys_user_group:GRP001"]).assert().code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        let err: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
        let msg = err["error"]["message"].as_str().unwrap();
        assert!(msg.contains("dropped by the instance"), "{msg}");
        assert!(err["error"].get("status_code").is_none(), "{err}");
    })
    .await
    .unwrap();
}

#[test]
fn get_unknown_prefix_is_a_usage_error_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd.args(["get", "FOO0001"]).assert().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("INC") && stderr.contains("SIR"), "{stderr}");
    assert!(stderr.contains("table:number"), "{stderr}");
}

#[test]
fn get_bare_sys_id_is_refused_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd.args(["get", HEX]).assert().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("names no table"), "{stderr}");
}

#[test]
fn get_rejects_output_raw_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args(["--output", "raw", "get", "INC0010001"])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("no single envelope"), "{stderr}");
}

// ------------------------------------------------------------- table get ---

#[tokio::test(flavor = "current_thread")]
async fn table_get_resolves_a_number_ref_via_the_canary_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_query", "number=INC0010001"))
        .and(query_param("sysparm_fields", "sys_id"))
        .and(query_param("sysparm_limit", "2"))
        .and(query_param("sysparm_display_value", "false"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": RESOLVED}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{RESOLVED}")))
        .and(query_param("sysparm_fields", "number"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"number": "INC0010001"}})),
        )
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "table",
                "get",
                "incident:INC0010001",
                "--fields",
                "number",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.trim(), r#"{"number":"INC0010001"}"#);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn table_get_sys_id_ref_goes_straight_to_the_record() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{HEX}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": HEX}})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", "get", &format!("incident:{HEX}")])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[test]
fn table_get_ref_plus_second_positional_is_refused_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args(["table", "get", &format!("incident:{HEX}"), "extra"])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("give the record once"), "{stderr}");
}

#[test]
fn table_get_bare_single_token_names_both_forms_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd.args(["table", "get", "incident"]).assert().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("missing SYS_ID"), "{stderr}");
}

// ----------------------------------------------------------- implied verb ---

#[tokio::test(flavor = "current_thread")]
async fn implied_verb_reads_a_colon_token_as_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{HEX}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": HEX}})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", &format!("incident:{HEX}")])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn implied_verb_still_reads_a_bare_noun_as_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": "a"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", "incident"]).assert().success();
    })
    .await
    .unwrap();
}

// ----------------------------------------------------------- delete guard ---

#[test]
fn delete_by_number_ref_requires_yes_before_any_network() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args(["table", "delete", "incident:INC0010001"])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("delete incident:INC0010001 requires --yes when stdin is not a terminal"),
        "{stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delete_by_number_ref_resolves_then_deletes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_query", "number=INC0010001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"sys_id": RESOLVED}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/now/table/incident/{RESOLVED}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", "delete", "incident:INC0010001", "--yes"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

// ------------------------------------------------------- attachment upload ---

#[tokio::test(flavor = "current_thread")]
async fn attachment_upload_accepts_a_record_ref_without_table() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/attachment/file"))
        .and(query_param("table_name", "incident"))
        .and(query_param("table_sys_id", HEX))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"result": {"sys_id": "att1"}})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    let file = tmp.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "attachment",
            "upload",
            "--record",
            &format!("incident:{HEX}"),
            "--file",
            file.to_str().unwrap(),
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

#[test]
fn attachment_upload_table_plus_ref_is_refused_offline() {
    let tmp = offline_profile();
    let file = tmp.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args([
            "attachment",
            "upload",
            "--table",
            "incident",
            "--record",
            &format!("incident:{HEX}"),
            "--file",
            file.to_str().unwrap(),
        ])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("give the table once"), "{stderr}");
}

#[test]
fn attachment_upload_bare_record_still_requires_table_offline() {
    let tmp = offline_profile();
    let file = tmp.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args([
            "attachment",
            "upload",
            "--record",
            "abc",
            "--file",
            file.to_str().unwrap(),
        ])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("--table is required"), "{stderr}");
}

// ------------------------------------------------------------- other sites ---

#[test]
fn open_print_url_takes_a_sys_id_ref_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args(["open", &format!("incident:{HEX}"), "--print-url"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(&format!("sys_id%3D{HEX}")), "{stdout}");
}

#[tokio::test(flavor = "current_thread")]
async fn cmdb_get_takes_a_class_ref() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/cmdb/instance/cmdb_ci_server/{HEX}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": HEX}})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["cmdb", "get", &format!("cmdb_ci_server:{HEX}")])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cmdb_relation_delete_ref_shifts_the_relation_slot() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/now/cmdb/instance/cmdb_ci_server/{HEX}/relation/rel1"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "cmdb",
            "relation",
            "delete",
            &format!("cmdb_ci_server:{HEX}"),
            "rel1",
            "--yes",
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

#[test]
fn cmdb_relation_delete_ref_without_relation_is_refused_offline() {
    let tmp = offline_profile();
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd
        .args([
            "cmdb",
            "relation",
            "delete",
            &format!("cmdb_ci_server:{HEX}"),
            "--yes",
        ])
        .assert()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("missing REL_SYS_ID"), "{stderr}");
}

#[tokio::test(flavor = "current_thread")]
async fn variables_get_takes_a_ref() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/question_answer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [
            {"question.name": "env", "question.question_text": "Environment", "value": "prod"},
        ]})))
        .mount(&server)
        .await;

    let tmp = profile_for(&server.uri());
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "variables", "get", &format!("incident:{HEX}")])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(stdout.contains(r#""name":"env""#), "{stdout}");
    })
    .await
    .unwrap();
}
