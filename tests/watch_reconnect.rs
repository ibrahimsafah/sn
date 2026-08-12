//! End-to-end guards for the `sn watch` reconnect marker.
//!
//! The marker's own rules — it is emitted once per gap, it does not count
//! against `--max-events`, it does not reset `--idle-timeout` — are unit-tested
//! in `sn::cli::watch`, because driving a real AMB drop needs a Bayeux
//! websocket server and wiremock speaks only HTTP.
//!
//! What is worth asserting from outside the process is the negative: a watch
//! that never reached a subscribed channel has no gap to report, so stdout must
//! stay empty. A marker there would be worse than no marker at all — it would
//! tell a consumer that events were missed on a stream that never carried any.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_watch_that_never_connects_emits_no_reconnect_marker() {
    // The session mint succeeds (cookie and all), so the upgrade is attempted —
    // against a plain HTTP mock that cannot speak websocket. `established` stays
    // false, so the supervisor fails fast and nothing is ever written.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Set-Cookie", "JSESSIONID=FAKE; Path=/; HttpOnly")
                .set_body_json(serde_json::json!({"result": [{"sys_id": "abc"}]})),
        )
        .mount(&server)
        .await;

    let tmp = write_profiles(
        "p",
        &[ProfileSpec {
            name: "p",
            instance: &server.uri(),
            username: "admin",
            password: "pw",
        }],
    );
    let out = tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args(["watch", "table", "incident", "-q", "active=true"])
            .assert()
            .failure()
            .code(3) // transport
            .get_output()
            .clone()
    })
    .await
    .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.is_empty(),
        "a stream that never started has no gap to announce: {stdout}"
    );
}
