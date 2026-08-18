//! Tests for `oauth::ensure_access_token` — the chokepoint that transparently
//! refreshes a stale token and persists the result. They write to the on-disk
//! credentials file, so each test points `SN_CONFIG_DIR` at a private TempDir
//! root to isolate itself from the developer's real config. The env var is
//! process-global, hence `#[serial]`.

mod common;

use serde_json::json;
use serial_test::serial;
use sn::config::{ResolvedOauth, ResolvedProfile, TokenSet, now_unix};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn ensure_access_token_refreshes_expired_and_persists() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: `#[serial]` + `current_thread` — no other thread reads the
    // environment while this test owns it.
    unsafe { std::env::set_var("SN_CONFIG_DIR", tmp.path()) };

    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth_token.do"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=RT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "NEW_AT",
            "refresh_token": "NEW_RT",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    let base = common::mock_oauth_profile(&server.uri(), "OLD_AT");
    let o = base.oauth.clone().unwrap();
    let profile = ResolvedProfile {
        name: "ref-test".into(),
        oauth: Some(ResolvedOauth {
            tokens: Some(TokenSet {
                access_token: "OLD_AT".into(),
                refresh_token: Some("RT".into()),
                expires_at: Some(now_unix() - 10), // already expired
                token_type: Some("Bearer".into()),
            }),
            ..o
        }),
        ..base
    };

    let token = tokio::task::spawn_blocking(move || sn::oauth::ensure_access_token(&profile, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token, "NEW_AT");

    // The refreshed token must be persisted under the temp config dir (at its
    // root — SN_CONFIG_DIR has no /sn subdir) so the next invocation reuses it.
    let creds =
        sn::config::load_credentials_from(&sn::config::credentials_path().unwrap()).unwrap();
    let saved = creds.profiles["ref-test"].oauth_tokens.as_ref().unwrap();
    assert_eq!(saved.access_token, "NEW_AT");
    assert_eq!(saved.refresh_token.as_deref(), Some("NEW_RT"));

    // SAFETY: `#[serial]` + `current_thread` — no other thread reads the
    // environment while this test owns it.
    unsafe { std::env::remove_var("SN_CONFIG_DIR") };
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn ensure_access_token_returns_cached_when_valid() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: `#[serial]` + `current_thread` — no other thread reads the
    // environment while this test owns it.
    unsafe { std::env::set_var("SN_CONFIG_DIR", tmp.path()) };

    // No mock and an unroutable instance: if the code attempted a refresh it
    // would fail. Returning the cached token proves it short-circuited.
    let base = common::mock_oauth_profile("http://127.0.0.1:1", "CACHED_AT");
    let o = base.oauth.clone().unwrap();
    let profile = ResolvedProfile {
        name: "cache-test".into(),
        oauth: Some(ResolvedOauth {
            tokens: Some(TokenSet {
                access_token: "CACHED_AT".into(),
                refresh_token: Some("RT".into()),
                expires_at: Some(now_unix() + 3600), // valid for an hour
                token_type: Some("Bearer".into()),
            }),
            ..o
        }),
        ..base
    };

    let token = tokio::task::spawn_blocking(move || sn::oauth::ensure_access_token(&profile, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token, "CACHED_AT");

    // SAFETY: `#[serial]` + `current_thread` — no other thread reads the
    // environment while this test owns it.
    unsafe { std::env::remove_var("SN_CONFIG_DIR") };
}
