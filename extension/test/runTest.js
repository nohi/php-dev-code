const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const { runTests } = require("@vscode/test-electron");

async function main() {
  let userDataDir;
  let extensionsDir;
  let cacheDir;
  try {
    process.env.VSCODE_LS_PHP_TEST = "1";
    const extensionDevelopmentPath = path.resolve(__dirname, "..");
    const extensionTestsPath = path.resolve(__dirname, "./suite/index");
    const workspacePath = path.resolve(extensionDevelopmentPath, "..", "sample-php-project");
    userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "vscode-ls-php-tests-"));
    extensionsDir = fs.mkdtempSync(path.join(os.tmpdir(), "vscode-ls-php-exts-"));
    cacheDir = fs.mkdtempSync(path.join(os.tmpdir(), "vscode-ls-php-cache-"));

    await runTests({
      version: "1.90.0",
      cachePath: cacheDir,
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        workspacePath,
        "--disable-extensions",
        "--disable-updates",
        `--user-data-dir=${userDataDir}`,
        `--extensions-dir=${extensionsDir}`,
      ],
    });
  } catch (err) {
    console.error("Failed to run extension tests");
    console.error(err);
    process.exit(1);
  } finally {
    if (userDataDir) {
      fs.rmSync(userDataDir, { recursive: true, force: true });
    }
    if (extensionsDir) {
      fs.rmSync(extensionsDir, { recursive: true, force: true });
    }
    if (cacheDir) {
      fs.rmSync(cacheDir, { recursive: true, force: true });
    }
  }
}

main();
