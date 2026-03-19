const assert = require("node:assert");
const path = require("node:path");
const vscode = require("vscode");

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
      "vscode-ls-php.writeXdebugTemplate",
      "vscode-ls-php.refreshTests",
      "vscode-ls-php.startContinuousTesting",
      "vscode-ls-php.stopContinuousTesting",
      "vscode-ls-php.openProfilingDashboard",
      "vscode-ls-php.resetProfilingMetrics",
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

      assert.ok(iniContent.includes("xdebug.mode=debug"), "xdebug.ini should contain debug mode setting.");
      assert.ok(launchContent.includes("VSCode LS PHP: Listen for Xdebug"), "launch.json should contain VSCode LS PHP debug configuration.");
    } finally {
      try {
        await vscode.workspace.fs.delete(xdebugIni, { useTrash: false });
      } catch {}
      try {
        await vscode.workspace.fs.delete(launchJson, { useTrash: false });
      } catch {}
      try {
        await vscode.workspace.fs.delete(vscodeDir, { useTrash: false, recursive: true });
      } catch {}
    }
  });
});
