mod common;

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(server_uri: &str) -> tempfile::TempDir {
    common::write_profiles(
        "test",
        &[common::ProfileSpec {
            name: "test",
            instance: server_uri,
            username: "u",
            password: "p",
        }],
    )
}

fn resource(http_method: &str, route: &str, description: &str) -> Value {
    json!({"httpMethod": http_method, "route": route, "description": description})
}

/// The catalogue as `/api/now/doc` returns it: namespace -> API title -> API,
/// each API holding a version map whose blocks carry the endpoint list.
///
/// Note that no name, route or description anywhere in here contains the word
/// "delete" — only the one endpoint's `httpMethod` does. That is deliberate:
/// it is what makes "searching for an HTTP method matches nothing" an assertion
/// about the code rather than about the fixture.
fn catalogue() -> Value {
    json!({"result": {
        "now": {
            "Table API": {
                "apiName": "now/table",
                "description": "CRUD on existing tables",
                "scripted": false,
                "svcName": "Table API",
                "versions": {
                    "v1": {"resources": [
                        resource("GET", "/now/v1/table/{tableName}", "Query records from a table")
                    ]},
                    "v2": {"resources": [
                        resource("GET", "/now/v2/table/{tableName}", "Query records from a table"),
                        resource("POST", "/now/v2/table/{tableName}", "Create a record")
                    ]}
                }
            },
            "Attachment API": {
                "apiName": "now/attachment",
                "description": "Upload and retrieve file attachments",
                "scripted": false,
                "svcName": "Attachment API",
                "versions": {"latest": {"resources": [
                    resource("POST", "/now/attachment/file", "Upload an attachment"),
                    resource("DELETE", "/now/attachment/{sys_id}", "Remove an attachment")
                ]}}
            }
        },
        "sn_chg_rest": {
            "Change Management API": {
                "apiName": "sn_chg_rest/change",
                "description": "Change request lifecycle",
                "scripted": true,
                "svcName": "Change Management API",
                "versions": {"latest": {"resources": [
                    resource("GET", "/sn_chg_rest/change", "List change requests")
                ]}}
            },
            "CSM Attachment API": {
                "apiName": "sn_chg_rest/csm_attachment",
                "description": "",
                "scripted": true,
                "svcName": "CSM Attachment API",
                "versions": {"latest": {"resources": [
                    resource("GET", "/sn_chg_rest/csm_attachment", "List attachments")
                ]}}
            }
        }
    }})
}

/// One namespace's slice, the way `/api/now/doc/services?namespace=` serves it.
fn slice(namespace: &str) -> Value {
    let mut cat = catalogue();
    cat["result"]
        .as_object_mut()
        .unwrap()
        .retain(|k, _| k == namespace);
    cat
}

async fn mock_catalogue(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/now/doc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalogue()))
        .mount(server)
        .await;
}

async fn mock_services(server: &MockServer, namespace: &str) {
    Mock::given(method("GET"))
        .and(path("/api/now/doc/services"))
        .and(query_param("namespace", namespace))
        .respond_with(ResponseTemplate::new(200).set_body_json(slice(namespace)))
        .mount(server)
        .await;
}

/// The namespace list, which the CLI probes to tell a bad argument apart from
/// an instance that never had these endpoints.
async fn mock_namespaces(server: &MockServer, namespaces: &[&str]) {
    Mock::given(method("GET"))
        .and(path("/api/now/doc/namespaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": namespaces})))
        .mount(server)
        .await;
}

fn stdout_json(out: &assert_cmd::assert::Assert) -> Value {
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("stdout was not JSON ({e}): {s}"))
}

