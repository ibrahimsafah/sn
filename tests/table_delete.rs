mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn delete_with_yes_succeeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/now/table/incident/abc"))
        .respond_with(ResponseTemplate::new(204))
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
            .args(["table", "delete", "incident", "abc", "--yes"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.trim(), "");
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn delete_without_yes_in_non_tty_errors() {
    tokio::task::spawn_blocking(move || {
        let tmp = write_profiles(
            "test",
            &[ProfileSpec {
                name: "test",
                instance: "http://127.0.0.1:1",
                username: "u",
                password: "p",
            }],
        );
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["table", "delete", "incident", "abc"])
            .assert()
            .code(1);
        drop(tmp);
    })
    .await
    .unwrap();
}
