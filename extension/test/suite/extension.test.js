const assert = require("node:assert");
const path = require("node:path");
const vscode = require("vscode");
const extensionModule = require(path.join(__dirname, "..", "..", "out", "extension.js"));

async function deleteIfExists(uri) {
  try {
    await vscode.workspace.fs.delete(uri, { useTrash: false, recursive: true });
  } catch {}
}

async function waitFor(predicate, timeoutMs = 5000, intervalMs = 100) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = await predicate();
    if (value) {
      return value;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error("Timed out waiting for condition.");
}

async function getComposerQuickFixes(uri, lineCount) {
  return (await vscode.commands.executeCommand(
    "vscode.executeCodeActionProvider",
    uri,
    new vscode.Range(0, 0, Math.max(0, lineCount - 1), 0),
    vscode.CodeActionKind.QuickFix.value,
  )) ?? [];
}

suite("VSCode LS PHP Integration", () => {
  test("activates on Laravel PHP file and contributes restart command", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const phpPath = path.join(workspaceFolder.uri.fsPath, "routes", "web.php");
    const doc = await vscode.workspace.openTextDocument(phpPath);
    await vscode.window.showTextDocument(doc);

    const extension = vscode.extensions.getExtension("local.vscode-ls-php");
    assert.ok(extension, "Extension local.vscode-ls-php should be installed in test host.");

    if (!extension.isActive) {
      await extension.activate();
    }

    assert.ok(extension.isActive, "Extension should be active after opening PHP file.");

    const allCommands = await vscode.commands.getCommands(true);
    const expectedCommands = [
      "vscode-ls-php.restartServer",
      "vscode-ls-php.launchXdebugSession",
      "vscode-ls-php.editDebugExpressionValue",
      "vscode-ls-php.addDebugWatchExpression",
      "vscode-ls-php.startMultiSessionDebug",
      "vscode-ls-php.stopMultiSessionDebug",
      "vscode-ls-php.startLaravelDevServer",
      "vscode-ls-php.stopLaravelDevServer",
      "vscode-ls-php.writeXdebugTemplate",
      "vscode-ls-php.writeDbgpProxyTemplate",
      "vscode-ls-php.inspectXdebugProfiles",
      "vscode-ls-php.refreshTests",
      "vscode-ls-php.startContinuousTesting",
      "vscode-ls-php.stopContinuousTesting",
      "vscode-ls-php.openProfilingDashboard",
      "vscode-ls-php.resetProfilingMetrics",
      "vscode-ls-php.batchFormatWorkspace",
    ];
    for (const command of expectedCommands) {
      assert.ok(allCommands.includes(command), `Command should be registered: ${command}`);
    }
  });

  test("opens php document and executes restart command", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const tempPath = path.join(workspaceFolder.uri.fsPath, "routes", "__runtime_test.php");
    const tempUri = vscode.Uri.file(tempPath);
    const content = "<?php\nRoute::get('/runtime-test', fn () => 'ok');\n";

    await vscode.workspace.fs.writeFile(tempUri, Buffer.from(content, "utf8"));

    try {
      const doc = await vscode.workspace.openTextDocument(tempUri);
      await vscode.window.showTextDocument(doc);

      await vscode.commands.executeCommand("vscode-ls-php.restartServer");

      assert.strictEqual(doc.languageId, "php", "Temporary document should be recognized as PHP.");
    } finally {
      await vscode.workspace.fs.delete(tempUri, { useTrash: false });
    }
  });

  test("rename provider handles registertesttest end-boundary append flow without duplicate suffix", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const targetUri = vscode.Uri.joinPath(
      workspaceFolder.uri,
      "app",
      "Providers",
      "AppServiceProvider.php",
    );
    const doc = await vscode.workspace.openTextDocument(targetUri);
    const editor = await vscode.window.showTextDocument(doc);
    const originalText = doc.getText();

    const toEditsArray = (workspaceEdit) => {
      if (!workspaceEdit) {
        return [];
      }
      if (typeof workspaceEdit.entries === "function") {
        const all = [];
        for (const [, textEdits] of workspaceEdit.entries()) {
          all.push(...textEdits);
        }
        return all;
      }
      const all = [];
      const rawChanges = workspaceEdit.changes ?? {};
      for (const uri of Object.keys(rawChanges)) {
        const textEdits = rawChanges[uri] ?? [];
        all.push(...textEdits);
      }
      return all;
    };

    const replaceFullDocumentText = async (nextText) => {
      const lastLine = editor.document.lineCount - 1;
      const lastChar = editor.document.lineAt(lastLine).text.length;
      const fullRange = new vscode.Range(0, 0, lastLine, lastChar);
      await editor.edit((builder) => builder.replace(fullRange, nextText));
    };

    try {
      const declarationLine = doc
        .getText()
        .split(/\r?\n/)
        .findIndex((line) => line.includes("public function registertesttest(): void"));
      assert.ok(declarationLine >= 0, "Target method declaration should exist.");
      const declarationText = doc.lineAt(declarationLine).text;
      const identifierStart = declarationText.indexOf("registertesttest");
      assert.ok(identifierStart >= 0, "Target identifier should exist on declaration line.");
      const identifierBoundary = new vscode.Position(declarationLine, identifierStart + "registertesttest".length);

      const appendOneResult = await vscode.commands.executeCommand(
        "vscode.executeDocumentRenameProvider",
        targetUri,
        identifierBoundary,
        "registertesttestA",
      );
      const appendOneEdits = toEditsArray(appendOneResult);
      assert.ok(appendOneResult, "Rename should not return empty/no-result for append-one-char flow.");
      assert.ok(appendOneEdits.length > 0, "Rename should provide at least one edit for append-one-char flow.");
      assert.ok(
        appendOneEdits.some((edit) => edit.newText === "registertesttestA"),
        "Rename edits should include the appended one-char target name.",
      );
      assert.ok(
        appendOneEdits.every((edit) => edit.newText !== "registertesttestAA"),
        "Rename edits should never double-append to registertesttestAA for one-char append.",
      );
    } finally {
      await replaceFullDocumentText(originalText);
      await editor.document.save();
    }
  });

  test("opens blade template and keeps extension active", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const tempPath = path.join(workspaceFolder.uri.fsPath, "resources", "views", "__runtime_test.blade.php");
    const tempUri = vscode.Uri.file(tempPath);
    const content = "@extends('layouts.app')\n@section('content')\n<div>ok</div>\n@endsection\n";

    await vscode.workspace.fs.createDirectory(vscode.Uri.file(path.dirname(tempPath)));
    await vscode.workspace.fs.writeFile(tempUri, Buffer.from(content, "utf8"));

    try {
      const doc = await vscode.workspace.openTextDocument(tempUri);
      await vscode.window.showTextDocument(doc);

      const extension = vscode.extensions.getExtension("local.vscode-ls-php");
      assert.ok(extension, "Extension local.vscode-ls-php should be installed in test host.");
      if (!extension.isActive) {
        await extension.activate();
      }

      assert.ok(extension.isActive, "Extension should remain active on Blade template files.");
      assert.ok(["php", "blade"].includes(doc.languageId), "Blade document should resolve to php or blade language mode.");
    } finally {
      await vscode.workspace.fs.delete(tempUri, { useTrash: false });
    }
  });

  test("writes xdebug template and launch config", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const vscodeDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode");
    const xdebugIni = vscode.Uri.joinPath(vscodeDir, "xdebug.ini");
    const launchJson = vscode.Uri.joinPath(vscodeDir, "launch.json");

    try {
      await vscode.commands.executeCommand("vscode-ls-php.writeXdebugTemplate");

      const iniContent = Buffer.from(await vscode.workspace.fs.readFile(xdebugIni)).toString("utf8");
      const launchContent = Buffer.from(await vscode.workspace.fs.readFile(launchJson)).toString("utf8");
      const launch = JSON.parse(launchContent);
      const configNames = (launch.configurations ?? []).map((entry) => entry.name);
      const compoundNames = (launch.compounds ?? []).map((entry) => entry.name);

      assert.ok(iniContent.includes("xdebug.mode=debug"), "xdebug.ini should contain debug mode setting.");
      assert.ok(iniContent.includes("xdebug.idekey=VSCODE"), "xdebug.ini should contain default idekey.");
      assert.ok(iniContent.includes("DBGp proxy mode is disabled"), "xdebug.ini should contain proxy disabled guidance by default.");
      assert.ok(configNames.includes("VSCode LS PHP: Listen for Xdebug"), "launch.json should contain VSCode LS PHP debug configuration.");
      assert.ok(
        configNames.includes("VSCode LS PHP: Listen for Xdebug (Container)"),
        "launch.json should contain VSCode LS PHP container mapping debug configuration.",
      );
      assert.ok(
        compoundNames.includes("VSCode LS PHP: Debug + Laravel Server Assist"),
        "launch.json should contain VSCode LS PHP Laravel assist compound.",
      );
    } finally {
      await deleteIfExists(xdebugIni);
      await deleteIfExists(launchJson);
      await deleteIfExists(vscodeDir);
    }
  });

  test("writes DBGp proxy-aware xdebug template and launch metadata when enabled", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const vscodeDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode");
    const xdebugIni = vscode.Uri.joinPath(vscodeDir, "xdebug.ini");
    const launchJson = vscode.Uri.joinPath(vscodeDir, "launch.json");
    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder.uri);
    const previousUseProxy = config.get("xdebugUseDbgpProxy", false);
    const previousProxyHost = config.get("xdebugProxyHost", "127.0.0.1");
    const previousProxyPort = config.get("xdebugProxyPort", 9001);
    const previousIdeKey = config.get("xdebugIdeKey", "VSCODE");

    try {
      await config.update("xdebugUseDbgpProxy", true, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyHost", "10.0.0.2", vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyPort", 9101, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugIdeKey", "VSCODE_PROXY", vscode.ConfigurationTarget.Workspace);
      await vscode.commands.executeCommand("vscode-ls-php.writeXdebugTemplate");

      const iniContent = Buffer.from(await vscode.workspace.fs.readFile(xdebugIni)).toString("utf8");
      const launch = JSON.parse(Buffer.from(await vscode.workspace.fs.readFile(launchJson)).toString("utf8"));
      const managed = (launch.configurations ?? []).find((entry) => entry.name === "VSCode LS PHP: Listen for Xdebug");
      assert.ok(managed, "Managed Xdebug launch config should exist.");
      assert.strictEqual(managed.hostname, "10.0.0.2", "Managed Xdebug launch config should include proxy host metadata.");
      assert.ok(managed.xdebugSettings, "Managed Xdebug launch config should include xdebug settings.");
      assert.ok(
        String(managed.xdebugSettings.vscode_ls_php_dbgp_proxy).includes("idekey=VSCODE_PROXY"),
        "Managed xdebug settings should include DBGp proxy idekey metadata.",
      );
      assert.ok(iniContent.includes("xdebug.idekey=VSCODE_PROXY"), "xdebug.ini should include configured idekey.");
      assert.ok(iniContent.includes("DBGp proxy mode is enabled"), "xdebug.ini should include proxy enabled guidance.");
      assert.ok(iniContent.includes("Proxy host: 10.0.0.2"), "xdebug.ini should include configured proxy host guidance.");
      assert.ok(iniContent.includes("Proxy port: 9101"), "xdebug.ini should include configured proxy port guidance.");
    } finally {
      await config.update("xdebugUseDbgpProxy", previousUseProxy, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyHost", previousProxyHost, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyPort", previousProxyPort, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugIdeKey", previousIdeKey, vscode.ConfigurationTarget.Workspace);
      await deleteIfExists(xdebugIni);
      await deleteIfExists(launchJson);
      await deleteIfExists(vscodeDir);
    }
  });

  test("writes DBGp proxy summary template", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const vscodeDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode");
    const proxyTemplate = vscode.Uri.joinPath(vscodeDir, "xdebug-proxy.json");
    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder.uri);
    const previousUseProxy = config.get("xdebugUseDbgpProxy", false);
    const previousProxyHost = config.get("xdebugProxyHost", "127.0.0.1");
    const previousProxyPort = config.get("xdebugProxyPort", 9001);
    const previousIdeKey = config.get("xdebugIdeKey", "VSCODE");

    try {
      await config.update("xdebugUseDbgpProxy", true, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyHost", "127.0.0.10", vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyPort", 9002, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugIdeKey", "PROXY_TEST", vscode.ConfigurationTarget.Workspace);
      await vscode.commands.executeCommand("vscode-ls-php.writeDbgpProxyTemplate");

      const parsed = JSON.parse(Buffer.from(await vscode.workspace.fs.readFile(proxyTemplate)).toString("utf8"));
      assert.deepStrictEqual(parsed, {
        enabled: true,
        host: "127.0.0.10",
        port: 9002,
        ideKey: "PROXY_TEST",
      });
    } finally {
      await config.update("xdebugUseDbgpProxy", previousUseProxy, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyHost", previousProxyHost, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProxyPort", previousProxyPort, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugIdeKey", previousIdeKey, vscode.ConfigurationTarget.Workspace);
      await deleteIfExists(proxyTemplate);
      await deleteIfExists(vscodeDir);
    }
  });

  test("writes only base launch template when compound templates are disabled", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const vscodeDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode");
    const xdebugIni = vscode.Uri.joinPath(vscodeDir, "xdebug.ini");
    const launchJson = vscode.Uri.joinPath(vscodeDir, "launch.json");
    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder.uri);
    const previous = config.get("enableCompoundLaunchTemplates", true);

    try {
      await config.update("enableCompoundLaunchTemplates", false, vscode.ConfigurationTarget.Workspace);
      await vscode.commands.executeCommand("vscode-ls-php.writeXdebugTemplate");

      const launchContent = Buffer.from(await vscode.workspace.fs.readFile(launchJson)).toString("utf8");
      const launch = JSON.parse(launchContent);
      const configNames = (launch.configurations ?? []).map((entry) => entry.name);
      const compoundNames = (launch.compounds ?? []).map((entry) => entry.name);

      assert.ok(configNames.includes("VSCode LS PHP: Listen for Xdebug"), "launch.json should still include base debug configuration.");
      assert.ok(
        !configNames.includes("VSCode LS PHP: Listen for Xdebug (Container)"),
        "launch.json should not include VSCode LS PHP container mapping debug configuration when disabled.",
      );
      assert.ok(
        !compoundNames.includes("VSCode LS PHP: Debug + Laravel Server Assist"),
        "launch.json should not include VSCode LS PHP managed compounds when disabled.",
      );
      assert.ok(
        !compoundNames.includes("VSCode LS PHP: Debug + Container Mapping"),
        "launch.json should not include VSCode LS PHP managed compounds when disabled.",
      );
    } finally {
      await config.update("enableCompoundLaunchTemplates", previous, vscode.ConfigurationTarget.Workspace);
      await deleteIfExists(xdebugIni);
      await deleteIfExists(launchJson);
      await deleteIfExists(vscodeDir);
    }
  });

  test("preserves unrelated launch entries while replacing managed templates", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const vscodeDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode");
    const launchJson = vscode.Uri.joinPath(vscodeDir, "launch.json");
    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder.uri);
    const previous = config.get("enableCompoundLaunchTemplates", true);
    const unrelatedConfigName = "Unrelated Custom Debug";
    const unrelatedCompoundName = "Unrelated Compound";

    try {
      await vscode.workspace.fs.createDirectory(vscodeDir);
      const existing = {
        version: "0.2.0",
        configurations: [
          {
            name: unrelatedConfigName,
            type: "php",
            request: "launch",
            port: 9009,
          },
          {
            name: "VSCode LS PHP: Listen for Xdebug",
            type: "php",
            request: "launch",
            port: 9003,
          },
          {
            name: "VSCode LS PHP: Listen for Xdebug (Container)",
            type: "php",
            request: "launch",
            port: 9003,
          },
        ],
        compounds: [
          {
            name: unrelatedCompoundName,
            configurations: [unrelatedConfigName],
          },
          {
            name: "VSCode LS PHP: Debug + Laravel Server Assist",
            configurations: ["VSCode LS PHP: Listen for Xdebug"],
          },
        ],
      };
      await vscode.workspace.fs.writeFile(launchJson, Buffer.from(`${JSON.stringify(existing, null, 2)}\n`, "utf8"));

      await config.update("enableCompoundLaunchTemplates", false, vscode.ConfigurationTarget.Workspace);
      await vscode.commands.executeCommand("vscode-ls-php.writeXdebugTemplate");

      const launch = JSON.parse(Buffer.from(await vscode.workspace.fs.readFile(launchJson)).toString("utf8"));
      const configNames = (launch.configurations ?? []).map((entry) => entry.name);
      const compoundNames = (launch.compounds ?? []).map((entry) => entry.name);

      assert.ok(configNames.includes(unrelatedConfigName), "Unrelated launch configurations should be preserved.");
      assert.ok(configNames.includes("VSCode LS PHP: Listen for Xdebug"), "Managed base launch configuration should remain.");
      assert.ok(
        !configNames.includes("VSCode LS PHP: Listen for Xdebug (Container)"),
        "Managed container launch configuration should be removed when disabled.",
      );
      assert.ok(compoundNames.includes(unrelatedCompoundName), "Unrelated compounds should be preserved.");
      assert.ok(
        !compoundNames.includes("VSCode LS PHP: Debug + Laravel Server Assist"),
        "Managed compounds should be removed when disabled.",
      );
      assert.ok(
        !compoundNames.includes("VSCode LS PHP: Debug + Container Mapping"),
        "Managed compounds should be removed when disabled.",
      );
    } finally {
      await config.update("enableCompoundLaunchTemplates", previous, vscode.ConfigurationTarget.Workspace);
      await deleteIfExists(launchJson);
      await deleteIfExists(vscodeDir);
    }
  });

  test("laravel dev server commands execute without throwing", async () => {
    await assert.doesNotReject(async () => {
      await vscode.commands.executeCommand("vscode-ls-php.startLaravelDevServer");
      await vscode.commands.executeCommand("vscode-ls-php.stopLaravelDevServer");
    });
  });

  test("multi-session debug commands execute without throwing", async () => {
    await assert.doesNotReject(async () => {
      await vscode.commands.executeCommand("vscode-ls-php.startMultiSessionDebug");
      await vscode.commands.executeCommand("vscode-ls-php.stopMultiSessionDebug");
    });
  });

  test("debug edit/watch commands execute without throwing outside active debug session", async () => {
    await assert.doesNotReject(async () => {
      await vscode.commands.executeCommand("vscode-ls-php.editDebugExpressionValue");
      await vscode.commands.executeCommand("vscode-ls-php.addDebugWatchExpression", "$runtimeValue");
    });
  });

  test("batch workspace format command executes without throwing", async () => {
    await assert.doesNotReject(async () => {
      await vscode.commands.executeCommand("vscode-ls-php.batchFormatWorkspace");
    });
  });

  test("inspect xdebug profiles command executes without throwing", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const config = vscode.workspace.getConfiguration("vscodeLsPhp", workspaceFolder.uri);
    const previousPath = config.get("xdebugProfilePath", "");
    const previousGlob = config.get("xdebugProfileGlob", "cachegrind.out.*");
    const tempDir = vscode.Uri.joinPath(workspaceFolder.uri, ".vscode-ls-php-test-xdebug-profiles");
    const profileUri = vscode.Uri.joinPath(tempDir, "cachegrind.out.123");
    const profileContent = [
      "version: 1",
      "cmd: php artisan test",
      "events: Time",
      "fl=/app/app/Example.php",
      "fn=App\\\\Example::run",
      "1 10",
      "",
    ].join("\n");

    try {
      await vscode.workspace.fs.createDirectory(tempDir);
      await vscode.workspace.fs.writeFile(profileUri, Buffer.from(profileContent, "utf8"));
      await config.update("xdebugProfilePath", path.basename(tempDir.fsPath), vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProfileGlob", "cachegrind.out.*", vscode.ConfigurationTarget.Workspace);

      await assert.doesNotReject(async () => {
        await vscode.commands.executeCommand("vscode-ls-php.inspectXdebugProfiles");
      });
    } finally {
      await config.update("xdebugProfilePath", previousPath, vscode.ConfigurationTarget.Workspace);
      await config.update("xdebugProfileGlob", previousGlob, vscode.ConfigurationTarget.Workspace);
      await deleteIfExists(profileUri);
      await deleteIfExists(tempDir);
    }
  });

  test("xdebug profile quick-pick helper handles 0/1/many options", async () => {
    const select = extensionModule.__test_selectXdebugProfileQuickPickItem;
    assert.strictEqual(typeof select, "function", "Xdebug quick-pick selector test hook should be exported.");

    const none = await select([]);
    assert.strictEqual(none, undefined, "Selector should return undefined when there are no options.");

    const singleItem = {
      label: "cachegrind.out.1",
      description: "cachegrind.out.1",
      detail: "Modified: now",
      uri: vscode.Uri.parse("file:///tmp/cachegrind.out.1"),
      mtime: Date.now(),
    };
    const single = await select([singleItem], async () => {
      throw new Error("Picker callback should not run for single item.");
    });
    assert.strictEqual(single, singleItem, "Selector should auto-select the only available profile.");

    const first = {
      label: "cachegrind.out.2",
      description: "cachegrind.out.2",
      detail: "Modified: now",
      uri: vscode.Uri.parse("file:///tmp/cachegrind.out.2"),
      mtime: Date.now(),
    };
    const second = {
      label: "cachegrind.out.3",
      description: "cachegrind.out.3",
      detail: "Modified: now",
      uri: vscode.Uri.parse("file:///tmp/cachegrind.out.3"),
      mtime: Date.now(),
    };
    const many = await select([first, second], async (items) => {
      assert.strictEqual(items.length, 2, "Picker should receive all options for many-item selection.");
      return items[1];
    });
    assert.strictEqual(many, second, "Selector should return the item chosen by the picker callback.");
  });

  test("discovers Pest high-order chained tests", () => {
    const text = `<?php
it('skipped works')->skip();
test(
  "grouped works"
)->group('smoke');
it('todo works')
  ->todo();
`;

    const parsed = extensionModule.__test_parseTestsInText(text);
    const pestNames = parsed.filter((entry) => entry.framework === "pest").map((entry) => entry.name).sort();

    assert.deepStrictEqual(
      pestNames,
      ["grouped works", "skipped works", "todo works"],
      "High-order Pest chained tests should be discovered by name.",
    );
  });

  test("extracts named inline datasets for Pest tests", () => {
    const text = `<?php
it('works with datasets', function (array $input) {
  expect($input)->not->toBeEmpty();
})->with([
  'case A' => [[1]],
  "case B" => [[2]],
  0 => [[3]],
]);
`;

    const parsed = extensionModule.__test_parseTestsInText(text);
    const datasetCase = parsed.find((entry) => entry.name === "works with datasets");

    assert.ok(datasetCase, "Expected Pest case to be parsed.");
    assert.deepStrictEqual(
      datasetCase.datasetNames,
      ["case A", "case B"],
      "Only named dataset entries should be extracted for dataset child discovery.",
    );
  });

  test("extracts named inline datasets when not at line start", () => {
    const text = `<?php\nif (true) it('inline dataset', fn () => null)->with(['one' => [1], 'two' => [2]]);`;
    const parsed = extensionModule.__test_parseTestsInText(text);
    const datasetCase = parsed.find((entry) => entry.name === "inline dataset");

    assert.ok(datasetCase, "Expected inline Pest case to be parsed.");
    assert.deepStrictEqual(datasetCase.datasetNames, ["one", "two"]);
  });

  test("builds Pest dataset filter pattern with dataset name", () => {
    const filter = extensionModule.__test_buildPestFilterPattern("works with datasets", "case A");
    assert.strictEqual(
      filter,
      "works with datasets.*case A",
      "Dataset-aware Pest filter should include test and dataset name.",
    );
  });

  test("collects inline value variable names from code lines in range", () => {
    const names = extensionModule.__test_collectInlineValueVariableNames(
      "<?php\n$foo = 1;\n$bar = $foo + 2;\n",
      1,
      2,
    );
    assert.deepStrictEqual(names, ["$foo", "$bar", "$foo"]);
  });

  test("deduplicates duplicate inline variables on the same line", () => {
    const names = extensionModule.__test_collectInlineValueVariableNames(
      "<?php\n$total = $total + $total + 1;\n",
      1,
      1,
    );
    assert.deepStrictEqual(names, ["$total"]);
  });

  test("ignores non-variable tokens when collecting inline variables", () => {
    const names = extensionModule.__test_collectInlineValueVariableNames(
      "<?php\nfoo bar 123 $$bad $9bad\n",
      1,
      1,
    );
    assert.deepStrictEqual(names, []);
  });

  test("collects inline variables from blade mixed-content lines", () => {
    const names = extensionModule.__test_collectInlineValueVariableNames(
      "<div>{{ $user->name }} @if($show) {{ $title }} @endif</div>\n",
      0,
      0,
    );
    assert.deepStrictEqual(names, ["$user", "$show", "$title"]);
  });

  test("extracts debug expression candidates from selection/cursor text helper", () => {
    const extract = extensionModule.__test_extractDebugExpressionCandidate;
    assert.strictEqual(typeof extract, "function", "Debug expression extraction helper should be exported.");
    assert.strictEqual(extract("  $foo['bar']; "), "$foo['bar']");
    assert.strictEqual(extract("$user->profile['name']"), "$user->profile['name']");
    assert.strictEqual(extract("foo()"), undefined);
    assert.strictEqual(extract("$9bad"), undefined);
  });

  test("parses command-line helper tokens with quotes and whitespace", () => {
    const parseCommandLine = extensionModule.__test_parseCommandLine;
    assert.strictEqual(typeof parseCommandLine, "function", "Command line parser helper should be exported.");
    assert.deepStrictEqual(parseCommandLine("artisan serve --host=127.0.0.1"), ["artisan", "serve", "--host=127.0.0.1"]);
    assert.deepStrictEqual(parseCommandLine("  artisan   serve   "), ["artisan", "serve"]);
    assert.deepStrictEqual(parseCommandLine("php \"artisan serve\" '--port=9000'"), ["php", "artisan serve", "--port=9000"]);
    assert.deepStrictEqual(parseCommandLine(""), []);
    assert.deepStrictEqual(parseCommandLine("   "), []);
  });

  test("detects option helper for exact and equals-form flags", () => {
    const hasOption = extensionModule.__test_hasOption;
    assert.strictEqual(typeof hasOption, "function", "Option detection helper should be exported.");
    assert.strictEqual(hasOption(["serve", "--host", "127.0.0.1"], "--host"), true);
    assert.strictEqual(hasOption(["serve", "--port=8080"], "--port"), true);
    assert.strictEqual(hasOption(["serve", "--hostname=example.test"], "--host"), false);
    assert.strictEqual(hasOption([], "--host"), false);
  });

  test("returns tail suggestion for matching previous line", () => {
    const collect = extensionModule.__test_collectLocalWholeLineSuggestions;
    const text = "<?php\n$userName = $user->name;\n$user";
    const suggestions = collect(text, "$user", 5, 3);
    assert.deepStrictEqual(suggestions, ["Name = $user->name;"]);
  });

  test("returns no whole-line suggestions when prefix is too short", () => {
    const collect = extensionModule.__test_collectLocalWholeLineSuggestions;
    const text = "<?php\nreturn response()->json($payload);\nre";
    const suggestions = collect(text, "re", 2, 3);
    assert.deepStrictEqual(suggestions, []);
  });

  test("dedupes and caps whole-line suggestions", () => {
    const collect = extensionModule.__test_collectLocalWholeLineSuggestions;
    const text = [
      "<?php",
      "return view('a');",
      "return view('a');",
      "return view('b');",
      "return view('c');",
      "return view('d');",
      "return view('e');",
      "return view('f');",
      "ret",
    ].join("\n");
    const suggestions = collect(text, "ret", 3, 3);
    assert.strictEqual(suggestions.length, 5, "Suggestions should be capped at five.");
    assert.deepStrictEqual(suggestions, [
      "urn view('a');",
      "urn view('b');",
      "urn view('c');",
      "urn view('d');",
      "urn view('e');",
    ]);
  });

  test("collects whole-line suggestions for blade mixed line prefixes", () => {
    const collect = extensionModule.__test_collectLocalWholeLineSuggestions;
    const text = [
      "<div>{{ $user->name }}</div>",
      "<div>{{",
    ].join("\n");
    const suggestions = collect(text, "<div>{{", 7, 3);
    assert.deepStrictEqual(suggestions, [" $user->name }}</div>"]);
  });

  test("produces composer diagnostics for missing required fields", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const composerUri = vscode.Uri.joinPath(workspaceFolder.uri, "composer.json");
    const content = "{\n  \"description\": \"Test package\"\n}\n";

    try {
      await vscode.workspace.fs.writeFile(composerUri, Buffer.from(content, "utf8"));
      const doc = await vscode.workspace.openTextDocument(composerUri);
      await vscode.window.showTextDocument(doc);

      const diagnostics = await waitFor(() => {
        const all = vscode.languages.getDiagnostics(composerUri);
        const warnings = all.filter((item) => item.source === "vscode-ls-php-composer");
        return warnings.length >= 2 ? warnings : undefined;
      });
      const messages = diagnostics.map((item) => item.message);

      assert.ok(messages.some((msg) => msg.includes("missing top-level \"name\"")), "Should warn about missing name.");
      assert.ok(messages.some((msg) => msg.includes("missing top-level \"require\"")), "Should warn about missing require.");
    } finally {
      await deleteIfExists(composerUri);
    }
  });

  test("provides composer quick fixes for missing fields", async () => {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(workspaceFolder, "Workspace folder should be available.");

    const composerUri = vscode.Uri.joinPath(workspaceFolder.uri, "composer.json");
    const content = "{\n  \"name\": \"vendor/demo\",\n  \"require\": {\n    \"laravel/framework\": \"^10.0\"\n  },\n  \"require-dev\": {\n    \"laravel/framework\": \"^10.0\"\n  }\n}\n";

    try {
      await vscode.workspace.fs.writeFile(composerUri, Buffer.from(content, "utf8"));
      const doc = await vscode.workspace.openTextDocument(composerUri);
      const editor = await vscode.window.showTextDocument(doc);

      const diagnostics = await waitFor(() => {
        const all = vscode.languages.getDiagnostics(composerUri).filter((item) => item.source === "vscode-ls-php-composer");
        const hasMissingPhp = all.some((item) => item.message.includes("missing \"php\" version constraint"));
        const hasDuplicate = all.some((item) => item.message.includes("duplicated in \"require\" and \"require-dev\""));
        return hasMissingPhp && hasDuplicate ? all : undefined;
      });

      const actions = await vscode.commands.executeCommand(
        "vscode.executeCodeActionProvider",
        composerUri,
        new vscode.Range(0, 0, Math.max(0, editor.document.lineCount - 1), 0),
        vscode.CodeActionKind.QuickFix.value,
      );

      const titles = (actions ?? []).map((item) => item.title);
      assert.ok(
        diagnostics.some((item) => item.message.includes("missing \"php\" version constraint")),
        "Should include missing require.php diagnostic.",
      );
      assert.ok(
        diagnostics.some((item) => item.message.includes("duplicated in \"require\" and \"require-dev\"")),
        "Should include duplicate package diagnostic.",
      );
      assert.ok(
        titles.includes("Add composer \"require.php\" constraint"),
        "Expected quick fix for missing require.php.",
      );
      assert.ok(
        titles.some((title) => title.includes("Remove duplicated \"laravel/framework\" from require-dev")),
        "Expected quick fix for removing duplicated package.",
      );
    } finally {
      await deleteIfExists(composerUri);
    }
  });

  test("builds composer code action previews for missing name", () => {
    const text = "{\n  \"require\": {}\n}\n";
    const previews = extensionModule.__test_buildComposerCodeActionPreviews(text, [
      { source: "vscode-ls-php-composer", code: "composer.missingName", message: "composer.json is missing top-level \"name\"." },
    ]);
    const fix = previews.find((item) => item.title === "Add composer \"name\" field");
    assert.ok(fix, "Expected preview for missing name.");
    const updated = JSON.parse(fix.updatedText);
    assert.strictEqual(updated.name, "vendor/package");
    assert.deepStrictEqual(updated.require, {});
  });

  test("builds composer code action previews for missing require.php", () => {
    const text = "{\n  \"name\": \"vendor/demo\",\n  \"require\": {\n    \"laravel/framework\": \"^10.0\"\n  }\n}\n";
    const previews = extensionModule.__test_buildComposerCodeActionPreviews(text, [
      { source: "vscode-ls-php-composer", code: "composer.missingRequirePhp", message: "composer.json \"require\" is missing \"php\" version constraint." },
    ]);
    const fix = previews.find((item) => item.title === "Add composer \"require.php\" constraint");
    assert.ok(fix, "Expected preview for missing require.php.");
    const updated = JSON.parse(fix.updatedText);
    assert.strictEqual(updated.require.php, "^8.2");
    assert.strictEqual(updated.require["laravel/framework"], "^10.0");
  });

  test("builds composer code action previews for duplicate require-dev package", () => {
    const text = "{\n  \"name\": \"vendor/demo\",\n  \"require\": {\n    \"php\": \"^8.2\",\n    \"laravel/framework\": \"^10.0\"\n  },\n  \"require-dev\": {\n    \"laravel/framework\": \"^10.0\",\n    \"phpunit/phpunit\": \"^11.0\"\n  }\n}\n";
    const previews = extensionModule.__test_buildComposerCodeActionPreviews(text, [
      { source: "vscode-ls-php-composer", code: "composer.duplicatePackageInRequireDev", message: "Package \"laravel/framework\" is duplicated in \"require\" and \"require-dev\"." },
    ]);
    const fix = previews.find((item) => item.title.includes("Remove duplicated \"laravel/framework\" from require-dev"));
    assert.ok(fix, "Expected preview for duplicate require-dev package.");
    const updated = JSON.parse(fix.updatedText);
    assert.ok(!("laravel/framework" in updated["require-dev"]));
    assert.strictEqual(updated["require-dev"]["phpunit/phpunit"], "^11.0");
  });

  test("builds composer code action previews for invalid JSON", () => {
    const text = "{\n  \"name\": \"vendor/demo\",\n  \"require\": {\n}\n";
    const previews = extensionModule.__test_buildComposerCodeActionPreviews(text, [
      { source: "vscode-ls-php-composer", code: "composer.invalidJson", message: "Invalid JSON: unexpected end of input" },
    ]);
    const fix = previews.find((item) => item.title === "Replace with minimal valid composer.json");
    assert.ok(fix, "Expected preview for invalid JSON.");
    const updated = JSON.parse(fix.updatedText);
    assert.strictEqual(updated.name, "vendor/package");
    assert.deepStrictEqual(updated.require, { php: "^8.2" });
  });
});