fn stderr(out: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(out.get_output().stderr.clone()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn list_summarizes_namespaces_with_api_and_endpoint_counts() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "list"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        assert_eq!(
            rows,
            json!([
                {"namespace": "now", "apis": 2, "endpoints": 5},
                {"namespace": "sn_chg_rest", "apis": 2, "endpoints": 2},
            ])
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn list_with_a_namespace_reads_the_services_endpoint_and_rows_the_apis() {
    let server = MockServer::start().await;
    // Only /services is mocked: a namespaced list must not pull the whole
    // catalogue, which on a real instance is several hundred kilobytes.
    mock_services(&server, "now").await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "list", "--namespace", "now"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let table = &rows.as_array().unwrap()[1];
        assert_eq!(table["name"], "Table API");
        assert_eq!(table["api_name"], "now/table");
        assert_eq!(table["versions"], json!(["v1", "v2"]));
        assert_eq!(table["endpoints"], 3);
        assert_eq!(table["scripted"], false);
        assert_eq!(rows.as_array().unwrap().len(), 2);
    })
    .await
    .unwrap();
}

/// The empty `{"result":{}}` an unknown namespace produces is the same signal
/// for both verbs, so both have to read it the same way — `search` returning an
/// empty array with exit 0 would make a typo indistinguishable from no matches.
#[tokio::test(flavor = "current_thread")]
async fn an_unknown_namespace_is_a_usage_error_for_list_and_search_alike() {
    let server = MockServer::start().await;
    // The endpoint answers an unknown namespace with an empty object and HTTP
    // 200, so the CLI has to notice it itself.
    Mock::given(method("GET"))
        .and(path("/api/now/doc/services"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
        .mount(&server)
        .await;
    mock_namespaces(&server, &["global", "now", "sn_chg_rest"]).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        for args in [
            vec!["api", "list", "--namespace", "nwo"],
            vec!["api", "search", "table", "--namespace", "nwo"],
        ] {
            let out = common::sn_cmd(tmp.path()).args(&args).assert().code(1);
            let err = stderr(&out);
            assert!(err.contains("no namespace 'nwo'"), "{args:?}: {err}");
            // Naming the near miss is the point: the caller cannot guess which
            // of ninety namespaces they fumbled.
            assert!(err.contains("did you mean 'now'"), "{args:?}: {err}");
        }
    })
    .await
    .unwrap();
}

/// The namespace list is a superset of the catalogue's keys — four of the
/// reference instance's ninety-four scopes publish no REST API — so an empty
/// answer for a *real* namespace must not be reported as a misspelling.
#[tokio::test(flavor = "current_thread")]
async fn a_namespace_with_no_apis_is_told_apart_from_a_misspelled_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/doc/services"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
        .mount(&server)
        .await;
    mock_namespaces(&server, &["crypto", "now"]).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["api", "list", "--namespace", "crypto"])
            .assert()
            .code(1);
        let err = stderr(&out);
        assert!(err.contains("publishes no REST API"), "{err}");
        assert!(!err.contains("did you mean"), "{err}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn list_raw_passes_the_catalogue_through_untouched() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "--output", "raw", "api", "list"])
            .assert()
            .success();
        assert_eq!(stdout_json(&out), catalogue());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_matches_routes_and_descriptions_case_insensitively() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "search", "ATTACHMENT"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let rows = rows.as_array().unwrap();
        // Both endpoints of the Attachment API (matched by name), plus the one
        // CSM endpoint whose route and description carry the word.
        assert_eq!(rows.len(), 3, "{rows:#?}");
        assert_eq!(rows[0]["namespace"], "now");
        assert_eq!(rows[0]["name"], "Attachment API");
        assert_eq!(rows[0]["version"], "latest");
        assert_eq!(rows[0]["method"], "POST");
        assert_eq!(rows[0]["route"], "/now/attachment/file");
        assert_eq!(rows[2]["api_name"], "sn_chg_rest/csm_attachment");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_reports_every_version_of_a_matching_endpoint() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "search", "table"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let versions: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["version"].as_str().unwrap())
            .collect();
        assert_eq!(versions, vec!["v1", "v2", "v2"]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_filters_by_http_method_without_matching_the_method_itself() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "search", "table", "--method", "post"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0]["method"], "POST");

        // The fixture really does serve a DELETE endpoint — this is what makes
        // the emptiness below load-bearing rather than vacuous.
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "search", "attachment", "-m", "delete"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0]["route"], "/now/attachment/{sys_id}");

        // ...yet nothing in the catalogue is *named* "delete", so searching for
        // the method must find nothing: were the term matched against
        // httpMethod, `sn api search get` would return every read endpoint on
        // the instance.
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "search", "delete"])
            .assert()
            .success();
        let rows = stdout_json(&out);
        assert_eq!(rows, json!([]), "{rows:#?}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_scoped_to_a_namespace_fetches_only_that_namespace() {
    let server = MockServer::start().await;
    mock_services(&server, "sn_chg_rest").await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "api",
                "search",
                "attachment",
                "--namespace",
                "sn_chg_rest",
            ])
            .assert()
            .success();
        let rows = stdout_json(&out);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0]["namespace"], "sn_chg_rest");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_renders_as_a_table() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["--output", "table", "api", "search", "attachment"])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        for expected in ["namespace", "route", "/now/attachment/file", "now"] {
            assert!(s.contains(expected), "missing {expected} in:\n{s}");
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn spec_sends_a_wildcard_accept_and_the_identifying_query() {
    let server = MockServer::start().await;
    mock_services(&server, "now").await;
    // Measured on a live instance: oas_3 rejects `Accept: application/json`
    // with 406, so this header is the whole reason the command exists.
    Mock::given(method("GET"))
        .and(path("/api/now/doc/oas_3"))
        .and(header("accept", "*/*"))
        .and(query_param("namespace", "now"))
        .and(query_param("name", "Table API"))
        .and(query_param("version", "v2"))
        .and(query_param("format", "json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"openapi": "3.0.1", "info": {"title": "Table API"}})),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "--compact",
                "api",
                "spec",
                "Table API",
                "--namespace",
                "now",
                "--version",
                "v2",
            ])
            .assert()
            .success();
        // No `result` envelope on this endpoint: the spec is emitted as served.
        assert_eq!(stdout_json(&out)["openapi"], "3.0.1");
    })
    .await
    .unwrap();
}

