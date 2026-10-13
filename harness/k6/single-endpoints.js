// k6 single-endpoint scenarios — the deliberate counterpoint to journeys.js:
// hammer ONE route to answer "how fast is this endpoint, alone". Mirrors the
// in-process generator's feed / write-heavy / profile / static scenarios.
//
// Run one at a time (or all — they are separate k6 scenarios):
//   k6 run harness/k6/single-endpoints.js
//   ONLY=profile USERS=30000 POSTS=300000 k6 run harness/k6/single-endpoints.js

import http from "k6/http";
import { sleep, check } from "k6";
import { Trend } from "k6/metrics";

const BASE = __ENV.BASE || "http://127.0.0.1:8113";
const USERS = Number(__ENV.USERS || 30000);
const POSTS = Number(__ENV.POSTS || USERS * 10);
const ONLY = (__ENV.ONLY || "").trim(); // "", or one of feed|naive|profile|writes|static

const serverTotal = new Trend("server_total_ms", true);

const scenario = (exec) => ({
  executor: "constant-vus",
  vus: Number(__ENV.VUS || 20),
  duration: __ENV.DURATION || "60s",
  exec,
});
const scenarios = {};
if (ONLY === "" || ONLY === "feed") scenarios.feed = scenario("feed");
if (ONLY === "" || ONLY === "naive") scenarios.feed_naive = scenario("feedNaive");
if (ONLY === "" || ONLY === "profile") scenarios.profile = scenario("profile");
if (ONLY === "" || ONLY === "writes") scenarios.writes = scenario("writes");
if (ONLY === "" || ONLY === "static") scenarios.static = scenario("staticPing");

export const options = {
  // 30k-VU runs keep hundreds of MB of in-flight bodies in memory otherwise;
  // the scripts only read status/headers, so drop bodies at the client.
  discardResponseBodies: true,
  scenarios,
  thresholds: {
    http_req_duration: ["p(95)<500", "p(99)<1000"],
    http_req_failed: ["rate<0.01"],
  },
};

const uid = () => 1 + Math.floor(Math.random() * USERS);
const pid = () => 1 + Math.floor(Math.random() * POSTS);

function call(method, path, user, body) {
  const url = `${BASE}${path}`;
  const params = {
    headers: { "x-bench-user": String(user) },
    tags: { name: path.replace(/\/\d+/g, "/{id}") },
  };
  const res =
    method === "POST" && body !== undefined
      ? http.post(url, JSON.stringify(body), { ...params, headers: { ...params.headers, "content-type": "application/json" } })
      : http[method.toLowerCase()](url, null, params);
  const key = Object.keys(res.headers).find((k) => k.toLowerCase() === "server-timing");
  if (key) {
    const m = /(?:^|,\s*)total;dur=([0-9.]+)/.exec(res.headers[key]);
    if (m) serverTotal.add(Number(m[1]));
  }
  check(res, { "status 2xx": (r) => r.status >= 200 && r.status < 300 });
  return res;
}

export function feed() {
  call("GET", "/api/feed", uid());
  sleep(0.01);
}
export function feedNaive() {
  call("GET", "/api/feed-naive?limit=25", uid());
  sleep(0.01);
}
export function profile() {
  call("GET", `/api/users/${uid()}/profile`, uid());
  sleep(0.01);
}
export function writes() {
  const user = uid();
  call("POST", "/api/posts", user, { body: `k6 post ${Date.now()}` });
  call("POST", `/api/posts/${pid()}/like`, user, {});
  sleep(0.01);
}
export function staticPing() {
  call("GET", "/api/ping", 1);
  sleep(0.01);
}
