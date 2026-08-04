use assert_cmd::Command;
use serde_json::Value;

#[test]
fn introspect_lists_all_subcommands() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["introspect"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let names: Vec<String> = v["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str().map(String::from))
        .collect();
    for expected in ["init", "auth", "profile", "table", "schema", "introspect"] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing subcommand {expected}"
        );
    }
}

#[test]
fn introspect_reports_flags_and_options_accurately() {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["introspect"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let table = find_sub(&v, "table");
    let list = find_sub(table, "list");
    let args = list["args"].as_array().unwrap();

    // Boolean flags must not claim to take a value (an agent following
    // `takes_value: true` would emit `--all true`, which clap rejects).
    let all = find_arg(args, "all");
    assert_eq!(all["takes_value"], false, "--all is a flag: {all}");
    assert!(
        all["possible_values"].as_array().unwrap().is_empty(),
        "flags must not advertise true/false values: {all}"
    );

    // Value-taking options still report takes_value, aliases, and defaults.
    let setlimit = find_arg(args, "setlimit");
    assert_eq!(setlimit["takes_value"], true);
    assert_eq!(setlimit["default_values"][0], "1000");
    assert!(setlimit["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "limit"));

    // Positionals are marked so agents don't render them as --flags.
    let table_arg = find_arg(args, "table");
    assert_eq!(table_arg["positional"], true);
    assert!(table_arg["long"].is_null());
}

/// clap's generated `help` subcommand mirrors the entire tree as arg-less
/// stubs. Emitting it put 274 of 391 nodes in the output there, and gave every
/// real command a same-named twin: `sn help table list` sat beside
/// `sn table list` with an empty `args`, indistinguishable to a generator.
#[test]
fn no_phantom_help_nodes() {
    let leaked: Vec<String> = nodes(&tree())
        .into_iter()
        .filter(|(path, _)| path.rsplit(' ').next() == Some("help"))
        .map(|(path, _)| path)
        .collect();
    assert!(
        leaked.is_empty(),
        "clap's help subcommand leaked into the tree: {leaked:#?}"
    );
}

/// The durable form of the guard above: a mirrored subtree always duplicates a
/// path that already exists. This keeps holding after globals are hoisted,
/// where "node has no args" stops being a phantom's tell.
#[test]
fn command_paths_are_unique() {
    let tree = tree();
    let mut seen = std::collections::HashSet::new();
    let dupes: Vec<String> = nodes(&tree)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| !seen.insert(path.clone()))
        .collect();
    assert!(dupes.is_empty(), "duplicate command paths: {dupes:#?}");
}

/// The filter keys on clap's *action*, not on the arg name. `sn app install`
/// takes a real `--version <VERSION>`; filtering by name would have deleted it
/// along with clap's own `--version` flag.
#[test]
fn builtins_go_but_a_real_version_option_stays() {
    let tree = tree();
    for sub in ["install", "publish", "rollback"] {
        let args = find_sub(find_sub(&tree, "app"), sub)["args"]
            .as_array()
            .unwrap()
            .clone();
        let version = find_arg(&args, "version");
        assert_eq!(
            version["takes_value"], true,
            "app {sub} lost its --version: {version}"
        );
    }
    let leaked: Vec<String> = nodes(&tree)
        .into_iter()
        .filter(|(_, node)| {
            node["args"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a["name"] == "help")
        })
        .map(|(path, _)| path)
        .collect();
    assert!(
        leaked.is_empty(),
        "clap's --help is still in args: {leaked:#?}"
    );
}

fn tree() -> Value {
    let out = Command::cargo_bin("sn")
        .unwrap()
        .args(["introspect"])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

/// Every command node paired with its full path, e.g. `("sn table list", ..)`.
fn nodes(tree: &Value) -> Vec<(String, &Value)> {
    fn walk<'a>(cmd: &'a Value, prefix: &str, out: &mut Vec<(String, &'a Value)>) {
        let name = cmd["name"].as_str().unwrap_or_default();
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix} {name}")
        };
        for sub in cmd["subcommands"].as_array().into_iter().flatten() {
            walk(sub, &path, out);
        }
        out.push((path, cmd));
    }
    let mut out = Vec::new();
    walk(tree, "", &mut out);
    out
}

fn find_sub<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("missing subcommand {name}"))
}

fn find_arg<'a>(args: &'a [Value], name: &str) -> &'a Value {
    args.iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("missing arg {name}"))
}
