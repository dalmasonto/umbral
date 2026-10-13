// Challenge phase 1 — the unscored warm-up.
//
// Runs the full workload at 1,000 VUs for a 60 s ramp plus a 2-minute hold, to
// fill the connection pool, the SQLite page cache, and the session store
// before anything is measured. A hold started on a cold server measures
// warm-up, not throughput.
//
// It is `scored: false`, so no thresholds apply and k6 exits 0 whatever the
// summary says: a warm-up is not a verdict. It is also the first script in the
// sequence, so its `preflight()` is where a mis-scoped id range or a pool built
// against the wrong database gets caught — for the price of a one-second abort
// instead of a failed hold six minutes later.
//
// There is deliberately no extra pacing here. `doIteration` already sleeps the
// workload's 3–8 s of think time, and a caller adding a `sleep` of its own is
// how the warm-up quietly becomes a different (lighter) workload from the hold
// it is warming for — the drift `lib.js` exists to prevent.
//
// Run: taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 \
//        harness/bench/k6/warmup.js

import { SharedArray } from "k6/data";
import { doIteration, buildOptions, preflight } from "./lib.js";

const sessions = new SharedArray("sessions", () =>
  JSON.parse(open("../session_pool.json")),
);

export const options = buildOptions({ vus: 1000, duration: "2m", scored: false });

export function setup() {
  return preflight(sessions);
}

export default function () {
  doIteration(sessions);
}
