#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const { spawn, execFile } = require("node:child_process");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);

function printHelp() {
  process.stdout.write(`Usage: node scripts/lsp-benchmark.js [options]

Options:
  --server <path>                                Path to LSP server binary
  --workspace <path>                             Workspace path to benchmark
  --output <path>                                Write benchmark JSON to file
  --fail-on-threshold                            Exit non-zero on threshold violations
  --completion-runs <number>                     Completion sample count (default: 60)
  --hover-runs <number>                          Hover sample count (default: 60)
  --warmup-runs <number>                         Warmup iterations (default: 10)
  --max-completion-p95-ms <number>               Absolute completion p95 threshold (default: 30)
  --max-hover-p95-ms <number>                    Absolute hover p95 threshold (default: 20)
  --max-index-ms <number>                        Absolute index duration threshold (default: env VSCODE_LS_PHP_INDEX_TARGET_MS or disabled)
  --index-timeout-ms <number>                    Index observation timeout (default: 30000)
  --baseline <path>                              Baseline benchmark JSON for regression deltas
  --max-completion-p95-regression-ms <number>    Allowed completion p95 regression vs baseline (default: 5)
  --max-hover-p95-regression-ms <number>         Allowed hover p95 regression vs baseline (default: 5)
  --max-index-regression-ms <number>             Allowed index duration regression vs baseline (default: 1000)
  --require-baseline                             Fail when baseline cannot be loaded
  --help                                         Show this help text
`);
}

function parseArgs(argv) {
  const result = {
    server: null,
    workspace: null,
    output: null,
    baseline: null,
    failOnThreshold: false,
    requireBaseline: false,
    help: false,
    completionRuns: 60,
    hoverRuns: 60,
    warmupRuns: 10,
    maxCompletionP95Ms: 30,
    maxHoverP95Ms: 20,
    maxCompletionP95RegressionMs: 5,
    maxHoverP95RegressionMs: 5,
    maxIndexRegressionMs: 1000,
    maxIndexMs: process.env.VSCODE_LS_PHP_INDEX_TARGET_MS
      ? Number(process.env.VSCODE_LS_PHP_INDEX_TARGET_MS)
      : 0,
    indexTimeoutMs: 30000,
  };

  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--fail-on-threshold") {
      result.failOnThreshold = true;
      continue;
    }
    if (arg === "--require-baseline") {
      result.requireBaseline = true;
      continue;
    }
    if (arg === "--help") {
      result.help = true;
      continue;
    }
    const next = argv[i + 1];
    if (!next || next.startsWith("--")) {
      throw new Error(`Missing value for ${arg}`);
    }
    i += 1;

    switch (arg) {
      case "--server":
        result.server = next;
        break;
      case "--workspace":
        result.workspace = next;
        break;
      case "--output":
        result.output = next;
        break;
      case "--baseline":
        result.baseline = next;
        break;
      case "--completion-runs":
        result.completionRuns = Number(next);
        break;
      case "--hover-runs":
        result.hoverRuns = Number(next);
        break;
      case "--warmup-runs":
        result.warmupRuns = Number(next);
        break;
      case "--max-completion-p95-ms":
        result.maxCompletionP95Ms = Number(next);
        break;
      case "--max-hover-p95-ms":
        result.maxHoverP95Ms = Number(next);
        break;
      case "--max-index-ms":
        result.maxIndexMs = Number(next);
        break;
      case "--index-timeout-ms":
        result.indexTimeoutMs = Number(next);
        break;
      case "--max-completion-p95-regression-ms":
        result.maxCompletionP95RegressionMs = Number(next);
        break;
      case "--max-hover-p95-regression-ms":
        result.maxHoverP95RegressionMs = Number(next);
        break;
      case "--max-index-regression-ms":
        result.maxIndexRegressionMs = Number(next);
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return result;
}

function toFileUri(filePath) {
  const resolved = path.resolve(filePath).replace(/\\/g, "/");
  return `file://${resolved.startsWith("/") ? "" : "/"}${resolved}`;
}

