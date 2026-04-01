import * as path from "node:path";
import { ChildProcess, spawn } from "node:child_process";
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
let extensionOutputChannel: vscode.OutputChannel | undefined;
let composerDiagnosticCollection: vscode.DiagnosticCollection | undefined;

type KnownFramework = "phpunit" | "pest" | "unknown";

type TestMetadata = {
  uri: vscode.Uri;
  framework: KnownFramework;
  kind: "file" | "case";
  name?: string;
  datasetName?: string;
};

type ProcessResult = {
  ok: boolean;
  output: string;
  durationMs: number;
  command: string;
};

type HookResult = {
  ok: boolean;
  output: string;
  durationMs: number;
  command: string;
  exitCode: number | null;
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

type ComposerTopLevelKey =
  | "name"
  | "description"
  | "type"
  | "license"
  | "require"
  | "require-dev"
  | "autoload"
  | "autoload-dev"
  | "scripts"
  | "config"
  | "minimum-stability"
  | "prefer-stable"
  | "repositories";

type ComposerCompletionContext = "top-level-key" | "dependency-package" | "dependency-version" | "none";

type ComposerDiagnosticCode =
  | "composer.missingName"
  | "composer.missingRequire"
  | "composer.missingRequirePhp"
  | "composer.duplicatePackageInRequireDev"
  | "composer.invalidJson";

type ComposerDiagnosticSpec = {
  code: ComposerDiagnosticCode;
  message: string;
  severity: vscode.DiagnosticSeverity;
  startOffset?: number;
  endOffset?: number;
};

const COMPOSER_DIAGNOSTIC_SOURCE = "vscode-ls-php-composer";
const COMPOSER_TOP_LEVEL_KEYS: ComposerTopLevelKey[] = [
  "name",
  "description",
  "type",
  "license",
  "require",
  "require-dev",
  "autoload",
  "autoload-dev",
  "scripts",
  "config",
  "minimum-stability",
  "prefer-stable",
  "repositories",
];
const COMPOSER_COMMON_PACKAGES = [
  "laravel/framework",
  "phpunit/phpunit",
  "pestphp/pest",
  "guzzlehttp/guzzle",
  "symfony/console",
  "symfony/http-foundation",
  "monolog/monolog",
  "ramsey/uuid",
  "nesbot/carbon",
  "spatie/laravel-permission",
];
const COMPOSER_VERSION_SNIPPETS = ["^1.0", "^2.0", "^10.0", "*"];

const itemMetadata = new WeakMap<vscode.TestItem, TestMetadata>();
const requestMetrics = new Map<string, RequestMetric>();
const recentTestRuns: TestRunMetric[] = [];

// Map from URI string → Map<line, pass(true)/fail(false)>
const testResultsByUri = new Map<string, Map<number, boolean>>();

// Decoration types for test result gutter indicators (created lazily)
let passDecorationType: vscode.TextEditorDecorationType | undefined;
let failDecorationType: vscode.TextEditorDecorationType | undefined;
const debugWebServers = new Map<string, ChildProcess>();
const laravelDevServers = new Map<string, ChildProcess>();
const multiSessionDebugSessionIds = new Set<string>();
const multiSessionDebugSessions = new Map<string, vscode.DebugSession>();
const pendingMultiSessionWorkspaceKeys = new Set<string>();
const XDEBUG_LISTEN_CONFIG_NAME = "VSCode LS PHP: Listen for Xdebug";
type DbgpProxySettings = {
  useDbgpProxy: boolean;
  proxyHost: string;
  proxyPort: number;
  ideKey: string;
};
const INLINE_VALUE_RESULT_LIMIT = 150;
const INLINE_VALUE_VARIABLE_REGEX = /(^|[^A-Za-z0-9_$])(\$[A-Za-z_][A-Za-z0-9_]*)/g;
const INLINE_VALUE_COMMENT_ONLY_LINE_REGEX = /^\s*(?:\/\/|#|\/\*|\*|\*\/|{{--|--}})/;
const DEBUG_WATCH_TOOLTIP_TOKEN_REGEX = /\$[A-Za-z_][A-Za-z0-9_]*/;
const DEBUG_EXPRESSION_CANDIDATE_REGEX = /^\$[A-Za-z_][A-Za-z0-9_]*(?:(?:->\??[A-Za-z_][A-Za-z0-9_]*)|(?:\[[^\]\r\n]+\]))*$/;
const LOCAL_WHOLE_LINE_MAX_RESULTS = 5;
const LOCAL_WHOLE_LINE_COMMENT_ONLY_LINE_REGEX = /^\s*(?:\/\/|#|\/\*|\*|\*\/|{{--|--}})/;
const LOCAL_WHOLE_LINE_WHITESPACE_PREFIX_REGEX = /^\s*/;

function getOutputChannel(): vscode.OutputChannel {
  if (!extensionOutputChannel) {
    extensionOutputChannel = vscode.window.createOutputChannel("VSCode LS PHP");
  }
  return extensionOutputChannel;
}

function registerCommand(
  context: vscode.ExtensionContext,
  id: string,
  handler: (...args: any[]) => unknown,
): void {
  context.subscriptions.push(vscode.commands.registerCommand(id, handler));
}

function isBatchFormatAutoConfirmEnabled(): boolean {
  return process.env.VSCODE_LS_PHP_TEST === "1" || process.env.NODE_ENV === "test";
}

async function batchFormatWorkspace(requireConfirmation = true): Promise<void> {
  const output = getOutputChannel();
  output.appendLine("[batch-format] Starting workspace batch format scan.");

  const files = await vscode.workspace.findFiles("**/*.php", "**/{vendor,node_modules,.git,target}/**");
  if (files.length === 0) {
    void vscode.window.showInformationMessage("VSCode LS PHP: No PHP files found to format in the workspace.");
    output.appendLine("[batch-format] No PHP files found.");
    return;
  }

  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  const previewPaths = files.slice(0, 5).map((file) => {
    const root = workspaceFolder?.uri.fsPath;
    return root ? path.relative(root, file.fsPath).replace(/\\/g, "/") : file.fsPath;
  });
  const previewSummary = previewPaths.length > 0
    ? `${previewPaths.join("\n")}${files.length > previewPaths.length ? "\n..." : ""}`
    : "";
  output.appendLine(`[batch-format] Found ${files.length} PHP file(s). Preview:\n${previewSummary}`);

  let confirmed = true;
  if (requireConfirmation && !isBatchFormatAutoConfirmEnabled()) {
    const choice = await vscode.window.showWarningMessage(
      `Format ${files.length} PHP file(s) in the workspace?\n\nPreview:\n${previewSummary}`,
      { modal: true },
      "Format",
    );
    confirmed = choice === "Format";
  }

  if (!confirmed) {
    void vscode.window.showInformationMessage("VSCode LS PHP: Batch workspace formatting cancelled.");
    output.appendLine("[batch-format] Cancelled by user.");
    return;
  }

  let changed = 0;
  let skipped = 0;
  let failed = 0;

  for (const file of files) {
    const root = workspaceFolder?.uri.fsPath;
    const label = root ? path.relative(root, file.fsPath).replace(/\\/g, "/") : file.fsPath;
    try {
      const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
        "vscode.executeFormatDocumentProvider",
        file,
        { insertSpaces: true, tabSize: 4 },
      );

      if (!edits || edits.length === 0) {
        skipped += 1;
        output.appendLine(`[batch-format] Skipped (no edits): ${label}`);
        continue;
      }

      const workspaceEdit = new vscode.WorkspaceEdit();
      workspaceEdit.set(file, edits);
      const applied = await vscode.workspace.applyEdit(workspaceEdit);
      if (!applied) {
        failed += 1;
        output.appendLine(`[batch-format] Failed to apply edits: ${label}`);
        continue;
      }

      changed += 1;
      output.appendLine(`[batch-format] Changed: ${label} (${edits.length} edit(s))`);

      const openDocument = vscode.workspace.textDocuments.find((doc) => doc.uri.toString() === file.toString());
      if (openDocument?.isDirty) {
        await openDocument.save();
      }
    } catch (error) {
      failed += 1;
      const message = error instanceof Error ? error.message : String(error);
      output.appendLine(`[batch-format] Failed: ${label} :: ${message}`);
    }
  }

  const scanned = files.length;
  const summary = `VSCode LS PHP batch format complete. Scanned: ${scanned}, Changed: ${changed}, Skipped: ${skipped}, Failed: ${failed}.`;
  output.appendLine(`[batch-format] ${summary}`);
  void vscode.window.showInformationMessage(summary);
}

function isProcessRunning(process: ChildProcess): boolean {
  return process.exitCode === null && process.signalCode === null && !process.killed;
}

function getPassDecorationType(): vscode.TextEditorDecorationType {
  if (!passDecorationType) {
    passDecorationType = vscode.window.createTextEditorDecorationType({
      after: {
        contentText: " ✓",
        color: new vscode.ThemeColor("testing.iconPassed"),
        margin: "0 0 0 1em",
      },
    });
  }
  return passDecorationType;
}

function getFailDecorationType(): vscode.TextEditorDecorationType {
  if (!failDecorationType) {
    failDecorationType = vscode.window.createTextEditorDecorationType({
      after: {
        contentText: " ✗",
        color: new vscode.ThemeColor("testing.iconFailed"),
        margin: "0 0 0 1em",
      },
    });
  }
  return failDecorationType;
}

function applyTestDecorations(editor: vscode.TextEditor): void {
  const uriKey = editor.document.uri.toString();
  const results = testResultsByUri.get(uriKey);
  if (!results) {
    editor.setDecorations(getPassDecorationType(), []);
    editor.setDecorations(getFailDecorationType(), []);
    return;
  }

  const passRanges: vscode.Range[] = [];
  const failRanges: vscode.Range[] = [];

  for (const [line, passed] of results) {
    const range = new vscode.Range(line, 0, line, 0);
    if (passed) {
      passRanges.push(range);
    } else {
      failRanges.push(range);
    }
  }

  editor.setDecorations(getPassDecorationType(), passRanges);
  editor.setDecorations(getFailDecorationType(), failRanges);
}

function applyTestDecorationsToAllEditors(): void {
  for (const editor of vscode.window.visibleTextEditors) {
    applyTestDecorations(editor);
  }
}

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

function collectInlineValueLookups(text: string, viewPort: vscode.Range): vscode.InlineValueVariableLookup[] {
  const lines = text.split(/\r?\n/);
  if (lines.length === 0) {
    return [];
  }

  const startLine = Math.max(0, viewPort.start.line);
  const endLine = Math.min(lines.length - 1, viewPort.end.line);
  if (startLine > endLine) {
    return [];
  }

  const lookups: vscode.InlineValueVariableLookup[] = [];
  for (let lineNumber = startLine; lineNumber <= endLine; lineNumber += 1) {
    const lineText = lines[lineNumber] ?? "";
    const sliceStart = lineNumber === startLine ? Math.max(0, viewPort.start.character) : 0;
    const sliceEnd = lineNumber === endLine ? Math.min(lineText.length, viewPort.end.character) : lineText.length;
    if (sliceStart >= sliceEnd) {
      continue;
    }

    const lineSegment = lineText.slice(sliceStart, sliceEnd);
    if (!lineSegment.trim() || INLINE_VALUE_COMMENT_ONLY_LINE_REGEX.test(lineSegment.trim())) {
      continue;
    }

    const seenNamesOnLine = new Set<string>();
    INLINE_VALUE_VARIABLE_REGEX.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = INLINE_VALUE_VARIABLE_REGEX.exec(lineSegment)) !== null) {
      const variableName = match[2];
      if (seenNamesOnLine.has(variableName)) {
        continue;
      }
      seenNamesOnLine.add(variableName);

      const leadingText = match[1] ?? "";
      const startCharacter = sliceStart + match.index + leadingText.length;
      const endCharacter = startCharacter + variableName.length;
      lookups.push(
        new vscode.InlineValueVariableLookup(
          new vscode.Range(lineNumber, startCharacter, lineNumber, endCharacter),
          variableName,
          true,
        ),
      );
      if (lookups.length >= INLINE_VALUE_RESULT_LIMIT) {
        return lookups;
      }
    }
  }
  return lookups;
}

