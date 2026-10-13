//! In-process load generator + run registry.
//!
//! A run spawns N virtual-user tasks that issue HTTP requests against this
//! same server over loopback (`reqwest`), so every measured request travels
//! the REAL stack — TLS-less loopback keeps the generator's own cost small
//! and the per-response `Server-Timing` header rides back for free, giving
//! the run record real per-layer numbers from the server side.
//!
//! Samples are aggregated once per second and broadcast, feeding both the
//! run record and the SSE stream (`GET /api/bench/stream`). Run records are
//! also appended to `traces/runs.jsonl` off the request path.
//!
//! v1 shape: fixed-VU runs. The binary search over VU levels (the design's
//! "ceiling search") is a thin driver over these runs — a later pass.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use umbral_auth::{AuthUser, auth_user};

use crate::models::{Post, post};

// ---------------------------------------------------------------------------
// Configuration knobs (process-wide, set once from main)
// ---------------------------------------------------------------------------

fn globals() -> &'static Mutex<Globals> {
    static G: OnceLock<Mutex<Globals>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(Globals::default()))
}

#[derive(Default)]
struct Globals {
    self_url: String,
    backend: String,
}

pub fn set_self_url(url: String) {
    globals().lock().unwrap().self_url = url;
}

pub fn set_backend(label: &str) {
    globals().lock().unwrap().backend = label.to_string();
}

/// The backend label this process serves (`sqlite` / `postgres`), for the UI.
pub fn backend_label() -> String {
    globals().lock().unwrap().backend.clone()
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub sec: u32,
    pub rps: f64,
    pub sent: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// p95 of the `Server-Timing: total` values reported by the server for
    /// this second's responses (client-observed latency minus network).
    pub server_p95_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Summary {
    pub rps_avg: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub errors: u64,
    pub sent: u64,
    /// Per-kind failure detail: status, path template, count, sample body.
    #[serde(default)]
    pub error_breakdown: Vec<ErrorKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: u64,
    pub scenario: String,
    pub vus: u32,
    pub duration_s: u32,
    pub backend: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub samples: Vec<Sample>,
    pub summary: Option<Summary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SampleEvent {
    pub run_id: u64,
    pub status: String,
    pub sample: Option<Sample>,
    pub summary: Option<Summary>,
    /// Error kinds seen for the FIRST time in this run during this tick —
    /// lets the live view show real failures without polling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_errors: Vec<ErrorKind>,
}

struct Registry {
    runs: Vec<RunRecord>,
    tx: broadcast::Sender<SampleEvent>,
    next_id: u64,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        let (tx, _) = broadcast::channel(4096);
        let (runs, next_id) = load_persisted_runs();
        Mutex::new(Registry { runs, tx, next_id })
    })
}

/// Rehydrate run history from `traces/runs.jsonl` so every run is kept
/// across restarts — each run is one append-only line, and ids continue past
/// the persisted maximum.
fn load_persisted_runs() -> (Vec<RunRecord>, u64) {
    let dir = std::env::var("UMBRAL_BENCH_TRACES_DIR").unwrap_or_else(|_| "traces".to_string());
    let path = std::path::Path::new(&dir).join("runs.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Vec::new(), 1);
    };
    let mut runs: Vec<RunRecord> = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        match serde_json::from_str::<RunRecord>(line) {
            Ok(r) => runs.push(r),
            Err(_) => skipped += 1,
        }
    }
    runs.sort_by_key(|r| r.id);
    let next_id = runs.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    if !runs.is_empty() || skipped > 0 {
        eprintln!(
            "[bench] rehydrated {} run record(s) from {} ({skipped} unreadable line(s) skipped)",
            runs.len(),
            path.display()
        );
    }
    (runs, next_id)
}

pub fn subscribe() -> broadcast::Receiver<SampleEvent> {
    registry().lock().unwrap().tx.subscribe()
}

pub fn list_runs() -> Vec<RunRecord> {
    registry().lock().unwrap().runs.clone()
}

