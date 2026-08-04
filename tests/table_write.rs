mod common;

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn update_sends_patch_with_only_named_fields() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/now/table/incident/abc"))
        .and(body_partial_json(json!({"state": 2})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": {"sys_id": "abc", "state": "2"}})),
        )
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = common::write_profiles(
            "test",
            &[common::ProfileSpec {
                name: "test",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = common::sn_cmd(tmp.path());
        cmd.args([
            "--compact",
            "table",
            "update",
            "incident",
            "abc",
            "--field",
            "state=2",
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn update_accepts_data_at_file_with_multiline_body() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/now/table/incident/abc"))
        .and(body_partial_json(
            json!({"description": "line one\nline two\nline three"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
        )
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().unwrap();
        let body_path = dir.path().join("body.json");
        std::fs::write(
            &body_path,
            "{\n  \"description\": \"line one\\nline two\\nline three\"\n}\n",
        )
        .unwrap();
        let data_arg = format!("@{}", body_path.to_str().unwrap());
        let tmp = common::write_profiles(
            "test",
            &[common::ProfileSpec {
                name: "test",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = common::sn_cmd(tmp.path());
        cmd.args([
            "--compact",
            "table",
            "update",
            "incident",
            "abc",
            "--data",
            &data_arg,
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn update_accepts_field_at_file_for_multiline_value() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/now/table/incident/abc"))
        .and(body_partial_json(
            json!({"work_notes": "first line\nsecond line"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
        )
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().unwrap();
        let value_path = dir.path().join("notes.txt");
        std::fs::write(&value_path, "first line\nsecond line").unwrap();
        let field_arg = format!("work_notes=@{}", value_path.to_str().unwrap());
        let tmp = common::write_profiles(
            "test",
            &[common::ProfileSpec {
                name: "test",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = common::sn_cmd(tmp.path());
        cmd.args([
            "--compact",
            "table",
            "update",
            "incident",
            "abc",
            "--field",
            &field_arg,
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

/// `replace` (PUT) was removed in 0.11.0 after a live instance showed PUT and
/// PATCH leaving a record in byte-identical states — ServiceNow treats both as
/// partial updates. Anything that reached for `replace` should use `update`.
#[tokio::test(flavor = "current_thread")]
async fn replace_is_no_longer_a_subcommand() {
    let tmp = common::write_profiles(
        "test",
        &[common::ProfileSpec {
            name: "test",
            instance: "https://example.invalid",
            username: "u",
            password: "p",
        }],
    );
    let mut cmd = common::sn_cmd(tmp.path());
    cmd.args(["table", "replace", "incident", "abc", "--data", "{}"])
        .assert()
        .failure()
        .code(1);
}
