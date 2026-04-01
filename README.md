# vscode-ls-php

VS Code extension project for high-performance PHP language features.

## Monorepo Layout

- `extension/`: VS Code extension (TypeScript)
- `server/`: Language Server (Rust)
- `.github/workflows/`: CI for cross-platform build
- `docs/`: architecture and planning docs
   - `docs/formatting-range-spec.md`: explicit range formatting behavior and boundary rules

## Quick Start

1. Install Node.js 20+ and Rust stable.
2. Build server:
   - `cd server`
   - `cargo build`
3. Build extension:
   - `cd extension`
   - `npm install`
   - `npm run compile`
4. Launch Extension Development Host from VS Code.
5. Run integration test:
   - `cd extension`
   - `npm test`
6. Run performance benchmark gate:
   - `cargo build --release --manifest-path server/Cargo.toml`
   - `npm run bench:gate`

## Performance Benchmarking

- Observational run (no threshold enforcement):
   - `npm run bench:server`
- Gate run (fails on threshold breach):
    - `npm run bench:gate`
- Gate run with baseline delta review:
   - `node scripts/lsp-benchmark.js --fail-on-threshold --require-baseline --baseline scripts/benchmark-baseline.json`

Default gate thresholds:

- Completion p95: <= 30 ms
- Hover p95: <= 20 ms
- Completion p95 regression vs baseline: <= 5 ms
- Hover p95 regression vs baseline: <= 5 ms
- Index duration regression vs baseline: <= 1000 ms

Optional index-duration target:

- Set `VSCODE_LS_PHP_INDEX_TARGET_MS` to enforce maximum initial indexing duration.
- Example (PowerShell):
   - `$env:VSCODE_LS_PHP_INDEX_TARGET_MS="5000"; npm run bench:gate`

## Current Status

This repository currently contains the initial scaffold:

- VS Code extension activates on PHP files.
- Rust Language Server starts over stdio.
- LSP foundation includes hover, didOpen/didChange/didSave/didClose handling, and diagnostics publishing.
- Document symbols and workspace symbol search are available with a lightweight PHP symbol extractor.
- Basic completion is available from PHP keywords and indexed workspace symbols.
- Basic go-to-definition resolves symbol names across open indexed documents.
- Basic find-references resolves identifier occurrences across open indexed documents.
- Basic rename refactoring returns workspace edits across open indexed documents.
- Rename now supports prepare-rename validation and placeholder ranges.
- References and rename now ignore matches inside comments and string literals.
- Document highlight now marks symbol occurrences in the current file.
- Server now performs initial workspace-wide PHP indexing (excluding heavy folders like vendor/node_modules/.git).
- Workspace indexing is incrementally updated when .php files are created/changed/deleted.
- Completion candidates are ranked with local-file and symbol-kind priority.
- Completion is context-aware for use statements and includes local variable suggestions.
- use statement completion now surfaces namespace-qualified symbol labels (FQN).
- Namespace parsing supports both inline and block namespace declarations.
- Go-to-definition now resolves symbols with namespace and use-alias context.
- References and rename also use namespace/use-aware resolution to reduce same-name collisions.
- use group import syntax parsing is supported for alias resolution.
- Minimal code action suggests adding missing `use` imports from indexed workspace symbols.
- Hover now surfaces generic template parameters from `@template` / `@psalm-template` / `@phpstan-template` docblocks.
- CI matrix for Windows/macOS/Linux with amd64/arm targets is prepared.
- Benchmark harness is available via `scripts/lsp-benchmark.js` and wired to CI performance-gate checks for completion/hover p95.
- Phase 5 tooling baseline is available in the extension:
   - Xdebug integration path with command-based launch and `.vscode/launch.json` / `.vscode/xdebug.ini` template generation.
   - Test explorer integration for PHPUnit/Pest discovery under `tests/**/*.php` with run/debug profiles.
   - Continuous testing mode that re-runs tests when test files change.
   - Profiling dashboard for LSP request latency and recent test-run durations.

The full feature list should be implemented incrementally by milestones documented in `docs/roadmap.md`.

## Formatter Settings

The extension exposes lightweight formatter controls through `vscodeLsPhp.*` settings:

- `vscodeLsPhp.formatStylePreset` (default: `default`)
  - Allowed values: `default`, `PSR-12`, `PSR-2`, `PER`, `K&R`, `Allman`, `Laravel`, `Drupal`, `WordPress`
  - Presets currently map to normalization profiles for existing lightweight rules (blank-line compaction, Blade directive spacing, trailing whitespace trimming), not full brace-style reformatting.
- `vscodeLsPhp.formatMaxBlankLines` (number, default: `2`, minimum: `0`)
- `vscodeLsPhp.formatBladeDirectiveSpacing` (boolean, default: `true`)
- `vscodeLsPhp.formatTrimTrailingWhitespace` (boolean, default: `true`)