/// `--namespace` narrows the name matching instead of switching it off: the
/// substring form `<NAME>`'s help advertises has to work either way, and a name
/// the catalogue cannot place must fail as a usage error naming the near miss
/// rather than as a 404 from the export endpoint.
#[tokio::test(flavor = "current_thread")]
async fn a_namespace_narrows_name_matching_rather_than_disabling_it() {
    let server = MockServer::start().await;
    mock_services(&server, "now").await;
    mock_namespaces(&server, &["now", "sn_chg_rest"]).await;
    Mock::given(method("GET"))
        .and(path("/api/now/doc/oas_3"))
        .and(query_param("namespace", "now"))
        .and(query_param("name", "Table API"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"openapi": "3.0.1"})))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        // A case-insensitive substring resolves to the catalogue's own spelling.
        let out = common::sn_cmd(tmp.path())
            .args(["--compact", "api", "spec", "table", "--namespace", "now"])
            .assert()
            .success();
        assert_eq!(stdout_json(&out)["openapi"], "3.0.1");

        // A name that is not in the namespace is diagnosed against the
        // catalogue, not reported as a missing endpoint.
        let out = common::sn_cmd(tmp.path())
            .args(["api", "spec", "tabel api", "--namespace", "now"])
            .assert()
            .code(1);
        let err = stderr(&out);
        assert!(err.contains("no API matching 'tabel api'"), "{err}");
        assert!(err.contains("in namespace 'now'"), "{err}");
        assert!(err.contains("did you mean 'Table API'"), "{err}");
        assert!(!err.contains("older releases"), "{err}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn spec_resolves_the_namespace_from_the_catalogue_when_none_is_given() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/now/doc/oas_3"))
        .and(query_param("namespace", "sn_chg_rest"))
        .and(query_param("name", "Change Management API"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"openapi": "3.0.1"})))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        // Case-insensitive, and by apiName as well as by catalogue title.
        for name in ["change management api", "sn_chg_rest/change"] {
            let out = common::sn_cmd(tmp.path())
                .args(["--compact", "api", "spec", name])
                .assert()
                .success();
            assert_eq!(stdout_json(&out)["openapi"], "3.0.1");
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn an_ambiguous_name_lists_the_candidates_instead_of_guessing() {
    let server = MockServer::start().await;
    mock_catalogue(&server).await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["api", "spec", "attachment"])
            .assert()
            .code(1);
        let err = stderr(&out);
        assert!(err.contains("now/Attachment API"), "stderr was: {err}");
        assert!(err.contains("sn_chg_rest/CSM Attachment API"), "{err}");
        // These two only matched as substrings, so a full name really would
        // settle it.
        assert!(err.contains("name one of them exactly"), "{err}");

        // A term that matches nothing points at the verb that would find it.
        let out = common::sn_cmd(tmp.path())
            .args(["api", "spec", "nothing like this"])
            .assert()
            .code(1);
        let err = stderr(&out);
        assert!(err.contains("no API matching"), "stderr was: {err}");
    })
    .await
    .unwrap();
}

