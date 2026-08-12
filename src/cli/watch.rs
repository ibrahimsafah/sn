//! `sn watch` — live record watchers over ServiceNow's AMB websocket.
//!
//! The protocol itself lives in [`crate::amb`]. This module is the policy layer:
//! it turns a subcommand into a channel, supervises the socket (reconnect with
//! backoff), enforces the termination limits that make an infinite stream usable
//! from a script, filters events, and hydrates the survivors.
//!
//! ## What an event carries, and why `--hydrate` exists
//!
//! An AMB event carries values. Its `record` holds every field named in `changes`
//! — with the new value, as a `{display_value, value}` pair — plus five `sys_*`
//! audit columns (`sys_created_by/on`, `sys_updated_by/on`, `sys_mod_count`). On
//! an insert that is the whole populated row, since `changes` then lists every
//! field. This is what the stream emits, unchanged, by default.
//!
//! What an event does *not* carry is any field that did not change. A watch on
//! `state` that also wants `number` or `assigned_to` gets neither, because they
//! were not written. `--hydrate` opts into one Table API read per event, whose
//! result *replaces* `record` with the whole row. It costs an API call per event,
//! and note the row it fetches is the row as of the fetch, not as of the event:
//! a record written twice in quick succession can hydrate the first event with
//! the second event's values. The event's own `record` has no such skew.
//!
//! Until 0.10.0 hydration was the default, on the false premise that events carry
//! only `sys_*` columns.
//!
//! ## Why filtering is client-side
//!
//! An AMB channel is defined solely by table + encoded query: it delivers every
//! insert, update and delete touching a matching record, and there is no way to
//! ask the server for a subset. `--operation` and `--on-change` therefore filter
//! here, before hydration, so an unwanted event costs nothing.
//!
//! ## Gaps: the stream is best-effort, and says where it broke
//!
//! AMB has no replay and no cursor. A subscription starts at "now", so every
//! change that happened while a session was down is simply gone — no later
//! message carries it, and a reconnect cannot ask for it. A watch is therefore a
//! best-effort feed, not a log; anything that must be complete has to be
//! reconciled against the table itself over the outage window.
//!
//! What the watcher can do honestly is say *where* the hole is. On a successful
//! resubscribe after a drop it writes one synthetic line:
//!
//! ```text
//! {"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}
//! ```
//!
//! Everything between the preceding line and that marker is missing. Without it
//! a consumer cannot tell a quiet table from a lost one, which is the failure
//! this shape exists to prevent — see [`gap_marker`] for why it is keyed rather
//! than shaped like an event, and [`Stream`] for why it is not counted as one.

use crate::amb::{self, Amb, Event};
use crate::cli::table::{build_client, build_profile, DisplayValueArg};
use crate::cli::GlobalFlags;
use crate::client::Client;
use crate::config::ResolvedProfile;
use crate::error::{Error, Result};
use crate::observability::log_note;
use crate::output::{map_stdout_err, write_jsonl_line};
use crate::query::GetQuery;
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the read loop surfaces to check deadlines and Ctrl-C. Bounds how
/// long the command can take to notice it should stop.
const TICK: Duration = Duration::from_millis(500);

/// Give up after this many consecutive failed reconnects. The counter resets
/// once a session has stayed healthy (see [`HEALTHY`]), so a watcher running for
/// days is not capped at ten lifetime reconnects.
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// A session that lasted at least this long counts as healthy: its failure is a
/// fresh problem, not a continuation of a reconnect storm, so backoff restarts.
const HEALTHY: Duration = Duration::from_secs(60);

#[derive(Subcommand, Debug)]
pub enum WatchSub {
    #[command(about = "Watch records in a table matching a query or a single sys_id")]
    Table(WatchTableArgs),
    #[command(about = "Watch the count of records matching a query")]
    Count(WatchCountArgs),
    #[command(about = "Watch a record's activity stream (comments, work notes, field changes)")]
    Activity(WatchActivityArgs),
    #[command(about = "Subscribe to a raw AMB channel (escape hatch for unmodeled channels)")]
    Channel(WatchChannelArgs),
}

/// Bounds shared by every watch. Without at least one of these the command runs
/// forever, which is right for a terminal and useless inside a script — so they
/// are on every subcommand rather than a special mode.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct WatchLimits {
    /// Stop cleanly after N events.
    #[arg(long, value_name = "N")]
    pub max_events: Option<u64>,
    /// Stop cleanly after N seconds.
    #[arg(long, value_name = "SECS")]
    pub duration: Option<u64>,
    /// Stop cleanly if no event arrives for N seconds.
    #[arg(long, value_name = "SECS")]
    pub idle_timeout: Option<u64>,
}

/// The write that produced an event. ServiceNow also reports a parallel `action`
/// vocabulary (`entry`/`change`/`exit`); `operation` is the one that says what it
/// means, so it is what we match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Operation {
    Insert,
    Update,
    Delete,
}

impl Operation {
    fn as_str(self) -> &'static str {
        match self {
            Operation::Insert => "insert",
            Operation::Update => "update",
            Operation::Delete => "delete",
        }
    }
}

