// The workload, in one place.
//
// warmup.js and hold.js call `doIteration` (the future saturation.js will too),
// so a change to the mix cannot make two scripts disagree about what "the
// workload" is. The
// mix this file implements is recorded (and ratified) in `bench/workload.md`,
// and the values a run actually used travel inside its own `--summary-export`
// (`setup_data`) so a result can never be read apart from the workload that
// produced it.
//
// Endpoints and mix follow the twelve-dollar challenge's shape (a read-heavy
// feed with occasional engagement writes), extended with an HTML page so the
// template-render path is exercised — a dimension the challenge has none of.
//
// NO `Server-Timing` METRIC LIVES HERE, deliberately. The measured binary
// (`bench-prod`, :8114) constructs no telemetry — spec §2 and its locked
// decision #1, so that a number can never have measurement scaffolding in its
// path — which means it emits no `Server-Timing` header on any response. A
// trend fed from that header would be structurally empty in every run of this
// rig, and an always-empty metric is worse than an absent one: it reads as
// "the server reported zero" rather than "nothing was measured". The
// instrumented binary on :8113 does emit the header, which is why
// `harness/k6/*.js` collect it there; no number from :8113 is ever quoted as a
// performance result. If a `Server-Timing` trend is ever added to this file it
// means prod grew telemetry, which would invalidate the measurement rather
// than improve it.

import http from "k6/http";
import { check, sleep } from "k6";

export const BASE = __ENV.BASE || "http://127.0.0.1:8114";

// The scale of the dataset the silo SERVES — not the size of the workload.
// These bound the id ranges the workload draws from (users 1..USERS, posts
// 1..POSTS), so they have to match the rows in the database `run.sh serve`
// started with, or every read past the end is a 404 and a 404 is a failed
// request in k6's `http_req_failed`. The defaults are the challenge's quoted
// scale (spec §2a: 50,000 users, 500,000 posts); a development-scale database
// needs `-e USERS=20000 -e POSTS=200000`. `preflight()` refuses to start a run
// where the two disagree.
export const USERS = Number(__ENV.USERS || 50000);
export const POSTS = Number(__ENV.POSTS || USERS * 10);

// ---------------------------------------------------------------------------
// THE MIX — the operator's call, and the values below are the call it made:
// ratified 2026-10-14 and recorded, with its reasoning, in `bench/workload.md`.
// These three numbers define what "fast" means for this benchmark — every
// threshold, every p99 and every RPS number this rig ever produces is a
// statement about the application under THIS traffic — so they change only by
// explicit choice (spec locked decision #7), never to make a number look
// better, and a comparison across a mix change is not a comparison.
//
//   LIKE_P = probability an iteration also likes a post
//   POST_P = probability an iteration also creates a post
//   PAGE_P = probability an iteration reads the HTML profile page in place of
//            the single post read (the JSON feed is read by EVERY iteration)
//
// Each is independent of the others: PAGE_P chooses which second read happens,
// LIKE_P and POST_P then fire at exactly their stated rate. With no writes and
// no page, an iteration is still a feed read — the dominant operation.
// ---------------------------------------------------------------------------
export const LIKE_P = Number(__ENV.LIKE_P || 0.15);
export const POST_P = Number(__ENV.POST_P || 0.02);
export const PAGE_P = Number(__ENV.PAGE_P || 0.10);

// Think time, in milliseconds: the gap between one request and the next, which
// is what makes this a workload rather than a hammer. 3–8 s is a person
// browsing a feed. THINK_MAX=0 disables it entirely and exists for ONE caller:
// saturation.js, which measures the request-rate ceiling a VU count cannot
// reach. A scored hold with no think time is the v1 mistake this rig replaces.
const THINK_MIN = Number(__ENV.THINK_MIN ?? 3000);
const THINK_MAX = Number(__ENV.THINK_MAX ?? 8000);

const uid = () => 1 + Math.floor(Math.random() * USERS);
const pid = () => 1 + Math.floor(Math.random() * POSTS);

function think() {
  if (THINK_MAX <= 0) return;
  sleep((THINK_MIN + Math.random() * (THINK_MAX - THINK_MIN)) / 1000);
}

function jar(session) {
  // Every VU is pinned to one pooled session, so its requests are a single
  // coherent user rather than a new identity per call. The cookie rides on
  // reads too: the session middleware does its real store lookup either way,
  // and that lookup is part of the honest cost (spec locked decision #3).
  return session ? { cookies: { umbral_session: session } } : {};
}