/// Two namespaces publishing the same API name is a tie that spelling the name
/// out *cannot* break, so the advice has to be the flag that can.
#[tokio::test(flavor = "current_thread")]
async fn an_exact_name_tie_is_sent_to_the_flag_that_can_break_it() {
    let server = MockServer::start().await;
    let mut cat = catalogue();
    let dup = cat["result"]["now"]["Attachment API"].clone();
    cat["result"]["sn_chg_rest"]
        .as_object_mut()
        .unwrap()
        .insert("Attachment API".to_string(), dup);
    Mock::given(method("GET"))
        .and(path("/api/now/doc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cat))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["api", "spec", "Attachment API"])
            .assert()
            .code(1);
        let err = stderr(&out);
        assert!(err.contains("now/Attachment API"), "{err}");
        assert!(err.contains("sn_chg_rest/Attachment API"), "{err}");
        assert!(err.contains("--namespace"), "{err}");
        assert!(err.contains("now, sn_chg_rest"), "{err}");
        // The name given was already exact; asking for it again is not advice.
        assert!(
            !err.contains("name one of them exactly"),
            "advice that cannot resolve the tie: {err}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn yaml_specs_are_written_verbatim_rather_than_through_the_json_emitter() {
    let server = MockServer::start().await;
    mock_services(&server, "now").await;
    Mock::given(method("GET"))
        .and(path("/api/now/doc/oas_3"))
        .and(header("accept", "*/*"))
        .and(query_param("format", "yaml"))
        .and(query_param("name", "Table API"))
        .respond_with(
            // The endpoint serves octet-stream whatever the format asks for.
            ResponseTemplate::new(200).set_body_raw(
                "openapi: \"3.0.1\"\ninfo:\n  title: \"Table API\"",
                "application/octet-stream",
            ),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "api",
                "spec",
                "Table API",
                "--namespace",
                "now",
                "--format",
                "yaml",
            ])
            .assert()
            .success();
        let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        // Byte-for-byte the served document, plus the trailing newline every
        // other command's stdout ends with.
        assert_eq!(s, "openapi: \"3.0.1\"\ninfo:\n  title: \"Table API\"\n");
    })
    .await
    .unwrap();
}

/// The "may be absent on older releases" reading of a 404 is only earned when
/// the endpoint family really is gone — proven by the cheapest member of it
/// answering 404 too.
#[tokio::test(flavor = "current_thread")]
async fn a_404_names_the_undocumented_endpoint_that_is_missing() {
    let server = MockServer::start().await;
    for p in ["/api/now/doc", "/api/now/doc/namespaces"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                json!({"error": {"message": "Requested URI does not represent any resource"}}),
            ))
            .mount(&server)
            .await;
    }
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args(["api", "list"])
            .assert()
            .code(2);
        let err = stderr(&out);
        assert!(err.contains("/api/now/doc"), "stderr was: {err}");
        assert!(err.contains("older releases"), "stderr was: {err}");
    })
    .await
    .unwrap();
}

/// ...and when the family answers, the 404 is about the request. ServiceNow
/// says which argument it disliked; telling the caller their instance is too
/// old instead sends them to debug something that is not broken.
#[tokio::test(flavor = "current_thread")]
async fn a_404_from_a_reachable_endpoint_family_is_not_blamed_on_the_release() {
    let server = MockServer::start().await;
    mock_services(&server, "now").await;
    mock_namespaces(&server, &["now", "sn_chg_rest"]).await;
    Mock::given(method("GET"))
        .and(path("/api/now/doc/oas_3"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": {
            "message": "Not Found",
            "detail": "Version v99 not found for now/Table API",
        }})))
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let tmp = profile(&uri);
        let out = common::sn_cmd(tmp.path())
            .args([
                "api",
                "spec",
                "Table API",
                "--namespace",
                "now",
                "--version",
                "v99",
            ])
            .assert()
            .code(2);
        let err = stderr(&out);
        assert!(
            err.contains("Version v99 not found for now\\/Table API")
                || err.contains("Version v99 not found for now/Table API"),
            "the endpoint's own reason must survive: {err}"
        );
        assert!(err.contains("sn api list --namespace now"), "{err}");
        assert!(
            !err.contains("older releases"),
            "a reachable endpoint family was blamed on the instance's age: {err}"
        );
    })
    .await
    .unwrap();
}
