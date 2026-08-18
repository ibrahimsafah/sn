//! `sn open --print-url` must emit an ABSOLUTE url.
//!
//! Profiles persist the bare host (`sn init` / `sn profile add` normalize
//! `dev123` to `dev123.service-now.com`, with no scheme), so a command that
//! interpolates `profile.instance` straight into a URL string emits
//! `acme.service-now.com/nav_to.do?...` — which is not a URL any browser will
//! open. That shipped, unnoticed, because nothing exercised `sn open`.
//!
//! No network: `--print-url` short-circuits before the browser call.

mod common;

use common::{ProfileSpec, sn_cmd, write_profiles};

fn print_url(instance: &str) -> String {
    let tmp = write_profiles(
        "t",
        &[ProfileSpec {
            name: "t",
            instance,
            username: "u",
            password: "p",
        }],
    );
    let out = sn_cmd(tmp.path())
        .args(["open", "incident", "abc123", "--print-url"])
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn bare_host_gets_a_scheme() {
    // The shape every profile created the documented way actually has.
    assert_eq!(
        print_url("acme.service-now.com"),
        "https://acme.service-now.com/nav_to.do?uri=%2Fincident.do%3Fsys_id%3Dabc123"
    );
}

#[test]
fn explicit_https_is_left_alone() {
    assert_eq!(
        print_url("https://acme.service-now.com"),
        "https://acme.service-now.com/nav_to.do?uri=%2Fincident.do%3Fsys_id%3Dabc123"
    );
}

#[test]
fn explicit_http_is_not_upgraded() {
    // A local/dev instance on plain http must not be silently rewritten to https.
    assert_eq!(
        print_url("http://localhost:8080"),
        "http://localhost:8080/nav_to.do?uri=%2Fincident.do%3Fsys_id%3Dabc123"
    );
}

#[test]
fn trailing_slash_does_not_double_up() {
    assert_eq!(
        print_url("https://acme.service-now.com/"),
        "https://acme.service-now.com/nav_to.do?uri=%2Fincident.do%3Fsys_id%3Dabc123"
    );
}
