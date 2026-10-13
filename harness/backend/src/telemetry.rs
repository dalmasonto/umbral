//! Per-request, per-layer telemetry — the harness's measuring instrument.
//!
//! # What it measures and through which seam
//!
//! umbral's request path nests like this (outer → inner, from `App::build`):
//!
//! ```text
//! core edge layers (trace span, hardening headers, body limit/timeout,
//!     host guard, CORS, compression, panic/render-500, route-context, slash)
//!   └─ typed MiddlewareStack (feature #68)          ← app middlewares first
//!        └─ plugin `wrap_router` layers (topological order, later = outer)
//!             └─ router → extractors → handler
//! ```
//!
//! Three seams give us the decomposition without touching the framework:
//!
//! 1. **App middlewares** (`AppBuilder::middleware`) — the harness registers
//!    `TraceRoot` (created here), `DevAuth`, `RequestLog`, `ResponseStamp`.
//!    Each self-times its own before/after work, so every app middleware
//!    shows up individually by `Middleware::name()`.
//! 2. **Marker plugins** (see `src/probes.rs`) — thin `wrap_router` layers
//!    registered *between* real plugins via `Plugin::dependencies()`, so the
//!    topo sort nests them exactly around `umbral-security` and
//!    `umbral-sessions`. Timestamp deltas between markers are those plugins'
//!    own before/after cost (`security`, `sessions` rows).
//! 3. **A tracing layer on `sqlx::query` events** — one event per executed
//!    statement (same seam `umbral-core/tests/query_counts.rs` counts on).
//!    Events are associated to the request via a span created in the
//!    outermost marker layer, so `database`, statement fingerprints, and the
//!    N+1 detector all come from the real SQL that ran.
//!
//! Everything reconciles: the stacked buckets sum to the measured wall time
//! (residue lands in `tail`), and the middleware rows sum to the `middleware`
//! bucket. A tool that lies about where time went is worse than no tool.
//!
//! # Where the numbers go
//!
//! - `Server-Timing` response header (visible in DevTools, cheap for clients
//!   to aggregate — the load generator reads them per response).
//! - `traces/requests.jsonl`, written by a dedicated writer thread off the
//!   request path (unbounded channel send; the request never blocks on I/O).
//! - In-memory rolling aggregates served at `GET /api/bench/telemetry`.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use axum::http::HeaderValue;
use serde::Serialize;
use tracing_subscriber::filter::{LevelFilter, filter_fn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, registry::LookupSpan};

use umbral::middleware::Middleware;
use umbral::web::{Request, Response};

/// Same-request repetition of one statement fingerprint above this count is
/// flagged as a probable N+1 (the detector's v1 heuristic, mirroring the
/// test-side statement counter).
pub const N1_THRESHOLD: usize = 25;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

// ===========================================================================
// Per-request trace state
// ===========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct SqlEvent {
    pub fingerprint: String,
    pub summary: String,
    pub elapsed_us: u64,
}

/// One middleware's own time, measured by the middleware itself.
#[derive(Debug, Clone, Serialize)]
pub struct MwRow {
    pub name: String,
    pub before_us: u64,
    pub after_us: u64,
}

/// The request-scoped record every seam writes into. Lives behind the
/// request's `extensions` (as `Trace`, a cheap handle) and in the active
/// registry (keyed by id) so the SQL layer can reach it via the request span.
pub struct TraceInner {
    pub id: u64,
    pub t0: Instant,
    pub method: String,
    pub path: String,
    pub route: String,
    /// The in-process run this request belongs to (`x-bench-run` header) —
    /// drives the per-run layer snapshot for the Layers view.
    pub run_id: Option<u64>,
    pub status: u16,
    pub mw: Vec<MwRow>,
    /// Marker-plugin timestamps, ms since `t0`, keyed `"<probe>:in|out"`.
    pub stamps: HashMap<String, f64>,
    pub sql: Vec<SqlEvent>,
    pub handler_start_ms: f64,
    pub handler_end_ms: f64,
    pub handler_db_wall_us: u64,
    pub handler_serialize_us: u64,
    pub handler_render_us: u64,
}

#[derive(Clone)]
pub struct Trace(pub Arc<Mutex<TraceInner>>);

