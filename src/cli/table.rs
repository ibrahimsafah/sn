use crate::body::{build_body, BodyInput};
use crate::cli::kernel::{bool_opt, confirm_delete, connect, emit, write_response};
use crate::cli::record_ref;
use crate::cli::{BodyArgs, DisplayValueOpt, GlobalFlags, OutputMode, ADVANCED};
use crate::error::{Error, Result};
use crate::query::{DeleteQuery, GetQuery, ListQuery, WriteQuery};
use clap::Subcommand;
use serde_json::Value;
use std::io;

#[derive(Subcommand, Debug)]
pub enum TableSub {
    #[command(about = "List records")]
    List(TableListArgs),
    #[command(about = "Get a single record by sys_id")]
    Get(TableGetArgs),
    #[command(about = "Create a record")]
    Create(TableCreateArgs),
    #[command(about = "Patch a record (partial update)")]
    Update(TableUpdateArgs),
    #[command(about = "Delete a record")]
    Delete(TableDeleteArgs),
}

#[derive(clap::Args, Debug)]
pub struct TableListArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Encoded query, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    /// Maximum records returned (default 1000). Maps to sysparm_limit. Also spelled --limit or --setLimit.
    #[arg(
        long,
        alias = "limit",
        alias = "setLimit",
        alias = "sysparm-limit",
        alias = "page-size",
        default_value_t = 1000
    )]
    pub setlimit: u32,
    /// Starting offset for manual pagination (ignored with --all).
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(
        long,
        alias = "sysparm-exclude-reference-link",
        help_heading = ADVANCED
    )]
    pub exclude_reference_link: bool,
    /// Skip X-Total-Count calculation.
    #[arg(
        long,
        alias = "sysparm-suppress-pagination-header",
        help_heading = ADVANCED
    )]
    pub suppress_pagination_header: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
    /// Query category for index selection.
    #[arg(long, alias = "sysparm-query-category", help_heading = ADVANCED)]
    pub query_category: Option<String>,
    /// Cross-domain access if authorized.
    #[arg(long, alias = "sysparm-query-no-domain", help_heading = ADVANCED)]
    pub query_no_domain: bool,
    /// Skip the count query.
    #[arg(long, alias = "sysparm-no-count", help_heading = ADVANCED)]
    pub no_count: bool,
    /// Auto-paginate: stream every matching record (JSONL unless --array).
    #[arg(long)]
    pub all: bool,
    /// With --all, buffer into a single JSON array instead of JSONL.
    #[arg(long, requires = "all")]
    pub array: bool,
    /// Cap total records returned (default 100000; 0 = unlimited).
    #[arg(long, default_value_t = 100_000)]
    pub max_records: u32,
}

#[derive(clap::Args, Debug)]
pub struct TableGetArgs {
    /// Table name (e.g. `incident`), or a combined `table:sys_id` /
    /// `table:number` reference (e.g. `incident:INC0010001`).
    pub table: String,
    /// sys_id of the record to fetch. Omit when TABLE is a `table:id` reference.
    pub sys_id: Option<String>,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(
        long,
        alias = "sysparm-exclude-reference-link",
        help_heading = ADVANCED
    )]
    pub exclude_reference_link: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
    /// Cross-domain access if authorized.
    #[arg(long, alias = "sysparm-query-no-domain", help_heading = ADVANCED)]
    pub query_no_domain: bool,
}

