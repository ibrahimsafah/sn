mod common;

use common::{sn_cmd, write_oauth_profile, write_profiles, ProfileSpec};
use serde_json::json;
use sn::config::now_unix;
use wiremock::matchers::{basic_auth, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn ping_ok() {
    let server = wiremock::MockServer::start().await;
    // The impersonation-session endpoint is ping's primary probe: one request
    // that proves the credentials authenticate and names who they are.
    Mock::given(method("GET"))
        .and(path("/api/now/sg/impersonation/session"))
        .and(basic_auth("u", "p"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "CanImpersonate": false,
            "CurrentUser": "api.user",
            "OriginalUser": "api.user",
            "admin": false,
        })))
        .expect(1)
        .mount(&server)
        .await;
    // ping's best-effort sys_properties probe 404s harmlessly (unmatched).
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = write_profiles(
            "p1",
            &[ProfileSpec {
                name: "p1",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        let assert = sn_cmd(tmp.path()).arg("ping").assert().success();
        let out: serde_json::Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("ping emits JSON on stdout");
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["profile"], json!("p1"));
        // The instance's answer, not the profile's `username = "u"` — see
        // tests/identity.rs for why the configured value is not evidence.
        assert_eq!(out["username"], json!("api.user"));
        assert!(out["latency_ms"].is_u64());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn ping_names_the_user_on_an_oauth_profile() {
    // An OAuth profile stores no username — the identity is in the token — so
    // reporting `profile.username` printed an empty string. Ask the instance.
    //
    // Neither undocumented endpoint is mounted, so this also covers a bearer
    // credential reaching the last rung of the fallback chain.
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "user_name=javascript:gs.getUserName()",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"result": [{"user_name": "sso.user"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let tmp = write_oauth_profile("sso", &server.uri(), "cid", (now_unix() + 3600) as i64);
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let assert = sn_cmd(&dir).arg("ping").assert().success();
        let out: serde_json::Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("ping emits JSON on stdout");
        assert_eq!(out["ok"], json!(true));
        assert_eq!(
            out["username"],
            json!("sso.user"),
            "oauth ping must name the authenticated user, not an empty string"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn ping_401_exit_4() {
    // 401 at the last rung of the identity chain: the two undocumented
    // endpoints are unmounted (404 → fall back), and the `sys_user` read that
    // remains rejects the credentials. tests/identity.rs covers a 401 raised at
    // each earlier rung, where the rule is that ping must *not* keep shopping.
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "nope"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    // The 401 aborts ping before its sys_properties probe.
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_properties"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(0)
        .mount(&server)
        .await;
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = write_profiles(
            "p1",
            &[ProfileSpec {
                name: "p1",
                instance: &server_uri,
                username: "u",
                password: "p",
            }],
        );
        sn_cmd(tmp.path()).arg("ping").assert().code(4);
    })
    .await
    .unwrap();
}
