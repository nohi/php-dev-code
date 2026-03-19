import * as path from "node:path";
import { spawn } from "node:child_process";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  Middleware,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let testController: vscode.TestController | undefined;
let continuousWatcher: vscode.FileSystemWatcher | undefined;

type KnownFramework = "phpunit" | "pest" | "unknown";

type TestMetadata = {
  uri: vscode.Uri;
  framework: KnownFramework;
  kind: "file" | "case";
  name?: string;
};

type ProcessResult = {
  ok: boolean;
  output: string;
  durationMs: number;
  command: string;
};

type RequestMetric = {
  count: number;
  totalMs: number;
  maxMs: number;
  lastMs: number;
};

type TestRunMetric = {
  label: string;
  ok: boolean;
  durationMs: number;
};

const itemMetadata = new WeakMap<vscode.TestItem, TestMetadata>();
const requestMetrics = new Map<string, RequestMetric>();
const recentTestRuns: TestRunMetric[] = [];

function rememberTestRun(label: string, ok: boolean, durationMs: number): void {
  recentTestRuns.push({ label, ok, durationMs });
  while (recentTestRuns.length > 30) {
    recentTestRuns.shift();
  }
}

function recordRequestMetric(name: string, durationMs: number): void {
  const current = requestMetrics.get(name) ?? {
    count: 0,
    totalMs: 0,
    maxMs: 0,
    lastMs: 0,
  };
  current.count += 1;
  current.totalMs += durationMs;
  current.maxMs = Math.max(current.maxMs, durationMs);
  current.lastMs = durationMs;
  requestMetrics.set(name, current);
}

async function withProfileMetric<T>(name: string, task: () => Promise<T>): Promise<T> {
  const started = Date.now();
  try {
    return await task();
  } finally {
    recordRequestMetric(name, Date.now() - started);
  }
}

function serverBinaryName(): string {
  return process.platform === "win32" ? "vscode-ls-php-server.exe" : "vscode-ls-php-server";
}

function defaultServerPath(context: vscode.ExtensionContext): string {
  const rel = path.join("..", "server", "target", "debug", serverBinaryName());
  return path.resolve(context.extensionPath, rel);
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration("vscodeLsPhp");
  const configuredPath = config.get<string>("serverPath")?.trim();
  const command = configuredPath && configuredPath.length > 0
    ? configuredPath
    : defaultServerPath(context);

  const serverOptions: ServerOptions = {
    run: { command, transport: TransportKind.stdio },
    debug: { command, transport: TransportKind.stdio },
  };

  const middleware: Middleware = {
    provideCompletionItem: (document, position, context, token, next) => {
      return withProfileMetric("completion", () => Promise.resolve(next(document, position, context, token)));
    },
    provideHover: (document, position, token, next) => {
      return withProfileMetric("hover", () => Promise.resolve(next(document, position, token)));
    },
    provideDefinition: (document, position, token, next) => {
      return withProfileMetric("definition", () => Promise.resolve(next(document, position, token)));
    },
    provideReferences: (document, position, context, token, next) => {
      return withProfileMetric("references", () => Promise.resolve(next(document, position, context, token)));
    },
    provideSignatureHelp: (document, position, context, token, next) => {
      return withProfileMetric("signatureHelp", () => Promise.resolve(next(document, position, context, token)));
    },
    provideRenameEdits: (document, position, newName, token, next) => {
      return withProfileMetric("rename", () => Promise.resolve(next(document, position, newName, token)));
    },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "php" },
      { scheme: "file", language: "blade" },
      { scheme: "file", language: "php", pattern: "**/*.blade.php" },
    ],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.{php,blade.php}"),
    },
    middleware,
  };

  client = new LanguageClient(
    "vscode-ls-php",
    "VSCode LS PHP",
    serverOptions,
    clientOptions,
  );

  await client.start();
}

