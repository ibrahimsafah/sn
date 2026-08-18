mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn paginates_following_link_header() {
    let server = wiremock::MockServer::start().await;
    let next_link = format!(
        "<{}/api/now/table/incident?sysparm_offset=2>;rel=\"next\"",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_limit", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", next_link.as_str())
                .set_body_json(json!({"result": [{"n": 1}, {"n": 2}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": [{"n": 3}]})))
        .expect(1)
        .mount(&server)
        .await;

    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = write_profiles(
            "test",
            &[ProfileSpec {
                name: "test",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["table", "list", "incident", "--setlimit", "2", "--all"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.lines().count(), 3);
        assert!(stdout.contains("\"n\":1"));
        assert!(stdout.contains("\"n\":3"));
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn max_records_caps_output() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"n": 1}, {"n": 2}, {"n": 3}]})),
        )
        .mount(&server)
        .await;

    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = write_profiles(
            "test",
            &[ProfileSpec {
                name: "test",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["table", "list", "incident", "--all", "--max-records", "2"])
            .assert()
            .success();
        assert_eq!(
            String::from_utf8(out.get_output().stdout.clone())
                .unwrap()
                .lines()
                .count(),
            2
        );
        drop(tmp);
    })
    .await
    .unwrap();
}