#[derive(clap::Args, Debug)]
#[command(group = clap::ArgGroup::new("target").required(true).args(["query", "sys_id"]))]
pub struct WatchTableArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Encoded query to watch, e.g. `priority=1^active=true`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Watch a single record. Shorthand for `--query sys_id=<SYS_ID>`.
    #[arg(long)]
    pub sys_id: Option<String>,
    /// Only emit these operations, e.g. `--operation insert`. Repeatable. Default: all.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "OP")]
    pub operation: Vec<Operation>,
    /// Only emit when one of these fields changed, e.g. `--on-change state,priority`.
    #[arg(
        long,
        alias = "watch-fields",
        value_delimiter = ',',
        value_name = "FIELDS"
    )]
    pub on_change: Vec<String>,
    /// Fetch the whole record for each event (one Table API read per event).
    /// Without it, `record` is the event's own: the fields that changed, with
    /// their new values.
    #[arg(long)]
    pub hydrate: bool,
    /// Comma-separated fields to fetch. Requires `--hydrate`.
    #[arg(long, short = 'f', alias = "sysparm-fields", requires = "hydrate")]
    pub fields: Option<String>,
    /// Resolve reference/choice fields: false (default), true, or all. Requires `--hydrate`.
    #[arg(
        long,
        alias = "sysparm-display-value",
        value_enum,
        requires = "hydrate"
    )]
    pub display_value: Option<DisplayValueArg>,
    /// Deprecated: not hydrating is now the default. Accepted as a no-op so
    /// scripts written against 0.9.1 keep working — they already ask for what
    /// they now get.
    #[arg(long, hide = true, conflicts_with = "hydrate")]
    pub no_hydrate: bool,
    #[command(flatten)]
    pub limits: WatchLimits,
}

#[derive(clap::Args, Debug)]
pub struct WatchCountArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Encoded query whose matching-record count to watch, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: String,
    #[command(flatten)]
    pub limits: WatchLimits,
}

#[derive(clap::Args, Debug)]
pub struct WatchActivityArgs {
    /// sys_id of the record whose activity stream to watch.
    pub sys_id: String,
    #[command(flatten)]
    pub limits: WatchLimits,
}

#[derive(clap::Args, Debug)]
pub struct WatchChannelArgs {
    /// Raw AMB channel, e.g. `/rw/default/incident/<base64>` or `/uxbannerannouncements`.
    pub channel: String,
    #[command(flatten)]
    pub limits: WatchLimits,
}

/// What to fetch when an event names a record.
struct Hydrate {
    /// Fallback table; an event's own `table_name` wins when present, so a watch
    /// on a base table still hydrates from the row's actual class.
    table: String,
    query: GetQuery,
}

/// Which events are worth emitting.
///
/// AMB has no server-side operation or field filter — a channel delivers every
/// change to every matching record — so selectivity has to happen here. It is
/// applied *before* hydration, so a rejected event never costs a Table API call.
#[derive(Default)]
struct Filter {
    /// Empty means every operation.
    operations: Vec<Operation>,
    /// Empty means any field.
    on_change: Vec<String>,
}

impl Filter {
    fn accepts(&self, data: &Value) -> bool {
        if !self.operations.is_empty() {
            let op = data.get("operation").and_then(Value::as_str).unwrap_or("");
            if !self.operations.iter().any(|o| o.as_str() == op) {
                return false;
            }
        }

        if !self.on_change.is_empty() {
            // `changes` is every field the write touched. On insert that is all
            // of them (so watching a field does catch new records that set it);
            // on delete it is empty (so a field filter never matches a delete);
            // on update it also includes fields ServiceNow *derived* from the
            // ones written — setting `urgency` reports `priority` as changed too.
            let hit = data
                .get("changes")
                .and_then(Value::as_array)
                .is_some_and(|changed| {
                    changed
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|c| self.on_change.iter().any(|want| want == c))
                });
            if !hit {
                return false;
            }
        }
        true
    }
}

/// A resolved watch: the channels to subscribe to, what to keep, whether to
/// hydrate, and when to stop.
struct Plan {
    channels: Vec<String>,
    filter: Filter,
    hydrate: Option<Hydrate>,
    limits: WatchLimits,
}

pub fn run(global: &GlobalFlags, sub: WatchSub) -> Result<()> {
    let plan = match sub {
        WatchSub::Table(a) => {
            // --sys-id is sugar for the filter the UI itself uses.
            let filter = match (&a.query, &a.sys_id) {
                (Some(q), _) => q.clone(),
                (None, Some(id)) => format!("sys_id={id}"),
                (None, None) => unreachable!("clap ArgGroup requires one"),
            };
            Plan {
                channels: vec![amb::record_channel(&a.table, &filter)],
                filter: Filter {
                    operations: a.operation,
                    on_change: a.on_change,
                },
                hydrate: a.hydrate.then(|| Hydrate {
                    table: a.table.clone(),
                    query: GetQuery {
                        fields: a.fields.clone(),
                        display_value: a.display_value.map(Into::into),
                        ..Default::default()
                    },
                }),
                limits: a.limits,
            }
        }
        // Count, activity and raw-channel payloads carry neither `operation` nor
        // `changes` nor a record, so there is nothing to filter or hydrate.
        WatchSub::Count(a) => Plan {
            channels: vec![amb::count_channel(&a.table, &a.query)],
            filter: Filter::default(),
            hydrate: None,
            limits: a.limits,
        },
        WatchSub::Activity(a) => Plan {
            channels: vec![amb::activity_channel(&a.sys_id)],
            filter: Filter::default(),
            hydrate: None,
            limits: a.limits,
        },
        WatchSub::Channel(a) => Plan {
            channels: vec![a.channel],
            filter: Filter::default(),
            hydrate: None,
            limits: a.limits,
        },
    };
    watch(global, plan)
}

