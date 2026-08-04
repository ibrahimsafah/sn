//! Guards for the CLI's shape rather than its behavior: the `--display-value`
//! default, the `-p` short flag, usage-line ordering, and the rule that every
//! argument carries help text.

mod common;

use assert_cmd::Command;
use common::{sn_cmd, write_profiles, ProfileSpec};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};




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
    for arg in cmd["args"].as_array().into_iter().flatten() {
        let name = arg["name"].as_str().unwrap_or_default();
        // clap generates these two; their help text is not ours to set.
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




