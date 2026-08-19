//! `sn watch` — live record watchers over ServiceNow's AMB websocket.
//!
//! The protocol itself lives in [`crate::amb`]. This module is the policy layer:
//! it turns the arguments into a channel, supervises the socket (reconnect with
//! backoff), enforces the termination limits that make an infinite stream usable
//! from a script, and filters events.
//!
//! ## What an event carries
//!
//! An AMB event carries values. Its `record` holds every field named in `changes`
//! — with the new value, as a `{display_value, value}` pair — plus five `sys_*`
//! audit columns (`sys_created_by/on`, `sys_updated_by/on`, `sys_mod_count`). On
//! an insert that is the whole populated row, since `changes` then lists every
//! field. This is what the stream emits, unchanged.
//!
//! What an event does *not* carry is any field that did not change. A watch on
//! `state` that also wants `number` or `assigned_to` gets neither, because they
//! were not written — read them explicitly (`sn table get <table> <sys_id>
//! --fields …`), which keeps the extra API call and its timing visible at the
//! call site. A `--hydrate` flag used to hide that read behind the stream (one
//! Table API GET per event, *replacing* `record` with the whole row); it was
//! removed in 0.13.0 because it traded a correct answer for a wrong one — a
//! fetched row is the row as of the fetch, so a record written twice in quick
//! succession hydrated the first event with the second write's values, and
//! nothing said so. The flag itself was a survivor of the pre-0.10.0 false
//! premise that events carry only `sys_*` columns (hydration was the *default*
//! until checking the wire disproved that).
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
//! The routine threat — the instance reaping a watcher's HTTP session ~60–120s
//! after it was minted (measured live; neither AMB traffic nor HTTP touches
//! prevent it) — does not produce drops at all: [`pump_rotating`] replaces the
//! session on a schedule (`--session-rotate`, default 45s, under the measured
//! floor) with an overlapped handoff that opens no gap and writes no marker.
//! A marker therefore reports a *genuine* failure: a network drop, or a
//! rotation that lost its race to the reaper. That fallback is
//! [`crate::amb::Amb::poll`] surfacing the session's death the moment a connect
//! reply reports it — the alternative was a watcher that kept polling the dead
//! session and silently delivered nothing forever.
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
//!
//! ## What `--idle-timeout` measures
//!
//! Subscribed time, and only subscribed time. The question the flag asks is
//! "has anything happened for N seconds?", and while the watcher is opening its
//! first socket or reconnecting after a drop it has no way to know — nothing
//! could reach it either way. So every interval spent off the channel is
//! forgiven ([`Stream::resume`]): the clock is started by the first successful
//! subscribe, not by the process, and an outage is added back rather than
//! counted. The marker's `downtime_ms` is exactly the interval that was
//! forgiven.
//!
//! The alternative — wall-clock time since the last event — made two silent
//! wrong answers, both of them exit 0 with a stream that looks merely quiet: an
//! instance slower to mint a session than the timeout never reached its first
//! poll, and any outage longer than the timeout (guaranteed once backoff caps
//! at 60s) made the marker the last line of the stream, so the hole it had just
//! announced was never covered. What forgiving does *not* do is restart the
//! clock per session, which would let a socket flapping faster than the timeout
//! hold a watcher open forever: silence accumulates across sessions.

use crate::amb::{self, Amb, Event};
use crate::cli::GlobalFlags;
use crate::cli::kernel::{build_client, build_profile};
use crate::config::ResolvedProfile;
use crate::error::{Error, Result};
use crate::observability::log_note;
use crate::output::write_jsonl_line;
use clap::ValueEnum;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How soon to try rotating again after a failed rotation attempt. Short,
/// because the clock is ticking toward the instance's session reaper: a
/// replacement that cannot be built before the reap fires means falling back to
/// the reactive reconnect path and its gap.
const ROTATE_RETRY: Duration = Duration::from_secs(10);

/// How long one drain poll waits for the old session's buffered frames. They
/// are already in the socket buffer, so this is latency budget, not a hold.
const DRAIN_WAIT: Duration = Duration::from_millis(100);

/// Upper bound on drain polls, so a session delivering a flood cannot pin the
/// watcher on a socket that is about to be discarded anyway — anything past
/// this is also flowing on the already-subscribed replacement.
const DRAIN_POLLS: usize = 50;

/// How many *consecutive* empty drain polls mean the old session is dry. One is
/// not enough: an empty poll is ambiguous between a read timeout and a
/// meta-only batch (the old session's expired long-poll being answered and
/// re-armed), and quitting on the ambiguous one abandons frames the re-armed
/// connect was about to deliver.
const DRAIN_QUIET_POLLS: usize = 3;

/// How long after a rotation begins the seam dedup stays armed. The overlap it
/// guards — writes the old session delivers in the drain and the new session
/// delivers again from its own buffer — is a few seconds wide; outside a seam
/// the dedup is inert, so it can never eat a legitimate event on the normal
/// path.
const SEAM_TTL: Duration = Duration::from_secs(30);

