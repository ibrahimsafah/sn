mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn get_unwraps_single_record() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident/abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": {"sys_id": "abc", "number": "INC1"}})),
        )
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "table", "get", "incident", "abc"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.trim(), r#"{"number":"INC1","sys_id":"abc"}"#);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn get_404_exit_2() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident/missing"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"message": "No Record found"}})),
        )
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server.uri(),
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", "get", "incident", "missing"])
            .assert()
            .code(2);
    })
    .await
    .unwrap();
}