function percentile(values, p) {
  if (!values.length) {
    return 0;
  }
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

function avg(values) {
  if (!values.length) {
    return 0;
  }
  return values.reduce((acc, cur) => acc + cur, 0) / values.length;
}

function readBaselineMetric(raw, getter, label) {
  const value = getter(raw);
  if (!Number.isFinite(value)) {
    throw new Error(`Baseline missing numeric ${label}`);
  }
  return value;
}

function loadBaseline(baselinePath) {
  if (!baselinePath) {
    return {
      loaded: false,
      path: null,
      message: "No baseline path provided; skipping delta checks.",
      baseline: null,
    };
  }

  const resolvedPath = path.resolve(baselinePath);
  try {
    const raw = JSON.parse(fs.readFileSync(resolvedPath, "utf8"));
    const baseline = {
      completionP95Ms: readBaselineMetric(raw, (v) => v && v.completion && v.completion.p95Ms, "completion.p95Ms"),
      hoverP95Ms: readBaselineMetric(raw, (v) => v && v.hover && v.hover.p95Ms, "hover.p95Ms"),
      indexDurationMs: readBaselineMetric(raw, (v) => v && v.index && v.index.durationMs, "index.durationMs"),
    };
    return {
      loaded: true,
      path: resolvedPath,
      message: `Loaded baseline from ${resolvedPath}.`,
      baseline,
    };
  } catch (error) {
    return {
      loaded: false,
      path: resolvedPath,
      message: `Unable to load baseline from ${resolvedPath}: ${error.message}`,
      baseline: null,
    };
  }
}

function computeDelta(metrics, baselineState, args) {
  const delta = {
    baselinePath: baselineState.path,
    baselineLoaded: baselineState.loaded,
    message: baselineState.message,
    completionP95Ms: null,
    hoverP95Ms: null,
    indexDurationMs: null,
    thresholds: {
      maxCompletionP95RegressionMs: args.maxCompletionP95RegressionMs,
      maxHoverP95RegressionMs: args.maxHoverP95RegressionMs,
      maxIndexRegressionMs: args.maxIndexRegressionMs,
    },
  };

  if (!baselineState.loaded || !baselineState.baseline) {
    return delta;
  }

  delta.completionP95Ms = Number(
    (metrics.completion.p95Ms - baselineState.baseline.completionP95Ms).toFixed(3)
  );
  delta.hoverP95Ms = Number((metrics.hover.p95Ms - baselineState.baseline.hoverP95Ms).toFixed(3));
  delta.indexDurationMs = Number(
    (metrics.index.durationMs - baselineState.baseline.indexDurationMs).toFixed(3)
  );
  return delta;
}

function collectFailures(metrics, args, delta) {
  const failures = [];
  if (metrics.completion.p95Ms > args.maxCompletionP95Ms) {
    failures.push(
      `completion p95 ${metrics.completion.p95Ms}ms > ${args.maxCompletionP95Ms}ms`
    );
  }
  if (metrics.hover.p95Ms > args.maxHoverP95Ms) {
    failures.push(`hover p95 ${metrics.hover.p95Ms}ms > ${args.maxHoverP95Ms}ms`);
  }
  if (args.maxIndexMs > 0 && metrics.index.durationMs > args.maxIndexMs) {
    failures.push(`index duration ${metrics.index.durationMs}ms > ${args.maxIndexMs}ms`);
  }

  if (delta.baselineLoaded) {
    if (
      Number.isFinite(delta.completionP95Ms)
      && delta.completionP95Ms > args.maxCompletionP95RegressionMs
    ) {
      failures.push(
        `completion p95 regression ${delta.completionP95Ms}ms > ${args.maxCompletionP95RegressionMs}ms`
      );
    }
    if (Number.isFinite(delta.hoverP95Ms) && delta.hoverP95Ms > args.maxHoverP95RegressionMs) {
      failures.push(
        `hover p95 regression ${delta.hoverP95Ms}ms > ${args.maxHoverP95RegressionMs}ms`
      );
    }
    if (Number.isFinite(delta.indexDurationMs) && delta.indexDurationMs > args.maxIndexRegressionMs) {
      failures.push(
        `index duration regression ${delta.indexDurationMs}ms > ${args.maxIndexRegressionMs}ms`
      );
    }
  }

  return failures;
}

async function getProcessRssBytes(pid) {
  try {
    if (process.platform === "win32") {
      const { stdout } = await execFileAsync("powershell", [
        "-NoProfile",
        "-Command",
        `(Get-Process -Id ${pid}).WorkingSet64`,
      ]);
      const parsed = Number(String(stdout).trim());
      return Number.isFinite(parsed) ? parsed : null;
    }

    const { stdout } = await execFileAsync("ps", ["-o", "rss=", "-p", String(pid)]);
    const kb = Number(String(stdout).trim());
    if (!Number.isFinite(kb)) {
      return null;
    }
    return kb * 1024;
  } catch {
    return null;
  }
}

class LspClient {
  constructor(command, args, cwd) {
    this.command = command;
    this.args = args;
    this.cwd = cwd;
    this.child = null;
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.logListeners = [];
  }

  async start() {
    this.child = spawn(this.command, this.args, {
      cwd: this.cwd,
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });

    this.child.stdout.on("data", (chunk) => this._onData(chunk));
    this.child.stderr.on("data", (chunk) => {
      const text = chunk.toString("utf8").trim();
      if (text.length > 0) {
        process.stderr.write(`[server-stderr] ${text}\n`);
      }
    });
    this.child.on("exit", (code, signal) => {
      const err = new Error(`Server exited unexpectedly (code=${code}, signal=${signal})`);
      for (const entry of this.pending.values()) {
        entry.reject(err);
      }
      this.pending.clear();
    });
  }

  onLogMessage(listener) {
    this.logListeners.push(listener);
  }

  async request(method, params) {
    const id = this.nextId;
    this.nextId += 1;

    const payload = { jsonrpc: "2.0", id, method, params };
    const responsePromise = new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });

    this._write(payload);
    return responsePromise;
  }

  notify(method, params) {
    this._write({ jsonrpc: "2.0", method, params });
  }

  async stop() {
    try {
      await this.request("shutdown", null);
    } catch {
      // Ignore failures during shutdown phase.
    }

    try {
      this.notify("exit", null);
    } catch {
      // Ignore failures during shutdown phase.
    }

    if (this.child && !this.child.killed) {
      this.child.kill();
    }
  }

  _write(message) {
    const json = JSON.stringify(message);
    const bytes = Buffer.from(json, "utf8");
    const header = Buffer.from(`Content-Length: ${bytes.length}\r\n\r\n`, "ascii");
    this.child.stdin.write(Buffer.concat([header, bytes]));
  }

  _onData(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);

    while (true) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) {
        return;
      }

      const header = this.buffer.slice(0, headerEnd).toString("ascii");
      const match = header.match(/Content-Length:\s*(\d+)/i);
      if (!match) {
        throw new Error("Invalid LSP header without Content-Length");
      }
      const length = Number(match[1]);
      const total = headerEnd + 4 + length;
      if (this.buffer.length < total) {
        return;
      }

      const body = this.buffer.slice(headerEnd + 4, total).toString("utf8");
      this.buffer = this.buffer.slice(total);

      const message = JSON.parse(body);
      this._dispatch(message);
    }
  }

  _dispatch(message) {
    if (Object.prototype.hasOwnProperty.call(message, "id")) {
      const pending = this.pending.get(message.id);
      if (!pending) {
        return;
      }
      this.pending.delete(message.id);
      if (message.error) {
        pending.reject(new Error(JSON.stringify(message.error)));
        return;
      }
      pending.resolve(message.result);
      return;
    }

    if (message.method === "window/logMessage") {
      const logText = message.params && message.params.message ? String(message.params.message) : "";
      for (const listener of this.logListeners) {
        listener(logText);
      }
    }
  }
}