async function stopClient(): Promise<void> {
  if (!client) {
    return;
  }
  const current = client;
  client = undefined;
  await current.stop();
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  initializeTestExplorer(context);

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.restartServer", async () => {
      await stopClient();
      await startClient(context);
      void vscode.window.showInformationMessage("VSCode LS PHP server restarted.");
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.launchXdebugSession", async () => {
      await launchXdebugSession();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.writeXdebugTemplate", async () => {
      await writeXdebugTemplate();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.refreshTests", async () => {
      await refreshWorkspaceTests();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.startContinuousTesting", async () => {
      await startContinuousTesting(context);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.stopContinuousTesting", async () => {
      stopContinuousTesting();
      void vscode.window.showInformationMessage("VSCode LS PHP continuous testing stopped.");
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.openProfilingDashboard", async () => {
      openProfilingDashboard();
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("vscode-ls-php.resetProfilingMetrics", async () => {
      requestMetrics.clear();
      recentTestRuns.length = 0;
      void vscode.window.showInformationMessage("VSCode LS PHP profiling metrics reset.");
    }),
  );

  await startClient(context);
  await refreshWorkspaceTests();
}

export async function deactivate(): Promise<void> {
  stopContinuousTesting();
  if (testController) {
    testController.dispose();
    testController = undefined;
  }
  await stopClient();
}

function initializeTestExplorer(context: vscode.ExtensionContext): void {
  const controller = vscode.tests.createTestController("vscodeLsPhpTests", "VSCode LS PHP Tests");
  controller.resolveHandler = async (item) => {
    if (!item) {
      await refreshWorkspaceTests();
    }
  };

  controller.createRunProfile(
    "Run PHPUnit/Pest",
    vscode.TestRunProfileKind.Run,
    async (request, token) => {
      await executeTestRun(request, false, token);
    },
    true,
    undefined,
    true,
  );

  controller.createRunProfile(
    "Debug PHPUnit/Pest",
    vscode.TestRunProfileKind.Debug,
    async (request, token) => {
      await executeTestRun(request, true, token);
    },
    false,
  );

  testController = controller;
  context.subscriptions.push(controller);
}

async function refreshWorkspaceTests(): Promise<void> {
  if (!testController) {
    return;
  }

  testController.items.replace([]);
  const files = await vscode.workspace.findFiles("**/tests/**/*.php", "**/{vendor,node_modules,.git}/**");

  for (const file of files) {
    const workspaceFolder = vscode.workspace.getWorkspaceFolder(file);
    const root = workspaceFolder ? workspaceFolder.uri.fsPath : "";
    const relativePath = root ? path.relative(root, file.fsPath).replace(/\\/g, "/") : file.fsPath;
    const fileItem = testController.createTestItem(file.toString(), relativePath, file);
    const parsed = await parseTestsInFile(file);
    const framework = inferFrameworkFromTests(parsed);
    itemMetadata.set(fileItem, { uri: file, framework, kind: "file" });

    for (const testCase of parsed) {
      const caseId = `${file.toString()}::${testCase.name}`;
      const caseItem = testController.createTestItem(caseId, testCase.name, file);
      if (typeof testCase.line === "number") {
        caseItem.range = new vscode.Range(testCase.line, 0, testCase.line, 120);
      }
      itemMetadata.set(caseItem, {
        uri: file,
        framework: testCase.framework,
        kind: "case",
        name: testCase.name,
      });
      fileItem.children.add(caseItem);
    }

    testController.items.add(fileItem);
  }
}

type ParsedCase = { name: string; framework: KnownFramework; line?: number };

