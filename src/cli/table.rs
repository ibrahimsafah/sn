use crate::body::{build_body, BodyInput};
use crate::cli::{GlobalFlags, OutputMode, ADVANCED};
use crate::client::{Auth, Client};
use crate::config::{
    config_path, credentials_path, load_config_from, load_credentials_from, resolve_profile,
    AuthMethod, ProfileResolverInputs, ResolvedProfile,
};
use crate::error::{Error, Result};
use crate::output::{emit_value, Format, ResolvedFormat};
use crate::query::{DeleteQuery, GetQuery, ListQuery, WriteQuery};
use clap::{Subcommand, ValueEnum};
use is_terminal::IsTerminal;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::io;
use std::time::Duration;

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
    /// Maximum records returned (default 1000). Maps to sysparm_limit.
    #[arg(
        long,
        alias = "limit",
        alias = "sysparm-limit",
        alias = "page-size",
        default_value_t = 1000
    )]
    pub setlimit: u32,
    /// Starting offset for manual pagination (ignored with --all).
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
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
    /// Table name (e.g. `incident`).
    pub table: String,
    /// sys_id of the record to fetch.
    pub sys_id: String,
    /// Comma-separated fields to return.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
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
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Comma-separated fields to return on the created record.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
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
    /// Table name (e.g. `incident`).
    pub table: String,
    /// sys_id of the record to patch.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Comma-separated fields to return on the updated record.
    #[arg(long, short = 'f', alias = "sysparm-fields")]
    pub fields: Option<String>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
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
    /// Table name (e.g. `incident`).
    pub table: String,
    /// sys_id of the record to delete.
    pub sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Cross-domain access if authorized.
    #[arg(long, alias = "sysparm-query-no-domain", help_heading = ADVANCED)]
    pub query_no_domain: bool,
}