pub fn get_run(id: u64) -> Option<RunRecord> {
    registry()
        .lock()
        .unwrap()
        .runs
        .iter()
        .find(|r| r.id == id)
        .cloned()
}

fn emit_event(ev: SampleEvent) {
    let _ = registry().lock().unwrap().tx.send(ev);
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioKind {
    /// Read-heavy social feed — the design's default (~8% writes).
    Feed,
    /// Write-heavy mix (~43% writes).
    WriteHeavy,
    /// The heavy profile aggregate, hammered.
    Profile,
    /// No-DB baseline (`GET /api/ping`).
    Static,
    /// Stateful browsing personas only (reader / lurker journeys).
    Readers,
    /// Persona mix: readers, writers and lurkers walking scripted journeys —
    /// users doing different things at different times, not one URL forever.
    Community,
}

impl ScenarioKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "feed" => Some(Self::Feed),
            "write-heavy" => Some(Self::WriteHeavy),
            "profile" => Some(Self::Profile),
            "static" => Some(Self::Static),
            "readers" => Some(Self::Readers),
            "community" => Some(Self::Community),
            _ => None,
        }
    }
}

enum Op {
    Get(&'static str),
    Post(&'static str),
}

struct Weighted {
    weight: u32,
    op: Op,
    /// Requests authenticate as a random user via `x-bench-user`.
    auth: bool,
}

/// How a scenario drives its virtual users.
enum Plan {
    /// A weighted mix of INDEPENDENT requests — single-endpoint hammering,
    /// kept because "how fast is this one route" is a real question.
    Ops(Vec<Weighted>),
    /// Weighted personas walking STATEful multi-step journeys with think
    /// time — how people actually move through the app.
    Journeys(Vec<Journey>),
}

/// One persona: a scripted sequence of requests and pauses.
struct Journey {
    weight: u32,
    steps: &'static [Step],
}

#[derive(Clone, Copy)]
enum Step {
    Get(&'static str),
    Post(&'static str),
    /// Pause for a random duration in `[min, max)` milliseconds.
    Think(u64, u64),
}

// Personas mirror how people move through a social app: browse, linger,
// open profiles, occasionally write. The Think pauses are the "reading
// time" between actions — they are what makes the load look like users
// instead of a request cannon.
const READER: &[Step] = &[
    Step::Think(400, 1800),
    Step::Get("/api/feed"),
    Step::Think(600, 2600),
    Step::Get("/u/{id}"),
    Step::Think(800, 3200),
    Step::Get("/api/users/{id}/profile"),
    Step::Think(300, 1500),
    Step::Get("/api/feed?limit=10"),
];
const WRITER: &[Step] = &[
    Step::Think(500, 2000),
    Step::Get("/api/feed"),
    Step::Think(700, 2500),
    Step::Post("/api/posts"),
    Step::Think(250, 1200),
    Step::Post("/api/posts/{pid}/like"),
    Step::Think(200, 900),
    Step::Get("/u/{id}"),
];
const LURKER: &[Step] = &[
    Step::Think(900, 4000),
    Step::Get("/"),
    Step::Think(1200, 5000),
    Step::Get("/u/{id}"),
    Step::Think(1000, 4000),
    Step::Get("/posts/{pid}"),
];

fn plan(kind: ScenarioKind) -> Plan {
    use Op::*;
    match kind {
        ScenarioKind::Feed => Plan::Ops(vec![
            Weighted {
                weight: 70,
                op: Get("/api/feed"),
                auth: true,
            },
            Weighted {
                weight: 5,
                op: Get("/api/feed-naive"),
                auth: true,
            },
            Weighted {
                weight: 10,
                op: Get("/u/{id}"),
                auth: true,
            },
            Weighted {
                weight: 10,
                op: Get("/api/users/{id}/profile"),
                auth: true,
            },
            Weighted {
                weight: 5,
                op: Post("/api/posts/{pid}/like"),
                auth: true,
            },
        ]),
        ScenarioKind::WriteHeavy => Plan::Ops(vec![
            Weighted {
                weight: 30,
                op: Post("/api/posts"),
                auth: true,
            },
            Weighted {
                weight: 25,
                op: Post("/api/posts/{pid}/like"),
                auth: true,
            },
            // `{other}` — the TARGET of a follow must differ from the acting
            // user (`{id}` is both the auth header and the path id elsewhere);
            // following yourself is a 400 by design and made every follow
            // request fail when this step used `{id}`.
            Weighted {
                weight: 10,
                op: Post("/api/users/{other}/follow"),
                auth: true,
            },
            Weighted {
                weight: 35,
                op: Get("/api/feed"),
                auth: true,
            },
        ]),
        ScenarioKind::Profile => Plan::Ops(vec![
            Weighted {
                weight: 85,
                op: Get("/api/users/{id}/profile"),
                auth: true,
            },
            Weighted {
                weight: 15,
                op: Get("/u/{id}"),
                auth: true,
            },
        ]),
        ScenarioKind::Static => Plan::Ops(vec![Weighted {
            weight: 100,
            op: Get("/api/ping"),
            auth: false,
        }]),
        ScenarioKind::Readers => Plan::Journeys(vec![
            Journey {
                weight: 70,
                steps: READER,
            },
            Journey {
                weight: 30,
                steps: LURKER,
            },
        ]),
        ScenarioKind::Community => Plan::Journeys(vec![
            Journey {
                weight: 60,
                steps: READER,
            },
            Journey {
                weight: 25,
                steps: WRITER,
            },
            Journey {
                weight: 15,
                steps: LURKER,
            },
        ]),
    }
}

// ---------------------------------------------------------------------------
// Run driver
// ---------------------------------------------------------------------------

pub async fn start_run(scenario: &str, vus: u32, duration_s: u32) -> Result<RunRecord, String> {
    let kind = ScenarioKind::parse(scenario).ok_or_else(|| {
        format!(
            "unknown scenario '{scenario}' \
             (feed | write-heavy | profile | static | readers | community)"
        )
    })?;
    if !(1..=10_000).contains(&vus) {
        return Err("vus must be 1..=10000".into());
    }
    if !(1..=3600).contains(&duration_s) {
        return Err("duration_s must be 1..=3600".into());
    }

    // One run at a time: per-run telemetry keys off the run id, and the
    // per-run SQL window works by diffing global counters across the run —
    // concurrent runs would pollute both.
    if let Some(active) = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .runs
        .iter()
        .find(|r| r.status == "running")
    {
        return Err(format!(
            "run r-{} is still in progress — runs are serialized so each run's telemetry window stays clean",
            active.id
        ));
    }

    // Resolve the id ranges ONCE, through the ORM, so path substitution only
    // ever points at rows that exist.
    let uid_max = AuthUser::objects()
        .order_by(auth_user::ID.desc())
        .limit(1)
        .first()
        .await
        .map_err(|e| e.to_string())?
        .map(|u| u.id)
        .unwrap_or(0);
    let pid_max = Post::objects()
        .order_by(post::ID.desc())
        .limit(1)
        .first()
        .await
        .map_err(|e| e.to_string())?
        .map(|p| p.id)
        .unwrap_or(0);
    if uid_max == 0 || pid_max == 0 {
        return Err("database has no bench rows — let the seed run (cargo run -- serve)".into());
    }

    let (self_url, backend) = {
        let g = globals().lock().unwrap();
        (g.self_url.clone(), g.backend.clone())
    };
    if self_url.is_empty() {
        return Err("load generator self URL not configured".into());
    }

    let (id, record) = {
        let mut r = registry().lock().unwrap();
        let id = r.next_id;
        r.next_id += 1;
        let record = RunRecord {
            id,
            scenario: scenario.to_string(),
            vus,
            duration_s,
            backend,
            status: "running".into(),
            started_at: Utc::now(),
            samples: Vec::new(),
            summary: None,
        };
        r.runs.push(record.clone());
        (id, record)
    };
    emit_event(SampleEvent {
        run_id: id,
        status: "running".into(),
        sample: None,
        summary: None,
        new_errors: Vec::new(),
    });

    let stop = Arc::new(AtomicBool::new(false));
    let plan = Arc::new(plan(kind));
    // Baseline BEFORE any run traffic; diffed at the end for the run's
    // statement window.
    let sql_before = crate::telemetry::sql_counters();
    let (tx, rx) = mpsc::unbounded_channel::<Observation>();
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(vus as usize + 8)
        .build()
        .map_err(|e| e.to_string())?;

    for v in 0..vus {
        let worker = Worker {
            client: client.clone(),
            base: self_url.clone(),
            run_id: id,
            plan: plan.clone(),
            tx: tx.clone(),
            stop: stop.clone(),
            uid_max,
            pid_max,
            seed: 0x9E37_79B9_7F4A_7C15u64 ^ (v as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9),
        };
        tokio::spawn(worker.run());
    }
    drop(tx);

    let agg = Aggregator {
        run_id: id,
        duration_s,
        rx,
        stop: stop.clone(),
        sql_before,
    };
    tokio::spawn(agg.run());

    Ok(record)
}

/// One completed request as seen by the client.
struct Observation {
    ms: f64,
    ok: bool,
    /// HTTP status; 0 = transport error (connection refused, reset, ...).
    status: u16,
    /// The path template this request exercised (never the concrete path).
    template: &'static str,
    /// Response body excerpt (or transport error) for FAILED requests.
    sample: Option<String>,
    server_total_ms: Option<f64>,
}

/// Aggregated failures of one (status, template) kind within a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorKind {
    pub status: u16,
    pub path: String,
    pub count: u64,
    pub sample: String,
}

struct Worker {
    client: reqwest::Client,
    base: String,
    run_id: u64,
    plan: Arc<Plan>,
    tx: mpsc::UnboundedSender<Observation>,
    stop: Arc<AtomicBool>,
    uid_max: i64,
    pid_max: i64,
    seed: u64,
}

impl Worker {
    async fn run(mut self) {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            // Clone the Arc so the plan borrow never overlaps `self.rand()`.
            let plan = Arc::clone(&self.plan);
            match &*plan {
                Plan::Ops(ops) => {
                    let total: u32 = ops.iter().map(|w| w.weight).sum();
                    let pick = (self.rand() % total.max(1) as u64) as u32;
                    let mut acc = 0u32;
                    let mut idx = 0usize;
                    for (i, w) in ops.iter().enumerate() {
                        acc += w.weight;
                        if pick < acc {
                            idx = i;
                            break;
                        }
                    }
                    let (template, is_post, auth): (&'static str, bool, bool) = {
                        let w = &ops[idx];
                        let t = match &w.op {
                            Op::Get(p) | Op::Post(p) => *p,
                        };
                        (t, matches!(w.op, Op::Post(_)), w.auth)
                    };
                    if !self.hit(template, is_post, auth).await {
                        break;
                    }
                    // Small think time — a closed-loop VU, like k6's default.
                    let think_ms = 5 + (self.rand() % 20);
                    tokio::time::sleep(Duration::from_millis(think_ms)).await;
                }
                Plan::Journeys(journeys) => {
                    // Re-pick the persona each cycle: users do different
                    // things at different times, not one URL forever.
                    let total: u32 = journeys.iter().map(|j| j.weight).sum();
                    let pick = (self.rand() % total.max(1) as u64) as u32;
                    let mut acc = 0u32;
                    let mut idx = 0usize;
                    for (i, j) in journeys.iter().enumerate() {
                        acc += j.weight;
                        if pick < acc {
                            idx = i;
                            break;
                        }
                    }
                    let steps: &'static [Step] = journeys[idx].steps;
                    for step in steps {
                        if self.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match *step {
                            Step::Think(lo, hi) => {
                                let span = hi.saturating_sub(lo).max(1);
                                let ms = lo + (self.rand() % span);
                                tokio::time::sleep(Duration::from_millis(ms)).await;
                            }
                            Step::Get(t) => {
                                if !self.hit(t, false, true).await {
                                    return;
                                }
                            }
                            Step::Post(t) => {
                                if !self.hit(t, true, true).await {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// One HTTP request against `template`. Returns `false` when the
    /// observation channel is closed (run over / aggregator gone).
    async fn hit(&mut self, template: &'static str, is_post: bool, auth: bool) -> bool {
        let uid = 1 + (self.rand() % self.uid_max.max(1) as u64) as i64;
        let pid = 1 + (self.rand() % self.pid_max.max(1) as u64) as i64;
        let other = {
            // A user id distinct from `uid` where possible (follows).
            let mut o = 1 + (self.rand() % self.uid_max.max(1) as u64) as i64;
            if self.uid_max > 1 && o == uid {
                o = if uid >= self.uid_max { 1 } else { uid + 1 };
            }
            o
        };
        let body_seed = self.rand();
        let path = subst(template, uid, pid, other);
        let method = if is_post { "POST" } else { "GET" };
        let url = format!("{}{}", self.base, path);

        let mut req = match method {
            "GET" => self.client.get(&url),
            _ => self.client.post(&url),
        };
        if auth {
            req = req.header("x-bench-user", uid);
        }
        // Tag every request with its run: the server keeps a per-run layer
        // profile (the Layers view's run selector) from this header.
        req = req.header("x-bench-run", self.run_id);
        if template == "/api/posts" {
            let payload = serde_json::json!({ "body": format!("bench post {body_seed}") });
            req = req.json(&payload);
        }

        let t0 = std::time::Instant::now();
        let obs = match req.send().await {
            Ok(resp) => {
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                let status = resp.status().as_u16();
                let ok = resp.status().is_success();
                let server_total_ms = resp
                    .headers()
                    .get("server-timing")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_total_from_server_timing);
                let sample = if ok {
                    None
                } else {
                    // Read the failing body — this is what makes a run
                    // diagnosable from the UI instead of a bare count.
                    let body = resp.text().await.unwrap_or_default();
                    Some(body.chars().take(200).collect())
                };
                Observation {
                    ms,
                    ok,
                    status,
                    template,
                    sample,
                    server_total_ms,
                }
            }
            Err(e) => {
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                Observation {
                    ms,
                    ok: false,
                    status: 0,
                    template,
                    sample: Some(format!("{e}").chars().take(200).collect()),
                    server_total_ms: None,
                }
            }
        };
        self.tx.send(obs).is_ok()
    }

    fn rand(&mut self) -> u64 {
        let mut x = self.seed;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.seed = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn subst(template: &str, uid: i64, pid: i64, other: i64) -> String {
    template
        .replace("{id}", &uid.to_string())
        .replace("{pid}", &pid.to_string())
        .replace("{other}", &other.to_string())
}

/// `"total;dur=41.2, ..."` → 41.2
fn parse_total_from_server_timing(v: &str) -> Option<f64> {
    v.split(',').find_map(|part| {
        let part = part.trim();
        let rest = part.strip_prefix("total;")?;
        let dur = rest.strip_prefix("dur=")?;
        let num: String = dur
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        num.parse().ok()
    })
}

struct Aggregator {
    run_id: u64,
    duration_s: u32,
    rx: mpsc::UnboundedReceiver<Observation>,
    stop: Arc<AtomicBool>,
    sql_before: crate::telemetry::SqlCounters,
}

impl Aggregator {
    async fn run(mut self) {
        let mut all_ms: Vec<f64> = Vec::new();
        let mut total_sent: u64 = 0;
        let mut total_errors: u64 = 0;
        // Per-kind failure detail, aggregated over the whole run; `seen_errors`
        // gates the one-time broadcast of each kind to live viewers.
        let mut error_kinds: std::collections::BTreeMap<(u16, &'static str), ErrorKind> =
            std::collections::BTreeMap::new();
        let mut seen_errors: std::collections::HashSet<(u16, &'static str)> =
            std::collections::HashSet::new();

        for sec in 0..self.duration_s {
            let sec_ms: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
            let sec_server: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
            let sec_sent = Arc::new(AtomicU64::new(0));
            let sec_errors = Arc::new(AtomicU64::new(0));
            let mut second_new_errors: Vec<ErrorKind> = Vec::new();

            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            loop {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    break;
                }
                match tokio::time::timeout(deadline - now, self.rx.recv()).await {
                    Ok(Some(obs)) => {
                        sec_sent.fetch_add(1, Ordering::Relaxed);
                        if !obs.ok {
                            sec_errors.fetch_add(1, Ordering::Relaxed);
                            let key = (obs.status, obs.template);
                            let entry = error_kinds.entry(key).or_insert_with(|| ErrorKind {
                                status: obs.status,
                                path: obs.template.to_string(),
                                count: 0,
                                sample: String::new(),
                            });
                            entry.count += 1;
                            if entry.sample.is_empty() {
                                if let Some(sm) = &obs.sample {
                                    entry.sample = sm.clone();
                                }
                            }
                            if seen_errors.insert(key) {
                                second_new_errors.push(entry.clone());
                            }
                        }
                        sec_ms.lock().unwrap().push(obs.ms);
                        if let Some(s) = obs.server_total_ms {
                            sec_server.lock().unwrap().push(s);
                        }
                    }
                    Ok(None) => break, // all senders dropped
                    Err(_) => break,   // this second's window closed
                }
            }

            let mut ms = std::mem::take(&mut *sec_ms.lock().unwrap());
            let mut server = std::mem::take(&mut *sec_server.lock().unwrap());
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            server.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let sent = sec_sent.load(Ordering::Relaxed);
            let errors = sec_errors.load(Ordering::Relaxed);

            let sample = Sample {
                sec,
                rps: sent as f64,
                sent,
                errors,
                p50_ms: pct(&ms, 0.50),
                p95_ms: pct(&ms, 0.95),
                p99_ms: pct(&ms, 0.99),
                server_p95_ms: pct(&server, 0.95),
            };
            total_sent += sent;
            total_errors += errors;
            all_ms.extend_from_slice(&ms);

            {
                let mut r = registry().lock().unwrap();
                if let Some(rec) = r.runs.iter_mut().find(|r| r.id == self.run_id) {
                    rec.samples.push(sample.clone());
                }
            }
            emit_event(SampleEvent {
                run_id: self.run_id,
                status: "running".into(),
                sample: Some(sample),
                summary: None,
                new_errors: second_new_errors,
            });
        }

        self.stop.store(true, Ordering::Relaxed);
        all_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut error_breakdown: Vec<ErrorKind> = error_kinds.into_values().collect();
        error_breakdown.sort_by(|a, b| b.count.cmp(&a.count));
        let summary = Summary {
            rps_avg: if self.duration_s > 0 {
                total_sent as f64 / self.duration_s as f64
            } else {
                0.0
            },
            p50_ms: pct(&all_ms, 0.50),
            p95_ms: pct(&all_ms, 0.95),
            p99_ms: pct(&all_ms, 0.99),
            errors: total_errors,
            sent: total_sent,
            error_breakdown,
        };

        let record = {
            let mut r = registry().lock().unwrap();
            if let Some(rec) = r.runs.iter_mut().find(|r| r.id == self.run_id) {
                rec.status = "done".into();
                rec.summary = Some(summary.clone());
            }
            r.runs.iter().find(|r| r.id == self.run_id).cloned()
        };

        // Persist this run's statement window for the Layers view BEFORE the
        // done event, so a client reacting to it finds the data ready.
        crate::telemetry::store_run_sql(self.run_id, crate::telemetry::sql_delta(&self.sql_before));

        emit_event(SampleEvent {
            run_id: self.run_id,
            status: "done".into(),
            sample: None,
            summary: Some(summary),
            new_errors: Vec::new(),
        });

        if let Some(record) = record {
            if let Ok(json) = serde_json::to_string(&record) {
                crate::telemetry::emit_jsonl("runs", json);
            }
        }
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}
