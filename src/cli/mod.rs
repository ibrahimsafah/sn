pub mod aggregate;
pub mod app;
pub mod atf;
pub mod attachment;
pub mod auth;
pub mod catalog;
pub mod change;
pub mod cmdb;
pub mod completion;
pub mod identify;
pub mod import;
pub mod init;
pub mod introspect;
pub mod open_record;
pub mod ping;
pub mod profile;
pub mod progress;
pub mod raw;
pub mod schema;
pub mod scores;
pub mod table;
pub mod update_set;
pub mod user;
pub mod watch;

pub use aggregate::AggregateArgs;
pub use app::{AppInstallArgs, AppPublishArgs, AppRollbackArgs, AppSub};
pub use atf::{AtfResultsArgs, AtfRunArgs, AtfSub};
pub use attachment::{
    AttachmentDeleteArgs, AttachmentDownloadArgs, AttachmentGetArgs, AttachmentListArgs,
    AttachmentSub, AttachmentUploadArgs,
};
pub use auth::AuthSub;
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
pub use identify::{IdentifyArgs, IdentifyEnhancedArgs, IdentifySub};
pub use import::{ImportBulkArgs, ImportCreateArgs, ImportGetArgs, ImportSub};
pub use init::InitArgs;
pub use open_record::OpenArgs;
pub use profile::{ProfileAddArgs, ProfileSub};
pub use progress::ProgressArgs;
pub use raw::RawArgs;
pub use schema::{SchemaChoicesArgs, SchemaColumnsArgs, SchemaSub, SchemaTablesArgs};
pub use scores::{ScoresFavoriteArgs, ScoresListArgs, ScoresSub, SortBy, SortDir};
pub use table::{
    DisplayValueArg, TableCreateArgs, TableDeleteArgs, TableGetArgs, TableListArgs, TableSub,
    TableUpdateArgs,
};
pub use update_set::{
    UpdateSetBackOutArgs, UpdateSetCommitMultipleArgs, UpdateSetCreateArgs, UpdateSetIdArg,
    UpdateSetRetrieveArgs, UpdateSetSub,
};
pub use user::UserSub;
pub use watch::{
    Operation, WatchActivityArgs, WatchChannelArgs, WatchCountArgs, WatchLimits, WatchSub,
    WatchTableArgs,
};

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

/// Parse argv through [`command`] so error messages carry the same usage lines
/// as `--help`.
pub fn parse() -> Result<Cli, clap::Error> {
    let matches = command().try_get_matches()?;
    Cli::from_arg_matches(&matches)
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
        usage.push_str(" <COMMAND>");
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
    #[arg(long, global = true)]
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
    /// Auth operations.
    Auth {
        #[command(subcommand)]
        sub: AuthSub,
    },
    /// Manage profiles.
    Profile {
        #[command(subcommand)]
        sub: ProfileSub,
    },
    /// Table API CRUD.
    Table {
        #[command(subcommand)]
        sub: TableSub,
    },
    /// Watch records change in real time (AMB websocket). Streams JSONL.
    Watch {
        #[command(subcommand)]
        sub: WatchSub,
    },
    /// Schema discovery.
    Schema {
        #[command(subcommand)]
        sub: SchemaSub,
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
