mod common;

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
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

const RITM: &str = "4069e3df2f6acb107efd1d707fa4e32b";
const TASK: &str = "081aab132faacb107efd1d707fa4e34b";
const INC: &str = "ba3a27532faacb107efd1d707fa4e351";

/// One `sc_item_option_mtom` join row as the Table API returns it.
fn mtom_row(name: &str, label: &str, value: &str) -> serde_json::Value {
    json!({
        "sc_item_option.item_option_new.name": name,
        "sc_item_option.item_option_new.question_text": label,
        "sc_item_option.value": value,
    })
}

fn mtom_response(rows: &[serde_json::Value]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "result": rows }))
}

#[tokio::test(flavor = "current_thread")]
async fn get_reads_the_mtom_join_sorted_and_skips_containers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .and(query_param("sysparm_query", format!("request_item={RITM}")))
        .respond_with(mtom_response(&[
            mtom_row("photoshop", "Adobe Photoshop", "false"),
            mtom_row("acrobat", "Adobe Acrobat", "true"),
            mtom_row("", "Optional Software", ""), // container row: no name
        ]))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "variables", "get", "sc_req_item", RITM])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2, "container row must be dropped");
        assert_eq!(rows[0]["name"], "acrobat");
        assert_eq!(rows[0]["label"], "Adobe Acrobat");
        assert_eq!(rows[0]["value"], "true");
        assert_eq!(rows[1]["name"], "photoshop");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn set_writes_flat_body_and_reports_the_diff() {
    let server = MockServer::start().await;
    // Pre-flight read sees the old value…
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[
            mtom_row("acrobat", "Adobe Acrobat", "true"),
            mtom_row("photoshop", "Adobe Photoshop", "false"),
        ]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // …the PUT carries the flat body, no wrapper (--field coerces JSON
    // scalars, so `acrobat=false` travels as a boolean)…
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/variables/sc_req_item/{RITM}"
        )))
        .and(body_json(json!({ "acrobat": false, "photoshop": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .expect(1)
        .mount(&server)
        .await;
    // …and the read-back sees the new value.
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[
            mtom_row("acrobat", "Adobe Acrobat", "false"),
            mtom_row("photoshop", "Adobe Photoshop", "false"),
        ]))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "variables",
                "set",
                "sc_req_item",
                RITM,
                "--field",
                "acrobat=false",
                "--field",
                "photoshop=false",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["table"], "sc_req_item");
        assert_eq!(v["updated"]["acrobat"]["from"], "true");
        assert_eq!(v["updated"]["acrobat"]["to"], "false");
        assert_eq!(v["unchanged"]["photoshop"], "false");
        assert!(v.get("resolved_from").is_none());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn set_fails_exit_2_when_the_write_did_not_persist() {
    let server = MockServer::start().await;
    // Both reads return the same value: the endpoint 200'd but wrote nothing.
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[mtom_row(
            "acrobat",
            "Adobe Acrobat",
            "true",
        )]))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/variables/sc_req_item/{RITM}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "variables",
                "set",
                "sc_req_item",
                RITM,
                "--field",
                "acrobat=false",
            ])
            .assert()
            .code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("did not persist"), "stderr: {stderr}");
        assert!(stderr.contains("acrobat"), "stderr: {stderr}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn set_rejects_unknown_names_before_writing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[mtom_row(
            "Additional_software_requirements",
            "Additional software requirements",
            "",
        )]))
        .mount(&server)
        .await;
    // The whole point: no PUT may happen on a name mismatch.
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "variables",
                "set",
                "sc_req_item",
                RITM,
                "--field",
                "additional_software_requirements=x", // wrong case
            ])
            .assert()
            .code(1);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("case-sensitive"), "stderr: {stderr}");
        assert!(
            stderr.contains("Additional_software_requirements"),
            "the error must list the real names; stderr: {stderr}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn sc_task_resolves_to_its_request_item() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_task/{TASK}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": { "request_item": { "value": RITM, "link": "x" } }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .and(query_param("sysparm_query", format!("request_item={RITM}")))
        .respond_with(mtom_response(&[mtom_row(
            "acrobat",
            "Adobe Acrobat",
            "true",
        )]))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // The PUT must target the RITM, not the task.
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/variables/sc_req_item/{RITM}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[mtom_row(
            "acrobat",
            "Adobe Acrobat",
            "false",
        )]))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "variables",
                "set",
                "sc_task",
                TASK,
                "--field",
                "acrobat=false",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["table"], "sc_req_item");
        assert_eq!(v["sys_id"], RITM);
        assert_eq!(v["resolved_from"]["table"], "sc_task");
        assert_eq!(v["resolved_from"]["sys_id"], TASK);
        assert_eq!(v["updated"]["acrobat"]["to"], "false");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn record_producer_variables_use_question_answer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/question_answer"))
        .and(query_param(
            "sysparm_query",
            format!("table_name=incident^table_sys_id={INC}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                { "question.name": "contact_me", "question.question_text": "How?", "value": "phone" }
            ]
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "variables", "get", "incident", INC])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v[0]["name"], "contact_me");
        assert_eq!(v[0]["value"], "phone");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn empty_pool_probes_record_existence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(mtom_response(&[]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_req_item/{RITM}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "message": "No Record found", "detail": "" }, "status": "failure"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&server_uri);
        common::sn_cmd(tmp.path())
            .args(["--compact", "variables", "get", "sc_req_item", RITM])
            .assert()
            .code(2);
    })
    .await
    .unwrap();
}