fn watch(global: &GlobalFlags, plan: Plan) -> Result<()> {
    let profile = build_profile(global)?;
    ensure_transport_supported(&profile)?;
    let client = build_client(&profile, global.timeout)?;

    let stop = install_sigint()?;
    let base = client.base_url().to_string();
    let connect_timeout = Duration::from_secs(global.timeout.unwrap_or(30));
    // The socket does not inherit reqwest's TLS config, so hand it the same
    // certificate policy the profile gave the HTTP client.
    let tls = amb::TlsOptions {
        insecure: profile.insecure,
        ca_cert: profile.ca_cert.clone(),
    };

    let deadline = plan
        .limits
        .duration
        .map(|s| Instant::now() + Duration::from_secs(s));
    let idle = plan.limits.idle_timeout.map(Duration::from_secs);
    // Held for the whole command rather than taken per line: nothing else in a
    // watch writes to stdout (`log_note` goes to stderr), so there is no one to
    // interleave with.
    let mut stream = Stream::new(io::stdout().lock(), plan.limits.max_events);

    let mut attempt: u32 = 0;
    // Retrying is only ever right for a connection that once worked.
    let mut established = false;
    // Armed only after a drop, so the first session announces nothing.
    let mut gap: Option<Gap> = None;

    loop {
        let started = Instant::now();
        let outcome = session(
            &client,
            &base,
            connect_timeout,
            &tls,
            &plan,
            &stop,
            deadline,
            idle,
            &mut stream,
            &mut established,
            &mut gap,
        );
        let ended = Instant::now();

        let next = decide(&SessionEnd {
            error: outcome.as_ref().err(),
            established,
            lifetime: ended - started,
            attempt,
            stopping: stop.load(Ordering::SeqCst) || past(deadline),
        });
        match next {
            Next::Stop => return Ok(()),
            Next::Fail => return outcome,
            Next::Retry {
                attempt: next_attempt,
                wait,
            } => {
                attempt = next_attempt;
                let e = outcome.expect_err("Next::Retry is only reachable with an error");
                log_note(&format!(
                    "amb: {e}; reconnecting in {}s (attempt {attempt}/{MAX_RECONNECT_ATTEMPTS})",
                    wait.as_secs()
                ));
                if !sleep_interruptibly(wait, &stop, deadline) {
                    return Ok(());
                }
                gap = Some(extend(gap, ended, attempt));
            }
        }
    }
}

/// What the supervisor does once a session has ended.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Exit 0: a limit was reached, or the caller asked to stop.
    Stop,
    /// Give up and report the session's error.
    Fail,
    /// Wait, then open a replacement session.
    Retry { attempt: u32, wait: Duration },
}

/// Everything the post-session decision depends on, as plain data.
///
/// The reconnect policy is the part of the supervisor worth testing and the
/// socket is the part that makes testing it expensive, so they are separated:
/// [`decide`] is pure, and the loop above only carries out its verdict.
struct SessionEnd<'a> {
    /// `None` when the session returned cleanly.
    error: Option<&'a Error>,
    /// Whether any session so far reached the subscribed state.
    established: bool,
    /// How long the session that just ended lasted.
    lifetime: Duration,
    /// Consecutive failed reconnects *before* this one.
    attempt: u32,
    /// Ctrl-C fired, or the overall `--duration` deadline passed.
    stopping: bool,
}

/// The whole reconnect policy. Order matters: an error is disqualified before
/// Ctrl-C is consulted, so a genuinely broken configuration still exits non-zero
/// even if the caller interrupted while it was failing.
fn decide(end: &SessionEnd<'_>) -> Next {
    let Some(e) = end.error else {
        return Next::Stop;
    };
    // Never got off the ground: the instance, the profile or AMB itself is
    // wrong, and retrying ten times over six minutes only postpones the same
    // error. Report it now. Likewise for an error that will reproduce exactly.
    if !end.established || !is_recoverable(e) {
        return Next::Fail;
    }
    // Ctrl-C or a deadline racing the socket teardown is not a failure.
    if end.stopping {
        return Next::Stop;
    }
    // A session that stayed up was healthy; its death starts a new problem, so
    // don't carry the old backoff into it.
    let attempt = if end.lifetime >= HEALTHY {
        1
    } else {
        end.attempt + 1
    };
    if attempt > MAX_RECONNECT_ATTEMPTS {
        return Next::Fail;
    }
    Next::Retry {
        attempt,
        wait: backoff(attempt),
    }
}

/// A hole in the stream: the interval between one session dropping and its
/// replacement being subscribed. Outlives a failed attempt, so a gap that takes
/// several tries to close is still reported as one interval.
#[derive(Clone, Copy)]
struct Gap {
    /// When the session whose loss opened the hole died.
    since: Instant,
    /// Which reconnect attempt closed it.
    attempt: u32,
}