function extractDebugExpressionCandidate(raw: string | undefined): string | undefined {
  if (!raw) {
    return undefined;
  }
  const trimmed = raw.trim().replace(/;+$/, "");
  if (!trimmed) {
    return undefined;
  }
  return DEBUG_EXPRESSION_CANDIDATE_REGEX.test(trimmed) ? trimmed : undefined;
}

function deriveDefaultDebugExpression(editor: vscode.TextEditor | undefined): string | undefined {
  if (!editor) {
    return undefined;
  }
  if (!editor.selection.isEmpty) {
    return extractDebugExpressionCandidate(editor.document.getText(editor.selection));
  }
  const wordRange = editor.document.getWordRangeAtPosition(editor.selection.active, DEBUG_WATCH_TOOLTIP_TOKEN_REGEX);
  if (!wordRange) {
    return undefined;
  }
  return extractDebugExpressionCandidate(editor.document.getText(wordRange));
}

function isCommentOnlyLine(line: string): boolean {
  return LOCAL_WHOLE_LINE_COMMENT_ONLY_LINE_REGEX.test(line.trim());
}

function collectCandidateLinesFromText(text: string, beforeLineIndex: number, target: string[]): void {
  const lines = text.split(/\r?\n/);
  if (lines.length === 0) {
    return;
  }
  const maxIndex = Math.min(lines.length - 1, beforeLineIndex);
  for (let index = 0; index <= maxIndex; index += 1) {
    const line = lines[index] ?? "";
    if (!line.trim()) {
      continue;
    }
    if (isCommentOnlyLine(line)) {
      continue;
    }
    target.push(line);
  }
}

function collectOpenDocumentCandidateLines(activeDocument: vscode.TextDocument): string[] {
  const collected: string[] = [];
  for (const doc of vscode.workspace.textDocuments) {
    if (doc.isClosed || doc.uri.toString() === activeDocument.uri.toString()) {
      continue;
    }
    if (doc.languageId !== "php" && doc.languageId !== "blade") {
      continue;
    }
    collectCandidateLinesFromText(doc.getText(), Number.MAX_SAFE_INTEGER, collected);
  }
  return collected;
}

function buildUniqueTailSuggestions(prefix: string, fullLineText: string, candidates: string[]): string[] {
  const seen = new Set<string>();
  const tails: string[] = [];
  for (const candidateLine of candidates) {
    if (!candidateLine.startsWith(prefix)) {
      continue;
    }
    if (candidateLine === fullLineText) {
      continue;
    }
    const tail = candidateLine.slice(prefix.length);
    if (!tail || !tail.trim()) {
      continue;
    }
    if (seen.has(tail)) {
      continue;
    }
    seen.add(tail);
    tails.push(tail);
    if (tails.length >= LOCAL_WHOLE_LINE_MAX_RESULTS) {
      break;
    }
  }
  return tails;
}

function inferCurrentLineIndex(text: string, lineText: string): number {
  const lines = text.split(/\r?\n/);
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    if ((lines[index] ?? "") === lineText) {
      return index;
    }
  }
  return Math.max(0, lines.length - 1);
}

function collectLocalWholeLineSuggestionsAtLine(
  text: string,
  lineText: string,
  cursorCol: number,
  minPrefix: number,
  currentLineIndex: number,
  additionalLines: string[] = [],
): string[] {
  const safeCursorCol = Math.max(0, Math.min(cursorCol, lineText.length));
  const prefix = lineText.slice(0, safeCursorCol);
  const trimmedPrefix = prefix.trim();
  if (trimmedPrefix.length < minPrefix) {
    return [];
  }
  if (isCommentOnlyLine(prefix)) {
    return [];
  }

  const leadingWhitespace = (prefix.match(LOCAL_WHOLE_LINE_WHITESPACE_PREFIX_REGEX) ?? [""])[0];
  const baseCandidates: string[] = [];
  collectCandidateLinesFromText(text, Math.max(0, currentLineIndex - 1), baseCandidates);

  const normalizedCandidates = baseCandidates
    .concat(additionalLines)
    .filter((candidate) => candidate.startsWith(leadingWhitespace))
    .filter((candidate) => !isCommentOnlyLine(candidate));

  return buildUniqueTailSuggestions(prefix, lineText, normalizedCandidates);
}

function collectLocalWholeLineSuggestions(
  text: string,
  lineText: string,
  cursorCol: number,
  minPrefix: number,
  additionalLines: string[] = [],
): string[] {
  return collectLocalWholeLineSuggestionsAtLine(
    text,
    lineText,
    cursorCol,
    minPrefix,
    inferCurrentLineIndex(text, lineText),
    additionalLines,
  );
}

function getLocalWholeLineConfig(): { enabled: boolean; minPrefixLength: number } {
  const config = vscode.workspace.getConfiguration("vscodeLsPhp");
  const enabled = config.get<boolean>("enableLocalWholeLineSuggestions", false);
  const minPrefixLength = Math.max(1, config.get<number>("localWholeLineMinPrefixLength", 3));
  return { enabled, minPrefixLength };
}

