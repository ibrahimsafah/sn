use crate::cli::{DisplayValueArg, GlobalFlags, ADVANCED};
use crate::error::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};

use super::table::{build_client, build_profile, unwrap_or_raw, write_response};

/// Score-series options, which only take effect alongside --include-scores.
const SCORE_DATA: &str = "Score data options";

#[derive(Subcommand, Debug)]
pub enum ScoresSub {
    /// List scorecards (GET /api/now/pa/scorecards).
    List(Box<ScoresListArgs>),
    /// Mark a scorecard as a favorite (POST /api/now/pa/scorecards).
    Favorite(ScoresFavoriteArgs),
    /// Remove a scorecard from favorites (DELETE /api/now/pa/scorecards).
    Unfavorite(ScoresFavoriteArgs),
}

#[derive(clap::Args, Debug)]
pub struct ScoresListArgs {
    /// Comma-separated scorecard UUIDs.
    #[arg(long, alias = "sysparm-uuid")]
    pub uuid: Option<String>,
    /// Breakdown sys_id.
    #[arg(long, alias = "sysparm-breakdown")]
    pub breakdown: Option<String>,
    /// Breakdown relation sys_id.
    #[arg(long, alias = "sysparm-breakdown-relation")]
    pub breakdown_relation: Option<String>,
    /// Elements filter sys_id.
    #[arg(long, alias = "sysparm-elements-filter")]
    pub elements_filter: Option<String>,
    /// Display value: true, false, or all.
    #[arg(long, alias = "sysparm-display", help_heading = ADVANCED, value_enum)]
    pub display: Option<DisplayValueArg>,
    /// Return only favorites.
    #[arg(long, alias = "sysparm-favorites")]
    pub favorites: bool,
    /// Return only key indicators.
    #[arg(long, alias = "sysparm-key")]
    pub key: bool,
    /// Return only indicators with a target.
    #[arg(long, alias = "sysparm-target")]
    pub target: bool,
    /// Comma-separated substrings to search for.
    #[arg(long, alias = "sysparm-contains")]
    pub contains: Option<String>,
    /// Comma-separated tag sys_ids to filter by.
    #[arg(long, alias = "sysparm-tags")]
    pub tags: Option<String>,
    /// Number of results per page (default 10, max 100).
    #[arg(long, alias = "sysparm-per-page", default_value_t = 10)]
    pub per_page: u32,
    /// Page number (default 1).
    #[arg(long, alias = "sysparm-page", default_value_t = 1)]
    pub page: u32,
    /// Field to sort by.
    #[arg(long, alias = "sysparm-sortby", value_enum)]
    pub sort_by: Option<SortBy>,
    /// Sort direction.
    #[arg(long, alias = "sysparm-sortdir", value_enum)]
    pub sort_dir: Option<SortDir>,
    /// Resolve reference/choice fields: true (display values), false (raw), or all (both).
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        default_value = "true"
    )]
    pub display_value: Option<DisplayValueArg>,
    /// Exclude reference link URLs from the response.
    #[arg(long, alias = "sysparm-exclude-reference-link", help_heading = ADVANCED)]
    pub exclude_reference_link: bool,
    /// Include historical score data.
    #[arg(long, alias = "sysparm-include-scores", help_heading = SCORE_DATA)]
    pub include_scores: bool,
    /// Start of score date range (ISO-8601).
    #[arg(long, alias = "sysparm-from", help_heading = SCORE_DATA)]
    pub from: Option<String>,
    /// End of score date range (ISO-8601).
    #[arg(long, alias = "sysparm-to", help_heading = SCORE_DATA)]
    pub to: Option<String>,
    /// Step between scores.
    #[arg(long, alias = "sysparm-step", help_heading = SCORE_DATA)]
    pub step: Option<u32>,
    /// Maximum number of scores to return (-1 = all).
    #[arg(long, alias = "sysparm-limit", help_heading = SCORE_DATA)]
    pub limit: Option<i64>,
    /// Include available breakdowns in the response.
    #[arg(long, alias = "sysparm-include-available-breakdowns", help_heading = ADVANCED)]
    pub include_available_breakdowns: bool,
    /// Include available aggregates in the response.
    #[arg(long, alias = "sysparm-include-available-aggregates", help_heading = ADVANCED)]
    pub include_available_aggregates: bool,
    /// Include real-time score data.
    #[arg(long, alias = "sysparm-include-realtime", help_heading = SCORE_DATA)]
    pub include_realtime: bool,
    /// Include target color scheme.
    #[arg(long, alias = "sysparm-include-target-color-scheme", help_heading = ADVANCED)]
    pub include_target_color_scheme: bool,
    /// Include forecast scores.
    #[arg(long, alias = "sysparm-include-forecast-scores", help_heading = SCORE_DATA)]
    pub include_forecast_scores: bool,
    /// Include trendline scores.
    #[arg(long, alias = "sysparm-include-trendline-scores", help_heading = SCORE_DATA)]
    pub include_trendline_scores: bool,
    /// Include prediction interval data.
    #[arg(long, alias = "sysparm-include-prediction-interval", help_heading = SCORE_DATA)]
    pub include_prediction_interval: bool,
}