impl Trace {
    fn new(id: u64, method: String, path: String, run_id: Option<u64>) -> Self {
        let route = route_key(&method, &path);
        Trace(Arc::new(Mutex::new(TraceInner {
            id,
            t0: Instant::now(),
            method,
            path,
            route,
            run_id,
            status: 200,
            mw: Vec::new(),
            stamps: HashMap::new(),
            sql: Vec::new(),
            handler_start_ms: 0.0,
            handler_end_ms: 0.0,
            handler_db_wall_us: 0,
            handler_serialize_us: 0,
            handler_render_us: 0,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, TraceInner> {
        // A poisoned lock must not take the telemetry down with it — the
        // trace data is diagnostic, never correctness-bearing.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn id(&self) -> u64 {
        self.lock().id
    }

    pub fn ms_now(&self) -> f64 {
        self.lock().t0.elapsed().as_secs_f64() * 1000.0
    }

    /// Record a marker timestamp (`"probe:inner:in"`).
    pub fn stamp(&self, key: &str) {
        let ms = self.ms_now();
        self.lock().stamps.insert(key.to_string(), ms);
    }

    fn mw_row(&self, name: &str) -> usize {
        let mut t = self.lock();
        if let Some(i) = t.mw.iter().position(|r| r.name == name) {
            return i;
        }
        t.mw.push(MwRow {
            name: name.to_string(),
            before_us: 0,
            after_us: 0,
        });
        t.mw.len() - 1
    }

    pub fn mw_before(&self, name: &str, us: u64) {
        let i = self.mw_row(name);
        self.lock().mw[i].before_us = us;
    }

    pub fn mw_after(&self, name: &str, us: u64) {
        let i = self.mw_row(name);
        self.lock().mw[i].after_us = us;
    }

    pub fn push_sql(&self, fingerprint: String, summary: String, elapsed_us: u64) {
        self.lock().sql.push(SqlEvent {
            fingerprint,
            summary,
            elapsed_us,
        });
    }

    pub fn handler_start(&self) {
        let ms = self.ms_now();
        let mut t = self.lock();
        if t.handler_start_ms == 0.0 {
            t.handler_start_ms = ms;
        }
    }

    pub fn handler_end(&self) {
        let ms = self.ms_now();
        let mut t = self.lock();
        if t.handler_end_ms == 0.0 {
            t.handler_end_ms = ms;
        }
    }

    /// Wall time around one or more ORM awaits in the handler. The `database`
    /// bucket comes from the sqlx layer; this is the outer wall the handler
    /// actually paid (build + pool checkout + execute + decode).
    pub fn add_db_wall(&self, us: u64) {
        self.lock().handler_db_wall_us += us;
    }

    pub fn add_serialize(&self, us: u64) {
        self.lock().handler_serialize_us += us;
    }

    pub fn add_render(&self, us: u64) {
        self.lock().handler_render_us += us;
    }
}

/// Collapse numeric path segments into `{id}` so aggregates key by route
/// shape, not by concrete id: `/api/users/42/profile` → `/api/users/{id}/profile`.
pub fn route_key(method: &str, path: &str) -> String {
    let collapsed: Vec<&str> = path
        .split('/')
        .map(|seg| {
            if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
                "{id}"
            } else {
                seg
            }
        })
        .collect();
    format!("{} {}", method, collapsed.join("/"))
}

// ===========================================================================
// Active trace registry (reached by the SQL layer via the request span)
// ===========================================================================

fn active_map() -> &'static Mutex<HashMap<u64, Trace>> {
    static ACTIVE: OnceLock<Mutex<HashMap<u64, Trace>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_active(t: &Trace) {
    active_map().lock().unwrap().insert(t.id(), t.clone());
}

fn unregister_active(id: u64) {
    active_map().lock().unwrap().remove(&id);
}

fn active(id: u64) -> Option<Trace> {
    active_map().lock().unwrap().get(&id).cloned()
}

fn span_map() -> &'static Mutex<HashMap<u64, u64>> {
    static SPAN_TRACE: OnceLock<Mutex<HashMap<u64, u64>>> = OnceLock::new();
    SPAN_TRACE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register `tracing` span id → trace id, so the SQL layer can walk from an
/// event's span up to the request trace. Called by the outermost marker.
pub fn register_span(span_id: u64, trace_id: u64) {
    span_map().lock().unwrap().insert(span_id, trace_id);
}

pub fn unregister_span(span_id: u64) {
    span_map().lock().unwrap().remove(&span_id);
}

// ===========================================================================
// The tracing layer: one sqlx::query event per statement
// ===========================================================================

#[derive(Default)]
struct SqlVisitor {
    summary: Option<String>,
    elapsed_debug: Option<String>,
}

impl tracing::field::Visit for SqlVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "summary" | "db.statement" => self.summary = Some(format!("{value:?}")),
            "elapsed" => self.elapsed_debug = Some(format!("{value:?}")),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if matches!(field.name(), "summary" | "db.statement") {
            self.summary = Some(value.to_string());
        }
    }
}

/// Parse Rust's `Duration` Debug form (`"1.234ms"`, `"812.5µs"`, `"12.3s"`)
/// into microseconds.
fn parse_duration_debug(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, factor) = if let Some(n) = s.strip_suffix("ns") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix("µs") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix("ms") {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1_000_000.0)
    } else {
        return None;
    };
    let v: f64 = num.parse().ok()?;
    Some((v * factor) as u64)
}

