//! Destructive commands share one confirmation guard
//! (`cli::table::confirm_destructive`): with `--yes` the request proceeds;
//! without it, a non-interactive stdin is refused (exit 1 + JSON usage envelope
//! on stderr) rather than acting silently. assert_cmd runs the binary with a
//! non-TTY stdin, so the no-`--yes` path here is exactly the guard path.
//!
//! "Destructive" is wider than `delete`: `updateset back-out` and `app rollback`
//! undo instance state rather than removing a row, and are gated too — the verb
//! in the refusal is the command's own, so the message stays honest.

mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn stderr_envelope(out: &assert_cmd::assert::Assert) -> Value {
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
        panic!("stderr is not the JSON error envelope ({e}): {stderr}");
    })
}

fn one_profile(instance: &str) -> tempfile::TempDir {
    write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance,
            username: "u",
            password: "p",
        }],
    )
}

/// Every guarded command must refuse a non-TTY stdin without `--yes`: exit 1
/// and a usage envelope naming both the `--yes` requirement and the operation
/// it is refusing, before any network call. The instance points at a closed
/// port, so a guard that ran too late would surface as a transport error.
fn assert_guarded(args: &[&str], verb: &str) {
    let tmp = one_profile("http://127.0.0.1:1");
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd.args(args).assert().code(1);
    let v = stderr_envelope(&out);
    let msg = v["error"]["message"].as_str().unwrap();
    assert_eq!(
        msg,
        format!("{verb} requires --yes when stdin is not a terminal"),
        "unexpected message for {args:?}"
    );
    drop(tmp);
}

#[test]
fn change_delete_without_yes_is_guarded() {
    assert_guarded(&["change", "delete", "chg001"], "delete");
}

#[test]
fn change_task_delete_without_yes_is_guarded() {
    assert_guarded(&["change", "task", "delete", "chg001", "task001"], "delete");
}

#[test]
fn attachment_delete_without_yes_is_guarded() {
    assert_guarded(&["attachment", "delete", "att001"], "delete");
}

#[test]
fn cmdb_relation_delete_without_yes_is_guarded() {
    assert_guarded(
        &[
            "cmdb",
            "relation",
            "delete",
            "cmdb_ci_server",
            "ci001",
            "rel001",
        ],
        "delete",
    );
}

#[test]
fn catalog_cart_remove_without_yes_is_guarded() {
    assert_guarded(&["catalog", "cart-remove", "item001"], "delete");
}

#[test]
fn catalog_cart_empty_without_yes_is_guarded() {
    assert_guarded(&["catalog", "cart-empty", "cart001"], "empty");
}

#[test]
fn change_conflict_remove_without_yes_is_guarded() {
    assert_guarded(&["change", "conflict", "remove", "chg001"], "delete");
}

#[test]
fn profile_remove_without_yes_is_guarded() {
    assert_guarded(&["profile", "remove", "test"], "delete");
}

#[test]
fn updateset_back_out_without_yes_is_guarded() {
    assert_guarded(
        &["updateset", "back-out", "--update-set-id", "us001"],
        "back out",
    );
}

#[test]
fn app_rollback_without_yes_is_guarded() {
    assert_guarded(
        &[
            "app",
            "rollback",
            "--scope",
            "x_acme_app",
            "--version",
            "1.0",
        ],
        "roll back",
    );
}

/// `change conflict get` shares the endpoint but not the args struct, so the
/// read must not have grown a `--yes` (nor the gate that flag implies).
#[test]
fn change_conflict_get_is_not_gated() {
    let tmp = one_profile("http://127.0.0.1:1");
    let mut cmd = sn_cmd(tmp.path());
    let out = cmd.args(["change", "conflict", "get", "chg001"]).assert();
    let v = stderr_envelope(&out);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        !msg.contains("--yes"),
        "a read command asked for confirmation: {msg}"
    );
    drop(tmp);
}

#[tokio::test(flavor = "current_thread")]
async fn change_delete_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/sn_chg_rest/change/chg001"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["change", "delete", "chg001", "--yes"])
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
async fn change_task_delete_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/sn_chg_rest/change/chg001/task/task001"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["change", "task", "delete", "chg001", "task001", "--yes"])
            .assert()
            .success();
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn attachment_delete_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/now/attachment/att001"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["attachment", "delete", "att001", "-y"])
            .assert()
            .success();
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_cart_empty_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/sn_sc/servicecatalog/cart/cart001/empty"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["catalog", "cart-empty", "cart001", "--yes"])
            .assert()
            .success();
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_cart_remove_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/sn_sc/servicecatalog/cart/item001"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["catalog", "cart-remove", "item001", "-y"])
            .assert()
            .success();
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_conflict_remove_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/sn_chg_rest/change/chg001/conflict"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args(["change", "conflict", "remove", "chg001", "--yes"])
            .assert()
            .success();
        drop(tmp);
    })
    .await
    .unwrap();
}

/// The two non-`delete` gates still reach the API with `--yes`: the guard is a
/// front door, not a replacement for the call.
#[tokio::test(flavor = "current_thread")]
async fn updateset_back_out_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/update_set/back_out"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": { "links": { "progress": { "id": "p1" } }, "status": "1" }
        })))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "updateset",
                "back-out",
                "--update-set-id",
                "us001",
                "--yes",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(stdout.contains("\"p1\""), "unexpected stdout: {stdout}");
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn app_rollback_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/app_repo/rollback"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": { "links": { "progress": { "id": "p2" } }, "status": "1" }
        })))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "app",
                "rollback",
                "--scope",
                "x_acme_app",
                "--version",
                "1.0",
                "--yes",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(stdout.contains("\"p2\""), "unexpected stdout: {stdout}");
        drop(tmp);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cmdb_relation_delete_with_yes_proceeds() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(
            "/api/now/cmdb/instance/cmdb_ci_server/ci001/relation/rel001",
        ))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = one_profile(&uri);
        let mut cmd = sn_cmd(tmp.path());
        cmd.args([
            "cmdb",
            "relation",
            "delete",
            "cmdb_ci_server",
            "ci001",
            "rel001",
            "--yes",
        ])
        .assert()
        .success();
        drop(tmp);
    })
    .await
    .unwrap();
}