/// Fold a fresh drop into whatever gap is still waiting to be announced.
///
/// One hole, however many attempts it takes to close. A gap still pending here
/// belongs to an attempt that died before it could announce anything, so its
/// start time carries forward: the consumer needs the whole outage, not the last
/// slice of it. `attempt` always takes the newest value — it is the counter the
/// backoff is on, and the marker reports which try finally worked.
fn extend(gap: Option<Gap>, dropped_at: Instant, attempt: u32) -> Gap {
    Gap {
        since: gap.map_or(dropped_at, |g| g.since),
        attempt,
    }
}

/// The line that marks a hole.
///
/// AMB has no replay, so whatever changed during `downtime_ms` is gone and no
/// later message will carry it. This marker is the only evidence a consumer ever
/// gets that its feed is incomplete, which drives both of its properties:
///
/// - **It is keyed, not shaped like an event.** `sn_watch` appears on nothing
///   else, and the marker deliberately carries neither `operation` nor `changes`
///   — the two fields `--operation`/`--on-change` match on, and the two a
///   consumer's own `jq` predicate is most likely to test. A marker that looked
///   like an event would be silently dropped by exactly the pipelines that most
///   need to see it.
/// - **It is not an event.** See [`Stream`]: it moves neither `--max-events` nor
///   the `--idle-timeout` clock.
fn gap_marker(downtime: Duration, attempt: u32) -> Value {
    json!({
        "sn_watch": "reconnected",
        // serde_json numbers top out at u64; a watcher would have to be down for
        // half a billion years to reach that, but saturating beats a panic.
        "downtime_ms": u64::try_from(downtime.as_millis()).unwrap_or(u64::MAX),
        "attempt": attempt,
    })
}

/// The output side of a watch, plus the two limits that bound it.
///
/// It is its own type because of one rule that is easy to state and easy to
/// break: **a meta line is not an event**. `--max-events` counts the records the
/// caller asked for, and `--idle-timeout` asks whether a record arrived — so a
/// supervisor notice must move neither counter. Otherwise a flapping connection
/// would spend the caller's event budget on its own chatter, and keep alive a
/// watcher that has delivered nothing.
struct Stream<W: Write> {
    out: W,
    max_events: Option<u64>,
    emitted: u64,
    /// When the last *event* went out. Deliberately not reset by a new session:
    /// an outage is time during which no event arrived, which is precisely what
    /// `--idle-timeout` asks about. (Before the reconnect marker existed, each
    /// session restarted this clock, so a connection that flapped faster than
    /// the timeout could hold a silent watcher open forever.)
    last_event: Instant,
}

impl<W: Write> Stream<W> {
    fn new(out: W, max_events: Option<u64>) -> Self {
        Self {
            out,
            max_events,
            emitted: 0,
            last_event: Instant::now(),
        }
    }

    /// Emit an event. Returns true once `--max-events` is satisfied.
    fn event(&mut self, v: &Value) -> Result<bool> {
        self.write(v)?;
        self.emitted += 1;
        self.last_event = Instant::now();
        Ok(self.max_events.is_some_and(|m| self.emitted >= m))
    }

    /// Emit a supervisor notice. See the type doc: not an event, so neither
    /// counter moves.
    fn meta(&mut self, v: &Value) -> Result<()> {
        self.write(v)
    }

    fn idle_expired(&self, idle: Option<Duration>) -> bool {
        idle.is_some_and(|i| self.last_event.elapsed() >= i)
    }

    /// One JSON value per line, flushed immediately. Without the flush a piped
    /// stdout is block-buffered and `sn watch … | jq` would look frozen for
    /// minutes.
    fn write(&mut self, v: &Value) -> Result<()> {
        write_jsonl_line(&mut self.out, v)?;
        self.out.flush().map_err(map_stdout_err)
    }
}

/// One socket lifetime: mint a session, connect, subscribe, pump until a limit
/// is reached or the connection breaks.
#[allow(clippy::too_many_arguments)]
fn session<W: Write>(
    client: &Client,
    base: &str,
    connect_timeout: Duration,
    tls: &amb::TlsOptions,
    plan: &Plan,
    stop: &Arc<AtomicBool>,
    deadline: Option<Instant>,
    idle: Option<Duration>,
    stream: &mut Stream<W>,
    established: &mut bool,
    gap: &mut Option<Gap>,
) -> Result<()> {
    // The quirk: AMB authenticates by session cookie, so an ordinary HTTP call
    // has to mint one before the socket can be opened. Goes out over the normal
    // client, so Basic and OAuth profiles both just work.
    let cookies = client.session_cookies()?;
    let mut amb = Amb::connect(base, &cookies, connect_timeout, tls)?;

    for channel in &plan.channels {
        amb.subscribe(channel)?;
    }
    // Subscribed: from here on a failure is a blip in something that worked, so
    // it earns a reconnect rather than an immediate error.
    *established = true;
    log_note(&format!("amb: subscribed to {}", plan.channels.join(", ")));

    // Announce the hole only now, and clear it only here. An attempt that dies
    // before this point has closed nothing, so it leaves the gap standing for
    // its successor to report in full — one marker per gap, not one per attempt.
    if let Some(g) = gap.take() {
        stream.meta(&gap_marker(g.since.elapsed(), g.attempt))?;
    }

    let result = pump(&mut amb, client, plan, stop, deadline, idle, stream);
    amb.disconnect();
    result
}