/// Statement fingerprint: lowercase, collapsed whitespace, digits → `?`.
pub fn fingerprint(stmt: &str) -> String {
    let mut out = String::with_capacity(stmt.len().min(200));
    let mut prev_space = false;
    let mut prev_digit = false;
    for ch in stmt.chars() {
        if ch.is_ascii_digit() {
            if !prev_digit {
                out.push('?');
            }
            prev_digit = true;
            prev_space = false;
        } else if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
            prev_digit = false;
        } else {
            out.extend(ch.to_lowercase());
            prev_space = false;
            prev_digit = false;
        }
    }
    let out = out.trim_end().to_string();
    if out.len() > 200 { out[..200].to_string() } else { out }
}

pub struct BenchLayer;

impl<S> Layer<S> for BenchLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !event.metadata().target().starts_with("sqlx::query") {
            return;
        }
        let mut v = SqlVisitor::default();
        event.record(&mut v);
        let stmt = v.summary.unwrap_or_default();
        // Connection-bootstrap PRAGMAs are sqlx internals, not app DML —
        // same exclusion the test-side counter documents.
        if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
            return;
        }
        let elapsed_us = v
            .elapsed_debug
            .as_deref()
            .and_then(parse_duration_debug)
            .unwrap_or(0);
        let fp = fingerprint(&stmt);

        // GLOBAL accounting first — this works for every driver. SQLite's
        // sqlx driver executes on a thread that carries no request span
        // context, so per-request attribution below simply yields nothing
        // there; the fingerprint aggregates and the burst detector still see
        // every statement.
        global_sql_record(&fp, &stmt, elapsed_us);

        // Per-request attribution when the driver emits within the request's
        // instrumented future (Postgres does). On SQLite this is a no-op.
        if let Some(trace) = span_trace_id(&ctx, event).and_then(active) {
            trace.push_sql(fp, stmt, elapsed_us);
        }
    }
}

fn span_trace_id<S>(ctx: &tracing_subscriber::layer::Context<'_, S>, event: &tracing::Event<'_>) -> Option<u64>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    // Prefer the event's explicit/contextual span; fall back to the span
    // current on this thread at emission time (`event_span` only resolves an
    // explicit `parent:`, while `tracing::event!` usually attaches the
    // current span as the event's context).
    let span = ctx
        .event_span(event)
        .or_else(|| ctx.current_span().id().and_then(|id| ctx.span(&id)))?;
    let map = span_map().lock().ok()?;
    let mut cur = Some(span);
    while let Some(s) = cur {
        if let Some(tid) = map.get(&s.id().into_u64()) {
            return Some(*tid);
        }
        cur = s.parent();
    }
    None
}

// ===========================================================================
// Global SQL accounting + burst N+1 heuristic
// ===========================================================================

struct SqlStat {
    calls: u64,
    sum_us: f64,
    max_us: f64,
    sample: String,
}

#[derive(Default)]
struct GlobalSql {
    stats: HashMap<String, SqlStat>,
    /// Trailing emission times per fingerprint, for the burst heuristic.
    recent: HashMap<String, VecDeque<Instant>>,
    n1: Vec<N1Flag>,
}

fn global_sql() -> &'static Mutex<GlobalSql> {
    static G: OnceLock<Mutex<GlobalSql>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(GlobalSql::default()))
}

/// Same fingerprint fired this many times inside [`BURST_WINDOW`] → flag as a
/// probable N+1 loop. Window-based because SQLite statement events carry no
/// request context (see the SQL layer docs); Postgres requests additionally
/// get the exact per-request detector.
const BURST_WINDOW: Duration = Duration::from_millis(250);

fn global_sql_record(fp: &str, stmt: &str, elapsed_us: u64) {
    let mut g = global_sql().lock().unwrap_or_else(|e| e.into_inner());

    let stat = g.stats.entry(fp.to_string()).or_insert_with(|| SqlStat {
        calls: 0,
        sum_us: 0.0,
        max_us: 0.0,
        sample: stmt.chars().take(160).collect(),
    });
    stat.calls += 1;
    stat.sum_us += elapsed_us as f64;
    stat.max_us = stat.max_us.max(elapsed_us as f64);

    let now = Instant::now();
    let burst_len = {
        let times = g.recent.entry(fp.to_string()).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) > BURST_WINDOW)
        {
            times.pop_front();
        }
        times.push_back(now);
        if times.len() > 128 {
            times.pop_front();
        }
        times.len()
    };
    if burst_len >= N1_THRESHOLD && !g.n1.iter().any(|f| f.fingerprint == fp) {
        g.n1.push(N1Flag {
            route: format!("burst (<{}ms window)", BURST_WINDOW.as_millis()),
            fingerprint: fp.to_string(),
            times: burst_len,
            sample: stmt.chars().take(160).collect(),
        });
    }
}

// ===========================================================================
// JSONL writer (off the request path)
// ===========================================================================

static JSONL_TX: OnceLock<std::sync::mpsc::Sender<(String, String)>> = OnceLock::new();

