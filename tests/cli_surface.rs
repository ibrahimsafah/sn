//! Guards for the CLI's shape rather than its behavior: the `--display-value`
//! default, the `-p` short flag, usage-line ordering, and the rule that every
//! argument carries help text.

mod common;

use assert_cmd::Command;
use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

/// 0.11.0 flipped `--display-value` to default `true`, so reads come back with
/// readable labels. Nothing pinned the old default, so nothing failed when it
/// changed — this is that pin.
#[tokio::test(flavor = "current_thread")]
async fn display_value_defaults_to_true() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident/abc"))
        .and(query_param("sysparm_display_value", "true"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
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
        sn_cmd(tmp.path())
            .args(["--compact", "table", "get", "incident", "abc"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// The escape hatch back to raw sys_ids and codes, for values that have to
/// round-trip into a query.
#[tokio::test(flavor = "current_thread")]
async fn display_value_false_is_honored() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident/abc"))
        .and(query_param("sysparm_display_value", "false"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
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
        sn_cmd(tmp.path())
            .args([
                "--compact",
                "table",
                "get",
                "incident",
                "abc",
                "--display-value",
                "false",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// `-p` selects a profile the way `--profile` does. The default profile points
/// at an unroutable host, so the request only succeeds if `-p` won.
#[tokio::test(flavor = "current_thread")]
async fn short_p_selects_a_profile() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident/abc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
        )
        .mount(&server)
        .await;
    let tmp = write_profiles(
        "wrong",
        &[
            ProfileSpec {
                name: "wrong",
                instance: "https://unrouteable.invalid",
                username: "u",
                password: "p",
            },
            ProfileSpec {
                name: "right",
                instance: &server.uri(),
                username: "u",
                password: "p",
            },
        ],
    );
    tokio::task::spawn_blocking(move || {
        sn_cmd(tmp.path())
            .args([
                "-p",
                "right",
                "--compact",
                "table",
                "get",
                "incident",
                "abc",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// Usage reads in the order people type: positionals, then flags.
#[test]
fn usage_puts_options_last() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["table", "get", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Usage: sn table get <TABLE> <SYS_ID> [OPTIONS]"),
        "usage line not reordered:\n{stdout}"
    );
}

/// A command group advertises its subcommands, not its flags.
#[test]
fn group_usage_names_the_subcommand_slot() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["table", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("Usage: sn table <COMMAND>"),
        "group usage line not reordered:\n{stdout}"
    );
}

/// 123 arguments across 54 commands once had no help text at all, so
/// `sn table get --help` listed bare flag names. Introspect is the audit.
#[test]
fn every_argument_has_help_text() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["introspect"])
        .assert()
        .success();
    let tree: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let mut undocumented = Vec::new();
    walk(&tree, &mut Vec::new(), &mut undocumented);
    assert!(
        undocumented.is_empty(),
        "arguments with no help text: {undocumented:#?}"
    );
}

fn walk(cmd: &Value, path: &mut Vec<String>, out: &mut Vec<String>) {
    path.push(cmd["name"].as_str().unwrap_or_default().to_string());
    // The globals live once at the root as `global_args`, not on every node, so
    // checking `args` alone would quietly stop covering eleven flags. Only the
    // root has the key; elsewhere it is null and yields nothing.
    for arg in cmd["args"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(cmd["global_args"].as_array().into_iter().flatten())
    {
        let name = arg["name"].as_str().unwrap_or_default();
        // clap generates these two. `introspect` drops them now, so this only
        // guards against them coming back.
        if matches!(name, "help" | "version") {
            continue;
        }
        if arg["help"].as_str().unwrap_or_default().is_empty() {
            out.push(format!("{} {}", path.join(" "), name));
        }
    }
    for sub in cmd["subcommands"].as_array().into_iter().flatten() {
        walk(sub, path, out);
    }
    path.pop();
}

/// `replace` is gone; `update` is the only write verb. Kept next to the surface
/// guards so a reintroduction has to be deliberate.
#[test]
fn replace_is_not_a_subcommand() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["table", "--help"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("replace"),
        "table still advertises replace:\n{stdout}"
    );
}

/// Issue #23: an agent wrote `sn table <table> <sys_id>` without `get`, which
/// mirrors the REST path it maps to. It now dispatches to `get`.
#[tokio::test(flavor = "current_thread")]
async fn omitted_get_verb_is_recovered() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_cat_item/abc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"sys_id": "abc"}})),
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
        sn_cmd(tmp.path())
            .args(["--compact", "table", "sc_cat_item", "abc"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// With no sys_id there is nothing to `get`, so the same shorthand means `list`.
/// Only one of the two can parse, which is what makes this a decision.
#[tokio::test(flavor = "current_thread")]
async fn omitted_list_verb_is_recovered() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
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
        sn_cmd(tmp.path())
            .args(["--compact", "table", "incident"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

/// The shorthand must not swallow a misspelled verb. `lst` is within clap's
/// edit distance of `list`, so it stays a typo rather than becoming a GET
/// against a table named `lst`.
#[test]
fn misspelled_verb_still_suggests_the_real_one() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["table", "lst", "incident"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("a similar subcommand exists") && stderr.contains("list"),
        "typo suggestion lost:\n{stderr}"
    );
}

/// Groups outside the shorthand keep erroring, but the error now names what is
/// actually valid — clap alone lists nothing.
#[test]
fn unrecognized_subcommand_lists_the_valid_ones() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["change", "sc_cat_item", "abc"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    for expected in ["list", "get", "nextstates", "approvals"] {
        assert!(
            stderr.contains(expected),
            "hint omits {expected}:\n{stderr}"
        );
    }
}

/// `--setlimit` is named after GlideRecord's `setLimit()`, so a ServiceNow
/// developer reaches for the camelCase spelling and clap — which matches long
/// flags case-sensitively — rejected it. Every site that takes a record cap
/// accepts all four spellings, so the muscle memory costs no round trip.
#[test]
fn every_record_cap_accepts_the_camel_case_spelling() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["introspect"])
        .assert()
        .success();
    let tree: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let mut missing = Vec::new();
    find_setlimit(&tree, &mut Vec::new(), &mut missing);
    assert!(
        !missing.is_empty() || tree["subcommands"].as_array().is_none(),
        "no `setlimit` argument found at all — did the flag get renamed?"
    );
    let gaps: Vec<_> = missing.iter().filter(|(_, ok)| !ok).collect();
    assert!(
        gaps.is_empty(),
        "setlimit without a setLimit alias: {gaps:#?}"
    );
}

fn find_setlimit(cmd: &Value, path: &mut Vec<String>, out: &mut Vec<(String, bool)>) {
    path.push(cmd["name"].as_str().unwrap_or_default().to_string());
    for arg in cmd["args"].as_array().into_iter().flatten() {
        if arg["long"].as_str() == Some("setlimit") {
            let aliases = arg["aliases"].as_array().cloned().unwrap_or_default();
            let has = aliases.iter().any(|a| a.as_str() == Some("setLimit"));
            out.push((path.join(" "), has));
        }
    }
    for sub in cmd["subcommands"].as_array().into_iter().flatten() {
        find_setlimit(sub, path, out);
    }
    path.pop();
}

/// The alias must not swallow a genuine typo: clap's near-miss suggestion is
/// what turns `--setLimt` into a fixable mistake rather than a silent default.
///
/// Either spelling is a correct suggestion — with the alias in place clap
/// offers `--setLimit`, which is one edit from what was typed and is a flag
/// that works. What must never happen is the typo being accepted, or rejected
/// with no pointer at all.
#[test]
fn a_misspelled_record_cap_still_suggests_the_real_flag() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["table", "list", "incident", "--setLimt", "5"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.to_lowercase().contains("setlimit"),
        "a near-miss must still point at a flag that works:\n{stderr}"
    );
}
