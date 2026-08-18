pub mod aggregate;
pub mod api;
pub mod app;
pub mod args;
pub mod atf;
pub mod attachment;
pub mod auth;
pub mod catalog;
pub mod change;
pub mod cmdb;
pub mod completion;
pub mod context;
pub mod get_record;
pub mod graphql;
pub mod identify;
pub mod import;
pub mod init;
pub mod introspect;
pub mod journal;
pub(crate) mod kernel;
pub mod open_record;
pub mod ping;
pub mod profile;
pub mod progress;
pub mod raw;
pub mod record_ref;
pub mod schema;
pub mod scores;
pub mod table;
pub mod update_set;
pub mod user;
pub mod variables;
pub mod watch;

pub use aggregate::AggregateArgs;
pub use api::{ApiListArgs, ApiSearchArgs, ApiSpecArgs, ApiSub, SpecFormat};
pub use app::{AppInstallArgs, AppPublishArgs, AppRollbackArgs, AppSub};
pub use args::{BodyArgs, DisplayValueArg, DisplayValueOpt, Paging, SetLimit, WaitArgs};
pub use atf::{AtfResultsArgs, AtfRunArgs, AtfSub};
pub use attachment::{
    AttachmentDeleteArgs, AttachmentDownloadArgs, AttachmentGetArgs, AttachmentListArgs,
    AttachmentSub, AttachmentUploadArgs,
};
pub use catalog::{
    CatalogCartEmptyArgs, CatalogCartItemArgs, CatalogCartUpdateArgs, CatalogCategoriesArgs,
    CatalogCategoryArgs, CatalogGetArgs, CatalogItemArgs, CatalogItemsArgs, CatalogListArgs,
    CatalogOrderArgs, CatalogSub,
};
pub use change::{
    ChangeApprovalsArgs, ChangeCiAddArgs, ChangeCiSub, ChangeConflictAddArgs, ChangeConflictSub,
    ChangeCreateArgs, ChangeDeleteArgs, ChangeGetArgs, ChangeListArgs, ChangeOptionalIdArg,
    ChangeRiskArgs, ChangeSub, ChangeSysIdArg, ChangeTaskCreateArgs, ChangeTaskDeleteArgs,
    ChangeTaskGetArgs, ChangeTaskListArgs, ChangeTaskSub, ChangeTaskUpdateArgs, ChangeType,
    ChangeUpdateArgs,
};
pub use cmdb::{
    CmdbCreateArgs, CmdbGetArgs, CmdbListArgs, CmdbMetaArgs, CmdbRelationAddArgs,
    CmdbRelationDeleteArgs, CmdbRelationSub, CmdbSub, CmdbUpdateArgs,
};
pub use completion::{CompletionArgs, Shell as CompletionShell};
pub use context::{ContextSub, ContextTargetArgs};
pub use get_record::GetRecordArgs;
pub use graphql::GraphqlArgs;
pub use identify::{IdentifyArgs, IdentifyEnhancedArgs, IdentifySub};
pub use import::{ImportBulkArgs, ImportCreateArgs, ImportGetArgs, ImportSub};
pub use init::InitArgs;
pub use journal::{JournalArgs, JournalSource};
pub use open_record::OpenArgs;
pub use profile::{ProfileAddArgs, ProfileSub};
pub use progress::ProgressArgs;
pub use raw::RawArgs;
pub use schema::{SchemaChoicesArgs, SchemaColumnsArgs, SchemaSub, SchemaTablesArgs};
pub use scores::{ScoresFavoriteArgs, ScoresListArgs, ScoresSub, SortBy, SortDir};
pub use table::{
    TableCreateArgs, TableDeleteArgs, TableGetArgs, TableListArgs, TableSub, TableUpdateArgs,
};
pub use update_set::{
    UpdateSetBackOutArgs, UpdateSetCommitMultipleArgs, UpdateSetCreateArgs, UpdateSetIdArg,
    UpdateSetRetrieveArgs, UpdateSetSub,
};
pub use user::UserSub;
pub use variables::{VariablesGetArgs, VariablesSetArgs, VariablesSub};
pub use watch::{Operation, WatchArgs, WatchLimits};

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

/// Help heading for raw `sysparm_*` passthroughs that most callers never touch.
/// Keeps the default "Options" section down to the working set of each command.
pub(crate) const ADVANCED: &str = "Advanced options";