#[allow(clippy::too_many_arguments)]
fn pump<W: Write>(
    amb: &mut Amb,
    client: &Client,
    plan: &Plan,
    stop: &Arc<AtomicBool>,
    deadline: Option<Instant>,
    idle: Option<Duration>,
    stream: &mut Stream<W>,
) -> Result<()> {
    loop {
        if stop.load(Ordering::SeqCst) || past(deadline) {
            return Ok(());
        }
        if stream.idle_expired(idle) {
            return Ok(());
        }

        // An empty batch means the poll simply expired — that is the tick that
        // lets the checks above run.
        for event in amb.poll(TICK)? {
            // Filter first: a discarded event must not cost a hydration call,
            // must not count against --max-events, and must not reset the idle
            // clock — otherwise `--operation insert --idle-timeout 30` would be
            // held open indefinitely by updates the caller said it did not want.
            if !plan.filter.accepts(&event.data) {
                continue;
            }
            if stream.event(&shape(event, client, plan.hydrate.as_ref()))? {
                return Ok(());
            }
        }
    }
}

/// Turn a raw AMB event into the record the caller actually wants.
///
/// With no `Hydrate` the event passes through untouched, which is the default:
/// its `record` is already the changed fields with their new values. When
/// hydrating, the fetched row *replaces* that record rather than merging with it.
///
/// Hydration failure is never fatal: a record can be deleted, or lose its ACL,
/// between the event firing and the fetch landing. Losing one record's detail
/// must not take down a stream the caller is relying on.
fn shape(event: Event, client: &Client, hydrate: Option<&Hydrate>) -> Value {
    let mut data = event.data;
    let (Some(h), Some(obj)) = (hydrate, data.as_object_mut()) else {
        return data;
    };

    // A deleted record cannot be fetched; say so rather than reporting an error.
    if obj.get("operation").and_then(Value::as_str) == Some("delete") {
        obj.insert("record".into(), Value::Null);
        return data;
    }

    let Some(sys_id) = obj.get("sys_id").and_then(Value::as_str).map(str::to_owned) else {
        return data;
    };
    let table = obj
        .get("table_name")
        .and_then(Value::as_str)
        .unwrap_or(&h.table)
        .to_owned();

    match client.get(
        &format!("/api/now/table/{table}/{sys_id}"),
        &h.query.to_pairs(),
    ) {
        Ok(v) => {
            obj.insert("record".into(), take_result(v));
        }
        Err(e) => {
            obj.insert("record".into(), Value::Null);
            obj.insert("hydrate_error".into(), json!(e.to_string()));
        }
    }
    data
}

/// Unwrap a `{"result": …}` envelope by moving the subtree out.
///
/// Every other command unwraps once per invocation; hydration does it once per
/// *event*, for the lifetime of the stream. Cloning the subtree out of a value
/// that is about to be dropped (`v.get("result").cloned().unwrap_or(v)`) copies
/// the whole record on every event for nothing.
fn take_result(v: Value) -> Value {
    match v {
        Value::Object(mut m) => match m.remove("result") {
            Some(r) => r,
            None => Value::Object(m),
        },
        other => other,
    }
}

/// The websocket is opened directly rather than through reqwest, so it does not
/// inherit the HTTP client's transport settings — each has to be carried across
/// deliberately. `--insecure` and `--ca-cert` are (see [`amb::TlsOptions`]);
/// proxying is not, because tunnelling a websocket needs HTTP CONNECT.
///
/// Refuse rather than ignore. Quietly bypassing a proxy the caller configured
/// would send the session cookie somewhere they did not sanction, which is a
/// worse outcome than not running at all.
fn ensure_transport_supported(p: &ResolvedProfile) -> Result<()> {
    if p.proxy.is_none() {
        return Ok(());
    }
    Err(Error::Config(
        "`sn watch` does not support a proxy: the AMB websocket is opened \
         directly, not through the HTTP client. Re-run with --no-proxy if the \
         instance is reachable without it."
            .into(),
    ))
}

/// SIGINT flips a flag the read loop already checks, so Ctrl-C unwinds through
/// the normal path — `/meta/disconnect`, socket close, exit 0 — instead of
/// killing the process mid-frame at exit 130.
fn install_sigint() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|e| Error::Transport(format!("install signal handler: {e}")))?;
    Ok(stop)
}

/// Only a broken connection is worth retrying. A bad table or malformed query
/// (`Api`) and a rejected session (`Auth`) will fail identically forever.
fn is_recoverable(e: &Error) -> bool {
    matches!(e, Error::Transport(_))
}

fn past(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}

fn backoff(attempt: u32) -> Duration {
    // 2s, 4s, 8s … capped at a minute, matching the server's own advice.
    let secs = 2u64.saturating_pow(attempt).min(60);
    Duration::from_secs(secs)
}

