//! Shared clap arg vocabulary, flattened (`#[command(flatten)]`) into the
//! command modules' arg structs. One declaration per flag pair keeps the doc
//! comments — which are the rendered `--help` and the `sn introspect`
//! contract — from drifting apart across copies.
//!
//! Flatten position is load-bearing: clap numbers a command's args in
//! insertion order (`Command::arg` assigns `disp_ord` sequentially, and
//! `augment_args` continues the same counter), so a flatten renders exactly
//! where the field sits in the host struct. Keep each flatten where the
//! fields it replaces were declared, or `--help` reorders.
//!
//! A site whose help text genuinely differs (cmdb's enveloped writes, raw's
//! passthrough semantics, variables' name/value map, table list's
//! `--all`-aware paging) keeps its own declaration rather than flattening a
//! lie; only the parsing helpers are shared there.

use crate::body::{self, BodyInput, EmptyBody};
use crate::error::Result;
use clap::ValueEnum;
use serde_json::Value;

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum DisplayValueArg {
    True,
    False,
    All,
}

impl From<DisplayValueArg> for crate::query::DisplayValue {
    fn from(v: DisplayValueArg) -> Self {
        match v {
            DisplayValueArg::True => crate::query::DisplayValue::True,
            DisplayValueArg::False => crate::query::DisplayValue::False,
            DisplayValueArg::All => crate::query::DisplayValue::All,
        }
    }
}

/// The `--display-value` flag with its default of `true` (see the convention
/// in CLAUDE.md: display values on by default, localized dates included).
#[derive(clap::Args, Debug)]
pub struct DisplayValueOpt {
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
}

/// The `--data`/`--field` body pair every JSON write command takes.
#[derive(clap::Args, Debug)]
pub struct BodyArgs {
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

impl BodyArgs {
    /// The pair as a [`BodyInput`]; neither flag given is [`BodyInput::None`].
    pub fn into_input(self) -> BodyInput {
        body::input_from(self.data, self.field)
    }

    /// Parse the pair into a JSON body, resolving an empty pair per `empty` —
    /// the one thing the write commands legitimately differ on.
    pub fn build(self, empty: EmptyBody) -> Result<Value> {
        body::from_flags(self.data, self.field, empty)
    }
}

/// The `--wait`/`--wait-timeout` pair of the async CICD commands, consumed by
/// `progress::finish_cicd`.
#[derive(clap::Args, Debug)]
pub struct WaitArgs {
    /// Block until the operation completes (polls progress API).
    #[arg(long)]
    pub wait: bool,
    /// Give up on --wait after this many seconds (exit 3). Default: no limit.
    #[arg(long, value_name = "SECS", requires = "wait")]
    pub wait_timeout: Option<u64>,
}

/// `--setlimit` for the list commands whose page size defaults differ —
/// the const parameter is the site's default, so the default stays visible
/// in `--help` instead of becoming a runtime fallback.
///
/// `default_value`, not `default_value_t`: the derive renders `_t` into a
/// `static OnceLock<String>` inside the generic `augment_args`, and Rust
/// shares one `static` across every monomorphization — so `SetLimit<100>`
/// and `SetLimit<1000>` would both show whichever default built first.
#[derive(clap::Args, Debug)]
pub struct SetLimit<const DEFAULT: u32> {
    /// Maximum records returned. Maps to sysparm_limit. Also spelled --limit or --setLimit.
    #[arg(
        long,
        alias = "sysparm-limit",
        alias = "limit",
        alias = "setLimit",
        default_value = DEFAULT.to_string()
    )]
    pub setlimit: u32,
}

/// [`SetLimit`] plus `--offset`, for the list commands that page manually.
#[derive(clap::Args, Debug)]
pub struct Paging<const DEFAULT: u32> {
    #[command(flatten)]
    pub limit: SetLimit<DEFAULT>,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
}

impl<const DEFAULT: u32> Paging<DEFAULT> {
    pub fn setlimit(&self) -> u32 {
        self.limit.setlimit
    }
}
