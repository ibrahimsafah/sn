mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn list_default_unwraps_result() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_limit", "5"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"number": "INC1"}, {"number": "INC2"}]})),
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
            .args(["table", "list", "incident", "--setlimit", "5", "--compact"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.trim(), r#"[{"number":"INC1"},{"number":"INC2"}]"#);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn list_raw_preserves_envelope() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [{"n": 1}]})))
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
            .args(["--output", "raw", "--compact", "table", "list", "incident"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.trim(), r#"{"result":[{"n":1}]}"#);
    })
    .await
    .unwrap();
}
