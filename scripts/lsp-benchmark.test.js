const test = require("node:test");
const assert = require("node:assert/strict");

const {
  parseArgs,
  loadBaseline,
  computeDelta,
  collectFailures,
} = require("./lsp-benchmark");

test("parseArgs supports baseline regression flags", () => {
  const args = parseArgs([
    "node",
    "scripts/lsp-benchmark.js",
    "--baseline",
    "scripts/benchmark-baseline.json",
    "--max-completion-p95-regression-ms",
    "7",
    "--max-hover-p95-regression-ms",
    "8",
    "--max-index-regression-ms",
    "900",
    "--require-baseline",
  ]);

  assert.equal(args.baseline, "scripts/benchmark-baseline.json");
  assert.equal(args.maxCompletionP95RegressionMs, 7);
  assert.equal(args.maxHoverP95RegressionMs, 8);
  assert.equal(args.maxIndexRegressionMs, 900);
  assert.equal(args.requireBaseline, true);
  assert.equal(args.failOnThreshold, false);
});

test("computeDelta calculates metric regressions from baseline", () => {
  const metrics = {
    completion: { p95Ms: 32 },
    hover: { p95Ms: 21 },
    index: { durationMs: 4200 },
  };
  const baselineState = {
    loaded: true,
    path: "baseline.json",
    message: "loaded",
    baseline: {
      completionP95Ms: 25,
      hoverP95Ms: 17,
      indexDurationMs: 3500,
    },
  };
  const args = {
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 5,
    maxIndexRegressionMs: 1000,
  };

  const delta = computeDelta(metrics, baselineState, args);
  assert.equal(delta.completionP95Ms, 7);
  assert.equal(delta.hoverP95Ms, 4);
  assert.equal(delta.indexDurationMs, 700);
});

test("computeDelta includes null deltas and metadata when baseline not loaded", () => {
  const metrics = {
    completion: { p95Ms: 12 },
    hover: { p95Ms: 8 },
    index: { durationMs: 1200 },
  };
  const baselineState = {
    loaded: false,
    path: "missing.json",
    message: "Unable to load baseline",
    baseline: null,
  };
  const args = {
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 5,
    maxIndexRegressionMs: 1000,
  };

  const delta = computeDelta(metrics, baselineState, args);
  assert.equal(delta.baselinePath, "missing.json");
  assert.equal(delta.baselineLoaded, false);
  assert.equal(delta.message, "Unable to load baseline");
  assert.equal(delta.completionP95Ms, null);
  assert.equal(delta.hoverP95Ms, null);
  assert.equal(delta.indexDurationMs, null);
  assert.deepEqual(delta.thresholds, {
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 5,
    maxIndexRegressionMs: 1000,
  });
});

test("collectFailures includes baseline regression threshold failures", () => {
  const metrics = {
    completion: { p95Ms: 29 },
    hover: { p95Ms: 19 },
    index: { durationMs: 4900 },
  };
  const args = {
    maxCompletionP95Ms: 30,
    maxHoverP95Ms: 20,
    maxIndexMs: 5000,
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 5,
    maxIndexRegressionMs: 1000,
  };
  const delta = {
    baselineLoaded: true,
    completionP95Ms: 6,
    hoverP95Ms: 2,
    indexDurationMs: 1500,
  };

  const failures = collectFailures(metrics, args, delta);
  assert.deepEqual(failures, [
    "completion p95 regression 6ms > 5ms",
    "index duration regression 1500ms > 1000ms",
  ]);
});

test("collectFailures enforces absolute thresholds and regression thresholds together", () => {
  const metrics = {
    completion: { p95Ms: 31 },
    hover: { p95Ms: 25 },
    index: { durationMs: 5200 },
  };
  const args = {
    maxCompletionP95Ms: 30,
    maxHoverP95Ms: 20,
    maxIndexMs: 5000,
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 3,
    maxIndexRegressionMs: 1000,
  };
  const delta = {
    baselineLoaded: true,
    completionP95Ms: 6,
    hoverP95Ms: 4,
    indexDurationMs: 1500,
  };

  const failures = collectFailures(metrics, args, delta);
  assert.deepEqual(failures, [
    "completion p95 31ms > 30ms",
    "hover p95 25ms > 20ms",
    "index duration 5200ms > 5000ms",
    "completion p95 regression 6ms > 5ms",
    "hover p95 regression 4ms > 3ms",
    "index duration regression 1500ms > 1000ms",
  ]);
});

test("collectFailures skips regression checks when baseline is not loaded", () => {
  const metrics = {
    completion: { p95Ms: 25 },
    hover: { p95Ms: 15 },
    index: { durationMs: 3000 },
  };
  const args = {
    maxCompletionP95Ms: 30,
    maxHoverP95Ms: 20,
    maxIndexMs: 5000,
    maxCompletionP95RegressionMs: 0,
    maxHoverP95RegressionMs: 0,
    maxIndexRegressionMs: 0,
  };
  const delta = {
    baselineLoaded: false,
    completionP95Ms: 100,
    hoverP95Ms: 100,
    indexDurationMs: 100,
  };

  const failures = collectFailures(metrics, args, delta);
  assert.deepEqual(failures, []);
});

test("loadBaseline returns not loaded state for missing file", () => {
  const baseline = loadBaseline("scripts/not-a-real-baseline.json");
  assert.equal(baseline.loaded, false);
  assert.equal(baseline.baseline, null);
  assert.match(baseline.message, /Unable to load baseline/);
});