/// Hand one JSON line to the writer thread. Never blocks the request.
pub fn emit_jsonl(kind: &str, line: String) {
    if let Some(tx) = JSONL_TX.get() {
        let _ = tx.send((kind.to_string(), line));
    }
}

fn spawn_writer() {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let _ = JSONL_TX.set(tx);
    let dir = std::env::var("UMBRAL_BENCH_TRACES_DIR").unwrap_or_else(|_| "traces".to_string());
    std::thread::Builder::new()
        .name("bench-jsonl".into())
        .spawn(move || {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("[bench] cannot create trace dir {dir}: {e}; traces go nowhere");
            }
            let mut files: HashMap<String, std::io::BufWriter<std::fs::File>> = HashMap::new();
            let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok((kind, line)) => {
                        if failed.contains(&kind) {
                            continue;
                        }
                        let f = files.entry(kind.clone()).or_insert_with(|| {
                            let path = format!("{dir}/{kind}.jsonl");
                            match std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)
                            {
                                Ok(file) => std::io::BufWriter::new(file),
                                Err(e) => {
                                    eprintln!("[bench] cannot open {path}: {e}");
                                    failed.insert(kind.clone());
                                    // A writer we immediately forget — keeps the
                                    // entry-shaped code path without panicking.
                                    std::io::BufWriter::new(
                                        std::fs::OpenOptions::new()
                                            .create(true)
                                            .append(true)
                                            .open("/dev/null")
                                            .expect("opening /dev/null"),
                                    )
                                }
                            }
                        });
                        if !failed.contains(&kind) {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        for f in files.values_mut() {
                            let _ = f.flush();
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        for f in files.values_mut() {
                            let _ = f.flush();
                        }
                        break;
                    }
                }
            }
        })
        .expect("spawn bench-jsonl writer thread");
}

/// Install the harness's tracing stack: our sqlx-capturing layer (unfiltered,
/// it checks the target itself), the fmt layer (RUST_LOG-driven), and the
/// JSONL writer thread.
pub fn init_subscriber() {
    let bench = BenchLayer.with_filter(filter_fn(|m: &tracing::Metadata<'_>| {
        m.target().starts_with("sqlx::query")
    }));
    let env = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(LevelFilter::TRACE)
        .with(bench)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(env),
        )
        .init();
    spawn_writer();
}

// ===========================================================================
// Buckets — the decomposition shown in the UI
// ===========================================================================

#[derive(Debug, Clone, Serialize, Default)]
pub struct Buckets {
    /// Sum of the individual middleware rows below.
    pub middleware_us: f64,
    pub security_us: f64,
    pub sessions_us: f64,
    pub trace_wrap_us: f64,
    pub typed_mw_us: f64,
    pub routing_us: f64,
    pub query_build_us: f64,
    pub database_us: f64,
    pub serialize_us: f64,
    pub render_us: f64,
    pub handler_logic_us: f64,
    /// Everything on the way out not captured above (edge layers' after-work,
    /// short-circuit residue).
    pub tail_us: f64,
    pub total_us: f64,
}

