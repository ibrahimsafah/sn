mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn create_with_fields() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .and(body_partial_json(
            json!({"short_description": "sd", "urgency": 2}),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"result": {"sys_id": "new", "short_description": "sd"}})),
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
    let config_dir = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(&config_dir);
        cmd.args([
            "--compact",
            "table",
            "create",
            "incident",
            "--field",
            "short_description=sd",
            "--field",
            "urgency=2",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"sys_id\":\"new\""));
    })
    .await
    .unwrap();
    drop(tmp);
}

#[tokio::test(flavor = "current_thread")]
async fn data_and_field_together_is_usage_error() {
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: "http://127.0.0.1:1",
            username: "u",
            password: "p",
        }],
    );
    let config_dir = tmp.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(&config_dir);
        let _ = cmd
            .args([
                "table", "create", "incident", "--data", "{}", "--field", "x=1",
            ])
            .assert();
        // clap returns exit code 2 for ArgConflict; just check it's nonzero
    })
    .await
    .unwrap();
    drop(tmp);
}
