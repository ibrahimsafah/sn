mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

// ── progress ─────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn progress_get_unwraps_result() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/progress/prog123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            // ServiceNow sends `percent_complete` as a snake_case *string*. An
            // earlier camelCase `percentComplete` here was read by nothing and
            // asserted by nothing — it just sat around looking like the API
            // shape, and the docs were written from it.
            "result": {
                "links": {},
                "percent_complete": "100",
                "status": "2",
                "status_detail": "Completed",
                "status_label": "Complete"
            }
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "progress", "prog123"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["status_label"], "Complete");
        assert!(!stdout.contains("\"result\""), "should unwrap result");
    })
    .await
    .unwrap();
}

// ── app install ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn app_install_posts_with_scope_query_param() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/app_repo/install"))
        .and(query_param("scope", "x_acme_myapp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"links": {}, "status": "0", "status_label": "Pending"}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "app", "install", "--scope", "x_acme_myapp"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["status_label"], "Pending");
    })
    .await
    .unwrap();
}

// ── update-set create ────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn update_set_create_posts_with_name_param() {
    // The CLI flag is `--name`, but the API's query parameter is
    // `update_set_name` (Required, per the CICD Update Set API docs). This mock
    // only matches the correct wire name, so it guards against a regression to
    // the ignored-`name` param.
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/update_set/create"))
        .and(query_param("update_set_name", "My Update Set"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"sys_id": "us001", "name": "My Update Set"}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "updateset",
                "create",
                "--name",
                "My Update Set",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["sys_id"], "us001");
    })
    .await
    .unwrap();
}

// ── update-set retrieve ──────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn update_set_retrieve_uses_update_source_param_names() {
    // The source selectors map to the API's `update_source_id` /
    // `update_source_instance_id` query parameters (per the CICD Update Set API
    // docs), NOT the shorter `source_id` / `source_instance_id`. ServiceNow
    // silently ignores unknown query params, so this mock — which matches only
    // the correct names — is the guard against that silent-no-op regression.
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/update_set/retrieve"))
        .and(query_param("update_set_id", "us_remote_1"))
        .and(query_param("update_source_id", "src_rec_1"))
        .and(query_param("update_source_instance_id", "src_inst_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"links": {}, "status": "0", "status_label": "Pending"}
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args([
                "--compact",
                "updateset",
                "retrieve",
                "--update-set-id",
                "us_remote_1",
                "--update-source-id",
                "src_rec_1",
                "--update-source-instance-id",
                "src_inst_1",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["status_label"], "Pending");
    })
    .await
    .unwrap();
}

// ── atf run ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn atf_run_posts_with_suite_name_param() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/testsuite/run"))
        .and(query_param("test_suite_name", "MySuite"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "links": {},
                "status": "0",
                "status_label": "Pending",
                "test_suite_name": "MySuite"
            }
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "atf", "run", "--suite-name", "MySuite"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["test_suite_name"], "MySuite");
    })
    .await
    .unwrap();
}

// ── aggregate ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn aggregate_count_incident() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/incident"))
        .and(query_param("sysparm_count", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "stats": {
                    "count": "42"
                }
            }
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "aggregate", "incident", "--count"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["stats"]["count"], "42");
    })
    .await
    .unwrap();
}

// ── scores list ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn scores_list_per_page() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/pa/scorecards"))
        .and(query_param("sysparm_per_page", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": []
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "scores", "list", "--per-page", "5"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert!(v.is_array());
    })
    .await
    .unwrap();
}

// ── atf results ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn atf_results_get_unwraps_result() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/testsuite/results/res456"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "id": "res456",
                "status": "success",
                "tests_total": 10,
                "tests_passed": 10
            }
        })))
        .mount(&server)
        .await;
    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let mut cmd = sn_cmd(tmp.path());
        let out = cmd
            .args(["--compact", "atf", "results", "res456"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["status"], "success");
        assert_eq!(v["tests_passed"], 10);
        assert!(!stdout.contains("\"result\""), "should unwrap result");
    })
    .await
    .unwrap();
}

// ── --wait flag ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn app_install_wait_polls_until_complete() {
    let server = wiremock::MockServer::start().await;

    // Initial install POST returns progress link
    Mock::given(method("POST"))
        .and(path("/api/sn_cicd/app_repo/install"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "links": {
                    "progress": {
                        "id": "prog123"
                    }
                },
                "status": "0",
                "status_label": "Pending"
            }
        })))
        .mount(&server)
        .await;

    // Progress poll: already complete
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/progress/prog123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "status": "2",
                "status_label": "Succeeded",
                "percent_complete": "100",
                "status_message": "Install complete"
            }
        })))
        .mount(&server)
        .await;

    let server_uri = server.uri();
    let tmp = write_profiles(
        "test",
        &[ProfileSpec {
            name: "test",
            instance: &server_uri,
            username: "u",
            password: "p",
        }],
    );
    tokio::task::spawn_blocking(move || {
        let out = sn_cmd(tmp.path())
            .args(["--compact", "app", "install", "--scope", "x_test", "--wait"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert!(
            v["status_label"] == "Succeeded" || v["status_message"] == "Install complete",
            "expected final progress result, got: {stdout}"
        );
    })
    .await
    .unwrap();
}