impl Buckets {
    fn compute(t: &TraceInner, total_ms: f64) -> Buckets {
        use crate::probes::{PROBE_AFTER_SECURITY, PROBE_AFTER_SESSIONS, PROBE_INNER, PROBE_OUTER};

        // ms-bounds → µs span; entries recorded inner-later. Keys come from
        // the probe constants so a label rename can never silently zero a
        // bucket again (it did once: `probe:inner` vs `probe:sessions-inner`).
        let s = |k: String| t.stamps.get(&k).copied();
        let se = || format!("{PROBE_AFTER_SECURITY}:in");
        let sx = || format!("{PROBE_AFTER_SECURITY}:out");
        let ss_in = || format!("{PROBE_AFTER_SESSIONS}:in");
        let ss_out = || format!("{PROBE_AFTER_SESSIONS}:out");
        let si_in = || format!("{PROBE_INNER}:in");
        let si_out = || format!("{PROBE_INNER}:out");
        let so_in = || format!("{PROBE_OUTER}:in");
        let so_out = || format!("{PROBE_OUTER}:out");
        let span = |a: Option<f64>, b: Option<f64>| -> f64 {
            match (a, b) {
                (Some(a), Some(b)) => ((b - a).max(0.0)) * 1000.0,
                _ => 0.0,
            }
        };

        let security_us = span(s(se()), s(ss_in())) + span(s(ss_out()), s(sx()));
        let sessions_us = span(s(ss_in()), s(si_in())) + span(s(si_out()), s(ss_out()));
        let trace_wrap_us = span(s(so_in()), s(se())) + span(s(sx()), s(so_out()));
        let typed_mw_us: f64 = t
            .mw
            .iter()
            .map(|r| (r.before_us + r.after_us) as f64)
            .sum();
        let middleware_us = typed_mw_us + security_us + sessions_us + trace_wrap_us;

        let routing_us = if t.handler_start_ms > 0.0 {
            span(s(si_in()), Some(t.handler_start_ms))
        } else {
            0.0
        };

        // Two honest modes:
        // - Attributed (Postgres, or any driver emitting within the request's
        //   future): `database` is the sum of sqlx-reported execution times;
        //   the rest of the handler's ORM wall is build + checkout + decode.
        // - Unattributed (SQLite: sqlx executes off the request context):
        //   per-statement times exist in the GLOBAL view but can't belong to
        //   one request, so the per-request `database` figure is the handler's
        //   own wall around ORM awaits and `query_build` stays 0.
        let sqlx_sum: f64 = t.sql.iter().map(|e| e.elapsed_us as f64).sum();
        let (database_us, query_build_us) = if t.sql.is_empty() {
            (t.handler_db_wall_us as f64, 0.0)
        } else {
            (
                sqlx_sum,
                (t.handler_db_wall_us as f64 - sqlx_sum).max(0.0),
            )
        };
        let database_us = database_us.max(0.0);

        let handler_total_us = if t.handler_end_ms > t.handler_start_ms {
            (t.handler_end_ms - t.handler_start_ms) * 1000.0
        } else {
            0.0
        };
        let handler_logic_us = (handler_total_us
            - t.handler_db_wall_us as f64
            - t.handler_serialize_us as f64
            - t.handler_render_us as f64)
            .max(0.0);

        let total_us = total_ms * 1000.0;
        let tail_us =
            (total_us - middleware_us - routing_us - handler_total_us).max(0.0);

        Buckets {
            middleware_us,
            security_us,
            sessions_us,
            trace_wrap_us,
            typed_mw_us,
            routing_us,
            query_build_us,
            database_us,
            serialize_us: t.handler_serialize_us as f64,
            render_us: t.handler_render_us as f64,
            handler_logic_us,
            tail_us,
            total_us,
        }
    }
}

// ===========================================================================
// Aggregates + snapshot for GET /api/bench/telemetry
// ===========================================================================

#[derive(Default)]
struct Stat {
    count: u64,
    sum_us: f64,
    max_us: f64,
}

impl Stat {
    fn add(&mut self, us: f64) {
        self.count += 1;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }
    fn avg(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.sum_us / self.count as f64 }
    }
}

#[derive(Default)]
struct RouteAgg {
    stat: Stat,
    ring_us: VecDeque<f64>,
    errors: u64,
}

#[derive(Default)]
struct Agg {
    routes: HashMap<String, RouteAgg>,
    middleware: HashMap<String, Stat>,
    layers: HashMap<String, Stat>,
    total_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct N1Flag {
    pub route: String,
    pub fingerprint: String,
    pub times: usize,
    pub sample: String,
}

fn agg() -> &'static Mutex<Agg> {
    static AGG: OnceLock<Mutex<Agg>> = OnceLock::new();
    AGG.get_or_init(|| Mutex::new(Agg::default()))
}

/// Per-run layer snapshots keyed by run id — the "profile of THIS run" the
/// Layers view selects. Deliberately NOT cleared by `reset_aggregates`: a
/// run's profile is the durable artifact.
fn per_run() -> &'static Mutex<std::collections::BTreeMap<u64, Agg>> {
    static P: OnceLock<Mutex<std::collections::BTreeMap<u64, Agg>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

/// Per-run statement windows (the loadgen diffs global SQL counters across
/// the run — exact whenever the run is the only traffic, an upper bound on
/// `max_us` otherwise).
fn run_sql() -> &'static Mutex<std::collections::BTreeMap<u64, Vec<serde_json::Value>>> {
    static R: OnceLock<Mutex<std::collections::BTreeMap<u64, Vec<serde_json::Value>>>> =
        OnceLock::new();
    R.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

const PER_RUN_CAP: usize = 128;

pub fn reset_aggregates() {
    *agg().lock().unwrap() = Agg::default();
    *global_sql().lock().unwrap() = GlobalSql::default();
}

const RING_CAP: usize = 4096;

/// Fold one request into an aggregate bucket — used for the global view and
/// for each run's per-run snapshot.
fn update_agg(a: &mut Agg, t: &TraceInner, b: &Buckets) {
    a.total_requests += 1;

    let r = a.routes.entry(t.route.clone()).or_default();
    r.stat.add(b.total_us);
    if r.ring_us.len() >= RING_CAP {
        r.ring_us.pop_front();
    }
    r.ring_us.push_back(b.total_us);
    if t.status >= 400 {
        r.errors += 1;
    }

    for row in &t.mw {
        a.middleware
            .entry(row.name.clone())
            .or_default()
            .add((row.before_us + row.after_us) as f64);
    }
    a.middleware.entry("security (headers+csrf)".into()).or_default().add(b.security_us);
    a.middleware.entry("sessions".into()).or_default().add(b.sessions_us);
    a.middleware.entry("bench probe (wrap)".into()).or_default().add(b.trace_wrap_us);

    for (k, v) in [
        ("middleware", b.middleware_us),
        ("routing", b.routing_us),
        ("query_build", b.query_build_us),
        ("database", b.database_us),
        ("serialize", b.serialize_us),
        ("render", b.render_us),
        ("handler_logic", b.handler_logic_us),
        ("tail", b.tail_us),
    ] {
        a.layers.entry(k.into()).or_default().add(v);
    }
}

