// Challenge phase 3 — the scored 5-minute hold.
//
// Ramp to VUS over 60 s, hold for 5 minutes, ramp down for 30 s — a 6.5-minute
// wall clock, and the run-level numbers in the summary span all three stages,
// not just the hold. Encodes the pass criteria as k6 thresholds, so the exit
// code IS the verdict (0 pass, 99 on a failed threshold) and a `--summary-export`
// of the same run is what Task 8's recorder files.
//
// The VU count is a parameter, never a choice: it is whatever `search.sh`
// discovered. 2,500 is the challenge's starting point, useful as a first data
// point and as the same starting point the search doubles from.
//
// Run: taskset -c 8,9,10,11 ~/.local/bin/k6 run -e VUS=2500 \
//        --summary-export=bench/results/raw.json harness/bench/k6/hold.js

import { SharedArray } from "k6/data";
import { doIteration, buildOptions, preflight } from "./lib.js";

const sessions = new SharedArray("sessions", () =>
  JSON.parse(open("../session_pool.json")),
);

const VUS = Number(__ENV.VUS || 2500);

export const options = buildOptions({ vus: VUS, duration: "5m", scored: true });

// A run that cannot mean what it claims should not be paid for: `preflight`
// aborts (throwing here fails the run before the first VU, with the fix in the
// message) when the id ranges run past the served dataset, or when the mix
// writes but the pooled session does not authenticate against the database the
// silo is serving. What it returns lands in the summary's `setup_data` — the
// run's own record of the mix and scale it used.
export function setup() {
  return preflight(sessions);
}

export default function () {
  doIteration(sessions);
}