async function editDebugExpressionValue(): Promise<void> {
  const output = getOutputChannel();
  const session = vscode.debug.activeDebugSession;
  if (!session) {
    output.appendLine("[debug-edit] No active debug session.");
    void vscode.window.showWarningMessage("VSCode LS PHP: Start a debug session before editing expression values.");
    return;
  }

  const defaultExpression = deriveDefaultDebugExpression(vscode.window.activeTextEditor);
  const expressionInput = await vscode.window.showInputBox({
    prompt: "Expression to edit",
    value: defaultExpression,
    validateInput: (value) => value.trim().length > 0 ? undefined : "Expression is required.",
  });
  if (typeof expressionInput !== "string") {
    output.appendLine("[debug-edit] Expression prompt cancelled.");
    return;
  }
  const expression = expressionInput.trim();

  const value = await vscode.window.showInputBox({
    prompt: `New value for ${expression}`,
    placeHolder: "e.g. 42, 'hello', ['key' => 'value']",
  });
  if (typeof value !== "string") {
    output.appendLine("[debug-edit] Value prompt cancelled.");
    return;
  }

  try {
    const preview = await session.customRequest("evaluate", { expression, context: "repl" });
    output.appendLine(`[debug-edit] Preview evaluate succeeded for ${expression}: ${JSON.stringify(preview)}`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(`[debug-edit] Preview evaluate failed for ${expression}: ${message}`);
  }

  try {
    const result = await session.customRequest("setExpression", { expression, value });
    output.appendLine(`[debug-edit] setExpression succeeded for ${expression}: ${JSON.stringify(result)}`);
    void vscode.window.showInformationMessage(`VSCode LS PHP: Updated ${expression} in active debug session.`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(`[debug-edit] setExpression unsupported/failed for ${expression}: ${message}`);
    void vscode.window.showWarningMessage(
      "VSCode LS PHP: Active debug adapter does not support direct setExpression updates.",
    );
  }
}

async function addDebugWatchExpression(providedExpression?: string): Promise<void> {
  const output = getOutputChannel();
  const activeEditor = vscode.window.activeTextEditor;
  if (!vscode.debug.activeDebugSession && !activeEditor) {
    output.appendLine("[debug-watch] No active debug session or editor.");
    void vscode.window.showWarningMessage("VSCode LS PHP: Open an editor or start a debug session to add watch expressions.");
    return;
  }

  let expression = extractDebugExpressionCandidate(providedExpression) ?? deriveDefaultDebugExpression(activeEditor);
  if (!expression) {
    const input = await vscode.window.showInputBox({
      prompt: "Expression to add to watch",
      validateInput: (value) => value.trim().length > 0 ? undefined : "Expression is required.",
    });
    if (typeof input !== "string") {
      output.appendLine("[debug-watch] Expression prompt cancelled.");
      return;
    }
    expression = input.trim();
  }

  try {
    await vscode.commands.executeCommand("debug.addToWatchExpressions", expression);
    output.appendLine(`[debug-watch] Added expression to watch: ${expression}`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(`[debug-watch] Failed to add expression "${expression}": ${message}`);
    void vscode.window.showWarningMessage(
      "VSCode LS PHP: Could not add watch expression automatically. Add it manually in the WATCH view.",
    );
  }
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration("vscodeLsPhp");
  const configuredPath = config.get<string>("serverPath")?.trim();
  const command = configuredPath && configuredPath.length > 0
    ? configuredPath
    : defaultServerPath(context);
  const formatStylePreset = config.get<string>("formatStylePreset", "default");
  const formatMaxBlankLines = config.get<number>("formatMaxBlankLines", 2);
  const formatBladeDirectiveSpacing = config.get<boolean>("formatBladeDirectiveSpacing", true);
  const formatTrimTrailingWhitespace = config.get<boolean>("formatTrimTrailingWhitespace", true);
  const formatterEnv: NodeJS.ProcessEnv = {
    ...process.env,
    VSCODE_LS_PHP_FORMAT_STYLE_PRESET: formatStylePreset,
    VSCODE_LS_PHP_FORMAT_MAX_BLANK_LINES: String(formatMaxBlankLines),
    VSCODE_LS_PHP_FORMAT_BLADE_DIRECTIVE_SPACING: String(formatBladeDirectiveSpacing),
    VSCODE_LS_PHP_FORMAT_TRIM_TRAILING_WHITESPACE: String(formatTrimTrailingWhitespace),
  };

  const serverOptions: ServerOptions = {
    run: { command, transport: TransportKind.stdio, options: { env: formatterEnv } },
    debug: { command, transport: TransportKind.stdio, options: { env: formatterEnv } },
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
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.{php,blade.php,ide.json}"),
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

function isComposerJsonDocument(document: vscode.TextDocument): boolean {
  if (document.languageId !== "json" && document.languageId !== "jsonc") {
    return false;
  }
  return path.basename(document.uri.fsPath).toLowerCase() === "composer.json";
}

function findMatchingBracket(text: string, startIndex: number, open: string, close: string): number {
  let depth = 0;
  let inString = false;
  let escapeNext = false;
  for (let i = startIndex; i < text.length; i += 1) {
    const char = text[i];
    if (inString) {
      if (escapeNext) {
        escapeNext = false;
      } else if (char === "\\") {
        escapeNext = true;
      } else if (char === "\"") {
        inString = false;
      }
      continue;
    }
    if (char === "\"") {
      inString = true;
      continue;
    }
    if (char === open) {
      depth += 1;
      continue;
    }
    if (char === close) {
      depth -= 1;
      if (depth === 0) {
        return i;
      }
    }
  }
  return -1;
}

function parseJsonString(text: string, start: number): { value: string; end: number } | undefined {
  if (text[start] !== "\"") {
    return undefined;
  }
  let value = "";
  let escapeNext = false;
  for (let i = start + 1; i < text.length; i += 1) {
    const char = text[i];
    if (escapeNext) {
      value += char;
      escapeNext = false;
      continue;
    }
    if (char === "\\") {
      escapeNext = true;
      continue;
    }
    if (char === "\"") {
      return { value, end: i };
    }
    value += char;
  }
  return undefined;
}

type ComposerTopLevelEntry = {
  key: string;
  keyStart: number;
  keyEnd: number;
  valueStart: number;
  valueEnd: number;
};

function parseComposerTopLevelEntries(text: string): Map<string, ComposerTopLevelEntry> {
  const entries = new Map<string, ComposerTopLevelEntry>();
  const rootStart = text.indexOf("{");
  if (rootStart === -1) {
    return entries;
  }
  const rootEnd = findMatchingBracket(text, rootStart, "{", "}");
  if (rootEnd === -1) {
    return entries;
  }

  let i = rootStart + 1;
  while (i < rootEnd) {
    while (i < rootEnd && /\s|,/.test(text[i])) {
      i += 1;
    }
    if (i >= rootEnd || text[i] !== "\"") {
      i += 1;
      continue;
    }
    const parsedKey = parseJsonString(text, i);
    if (!parsedKey) {
      i += 1;
      continue;
    }
    let cursor = parsedKey.end + 1;
    while (cursor < rootEnd && /\s/.test(text[cursor])) {
      cursor += 1;
    }
    if (text[cursor] !== ":") {
      i = cursor + 1;
      continue;
    }
    cursor += 1;
    while (cursor < rootEnd && /\s/.test(text[cursor])) {
      cursor += 1;
    }
    const valueStart = cursor;
    let valueEnd = cursor;
    if (text[cursor] === "{") {
      valueEnd = findMatchingBracket(text, cursor, "{", "}");
    } else if (text[cursor] === "[") {
      valueEnd = findMatchingBracket(text, cursor, "[", "]");
    } else if (text[cursor] === "\"") {
      const parsedValue = parseJsonString(text, cursor);
      valueEnd = parsedValue ? parsedValue.end : cursor;
    } else {
      while (valueEnd < rootEnd && !/[,\n\r}]/.test(text[valueEnd])) {
        valueEnd += 1;
      }
      valueEnd -= 1;
    }
    if (valueEnd < valueStart) {
      valueEnd = valueStart;
    }
    entries.set(parsedKey.value, {
      key: parsedKey.value,
      keyStart: i,
      keyEnd: parsedKey.end,
      valueStart,
      valueEnd,
    });
    i = valueEnd + 1;
  }
  return entries;
}

function parseComposerObjectPropertyOffsets(text: string, objectStart: number, objectEnd: number): Map<string, { keyStart: number; keyEnd: number }> {
  const map = new Map<string, { keyStart: number; keyEnd: number }>();
  let i = objectStart + 1;
  while (i < objectEnd) {
    while (i < objectEnd && /\s|,/.test(text[i])) {
      i += 1;
    }
    if (i >= objectEnd || text[i] !== "\"") {
      i += 1;
      continue;
    }
    const parsedKey = parseJsonString(text, i);
    if (!parsedKey) {
      i += 1;
      continue;
    }
    map.set(parsedKey.value, { keyStart: i, keyEnd: parsedKey.end });
    i = parsedKey.end + 1;
    while (i < objectEnd && text[i] !== ",") {
      if (text[i] === "{") {
        const end = findMatchingBracket(text, i, "{", "}");
        i = end === -1 ? i + 1 : end + 1;
        continue;
      }
      if (text[i] === "[") {
        const end = findMatchingBracket(text, i, "[", "]");
        i = end === -1 ? i + 1 : end + 1;
        continue;
      }
      if (text[i] === "\"") {
        const parsedValue = parseJsonString(text, i);
        i = parsedValue ? parsedValue.end + 1 : i + 1;
        continue;
      }
      i += 1;
    }
  }
  return map;
}

function inferJsonErrorOffset(message: string): number {
  const match = message.match(/position\s+(\d+)/i);
  if (!match) {
    return 0;
  }
  const parsed = Number.parseInt(match[1], 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

function buildComposerDiagnosticSpecs(text: string): ComposerDiagnosticSpec[] {
  const diagnostics: ComposerDiagnosticSpec[] = [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    const message = error instanceof Error ? error.message : "Invalid JSON.";
    const offset = inferJsonErrorOffset(message);
    diagnostics.push({
      code: "composer.invalidJson",
      message: `Invalid JSON: ${message}`,
      severity: vscode.DiagnosticSeverity.Error,
      startOffset: offset,
      endOffset: offset + 1,
    });
    return diagnostics;
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    diagnostics.push({
      code: "composer.invalidJson",
      message: "composer.json root must be a JSON object.",
      severity: vscode.DiagnosticSeverity.Error,
      startOffset: 0,
      endOffset: 1,
    });
    return diagnostics;
  }

  const root = parsed as Record<string, unknown>;
  const topLevelEntries = parseComposerTopLevelEntries(text);
  if (!Object.prototype.hasOwnProperty.call(root, "name")) {
    diagnostics.push({
      code: "composer.missingName",
      message: "composer.json is missing top-level \"name\".",
      severity: vscode.DiagnosticSeverity.Warning,
      startOffset: 0,
      endOffset: 1,
    });
  }
  if (!Object.prototype.hasOwnProperty.call(root, "require")) {
    diagnostics.push({
      code: "composer.missingRequire",
      message: "composer.json is missing top-level \"require\".",
      severity: vscode.DiagnosticSeverity.Warning,
      startOffset: 0,
      endOffset: 1,
    });
  }

  const requireValue = root.require;
  const requireObject = requireValue && typeof requireValue === "object" && !Array.isArray(requireValue)
    ? requireValue as Record<string, unknown>
    : undefined;

  if (requireObject && !Object.prototype.hasOwnProperty.call(requireObject, "php")) {
    const requireEntry = topLevelEntries.get("require");
    diagnostics.push({
      code: "composer.missingRequirePhp",
      message: "composer.json \"require\" is missing \"php\" version constraint.",
      severity: vscode.DiagnosticSeverity.Warning,
      startOffset: requireEntry?.valueStart ?? 0,
      endOffset: (requireEntry?.valueStart ?? 0) + 1,
    });
  }

  const requireDevValue = root["require-dev"];
  const requireDevObject = requireDevValue && typeof requireDevValue === "object" && !Array.isArray(requireDevValue)
    ? requireDevValue as Record<string, unknown>
    : undefined;

  if (requireObject && requireDevObject) {
    const requirePackages = new Set(Object.keys(requireObject));
    const requireDevEntry = topLevelEntries.get("require-dev");
    let requireDevOffsets = new Map<string, { keyStart: number; keyEnd: number }>();
    if (requireDevEntry && text[requireDevEntry.valueStart] === "{" && requireDevEntry.valueEnd > requireDevEntry.valueStart) {
      requireDevOffsets = parseComposerObjectPropertyOffsets(text, requireDevEntry.valueStart, requireDevEntry.valueEnd);
    }
    for (const pkg of Object.keys(requireDevObject)) {
      if (!requirePackages.has(pkg)) {
        continue;
      }
      const keyOffset = requireDevOffsets.get(pkg);
      diagnostics.push({
        code: "composer.duplicatePackageInRequireDev",
        message: `Package "${pkg}" is duplicated in "require" and "require-dev".`,
        severity: vscode.DiagnosticSeverity.Warning,
        startOffset: keyOffset?.keyStart ?? requireDevEntry?.valueStart ?? 0,
        endOffset: (keyOffset?.keyEnd ?? requireDevEntry?.valueStart ?? 0) + 1,
      });
    }
  }

  return diagnostics;
}

function toComposerDiagnostics(document: vscode.TextDocument, specs: ComposerDiagnosticSpec[]): vscode.Diagnostic[] {
  return specs.map((spec) => {
    const start = document.positionAt(Math.max(0, spec.startOffset ?? 0));
    const end = document.positionAt(Math.max(spec.startOffset ?? 0, spec.endOffset ?? (spec.startOffset ?? 0) + 1));
    const diagnostic = new vscode.Diagnostic(new vscode.Range(start, end), spec.message, spec.severity);
    diagnostic.source = COMPOSER_DIAGNOSTIC_SOURCE;
    diagnostic.code = spec.code;
    return diagnostic;
  });
}

function extractComposerDependencyNamesFromInstalledJson(content: string): string[] {
  try {
    const parsed = JSON.parse(content) as unknown;
    const names = new Set<string>();
    const collect = (entries: unknown) => {
      if (!Array.isArray(entries)) {
        return;
      }
      for (const entry of entries) {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
          continue;
        }
        const name = (entry as Record<string, unknown>).name;
        if (typeof name === "string" && name.trim().length > 0) {
          names.add(name.trim());
        }
      }
    };
    if (Array.isArray(parsed)) {
      collect(parsed);
    } else if (parsed && typeof parsed === "object") {
      const root = parsed as Record<string, unknown>;
      collect(root.packages);
      collect(root["packages-dev"]);
    }
    return Array.from(names.values()).sort((a, b) => a.localeCompare(b));
  } catch {
    return [];
  }
}

async function loadComposerPackageSuggestions(document: vscode.TextDocument): Promise<string[]> {
  const workspaceFolder = vscode.workspace.getWorkspaceFolder(document.uri);
  const fallback = new Set(COMPOSER_COMMON_PACKAGES);
  if (!workspaceFolder) {
    return Array.from(fallback.values()).sort((a, b) => a.localeCompare(b));
  }
  const installedJsonUri = vscode.Uri.joinPath(workspaceFolder.uri, "vendor", "composer", "installed.json");
  try {
    const bytes = await vscode.workspace.fs.readFile(installedJsonUri);
    const names = extractComposerDependencyNamesFromInstalledJson(Buffer.from(bytes).toString("utf8"));
    for (const name of names) {
      fallback.add(name);
    }
  } catch {
    // Ignore missing vendor/composer/installed.json
  }
  return Array.from(fallback.values()).sort((a, b) => a.localeCompare(b));
}

function computeJsonDepthAtOffset(text: string, offset: number): number {
  let depth = 0;
  let inString = false;
  let escapeNext = false;
  for (let i = 0; i < Math.min(offset, text.length); i += 1) {
    const char = text[i];
    if (inString) {
      if (escapeNext) {
        escapeNext = false;
      } else if (char === "\\") {
        escapeNext = true;
      } else if (char === "\"") {
        inString = false;
      }
      continue;
    }
    if (char === "\"") {
      inString = true;
      continue;
    }
    if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth = Math.max(0, depth - 1);
    }
  }
  return depth;
}

function getComposerCompletionContext(document: vscode.TextDocument, position: vscode.Position): ComposerCompletionContext {
  const text = document.getText();
  const offset = document.offsetAt(position);
  const linePrefix = document.lineAt(position.line).text.slice(0, position.character);
  const topLevelEntries = parseComposerTopLevelEntries(text);
  const requireRange = topLevelEntries.get("require");
  const requireDevRange = topLevelEntries.get("require-dev");
  const inDependencyRange = [requireRange, requireDevRange].some((entry) =>
    entry && offset >= entry.valueStart && offset <= entry.valueEnd
  );
  if (inDependencyRange) {
    if (/^\s*"[^"]*$/.test(linePrefix) || /^\s*$/.test(linePrefix)) {
      return "dependency-package";
    }
    if (/^\s*"[^"]+"\s*:\s*"[^"]*$/.test(linePrefix)) {
      return "dependency-version";
    }
    return "none";
  }

  const depth = computeJsonDepthAtOffset(text, offset);
  if (depth === 1 && (/^\s*"[^"]*$/.test(linePrefix) || /^\s*$/.test(linePrefix))) {
    return "top-level-key";
  }
  return "none";
}

function makeComposerCompletionProvider(): vscode.CompletionItemProvider {
  return {
    provideCompletionItems: async (document, position) => {
      if (!isComposerJsonDocument(document)) {
        return [];
      }
      const context = getComposerCompletionContext(document, position);
      if (context === "top-level-key") {
        return COMPOSER_TOP_LEVEL_KEYS.map((key) => {
          const item = new vscode.CompletionItem(key, vscode.CompletionItemKind.Property);
          item.insertText = key;
          item.detail = "composer.json top-level key";
          return item;
        });
      }
      if (context === "dependency-package") {
        const packages = await loadComposerPackageSuggestions(document);
        return packages.map((pkg) => {
          const item = new vscode.CompletionItem(pkg, vscode.CompletionItemKind.Module);
          item.insertText = pkg;
          item.detail = "Composer package";
          return item;
        });
      }
      if (context === "dependency-version") {
        return COMPOSER_VERSION_SNIPPETS.map((version) => {
          const item = new vscode.CompletionItem(version, vscode.CompletionItemKind.Value);
          item.insertText = version;
          item.detail = "Composer version constraint";
          return item;
        });
      }
      return [];
    },
  };
}

async function refreshComposerDiagnosticsForDocument(document: vscode.TextDocument): Promise<void> {
  if (!composerDiagnosticCollection) {
    return;
  }
  if (!isComposerJsonDocument(document)) {
    composerDiagnosticCollection.delete(document.uri);
    return;
  }
  const specs = buildComposerDiagnosticSpecs(document.getText());
  composerDiagnosticCollection.set(document.uri, toComposerDiagnostics(document, specs));
}

function composeJsonReplacement(document: vscode.TextDocument, root: Record<string, unknown>): vscode.WorkspaceEdit {
  const eol = document.eol === vscode.EndOfLine.CRLF ? "\r\n" : "\n";
  const rendered = `${JSON.stringify(root, null, 2).replace(/\n/g, eol)}${eol}`;
  const lastLine = document.lineAt(Math.max(0, document.lineCount - 1));
  const fullRange = new vscode.Range(0, 0, Math.max(0, document.lineCount - 1), lastLine.text.length);
  const edit = new vscode.WorkspaceEdit();
  edit.replace(document.uri, fullRange, rendered);
  return edit;
}

function buildComposerCodeActions(
  document: vscode.TextDocument,
  diagnostics: readonly vscode.Diagnostic[],
): vscode.CodeAction[] {
  const actions: vscode.CodeAction[] = [];
  const buildReplaceWithTemplateAction = (diagnostic: vscode.Diagnostic) => {
    const template = {
      name: "vendor/package",
      require: {
        php: "^8.2",
      },
    };
    const action = new vscode.CodeAction("Replace with minimal valid composer.json", vscode.CodeActionKind.QuickFix);
    action.diagnostics = [diagnostic];
    action.edit = composeJsonReplacement(document, template);
    return action;
  };
  let root: Record<string, unknown> | undefined;
  const getRoot = () => {
    if (root) {
      return root;
    }
    try {
      const parsed = JSON.parse(document.getText()) as unknown;
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return undefined;
      }
      root = parsed as Record<string, unknown>;
      return root;
    } catch {
      return undefined;
    }
  };

  const extractDuplicatePackageName = (message: string): string | undefined => {
    const match = message.match(/Package\s+"([^"]+)"/);
    return match?.[1];
  };

  for (const diagnostic of diagnostics) {
    if (diagnostic.source !== COMPOSER_DIAGNOSTIC_SOURCE || typeof diagnostic.code !== "string") {
      continue;
    }
    if (diagnostic.code === "composer.invalidJson") {
      actions.push(buildReplaceWithTemplateAction(diagnostic));
      continue;
    }
    if (diagnostic.code === "composer.missingName") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const updated = { ...parsed, name: parsed.name ?? "vendor/package" };
      const action = new vscode.CodeAction("Add composer \"name\" field", vscode.CodeActionKind.QuickFix);
      action.diagnostics = [diagnostic];
      action.edit = composeJsonReplacement(document, updated);
      actions.push(action);
      continue;
    }
    if (diagnostic.code === "composer.missingRequire") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const updated = { ...parsed, require: parsed.require ?? {} };
      const action = new vscode.CodeAction("Add composer \"require\" object", vscode.CodeActionKind.QuickFix);
      action.diagnostics = [diagnostic];
      action.edit = composeJsonReplacement(document, updated);
      actions.push(action);
      continue;
    }
    if (diagnostic.code === "composer.missingRequirePhp") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const require = parsed.require && typeof parsed.require === "object" && !Array.isArray(parsed.require)
        ? parsed.require as Record<string, unknown>
        : {};
      const updated = {
        ...parsed,
        require: {
          ...require,
          php: typeof require.php === "string" && require.php.length > 0 ? require.php : "^8.2",
        },
      };
      const action = new vscode.CodeAction("Add composer \"require.php\" constraint", vscode.CodeActionKind.QuickFix);
      action.diagnostics = [diagnostic];
      action.edit = composeJsonReplacement(document, updated);
      actions.push(action);
      continue;
    }
    if (diagnostic.code === "composer.duplicatePackageInRequireDev") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const packageName = extractDuplicatePackageName(diagnostic.message);
      if (!packageName) {
        continue;
      }
      const requireDev = parsed["require-dev"];
      if (!requireDev || typeof requireDev !== "object" || Array.isArray(requireDev)) {
        continue;
      }
      const requireDevMap = { ...(requireDev as Record<string, unknown>) };
      if (!Object.prototype.hasOwnProperty.call(requireDevMap, packageName)) {
        continue;
      }
      delete requireDevMap[packageName];
      const updated = { ...parsed, "require-dev": requireDevMap };
      const action = new vscode.CodeAction(
        `Remove duplicated "${packageName}" from require-dev`,
        vscode.CodeActionKind.QuickFix,
      );
      action.diagnostics = [diagnostic];
      action.edit = composeJsonReplacement(document, updated);
      actions.push(action);
    }
  }
  return actions;
}