fn record(t: &TraceInner, b: &Buckets) {
    {
        let mut a = agg().lock().unwrap_or_else(|e| e.into_inner());
        update_agg(&mut a, t, b);
    }
    if let Some(id) = t.run_id {
        let mut map = per_run().lock().unwrap_or_else(|e| e.into_inner());
        update_agg(map.entry(id).or_default(), t, b);
        while map.len() > PER_RUN_CAP {
            let oldest = *map.keys().next().expect("non-empty");
            map.remove(&oldest);
            run_sql().lock().unwrap_or_else(|e| e.into_inner()).remove(&oldest);
        }
    }

    // Exact per-request N+1: one fingerprint repeated above threshold within
    // THIS request (only reachable when the driver carries request context).
    let mut per_fp: HashMap<&str, usize> = HashMap::new();
    for e in &t.sql {
        *per_fp.entry(e.fingerprint.as_str()).or_default() += 1;
    }
    for (fp, n) in per_fp {
        if n >= N1_THRESHOLD {
            let mut g = global_sql().lock().unwrap_or_else(|e| e.into_inner());
            if !g.n1.iter().any(|f| f.fingerprint == fp && f.route == t.route) {
                g.n1.push(N1Flag {
                    route: t.route.clone(),
                    fingerprint: fp.to_string(),
                    times: n,
                    sample: t
                        .sql
                        .iter()
                        .find(|e| e.fingerprint == fp)
                        .map(|e| e.summary.chars().take(160).collect())
                        .unwrap_or_default(),
                });
            }
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// The shared JSON shape of a layer profile — used for the global view and
/// for each run's snapshot.
fn agg_json(a: &Agg) -> serde_json::Value {
    let mut routes: Vec<serde_json::Value> = a
        .routes
        .iter()
        .map(|(route, r)| {
            let mut v: Vec<f64> = r.ring_us.iter().copied().collect();
            v.sort_by(|x, y| x.partial_cmp(y).unwrap());
            serde_json::json!({
                "route": route,
                "count": r.stat.count,
                "errors": r.errors,
                "avg_us": r.stat.avg(),
                "p50_us": percentile(&v, 0.50),
                "p95_us": percentile(&v, 0.95),
                "p99_us": percentile(&v, 0.99),
                "max_us": r.stat.max_us,
            })
        })
        .collect();
    routes.sort_by(|x, y| {
        y["count"].as_u64().cmp(&x["count"].as_u64())
    });

    let middleware: Vec<serde_json::Value> = a
        .middleware
        .iter()
        .map(|(name, s)| {
            serde_json::json!({
                "name": name, "count": s.count, "avg_us": s.avg(), "max_us": s.max_us,
            })
        })
        .collect();

    let layers: Vec<serde_json::Value> = ["middleware", "routing", "query_build", "database",
        "serialize", "render", "handler_logic", "tail"]
        .iter()
        .map(|k| {
            let s = a.layers.get(*k);
            serde_json::json!({
                "layer": k,
                "count": s.map(|s| s.count).unwrap_or(0),
                "avg_us": s.map(|s| s.avg()).unwrap_or(0.0),
            })
        })
        .collect();

    serde_json::json!({
        "backend": crate::loadgen::backend_label(),
        "total_requests": a.total_requests,
        "routes": routes,
        "middleware": middleware,
        "layers": layers,
    })
}

/// Rolling aggregates as JSON — what the frontend's layers/runs views read.
pub fn snapshot() -> serde_json::Value {
    let mut v = {
        let a = agg().lock().unwrap_or_else(|e| e.into_inner());
        agg_json(&a)
    };

    let mut sql: Vec<serde_json::Value> = global_sql()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stats
        .iter()
        .map(|(fp, s)| {
            serde_json::json!({
                "fingerprint": fp, "sample": s.sample, "calls": s.calls,
                "avg_us": if s.calls == 0 { 0.0 } else { s.sum_us / s.calls as f64 },
                "max_us": s.max_us,
            })
        })
        .collect();
    sql.sort_by(|x, y| {
        y["avg_us"].as_f64().partial_cmp(&x["avg_us"].as_f64()).unwrap()
    });
    sql.truncate(25);
    let n1 = global_sql().lock().unwrap_or_else(|e| e.into_inner()).n1.clone();

    v["sql"] = serde_json::json!(sql);
    v["n1"] = serde_json::json!(n1);
    v
}

/// Baseline of the global SQL counters — the loadgen takes one at run start
/// and diffs it at run end for that run's statement window.
pub type SqlCounters = HashMap<String, (u64, f64, f64, String)>;

pub fn sql_counters() -> SqlCounters {
    global_sql()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stats
        .iter()
        .map(|(k, s)| (k.clone(), (s.calls, s.sum_us, s.max_us, s.sample.clone())))
        .collect()
}

pub fn sql_delta(before: &SqlCounters) -> Vec<serde_json::Value> {
    let after = global_sql().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<serde_json::Value> = after
        .stats
        .iter()
        .filter_map(|(fp, s)| {
            let b = before.get(fp);
            let calls = s.calls - b.map(|b| b.0).unwrap_or(0);
            if calls == 0 {
                return None;
            }
            let sum = s.sum_us - b.map(|b| b.1).unwrap_or(0.0);
            Some(serde_json::json!({
                "fingerprint": fp,
                "sample": s.sample,
                "calls": calls,
                "avg_us": sum / calls as f64,
                // Upper bound: the global max for this fingerprint (exact
                // when the run is the only traffic).
                "max_us": s.max_us,
            }))
        })
        .collect();
    out.sort_by(|x, y| y["avg_us"].as_f64().partial_cmp(&x["avg_us"].as_f64()).unwrap());
    out.truncate(25);
    out
}

pub fn store_run_sql(id: u64, rows: Vec<serde_json::Value>) {
    run_sql().lock().unwrap_or_else(|e| e.into_inner()).insert(id, rows);
}

/// The layer profile of ONE run: only requests whose `x-bench-run` header
/// named it. `None` until at least one such request has been captured.
pub fn per_run_snapshot(id: u64) -> Option<serde_json::Value> {
    let mut v = {
        let map = per_run().lock().unwrap_or_else(|e| e.into_inner());
        let a = map.get(&id)?;
        agg_json(a)
    };
    let sql = run_sql()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
        .unwrap_or_default();
    v["captured"] = serde_json::json!(true);
    v["sql"] = serde_json::json!(sql);
    v["n1"] = serde_json::json!([]);
    Some(v)
}

// ===========================================================================
// The app middlewares
// ===========================================================================

/// Outer boundary of the measured window: creates the trace, finalizes it.
/// Registered first with `order = i32::MIN` so it runs before every other
/// middleware; its `after_response` runs last (onion) and composes the
/// `Server-Timing` header, the aggregates, and the JSONL line.
pub struct TraceRoot;

#[umbral::async_trait]
impl Middleware for TraceRoot {
    fn name(&self) -> &'static str {
        "bench trace (root)"
    }

    fn order(&self) -> i32 {
        i32::MIN
    }

    async fn before_request(&self, mut req: Request) -> Result<Request, Response> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let run_id = req
            .headers()
            .get("x-bench-run")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let trace = Trace::new(
            id,
            req.method().to_string(),
            req.uri().path().to_string(),
            run_id,
        );
        register_active(&trace);
        req.extensions_mut().insert(trace);
        Ok(req)
    }

    async fn after_response(&self, res: Response) -> Response {
        let Some(trace) = res.extensions().get::<Trace>().cloned() else {
            return res;
        };
        let mut res = res;
        let status = res.status().as_u16();

        let t = trace.lock();
        let total_ms = t.t0.elapsed().as_secs_f64() * 1000.0;
        let b = Buckets::compute(&t, total_ms);

        // Server-Timing: browsers/devtools render these natively; the load
        // generator aggregates them per response.
        let mut st = format!("total;dur={total_ms:.1}");
        for row in &t.mw {
            st.push_str(&format!(
                ", {};dur={:.2}",
                slug(&row.name),
                (row.before_us + row.after_us) as f64 / 1000.0
            ));
        }
        st.push_str(&format!(", security;dur={:.2}", b.security_us / 1000.0));
        st.push_str(&format!(", sessions;dur={:.2}", b.sessions_us / 1000.0));
        if t.sql.is_empty() {
            st.push_str(&format!(
                ", db;dur={:.2};desc=\"orm wall, statements global\"",
                b.database_us / 1000.0
            ));
        } else {
            st.push_str(&format!(
                ", db;dur={:.2};desc=\"{} queries\"",
                b.database_us / 1000.0,
                t.sql.len()
            ));
        }
        st.push_str(&format!(", handler;dur={:.2}", (b.total_us - b.middleware_us - b.routing_us - b.tail_us) / 1000.0));

        let line = RequestLine {
            ts: chrono::Utc::now(),
            id: t.id,
            method: t.method.clone(),
            path: t.path.clone(),
            route: t.route.clone(),
            status,
            total_ms,
            sql_count: t.sql.len(),
            db_source: if t.sql.is_empty() { "orm_wall" } else { "sqlx" },
            mw: t.mw.clone(),
            security_ms: b.security_us / 1000.0,
            sessions_ms: b.sessions_us / 1000.0,
            trace_wrap_ms: b.trace_wrap_us / 1000.0,
            routing_ms: b.routing_us / 1000.0,
            query_build_ms: b.query_build_us / 1000.0,
            database_ms: b.database_us / 1000.0,
            serialize_ms: b.serialize_us / 1000.0,
            render_ms: b.render_us / 1000.0,
            handler_logic_ms: b.handler_logic_us / 1000.0,
            tail_ms: b.tail_us / 1000.0,
            middleware_ms: b.middleware_us / 1000.0,
            sql: t.sql.iter().take(12).cloned().collect(),
        };
        drop(t);

        let mut inner = trace.lock();
        inner.status = status;
        record(&inner, &b);
        drop(inner);

        unregister_active(trace.id());

        if let Ok(v) = HeaderValue::from_str(&st) {
            res.headers_mut().insert("server-timing", v);
        }
        if let Ok(v) = HeaderValue::from_str(&trace.id().to_string()) {
            res.headers_mut().insert("x-bench-trace", v);
        }
        if let Ok(json) = serde_json::to_string(&line) {
            emit_jsonl("requests", json);
        }
        res
    }
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Serialize)]
struct RequestLine {
    ts: chrono::DateTime<chrono::Utc>,
    id: u64,
    method: String,
    path: String,
    route: String,
    status: u16,
    total_ms: f64,
    sql_count: usize,
    db_source: &'static str,
    mw: Vec<MwRow>,
    security_ms: f64,
    sessions_ms: f64,
    trace_wrap_ms: f64,
    routing_ms: f64,
    query_build_ms: f64,
    database_ms: f64,
    serialize_ms: f64,
    render_ms: f64,
    handler_logic_ms: f64,
    tail_ms: f64,
    middleware_ms: f64,
    sql: Vec<SqlEvent>,
}