/// The command tree, with every usage line rewritten to put `[OPTIONS]` last.
///
/// clap renders `sn table list [OPTIONS] <TABLE>`, which reads as though flags
/// come first; nobody types it that way. `override_usage` is verbatim, so each
/// line is rebuilt from the command's own positionals and must carry the full
/// path. Parsing is untouched — flags still work in any position.
pub fn command() -> clap::Command {
    with_usage(Cli::command(), "sn")
}

/// Groups whose verb may be omitted: `sn table incident <sys_id>` is `get`,
/// `sn table incident` is `list`. Both mirror a REST path that is already
/// `{noun}/{id}` (`/api/now/table/{table}/{sys_id}`,
/// `/api/now/cmdb/instance/{class}/{sys_id}`), so the verb is implied by the
/// method for a read. Groups with non-CRUD subcommands (`change nextstates`,
/// `catalog checkout`) are deliberately excluded — there the first token is not
/// reliably a noun.
const IMPLIED_VERB_GROUPS: [&str; 2] = ["table", "cmdb"];

/// Parse argv through [`command`] so error messages carry the same usage lines
/// as `--help`, recovering an omitted read verb where that is unambiguous.
pub fn parse() -> Result<Cli, clap::Error> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let err = match command().try_get_matches_from(argv.clone()) {
        Ok(matches) => return Cli::from_arg_matches(&matches),
        Err(err) => err,
    };
    for candidate in implied_verb_argv(&argv, &err) {
        if let Ok(matches) = command().try_get_matches_from(candidate) {
            return Cli::from_arg_matches(&matches);
        }
    }
    Err(err)
}

/// Rewrites of argv with a read verb inserted, most specific first. Empty when
/// the shorthand does not apply.
///
/// Which verb wins is a decision, not a guess, carried by the token's shape:
/// a `:` marks a `table:id` record reference (a table name can never contain
/// one), so only `get` is offered. A plain token tries `list` first — `list`
/// rejects a second positional, so `sn table incident abc` falls through to
/// `get`, while `sn table incident` stops at `list` before `get` (whose
/// sys_id positional is optional for the reference form) could claim it.
fn implied_verb_argv(
    argv: &[std::ffi::OsString],
    err: &clap::Error,
) -> Vec<Vec<std::ffi::OsString>> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if err.kind() != ErrorKind::InvalidSubcommand {
        return Vec::new();
    }
    // clap matched the token against a real verb by edit distance, so this is a
    // misspelling (`sn table lst incident`), not a table name. Leave its "did
    // you mean" intact rather than firing a GET at a table called `lst`.
    if let Some(ContextValue::Strings(near)) = err.get(ContextKind::SuggestedSubcommand) {
        if !near.is_empty() {
            return Vec::new();
        }
    }
    let Some(ContextValue::String(token)) = err.get(ContextKind::InvalidSubcommand) else {
        return Vec::new();
    };
    let Some(group) = usage_group(err) else {
        return Vec::new();
    };
    if !IMPLIED_VERB_GROUPS.contains(&group.as_str()) {
        return Vec::new();
    }
    // The rejected token sits directly after its group in argv.
    let Some(at) = argv
        .windows(2)
        .position(|w| w[0] == *group.as_str() && w[1] == *token.as_str())
    else {
        return Vec::new();
    };
    let verbs: &[&str] = if token.contains(':') {
        &["get"]
    } else {
        &["list", "get"]
    };
    verbs
        .iter()
        .map(|verb| {
            let mut rewritten = argv.to_vec();
            rewritten.insert(at + 1, std::ffi::OsString::from(verb));
            rewritten
        })
        .collect()
}

/// The command group an error came from, read back out of its usage line —
/// which [`usage_line`] renders as `sn <path> <COMMAND>`.
fn usage_group(err: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ContextValue};

    let ContextValue::StyledStr(usage) = err.get(ContextKind::Usage)? else {
        return None;
    };
    let text = usage.to_string();
    let mut parts = text.split_whitespace().rev();
    (parts.next()? == "<COMMAND>").then(|| parts.next().map(str::to_string))?
}

/// The subcommands a group accepts, for an error message that would otherwise
/// name none of them. `None` when the group can't be resolved.
pub fn valid_subcommands(err: &clap::Error) -> Option<(String, Vec<String>)> {
    let group = usage_group(err)?;
    let mut cmd = command();
    cmd.build();
    let node = find_command(&cmd, &group)?;
    let names: Vec<String> = node
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .map(|s| s.get_name().to_string())
        .collect();
    (!names.is_empty()).then_some((group, names))
}

fn find_command<'a>(cmd: &'a clap::Command, name: &str) -> Option<&'a clap::Command> {
    for sub in cmd.get_subcommands() {
        if sub.get_name() == name {
            return Some(sub);
        }
        if let Some(found) = find_command(sub, name) {
            return Some(found);
        }
    }
    None
}