/// Bounds on the stream. Without at least one of these the command runs
/// forever, which is right for a terminal and useless inside a script — so they
/// sit beside the other flags rather than in a special mode.
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
pub struct WatchArgs {
    /// Table name (e.g. `incident`).
    pub table: String,
    /// Encoded query to watch, e.g. `priority=1^active=true` — or
    /// `sys_id=<SYS_ID>` for a single record.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: String,
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
    /// Replace the session every N seconds, before the instance can reap it
    /// (measured floor ~60s). The replacement subscribes before the old session
    /// disconnects, so rotation opens no gap. 0 disables rotation.
    #[arg(long, value_name = "SECS", default_value_t = 45)]
    pub session_rotate: u64,
    /// Deprecated: events are never hydrated (`--hydrate` was removed in
    /// 0.13.0). Accepted as a no-op so scripts written against 0.9.1 keep
    /// working — they already ask for what they now get.
    #[arg(long, hide = true)]
    pub no_hydrate: bool,
    #[command(flatten)]
    pub limits: WatchLimits,
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

/// A resolved watch: the channel to subscribe to, what to keep, and when to
/// stop.
struct Plan {
    channel: String,
    filter: Filter,
    limits: WatchLimits,
    /// Replace the session this often, before the instance's reaper can. `None`
    /// disables rotation and leaves only the reactive reconnect path.
    rotate: Option<Duration>,
}

pub fn run(global: &GlobalFlags, a: WatchArgs) -> Result<()> {
    let plan = Plan {
        channel: amb::record_channel(&a.table, &a.query),
        filter: Filter {
            operations: a.operation,
            on_change: a.on_change,
        },
        limits: a.limits,
        rotate: (a.session_rotate > 0).then(|| Duration::from_secs(a.session_rotate)),
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

    // The quirk: AMB authenticates by session cookie, so an ordinary HTTP call
    // has to mint one before each socket can be opened. Each attempt rebuilds
    // the client rather than reusing the one above: for OAuth profiles that is
    // what re-runs `ensure_access_token`, and reconnecting is the designed
    // steady state (the instance reaps the session every minute or two), so a
    // watch outliving its access token would otherwise die exit 4 at the first
    // reconnect past expiry — with a usable refresh token sitting on disk.
    let mut connect = || -> Result<Amb> {
        let cookies = build_client(&profile, global.timeout)?.session_cookies()?;
        Amb::connect(&base, &cookies, connect_timeout, &tls)
    };
    let mut wait = |d: Duration| sleep_interruptibly(d, &stop, deadline);

    let ctx = Ctx {
        plan: &plan,
        stop: &stop,
        deadline,
        idle,
    };
    supervise(&ctx, &mut connect, &mut stream, &mut wait)
}

/// Everything a session needs that outlives it. Passed as one borrow so the
/// session functions stay readable as the supervisor grows.
struct Ctx<'a> {
    plan: &'a Plan,
    stop: &'a AtomicBool,
    deadline: Option<Instant>,
    idle: Option<Duration>,
}

/// The subscribe-and-read half of an AMB session, as the supervisor uses it.
///
/// A trait rather than the concrete [`Amb`] for one reason: the marker's central
/// claim — it lands between the last event of a dropped session and the first
/// event of its replacement — is a property of a *sequence* of sessions, and
/// producing a real drop needs a Bayeux server (wiremock speaks only HTTP). This
/// is the seam that lets a test drive drop → resubscribe and read the bytes a
/// consumer would see.
trait Link {
    fn subscribe(&mut self, channel: &str) -> Result<()>;
    fn poll(&mut self, wait: Duration) -> Result<Vec<Event>>;
    fn disconnect(&mut self);
}

impl Link for Amb {
    fn subscribe(&mut self, channel: &str) -> Result<()> {
        Amb::subscribe(self, channel)
    }
    fn poll(&mut self, wait: Duration) -> Result<Vec<Event>> {
        Amb::poll(self, wait)
    }
    fn disconnect(&mut self) {
        Amb::disconnect(self);
    }
}

/// One session after another, with [`decide`]'s policy in between.
///
/// Generic over the link and over how one is opened, so the whole loop —
/// including what the stream looks like across a drop — can be driven without a
/// socket. `wait` is the backoff sleep, injected for the same reason.
fn supervise<W, L, C>(
    ctx: &Ctx<'_>,
    connect: &mut C,
    stream: &mut Stream<W>,
    wait: &mut dyn FnMut(Duration) -> bool,
) -> Result<()>
where
    W: Write,
    L: Link,
    C: FnMut() -> Result<L>,
{
    let mut attempt: u32 = 0;
    // Retrying is only ever right for a connection that once worked.
    let mut established = false;
    // Armed only after a drop, so the first session announces nothing. Holds the
    // ordinal of the reconnect that will close the hole.
    let mut gap: Option<u32> = None;
    // Start of the interval the watcher is currently spending *not* subscribed:
    // before the first successful subscribe, then from each drop until the
    // resubscribe that ends it. One measurement, two uses — the idle clock
    // forgives it, and the marker reports it.
    let mut offline_since = Instant::now();

    loop {
        let started = Instant::now();
        let outcome = session(
            ctx,
            connect,
            stream,
            &mut established,
            &mut gap,
            offline_since,
        );
        let ended = Instant::now();

        let next = decide(&SessionEnd {
            error: outcome.as_ref().err(),
            established,
            lifetime: ended - started,
            attempt,
            stopping: ctx.stop.load(Ordering::SeqCst) || past(ctx.deadline),
        });
        match next {
            Next::Stop => return Ok(()),
            Next::Fail => return outcome,
            Next::Retry {
                attempt: next_attempt,
                wait: pause,
            } => {
                attempt = next_attempt;
                let e = outcome.expect_err("Next::Retry is only reachable with an error");
                log_note(&format!(
                    "amb: {e}; reconnecting in {}s (attempt {attempt}/{MAX_RECONNECT_ATTEMPTS})",
                    pause.as_secs()
                ));
                if !wait(pause) {
                    return Ok(());
                }
                // One hole, however many attempts it takes to close. A gap still
                // pending belongs to an attempt that died before it could
                // announce anything, so the outage keeps dating from the drop
                // that opened it; only a session that was actually subscribed
                // starts a new one.
                if gap.is_none() {
                    offline_since = ended;
                }
                gap = Some(attempt);
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
    /// The ordinal of the reconnect that opened the session that just ended: 0
    /// for the first session, which no reconnect produced. It is where the next
    /// attempt's ordinal (and so its backoff) counts from — not a tally of
    /// failures, since the reconnect this names may well have succeeded.
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
    /// When the last *event* went out — in subscribed time. Not a plain
    /// timestamp: [`resume`](Stream::resume) pushes it forward by every interval
    /// the watcher spent off the channel, so `elapsed()` reads how long the
    /// watcher has been listening to silence rather than how long it has been
    /// running. A new session does not reset it, so silence accumulates across
    /// reconnects.
    last_event: Instant,
    /// Keys of events emitted inside the current rotation seam. Session
    /// rotation overlaps the old and new subscription, so one write can arrive
    /// on both; the second copy is dropped here. Keyed on
    /// `sys_id|operation|sys_mod_count`, so a genuine re-write — which
    /// increments `sys_mod_count` — never matches.
    recent: Vec<String>,
    /// When the current seam stops being suspect. `None` (the steady state)
    /// makes [`duplicate`](Stream::duplicate) inert, so the dedup cannot eat a
    /// legitimate event anywhere but the few seconds around a rotation.
    seam_until: Option<Instant>,
}

impl<W: Write> Stream<W> {
    fn new(out: W, max_events: Option<u64>) -> Self {
        Self {
            out,
            max_events,
            emitted: 0,
            last_event: Instant::now(),
            recent: Vec::new(),
            seam_until: None,
        }
    }

    /// Arm the seam dedup for the rotation that is about to drain. Clears the
    /// previous seam's keys: nothing older than the last handoff can still be
    /// in flight on either session.
    fn open_seam(&mut self) {
        self.recent.clear();
        self.seam_until = Some(Instant::now() + SEAM_TTL);
    }

    /// Whether this raw event was already emitted inside the current rotation
    /// seam, recording it if not. Inert outside a seam. An event with no
    /// `sys_id` (or no usable write identity — see [`dedup_key`]) is never
    /// treated as a duplicate: dropping an unidentifiable event is worse than
    /// repeating one.
    fn duplicate(&mut self, data: &Value) -> bool {
        match self.seam_until {
            Some(until) if Instant::now() <= until => {}
            Some(_) => {
                self.recent.clear();
                self.seam_until = None;
                return false;
            }
            None => return false,
        }
        let Some(key) = dedup_key(data) else {
            return false;
        };
        if self.recent.contains(&key) {
            return true;
        }
        self.recent.push(key);
        false
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

    /// Forgive an interval the watcher spent *not* subscribed — the first
    /// connect, or an outage — by pushing the idle clock forward by it.
    ///
    /// Nothing could have arrived while there was no channel to arrive on, so
    /// that time is not idleness. Counting it exited 0 with an empty stream
    /// whenever connecting outlasted `--idle-timeout`, which reads exactly like
    /// a quiet table. Forgiving is deliberately weaker than resetting: what is
    /// returned is only the offline interval, so silence either side of it still
    /// adds up and a flapping socket cannot hold a silent watcher open.
    ///
    /// Clamped to now. `offline` is always a measured interval that ends at the
    /// call, so it cannot overshoot today — but a clock parked in the future
    /// would measure the next silence from the wrong end and never expire.
    fn resume(&mut self, offline: Duration) {
        let now = Instant::now();
        self.last_event = self.last_event.checked_add(offline).unwrap_or(now).min(now);
    }

    fn idle_expired(&self, idle: Option<Duration>) -> bool {
        idle.is_some_and(|i| self.last_event.elapsed() >= i)
    }

    /// One JSON value per line, flushed immediately — without that, a piped
    /// stdout is block-buffered and `sn watch … | jq` looks frozen for minutes.
    ///
    /// The flush belongs to [`write_jsonl_line`], which owns it for every
    /// streaming caller. Repeating it here is not merely redundant: a second
    /// flush is a second syscall on a pipe that may already be gone, so a
    /// reader that closed after the first one turns the *next* record's write
    /// into the failure instead of this one, and the error is attributed to the
    /// wrong record.
    fn write(&mut self, v: &Value) -> Result<()> {
        write_jsonl_line(&mut self.out, v)
    }
}

/// The identity of one write, as its event reports it — or `None` when the
/// event carries no identity sharp enough to dedup on.
///
/// `sys_mod_count` increments per write, so two events with the same key are
/// the same write seen twice, not two writes. A table without that column
/// (`syslog` is one, measured) sends events that cannot tell one write from the
/// next — those are never keyed, because deduping them would swallow a genuine
/// second write. A delete carries no record at all, so `sys_id` alone
/// identifies it; a delete→re-insert→delete of one `sys_id` inside a single
/// seam window is accepted as out of scope.
fn dedup_key(data: &Value) -> Option<String> {
    let sys_id = data.get("sys_id").and_then(Value::as_str)?;
    let operation = data.get("operation").and_then(Value::as_str).unwrap_or("");
    if operation == "delete" {
        return Some(format!("{sys_id}|delete|"));
    }
    let mod_count = data
        .pointer("/record/sys_mod_count/value")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })?;
    Some(format!("{sys_id}|{operation}|{mod_count}"))
}

/// One socket lifetime: open a link, subscribe, pump until a limit is reached or
/// the connection breaks.
///
/// `offline_since` is when the watcher stopped being subscribed — process start
/// for the first session, the drop that opened the hole for a replacement.
fn session<W: Write, L: Link, C: FnMut() -> Result<L>>(
    ctx: &Ctx<'_>,
    connect: &mut C,
    stream: &mut Stream<W>,
    established: &mut bool,
    gap: &mut Option<u32>,
    offline_since: Instant,
) -> Result<()> {
    let mut link = connect()?;

    link.subscribe(&ctx.plan.channel)?;
    // Subscribed: from here on a failure is a blip in something that worked, so
    // it earns a reconnect rather than an immediate error.
    *established = true;
    log_note(&format!("amb: subscribed to {}", ctx.plan.channel));

    // Everything since `offline_since` was spent connecting, not listening. One
    // measurement, two uses: the idle clock forgives it, and the marker reports
    // it, so the two can never disagree about how long the hole was.
    let offline = offline_since.elapsed();
    stream.resume(offline);

    // Announce the hole only now, and clear it only here. An attempt that dies
    // before this point has closed nothing, so it leaves the gap standing for
    // its successor to report in full — one marker per gap, not one per attempt.
    let announced = match gap.take() {
        Some(attempt) => stream.meta(&gap_marker(offline, attempt)),
        None => Ok(()),
    };

    // Every post-subscribe exit disconnects, the failed announcement included:
    // `sn watch … | head -2` closes stdout mid-marker, and returning straight
    // out of here would abandon a live subscription without /meta/disconnect.
    let result = announced.and_then(|()| pump_rotating(&mut link, ctx, connect, stream));
    link.disconnect();
    result
}

/// Why [`pump`] surfaced without an error.
enum PumpEnd {
    /// A limit was reached or the caller asked to stop.
    Stopped,
    /// The rotation deadline passed; time to build a replacement session.
    Rotate,
}

/// Pump one link, replacing it on schedule so the instance's session reaper
/// never catches a live watch.
///
/// This is make-before-break: the replacement is connected *and subscribed*
/// before the old session is touched, so at every instant at least one live
/// subscription exists and a rotation opens no gap — no marker, no idle-clock
/// forgiveness, nothing on stdout. The old session keeps buffering while the
/// replacement is built; [`drain`] empties it before it is dropped, and the
/// seam — where one write arrives on both sessions — is closed by
/// [`Stream::duplicate`].
///
/// A rotation that cannot build its replacement is not an error: the current
/// session is still healthy, so keep pumping it and retry shortly
/// ([`ROTATE_RETRY`]). If the reaper wins that race the session dies, and the
/// supervisor's reactive path — reconnect, backoff, gap marker — takes over,
/// which is exactly the degraded behavior the marker exists to report.
fn pump_rotating<W: Write, L: Link, C: FnMut() -> Result<L>>(
    link: &mut L,
    ctx: &Ctx<'_>,
    connect: &mut C,
    stream: &mut Stream<W>,
) -> Result<()> {
    let mut rotate_at = ctx.plan.rotate.map(|d| Instant::now() + d);
    loop {
        match pump(link, ctx, stream, rotate_at)? {
            PumpEnd::Stopped => return Ok(()),
            PumpEnd::Rotate => {
                // The replacement's reap countdown starts at its cookie mint,
                // not when the drain finishes — anchor its rotation deadline
                // there, or the handoff time is silently spent out of the
                // margin between the rotation interval and the reap floor.
                let minted = Instant::now();
                match replacement(ctx, connect) {
                    Ok(mut fresh) => {
                        stream.open_seam();
                        let done = match drain(link, ctx, stream) {
                            Ok(done) => done,
                            // The drain failed on this process's stdout, not on
                            // a socket. Both sessions are subscribed and both
                            // must say goodbye: the old one via session()'s
                            // teardown once this propagates, the new one here —
                            // dropping it would strand a live subscription with
                            // no /meta/disconnect.
                            Err(e) => {
                                fresh.disconnect();
                                return Err(e);
                            }
                        };
                        link.disconnect();
                        *link = fresh;
                        rotate_at = ctx.plan.rotate.map(|d| minted + d);
                        log_note("amb: rotated to a fresh session");
                        if done {
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        log_note(&format!(
                            "amb: session rotation failed ({e}); keeping the current session"
                        ));
                        rotate_at = Some(Instant::now() + ROTATE_RETRY);
                    }
                }
            }
        }
    }
}

/// A connected, subscribed successor for the current session.
fn replacement<L: Link, C: FnMut() -> Result<L>>(ctx: &Ctx<'_>, connect: &mut C) -> Result<L> {
    let mut fresh = connect()?;
    fresh.subscribe(&ctx.plan.channel)?;
    Ok(fresh)
}

/// Empty the old session's buffered frames before it is discarded. Returns
/// whether the caller should stop pumping (`--max-events` satisfied, Ctrl-C,
/// or the `--duration` deadline).
///
/// A link error here is the old session dying under the drain — its remaining
/// frames are gone, but the replacement has been subscribed since before the
/// drain began, so the same writes are already flowing there and the seam dedup
/// keeps the copies straight. Emit errors still propagate: a broken pipe is
/// about this process's stdout, not the discarded socket.
fn drain<W: Write, L: Link>(link: &mut L, ctx: &Ctx<'_>, stream: &mut Stream<W>) -> Result<bool> {
    let mut quiet = 0usize;
    for _ in 0..DRAIN_POLLS {
        // The drain must stay as interruptible as the pump: a Ctrl-C that
        // waited out a burst of buffered frames would read as a hang.
        if ctx.stop.load(Ordering::SeqCst) || past(ctx.deadline) {
            return Ok(true);
        }
        let events = match link.poll(DRAIN_WAIT) {
            Ok(events) => events,
            Err(_) => break,
        };
        if events.is_empty() {
            // Ambiguous: a timeout, or a meta-only batch (see
            // [`DRAIN_QUIET_POLLS`]). Only a run of them means dry.
            quiet += 1;
            if quiet >= DRAIN_QUIET_POLLS {
                break;
            }
            continue;
        }
        quiet = 0;
        if deliver(events, ctx, stream)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Deliver one batch to the caller. Returns whether `--max-events` was
/// satisfied.
///
/// The order here is the invariant, and it lives in this function rather than
/// at its call sites: the filter first, so a discarded event costs nothing (no
/// `--max-events` spend, no idle-clock reset — otherwise `--operation insert
/// --idle-timeout 30` would be held open indefinitely by updates the caller
/// said it did not want); the seam check second, so a rotation's duplicate
/// costs nothing either.
fn deliver<W: Write>(events: Vec<Event>, ctx: &Ctx<'_>, stream: &mut Stream<W>) -> Result<bool> {
    for event in events {
        if !ctx.plan.filter.accepts(&event.data) {
            continue;
        }
        if stream.duplicate(&event.data) {
            continue;
        }
        if stream.event(&event.data)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn pump<W: Write, L: Link>(
    link: &mut L,
    ctx: &Ctx<'_>,
    stream: &mut Stream<W>,
    rotate_at: Option<Instant>,
) -> Result<PumpEnd> {
    loop {
        if ctx.stop.load(Ordering::SeqCst) || past(ctx.deadline) {
            return Ok(PumpEnd::Stopped);
        }
        if stream.idle_expired(ctx.idle) {
            return Ok(PumpEnd::Stopped);
        }
        if past(rotate_at) {
            return Ok(PumpEnd::Rotate);
        }

        // An empty batch means the poll simply expired — that is the tick that
        // lets the checks above run.
        if deliver(link.poll(TICK)?, ctx, stream)? {
            return Ok(PumpEnd::Stopped);
        }
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
fn sleep_interruptibly(total: Duration, stop: &AtomicBool, deadline: Option<Instant>) -> bool {
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

    /// An event with the identity fields [`dedup_key`] reads.
    fn keyed(op: &str, sys_id: &str, mod_count: u32) -> Value {
        json!({"operation": op, "sys_id": sys_id, "changes": ["state"],
        "record": {"sys_mod_count": {
            "display_value": mod_count.to_string(),
            "value": mod_count.to_string(),
        }}})
    }

    #[test]
    fn dedup_distinguishes_writes_by_mod_count_and_operation() {
        let mut s = buffered(None);
        s.open_seam();
        assert!(!s.duplicate(&keyed("update", "a1", 7)), "first sighting");
        assert!(s.duplicate(&keyed("update", "a1", 7)), "same write again");
        assert!(!s.duplicate(&keyed("update", "a1", 8)), "a real re-write");
        assert!(!s.duplicate(&keyed("delete", "a1", 8)), "another operation");
        assert!(
            s.duplicate(&keyed("delete", "a1", 8)),
            "one delete per record"
        );
    }

    #[test]
    fn dedup_is_inert_outside_a_rotation_seam() {
        // On the normal path nothing can arrive twice, so the dedup must not
        // exist there — a false positive would eat a real event.
        let mut s = buffered(None);
        assert!(!s.duplicate(&keyed("update", "a1", 7)));
        assert!(!s.duplicate(&keyed("update", "a1", 7)));
    }

    #[test]
    fn an_event_without_a_sys_id_is_never_a_duplicate() {
        // Dropping an unidentifiable event is worse than repeating one.
        let mut s = buffered(None);
        s.open_seam();
        assert!(!s.duplicate(&update()));
        assert!(!s.duplicate(&update()));
    }

    #[test]
    fn a_write_without_a_mod_count_is_never_a_duplicate() {
        // Tables without sys_mod_count (syslog, measured) send events that
        // cannot tell one write from the next. Two genuine writes inside one
        // seam must both come through.
        let bare = json!({"operation": "update", "sys_id": "a1",
                          "changes": ["message"], "record": {}});
        let mut s = buffered(None);
        s.open_seam();
        assert!(!s.duplicate(&bare));
        assert!(!s.duplicate(&bare));
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
        // A malformed or foreign payload carries neither key; a filter must not
        // silently treat "field absent" as "matches".
        let bare = json!({"unrelated": 7});
        assert!(
            !Filter {
                operations: vec![Operation::Update],
                ..Default::default()
            }
            .accepts(&bare)
        );
        assert!(
            !Filter {
                on_change: vec!["state".into()],
                ..Default::default()
            }
            .accepts(&bare)
        );
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

    // (One marker per gap, and the interval it spans, are asserted end-to-end
    // over the supervisor further down — that is where the bookkeeping lives.)

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
        // Deliberately lopsided thresholds. A wall-clock assertion taken right
        // after the call it is about is a coin flip on a loaded CI runner, so
        // each direction gets a wide margin instead — half a second of staleness
        // against a 250ms timeout, an hour against a fresh event — and the
        // staleness is set exactly rather than slept for.
        const STALE: Option<Duration> = Some(Duration::from_millis(250));
        const FRESH: Option<Duration> = Some(Duration::from_secs(3600));
        let mut s = buffered(None);
        s.last_event -= Duration::from_millis(500);
        assert!(s.idle_expired(STALE), "nothing has arrived");

        s.meta(&gap_marker(Duration::from_secs(4), 1)).unwrap();
        assert!(
            s.idle_expired(STALE),
            "a reconnect notice is not an arrival: --idle-timeout must still fire"
        );

        s.event(&update()).unwrap();
        assert!(!s.idle_expired(FRESH), "a real event does reset it");
    }

    #[test]
    fn an_outage_is_forgiven_rather_than_counted_as_idle() {
        // Time with no channel open is time nothing *could* have arrived on, so
        // it is not idleness. The staleness is set exactly rather than slept for,
        // but the second reading is still against a real clock — forgiving all of
        // it parks `last_event` at the `buffered` call, so the timeout has to
        // outlast this test's own scheduling. Seconds against microseconds of
        // work; nothing here sleeps, so the size is free.
        let idle = Some(Duration::from_secs(1));
        let mut s = buffered(None);
        s.last_event -= Duration::from_secs(3);
        assert!(s.idle_expired(idle), "3s with nothing delivered");

        s.resume(Duration::from_secs(3));
        assert!(
            !s.idle_expired(idle),
            "…but all of it was spent off the socket"
        );
    }

    #[test]
    fn forgiving_more_than_elapsed_parks_the_clock_at_now_not_in_the_future() {
        // `resume` is only ever handed a measured interval, so this cannot
        // happen today. It is clamped anyway because the failure is silent and
        // permanent: `Instant::elapsed` saturates at zero, so a clock in the
        // future would make --idle-timeout never fire again.
        let mut s = buffered(None);
        s.resume(Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(30));
        assert!(s.idle_expired(Some(Duration::from_millis(5))));
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
        // Ordering only: this proves the writer preserves the order it is called
        // in, not that anything calls it that way. That the *supervisor* puts a
        // marker between a pre-drop and a post-drop event is asserted below,
        // over a scripted link.
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

    // ── the supervisor, over a scripted link ────────────────────────────────
    //
    // What the marker claims is about a *sequence* of sessions: it lands between
    // the last event a dropped session delivered and the first its replacement
    // does. Nothing below `supervise` can check that, and a real drop needs a
    // Bayeux server, so these drive the loop over a `Link` that is scripted
    // rather than connected.

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    const CHANNEL: &str = "/rw/default/incident/YWN0aXZlPXRydWU-";

    /// One `poll` worth of scripted behavior.
    enum Step {
        /// Deliver these payloads.
        Deliver(Vec<Value>),
        /// Nothing arrives for this long — the poll expiring, which is what
        /// lets the deadline and idle checks run.
        Quiet(Duration),
        /// The socket dies. Transport, i.e. the recoverable kind.
        Drop,
    }

    /// One session's worth of scripted connect.
    enum Attempt {
        /// The socket never opens (a failed session mint or upgrade).
        ConnectFails,
        /// The socket opens and subscribes, then plays these steps.
        Opens(Vec<Step>),
    }

    /// What the supervisor did to the links it was handed.
    #[derive(Default)]
    struct Log {
        subscribes: Vec<String>,
        disconnects: usize,
    }

    struct FakeLink {
        steps: VecDeque<Step>,
        log: Rc<RefCell<Log>>,
    }

    impl Link for FakeLink {
        fn subscribe(&mut self, channel: &str) -> Result<()> {
            self.log.borrow_mut().subscribes.push(channel.to_string());
            Ok(())
        }

        fn poll(&mut self, _wait: Duration) -> Result<Vec<Event>> {
            match self.steps.pop_front() {
                Some(Step::Deliver(payloads)) => Ok(payloads
                    .into_iter()
                    .map(|data| Event {
                        channel: CHANNEL.to_string(),
                        data,
                    })
                    .collect()),
                Some(Step::Quiet(d)) => {
                    std::thread::sleep(d);
                    Ok(Vec::new())
                }
                // Running out is a script that never said how the session ends.
                // Dying is the honest reading and keeps the loop finite.
                Some(Step::Drop) | None => Err(Error::Transport("amb websocket closed".into())),
            }
        }

        fn disconnect(&mut self) {
            self.log.borrow_mut().disconnects += 1;
        }
    }

    /// A supervisor run to drive. `--max-events` is not here: it belongs to the
    /// `Stream` the caller hands in, which is also where the bytes end up.
    #[derive(Default)]
    struct Script {
        attempts: Vec<Attempt>,
        idle: Option<Duration>,
        /// What each backoff wait costs in wall clock. Zero unless a test needs
        /// an outage longer than its own `--idle-timeout`; nothing else pays.
        pause: Duration,
        /// How long the first connect takes — a slow session mint.
        connect_delay: Duration,
        /// Preemptive rotation interval, as `Plan.rotate`.
        rotate: Option<Duration>,
    }

    /// Drive `supervise` over the script, writing into `stream`.
    fn play<W: Write>(script: Script, stream: &mut Stream<W>) -> (Result<()>, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        let plan = Plan {
            channel: CHANNEL.to_string(),
            filter: Filter::default(),
            limits: WatchLimits::default(),
            rotate: script.rotate,
        };
        let stop = AtomicBool::new(false);
        let ctx = Ctx {
            plan: &plan,
            stop: &stop,
            deadline: None,
            idle: script.idle,
        };

        let mut attempts: VecDeque<Attempt> = script.attempts.into();
        let connect_delay = script.connect_delay;
        let mut first = true;
        let links = Rc::clone(&log);
        let mut connect = move || -> Result<FakeLink> {
            if first {
                std::thread::sleep(connect_delay);
                first = false;
            }
            match attempts.pop_front() {
                Some(Attempt::Opens(steps)) => Ok(FakeLink {
                    steps: steps.into(),
                    log: Rc::clone(&links),
                }),
                Some(Attempt::ConnectFails) => {
                    Err(Error::Transport("amb websocket upgrade: refused".into()))
                }
                None => Err(Error::Transport("scripted link exhausted".into())),
            }
        };
        let pause = script.pause;
        let mut wait = |_: Duration| {
            std::thread::sleep(pause);
            true
        };

        let result = supervise(&ctx, &mut connect, stream, &mut wait);
        (result, log)
    }

    #[test]
    fn the_marker_lands_between_the_last_pre_drop_event_and_the_first_after() {
        // The acceptance criterion, asserted on the bytes: a stream that
        // survived a drop says where the hole is, in place.
        let mut s = buffered(Some(2));
        let (result, log) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![Step::Deliver(vec![insert()]), Step::Drop]),
                    Attempt::Opens(vec![Step::Deliver(vec![update()])]),
                ],
                ..Default::default()
            },
            &mut s,
        );
        assert!(
            result.is_ok(),
            "a survivable drop is not a failure: {result:?}"
        );

        let out = lines(&s);
        assert_eq!(out.len(), 3, "event, marker, event: {out:?}");
        assert_eq!(out[0]["operation"], "insert");
        assert_eq!(out[1]["sn_watch"], "reconnected", "the hole is announced");
        assert_eq!(out[1]["attempt"], 1);
        assert_eq!(out[2]["operation"], "update");
        assert_eq!(
            log.borrow().subscribes.len(),
            2,
            "the replacement session resubscribed"
        );
    }

    #[test]
    fn rotation_swaps_sessions_without_a_marker() {
        // Make-before-break: the replacement subscribes, the old session is
        // drained and dropped, and the stream carries no marker — nothing was
        // lost, so there is no hole to announce.
        let mut s = buffered(Some(2));
        let (result, log) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![
                        Step::Deliver(vec![insert()]),
                        Step::Quiet(Duration::from_millis(30)),
                    ]),
                    Attempt::Opens(vec![Step::Deliver(vec![update()])]),
                ],
                rotate: Some(Duration::from_millis(20)),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        let out = lines(&s);
        assert_eq!(out.len(), 2, "two events, no marker: {out:?}");
        assert_eq!(out[0]["operation"], "insert");
        assert_eq!(out[1]["operation"], "update");
        assert_eq!(log.borrow().subscribes.len(), 2, "replacement subscribed");
        assert_eq!(
            log.borrow().disconnects,
            2,
            "the old session at rotation, the new one at exit"
        );
    }

    #[test]
    fn the_rotation_seam_emits_each_write_once() {
        // During the overlap one write can arrive on both the old session (in
        // the drain) and the replacement. Exactly one copy reaches the caller,
        // and a genuine later write still gets through.
        let mut s = buffered(Some(2));
        let (result, _) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![
                        Step::Quiet(Duration::from_millis(30)),
                        Step::Deliver(vec![keyed("update", "a1", 7)]),
                    ]),
                    Attempt::Opens(vec![Step::Deliver(vec![
                        keyed("update", "a1", 7),
                        keyed("update", "a1", 8),
                    ])]),
                ],
                rotate: Some(Duration::from_millis(20)),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        let out = lines(&s);
        assert_eq!(out.len(), 2, "the seam duplicate is dropped: {out:?}");
        assert_eq!(out[0]["record"]["sys_mod_count"]["value"], "7");
        assert_eq!(out[1]["record"]["sys_mod_count"]["value"], "8");
    }

    #[test]
    fn a_failed_rotation_keeps_the_current_session() {
        // The current session is healthy; a replacement that cannot be built is
        // retry-later, not an error and not a teardown.
        let mut s = buffered(Some(1));
        let (result, log) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![
                        Step::Quiet(Duration::from_millis(30)),
                        Step::Deliver(vec![insert()]),
                    ]),
                    Attempt::ConnectFails,
                ],
                rotate: Some(Duration::from_millis(20)),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        let out = lines(&s);
        assert_eq!(out.len(), 1, "the old session kept delivering: {out:?}");
        assert_eq!(out[0]["operation"], "insert");
        assert_eq!(log.borrow().disconnects, 1, "only the session that lived");
    }

    #[test]
    fn a_clean_run_says_nothing_about_gaps() {
        // The marker is evidence of loss; on a stream that never lost anything
        // it would be a lie, and a consumer reconciling against the table would
        // do it for nothing.
        let mut s = buffered(Some(1));
        let (result, _) = play(
            Script {
                attempts: vec![Attempt::Opens(vec![Step::Deliver(vec![insert()])])],
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        let out = lines(&s);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].get("sn_watch").is_none(), "{out:?}");
    }

    #[test]
    fn one_marker_per_gap_however_many_attempts_it_takes_to_close() {
        // Attempts 1 and 2 die before subscribing, so they announce nothing —
        // and the downtime attempt 3 reports has to span the whole outage, not
        // just the slice after the last failure.
        let pause = Duration::from_millis(40);
        let mut s = buffered(Some(2));
        let (result, _) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![Step::Deliver(vec![insert()]), Step::Drop]),
                    Attempt::ConnectFails,
                    Attempt::ConnectFails,
                    Attempt::Opens(vec![Step::Deliver(vec![update()])]),
                ],
                pause,
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");