async function run() {
  const args = parseArgs(process.argv);
  if (args.help) {
    printHelp();
    return;
  }
  const repoRoot = path.resolve(__dirname, "..");

  const binaryName = process.platform === "win32" ? "vscode-ls-php-server.exe" : "vscode-ls-php-server";
  const releaseServer = path.resolve(repoRoot, "server", "target", "release", binaryName);
  const debugServer = path.resolve(repoRoot, "server", "target", "debug", binaryName);
  const defaultServer = fs.existsSync(releaseServer) ? releaseServer : debugServer;
  const serverPath = path.resolve(args.server || defaultServer);
  const workspacePath = path.resolve(args.workspace || path.join(repoRoot, "sample-php-project"));

  if (!fs.existsSync(serverPath)) {
    throw new Error(`Server binary not found: ${serverPath}`);
  }
  if (!fs.existsSync(workspacePath)) {
    throw new Error(`Workspace path not found: ${workspacePath}`);
  }

  const benchmarkText = [
    "<?php",
    "namespace App\\Bench;",
    "",
    "function benchTarget(int $value, string $name): string {",
    "    return (string)$value;",
    "}",
    "",
    "$foo = 1;",
    "ben",
    "strlen(\"abc\");",
    "",
  ].join("\n");

  const benchmarkFileName = `vscode_ls_php_bench_${process.pid}_${Date.now()}.php`;
  const benchmarkUri = toFileUri(path.join(workspacePath, "storage", "framework", benchmarkFileName));
  const workspaceUri = toFileUri(workspacePath);

  const command = serverPath;
  const commandArgs = [];
  const client = new LspClient(command, commandArgs, repoRoot);

  await client.start();
  const pid = client.child && client.child.pid ? client.child.pid : null;

  const initializedAt = Date.now();
  let indexedAt = null;
  let indexedFiles = null;

  client.onLogMessage((line) => {
    const lowered = line.toLowerCase();
    if (lowered.includes("indexed") && lowered.includes("php files") && indexedAt === null) {
      indexedAt = Date.now();
      const m = line.match(/indexed\s+(\d+)\s+php files/i);
      if (m) {
        indexedFiles = Number(m[1]);
      }
    }
  });

  await client.request("initialize", {
    processId: process.pid,
    clientInfo: { name: "vscode-ls-php-bench", version: "1.0.0" },
    rootUri: workspaceUri,
    workspaceFolders: [{ uri: workspaceUri, name: path.basename(workspacePath) }],
    capabilities: {},
    trace: "off",
  });
  client.notify("initialized", {});

  const indexStart = Date.now();
  while (indexedAt === null && Date.now() - indexStart < args.indexTimeoutMs) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const fallbackIndexDurationMs = Date.now() - initializedAt;

  client.notify("textDocument/didOpen", {
    textDocument: {
      uri: benchmarkUri,
      languageId: "php",
      version: 1,
      text: benchmarkText,
    },
  });

  const rssStart = pid ? await getProcessRssBytes(pid) : null;

  async function requestDurationMs(method, params) {
    const start = process.hrtime.bigint();
    await client.request(method, params);
    const end = process.hrtime.bigint();
    return Number(end - start) / 1_000_000;
  }

  const completionParams = {
    textDocument: { uri: benchmarkUri },
    position: { line: 8, character: 3 },
    context: { triggerKind: 1 },
  };

  const hoverParams = {
    textDocument: { uri: benchmarkUri },
    position: { line: 9, character: 2 },
  };

  for (let i = 0; i < args.warmupRuns; i += 1) {
    await requestDurationMs("textDocument/completion", completionParams);
    await requestDurationMs("textDocument/hover", hoverParams);
  }

  const completionSamples = [];
  for (let i = 0; i < args.completionRuns; i += 1) {
    completionSamples.push(await requestDurationMs("textDocument/completion", completionParams));
  }

  const hoverSamples = [];
  for (let i = 0; i < args.hoverRuns; i += 1) {
    hoverSamples.push(await requestDurationMs("textDocument/hover", hoverParams));
  }

  const rssEnd = pid ? await getProcessRssBytes(pid) : null;

  const metrics = {
    timestamp: new Date().toISOString(),
    serverPath,
    workspacePath,
    samples: {
      completion: completionSamples.length,
      hover: hoverSamples.length,
      warmup: args.warmupRuns,
    },
    index: {
      durationMs: indexedAt === null ? fallbackIndexDurationMs : indexedAt - initializedAt,
      timedOut: indexedAt === null,
      indexedFiles,
    },
    completion: {
      avgMs: Number(avg(completionSamples).toFixed(3)),
      p95Ms: Number(percentile(completionSamples, 95).toFixed(3)),
      maxMs: Number(Math.max(...completionSamples).toFixed(3)),
    },
    hover: {
      avgMs: Number(avg(hoverSamples).toFixed(3)),
      p95Ms: Number(percentile(hoverSamples, 95).toFixed(3)),
      maxMs: Number(Math.max(...hoverSamples).toFixed(3)),
    },
    rss: {
      startBytes: rssStart,
      endBytes: rssEnd,
      maxBytes:
        rssStart !== null || rssEnd !== null
          ? Math.max(rssStart || 0, rssEnd || 0)
          : null,
    },
    thresholds: {
      maxCompletionP95Ms: args.maxCompletionP95Ms,
      maxHoverP95Ms: args.maxHoverP95Ms,
      maxIndexMs: args.maxIndexMs > 0 ? args.maxIndexMs : null,
      maxCompletionP95RegressionMs: args.maxCompletionP95RegressionMs,
      maxHoverP95RegressionMs: args.maxHoverP95RegressionMs,
      maxIndexRegressionMs: args.maxIndexRegressionMs,
    },
  };

  const baselineState = loadBaseline(args.baseline);
  if (!baselineState.loaded && args.baseline) {
    process.stderr.write(`${baselineState.message}\n`);
  }

  metrics.delta = computeDelta(metrics, baselineState, args);

  const failures = collectFailures(metrics, args, metrics.delta);
  let baselineRequirementFailure = null;
  if (args.requireBaseline && !baselineState.loaded) {
    baselineRequirementFailure = `baseline is required but could not be loaded (${baselineState.path || "no path"})`;
  }

  const outputJson = `${JSON.stringify(metrics, null, 2)}\n`;
  if (args.output) {
    const outputPath = path.resolve(args.output);
    fs.mkdirSync(path.dirname(outputPath), { recursive: true });
    fs.writeFileSync(outputPath, outputJson, "utf8");
  }

  process.stdout.write(outputJson);

  await client.stop();

  if (args.failOnThreshold && failures.length > 0) {
    process.stderr.write(`Performance gate failed: ${failures.join("; ")}\n`);
    process.exit(2);
  }
  if (baselineRequirementFailure) {
    process.stderr.write(`Performance gate failed: ${baselineRequirementFailure}\n`);
    process.exit(2);
  }
}

if (require.main === module) {
  run().catch((error) => {
    process.stderr.write(`${error.stack || error.message}\n`);
    process.exit(1);
  });
}

module.exports = {
  parseArgs,
  loadBaseline,
  computeDelta,
  collectFailures,
};