#[derive(clap::Args, Debug)]
pub struct ScoresFavoriteArgs {
    /// Scorecard UUID.
    pub uuid: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SortBy {
    #[value(name = "VALUE")]
    Value,
    #[value(name = "CHANGE")]
    Change,
    #[value(name = "CHANGEPERC")]
    ChangePerc,
    #[value(name = "GAP")]
    Gap,
    #[value(name = "GAPPERC")]
    GapPerc,
    #[value(name = "NAME")]
    Name,
    #[value(name = "ORDER")]
    Order,
    #[value(name = "DEFAULT")]
    Default,
    #[value(name = "INDICATOR_GROUP")]
    IndicatorGroup,
    #[value(name = "FREQUENCY")]
    Frequency,
    #[value(name = "TARGET")]
    Target,
    #[value(name = "DATE")]
    Date,
    #[value(name = "DIRECTION")]
    Direction,
}

impl SortBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "VALUE",
            Self::Change => "CHANGE",
            Self::ChangePerc => "CHANGEPERC",
            Self::Gap => "GAP",
            Self::GapPerc => "GAPPERC",
            Self::Name => "NAME",
            Self::Order => "ORDER",
            Self::Default => "DEFAULT",
            Self::IndicatorGroup => "INDICATOR_GROUP",
            Self::Frequency => "FREQUENCY",
            Self::Target => "TARGET",
            Self::Date => "DATE",
            Self::Direction => "DIRECTION",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SortDir {
    #[value(name = "ASC")]
    Asc,
    #[value(name = "DESC")]
    Desc,
}

impl SortDir {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

const SCORECARDS_PATH: &str = "/api/now/pa/scorecards";

fn push_flag(q: &mut Vec<(String, String)>, key: &str, val: bool) {
    if val {
        q.push((key.into(), "true".into()));
    }
}

pub fn list(global: &GlobalFlags, args: ScoresListArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let mut q: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.uuid {
        q.push(("sysparm_uuid".into(), v));
    }
    if let Some(v) = args.breakdown {
        q.push(("sysparm_breakdown".into(), v));
    }
    if let Some(v) = args.breakdown_relation {
        q.push(("sysparm_breakdown_relation".into(), v));
    }
    if let Some(v) = args.elements_filter {
        q.push(("sysparm_elements_filter".into(), v));
    }
    if let Some(dv) = args.display {
        let dv: crate::query::DisplayValue = dv.into();
        q.push(("sysparm_display".into(), dv.as_str().to_string()));
    }
    push_flag(&mut q, "sysparm_favorites", args.favorites);
    push_flag(&mut q, "sysparm_key", args.key);
    push_flag(&mut q, "sysparm_target", args.target);
    if let Some(v) = args.contains {
        q.push(("sysparm_contains".into(), v));
    }
    if let Some(v) = args.tags {
        q.push(("sysparm_tags".into(), v));
    }
    q.push(("sysparm_per_page".into(), args.per_page.to_string()));
    q.push(("sysparm_page".into(), args.page.to_string()));
    if let Some(s) = args.sort_by {
        q.push(("sysparm_sortby".into(), s.as_str().to_string()));
    }
    if let Some(d) = args.sort_dir {
        q.push(("sysparm_sortdir".into(), d.as_str().to_string()));
    }
    if let Some(dv) = args.display_value {
        let dv: crate::query::DisplayValue = dv.into();
        q.push(("sysparm_display_value".into(), dv.as_str().to_string()));
    }
    push_flag(
        &mut q,
        "sysparm_exclude_reference_link",
        args.exclude_reference_link,
    );
    push_flag(&mut q, "sysparm_include_scores", args.include_scores);
    if let Some(v) = args.from {
        q.push(("sysparm_from".into(), v));
    }
    if let Some(v) = args.to {
        q.push(("sysparm_to".into(), v));
    }
    if let Some(v) = args.step {
        q.push(("sysparm_step".into(), v.to_string()));
    }
    if let Some(v) = args.limit {
        q.push(("sysparm_limit".into(), v.to_string()));
    }
    push_flag(
        &mut q,
        "sysparm_include_available_breakdowns",
        args.include_available_breakdowns,
    );
    push_flag(
        &mut q,
        "sysparm_include_available_aggregates",
        args.include_available_aggregates,
    );
    push_flag(&mut q, "sysparm_include_realtime", args.include_realtime);
    push_flag(
        &mut q,
        "sysparm_include_target_color_scheme",
        args.include_target_color_scheme,
    );
    push_flag(
        &mut q,
        "sysparm_include_forecast_scores",
        args.include_forecast_scores,
    );
    push_flag(
        &mut q,
        "sysparm_include_trendline_scores",
        args.include_trendline_scores,
    );
    push_flag(
        &mut q,
        "sysparm_include_prediction_interval",
        args.include_prediction_interval,
    );

    let resp = client.get(SCORECARDS_PATH, &q)?;
    let out = unwrap_or_raw(resp, global.output);
    write_response(global, &out)
}

pub fn favorite(global: &GlobalFlags, args: ScoresFavoriteArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let q = vec![("sysparm_uuid".to_string(), args.uuid)];
    let resp = client.post(SCORECARDS_PATH, &q, &json!({}))?;
    let out = unwrap_or_raw(resp, global.output);
    write_response(global, &out)
}

pub fn unfavorite(global: &GlobalFlags, args: ScoresFavoriteArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;

    let uuid = args.uuid.clone();
    let q = vec![("sysparm_uuid".to_string(), args.uuid)];
    // Same endpoint as `favorite`, so it reports the same way: a caller that can
    // parse one result must not have to special-case the other on empty stdout.
    // A DELETE answered 204/no body parses to `null`, which is nothing to branch
    // on, so name the operation instead.
    let out = match unwrap_or_raw(client.delete_json(SCORECARDS_PATH, &q)?, global.output) {
        Value::Null => json!({ "ok": true, "uuid": uuid }),
        v => v,
    };
    write_response(global, &out)
}