        let out = lines(&s);
        let markers: Vec<&Value> = out.iter().filter(|v| v.get("sn_watch").is_some()).collect();
        assert_eq!(markers.len(), 1, "one hole, one marker: {out:?}");
        assert_eq!(
            markers[0]["attempt"], 3,
            "reported as closed by the try that worked"
        );
        // Three waits of `pause` separate the drop from the resubscribe; timing
        // out the last one alone would report a third of the hole.
        let reported = markers[0]["downtime_ms"].as_u64().expect("a number");
        let whole = 3 * u64::try_from(pause.as_millis()).unwrap();
        assert!(
            reported >= whole * 3 / 4,
            "downtime must span the outage, not its last slice: {reported}ms of ~{whole}ms"
        );
    }

    #[test]
    fn a_broken_pipe_on_the_marker_still_closes_the_subscription() {
        // `sn watch … | head -1`: stdout goes away while the marker is being
        // written. Every other post-subscribe exit sends /meta/disconnect, and
        // this one must too — otherwise the instance keeps pushing events into a
        // subscription nobody will ever read.
        let mut s = Stream::new(ClosesAfter { lines: 1 }, Some(3));
        let (result, log) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![Step::Deliver(vec![insert()]), Step::Drop]),
                    Attempt::Opens(vec![Step::Deliver(vec![update()])]),
                ],
                ..Default::default()
            },
            &mut s,
        );
        assert!(
            matches!(result, Err(Error::BrokenPipe)),
            "a closed stdout is a silent exit, not a crash: {result:?}"
        );
        assert_eq!(
            log.borrow().disconnects,
            2,
            "the session that could not announce its gap is still closed"
        );
    }

    /// A stdout that goes away after `lines` complete lines — `… | head -N`.
    /// Counted on flushes, because one line is several `write` calls.
    struct ClosesAfter {
        lines: usize,
    }

    impl Write for ClosesAfter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.lines == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.lines == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"));
            }
            self.lines -= 1;
            Ok(())
        }
    }

    #[test]
    fn a_slow_first_connect_is_not_idleness() {
        // Nothing can arrive before there is a channel to arrive on. Counting
        // the session mint and handshake against --idle-timeout made a watcher
        // on a slow instance subscribe and then immediately exit 0 with an empty
        // stream — indistinguishable from a table where nothing happened.
        //
        // Two constraints. The connect must outlast the timeout or the test
        // proves nothing — safe in itself, since a sleep only ever overruns. The
        // fragile one is the timeout: `resume` parks the idle clock at the
        // subscribe, so the whole of it is the scheduling stall tolerated before
        // the check preceding the first poll fires and empties the stream.
        let mut s = buffered(Some(1));
        let (result, _) = play(
            Script {
                attempts: vec![Attempt::Opens(vec![Step::Deliver(vec![insert()])])],
                idle: Some(Duration::from_millis(250)),
                connect_delay: Duration::from_millis(400),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            lines(&s).len(),
            1,
            "the event it was subscribed for, not an empty stream"
        );
    }

    #[test]
    fn an_outage_does_not_make_the_marker_the_last_line() {
        // The idle clock used to run through the outage, so any --idle-timeout
        // shorter than the backoff (guaranteed once backoff caps at 60s) turned
        // "we reconnected" into "…and then gave up", never covering the hole it
        // had just announced.
        //
        // Same two constraints as the slow first connect, with the backoff wait
        // standing in for the connect: the outage must outlast the timeout, and
        // the timeout is what the replacement session then has to reach its
        // event in — 250ms of scheduling stall before the marker is left as the
        // last line, which is the failure this test exists to catch.
        let mut s = buffered(Some(2));
        let (result, _) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![Step::Deliver(vec![insert()]), Step::Drop]),
                    Attempt::Opens(vec![Step::Deliver(vec![update()])]),
                ],
                idle: Some(Duration::from_millis(250)),
                pause: Duration::from_millis(400),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        let out = lines(&s);
        assert_eq!(
            out.len(),
            3,
            "a marker must not be the last thing a watcher says: {out:?}"
        );
        assert_eq!(out[2]["operation"], "update");
    }

    #[test]
    fn silence_accumulates_across_sessions_so_flapping_cannot_hold_a_watcher_open() {
        // Forgiving an outage must be weaker than restarting the clock: a socket
        // that drops faster than --idle-timeout would otherwise keep a watcher
        // that has delivered nothing alive forever. The property is a sandwich,
        // Q < T < 2Q: one session's quiet (Q=200ms) must not expire the timeout
        // (T=300ms), while quiet either side of a drop (400ms) must — so the
        // second session expires on its first quiet poll and never reaches the
        // event scripted behind it.
        //
        // The magnitudes buy nothing but margin, and the margin is what the test
        // is: T-Q is the scheduling stall it tolerates in either direction. At
        // 20/30 that was 10ms, and a loaded CI runner expired the first session
        // before it could drop, leaving one subscribe instead of two; a loaded
        // macOS runner then blew through 200/300's 100ms the same way. Scale
        // both together or the property goes with them.
        let mut s = buffered(None);
        let (result, log) = play(
            Script {
                attempts: vec![
                    Attempt::Opens(vec![Step::Quiet(Duration::from_millis(400)), Step::Drop]),
                    Attempt::Opens(vec![
                        Step::Quiet(Duration::from_millis(400)),
                        Step::Deliver(vec![update()]),
                    ]),
                ],
                idle: Some(Duration::from_millis(600)),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(log.borrow().subscribes.len(), 2, "it did reconnect");
        let out = lines(&s);
        assert_eq!(out.len(), 1, "the marker, and nothing after it: {out:?}");
        assert_eq!(out[0]["sn_watch"], "reconnected");
    }

    #[test]
    fn silence_while_subscribed_still_expires_the_idle_clock() {
        // Quiet is twice the timeout so the expiry is the poll's silence and not
        // the harness's own overhead: a stall large enough to expire the clock
        // *before* the first poll would leave the same empty stream and assert
        // nothing. 100ms of headroom before that reading becomes possible.
        let mut s = buffered(None);
        let (result, _) = play(
            Script {
                attempts: vec![Attempt::Opens(vec![
                    Step::Quiet(Duration::from_millis(200)),
                    Step::Deliver(vec![update()]),
                ])],
                idle: Some(Duration::from_millis(100)),
                ..Default::default()
            },
            &mut s,
        );
        assert!(result.is_ok(), "an idle stop is a clean exit: {result:?}");
        assert!(lines(&s).is_empty(), "nothing arrived in time");
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
        // So will a table that does not exist — the subscribe refusal carries
        // the NO_HTTP_STATUS sentinel (in-band failure, no HTTP error), and the
        // sentinel must not make it retryable.
        assert!(!is_recoverable(&Error::Api {
            status: crate::error::NO_HTTP_STATUS,
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
            api_key: None,
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
}
