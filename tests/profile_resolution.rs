mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};
use predicates::str::contains;
use serde_json::json;
use wiremock::matchers::{basic_auth, method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Spawn a wiremock server that expects `n_calls` to `GET /api/now/table/sys_user`
/// (the probe `sn ping` issues) with the given basic-auth pair. Ping's extra
/// best-effort `sys_properties` call 404s harmlessly and is not counted.
async fn mount_ping_mock(server: &MockServer, user: &str, pass: &str, n_calls: u64) {
    Mock::given(method("GET"))
        .and(path_matcher("/api/now/table/sys_user"))
        .and(basic_auth(user, pass))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": [{"user_name": "ok"}]})),
        )
        .expect(n_calls)
        .mount(server)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn profile_flag_selects_correct_instance() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;

    // Server A expects exactly one call; Server B must receive none.
    mount_ping_mock(&server_a, "ua", "pa", 1).await;
    mount_ping_mock(&server_b, "ub", "pb", 0).await;

    let uri_a = server_a.uri();
    let uri_b = server_b.uri();

    let tmp = write_profiles(
        "profile_a",
        &[
            ProfileSpec {
                name: "profile_a",
                instance: &uri_a,
                username: "ua",
                password: "pa",
            },
            ProfileSpec {
                name: "profile_b",
                instance: &uri_b,
                username: "ub",
                password: "pb",
            },
        ],
    );
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args(["--profile", "profile_a", "ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // wiremock verifies expectations on drop, but make it explicit.
    drop(server_a);
    drop(server_b);
}

#[tokio::test(flavor = "current_thread")]
async fn default_profile_used_when_no_flag() {
    let server_dev = MockServer::start().await;
    let server_prod = MockServer::start().await;

    // No flag -> default_profile = "prod" -> server_prod is hit.
    mount_ping_mock(&server_dev, "dev-u", "dev-p", 0).await;
    mount_ping_mock(&server_prod, "prod-u", "prod-p", 1).await;

    let uri_dev = server_dev.uri();
    let uri_prod = server_prod.uri();

    let tmp = write_profiles(
        "prod",
        &[
            ProfileSpec {
                name: "dev",
                instance: &uri_dev,
                username: "dev-u",
                password: "dev-p",
            },
            ProfileSpec {
                name: "prod",
                instance: &uri_prod,
                username: "prod-u",
                password: "prod-p",
            },
        ],
    );
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir).args(["ping"]).assert().success();
    })
    .await
    .unwrap();

    drop(server_dev);
    drop(server_prod);
}

#[tokio::test(flavor = "current_thread")]
async fn profile_flag_overrides_default_profile() {
    let server_dev = MockServer::start().await;
    let server_prod = MockServer::start().await;

    // default_profile points at prod, but --profile dev wins.
    mount_ping_mock(&server_dev, "dev-u", "dev-p", 1).await;
    mount_ping_mock(&server_prod, "prod-u", "prod-p", 0).await;

    let uri_dev = server_dev.uri();
    let uri_prod = server_prod.uri();

    let tmp = write_profiles(
        "prod",
        &[
            ProfileSpec {
                name: "dev",
                instance: &uri_dev,
                username: "dev-u",
                password: "dev-p",
            },
            ProfileSpec {
                name: "prod",
                instance: &uri_prod,
                username: "prod-u",
                password: "prod-p",
            },
        ],
    );
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args(["--profile", "dev", "ping"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(server_dev);
    drop(server_prod);
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_profile_errors_clearly() {
    // Only `dev` is configured; ask for a non-existent profile.
    let server = MockServer::start().await;
    let uri = server.uri();

    let tmp = write_profiles(
        "dev",
        &[ProfileSpec {
            name: "dev",
            instance: &uri,
            username: "u",
            password: "p",
        }],
    );
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args(["--profile", "nonexistent", "ping"])
            .assert()
            .code(1)
            .stderr(contains("nonexistent"));
    })
    .await
    .unwrap();

    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn no_profile_selected_errors_clearly() {
    // Config exists but has no default_profile, and no --profile flag is
    // passed: the CLI must error with "no profile selected" instead of
    // inventing a phantom "default" profile. The shared `write_profiles`
    // always sets default_profile, so this fixture is written inline.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = sn::config::Config {
        default_profile: None,
        profiles: std::collections::BTreeMap::from([(
            "dev".to_string(),
            sn::config::ProfileConfig {
                instance: "dev.example.com".to_string(),
                ..Default::default()
            },
        )]),
    };
    let cr = sn::config::Credentials {
        profiles: std::collections::BTreeMap::from([(
            "dev".to_string(),
            sn::config::ProfileCredentials {
                username: "u".to_string(),
                password: "p".to_string(),
                ..Default::default()
            },
        )]),
    };
    sn::config::save_config_to(&tmp.path().join("config.toml"), &cfg).unwrap();
    sn::config::save_credentials_to(&tmp.path().join("credentials.toml"), &cr).unwrap();
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        sn_cmd(&dir)
            .args(["ping"])
            .assert()
            .code(1)
            .stderr(contains("no profile selected"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_profiles_isolation() {
    let server_a = MockServer::start().await;
    let server_b = MockServer::start().await;
    let server_c = MockServer::start().await;

    // Each server should be hit exactly once when called with its profile,
    // and zero times when other profiles are addressed.
    mount_ping_mock(&server_a, "ua", "pa", 1).await;
    mount_ping_mock(&server_b, "ub", "pb", 1).await;
    mount_ping_mock(&server_c, "uc", "pc", 1).await;

    let uri_a = server_a.uri();
    let uri_b = server_b.uri();
    let uri_c = server_c.uri();

    let tmp = write_profiles(
        "a",
        &[
            ProfileSpec {
                name: "a",
                instance: &uri_a,
                username: "ua",
                password: "pa",
            },
            ProfileSpec {
                name: "b",
                instance: &uri_b,
                username: "ub",
                password: "pb",
            },
            ProfileSpec {
                name: "c",
                instance: &uri_c,
                username: "uc",
                password: "pc",
            },
        ],
    );
    let dir = tmp.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        for prof in ["a", "b", "c"] {
            sn_cmd(&dir)
                .args(["--profile", prof, "ping"])
                .assert()
                .success();
        }
    })
    .await
    .unwrap();

    drop(server_a);
    drop(server_b);
    drop(server_c);
}
