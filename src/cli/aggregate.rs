use crate::cli::{DisplayValueArg, GlobalFlags};
use crate::error::Result;

use super::kernel::{bool_opt, connect, emit};

#[derive(clap::Args, Debug)]
pub struct AggregateArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Encoded query filter.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Comma-separated fields to average.
    #[arg(long, alias = "sysparm-avg-fields")]
    pub avg_fields: Option<String>,
    /// Count the number of records in the query.
    #[arg(long, alias = "sysparm-count")]
    pub count: bool,
    /// Comma-separated fields for minimum value.
    #[arg(long, alias = "sysparm-min-fields")]
    pub min_fields: Option<String>,
    /// Comma-separated fields for maximum value.
    #[arg(long, alias = "sysparm-max-fields")]
    pub max_fields: Option<String>,
    /// Comma-separated fields to sum.
    #[arg(long, alias = "sysparm-sum-fields")]
    pub sum_fields: Option<String>,
    /// Comma-separated fields to group by.
    #[arg(long, alias = "sysparm-group-by")]
    pub group_by: Option<String>,
    /// Comma-separated fields to order by.
    #[arg(long, alias = "sysparm-order-by")]
    pub order_by: Option<String>,
    /// Aggregate filter (HAVING clause).
    #[arg(long, alias = "sysparm-having")]
    pub having: Option<String>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
    /// Query category for index selection.
    #[arg(long, alias = "sysparm-query-category")]
    pub query_category: Option<String>,
}

pub fn run(global: &GlobalFlags, args: AggregateArgs) -> Result<()> {
    let client = connect(global)?;

    let mut q: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        q.push(("sysparm_query".into(), v));
    }
    if let Some(v) = args.avg_fields {
        q.push(("sysparm_avg_fields".into(), v));
    }
    if let Some(b) = bool_opt(args.count) {
        q.push(("sysparm_count".into(), b.to_string()));
    }
    if let Some(v) = args.min_fields {
        q.push(("sysparm_min_fields".into(), v));
    }
    if let Some(v) = args.max_fields {
        q.push(("sysparm_max_fields".into(), v));
    }
    if let Some(v) = args.sum_fields {
        q.push(("sysparm_sum_fields".into(), v));
    }
    if let Some(v) = args.group_by {
        q.push(("sysparm_group_by".into(), v));
    }
    if let Some(v) = args.order_by {
        q.push(("sysparm_order_by".into(), v));
    }
    if let Some(v) = args.having {
        q.push(("sysparm_having".into(), v));
    }
    if let Some(dv) = args.display_value {
        let dv: crate::query::DisplayValue = dv.into();
        q.push(("sysparm_display_value".into(), dv.as_str().to_string()));
    }
    if let Some(v) = args.query_category {
        q.push(("sysparm_query_category".into(), v));
    }

    let path = format!("/api/now/stats/{}", args.table);
    let resp = client.get(&path, &q)?;
    emit(global, resp)
}
