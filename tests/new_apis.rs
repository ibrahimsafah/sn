mod common;

use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

// ── change management ────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn change_list_normal() {
    let server = wiremock::MockServer::start().await;
    // The Change API ends every list's result array with a `__meta` element.
    // With no sysparm_query it reports the CLI's own sysparm_* switches (and a
    // bare "") as "ignored" — parameters, not dropped terms, as measured live.
    Mock::given(method("GET"))
        .and(path("/api/sn_chg_rest/change/normal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {"number": "CHG001", "type": "normal"},
                {"__meta": {"encodedQuery": "", "fields": {"applied": [],
                    "ignored": ["", "sysparm_display_value", "sysparm_limit"]}}}
            ]
        })))
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
            .args(["--compact", "change", "list", "--type", "normal"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["number"], "CHG001");
        // `list` returns records only: the meta element is stripped, so length
        // agrees with the record count and `.[].number` has no trailing null.
        assert_eq!(v.as_array().unwrap().len(), 1);
        // A "" in `ignored` is noise, not a dropped term — no warning for it.
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(
            !stderr.contains("warning"),
            "no warning expected, got: {stderr}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_list_warns_when_the_instance_drops_query_terms() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_chg_rest/change/normal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {"number": "CHG001"},
                {"__meta": {"encodedQuery": "state=-5",
                            "fields": {"applied": ["state"], "ignored": ["assigned_two", ""]}}}
            ]
        })))
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
            .args([
                "--compact",
                "change",
                "list",
                "--type",
                "normal",
                "-q",
                "assigned_two=x^state=-5",
            ])
            .assert()
            .success();
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("ignored query field(s): assigned_two"),
            "stderr should name the dropped term, got: {stderr}"
        );
        // The warning names real terms only; the "" entry is filtered out.
        assert!(!stderr.contains("assigned_two, "));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_list_raw_keeps_the_meta_element() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_chg_rest/change/normal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {"number": "CHG001"},
                {"__meta": {"encodedQuery": "", "fields": {"applied": [], "ignored": ["bogus"]}}}
            ]
        })))
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
            .args([
                "--compact",
                "--output",
                "raw",
                "change",
                "list",
                "--type",
                "normal",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        // Raw means the envelope as ServiceNow sent it, meta element included.
        assert_eq!(v["result"].as_array().unwrap().len(), 2);
        assert!(v["result"][1].get("__meta").is_some());
        // The dropped-term warning fires in every output mode.
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(stderr.contains("ignored query field(s): bogus"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_list_refuses_a_sort_clause_before_the_network() {
    // No mock server: the refusal must fire on argv alone. A live call would
    // fail loudly here (connection refused, exit 3), so a passing exit 1
    // proves the gate runs first.
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
    let out = cmd
        .args(["change", "list", "-q", "state=-5^ORDERBYDESCopened_at"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("ORDERBYDESCopened_at"),
        "error should name the clause, got: {stderr}"
    );
    assert!(
        stderr.contains("sn table list change_request"),
        "error should point at the working alternative, got: {stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn change_models_strips_the_meta_element() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_chg_rest/change/model"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                {"sys_id": "m1"},
                {"__meta": {"encodedQuery": "", "fields": {"applied": [], "ignored": []}}}
            ]
        })))
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
            .args(["--compact", "change", "models"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["sys_id"], "m1");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_create_normal() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_chg_rest/change/normal"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "result": {"sys_id": "chg001", "number": "CHG001"}
        })))
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
            .args([
                "--compact",
                "change",
                "create",
                "--type",
                "normal",
                "--field",
                "short_description=Test change",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["number"], "CHG001");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn change_task_list() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_chg_rest/change/chg001/task"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"number": "CTASK001"}]
        })))
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
            .args(["--compact", "change", "task", "list", "chg001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["number"], "CTASK001");
    })
    .await
    .unwrap();
}

// ── attachment ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn attachment_list_with_query() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/attachment"))
        .and(query_param("sysparm_query", "table_name=incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": "att001", "file_name": "log.txt"}]
        })))
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
            .args([
                "--compact",
                "attachment",
                "list",
                "--query",
                "table_name=incident",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["file_name"], "log.txt");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn attachment_get_metadata() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/attachment/att001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"sys_id": "att001", "file_name": "log.txt", "size_bytes": "1024"}
        })))
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
            .args(["--compact", "attachment", "get", "att001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["file_name"], "log.txt");
    })
    .await
    .unwrap();
}