export function __test_buildComposerDiagnosticSpecs(text: string): ComposerDiagnosticSpec[] {
  return buildComposerDiagnosticSpecs(text);
}

type ComposerCodeActionPreview = {
  title: string;
  updatedText: string;
};

type ComposerCodeActionPreviewDiagnostic = {
  source?: string;
  code?: string;
  message: string;
};

export function __test_buildComposerCodeActionPreviews(
  text: string,
  diagnostics: readonly ComposerCodeActionPreviewDiagnostic[],
): ComposerCodeActionPreview[] {
  const previews: ComposerCodeActionPreview[] = [];
  const render = (root: Record<string, unknown>) => `${JSON.stringify(root, null, 2)}\n`;
  const extractDuplicatePackageName = (message: string): string | undefined => {
    const match = message.match(/Package\s+"([^"]+)"/);
    return match?.[1];
  };
  let root: Record<string, unknown> | undefined;
  const getRoot = () => {
    if (root) {
      return root;
    }
    try {
      const parsed = JSON.parse(text) as unknown;
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return undefined;
      }
      root = parsed as Record<string, unknown>;
      return root;
    } catch {
      return undefined;
    }
  };

  for (const diagnostic of diagnostics) {
    if (diagnostic.source && diagnostic.source !== COMPOSER_DIAGNOSTIC_SOURCE) {
      continue;
    }
    if (diagnostic.code === "composer.invalidJson") {
      previews.push({
        title: "Replace with minimal valid composer.json",
        updatedText: render({
          name: "vendor/package",
          require: {
            php: "^8.2",
          },
        }),
      });
      continue;
    }
    if (diagnostic.code === "composer.missingName") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      previews.push({
        title: "Add composer \"name\" field",
        updatedText: render({ ...parsed, name: parsed.name ?? "vendor/package" }),
      });
      continue;
    }
    if (diagnostic.code === "composer.missingRequire") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      previews.push({
        title: "Add composer \"require\" object",
        updatedText: render({ ...parsed, require: parsed.require ?? {} }),
      });
      continue;
    }
    if (diagnostic.code === "composer.missingRequirePhp") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const require = parsed.require && typeof parsed.require === "object" && !Array.isArray(parsed.require)
        ? parsed.require as Record<string, unknown>
        : {};
      previews.push({
        title: "Add composer \"require.php\" constraint",
        updatedText: render({
          ...parsed,
          require: {
            ...require,
            php: typeof require.php === "string" && require.php.length > 0 ? require.php : "^8.2",
          },
        }),
      });
      continue;
    }
    if (diagnostic.code === "composer.duplicatePackageInRequireDev") {
      const parsed = getRoot();
      if (!parsed) {
        continue;
      }
      const packageName = extractDuplicatePackageName(diagnostic.message);
      if (!packageName) {
        continue;
      }
      const requireDev = parsed["require-dev"];
      if (!requireDev || typeof requireDev !== "object" || Array.isArray(requireDev)) {
        continue;
      }
      const requireDevMap = { ...(requireDev as Record<string, unknown>) };
      if (!Object.prototype.hasOwnProperty.call(requireDevMap, packageName)) {
        continue;
      }
      delete requireDevMap[packageName];
      previews.push({
        title: `Remove duplicated "${packageName}" from require-dev`,
        updatedText: render({ ...parsed, "require-dev": requireDevMap }),
      });
    }
  }
  return previews;
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  initializeTestExplorer(context);
  composerDiagnosticCollection = vscode.languages.createDiagnosticCollection(COMPOSER_DIAGNOSTIC_SOURCE);
  context.subscriptions.push(composerDiagnosticCollection);

  const composerSelector: vscode.DocumentSelector = [
    { scheme: "file", language: "json", pattern: "**/composer.json" },
    { scheme: "file", language: "jsonc", pattern: "**/composer.json" },
  ];
  context.subscriptions.push(
    vscode.languages.registerCompletionItemProvider(
      composerSelector,
      makeComposerCompletionProvider(),
      "\"",
    ),
  );
  context.subscriptions.push(
    vscode.languages.registerCodeActionsProvider(
      composerSelector,
      {
        provideCodeActions: (document, _range, context) => {
          if (!isComposerJsonDocument(document)) {
            return [];
          }
          return buildComposerCodeActions(document, context.diagnostics);
        },
      },
      {
        providedCodeActionKinds: [vscode.CodeActionKind.QuickFix],
      },
    ),
  );
  const inlineValueSelector: vscode.DocumentSelector = [
    { scheme: "file", language: "php" },
    { scheme: "file", language: "blade" },
    { scheme: "file", language: "php", pattern: "**/*.blade.php" },
  ];
  context.subscriptions.push(
    vscode.languages.registerInlineValuesProvider(
      inlineValueSelector,
      {
        provideInlineValues(document, viewPort) {
          if (!vscode.debug.activeDebugSession) {
            return [];
          }
          if (document.languageId !== "php" && document.languageId !== "blade") {
            return [];
          }
          return collectInlineValueLookups(document.getText(), viewPort);
        },
      },
    ),
  );
  context.subscriptions.push(
    vscode.languages.registerHoverProvider(inlineValueSelector, {
      provideHover(document, position) {
        if (!vscode.debug.activeDebugSession) {
          return null;
        }
        if (document.languageId !== "php" && document.languageId !== "blade") {
          return null;
        }
        const range = document.getWordRangeAtPosition(position, DEBUG_WATCH_TOOLTIP_TOKEN_REGEX);
        if (!range) {
          return null;
        }
        const token = document.getText(range);
        if (!extractDebugExpressionCandidate(token)) {
          return null;
        }

        const markdown = new vscode.MarkdownString(undefined, true);
        markdown.appendMarkdown("**Debug Quick Actions**\n\n");
        markdown.appendMarkdown("- VSCode LS PHP: Edit Debug Expression Value\n");
        markdown.appendMarkdown("- VSCode LS PHP: Add Debug Watch Expression");
        return new vscode.Hover(markdown, range);
      },
    }),
  );
  context.subscriptions.push(
    vscode.languages.registerInlineCompletionItemProvider(
      inlineValueSelector,
      {
        provideInlineCompletionItems(document, position) {
          const { enabled, minPrefixLength } = getLocalWholeLineConfig();
          if (!enabled) {
            return [];
          }
          if (document.languageId !== "php" && document.languageId !== "blade") {
            return [];
          }
          const lineText = document.lineAt(position.line).text;
          const openDocumentLines = collectOpenDocumentCandidateLines(document);
          const tails = collectLocalWholeLineSuggestionsAtLine(
            document.getText(),
            lineText,
            position.character,
            minPrefixLength,
            position.line,
            openDocumentLines,
          );
          if (tails.length === 0) {
            return [];
          }
          const range = new vscode.Range(position.line, position.character, position.line, position.character);
          return tails.map((tail) => new vscode.InlineCompletionItem(tail, range));
        },
      },
    ),
  );

  // Apply test decorations when an editor becomes visible
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      if (editor) {
        applyTestDecorations(editor);
      }
    }),
  );

  registerCommand(context, "vscode-ls-php.restartServer", async () => {
    await stopClient();
    await startClient(context);
    void vscode.window.showInformationMessage("VSCode LS PHP server restarted.");
  });

  registerCommand(context, "vscode-ls-php.launchXdebugSession", async () => {
    await launchXdebugSession();
  });

  registerCommand(context, "vscode-ls-php.editDebugExpressionValue", async () => {
    await editDebugExpressionValue();
  });

  registerCommand(context, "vscode-ls-php.addDebugWatchExpression", async (expression?: string) => {
    await addDebugWatchExpression(expression);
  });

  registerCommand(context, "vscode-ls-php.startMultiSessionDebug", async () => {
    await startMultiSessionDebug();
  });

  registerCommand(context, "vscode-ls-php.stopMultiSessionDebug", async () => {
    await stopMultiSessionDebug();
  });

  registerCommand(context, "vscode-ls-php.startLaravelDevServer", async () => {
    const folder = vscode.workspace.workspaceFolders?.[0];
    if (!folder) {
      void vscode.window.showWarningMessage("Open a workspace folder to start Laravel dev server.");
      return;
    }
    await startLaravelDevServer(folder);
  });

  registerCommand(context, "vscode-ls-php.stopLaravelDevServer", async () => {
    const folder = vscode.workspace.workspaceFolders?.[0];
    if (!folder) {
      void vscode.window.showInformationMessage("VSCode LS PHP Laravel dev server is not running (no workspace folder).");
      return;
    }
    await stopLaravelDevServer(folder);
  });

  registerCommand(context, "vscode-ls-php.writeXdebugTemplate", async () => {
    await writeXdebugTemplate();
  });

  registerCommand(context, "vscode-ls-php.writeDbgpProxyTemplate", async () => {
    await writeDbgpProxyTemplate();
  });

  registerCommand(context, "vscode-ls-php.inspectXdebugProfiles", async () => {
    await inspectXdebugProfiles();
  });

  registerCommand(context, "vscode-ls-php.refreshTests", async () => {
    await refreshWorkspaceTests();
  });

  registerCommand(context, "vscode-ls-php.startContinuousTesting", async () => {
    await startContinuousTesting(context);
  });

  registerCommand(context, "vscode-ls-php.stopContinuousTesting", async () => {
    stopContinuousTesting();
    void vscode.window.showInformationMessage("VSCode LS PHP continuous testing stopped.");
  });

  registerCommand(context, "vscode-ls-php.openProfilingDashboard", async () => {
    openProfilingDashboard();
  });

  registerCommand(context, "vscode-ls-php.resetProfilingMetrics", async () => {
    requestMetrics.clear();
    recentTestRuns.length = 0;
    void vscode.window.showInformationMessage("VSCode LS PHP profiling metrics reset.");
  });

  registerCommand(context, "vscode-ls-php.batchFormatWorkspace", async () => {
    await batchFormatWorkspace(true);
  });

  await startClient(context);

  // Format-on-save
  context.subscriptions.push(
    vscode.workspace.onWillSaveTextDocument((event) => {
      const doc = event.document;
      if (doc.languageId !== "php" && doc.languageId !== "blade") {
        return;
      }
      const cfg = vscode.workspace.getConfiguration("vscodeLsPhp", doc.uri);
      if (!cfg.get<boolean>("formatOnSave", false)) {
        return;
      }
      event.waitUntil(
        vscode.commands.executeCommand<vscode.TextEdit[]>(
          "vscode.executeFormatDocumentProvider",
          doc.uri,
          { insertSpaces: true, tabSize: 4 },
        ).then((edits) => edits ?? []),
      );
    }),
  );

  // Format-on-paste
  context.subscriptions.push(
    vscode.workspace.onDidChangeTextDocument((event) => {
      void refreshComposerDiagnosticsForDocument(event.document);

      const doc = event.document;
      if (doc.languageId !== "php" && doc.languageId !== "blade") {
        return;
      }
      const cfg = vscode.workspace.getConfiguration("vscodeLsPhp", doc.uri);
      if (!cfg.get<boolean>("formatOnPaste", false)) {
        return;
      }
      // Detect paste: single content change that spans multiple characters or lines
      if (event.contentChanges.length !== 1) {
        return;
      }
      const change = event.contentChanges[0];
      const isMultiLine = change.text.includes("\n");
      const isLongPaste = change.text.length > 10 && !change.text.match(/^\s+$/);
      if (!isMultiLine && !isLongPaste) {
        return;
      }
      // Format the pasted range
      const pastedLines = change.text.split("\n").length;
      const pasteStartLine = change.range.start.line;
      const pasteEndLine = pasteStartLine + pastedLines - 1;
      const rangeToFormat = new vscode.Range(
        pasteStartLine, 0,
        pasteEndLine, doc.lineAt(Math.min(pasteEndLine, doc.lineCount - 1)).text.length,
      );
      void vscode.commands.executeCommand<vscode.TextEdit[]>(
        "vscode.executeFormatRangeProvider",
        doc.uri,
        rangeToFormat,
        { insertSpaces: true, tabSize: 4 },
      ).then((edits) => {
        if (!edits || edits.length === 0) {
          return;
        }
        const wsEdit = new vscode.WorkspaceEdit();
        wsEdit.set(doc.uri, edits);
        return vscode.workspace.applyEdit(wsEdit);
      });
    }),
  );

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => {
      void refreshComposerDiagnosticsForDocument(document);
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((document) => {
      void refreshComposerDiagnosticsForDocument(document);
    }),
  );
  context.subscriptions.push(
    vscode.debug.onDidStartDebugSession((session) => {
      if (session.name !== XDEBUG_LISTEN_CONFIG_NAME) {
        return;
      }
      const workspaceKey = session.workspaceFolder?.uri.toString();
      if (!workspaceKey || !pendingMultiSessionWorkspaceKeys.has(workspaceKey)) {
        return;
      }
      pendingMultiSessionWorkspaceKeys.delete(workspaceKey);
      multiSessionDebugSessionIds.add(session.id);
      multiSessionDebugSessions.set(session.id, session);
      getOutputChannel().appendLine(`[multi-session-debug] tracking session ${session.id} for ${workspaceKey}`);
    }),
  );
  context.subscriptions.push(
    vscode.debug.onDidTerminateDebugSession((session) => {
      pendingMultiSessionWorkspaceKeys.delete(session.workspaceFolder?.uri.toString() ?? "");
      if (!multiSessionDebugSessionIds.delete(session.id)) {
        return;
      }
      multiSessionDebugSessions.delete(session.id);
      getOutputChannel().appendLine(`[multi-session-debug] session terminated ${session.id}`);
    }),
  );

  await refreshWorkspaceTests();
  for (const document of vscode.workspace.textDocuments) {
    await refreshComposerDiagnosticsForDocument(document);
  }
}

