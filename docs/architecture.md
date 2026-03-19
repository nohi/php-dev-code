# Architecture

## Goals

- High-performance and low-memory language intelligence in Rust.
- Cross-platform binaries for Windows, macOS, Linux on amd64 and arm.
- Feature-rich VS Code integration via extension host.

## Components

- Extension Host (`extension/`):
  - VS Code activation, commands, settings, UX integration.
  - Starts and supervises language server process.
  - Bridges protocol extensions for Laravel, Blade, formatter, debugger UI.

- Language Server (`server/`):
  - Core parser, indexing, type inference, diagnostics, completion, code actions.
  - Incremental analysis with on-disk cache and memory-aware eviction.
  - Symbol graph for go-to-definition/references/implementations.

## Runtime Model

- Transport: stdio LSP between extension and server.
- Indexing:
  - Initial workspace scan with prioritized open files.
  - Incremental updates from didOpen/didChange/didSave and file watcher events.
- Memory:
  - Arena allocations for AST/HIR when possible.
  - Separate hot caches (current file and dependencies) from cold caches.
  - Bounded LRU + generation markers for reusable type results.

## Formatting Semantics

- Range formatting behavior is specified in `docs/formatting-range-spec.md`.
- Key property: for partial-line ranges, text outside requested start/end columns is preserved.
- Full-document formatting and range formatting intentionally differ in replacement scope.

## Milestone Mapping

- M1: Server process lifecycle, sync, hover, diagnostics skeleton.
- M2: Completion, import resolution, signature help, symbol search.
- M3: Navigation suite, rename, references, implementations.
- M4: Formatter and code actions/fixes.
- M5: Laravel + Blade + metadata ecosystem support.
- M6: Testing explorer, debugging integration, profiling pipeline.

## Packaging Strategy

- Build target-specific server binaries in CI matrix.
- Publish extension with platform bundles and fallback `serverPath` override.
- Keep protocol compatibility versioned between extension and server.
