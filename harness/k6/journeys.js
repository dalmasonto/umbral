// k6 journeys — stateful personas, the Arjay-style workload: each virtual
// user walks a scripted sequence (browse → linger → open a profile →
// occasionally write) instead of hammering one URL. Every request carries
// `x-bench-user`, and the server's `Server-Timing` header is collected as a
// custom trend so k6 reports the server-side time alongside the end-to-end
// one (their difference is generator + network overhead).
//
// Run:
//   k6 run harness/k6/journeys.js
//   BASE=http://127.0.0.1:8113 USERS=30000 READERS=30 WRITERS=6 LURKERS=10 \
//     DURATION=2m k6 run harness/k6/journeys.js
//
// (Single-endpoint hammering lives in single-endpoints.js.)

import http from "k6/http";
import { check, sleep } from "k6";
import { Trend } from "k6/metrics";

const BASE = __ENV.BASE || "http://127.0.0.1:8113";
const USERS = Number(__ENV.USERS || 30000);
const POSTS = Number(__ENV.POSTS || USERS * 10);

const serverTotal = new Trend("server_total_ms", true);
const serverDb = new Trend("server_db_ms", true);

// Personas mirror the in-process generator's journeys (src/loadgen.rs).
export const options = {
  // 30k-VU runs keep hundreds of MB of in-flight bodies in memory otherwise;
  // the scripts only read status/headers, so drop bodies at the client.
  discardResponseBodies: true,
  scenarios: {
    readers: {
      exec: "reader",
      executor: "constant-vus",
      vus: Number(__ENV.READERS || 30),
      duration: __ENV.DURATION || "60s",
    },
    writers: {
      exec: "writer",
      executor: "constant-vus",
      vus: Number(__ENV.WRITERS || 6),
      duration: __ENV.DURATION || "60s",
    },
    lurkers: {
      exec: "lurker",
      executor: "constant-vus",
      vus: Number(__ENV.LURKERS || 10),
      duration: __ENV.DURATION || "60s",
    },
  },
  thresholds: {
    http_req_duration: ["p(95)<500", "p(99)<1000"],
    http_req_failed: ["rate<0.01"],
  },
};

function uid() {
  return 1 + Math.floor(Math.random() * USERS);
}
function pid() {
  return 1 + Math.floor(Math.random() * POSTS);
}
function think(minMs, maxMs) {
  sleep((minMs + Math.random() * (maxMs - minMs)) / 1000);
}

function recordServerTiming(res) {
  const key = Object.keys(res.headers).find((k) => k.toLowerCase() === "server-timing");
  if (!key) return;
  const header = res.headers[key];
  const total = /(?:^|,\s*)total;dur=([0-9.]+)/.exec(header);
  if (total) serverTotal.add(Number(total[1]));
  const db = /,\s*db;dur=([0-9.]+)/.exec(header);
  if (db) serverDb.add(Number(db[1]));
}

function get(path, user) {
  const res = http.get(`${BASE}${path}`, {
    headers: { "x-bench-user": String(user) },
    tags: { name: path.replace(/\/\d+/g, "/{id}") },
  });
  recordServerTiming(res);
  check(res, { "status 2xx": (r) => r.status >= 200 && r.status < 300 });
  return res;
}

function postJson(path, user, body) {
  const res = http.post(`${BASE}${path}`, JSON.stringify(body), {
    headers: { "content-type": "application/json", "x-bench-user": String(user) },
    tags: { name: path.replace(/\/\d+/g, "/{id}") },
  });
  recordServerTiming(res);
  check(res, { "status 2xx": (r) => r.status >= 200 && r.status < 300 });
  return res;
}

// reader: browse the feed, open a profile, read the heavy aggregate.
export function reader() {
  const user = uid();
  think(400, 1800);
  get("/api/feed", user);
  think(600, 2600);
  get(`/u/${uid()}`, user);
  think(800, 3200);
  get(`/api/users/${uid()}/profile`, user);
  think(300, 1500);
  get("/api/feed?limit=10", user);
}

// writer: skim, post, like, move on.
export function writer() {
  const user = uid();
  think(500, 2000);
  get("/api/feed", user);
  think(700, 2500);
  postJson("/api/posts", user, { body: `k6 post ${Date.now()}` });
  think(250, 1200);
  postJson(`/api/posts/${pid()}/like`, user, {});
  think(200, 900);
  get(`/u/${uid()}`, user);
}

// lurker: purely consumes HTML pages.
export function lurker() {
  const user = uid();
  think(900, 4000);
  get("/", user);
  think(1200, 5000);
  get(`/u/${uid()}`, user);
  think(1000, 4000);
  get(`/posts/${pid()}`, user);
}