fn with_usage(cmd: clap::Command, path: &str) -> clap::Command {
    let path = path.to_string();
    let usage = usage_line(&path, &cmd);
    cmd.override_usage(usage).mut_subcommands(move |sc| {
        let child = format!("{} {}", path, sc.get_name());
        with_usage(sc, &child)
    })
}

/// `sn table get <TABLE> <SYS_ID> [OPTIONS]` for a leaf, `sn table <COMMAND>`
/// for a group. Value names fall back to the uppercased arg id, which is what
/// clap would derive anyway — the tree is walked before `build()` fills them in.
fn usage_line(path: &str, cmd: &clap::Command) -> String {
    let mut usage = String::from(path);
    if cmd.get_subcommands().next().is_some() {
        // A group whose bare invocation is itself a command (`sn context`)
        // has an optional subcommand; promise `[COMMAND]`, not `<COMMAND>`.
        usage.push_str(if cmd.is_subcommand_required_set() {
            " <COMMAND>"
        } else {
            " [COMMAND]"
        });
        return usage;
    }
    for arg in cmd.get_positionals() {
        let name = arg
            .get_value_names()
            .and_then(|names| names.first())
            .map(|n| n.to_string())
            .unwrap_or_else(|| arg.get_id().as_str().to_uppercase());
        usage.push(' ');
        if arg.is_required_set() {
            usage.push_str(&format!("<{name}>"));
        } else {
            usage.push_str(&format!("[{name}]"));
        }
        if matches!(arg.get_action(), clap::ArgAction::Append) {
            usage.push_str("...");
        }
    }
    usage.push_str(" [OPTIONS]");
    usage
}

#[derive(Parser, Debug)]
#[command(
    name = "sn",
    version,
    about = "ServiceNow CLI (Table API + schema + CICD)",
    disable_version_flag = true,
    after_help = "BODY INPUT (write commands accept any of these):\n  --data '<inline JSON>'             inline JSON object\n  --data @body.json                  read body from a file (multi-line safe)\n  --data @-                          read body from stdin (e.g. piped from jq)\n  --field name=value                 set one field\n  --field description=@notes.md      read one field's value from a file\n\nEXAMPLES:\n  sn table update incident <sys_id> --data @body.json\n  sn table update incident <sys_id> --field description=@notes.md\n  jq '.fields' patch.json | sn table update incident <sys_id> --data @-"
)]
#[allow(clippy::manual_non_exhaustive)] // `version` field exists only to wire the -v flag.
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalFlags,

    #[command(subcommand)]
    pub command: Command,

    /// Print version and exit. `-V` is accepted as an alias.
    #[arg(short = 'v', short_alias = 'V', long = "version", action = clap::ArgAction::Version)]
    version: (),
}

/// Flags accepted by every command. They carry their own help heading so they
/// sink to the bottom of `--help` instead of interleaving with each command's
/// own flags — clap gives every `Args` container its own display-order counter
/// and merges them, which otherwise zippers 11 globals through the list.
#[derive(clap::Args, Debug, Clone, Default)]
#[command(next_help_heading = "Global options")]
pub struct GlobalFlags {
    /// Profile name (overrides default_profile).
    #[arg(long, short = 'p', global = true)]
    pub profile: Option<String>,

    /// Output mode. `default` (unwrapped result) or `raw` (full SN envelope).
    #[arg(long, global = true, value_enum, default_value_t = OutputMode::Default)]
    pub output: OutputMode,

    /// Force pretty-printed JSON regardless of TTY detection.
    #[arg(long, global = true, conflicts_with = "compact")]
    pub pretty: bool,

    /// Force compact JSON regardless of TTY detection.
    #[arg(long, global = true, conflicts_with = "pretty")]
    pub compact: bool,

    /// Request timeout in seconds. Defaults to 30s.
    #[arg(long, global = true, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Increase verbosity (-d requests, -dd headers, -ddd bodies).
    #[arg(short = 'd', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Proxy URL (http://, https://, socks5://). Overrides SN_PROXY and profile config.
    #[arg(long, global = true, value_name = "URL")]
    pub proxy: Option<String>,

    /// Bypass any configured proxy for this invocation.
    #[arg(long, global = true)]
    pub no_proxy: bool,

    /// Custom CA certificate for the proxy connection.
    #[arg(long, global = true, value_name = "PATH")]
    pub proxy_ca_cert: Option<String>,