/// Sleep, but stay responsive to Ctrl-C and the overall deadline. Returns false
/// if the caller should stop instead of continuing to wait.
fn sleep_interruptibly(total: Duration, stop: &Arc<AtomicBool>, deadline: Option<Instant>) -> bool {
    let until = Instant::now() + total;
    while Instant::now() < until {
        if stop.load(Ordering::SeqCst) || past(deadline) {
            return false;
        }
        std::thread::sleep(TICK);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payloads copied from real events observed on a live instance.
    fn insert() -> Value {
        json!({"operation": "insert", "action": "entry",
               "changes": ["number", "state", "priority", "short_description", "urgency"]})
    }
    fn update() -> Value {
        json!({"operation": "update", "action": "change",
               "changes": ["state", "incident_state"]})
    }
    fn delete() -> Value {
        json!({"operation": "delete", "action": "exit", "changes": []})
    }

    #[test]
    fn empty_filter_accepts_everything() {
        let f = Filter::default();
        assert!(f.accepts(&insert()));
        assert!(f.accepts(&update()));
        assert!(f.accepts(&delete()));
    }

    #[test]
    fn operation_filter_selects_one_kind() {
        let f = Filter {
            operations: vec![Operation::Insert],
            ..Default::default()
        };
        assert!(f.accepts(&insert()));
        assert!(!f.accepts(&update()));
        assert!(!f.accepts(&delete()));
    }

    #[test]
    fn operation_filter_accepts_any_listed_kind() {
        let f = Filter {
            operations: vec![Operation::Insert, Operation::Delete],
            ..Default::default()
        };
        assert!(f.accepts(&insert()));
        assert!(f.accepts(&delete()));
        assert!(!f.accepts(&update()));
    }

    #[test]
    fn on_change_matches_a_changed_field() {
        let f = Filter {
            on_change: vec!["state".into()],
            ..Default::default()
        };
        assert!(f.accepts(&update()), "state is in the changed list");
    }

    #[test]
    fn on_change_rejects_an_untouched_field() {
        let f = Filter {
            on_change: vec!["assigned_to".into()],
            ..Default::default()
        };
        assert!(!f.accepts(&update()));
    }

    #[test]
    fn on_change_matches_an_insert_that_set_the_field() {
        // Inserts report every populated field as changed, so watching a field
        // legitimately catches a new record that sets it.
        let f = Filter {
            on_change: vec!["short_description".into()],
            ..Default::default()
        };
        assert!(f.accepts(&insert()));
    }

    #[test]
    fn on_change_never_matches_a_delete() {
        // A delete carries `changes: []` — nothing changed, the row left.
        let f = Filter {
            on_change: vec!["state".into()],
            ..Default::default()
        };
        assert!(!f.accepts(&delete()));
    }

    #[test]
    fn operation_and_on_change_are_anded() {
        let f = Filter {
            operations: vec![Operation::Update],
            on_change: vec!["state".into()],
        };
        assert!(f.accepts(&update()));
        // Right field, wrong operation.
        assert!(!f.accepts(&insert()));

        let g = Filter {
            operations: vec![Operation::Update],
            on_change: vec!["assigned_to".into()],
        };
        // Right operation, wrong field.
        assert!(!g.accepts(&update()));
    }

    #[test]
    fn a_payload_without_the_keys_is_rejected_by_any_filter() {
        // Count/activity payloads carry neither key; a filter must not silently
        // treat "field absent" as "matches".
        let bare = json!({"count": 7});
        assert!(!Filter {
            operations: vec![Operation::Update],
            ..Default::default()
        }
        .accepts(&bare));
        assert!(!Filter {
            on_change: vec!["state".into()],
            ..Default::default()
        }
        .accepts(&bare));
    }

    // ── the reconnect gap marker ────────────────────────────────────────────

    #[test]
    fn the_marker_reports_the_downtime_and_the_attempt() {
        assert_eq!(
            gap_marker(Duration::from_millis(4100), 2),
            json!({"sn_watch": "reconnected", "downtime_ms": 4100, "attempt": 2})
        );
    }

    #[test]
    fn an_absurd_downtime_saturates_rather_than_panicking() {
        // `as_millis` is u128 and serde_json numbers are u64. Unreachable in
        // practice; a panic in the one line that reports a fault is not.
        let m = gap_marker(Duration::from_secs(u64::MAX), 1);
        assert_eq!(m["downtime_ms"], json!(u64::MAX));
    }

    #[test]
    fn the_marker_is_not_shaped_like_an_event() {
        // Its whole job is to survive filtering. `--operation`/`--on-change`
        // match on these two keys, and so does the average consumer's own jq
        // predicate — a marker carrying either could be mistaken for a record
        // change, and one carrying neither can only be recognised by its key.
        let m = gap_marker(Duration::from_secs(1), 1);
        assert!(m.get("operation").is_none(), "{m}");
        assert!(m.get("changes").is_none(), "{m}");
        assert!(m.get("record").is_none(), "{m}");
        assert_eq!(m["sn_watch"], "reconnected");
    }

    #[test]
    fn a_gap_that_takes_several_attempts_is_still_one_interval() {
        // Attempt 1 dies before it can subscribe, so it announces nothing. The
        // downtime attempt 2 reports has to span the whole outage, not just the
        // part after attempt 1 gave up.
        let drop = Instant::now();
        let first = extend(None, drop, 1);
        let second = extend(Some(first), drop + Duration::from_secs(5), 2);
        assert_eq!(second.since, drop, "the hole opened once");
        assert_eq!(
            second.attempt, 2,
            "reported as closed by the try that worked"
        );
    }

    #[test]
    fn a_fresh_drop_after_an_announced_gap_starts_a_new_interval() {
        // `session` clears the gap once it has emitted the marker, so the next
        // drop must time from itself rather than from the previous outage.
        let drop = Instant::now();
        assert_eq!(extend(None, drop, 1).since, drop);
    }

    // ── Stream: a meta line is not an event ─────────────────────────────────

    /// A stream writing into a buffer, so the bytes a consumer would see are
    /// what the assertions look at.
    fn buffered(max_events: Option<u64>) -> Stream<Vec<u8>> {
        Stream::new(Vec::new(), max_events)
    }

    fn lines(s: &Stream<Vec<u8>>) -> Vec<Value> {
        String::from_utf8(s.out.clone())
            .expect("output is utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is one JSON value"))
            .collect()
    }

    #[test]
    fn a_meta_line_does_not_count_against_max_events() {
        let mut s = buffered(Some(2));
        s.meta(&gap_marker(Duration::from_secs(4), 1)).unwrap();
        s.meta(&gap_marker(Duration::from_secs(4), 2)).unwrap();
        s.meta(&gap_marker(Duration::from_secs(4), 3)).unwrap();
        assert!(
            !s.event(&update()).unwrap(),
            "three markers must not have consumed a budget of two events"
        );
        assert!(s.event(&update()).unwrap(), "the second event satisfies it");
    }

    #[test]
    fn a_meta_line_does_not_reset_the_idle_clock() {
        let idle = Some(Duration::from_millis(5));
        let mut s = buffered(None);
        std::thread::sleep(Duration::from_millis(30));
        assert!(s.idle_expired(idle), "nothing has arrived");

        s.meta(&gap_marker(Duration::from_secs(4), 1)).unwrap();
        assert!(
            s.idle_expired(idle),
            "a reconnect notice is not an arrival: --idle-timeout must still fire"
        );

        s.event(&update()).unwrap();
        assert!(!s.idle_expired(idle), "a real event does reset it");
    }

    #[test]
    fn max_events_is_satisfied_by_the_nth_event_and_never_without_one() {
        let mut s = buffered(Some(1));
        assert!(s.event(&insert()).unwrap());

        let mut unbounded = buffered(None);
        for _ in 0..50 {
            assert!(!unbounded.event(&insert()).unwrap());
        }
    }

    #[test]
    fn every_value_is_one_flushed_jsonl_line() {
        let mut s = buffered(None);
        s.event(&insert()).unwrap();
        s.meta(&gap_marker(Duration::from_millis(4100), 2)).unwrap();
        s.event(&update()).unwrap();

        let out = lines(&s);
        assert_eq!(out.len(), 3, "one line per value: {out:?}");
        // The acceptance criterion: the marker sits between the pre-drop and
        // post-drop events, so a consumer can place the hole exactly.
        assert_eq!(out[0]["operation"], "insert");
        assert_eq!(out[1]["sn_watch"], "reconnected");
        assert_eq!(out[2]["operation"], "update");
    }

    // ── the reconnect policy ────────────────────────────────────────────────

    fn ended(error: Option<&Error>) -> SessionEnd<'_> {
        SessionEnd {
            error,
            established: true,
            lifetime: Duration::from_secs(1),
            attempt: 0,
            stopping: false,
        }
    }

    fn dropped() -> Error {
        Error::Transport("amb websocket closed".into())
    }

    #[test]
    fn a_clean_session_stops() {
        assert_eq!(decide(&ended(None)), Next::Stop);
    }

    #[test]
    fn a_connection_that_never_established_fails_instead_of_retrying() {
        // Ten backoff rounds is ~6 minutes of postponing the same error.
        let e = dropped();
        let end = SessionEnd {
            established: false,
            ..ended(Some(&e))
        };
        assert_eq!(decide(&end), Next::Fail);
    }

    #[test]
    fn an_unrecoverable_error_fails_even_after_establishing() {
        let e = Error::Auth {
            status: 401,
            message: "nope".into(),
            transaction_id: None,
        };
        assert_eq!(decide(&ended(Some(&e))), Next::Fail);
    }

    #[test]
    fn ctrl_c_racing_the_teardown_is_not_a_failure() {
        let e = dropped();
        let end = SessionEnd {
            stopping: true,
            ..ended(Some(&e))
        };
        assert_eq!(decide(&end), Next::Stop);
    }

    #[test]
    fn consecutive_short_sessions_escalate_the_backoff() {
        let e = dropped();
        let short = Duration::from_secs(1);
        for (attempt, wait) in [(0u32, 2u64), (1, 4), (2, 8)] {
            assert_eq!(
                decide(&SessionEnd {
                    lifetime: short,
                    attempt,
                    ..ended(Some(&e))
                }),
                Next::Retry {
                    attempt: attempt + 1,
                    wait: Duration::from_secs(wait)
                }
            );
        }
    }

    #[test]
    fn a_healthy_session_restarts_the_backoff_and_the_budget() {
        // A watcher running for days must not be capped at ten lifetime
        // reconnects, so a session that stayed up resets the counter — even one
        // that had already exhausted it.
        let e = dropped();
        assert_eq!(
            decide(&SessionEnd {
                lifetime: HEALTHY,
                attempt: MAX_RECONNECT_ATTEMPTS,
                ..ended(Some(&e))
            }),
            Next::Retry {
                attempt: 1,
                wait: Duration::from_secs(2)
            }
        );
    }

    #[test]
    fn a_reconnect_storm_gives_up_after_the_cap() {
        let e = dropped();
        assert_eq!(
            decide(&SessionEnd {
                lifetime: Duration::from_secs(1),
                attempt: MAX_RECONNECT_ATTEMPTS,
                ..ended(Some(&e))
            }),
            Next::Fail
        );
    }

    // ── hydration envelope ──────────────────────────────────────────────────

    #[test]
    fn take_result_unwraps_the_envelope() {
        let row = json!({"sys_id": "abc", "number": "INC0001"});
        assert_eq!(take_result(json!({"result": row.clone()})), row);
    }

    #[test]
    fn take_result_keeps_a_body_that_has_no_envelope() {
        // Some responses arrive already unwrapped; dropping them would replace
        // the record with null.
        let bare = json!({"sys_id": "abc"});
        assert_eq!(take_result(bare.clone()), bare);
        assert_eq!(take_result(json!([1, 2])), json!([1, 2]));
        assert_eq!(take_result(Value::Null), Value::Null);
    }

    #[test]
    fn backoff_doubles_then_caps_at_a_minute() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(10), Duration::from_secs(60));
        // Must not overflow into a multi-century sleep.
        assert_eq!(backoff(64), Duration::from_secs(60));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn only_transport_errors_are_retried() {
        assert!(is_recoverable(&Error::Transport("socket died".into())));
        // A rejected session will be rejected identically on every retry.
        assert!(!is_recoverable(&Error::Auth {
            status: 401,
            message: "nope".into(),
            transaction_id: None,
        }));
        // So will a table that does not exist.
        assert!(!is_recoverable(&Error::Api {
            status: 400,
            message: "bad table".into(),
            detail: None,
            transaction_id: None,
            sn_error: None,
        }));
        assert!(!is_recoverable(&Error::Config("bad profile".into())));
    }

    fn profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "p".into(),
            instance: "dev1.service-now.com".into(),
            username: "u".into(),
            password: "p".into(),
            proxy: None,
            no_proxy: None,
            insecure: false,
            ca_cert: None,
            proxy_ca_cert: None,
            proxy_username: None,
            proxy_password: None,
            auth_method: crate::config::AuthMethod::Basic,
            oauth: None,
        }
    }

    #[test]
    fn plain_profile_is_supported() {
        assert!(ensure_transport_supported(&profile()).is_ok());
    }

    #[test]
    fn tls_overrides_are_carried_to_the_socket_not_refused() {
        // The websocket honors these itself (see amb::TlsOptions), so they must
        // not trip the transport check.
        let mut p = profile();
        p.insecure = true;
        p.ca_cert = Some("/etc/corp-ca.pem".into());
        assert!(ensure_transport_supported(&p).is_ok());
    }

    #[test]
    fn a_proxy_is_refused_rather_than_silently_bypassed() {
        let mut p = profile();
        p.proxy = Some("http://proxy:8080".into());
        let err = ensure_transport_supported(&p).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("proxy"), "must name the culprit: {msg}");
        assert!(msg.contains("--no-proxy"), "must offer a way out: {msg}");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn delete_event_is_not_hydrated_and_reports_a_null_record() {
        // No client call can succeed for a row that no longer exists, so `shape`
        // must short-circuit before touching the network.
        let event = Event {
            channel: "/rw/default/incident/x".into(),
            data: json!({
                "table_name": "incident",
                "sys_id": "abc",
                "operation": "delete",
                "record": {"sys_mod_count": {"value": "3"}},
            }),
        };
        let h = Hydrate {
            table: "incident".into(),
            query: GetQuery::default(),
        };
        // A null client would panic if `shape` tried to fetch; reaching the
        // assert at all proves it did not.
        let out = shape(event, &unreachable_client(), Some(&h));
        assert_eq!(out["record"], Value::Null);
        assert_eq!(out["operation"], "delete");
    }

    #[test]
    fn events_pass_through_untouched_without_hydration() {
        let data = json!({"table_name": "incident", "sys_id": "abc", "operation": "update"});
        let event = Event {
            channel: "/rw/default/incident/x".into(),
            data: data.clone(),
        };
        assert_eq!(shape(event, &unreachable_client(), None), data);
    }

    /// A client pointed at an address nothing will answer on. `shape` must never
    /// reach it in these tests; if it does, the test fails on the timeout rather
    /// than silently passing.
    fn unreachable_client() -> Client {
        let p = ResolvedProfile {
            instance: "127.0.0.1:1".into(),
            ..profile()
        };
        Client::builder()
            .timeout(Duration::from_millis(1))
            .build(&p)
            .expect("builder should not need the network")
    }
}