#[derive(clap::Args, Debug)]
pub struct TableCreateArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Comma-separated fields to return on the created record.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(
        long,
        alias = "sysparm-exclude-reference-link",
        help_heading = ADVANCED
    )]
    pub exclude_reference_link: bool,
    /// Interpret submitted values as display values (e.g. a user's name instead of its sys_id).
    #[arg(long, alias = "sysparm-input-display-value", help_heading = ADVANCED)]
    pub input_display_value: bool,
    /// Suppress auto-generation of the sys_created/sys_updated audit fields.
    #[arg(
        long,
        alias = "sysparm-suppress-auto-sys-field",
        help_heading = ADVANCED
    )]
    pub suppress_auto_sys_field: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct TableUpdateArgs {
    /// Table name (e.g. `incident`), or a combined `table:sys_id` /
    /// `table:number` reference (e.g. `incident:INC0010001`).
    pub table: String,
    /// sys_id of the record to patch. Omit when TABLE is a `table:id` reference.
    pub sys_id: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Comma-separated fields to return on the updated record.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    #[command(flatten)]
    pub display_value: DisplayValueOpt,
    /// Strip reference-link URLs from reference fields.
    #[arg(
        long,
        alias = "sysparm-exclude-reference-link",
        help_heading = ADVANCED
    )]
    pub exclude_reference_link: bool,
    /// Interpret submitted values as display values (e.g. a user's name instead of its sys_id).
    #[arg(long, alias = "sysparm-input-display-value", help_heading = ADVANCED)]
    pub input_display_value: bool,
    /// Suppress auto-generation of the sys_created/sys_updated audit fields.
    #[arg(
        long,
        alias = "sysparm-suppress-auto-sys-field",
        help_heading = ADVANCED
    )]
    pub suppress_auto_sys_field: bool,
    /// Apply a named form/list view.
    #[arg(long, alias = "sysparm-view", help_heading = ADVANCED)]
    pub view: Option<String>,
    /// Cross-domain access if authorized.
    #[arg(long, alias = "sysparm-query-no-domain", help_heading = ADVANCED)]
    pub query_no_domain: bool,
}

#[derive(clap::Args, Debug)]
pub struct TableDeleteArgs {
    /// Table name (e.g. `incident`), or a combined `table:sys_id` /
    /// `table:number` reference (e.g. `incident:INC0010001`).
    pub table: String,
    /// sys_id of the record to delete. Omit when TABLE is a `table:id` reference.
    pub sys_id: Option<String>,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Cross-domain access if authorized.
    #[arg(long, alias = "sysparm-query-no-domain", help_heading = ADVANCED)]
    pub query_no_domain: bool,
}

pub fn list(global: &GlobalFlags, args: TableListArgs) -> Result<()> {
    let paginate = args.all;
    let array = args.array;
    let max_records = args.max_records;

    // Before `build_profile`/`build_client`, because this reads nothing but
    // argv and must not be preempted by them. `build_client` mints an OAuth
    // token, so a guard placed after it turns an unreachable instance or an
    // IdP outage into exit 3 for what is a fixable typo in the caller's own
    // command line — the usage error (exit 1) never gets a chance to print.
    if paginate {
        reject_unstreamable_output(global.output, array)?;
    }

    let client = connect(global)?;

    let q = ListQuery {
        query: args.query,
        fields: args.fields,
        page_size: Some(args.setlimit),
        offset: if paginate { None } else { args.offset },
        display_value: args.display_value.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        suppress_pagination_header: bool_opt(args.suppress_pagination_header),
        view: args.view,
        query_category: args.query_category,
        query_no_domain: bool_opt(args.query_no_domain),
        no_count: bool_opt(args.no_count),
    };
    let path = format!("/api/now/table/{}", args.table);

    if paginate {
        let cap = if max_records == 0 {
            None
        } else {
            Some(max_records)
        };
        let it = client.paginate(&path, &q.to_pairs(), cap);

        if array {
            let mut out = Vec::new();
            for r in it {
                out.push(r?);
            }
            write_response(global, &Value::Array(out))?;
        } else {
            let mut stdout = io::stdout().lock();
            for r in it {
                let v = r?;
                crate::output::write_jsonl_line(&mut stdout, &v)?;
            }
        }
        return Ok(());
    }

    let resp: Value = client.get(&path, &q.to_pairs())?;
    emit(global, resp)
}

