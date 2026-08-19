#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Reserve and immediately release a free localhost TCP port, returning its
/// number. The port is unbound on return so a loopback server can claim it.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Connect to a loopback HTTP server on `127.0.0.1:port` (retrying until it
/// binds), send one raw `GET <target>` request, and drain the response. Used to
/// simulate the browser's OAuth redirect callback against `run_loopback`.
pub fn send_loopback_request(port: u16, target: &str) {
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stream = stream.expect("connect to loopback server");
    let req = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
}

/// Description of a basic-auth profile to be written into
/// `config.toml` + `credentials.toml` by [`write_profiles`].
pub struct ProfileSpec<'a> {
    pub name: &'a str,
    pub instance: &'a str,
    pub username: &'a str,
    pub password: &'a str,
}

/// Write `config.toml` and `credentials.toml` at the root of a fresh temp dir
/// (point `SN_CONFIG_DIR` at it via [`sn_cmd`]). `default_profile` is always
/// set explicitly. Returns the temp dir handle (drop = cleanup).
pub fn write_profiles(default_profile: &str, profiles: &[ProfileSpec<'_>]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();

    let mut cfg_profiles: std::collections::BTreeMap<String, sn::config::ProfileConfig> =
        std::collections::BTreeMap::new();
    let mut cred_profiles: std::collections::BTreeMap<String, sn::config::ProfileCredentials> =
        std::collections::BTreeMap::new();

    for p in profiles {
        cfg_profiles.insert(
            p.name.to_string(),
            sn::config::ProfileConfig {
                instance: p.instance.to_string(),
                ..Default::default()
            },
        );
        cred_profiles.insert(
            p.name.to_string(),
            sn::config::ProfileCredentials {
                username: p.username.to_string(),
                password: p.password.to_string(),
                ..Default::default()
            },
        );
    }

    let cfg = sn::config::Config {
        default_profile: Some(default_profile.to_string()),
        profiles: cfg_profiles,
    };
    let cr = sn::config::Credentials {
        profiles: cred_profiles,
    };

    sn::config::save_config_to(&tmp.path().join("config.toml"), &cfg).unwrap();
    sn::config::save_credentials_to(&tmp.path().join("credentials.toml"), &cr).unwrap();

    tmp
}

/// Seed an OAuth (authorization_code + PKCE) profile as raw TOML at the root
/// of a fresh temp dir: `config.toml` with the profile + `[oauth]` block, and
/// `credentials.toml` with a client secret and a cached token set
/// (`access_token = "VALID_AT"`, `refresh_token = "RT"`) expiring at
/// `expires_at` (unix seconds). `default_profile` is set to `name`.
pub fn write_oauth_profile(
    name: &str,
    instance: &str,
    client_id: &str,
    expires_at: i64,
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.toml"),
        format!(
            "default_profile = \"{name}\"\n\n\
             [profiles.{name}]\n\
             instance = \"{instance}\"\n\
             auth = \"oauth\"\n\n\
             [profiles.{name}.oauth]\n\
             client_id = \"{client_id}\"\n\
             redirect_uri = \"http://localhost:8400/callback\"\n\
             grant = \"authorization_code\"\n\
             pkce = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("credentials.toml"),
        format!(
            "[profiles.{name}]\n\
             client_secret = \"shh\"\n\n\
             [profiles.{name}.oauth_tokens]\n\
             access_token = \"VALID_AT\"\n\
             refresh_token = \"RT\"\n\
             expires_at = {expires_at}\n\
             token_type = \"Bearer\"\n"
        ),
    )
    .unwrap();
    tmp
}

/// Seed an API-key profile as raw TOML at the root of a fresh temp dir:
/// `config.toml` with `auth = "apikey"` and `credentials.toml` holding the key.
/// `default_profile` is set to `name`.
pub fn write_apikey_profile(name: &str, instance: &str, api_key: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.toml"),
        format!(
            "default_profile = \"{name}\"\n\n\
             [profiles.{name}]\n\
             instance = \"{instance}\"\n\
             auth = \"apikey\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("credentials.toml"),
        format!(
            "[profiles.{name}]\n\
             api_key = \"{api_key}\"\n"
        ),
    )
    .unwrap();
    tmp
}

/// Build a `Command` for the compiled `sn` binary rooted at `config_dir`
/// (via `SN_CONFIG_DIR`, which points directly at the directory containing
/// `config.toml`/`credentials.toml`). Every env var the binary might read —
/// plus system proxy vars that could redirect requests on a CI host — is
/// explicitly cleared; tests opt back in with `cmd.env(...)`. Per-process env
/// only (no `std::env::set_var`), so tests can run in parallel safely.
pub fn sn_cmd(config_dir: &std::path::Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("sn").unwrap();
    cmd.env("SN_CONFIG_DIR", config_dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("SN_PROXY")
        .env_remove("SN_NO_PROXY")
        .env_remove("SN_INSECURE")
        .env_remove("SN_CA_CERT")
        .env_remove("SN_PROXY_CA_CERT")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("NO_PROXY");
    cmd
}

pub fn mock_profile(instance: &str) -> sn::config::ResolvedProfile {
    sn::config::ResolvedProfile {
        name: "test".into(),
        instance: instance.to_string(),
        username: "admin".into(),
        password: "pw".into(),
        proxy: None,
        no_proxy: None,
        insecure: false,
        ca_cert: None,
        proxy_ca_cert: None,
        proxy_username: None,
        proxy_password: None,
        auth_method: sn::config::AuthMethod::Basic,
        oauth: None,
        api_key: None,
    }
}

/// A `ResolvedProfile` that authenticates with a bearer token (OAuth), for
/// exercising the bearer code path without a live token endpoint.
pub fn mock_oauth_profile(instance: &str, access_token: &str) -> sn::config::ResolvedProfile {
    sn::config::ResolvedProfile {
        name: "oauth-test".into(),
        instance: instance.to_string(),
        username: String::new(),
        password: String::new(),
        proxy: None,
        no_proxy: None,
        insecure: false,
        ca_cert: None,
        proxy_ca_cert: None,
        proxy_username: None,
        proxy_password: None,
        auth_method: sn::config::AuthMethod::Oauth,
        oauth: Some(sn::config::ResolvedOauth {
            client_id: "cid".into(),
            client_secret: None,
            redirect_uri: sn::config::default_redirect_uri(),
            auth_path: "/oauth_auth.do".into(),
            token_path: "/oauth_token.do".into(),
            grant: sn::config::OAuthGrant::AuthorizationCode,
            pkce: true,
            tokens: Some(sn::config::TokenSet {
                access_token: access_token.to_string(),
                refresh_token: None,
                expires_at: None,
                token_type: Some("Bearer".into()),
            }),
        }),
        api_key: None,
    }
}