/// `attachment download` used to panic (exit 101) on EVERY invocation — flag or
/// no flag. Its local `--output <PATH>` (a String) collided with the clap-global
/// `--output default|raw|table` (an OutputMode): clap merges args by id, so the
/// local definition shadowed the global one's type and `GlobalFlags` then tried
/// to downcast an `OutputMode` out of a `String`. Nothing exercised the command,
/// so a total crash shipped. The flag is `--out`/`-o` now.
///
/// The load-bearing assertion is simply that these RUN: a panic is exit 101, and
/// `.success()` / `.code(n)` catches it no matter what the body says.
#[tokio::test(flavor = "current_thread")]
async fn attachment_download_writes_the_file_and_does_not_panic() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/attachment/att001/file"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"hello-bytes".to_vec(), "application/octet-stream"),
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
        let dest = tmp.path().join("downloaded.bin");
        let dest_str = dest.to_str().unwrap().to_string();

        // --out writes to disk and reports where.
        let out = sn_cmd(tmp.path())
            .args(["--compact", "attachment", "download", "att001", "--out"])
            .arg(&dest_str)
            .assert()
            .success();
        let v: serde_json::Value =
            serde_json::from_slice(&out.get_output().stdout).expect("emits JSON on stdout");
        assert_eq!(v["size"], 11);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello-bytes");

        // No flag at all: the bytes go to stdout. This is the exact invocation
        // that used to panic before parsing even finished.
        let out = sn_cmd(tmp.path())
            .args(["attachment", "download", "att001"])
            .assert()
            .success();
        assert_eq!(out.get_output().stdout, b"hello-bytes");

        // The global --output must still be usable on this command — that
        // coexistence is the whole point of the rename.
        sn_cmd(tmp.path())
            .args(["--output", "raw", "attachment", "download", "att001", "-o"])
            .arg(&dest_str)
            .assert()
            .success();
    })
    .await
    .unwrap();
}

// ── cmdb ─────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn cmdb_list_servers() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/cmdb/instance/cmdb_ci_server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": "ci001", "name": "web-server-01"}]
        })))
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
            .args(["--compact", "cmdb", "list", "cmdb_ci_server"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["name"], "web-server-01");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cmdb_meta() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/cmdb/meta/cmdb_ci_server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"name": "cmdb_ci_server", "label": "Server"}
        })))
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
            .args(["--compact", "cmdb", "meta", "cmdb_ci_server"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["label"], "Server");
    })
    .await
    .unwrap();
}

// ── import set ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn import_create_record() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/import/u_staging_table"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "result": [{"sys_id": "imp001", "status": "inserted"}]
        })))
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
            .args([
                "--compact",
                "import",
                "create",
                "u_staging_table",
                "--field",
                "u_name=test",
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["status"], "inserted");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn import_get_record() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/import/u_staging_table/imp001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"sys_id": "imp001", "u_name": "test"}
        })))
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
            .args(["--compact", "import", "get", "u_staging_table", "imp001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["u_name"], "test");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn import_bulk_wraps_array_into_records() {
    let server = wiremock::MockServer::start().await;
    // The README-documented bare-array form must reach the API wrapped as
    // {"records": [...]} — the shape insertMultiple requires.
    Mock::given(method("POST"))
        .and(path("/api/now/import/u_staging_table/insertMultiple"))
        .and(body_json(json!({
            "records": [{"u_name": "Server-01"}, {"u_name": "Server-02"}]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "result": [{"status": "inserted"}, {"status": "inserted"}]
        })))
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
            .args([
                "--compact",
                "import",
                "bulk",
                "u_staging_table",
                "--data",
                r#"[{"u_name":"Server-01"},{"u_name":"Server-02"}]"#,
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["status"], "inserted");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn import_bulk_accepts_prewrapped_records_object() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/import/u_staging_table/insertMultiple"))
        .and(body_json(json!({"records": [{"u_name": "Server-03"}]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "result": [{"status": "inserted"}]
        })))
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
        cmd.args([
            "--compact",
            "import",
            "bulk",
            "u_staging_table",
            "--data",
            r#"{"records":[{"u_name":"Server-03"}]}"#,
        ])
        .assert()
        .success();
    })
    .await
    .unwrap();
}

// ── service catalog ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn catalog_list() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": "cat001", "title": "Service Catalog"}]
        })))
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
            .args(["--compact", "catalog", "list"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["title"], "Service Catalog");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_items_with_text_search() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/items"))
        .and(query_param("sysparm_text", "laptop"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"sys_id": "item001", "name": "Laptop Request"}]
        })))
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
            .args(["--compact", "catalog", "items", "--text", "laptop"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v[0]["name"], "Laptop Request");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_order_item() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sn_sc/servicecatalog/items/item001/order_now"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"request_number": "REQ001", "request_id": "req001"}
        })))
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
            .args(["--compact", "catalog", "order", "item001"])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["request_number"], "REQ001");
    })
    .await
    .unwrap();
}

// ── identify & reconcile ─────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn identify_create_update() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/identifyreconcile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"items": [{"sysId": "ci001", "className": "cmdb_ci_server"}]}
        })))
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
            .args([
                "--compact",
                "identify",
                "create-update",
                "--data",
                r#"{"items":[{"className":"cmdb_ci_server","values":{"name":"web01"}}]}"#,
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["items"][0]["className"], "cmdb_ci_server");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn identify_query() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/identifyreconcile/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {"items": [{"sysId": "ci001", "className": "cmdb_ci_server"}]}
        })))
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
            .args([
                "--compact",
                "identify",
                "query",
                "--data",
                r#"{"items":[{"className":"cmdb_ci_server","values":{"name":"web01"}}]}"#,
            ])
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["items"][0]["sysId"], "ci001");
    })
    .await
    .unwrap();
}