export async function deactivate(): Promise<void> {
  stopContinuousTesting();
  stopDebugWebServers();
  await stopLaravelDevServer();
  passDecorationType?.dispose();
  passDecorationType = undefined;
  failDecorationType?.dispose();
  failDecorationType = undefined;
  extensionOutputChannel?.dispose();
  extensionOutputChannel = undefined;
  composerDiagnosticCollection?.dispose();
  composerDiagnosticCollection = undefined;
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

  testResultsByUri.clear();
  applyTestDecorationsToAllEditors();
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
      if (testCase.datasetNames && testCase.datasetNames.length > 0) {
        for (const datasetName of testCase.datasetNames) {
          const datasetId = `${caseId}::dataset:${datasetName}`;
          const datasetItem = testController.createTestItem(
            datasetId,
            `${testCase.name} [dataset: ${datasetName}]`,
            file,
          );
          if (typeof testCase.line === "number") {
            datasetItem.range = new vscode.Range(testCase.line, 0, testCase.line, 120);
          }
          itemMetadata.set(datasetItem, {
            uri: file,
            framework: testCase.framework,
            kind: "case",
            name: testCase.name,
            datasetName,
          });
          caseItem.children.add(datasetItem);
        }
      }
      fileItem.children.add(caseItem);
    }

    testController.items.add(fileItem);
  }
}

type ParsedCase = { name: string; framework: KnownFramework; line?: number; datasetNames?: string[] };

async function parseTestsInFile(uri: vscode.Uri): Promise<ParsedCase[]> {
  try {
    const bytes = await vscode.workspace.fs.readFile(uri);
    const text = Buffer.from(bytes).toString("utf8");
    return parseTestsInText(text);
  } catch {
    return [];
  }
}

export function __test_parseTestsInText(text: string): ParsedCase[] {
  return parseTestsInText(text);
}

export function __test_collectInlineValueVariableNames(text: string, startLine: number, endLine: number): string[] {
  const viewPort = new vscode.Range(startLine, 0, endLine, Number.MAX_SAFE_INTEGER);
  return collectInlineValueLookups(text, viewPort)
    .map((lookup) => lookup.variableName)
    .filter((name): name is string => typeof name === "string");
}

export function __test_extractDebugExpressionCandidate(raw: string): string | undefined {
  return extractDebugExpressionCandidate(raw);
}

export function __test_buildPestFilterPattern(name: string, datasetName?: string): string {
  return buildPestFilterPattern(name, datasetName);
}