    /// Disable TLS certificate verification (DANGEROUS).
    #[arg(long, global = true)]
    pub insecure: bool,

    /// Custom CA certificate bundle for the ServiceNow endpoint.
    #[arg(long, global = true, value_name = "PATH")]
    pub ca_cert: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq, Default)]
#[value(rename_all = "lowercase")]
pub enum OutputMode {
    #[default]
    Default,
    Raw,
    /// Render JSON results as a human-readable columnar table (suitable for interactive use, not piping).
    Table,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create or update a profile interactively.
    Init(InitArgs),
    /// Manage profiles and their auth sessions.
    Profile {
        #[command(subcommand)]
        sub: ProfileSub,
    },
    /// Table API CRUD.
    Table {
        #[command(subcommand)]
        sub: TableSub,
    },
    /// Get one record by reference — with its variables and journal entries
    /// (`sn get table:sys_id`, `sn get incident:INC0010001`, `sn get INC0010001`).
    Get(GetRecordArgs),
    /// Comments and work notes for one record, parsed into entries (`sn journal <TABLE> <SYS_ID>`).
    Journal(JournalArgs),
    /// Catalog variables on a record: read, and write with verification.
    Variables {
        #[command(subcommand)]
        sub: VariablesSub,
    },
    /// Watch records in a table change in real time (AMB websocket). Streams JSONL.
    Watch(WatchArgs),
    /// Schema discovery.
    Schema {
        #[command(subcommand)]
        sub: SchemaSub,
    },
    /// Discover the instance's REST APIs (namespaces, endpoints, OpenAPI specs).
    Api {
        #[command(subcommand)]
        sub: ApiSub,
    },
    /// Dump the full command tree as JSON for agent/MCP generation.
    Introspect,
    /// Get pipeline/deployment progress by ID.
    Progress(ProgressArgs),
    /// App repository operations (install, publish, rollback).
    App {
        #[command(subcommand)]
        sub: AppSub,
    },
    /// Update Set lifecycle operations.
    #[command(name = "updateset")]
    UpdateSet {
        #[command(subcommand)]
        sub: UpdateSetSub,
    },
    /// Instance-side session context: the application scope and update set the
    /// caller's tracked writes are captured under. Bare `sn context` shows it.
    Context {
        #[command(subcommand)]
        sub: Option<ContextSub>,
    },
    /// Aggregate statistics for a table (GET /api/now/stats/{table}).
    Aggregate(AggregateArgs),
    /// Performance Analytics scorecard operations.
    Scores {
        #[command(subcommand)]
        sub: ScoresSub,
    },
    /// Automated Test Framework operations.
    Atf {
        #[command(subcommand)]
        sub: AtfSub,
    },
    /// Change Management operations (normal, emergency, standard).
    Change {
        #[command(subcommand)]
        sub: ChangeSub,
    },
    /// Attachment operations (upload, download, list, delete).
    Attachment {
        #[command(subcommand)]
        sub: AttachmentSub,
    },
    /// CMDB Instance and Meta operations.
    Cmdb {
        #[command(subcommand)]
        sub: CmdbSub,
    },
    /// Import Set operations (staging table imports).
    Import {
        #[command(subcommand)]
        sub: ImportSub,
    },
    /// Service Catalog operations (catalogs, items, cart, ordering).
    Catalog {
        #[command(subcommand)]
        sub: CatalogSub,
    },
    /// Identification and Reconciliation (CI create/update/query).
    Identify {
        #[command(subcommand)]
        sub: IdentifySub,
    },
    /// Show the currently authenticated user.
    User {
        #[command(subcommand)]
        sub: UserSub,
    },
    /// Health check the configured instance (auth + latency + build version).
    Ping,
    /// Open a record in the ServiceNow web UI (`sn open <table> <sys_id>`).
    Open(OpenArgs),
    /// Generic REST passthrough for unmodeled endpoints (`sn raw <METHOD> <PATH>`).
    Raw(RawArgs),
    /// Run a GraphQL query against POST /api/now/graphql (`sn graphql <QUERY>`).
    Graphql(GraphqlArgs),
    /// Generate a shell completion script (`sn completion <SHELL>`).
    Completion(CompletionArgs),
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_compiles_and_debugs() {
        Cli::command().debug_assert();
    }

    #[test]
    fn pretty_and_compact_conflict() {
        let err = Cli::try_parse_from(["sn", "--pretty", "--compact", "introspect"]).unwrap_err();
        // clap emits conflict error; kind may differ by version, just assert it's an error
        let _ = err.kind();
    }
}