function go(method, path, session, body) {
  const params = {
    ...jar(session),
    // Report by path TEMPLATE, so /api/users/123/profile aggregates as
    // /api/users/{id}/profile instead of as a new metric per id.
    tags: { name: path.replace(/\/\d+/g, "/{id}") },
  };
  if (method === "GET") {
    return http.get(`${BASE}${path}`, params);
  }
  return http.post(`${BASE}${path}`, body === undefined ? null : JSON.stringify(body), {
    ...params,
    headers: { "content-type": "application/json" },
  });
}

/// One iteration of the workload. `sessions` is the SharedArray from the pool.
export function doIteration(sessions) {
  // One VU = one client: `preflight` refuses any run whose pool is smaller
  // than its VU count, so this modulo maps every VU to a DISTINCT pool entry
  // (and never past the end). The pool holds one session per dataset user and
  // was shuffled at mint, so the entries the first `vus` VUs land on are a
  // random sample of the user base, not the lowest ids.
  const session = sessions[(__VU - 1) % sessions.length];

  // 1. The feed. Every iteration reads it; it is the one unconditional request
  //    and the reason this mix is read-dominant.
  const feed = go("GET", "/api/feed", session);
  check(feed, { "feed 200": (r) => r.status === 200 });

  // 2. The human beat: the time spent looking at what just arrived.
  think();

  // 3. One deeper read — the heavier HTML page, or a single post. `post 200` is
  //    deliberately strict: a 404 here means the id range and the served
  //    dataset disagree, so the run is measuring the miss path (cheaper, and
  //    not the app under test) rather than the render path. It must fail
  //    loudly rather than be tolerated.
  if (Math.random() < PAGE_P) {
    const page = go("GET", `/u/${uid()}`, session);
    check(page, { "page 200": (r) => r.status === 200 });
  } else {
    const post = go("GET", `/posts/${pid()}`, session);
    check(post, { "post 200": (r) => r.status === 200 });
  }

  // 4. Engagement. The like's target is a FRESH random post, not the one just
  //    read — the challenge's script draws them independently too — so the
  //    write working set is not pinned to the read's. A write is a real
  //    transaction against the same SQLite file the reads are hitting, which is
  //    the contention this workload exists to expose.
  if (Math.random() < LIKE_P) {
    const like = go("POST", `/api/posts/${pid()}/like`, session, {});
    check(like, { "like 2xx": (r) => r.status >= 200 && r.status < 300 });
  }
  if (Math.random() < POST_P) {
    const created = go("POST", "/api/posts", session, {
      body: `bench ${Date.now()} vu${__VU} iter${__ITER}`,
    });
    check(created, { "create 2xx": (r) => r.status >= 200 && r.status < 300 });
  }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// The scenario every script shares, so warm-up, hold and saturation ramp and
/// drain the same way and their summaries are shaped alike.
///
/// `duration` is the HOLD length only. The run is longer than it sounds: a
/// 60 s ramp up to `vus`, then the hold, then a 30 s ramp down — `hold.js` with
/// its 5 m hold is a 6.5 m wall clock. The ramp does not eat into the hold; it
/// precedes it. It does mean the run-level p95/p99 is taken over the ramp and
/// the drain too, so a hold's numbers are a mild average of the whole run
/// rather than of the steady state alone (the same is true of the challenge
/// this rig is comparable to).
export function buildOptions({ vus, duration, scored, ramp }) {
  const opts = {
    // Nothing in the loop reads a response body: every check is on the status
    // line. Dropping bodies at the client cannot change what the server does —
    // the bytes still cross the wire and still count in `data_received` — and
    // it keeps the generator's memory flat as VU counts climb (v1's
    // journeys.js learned this at 30k VUs; the search's cap is 40,000).
    discardResponseBodies: true,
    // p(95)/p(99) have to be asked for explicitly to appear in the summary
    // export Task 8's recorder reads.
    summaryTrendStats: ["avg", "med", "p(95)", "p(99)", "max"],
    scenarios: {
      users: {
        executor: "ramping-vus",
        startVUs: 0,
        gracefulRampDown: "10s",
        stages: ramp || [
          { duration: "60s", target: vus },
          { duration: duration || "5m", target: vus },
          { duration: "30s", target: 0 },
        ],
      },
    },
  };
  if (scored) {
    // The pass criteria, verbatim from the spec. `scored: false` means no
    // thresholds at all, and with no thresholds k6 exits 0 no matter what the
    // summary says — which is exactly what an unscored warm-up needs, since it
    // is not being judged and its write mix may still be 401-ing.
    opts.thresholds = {
      http_req_duration: ["p(95)<500", "p(99)<1000"],
      http_req_failed: ["rate<0.01"],
    };
  }
  return opts;
}

// ---------------------------------------------------------------------------
// The preflight
// ---------------------------------------------------------------------------

/// The run's parameters, echoed into every `--summary-export` as `setup_data`.
/// Task 8's recorder reads the mix and the dataset scale from there instead of
/// parsing this file, so a result cannot be filed apart from the workload that
/// produced it.
export function runParams() {
  return {
    base: BASE,
    users: USERS,
    posts: POSTS,
    mix: { LIKE_P: LIKE_P, POST_P: POST_P, PAGE_P: PAGE_P },
    think_ms: { min: THINK_MIN, max: THINK_MAX },
  };
}

/// The rig's pool scale: one session for every dataset user (20,000), minted
/// once per database and shuffled by `bench/seed-sessions.sh` so the VUs
/// active in a run are a random sample of the user base rather than always
/// the same first N ids. Every VU is pinned to one pooled session — one VU is
/// one client — and one pool serves every VU count up to the dataset size.
/// Only used for the size suggested in preflight's failure messages; the
/// authoritative numbers are `BENCH_SESSION_POOL_SIZE` (the seeder) and the
/// script's argument, and `bench/seed-sessions.sh` refuses to run when it can
/// see those two disagree — pass it the same number you gave the seeder.
const POOL_TARGET = 20000;

/// The two commands that rebuild the pool against the database the silo is
/// serving, as the lines of a preflight error message. `n` is the pool size
/// the run needs; the seeder's `BENCH_SESSION_POOL_SIZE` and the script's
/// argument must be that same number, or one half of the pool is sized for
/// the other (the script refuses to run when it can see the two disagree).
function rebuildCommands(n) {
  return (
    "  docker exec umbral-bench /app/umbral-bench-prod seed-sessions\n" +
    `  harness/bench/seed-sessions.sh 8114 ${n}   # see bench/workload.md`
  );
}

/// Call this from `setup()`. It spends a handful of requests to make sure the
/// run is not about to measure the wrong thing, and it THROWS — aborting the
/// run before a single iteration, with the fix in the message — rather than
/// letting a hold fail six minutes later for a reason that is not the server's.
///
/// `vus` is the run's VU count — the number of concurrent clients — and the
/// pool must hold at least that many DISTINCT sessions. The pool is minted
/// with one session per dataset user (20,000) and shuffled, so the entries a
/// run's VUs land on are a random sample of the user base. Three ways this rig
/// lies to itself, all structural rather than hypothetical:
///
///   * THE POOL vs THE VU COUNT. Every VU is pinned to one pooled session (one
///     VU = one client, bench/workload.md). With fewer sessions than VUs,
///     `(VU-1) % pool.length` hands each identity to several concurrent VUs:
///     real users do not share sessions, and sharing shrinks the session
///     working set and the last-seen write set inside the measured database
///     time. This is checked FIRST and is deliberately not skippable: it is
///     knowable from two numbers with no request at all, so `PREFLIGHT=0` —
///     the escape hatch for the server probes below — must not be able to
///     switch it off and silently restore the sharing this rig exists to
///     avoid.
///   * ID RANGES vs DATASET. USERS/POSTS are env knobs; the served rows are a
///     file. A range that runs past the seeded rows turns every read past the
///     end into a 404, a 404 counts as a failed request, and the hold fails its
///     `http_req_failed` threshold while measuring the miss path — a faster,
///     different workload wearing the real one's name.
///   * THE POOL vs THE SERVED DATABASE. Sessions are rows in the database the
///     server opened, so a pool built against a different file (Task 5 built
///     one against a scratch copy on purpose) authenticates nothing. With
///     writes in the mix that is 401s on every write, which reads exactly like
///     the server rejecting load under stress. `GET /api/auth/me` settles it in
///     one side-effect-free request: 200 when the cookie names a live session,
///     401 when it names nothing.
///
/// `PREFLIGHT=0` skips the SERVER probes (the two above that need a request) —
/// the ids are then on their own honour. The pool-vs-VU check always runs.
export function preflight(sessions, vus) {
  const params = runParams();

  if (!Number.isInteger(vus) || vus < 1) {
    throw new Error(
      "preflight: preflight(sessions, vus) needs the run's VU count as a positive " +
        `integer; got ${vus}. Every script that builds its options from a VU count ` +
        "must pass that same count here: without it the rig cannot tell whether the " +
        "session pool is big enough, and an unchecked pool is exactly the identity " +
        "sharing this check exists to prevent.",
    );
  }

  if (sessions.length === 0) {
    throw new Error(
      "preflight: session_pool.json is empty, so every VU would send no session " +
        "and every write would 401. Build the pool — one session per dataset user, " +
        "shuffled — against the database the silo is SERVING (the seeder and the " +
        "script must be the same size):\n" +
        rebuildCommands(Math.max(vus, POOL_TARGET)),
    );
  }

  if (sessions.length < vus) {
    throw new Error(
      `preflight: the session pool is smaller than this run — ${sessions.length} ` +
        `pooled sessions for ${vus} VUs. Every VU must get its own session: with a ` +
        `smaller pool, (VU-1) % ${sessions.length} gives each identity ` +
        `${Math.ceil(vus / sessions.length)} concurrent VUs, and real users do not ` +
        "share sessions — it shrinks the session working set and the last-seen write " +
        "set inside the measured database time, and biases the very number this rig " +
        "produces. The pool holds one session per dataset user (20,000), not one " +
        "per run. Rebuild it against the database the silo is SERVING, seeder and " +
        "script at the same size:\n" +
        rebuildCommands(Math.max(vus, POOL_TARGET)),
    );
  }

  // The pool's entry count travels in `setup_data` like the mix and the scale:
  // it is a fact about the SETUP the load drew from — which pool, shuffled
  // how far back — the recorder writes it into every result's `session`
  // block, and `results/compare.sh` refuses to diff two results whose pool
  // sizes differ. The number of distinct identities a run actually carried is
  // its own `vus` field; the pool is never smaller than that.
  const pool = { pool_size: sessions.length };
  if (Number(__ENV.PREFLIGHT ?? 1) === 0) {
    return { ...params, ...pool, preflight: "skipped (PREFLIGHT=0)" };
  }

  const ping = http.get(`${BASE}/api/ping`, { tags: { name: "preflight" } });
  if (ping.status !== 200) {
    throw new Error(
      `preflight: GET ${BASE}/api/ping -> ${ping.status}. Nothing is serving there; ` +
        "start the silo with `harness/bench/run.sh serve` and check `docker ps` " +
        "and its log before blaming the workload.",
    );
  }

  const writes = LIKE_P > 0 || POST_P > 0;
  if (writes) {
    const me = http.get(`${BASE}/api/auth/me`, {
      ...jar(sessions[0]),
      tags: { name: "preflight" },
    });
    if (me.status !== 200) {
      throw new Error(
        `preflight: the first pooled session does not authenticate ` +
          `(GET /api/auth/me -> ${me.status}), and the mix writes ` +
          `(LIKE_P=${LIKE_P}, POST_P=${POST_P}), so every write would 401 and the ` +
          "hold would fail `http_req_failed` for a reason that is not the server's. " +
          "The pool and the database it was built against must be the one the silo " +
          "serves (users AND session rows):\n" +
          rebuildCommands(sessions.length),
      );
    }
  } else {
    console.log(
      "preflight: no writes in the mix (LIKE_P=0, POST_P=0), so the pool is not " +
        "exercised — reads still carry a session cookie, but a session that does " +
        "not authenticate skips the user lookup the real workload pays for.",
    );
  }

  const misses = [];
  for (const probe of [
    { knob: "USERS", top: USERS, path: (id) => `/u/${id}` },
    { knob: "POSTS", top: POSTS, path: (id) => `/posts/${id}` },
  ]) {
    const miss = rangeMiss(probe.top, probe.path);
    if (miss) misses.push(`${probe.knob}=${probe.top}: ${miss}`);
  }
  if (misses.length > 0) {
    throw new Error(
      "preflight: the id ranges run past the rows the silo is serving —\n  " +
        misses.join("\n  ") +
        "\nThe workload draws ids from 1..USERS and 1..POSTS, so a read past the " +
        "end is a 404, and a 404 is a failed request in `http_req_failed`: the run " +
        "would fail its threshold for a reason that is not the server's. Set them " +
        "to the seeded scale, e.g. `-e USERS=20000 -e POSTS=200000` " +
        "(bench/workload.md), or set PREFLIGHT=0 to run anyway.",
    );
  }

  return { ...params, ...pool, preflight: "ok" };
}

/// Probe the top of an id range. Tolerant of a merely gappy top — three
/// consecutive misses is the range sitting outside the dataset, not a hole in
/// it — and of anything below id 1.
function rangeMiss(top, path) {
  for (let i = 0; i < 3 && top - i >= 1; i++) {
    const res = http.get(`${BASE}${path(top - i)}`, { tags: { name: "preflight" } });
    if (res.status === 200) return null;
    if (res.status !== 404) {
      return `${path(top - i)} -> ${res.status} (expected 200)`;
    }
  }
  return `the top three ids all 404 (${path(top)})`;
}