/// Bumps a counter header so the middleware has real (if tiny) after-work.
pub struct ResponseStamp;

#[umbral::async_trait]
impl Middleware for ResponseStamp {
    fn name(&self) -> &'static str {
        "response stamp"
    }

    fn order(&self) -> i32 {
        -80
    }

    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        let t = Instant::now();
        let trace = req.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_before(self.name(), t.elapsed().as_micros() as u64);
        }
        Ok(req)
    }

    async fn after_response(&self, mut res: Response) -> Response {
        let t = Instant::now();
        res.headers_mut()
            .insert("x-served-by", HeaderValue::from_static("umbral-bench"));
        let trace = res.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_after(self.name(), t.elapsed().as_micros() as u64);
        }
        res
    }
}

/// Debug-level request/response logging — the "request log" row in the UI,
/// and a template for instrumenting a middleware of your own.
pub struct RequestLog;

#[umbral::async_trait]
impl Middleware for RequestLog {
    fn name(&self) -> &'static str {
        "request log"
    }

    fn order(&self) -> i32 {
        -90
    }

    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        let t = Instant::now();
        tracing::debug!(method = %req.method(), path = %req.uri().path(), "bench request");
        let trace = req.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_before(self.name(), t.elapsed().as_micros() as u64);
        }
        Ok(req)
    }

    async fn after_response(&self, res: Response) -> Response {
        let t = Instant::now();
        tracing::debug!(status = res.status().as_u16(), "bench response");
        let trace = res.extensions().get::<Trace>().cloned();
        if let Some(trace) = trace {
            trace.mw_after(self.name(), t.elapsed().as_micros() as u64);
        }
        res
    }
}

// ===========================================================================
// Middleware inventory — "show ALL middleware", registry + measured
// ===========================================================================

/// The framework's typed-middleware registry (task #323), with the
/// harness's live per-middleware measurements merged in by name.
pub fn middleware_inventory() -> serde_json::Value {
    let mut out = Vec::new();
    if let Some(reg) = umbral::middleware::get() {
        for spec in reg.effective_order() {
            out.push(serde_json::json!({
                "name": spec.name,
                "order": spec.order,
                "source": format!("{:?}", spec.source),
            }));
        }
    }
    // Wrap-layer rows the typed registry deliberately does not record
    // (see the registry's drift caveat) — measured by the probe seam.
    for name in [
        "bench probe (wrap)",
        "security (headers+csrf)",
        "sessions",
    ] {
        out.push(serde_json::json!({
            "name": name, "order": null, "source": "WrapRouter",
        }));
    }

    let a = agg().lock().unwrap();
    let measured: Vec<serde_json::Value> = a
        .middleware
        .iter()
        .map(|(name, s)| {
            serde_json::json!({
                "name": name, "count": s.count, "avg_us": s.avg(), "max_us": s.max_us,
            })
        })
        .collect();

    serde_json::json!({ "chain": out, "measured": measured })
}
