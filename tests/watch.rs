//! End-to-end CLI tests for `sn watch` (AMB record watchers).
//!
//! The websocket itself is not mocked — wiremock speaks HTTP, not Bayeux. What
//! *is* covered here is everything that guards the socket: argument validation,
//! the session-minting call that has to happen before the upgrade, and the
//! fail-fast behavior when a connection was never established. The Bayeux
//! protocol proper (channel encoding, the session-status trap, the long-poll) is
//! unit-tested in `sn::amb`.

mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::Value;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile_at(instance: &str) -> tempfile::TempDir {
    write_profiles(
        "p",
        &[ProfileSpec {
            name: "p",
            instance,
            username: "admin",
            password: "pw",
        }],
    )
}

fn err_of(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stderr).expect("stderr should be the JSON error envelope")
}

// ── argument validation (no network) ────────────────────────────────────────

#[test]
fn watch_requires_a_query() {
    // Watching a whole table with no filter would be a firehose keyed off an
    // empty channel name; clap must demand the one targeting flag. As a plain
    // required argument (not a two-alternative group), the error names exactly
    // what to add.
    let tmp = profile_at("https://example.invalid");
    let out = sn_cmd(tmp.path())
        .args(["watch", "incident"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.contains("--query"),
        "must name the missing target: {stderr}"
    );
}

#[test]
fn the_removed_targeting_and_hydration_flags_are_unknown() {
    // 0.13.0 removed --sys-id, --hydrate, --fields and --display-value. They
    // must fail as unknown flags, not silently parse: --sys-id has a rewrite
    // (`-q "sys_id=…"`), and hydration changed what `record` held, so a script
    // still passing them needs to find out here, not from a changed stream.
    let tmp = profile_at("https://example.invalid");
    for flag in [
        "--sys-id=abc123",
        "--hydrate",
        "--fields=number",
        "--display-value=all",
    ] {
        sn_cmd(tmp.path())
            .args(["watch", "incident", "-q", "active=true", flag])
            .assert()
            .failure()
            .code(1);
    }
}

#[test]
fn watch_rejects_an_unknown_operation() {
    let tmp = profile_at("https://example.invalid");
    sn_cmd(tmp.path())
        .args([
            "watch",
            "incident",
            "-q",
            "active=true",
            "--operation",
            "upsert",
        ])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn no_hydrate_is_still_accepted_as_a_no_op() {
    // 0.9.1 scripts pass --no-hydrate. It describes the only behavior left, so
    // it must parse rather than exit 1 on an unknown flag. It gets as far as
    // the network (exit 3 against an address nothing answers on), which proves
    // it parsed.
    let tmp = profile_at("https://127.0.0.1:1");
    sn_cmd(tmp.path())
        .args([
            "watch",
            "incident",
            "-q",
            "active=true",
            "--no-hydrate",
            "--max-events",
            "1",
        ])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn watch_refuses_a_proxy_rather_than_silently_bypassing_it() {
    // The socket is opened directly, so a configured proxy would be ignored —
    // quietly sending the session cookie outside the sanctioned egress path.
    let tmp = profile_at("https://example.invalid");
    let out = sn_cmd(tmp.path())
        .args([
            "watch",
            "incident",
            "-q",
            "active=true",
            "--proxy",
            "http://127.0.0.1:9",
        ])
        .assert()
        .failure()
        .code(1);
    let msg = err_of(out.get_output())["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("proxy"), "{msg}");
    assert!(msg.contains("--no-proxy"), "must offer a way out: {msg}");
}

// ── the session-minting call (the AMB quirk) ────────────────────────────────

#[tokio::test]
async fn watch_surfaces_an_auth_failure_from_the_session_mint() {
    // AMB authenticates by session cookie, so `watch` must first make an
    // ordinary authenticated call. If that 401s there is no session to carry
    // onto the socket, and the caller gets the normal auth exit code.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {"message": "User Not Authenticated"}
        })))
        .mount(&server)
        .await;

    let tmp = profile_at(&server.uri());
    let out = tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args(["watch", "incident", "-q", "active=true"])
            .assert()
            .failure()
            .code(4)
            .get_output()
            .clone()
    })
    .await
    .unwrap();

    assert_eq!(err_of(&out)["error"]["status_code"], 401);
}

#[tokio::test]
async fn watch_fails_when_the_instance_mints_no_session_cookie() {
    // A 200 with no Set-Cookie means there is no session to authenticate the
    // upgrade with. Going ahead would produce a socket that connects, handshakes
    // "successfully", and then silently never delivers an event — so refuse.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wm_path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"sys_id": "abc"}]
        })))
        .mount(&server)
        .await;

    let tmp = profile_at(&server.uri());
    let out = tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args(["watch", "incident", "-q", "active=true"])
            .assert()
            .failure()
            .code(4)
            .get_output()
            .clone()
    })
    .await
    .unwrap();

    let msg = err_of(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("JSESSIONID"),
        "must name the missing cookie: {msg}"
    );
}

#[tokio::test]
async fn watch_fails_fast_when_the_socket_never_connects() {
    // The mint succeeds, so the upgrade is attempted — against a plain HTTP mock
    // that cannot speak websocket. A connection that was *never* established must
    // report immediately rather than burn ten backoff rounds (~6 minutes) on a
    // configuration that cannot work.
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

    let tmp = profile_at(&server.uri());
    let started = std::time::Instant::now();
    let out = tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args(["watch", "incident", "-q", "active=true"])
            .assert()
            .failure()
            .code(3) // transport
            .get_output()
            .clone()
    })
    .await
    .unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must fail fast, not retry a connection that never worked (took {:?})",
        started.elapsed()
    );
    let msg = err_of(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("amb"), "should point at the websocket: {msg}");
}