async function parseTestsInFile(uri: vscode.Uri): Promise<ParsedCase[]> {
  try {
    const bytes = await vscode.workspace.fs.readFile(uri);
    const text = Buffer.from(bytes).toString("utf8");
    const parsed: ParsedCase[] = [];

    const phpunitRegex = /function\s+(test[A-Za-z0-9_]+)\s*\(/g;
    let match: RegExpExecArray | null;
    while ((match = phpunitRegex.exec(text)) !== null) {
      parsed.push({
        name: match[1],
        framework: "phpunit",
        line: offsetToLine(text, match.index),
      });
    }

    const attrRegex = /#\[Test\][\s\r\n]*public\s+function\s+([A-Za-z0-9_]+)\s*\(/g;
    while ((match = attrRegex.exec(text)) !== null) {
      parsed.push({
        name: match[1],
        framework: "phpunit",
        line: offsetToLine(text, match.index),
      });
    }

    const pestRegex = /(?:^|\s)(?:it|test)\(\s*["'`](.+?)["'`]/gm;
    while ((match = pestRegex.exec(text)) !== null) {
      parsed.push({
        name: match[1],
        framework: "pest",
        line: offsetToLine(text, match.index),
      });
    }

    const uniq = new Map<string, ParsedCase>();
    for (const item of parsed) {
      uniq.set(`${item.framework}:${item.name}`, item);
    }
    return Array.from(uniq.values()).sort((a, b) => a.name.localeCompare(b.name));
  } catch {
    return [];
  }
}

function inferFrameworkFromTests(tests: ParsedCase[]): KnownFramework {
  if (tests.some((t) => t.framework === "pest")) {
    return "pest";
  }
  if (tests.some((t) => t.framework === "phpunit")) {
    return "phpunit";
  }
  return "unknown";
}

function offsetToLine(text: string, offset: number): number {
  return text.slice(0, offset).split("\n").length - 1;
}

async function executeTestRun(
  request: vscode.TestRunRequest,
  debugMode: boolean,
  token: vscode.CancellationToken,
): Promise<void> {
  if (!testController) {
    return;
  }

  const run = testController.createTestRun(request, debugMode ? "Debug" : "Run");
  const items = collectRequestedItems(request);

  for (const item of items) {
    if (token.isCancellationRequested) {
      run.skipped(item);
      continue;
    }

    run.started(item);
    const metadata = itemMetadata.get(item);
    if (!metadata) {
      run.skipped(item);
      continue;
    }

    try {
      const result = await runSingleTestTarget(metadata, debugMode, token);
      const ms = result.durationMs;
      rememberTestRun(item.label, result.ok, ms);
      run.appendOutput(`\n[${result.ok ? "PASS" : "FAIL"}] ${item.label}\n${result.command}\n${result.output}\n`);
      if (result.ok) {
        run.passed(item, ms);
      } else {
        run.failed(item, new vscode.TestMessage(result.output || "Test command failed"), ms);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      run.failed(item, new vscode.TestMessage(message));
    }
  }

  run.end();
}

function collectRequestedItems(request: vscode.TestRunRequest): vscode.TestItem[] {
  const collected: vscode.TestItem[] = [];
  const seen = new Set<string>();

  const add = (item: vscode.TestItem) => {
    if (seen.has(item.id)) {
      return;
    }
    seen.add(item.id);
    if (item.children.size === 0) {
      collected.push(item);
      return;
    }
    item.children.forEach((child) => add(child));
  };

  if (request.include && request.include.length > 0) {
    for (const item of request.include) {
      add(item);
    }
  } else if (testController) {
    testController.items.forEach((item) => add(item));
  }

  const excluded = new Set((request.exclude ?? []).map((i) => i.id));
  return collected.filter((item) => !excluded.has(item.id));
}

async function runSingleTestTarget(
  metadata: TestMetadata,
  debugMode: boolean,
  token: vscode.CancellationToken,
): Promise<ProcessResult> {
  const folder = vscode.workspace.getWorkspaceFolder(metadata.uri);
  if (!folder) {
    return {
      ok: false,
      output: "Unable to resolve workspace folder for test item.",
      durationMs: 0,
      command: "",
    };
  }

  const config = vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri);
  const phpExecutable = config.get<string>("phpExecutable", "php");
  const phpunitBinary = config.get<string>("phpunitBinary", "vendor/bin/phpunit");
  const pestBinary = config.get<string>("pestBinary", "vendor/bin/pest");

  const phpunitUri = vscode.Uri.joinPath(folder.uri, phpunitBinary);
  const pestUri = vscode.Uri.joinPath(folder.uri, pestBinary);
  const phpunitExists = await fileExists(phpunitUri);
  const pestExists = await fileExists(pestUri);

  const preferPest = metadata.framework === "pest";
  const filterName = metadata.kind === "case" ? metadata.name : undefined;
  const relativeFile = path.relative(folder.uri.fsPath, metadata.uri.fsPath).replace(/\\/g, "/");

  const args: string[] = [];
  if (preferPest && pestExists) {
    args.push(pestBinary, relativeFile);
    if (filterName) {
      args.push("--filter", filterName);
    }
  } else if (phpunitExists) {
    args.push(phpunitBinary, relativeFile);
    if (filterName) {
      args.push("--filter", filterName);
    }
  } else {
    args.push("artisan", "test", relativeFile);
    if (filterName) {
      args.push("--filter", filterName);
    }
  }

  if (debugMode) {
    await launchXdebugSession(folder);
  }

  return runProcess(phpExecutable, args, folder.uri.fsPath, token);
}

async function runProcess(
  command: string,
  args: string[],
  cwd: string,
  token: vscode.CancellationToken,
): Promise<ProcessResult> {
  const started = Date.now();
  return new Promise((resolve) => {
    let output = "";
    const child = spawn(command, args, { cwd, shell: false });

    token.onCancellationRequested(() => {
      child.kill();
    });

    child.stdout.on("data", (chunk) => {
      output += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      output += chunk.toString();
    });

    child.on("close", (code) => {
      const renderedCommand = `${command} ${args.join(" ")}`;
      resolve({
        ok: code === 0,
        output,
        durationMs: Date.now() - started,
        command: renderedCommand,
      });
    });

    child.on("error", (error) => {
      const renderedCommand = `${command} ${args.join(" ")}`;
      resolve({
        ok: false,
        output: `${output}\n${error.message}`,
        durationMs: Date.now() - started,
        command: renderedCommand,
      });
    });
  });
}

async function fileExists(uri: vscode.Uri): Promise<boolean> {
  try {
    await vscode.workspace.fs.stat(uri);
    return true;
  } catch {
    return false;
  }
}

async function launchXdebugSession(explicitFolder?: vscode.WorkspaceFolder): Promise<void> {
  const folder = explicitFolder ?? vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    void vscode.window.showWarningMessage("Open a workspace folder to launch Xdebug.");
    return;
  }

  await ensureXdebugLaunchConfig(folder);

  const started = await vscode.debug.startDebugging(folder, "VSCode LS PHP: Listen for Xdebug");
  if (!started) {
    void vscode.window.showWarningMessage("Unable to start Xdebug session. Ensure PHP Debug extension is installed.");
  }
}

async function ensureXdebugLaunchConfig(folder: vscode.WorkspaceFolder): Promise<void> {
  const vscodeDir = vscode.Uri.joinPath(folder.uri, ".vscode");
  const launchUri = vscode.Uri.joinPath(vscodeDir, "launch.json");
  await vscode.workspace.fs.createDirectory(vscodeDir);

  let launchConfig: {
    version: string;
    configurations: Array<Record<string, unknown>>;
  } = {
    version: "0.2.0",
    configurations: [],
  };

  if (await fileExists(launchUri)) {
    try {
      const existing = await vscode.workspace.fs.readFile(launchUri);
      const parsed = JSON.parse(Buffer.from(existing).toString("utf8"));
      if (parsed && Array.isArray(parsed.configurations)) {
        launchConfig = {
          version: typeof parsed.version === "string" ? parsed.version : "0.2.0",
          configurations: parsed.configurations,
        };
      }
    } catch {
      // Keep default shape if existing launch.json is not parseable.
    }
  }

  const name = "VSCode LS PHP: Listen for Xdebug";
  const nextConfigurations = launchConfig.configurations.filter((item) => item.name !== name);
  nextConfigurations.push({
    name,
    type: "php",
    request: "launch",
    port: 9003,
    log: false,
    pathMappings: {
      "/var/www/html": "${workspaceFolder}",
    },
    xdebugSettings: {
      max_children: 128,
      max_data: 512,
      max_depth: 3,
    },
  });

  const rendered = `${JSON.stringify({
    version: launchConfig.version,
    configurations: nextConfigurations,
  }, null, 2)}\n`;
  await vscode.workspace.fs.writeFile(launchUri, Buffer.from(rendered, "utf8"));
}

async function writeXdebugTemplate(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    void vscode.window.showWarningMessage("Open a workspace folder to write xdebug.ini template.");
    return;
  }

  const vscodeDir = vscode.Uri.joinPath(folder.uri, ".vscode");
  await vscode.workspace.fs.createDirectory(vscodeDir);

  const templateUri = vscode.Uri.joinPath(vscodeDir, "xdebug.ini");
  const text = [
    "zend_extension=xdebug",
    "xdebug.mode=debug",
    "xdebug.start_with_request=yes",
    "xdebug.client_host=127.0.0.1",
    "xdebug.client_port=9003",
    "xdebug.log_level=0",
    "",
  ].join("\n");
  await vscode.workspace.fs.writeFile(templateUri, Buffer.from(text, "utf8"));

  await ensureXdebugLaunchConfig(folder);
  void vscode.window.showInformationMessage("VSCode LS PHP wrote .vscode/xdebug.ini and updated launch.json.");
}

async function startContinuousTesting(context: vscode.ExtensionContext): Promise<void> {
  if (continuousWatcher) {
    void vscode.window.showInformationMessage("VSCode LS PHP continuous testing is already running.");
    return;
  }

  const watcher = vscode.workspace.createFileSystemWatcher("**/tests/**/*.php");
  const trigger = async () => {
    const enabled = vscode.workspace
      .getConfiguration("vscodeLsPhp")
      .get<boolean>("enableContinuousTesting", true);
    if (!enabled) {
      return;
    }

    await refreshWorkspaceTests();
    if (!testController) {
      return;
    }

    const request = new vscode.TestRunRequest();
    const source = new vscode.CancellationTokenSource();
    await executeTestRun(request, false, source.token);
    source.dispose();
  };

  const safeTrigger = async () => {
    try {
      await trigger();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void vscode.window.showWarningMessage(`VSCode LS PHP continuous testing failed: ${message}`);
    }
  };

  watcher.onDidCreate(safeTrigger, undefined, context.subscriptions);
  watcher.onDidChange(safeTrigger, undefined, context.subscriptions);
  watcher.onDidDelete(safeTrigger, undefined, context.subscriptions);
  continuousWatcher = watcher;
  context.subscriptions.push(watcher);
  void vscode.window.showInformationMessage("VSCode LS PHP continuous testing started.");
}

function stopContinuousTesting(): void {
  if (!continuousWatcher) {
    return;
  }
  continuousWatcher.dispose();
  continuousWatcher = undefined;
}

function openProfilingDashboard(): void {
  const panel = vscode.window.createWebviewPanel(
    "vscodeLsPhpProfiling",
    "VSCode LS PHP Profiling",
    vscode.ViewColumn.Beside,
    { enableScripts: false },
  );

  const requestRows = Array.from(requestMetrics.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, metric]) => {
      const avg = metric.count === 0 ? 0 : metric.totalMs / metric.count;
      return `<tr><td>${name}</td><td>${metric.count}</td><td>${avg.toFixed(2)} ms</td><td>${metric.maxMs.toFixed(2)} ms</td><td>${metric.lastMs.toFixed(2)} ms</td></tr>`;
    })
    .join("\n");

  const testRows = recentTestRuns
    .slice()
    .reverse()
    .map((metric) => {
      const color = metric.ok ? "#0a7d35" : "#b42318";
      return `<tr><td>${escapeHtml(metric.label)}</td><td style=\"color:${color};font-weight:600;\">${metric.ok ? "PASS" : "FAIL"}</td><td>${metric.durationMs.toFixed(0)} ms</td></tr>`;
    })
    .join("\n");

  panel.webview.html = `<!DOCTYPE html>
<html lang=\"en\">
<head>
  <meta charset=\"UTF-8\" />
  <style>
    body { font-family: Segoe UI, sans-serif; padding: 16px; }
    h2 { margin-top: 0; }
    table { border-collapse: collapse; width: 100%; margin-bottom: 20px; }
    th, td { border: 1px solid #ddd; padding: 8px; text-align: left; }
    th { background: #f3f3f3; }
    .empty { color: #666; font-style: italic; }
  </style>
</head>
<body>
  <h2>LSP Request Metrics</h2>
  ${requestRows.length > 0 ? `<table><thead><tr><th>Request</th><th>Count</th><th>Avg</th><th>Max</th><th>Last</th></tr></thead><tbody>${requestRows}</tbody></table>` : `<p class=\"empty\">No request metrics yet. Trigger completion/hover/definition first.</p>`}
  <h2>Recent Test Runs</h2>
  ${testRows.length > 0 ? `<table><thead><tr><th>Target</th><th>Status</th><th>Duration</th></tr></thead><tbody>${testRows}</tbody></table>` : `<p class=\"empty\">No test run metrics yet. Run tests from Test Explorer.</p>`}
</body>
</html>`;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