pub fn list(global: &GlobalFlags, args: TableListArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let paginate = args.all;
    let array = args.array;
    let max_records = args.max_records;

    let q = ListQuery {
        query: args.query,
        fields: args.fields,
        page_size: Some(args.setlimit),
        offset: if paginate { None } else { args.offset },
        display_value: args.display_value.map(Into::into),
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
    let out = unwrap_or_raw(resp, global.output);
    write_response(global, &out)?;
    Ok(())
}

pub(crate) fn build_profile(global: &GlobalFlags) -> Result<ResolvedProfile> {
    let config = load_config_from(&config_path()?)?;
    let creds = load_credentials_from(&credentials_path()?)?;
    let env_proxy = std::env::var("SN_PROXY").ok();
    let env_no_proxy = std::env::var("SN_NO_PROXY").ok();
    let env_insecure = std::env::var("SN_INSECURE").ok();
    let env_ca_cert = std::env::var("SN_CA_CERT").ok();
    let env_proxy_ca_cert = std::env::var("SN_PROXY_CA_CERT").ok();
    resolve_profile(ProfileResolverInputs {
        cli_profile: global.profile.as_deref(),
        cli_proxy: global.proxy.as_deref(),
        env_proxy: env_proxy.as_deref(),
        cli_no_proxy: global.no_proxy,
        env_no_proxy: env_no_proxy.as_deref(),
        cli_insecure: global.insecure,
        env_insecure: env_insecure.as_deref(),
        cli_ca_cert: global.ca_cert.as_deref(),
        env_ca_cert: env_ca_cert.as_deref(),
        cli_proxy_ca_cert: global.proxy_ca_cert.as_deref(),
        env_proxy_ca_cert: env_proxy_ca_cert.as_deref(),
        config: &config,
        credentials: &creds,
    })
}

pub(crate) fn build_client(profile: &ResolvedProfile, timeout: Option<u64>) -> Result<Client> {
    build_client_with_headers(profile, timeout, HeaderMap::new())
}

/// [`build_client`] plus caller-supplied request headers (`sn raw --header`),
/// which the client applies after auth and `Content-Type` so they win. A
/// separate entry point rather than a third parameter on `build_client`: only
/// `raw` has headers to pass, and every other call site stays untouched.
pub(crate) fn build_client_with_headers(
    profile: &ResolvedProfile,
    timeout: Option<u64>,
    extra_headers: HeaderMap,
) -> Result<Client> {
    let mut b = Client::builder()
        .extra_headers(extra_headers)
        .proxy(profile.proxy.clone())
        .no_proxy(profile.no_proxy.clone())
        .insecure(profile.insecure)
        .ca_cert(profile.ca_cert.clone())
        .proxy_ca_cert(profile.proxy_ca_cert.clone())
        .proxy_auth(
            profile.proxy_username.clone(),
            profile.proxy_password.clone(),
        );
    // OAuth profiles attach a bearer token, refreshing (or minting, for
    // client-credentials) it transparently and persisting any new tokens.
    // Basic profiles fall through to the builder's default username/password.
    if matches!(profile.auth_method, AuthMethod::Oauth) {
        let token = crate::oauth::ensure_access_token(profile, timeout)?;
        b = b.auth(Auth::Bearer { token });
    }
    if let Some(secs) = timeout {
        b = b.timeout(Duration::from_secs(secs));
    }
    b.build(profile)
}

pub(crate) fn bool_opt(b: bool) -> Option<bool> {
    if b {
        Some(true)
    } else {
        None
    }
}

pub(crate) fn format_from_flags(g: &GlobalFlags) -> ResolvedFormat {
    if g.pretty {
        Format::Pretty.resolve()
    } else if g.compact {
        Format::Compact.resolve()
    } else {
        Format::Auto.resolve()
    }
}

pub(crate) fn unwrap_or_raw(v: Value, mode: OutputMode) -> Value {
    match mode {
        OutputMode::Raw => v,
        OutputMode::Default | OutputMode::Table => v.get("result").cloned().unwrap_or(v),
    }
}

/// Write a response value to stdout in whichever shape the global `--output` flag selects:
/// JSON (`default`/`raw`) or human-readable columnar (`table`). Centralizes the OutputMode
/// dispatch so each command's call site stays a one-liner.
pub(crate) fn write_response(global: &GlobalFlags, value: &Value) -> Result<()> {
    if matches!(global.output, OutputMode::Table) {
        crate::output_table::write_table(value)
    } else {
        emit_value(io::stdout().lock(), value, format_from_flags(global))
            .map_err(crate::output::map_stdout_err)
    }
}

pub fn get(global: &GlobalFlags, args: TableGetArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let q = GetQuery {
        fields: args.fields,
        display_value: args.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        view: args.view,
        query_no_domain: bool_opt(args.query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", args.table, args.sys_id);
    let resp = client.get(&path, &q.to_pairs())?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn create(global: &GlobalFlags, args: TableCreateArgs) -> Result<()> {
    let body_input = match (args.data, args.field.is_empty()) {
        (Some(d), true) => BodyInput::Data(d),
        (None, false) => BodyInput::Fields(args.field),
        (None, true) => return Err(Error::Usage("provide --data or one or more --field".into())),
        (Some(_), false) => {
            return Err(Error::Usage(
                "--data and --field are mutually exclusive".into(),
            ))
        }
    };
    let body = build_body(body_input)?;

    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let q = WriteQuery {
        fields: args.fields,
        display_value: args.display_value.map(Into::into),
        exclude_reference_link: bool_opt(args.exclude_reference_link),
        input_display_value: bool_opt(args.input_display_value),
        suppress_auto_sys_field: bool_opt(args.suppress_auto_sys_field),
        view: args.view,
        query_no_domain: None,
    };
    let path = format!("/api/now/table/{}", args.table);
    let resp = client.post(&path, &q.to_pairs(), &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn update(global: &GlobalFlags, args: TableUpdateArgs) -> Result<()> {
    write_op(
        global,
        args.table,
        args.sys_id,
        args.data,
        args.field,
        args.fields,
        args.display_value,
        args.exclude_reference_link,
        args.input_display_value,
        args.suppress_auto_sys_field,
        args.view,
        args.query_no_domain,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_op(
    global: &GlobalFlags,
    table: String,
    sys_id: String,
    data: Option<String>,
    field: Vec<String>,
    fields: Option<String>,
    display_value: Option<DisplayValueArg>,
    exclude_reference_link: bool,
    input_display_value: bool,
    suppress_auto_sys_field: bool,
    view: Option<String>,
    query_no_domain: bool,
) -> Result<()> {
    let body_input = match (data, field.is_empty()) {
        (Some(d), true) => BodyInput::Data(d),
        (None, false) => BodyInput::Fields(field),
        (None, true) => return Err(Error::Usage("provide --data or one or more --field".into())),
        (Some(_), false) => {
            return Err(Error::Usage(
                "--data and --field are mutually exclusive".into(),
            ))
        }
    };
    let body = build_body(body_input)?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let q = WriteQuery {
        fields,
        display_value: display_value.map(Into::into),
        exclude_reference_link: bool_opt(exclude_reference_link),
        input_display_value: bool_opt(input_display_value),
        suppress_auto_sys_field: bool_opt(suppress_auto_sys_field),
        view,
        query_no_domain: bool_opt(query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", table, sys_id);
    let resp = client.patch(&path, &q.to_pairs(), &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

/// Gate a destructive operation behind a confirmation, shared by every `delete`
/// command. With `--yes` it is a no-op; otherwise it refuses to proceed on a
/// non-interactive stdin (exit 1) and, on a TTY, prompts `Delete {what}? [y/N]:`
/// and aborts unless the answer is affirmative. `what` names the target, e.g.
/// `incident/abc123`, `attachment abc123`, or `relation r1 on cmdb_ci_server/x`.
pub(crate) fn confirm_delete(yes: bool, what: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Usage(
            "delete requires --yes when stdin is not a terminal".into(),
        ));
    }
    eprint!("Delete {what}? [y/N]: ");
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .map_err(|e| Error::Usage(format!("read stdin: {e}")))?;
    if !matches!(s.trim(), "y" | "Y" | "yes" | "YES") {
        return Err(Error::Usage("aborted".into()));
    }
    Ok(())
}

pub fn delete(global: &GlobalFlags, args: TableDeleteArgs) -> Result<()> {
    confirm_delete(args.yes, &format!("{}/{}", args.table, args.sys_id))?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let q = DeleteQuery {
        query_no_domain: bool_opt(args.query_no_domain),
    };
    let path = format!("/api/now/table/{}/{}", args.table, args.sys_id);
    client.delete(&path, &q.to_pairs())
}