/// Refuse the `--output` modes that `--all` cannot honor, instead of accepting
/// the flag and emitting JSONL anyway.
///
/// `--all` streams: the paginator flattens every page's `result` envelope into a
/// record-at-a-time iterator of unbounded length.
/// - `--output raw` means "keep the envelope", and after that flattening there
///   is no envelope left to keep — in the `--array` form either, which is why
///   this rejects raw for both.
/// - `--output table` has to see every row before it can size a column, so it
///   cannot render a stream. `--array` buffers the whole result set (bounded by
///   `--max-records`), which is exactly what the renderer needs — so that form
///   is allowed, and is what the error points at.
fn reject_unstreamable_output(mode: OutputMode, array: bool) -> Result<()> {
    match mode {
        OutputMode::Raw => Err(Error::Usage(
            "--output raw cannot be combined with --all: pagination flattens each page's \
             envelope into a record stream, so there is no envelope to keep; drop --all and \
             page with --offset/--setlimit, or drop --output raw"
                .into(),
        )),
        OutputMode::Table if !array => Err(Error::Usage(
            "--output table cannot render the unbounded stream --all produces; add --array to \
             buffer the records (capped by --max-records), or drop --all"
                .into(),
        )),
        _ => Ok(()),
    }
}

pub fn get(global: &GlobalFlags, args: TableGetArgs) -> Result<()> {
    let r = record_ref::parse_pair(&args.table, args.sys_id.as_deref(), "table")?;
    let client = connect(global)?;
    let sys_id = r.resolve(&client)?;
    let q = GetQuery {
        fields: args.fields,
        display_value: args.display_value.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        view: args.view,
        query_no_domain: bool_opt(args.query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", r.table, sys_id);
    let resp = client.get(&path, &q.to_pairs())?;
    emit(global, resp)
}

pub fn create(global: &GlobalFlags, args: TableCreateArgs) -> Result<()> {
    let body = require_body(args.body)?;

    let client = connect(global)?;
    let q = WriteQuery {
        fields: args.fields,
        display_value: args.display_value.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        input_display_value: bool_opt(args.input_display_value),
        suppress_auto_sys_field: bool_opt(args.suppress_auto_sys_field),
        view: args.view,
        query_no_domain: None,
    };
    let path = format!("/api/now/table/{}", args.table);
    let resp = client.post(&path, &q.to_pairs(), &body)?;
    emit(global, resp)
}

pub fn update(global: &GlobalFlags, args: TableUpdateArgs) -> Result<()> {
    let r = record_ref::parse_pair(&args.table, args.sys_id.as_deref(), "table")?;
    let body = require_body(args.body)?;
    let client = connect(global)?;
    let sys_id = r.resolve(&client)?;
    let q = WriteQuery {
        fields: args.fields,
        display_value: args.display_value.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        input_display_value: bool_opt(args.input_display_value),
        suppress_auto_sys_field: bool_opt(args.suppress_auto_sys_field),
        view: args.view,
        query_no_domain: bool_opt(args.query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", r.table, sys_id);
    let resp = client.patch(&path, &q.to_pairs(), &body)?;
    emit(global, resp)
}

/// Table's writes shipped their own empty-pair message before `build_body`
/// grew one; the wording is observable stderr, so it stays rather than
/// silently becoming [`crate::body::EmptyBody::Reject`]'s.
fn require_body(body: BodyArgs) -> Result<Value> {
    match body.into_input() {
        BodyInput::None => Err(Error::Usage("provide --data or one or more --field".into())),
        input => build_body(input),
    }
}

pub fn delete(global: &GlobalFlags, args: TableDeleteArgs) -> Result<()> {
    let r = record_ref::parse_pair(&args.table, args.sys_id.as_deref(), "table")?;
    // The confirm names what the caller typed (a number ref renders as
    // `table:number`): learning the sys_id would take a network call, and the
    // gate must stay pure argv.
    confirm_delete(args.yes, &r.to_string())?;
    let client = connect(global)?;
    let sys_id = r.resolve(&client)?;
    let q = DeleteQuery {
        query_no_domain: bool_opt(args.query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", r.table, sys_id);
    client.delete(&path, &q.to_pairs())
}
