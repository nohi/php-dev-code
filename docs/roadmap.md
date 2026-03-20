# Roadmap

## Phase 1: Foundation (2-4 weeks)

- [x] Stable extension activation and server restart flow.
- [x] Rust LSP skeleton with initialize/hover/diagnostic stubs.
- [x] CI matrix for all required OS and architectures.
- [x] Benchmark harness for latency and RSS baselines.

## Phase 2: Core IntelliSense (4-8 weeks)

- [x] Workspace indexer and symbol table.
- [x] Completion with context-aware ranking.
- [x] Automatic import insertion with alias support.
- [x] Signature help and rich hover with PHP manual linking.
- [x] Basic rename/go-to-definition/references.

### Phase 2 Reallocated Backlog (Status + Priority)

- [x] [High] Auto-import with alias insertion directly from completion accept flow.
- [-] [Medium] Signature help support for PHP 8 named arguments.
- [-] [Medium] Go to Implementations expansion for traits and overridden functions.
- [x] [Medium] PHPDoc generator on `/**` trigger with keyword completion.
- [-] [Low] Rich localized hover/tooltips (localized descriptions, styled headers, doc URLs).
- [ ] [Low] PHAR content navigation and symbol/index integration.
- [ ] [Low] Linked editing for local variable rename-in-place workflows.
- [ ] [Low] Local AI whole-line suggestions integration.

## Phase 3: Smart Analysis (4-8 weeks)

- [x] Type system enhancements for generics.
- [x] Psalm/PHPStan annotations and PHPStorm metadata.
- [x] Diagnostics engine with quick fixes and code actions.
- [x] Inlay hints, code lens, occurrence highlighting.

### Phase 3 Reallocated Backlog (Status + Priority)

- [-] [High] Advanced code actions for namespace resolution, getter/setter generation, and interface implementation.
- [-] [High] Extended code fixes for expression refactoring and broader common issue auto-fixes.
- [x] [High] Per-directory and per-rule diagnostics configurability.
- [x] [Medium] Deprecation diagnostics for libraries and user code.
- [x] [Medium] PHP version compatibility diagnostics.
- [-] [Medium] Native `@mixin` and additional advanced annotation support across IntelliSense.
- [-] [Medium] Inlay hints expansion for by-ref arguments and richer type annotations.
- [-] [Low] Significant semantic rule-set expansion beyond current baseline.
- [x] [Low] TODO highlighting in comments and doc comments.

## Phase 4: Frameworks and Mixed Languages (4-8 weeks)

- [x] Laravel Blade parsing, formatting, and completions.
- [x] Eloquent magic, routes, services, facades, config keys.
- [x] Mixed PHP/HTML/JS/CSS formatting and semantic support.

### Phase 4 Reallocated Backlog (Status + Priority)

- [ ] [High] Blade completion expansion for view/component IDs, `x-` tags, `livewire:` tags, attributes, and component variables.
- [ ] [High] Livewire component analysis/completion support (actions/properties).
- [ ] [Medium] Blade section completion between `@yield` and `@section`.
- [ ] [Medium] Laravel `ide.json` support for custom completions/directives/components.
- [-] [Medium] Rich Laravel model/query/view/config/path tooltips with detailed metadata.
- [ ] [Medium] Composer ecosystem integration (`composer.json` IntelliSense/diagnostics/actions).
- [-] [Medium] Full PSR-12 formatter compliance mode.
- [x] [Medium] Format-on-paste / format-on-type / format-on-save behavior controls.
- [ ] [Medium] Laravel built-in dev server assisted run/debug workflow.
- [ ] [Low] Multiple style presets (`PER`, `PSR-12`, `PSR-2`, `Allman`, `K&R`, `Laravel`, `Drupal`, `WordPress`).
- [ ] [Low] Custom formatting rule configuration.
- [ ] [Low] Workspace batch formatting for multiple `.php` files with preview.

## Phase 5: Tooling (3-6 weeks)

- [x] Debug launcher and Xdebug integration path.
- [x] Test explorer integration for PHPUnit and Pest.
- [x] Continuous testing and profiling visualization.

### Phase 5 Reallocated Backlog (Status + Priority)

- [ ] [High] Built-in web server auto-start as part of debug launch.
- [ ] [Medium] Debug adornments (inline value display) in editor.
- [ ] [Medium] DBGp Proxy support.
- [ ] [Medium] Multi-session debugging orchestration.
- [ ] [Medium] Compound launch templates for multi-server startup.
- [-] [Medium] Remote server debugging presets with path mappings.
- [ ] [Low] Debug value editing and watch tooltip enhancements.
- [ ] [Low] Xdebug profile file inspection workflow integration.
- [ ] [Medium] Pest High-Order Tests intelligence and analysis support.
- [-] [Medium] PHPUnit test-case profiling results visualization.
- [ ] [Medium] DataSet-level test discovery and per-dataset run support.
- [x] [Low] In-editor latest test result gutter/margin indicators.
- [ ] [Low] Test run hooks (`preTask` / `postTask`) integration.

## Performance and Quality Gates

- [x] Completion p95 latency target: <= 30 ms in warm cache.
- [x] Hover p95 latency target: <= 20 ms in warm cache.
- [x] Full workspace initial index target: configurable by project size.
- [ ] No regressions accepted without benchmark delta review.
