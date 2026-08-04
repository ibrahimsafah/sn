use crate::cli::GlobalFlags;
use crate::error::Result;
use clap::{Arg, ArgAction, Command as ClapCommand};
use serde_json::{json, Value};

use super::table::write_response;

pub fn run(global: &GlobalFlags) -> Result<()> {
    // Via cli::command(), not Cli::command(), so this describes the same tree
    // that renders --help. build() then finalizes defaults, actions, and value
    // parsers; without it num_args/actions are unset and every arg looks like
    // it takes a value. build() also generates the `help` subcommand, which
    // mirrors the entire tree as arg-less stubs — describe_command filters
    // that back out.
    let mut cmd = crate::cli::command();
    cmd.build();
    let mut tree = describe_command(&cmd, "sn");
    // Stamped at the root only, so a consumer caching generated schemas can
    // tell which binary produced the tree and invalidate on an upgrade.
    tree["version"] = json!(env!("CARGO_PKG_VERSION"));
    write_response(global, &tree)
}

fn describe_command(cmd: &ClapCommand, name: &str) -> Value {
    let args: Vec<Value> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !is_builtin(a))
        .map(|a| describe_arg(cmd, a))
        .collect();
    // `help` is clap's, and it mirrors the whole tree as stubs that carry no
    // args — emitting it doubles the node count with nodes that describe
    // nothing callable, and gives every real command a same-named twin.
    let subs: Vec<Value> = cmd
        .get_subcommands()
        .filter(|sc| sc.get_name() != "help" && !sc.is_hide_set())
        .map(|sc| describe_command(sc, sc.get_name()))
        .collect();
    json!({
        "name": name,
        "about": cmd.get_about().map(|s| s.to_string()),
        "args": args,
        "subcommands": subs,
    })
}

/// `--help` and `--version` are clap's own: they exit before any handler runs,
/// so a generated tool schema has no use for them. Match on the action rather
/// than the name — `sn app install --version <VERSION>` is a real option that
/// takes a value, and filtering by name would delete it.
fn is_builtin(a: &Arg) -> bool {
    matches!(
        a.get_action(),
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
    )
}

/// `cmd` is needed for `conflicts_with`: clap exposes the relation from the
/// command, not the argument. Its inverse, `requires`, has no public getter at
/// all (`Arg::requires` is `pub(crate)`), so `--wait-timeout requires --wait`
/// survives only as prose in that flag's help text. Don't go looking for it.
fn describe_arg(cmd: &ClapCommand, a: &Arg) -> Value {
    let aliases: Vec<&str> = a.get_all_aliases().unwrap_or_default();
    // Sorted so the tree is byte-stable across runs — a machine-readable
    // contract that reorders itself is painful to diff and cache.
    let mut conflicts: Vec<String> = cmd
        .get_arg_conflicts_with(a)
        .iter()
        .map(|c| c.get_id().as_str().to_string())
        .collect();
    conflicts.sort();
    // build() fills these in for every value-taking arg, so this is the
    // placeholder `--help` actually shows, not just an explicit `value_name`.
    let value_name = a
        .get_value_names()
        .and_then(|names| names.first())
        .map(|n| n.to_string());
    let takes_value = matches!(a.get_action(), ArgAction::Set | ArgAction::Append);
    // Flags (SetTrue/Count) carry a synthetic bool parser; suppress its
    // ["true","false"] so agents don't emit `--flag true`.
    let possible_values: Vec<String> = if takes_value {
        a.get_possible_values()
            .iter()
            .map(|p| p.get_name().to_string())
            .collect()
    } else {
        Vec::new()
    };
    let default_values: Vec<String> = a
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    json!({
        "name": a.get_id().as_str(),
        "long": a.get_long(),
        "short": a.get_short(),
        "aliases": aliases,
        "help": a.get_help().map(|s| s.to_string()),
        "help_heading": a.get_help_heading(),
        "required": a.is_required_set(),
        "takes_value": takes_value,
        "value_name": value_name,
        "positional": a.is_positional(),
        "repeatable": matches!(a.get_action(), ArgAction::Append | ArgAction::Count),
        "default_values": default_values,
        "possible_values": possible_values,
        "conflicts_with": conflicts,
    })
}