export function __test_collectLocalWholeLineSuggestions(
  text: string,
  lineText: string,
  cursorCol: number,
  minPrefix: number,
  additionalLines: string[] = [],
): string[] {
  return collectLocalWholeLineSuggestions(text, lineText, cursorCol, minPrefix, additionalLines);
}

export function __test_parseCommandLine(commandLine: string): string[] {
  return parseCommandLine(commandLine);
}

export function __test_hasOption(args: string[], optionName: string): boolean {
  return hasOption(args, optionName);
}

function parseTestsInText(text: string): ParsedCase[] {
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

  const pestRegex = /(^|[^\w$])(it|test)\s*\(\s*(["'`])((?:\\.|(?!\3)[\s\S])*)\3/gm;
  while ((match = pestRegex.exec(text)) !== null) {
    const functionStart = match.index + match[1].length;
    const statement = extractStatementSegment(text, functionStart);
    parsed.push({
      name: match[4],
      framework: "pest",
      line: offsetToLine(text, match.index),
      datasetNames: extractNamedInlineDatasets(statement),
    });
  }

  const uniq = new Map<string, ParsedCase>();
  for (const item of parsed) {
    const key = `${item.framework}:${item.name}`;
    const existing = uniq.get(key);
    if (!existing) {
      uniq.set(key, {
        ...item,
        datasetNames: item.datasetNames ? [...item.datasetNames] : undefined,
      });
      continue;
    }

    if (typeof existing.line !== "number" || (typeof item.line === "number" && item.line < existing.line)) {
      existing.line = item.line;
    }
    if (item.datasetNames && item.datasetNames.length > 0) {
      const merged = new Set([...(existing.datasetNames ?? []), ...item.datasetNames]);
      existing.datasetNames = Array.from(merged.values());
    }
  }

  return Array.from(uniq.values())
    .map((item) => ({
      ...item,
      datasetNames: item.datasetNames ? [...item.datasetNames].sort((a, b) => a.localeCompare(b)) : undefined,
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

function extractStatementSegment(text: string, startOffset: number): string {
  const openParenIndex = text.indexOf("(", startOffset);
  if (openParenIndex === -1) {
    return "";
  }

  let depth = 0;
  let closeParenIndex = -1;
  for (let i = openParenIndex; i < text.length; i += 1) {
    const char = text[i];
    if (char === "(") {
      depth += 1;
    } else if (char === ")") {
      depth -= 1;
      if (depth === 0) {
        closeParenIndex = i;
        break;
      }
    }
  }

  if (closeParenIndex === -1) {
    return text.slice(startOffset);
  }

  const semicolonIndex = text.indexOf(";", closeParenIndex);
  if (semicolonIndex === -1) {
    return text.slice(startOffset);
  }
  return text.slice(startOffset, semicolonIndex + 1);
}

function extractNamedInlineDatasets(statement: string): string[] {
  const datasetNames = new Set<string>();
  const withStartRegex = /->\s*with\s*\(/g;
  let withStartMatch: RegExpExecArray | null;

  while ((withStartMatch = withStartRegex.exec(statement)) !== null) {
    const callStart = withStartRegex.lastIndex;
    const arrayStart = statement.indexOf("[", callStart);
    if (arrayStart === -1) {
      continue;
    }

    let depth = 0;
    let arrayEnd = -1;
    for (let i = arrayStart; i < statement.length; i += 1) {
      const char = statement[i];
      if (char === "[") {
        depth += 1;
      } else if (char === "]") {
        depth -= 1;
        if (depth === 0) {
          arrayEnd = i;
          break;
        }
      }
    }

    if (arrayEnd === -1) {
      continue;
    }

    const body = statement.slice(arrayStart + 1, arrayEnd);
    const keyRegex = /["']([^"']+)["']\s*=>/g;
    let keyMatch: RegExpExecArray | null;
    while ((keyMatch = keyRegex.exec(body)) !== null) {
      datasetNames.add(keyMatch[1]);
    }

    withStartRegex.lastIndex = arrayEnd + 1;
  }

  return Array.from(datasetNames.values());
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function buildPestFilterPattern(name: string, datasetName?: string): string {
  if (!datasetName) {
    return name;
  }
  return `${escapeRegex(name)}.*${escapeRegex(datasetName)}`;
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
  const hookFolder = resolveHookWorkspaceFolder(items);
  const preTask = hookFolder
    ? vscode.workspace.getConfiguration("vscodeLsPhp", hookFolder.uri).get<string>("testPreTask", "").trim()
    : "";
  const postTask = hookFolder
    ? vscode.workspace.getConfiguration("vscodeLsPhp", hookFolder.uri).get<string>("testPostTask", "").trim()
    : "";

  let preHookFailed = false;
  try {
    if (preTask) {
      const preResult = await runHookCommand(preTask, hookFolder, token);
      appendHookResult(run, "preTask", preResult);
      if (!preResult.ok) {
        preHookFailed = true;
        const reason = `preTask failed with exit code ${preResult.exitCode ?? "unknown"}.`;
        for (const item of items) {
          if (token.isCancellationRequested) {
            run.skipped(item);
          } else {
            run.failed(item, new vscode.TestMessage(`${reason}\n${preResult.output || preResult.command}`));
          }
        }
      }
    }

    if (!preHookFailed) {
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
          // Store gutter indicator result
          const uriKey = metadata.uri.toString();
          if (item.range) {
            if (!testResultsByUri.has(uriKey)) {
              testResultsByUri.set(uriKey, new Map());
            }
            testResultsByUri.get(uriKey)!.set(item.range.start.line, result.ok);
          }
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
    }
  } finally {
    if (postTask) {
      try {
        const postResult = await runHookCommand(postTask, hookFolder, token);
        appendHookResult(run, "postTask", postResult);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        const output = getOutputChannel();
        output.appendLine(`[postTask] execution failed: ${message}`);
        run.appendOutput(`\n[postTask] execution failed: ${message}\n`);
      }
    }
    run.end();
    applyTestDecorationsToAllEditors();
  }
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

function resolveHookWorkspaceFolder(items: vscode.TestItem[]): vscode.WorkspaceFolder | undefined {
  for (const item of items) {
    const metadata = itemMetadata.get(item);
    if (!metadata) {
      continue;
    }
    const folder = vscode.workspace.getWorkspaceFolder(metadata.uri);
    if (folder) {
      return folder;
    }
  }
  return vscode.workspace.workspaceFolders?.[0];
}

async function runHookCommand(
  command: string,
  folder: vscode.WorkspaceFolder | undefined,
  token: vscode.CancellationToken,
): Promise<HookResult> {
  const started = Date.now();
  const cwd = folder?.uri.fsPath ?? process.cwd();
  return new Promise((resolve) => {
    let output = "";
    const child = spawn(command, {
      cwd,
      shell: true,
    });

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
      const renderedCommand = command;
      resolve({
        ok: code === 0,
        output,
        durationMs: Date.now() - started,
        command: renderedCommand,
        exitCode: code,
      });
    });

    child.on("error", (error) => {
      const renderedCommand = command;
      resolve({
        ok: false,
        output: `${output}\n${error.message}`,
        durationMs: Date.now() - started,
        command: renderedCommand,
        exitCode: null,
      });
    });
  });
}

function appendHookResult(
  run: vscode.TestRun,
  hookName: "preTask" | "postTask",
  result: HookResult,
): void {
  const output = getOutputChannel();
  const status = result.ok ? "PASS" : "FAIL";
  const exitCode = result.exitCode === null ? "n/a" : String(result.exitCode);
  const rendered = `[${hookName}] [${status}] exit=${exitCode}\n${result.command}\n${result.output}\n`;
  output.appendLine(rendered);
  run.appendOutput(`\n${rendered}`);
  if (!result.ok) {
    void vscode.window.showWarningMessage(`VSCode LS PHP ${hookName} failed with exit code ${exitCode}.`);
  }
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
  const filterName = metadata.kind === "case"
    ? (preferPest ? buildPestFilterPattern(metadata.name ?? "", metadata.datasetName) : metadata.name)
    : undefined;
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

  await ensureLaravelOrDebugServer(folder);
  await ensureXdebugLaunchConfig(folder);

  const started = await vscode.debug.startDebugging(folder, XDEBUG_LISTEN_CONFIG_NAME);
  if (!started) {
    void vscode.window.showWarningMessage("Unable to start Xdebug session. Ensure PHP Debug extension is installed.");
  }
}

async function startMultiSessionDebug(): Promise<void> {
  const folders = vscode.workspace.workspaceFolders ?? [];
  if (folders.length === 0) {
    void vscode.window.showWarningMessage("Open workspace folders to start multi-session debug.");
    return;
  }

  const output = getOutputChannel();
  output.appendLine(`[multi-session-debug] starting orchestration for ${folders.length} workspace folder(s)`);

  let startedCount = 0;
  let failedCount = 0;
  for (const folder of folders) {
    const workspaceKey = folder.uri.toString();
    try {
      output.appendLine(`[multi-session-debug] preparing ${folder.name}`);
      await ensureLaravelOrDebugServer(folder);
      await ensureXdebugLaunchConfig(folder);
      pendingMultiSessionWorkspaceKeys.add(workspaceKey);
      const started = await vscode.debug.startDebugging(folder, XDEBUG_LISTEN_CONFIG_NAME);
      if (started) {
        startedCount += 1;
        output.appendLine(`[multi-session-debug] started debug session for ${folder.name}`);
      } else {
        failedCount += 1;
        pendingMultiSessionWorkspaceKeys.delete(workspaceKey);
        output.appendLine(`[multi-session-debug] failed to start debug session for ${folder.name}`);
      }
    } catch (error) {
      failedCount += 1;
      pendingMultiSessionWorkspaceKeys.delete(workspaceKey);
      const message = error instanceof Error ? error.message : String(error);
      output.appendLine(`[multi-session-debug] ${folder.name} failed: ${message}`);
    }
  }

  const summary = `VSCode LS PHP multi-session debug complete. Started: ${startedCount}, Failed: ${failedCount}.`;
  if (failedCount > 0) {
    void vscode.window.showWarningMessage(summary);
  } else {
    void vscode.window.showInformationMessage(summary);
  }
}

async function stopMultiSessionDebug(): Promise<void> {
  const output = getOutputChannel();
  const trackedSessions = Array.from(multiSessionDebugSessions.values());
  const runningDebugServers = Array.from(debugWebServers.values()).filter((child) => isProcessRunning(child)).length;
  const runningLaravelServers = Array.from(laravelDevServers.values()).filter((child) => isProcessRunning(child)).length;
  if (trackedSessions.length === 0 && runningDebugServers === 0 && runningLaravelServers === 0) {
    void vscode.window.showInformationMessage("VSCode LS PHP multi-session debug has no active sessions or managed servers.");
    return;
  }

  output.appendLine(
    `[multi-session-debug] stop requested: sessions=${trackedSessions.length}, debugServers=${runningDebugServers}, laravelServers=${runningLaravelServers}`,
  );
  for (const session of trackedSessions) {
    try {
      await vscode.debug.stopDebugging(session);
      output.appendLine(`[multi-session-debug] stop requested for session ${session.id}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      output.appendLine(`[multi-session-debug] failed to stop session ${session.id}: ${message}`);
    } finally {
      multiSessionDebugSessionIds.delete(session.id);
      multiSessionDebugSessions.delete(session.id);
    }
  }
  pendingMultiSessionWorkspaceKeys.clear();
  stopDebugWebServers();
  await stopLaravelDevServer();

  void vscode.window.showInformationMessage(
    `VSCode LS PHP multi-session debug stop completed. Stopped sessions: ${trackedSessions.length}.`,
  );
}

async function ensureLaravelOrDebugServer(folder: vscode.WorkspaceFolder): Promise<void> {
  const config = vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri);
  if (config.get<boolean>("laravelDevServerEnabled", false)) {
    await ensureLaravelDevServer(folder);
    return;
  }
  await ensureDebugWebServer(folder);
}

async function ensureLaravelDevServer(folder: vscode.WorkspaceFolder): Promise<void> {
  const workspaceKey = folder.uri.toString();
  const existing = laravelDevServers.get(workspaceKey);
  if (existing && isProcessRunning(existing)) {
    return;
  }
  await startLaravelDevServer(folder);
}

async function ensureDebugWebServer(folder: vscode.WorkspaceFolder): Promise<void> {
  const config = vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri);
  if (!config.get<boolean>("debugAutoStartServer", false)) {
    return;
  }

  const workspaceKey = folder.uri.toString();
  const existing = debugWebServers.get(workspaceKey);
  if (existing && isProcessRunning(existing)) {
    return;
  }

  const host = config.get<string>("debugServerHost", "127.0.0.1").trim() || "127.0.0.1";
  const port = config.get<number>("debugServerPort", 8000);
  const configuredDocroot = config.get<string>("debugServerDocroot", "public").trim() || "public";
  const configuredRootUri = vscode.Uri.joinPath(folder.uri, configuredDocroot);
  const docrootExists = await fileExists(configuredRootUri);
  const docrootPath = docrootExists ? configuredRootUri.fsPath : folder.uri.fsPath;

  if (!docrootExists) {
    void vscode.window.showWarningMessage(
      `VSCode LS PHP debug server docroot "${configuredDocroot}" was not found in ${folder.name}; using workspace root.`,
    );
  }

  const phpExecutable = config.get<string>("phpExecutable", "php");
  const args = ["-S", `${host}:${port}`, "-t", docrootPath];
  const output = getOutputChannel();
  output.appendLine(`[debug-server] starting: ${phpExecutable} ${args.join(" ")} (cwd=${folder.uri.fsPath})`);

  const child = spawn(phpExecutable, args, { cwd: folder.uri.fsPath, shell: false });
  debugWebServers.set(workspaceKey, child);

  child.stdout?.on("data", (chunk) => {
    output.append(chunk.toString());
  });
  child.stderr?.on("data", (chunk) => {
    output.append(chunk.toString());
  });
  child.on("error", (error) => {
    debugWebServers.delete(workspaceKey);
    output.appendLine(`[debug-server] failed for ${folder.name}: ${error.message}`);
    void vscode.window.showWarningMessage(
      `VSCode LS PHP failed to start debug server for ${folder.name}: ${error.message}`,
    );
  });
  child.on("exit", (code, signal) => {
    debugWebServers.delete(workspaceKey);
    output.appendLine(
      `[debug-server] stopped for ${folder.name} (code=${code ?? "null"}, signal=${signal ?? "none"})`,
    );
    if (code !== null && code !== 0) {
      void vscode.window.showWarningMessage(
        `VSCode LS PHP debug server for ${folder.name} exited with code ${code}. Check the "VSCode LS PHP" output channel.`,
      );
    }
  });

  await new Promise<void>((resolve) => {
    let settled = false;
    const settle = () => {
      if (settled) {
        return;
      }
      settled = true;
      resolve();
    };

    child.once("spawn", () => {
      output.appendLine(`[debug-server] running for ${folder.name} at http://${host}:${port}`);
      settle();
    });
    child.once("error", settle);
    setTimeout(settle, 400);
  });
}

function parseCommandLine(commandLine: string): string[] {
  const tokens = commandLine.match(/"[^"]*"|'[^']*'|\S+/g) ?? [];
  return tokens.map((token) => {
    if ((token.startsWith("\"") && token.endsWith("\"")) || (token.startsWith("'") && token.endsWith("'"))) {
      return token.slice(1, -1);
    }
    return token;
  });
}

function hasOption(args: string[], optionName: string): boolean {
  return args.some((arg) => arg === optionName || arg.startsWith(`${optionName}=`));
}

async function startLaravelDevServer(folder: vscode.WorkspaceFolder): Promise<void> {
  const workspaceKey = folder.uri.toString();
  const existing = laravelDevServers.get(workspaceKey);
  if (existing && isProcessRunning(existing)) {
    void vscode.window.showInformationMessage(`VSCode LS PHP Laravel dev server is already running for ${folder.name}.`);
    return;
  }

  const config = vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri);
  const host = config.get<string>("laravelDevServerHost", "127.0.0.1").trim() || "127.0.0.1";
  const port = config.get<number>("laravelDevServerPort", 8000);
  const commandLine = config.get<string>("laravelDevServerCommand", "artisan serve").trim() || "artisan serve";
  const phpExecutable = config.get<string>("phpExecutable", "php");
  const parsedCommand = parseCommandLine(commandLine);
  const output = getOutputChannel();

  if (parsedCommand.length === 0) {
    const message = "VSCode LS PHP Laravel dev server command is empty.";
    output.appendLine(`[laravel-dev-server] ${message}`);
    void vscode.window.showWarningMessage(message);
    return;
  }

  let command = parsedCommand[0];
  let args = parsedCommand.slice(1);
  if (command.toLowerCase() === "artisan") {
    args = [command, ...args];
    command = phpExecutable;
  }

  if (!hasOption(args, "--host")) {
    args.push("--host", host);
  }
  if (!hasOption(args, "--port")) {
    args.push("--port", String(port));
  }

  output.appendLine(`[laravel-dev-server] starting: ${command} ${args.join(" ")} (cwd=${folder.uri.fsPath})`);

  let child: ChildProcess;
  try {
    child = spawn(command, args, { cwd: folder.uri.fsPath, shell: false });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(`[laravel-dev-server] failed for ${folder.name}: ${message}`);
    void vscode.window.showWarningMessage(`VSCode LS PHP failed to start Laravel dev server for ${folder.name}: ${message}`);
    return;
  }

  laravelDevServers.set(workspaceKey, child);
  child.stdout?.on("data", (chunk) => {
    output.append(chunk.toString());
  });
  child.stderr?.on("data", (chunk) => {
    output.append(chunk.toString());
  });
  child.on("error", (error) => {
    laravelDevServers.delete(workspaceKey);
    output.appendLine(`[laravel-dev-server] failed for ${folder.name}: ${error.message}`);
    void vscode.window.showWarningMessage(
      `VSCode LS PHP failed to start Laravel dev server for ${folder.name}: ${error.message}`,
    );
  });
  child.on("exit", (code, signal) => {
    laravelDevServers.delete(workspaceKey);
    output.appendLine(
      `[laravel-dev-server] stopped for ${folder.name} (code=${code ?? "null"}, signal=${signal ?? "none"})`,
    );
    if (code !== null && code !== 0) {
      void vscode.window.showWarningMessage(
        `VSCode LS PHP Laravel dev server for ${folder.name} exited with code ${code}. Check the "VSCode LS PHP" output channel.`,
      );
    }
  });

  await new Promise<void>((resolve) => {
    let settled = false;
    const settle = () => {
      if (settled) {
        return;
      }
      settled = true;
      resolve();
    };

    child.once("spawn", () => {
      output.appendLine(`[laravel-dev-server] running for ${folder.name} at http://${host}:${port}`);
      void vscode.window.showInformationMessage(
        `VSCode LS PHP Laravel dev server started for ${folder.name} at http://${host}:${port}`,
      );
      settle();
    });
    child.once("error", settle);
    setTimeout(settle, 400);
  });
}

async function stopLaravelDevServer(folder?: vscode.WorkspaceFolder): Promise<void> {
  const output = extensionOutputChannel;
  if (!folder) {
    for (const [workspaceKey, child] of laravelDevServers.entries()) {
      try {
        if (!child.killed) {
          child.kill();
        }
        output?.appendLine(`[laravel-dev-server] stop requested for ${workspaceKey}`);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        output?.appendLine(`[laravel-dev-server] stop failed for ${workspaceKey}: ${message}`);
      }
    }
    laravelDevServers.clear();
    return;
  }

  const workspaceKey = folder.uri.toString();
  const child = laravelDevServers.get(workspaceKey);
  if (!child || !isProcessRunning(child)) {
    laravelDevServers.delete(workspaceKey);
    void vscode.window.showInformationMessage(`VSCode LS PHP Laravel dev server is not running for ${folder.name}.`);
    return;
  }

  try {
    child.kill();
    output?.appendLine(`[laravel-dev-server] stop requested for ${workspaceKey}`);
    void vscode.window.showInformationMessage(`VSCode LS PHP Laravel dev server stopped for ${folder.name}.`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output?.appendLine(`[laravel-dev-server] stop failed for ${workspaceKey}: ${message}`);
    void vscode.window.showWarningMessage(`VSCode LS PHP failed to stop Laravel dev server for ${folder.name}: ${message}`);
  } finally {
    laravelDevServers.delete(workspaceKey);
  }
}

function stopDebugWebServers(): void {
  const output = extensionOutputChannel;
  for (const [workspaceKey, child] of debugWebServers.entries()) {
    try {
      if (!child.killed) {
        child.kill();
      }
      output?.appendLine(`[debug-server] stop requested for ${workspaceKey}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      output?.appendLine(`[debug-server] stop failed for ${workspaceKey}: ${message}`);
    }
  }
  debugWebServers.clear();
}

async function ensureXdebugLaunchConfig(folder: vscode.WorkspaceFolder): Promise<void> {
  const vscodeDir = vscode.Uri.joinPath(folder.uri, ".vscode");
  const launchUri = vscode.Uri.joinPath(vscodeDir, "launch.json");
  await vscode.workspace.fs.createDirectory(vscodeDir);
  const config = vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri);
  const enableCompoundLaunchTemplates = config.get<boolean>("enableCompoundLaunchTemplates", true);
  const dbgpProxySettings = resolveDbgpProxySettings(config);

  let launchConfig: {
    version: string;
    configurations: Array<Record<string, unknown>>;
    compounds?: Array<Record<string, unknown>>;
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
          compounds: Array.isArray(parsed.compounds) ? parsed.compounds : undefined,
        };
      }
    } catch {
      // Keep default shape if existing launch.json is not parseable.
    }
  }

  const baseConfigName = XDEBUG_LISTEN_CONFIG_NAME;
  const containerConfigName = "VSCode LS PHP: Listen for Xdebug (Container)";
  const laravelAssistCompoundName = "VSCode LS PHP: Debug + Laravel Server Assist";
  const containerCompoundName = "VSCode LS PHP: Debug + Container Mapping";
  const managedConfigNames = new Set([baseConfigName, containerConfigName]);
  const managedCompoundNames = new Set([laravelAssistCompoundName, containerCompoundName]);
  const hasManagedName = (entry: Record<string, unknown>, managedNames: Set<string>): boolean =>
    typeof entry.name === "string" && managedNames.has(entry.name);
  const proxyMetadata = dbgpProxySettings.useDbgpProxy
    ? `DBGp proxy enabled (${dbgpProxySettings.proxyHost}:${dbgpProxySettings.proxyPort}, idekey=${dbgpProxySettings.ideKey})`
    : undefined;
  const createManagedXdebugSettings = (): Record<string, unknown> => {
    const settings: Record<string, unknown> = {
      max_children: 128,
      max_data: 512,
      max_depth: 3,
    };
    if (proxyMetadata) {
      settings.vscode_ls_php_dbgp_proxy = proxyMetadata;
    }
    return settings;
  };
  const createManagedListenConfig = (name: string, pathMappings: Record<string, unknown>): Record<string, unknown> => {
    const managed: Record<string, unknown> = {
      name,
      type: "php",
      request: "launch",
      port: 9003,
      log: false,
      pathMappings,
      xdebugSettings: createManagedXdebugSettings(),
    };
    if (dbgpProxySettings.useDbgpProxy) {
      managed.hostname = dbgpProxySettings.proxyHost;
    }
    return managed;
  };

  const nextConfigurations = launchConfig.configurations.filter((item) => !hasManagedName(item, managedConfigNames));
  nextConfigurations.push(
    createManagedListenConfig(baseConfigName, {
      "/var/www/html": "${workspaceFolder}",
    }),
  );

  if (enableCompoundLaunchTemplates) {
    nextConfigurations.push(
      createManagedListenConfig(containerConfigName, {
        "/var/www/html": "${workspaceFolder}",
        "/app": "${workspaceFolder}",
      }),
    );
  }

  const existingCompounds = Array.isArray(launchConfig.compounds) ? launchConfig.compounds : [];
  const nextCompounds = existingCompounds.filter((item) => !hasManagedName(item, managedCompoundNames));
  if (enableCompoundLaunchTemplates) {
    nextCompounds.push(
      {
        name: laravelAssistCompoundName,
        configurations: [baseConfigName],
      },
      {
        name: containerCompoundName,
        configurations: [containerConfigName],
      },
    );
  }

  const renderedConfig: {
    version: string;
    configurations: Array<Record<string, unknown>>;
    compounds?: Array<Record<string, unknown>>;
  } = {
    version: launchConfig.version,
    configurations: nextConfigurations,
  };
  if (nextCompounds.length > 0) {
    renderedConfig.compounds = nextCompounds;
  }

  const rendered = `${JSON.stringify(renderedConfig, null, 2)}\n`;
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
  const dbgpProxySettings = resolveDbgpProxySettings(vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri));
  const text = [
    "zend_extension=xdebug",
    "xdebug.mode=debug",
    "xdebug.start_with_request=yes",
    "xdebug.client_host=127.0.0.1",
    "xdebug.client_port=9003",
    `xdebug.idekey=${dbgpProxySettings.ideKey}`,
    "",
    "; Keep client_host/client_port as fallback direct-connect settings.",
    ...(dbgpProxySettings.useDbgpProxy
      ? [
          "; DBGp proxy mode is enabled by VSCode LS PHP settings.",
          `; Proxy host: ${dbgpProxySettings.proxyHost}`,
          `; Proxy port: ${dbgpProxySettings.proxyPort}`,
          `; IDE key: ${dbgpProxySettings.ideKey}`,
          "; Register this IDE key with your DBGp proxy tool/server.",
          "; Xdebug uses idekey during DBGp proxy routing; no extra xdebug.ini proxy directive is required.",
          "; Optional proxy environment examples:",
          ";   DBGP_IDEKEY=<your-idekey>",
          ";   DBGP_PROXY=<proxy-host:proxy-port>",
        ]
      : [
          "; DBGp proxy mode is disabled.",
          "; Enable vscodeLsPhp.xdebugUseDbgpProxy to add proxy guidance for external DBGp proxy tooling.",
        ]),
    "xdebug.log_level=0",
    "",
  ].join("\n");
  await vscode.workspace.fs.writeFile(templateUri, Buffer.from(text, "utf8"));

  await ensureXdebugLaunchConfig(folder);
  void vscode.window.showInformationMessage("VSCode LS PHP wrote .vscode/xdebug.ini and updated launch.json.");
}

async function writeDbgpProxyTemplate(): Promise<void> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    void vscode.window.showWarningMessage("Open a workspace folder to write DBGp proxy template.");
    return;
  }

  const vscodeDir = vscode.Uri.joinPath(folder.uri, ".vscode");
  await vscode.workspace.fs.createDirectory(vscodeDir);
  const templateUri = vscode.Uri.joinPath(vscodeDir, "xdebug-proxy.json");
  const settings = resolveDbgpProxySettings(vscode.workspace.getConfiguration("vscodeLsPhp", folder.uri));
  const text = `${JSON.stringify(
    {
      enabled: settings.useDbgpProxy,
      host: settings.proxyHost,
      port: settings.proxyPort,
      ideKey: settings.ideKey,
    },
    null,
    2,
  )}\n`;
  await vscode.workspace.fs.writeFile(templateUri, Buffer.from(text, "utf8"));
  void vscode.window.showInformationMessage("VSCode LS PHP wrote .vscode/xdebug-proxy.json.");
}

function resolveDbgpProxySettings(config: vscode.WorkspaceConfiguration): DbgpProxySettings {
  const proxyHost = config.get<string>("xdebugProxyHost", "127.0.0.1").trim() || "127.0.0.1";
  const proxyPortRaw = config.get<number>("xdebugProxyPort", 9001);
  const proxyPort = Number.isFinite(proxyPortRaw) && proxyPortRaw > 0 ? Math.floor(proxyPortRaw) : 9001;
  const ideKey = config.get<string>("xdebugIdeKey", "VSCODE").trim() || "VSCODE";
  return {
    useDbgpProxy: config.get<boolean>("xdebugUseDbgpProxy", false),
    proxyHost,
    proxyPort,
    ideKey,
  };
}

type XdebugProfileQuickPickItem = vscode.QuickPickItem & { uri: vscode.Uri; mtime: number };

function buildXdebugProfileQuickPickItems(
  entries: Array<{ uri: vscode.Uri; mtime: number }>,
  workspaceFolder: vscode.WorkspaceFolder | undefined,
  searchBaseUri: vscode.Uri,
): XdebugProfileQuickPickItem[] {
  return entries.map((entry) => {
    const filename = path.basename(entry.uri.fsPath);
    const relativePath = workspaceFolder
      ? path.relative(workspaceFolder.uri.fsPath, entry.uri.fsPath)
      : path.relative(searchBaseUri.fsPath, entry.uri.fsPath);
    return {
      label: filename,
      description: relativePath.replace(/\\/g, "/"),
      detail: `Modified: ${new Date(entry.mtime).toLocaleString()}`,
      uri: entry.uri,
      mtime: entry.mtime,
    };
  });
}

async function selectXdebugProfileQuickPickItem(
  quickPickItems: XdebugProfileQuickPickItem[],
  picker?: (
    items: XdebugProfileQuickPickItem[],
  ) => Thenable<XdebugProfileQuickPickItem | undefined> | Promise<XdebugProfileQuickPickItem | undefined> |
    XdebugProfileQuickPickItem | undefined,
): Promise<XdebugProfileQuickPickItem | undefined> {
  if (quickPickItems.length <= 1) {
    return quickPickItems[0];
  }
  if (picker) {
    return await picker(quickPickItems);
  }
  return await vscode.window.showQuickPick(quickPickItems, {
    placeHolder: "Select an Xdebug profile to open",
    matchOnDescription: true,
    matchOnDetail: true,
  });
}

export async function __test_selectXdebugProfileQuickPickItem(
  quickPickItems: XdebugProfileQuickPickItem[],
  picker?: (items: XdebugProfileQuickPickItem[]) => Promise<XdebugProfileQuickPickItem | undefined> | XdebugProfileQuickPickItem | undefined,
): Promise<XdebugProfileQuickPickItem | undefined> {
  return await selectXdebugProfileQuickPickItem(quickPickItems, picker);
}

async function inspectXdebugProfiles(): Promise<void> {
  try {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder?.uri);
    const configuredProfilePath = config.get<string>("xdebugProfilePath", "").trim();
    const configuredGlob = config.get<string>("xdebugProfileGlob", "cachegrind.out.*").trim() || "cachegrind.out.*";
    const normalizedGlob = configuredGlob.replace(/\\/g, "/");
    const recursiveGlob = normalizedGlob.includes("/") || normalizedGlob.startsWith("**/")
      ? normalizedGlob
      : `**/${normalizedGlob}`;

    let searchBaseUri: vscode.Uri;
    if (configuredProfilePath) {
      if (path.isAbsolute(configuredProfilePath)) {
        searchBaseUri = vscode.Uri.file(configuredProfilePath);
      } else if (workspaceFolder) {
        const segments = configuredProfilePath.replace(/\\/g, "/").split("/").filter(Boolean);
        searchBaseUri = vscode.Uri.joinPath(workspaceFolder.uri, ...segments);
      } else {
        void vscode.window.showWarningMessage(
          "VSCode LS PHP could not resolve xdebugProfilePath without an open workspace folder.",
        );
        return;
      }
    } else if (workspaceFolder) {
      searchBaseUri = workspaceFolder.uri;
    } else {
      void vscode.window.showWarningMessage(
        "Open a workspace folder or configure vscodeLsPhp.xdebugProfilePath to inspect Xdebug profiles.",
      );
      return;
    }

    const pattern = new vscode.RelativePattern(searchBaseUri, recursiveGlob);
    const files = await vscode.workspace.findFiles(pattern);
    if (files.length === 0) {
      void vscode.window.showInformationMessage("VSCode LS PHP found no Xdebug profile files.");
      return;
    }

    const entries = (
      await Promise.all(
        files.map(async (uri) => {
          try {
            const stat = await vscode.workspace.fs.stat(uri);
            return { uri, mtime: stat.mtime };
          } catch {
            return undefined;
          }
        }),
      )
    ).filter((entry): entry is { uri: vscode.Uri; mtime: number } => Boolean(entry));
    entries.sort((a, b) => b.mtime - a.mtime);
    if (entries.length === 0) {
      void vscode.window.showInformationMessage("VSCode LS PHP found no readable Xdebug profile files.");
      return;
    }

    const output = getOutputChannel();
    const newest = entries[0];
    output.appendLine(`[Xdebug Profiles] Found ${entries.length} file(s).`);
    output.appendLine(`[Xdebug Profiles] Search base: ${searchBaseUri.fsPath}`);
    output.appendLine(`[Xdebug Profiles] Newest: ${newest.uri.fsPath}`);

    const quickPickItems = buildXdebugProfileQuickPickItems(entries, workspaceFolder, searchBaseUri);
    const selected = await selectXdebugProfileQuickPickItem(quickPickItems);
    if (!selected) {
      return;
    }

    await notifyIfCachegrindProfile(selected.uri);
    const document = await vscode.workspace.openTextDocument(selected.uri);
    await vscode.window.showTextDocument(document, { preview: false });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    void vscode.window.showWarningMessage(`VSCode LS PHP failed to inspect Xdebug profiles: ${message}`);
  }
}

async function notifyIfCachegrindProfile(uri: vscode.Uri): Promise<void> {
  try {
    const bytes = await vscode.workspace.fs.readFile(uri);
    const head = Buffer.from(bytes).toString("utf8").split(/\r?\n/).slice(0, 20);
    const looksLikeCachegrind = head.some((line) => line.startsWith("version:")) &&
      head.some((line) => line.startsWith("cmd:") || line.startsWith("events:") || line.startsWith("fl="));
    if (looksLikeCachegrind) {
      void vscode.window.showInformationMessage("VSCode LS PHP: cachegrind profile detected.");
    }
  } catch {
    // Ignore preview read failures and continue opening file.
  }
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
