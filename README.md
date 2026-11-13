# Rowglide

A native data browser built with Rust and [GPUI Kit](https://github.com/longbridge/gpui-kit).

## Run

```sh
cargo run -p rowglide-ui --bin rowglide
```

macOS development bundle (requires Rust and Xcode command-line tools):

```sh
sh scripts/bundle-macos.sh
open target/Rowglide.app
```

Use **Open files…** to select local `.parquet`, `.pq`, or `.csv` files. Files open
in folder tabs with independent Browse and SQL sessions. Browse supports text
search across string columns, scalar sorting, Limit and Offset. SQL supports a
single SELECT/WITH query against `current_file`, including aggregates and joins.

Browse uses explicit pages: **Limit** is the page size (default 1,000) and
**Offset** is the zero-based start. **Previous / Next** replace the page and
reset vertical scrolling. Apply edited controls before navigating; clearing
Limit restores the default. Sorting starts at Offset 0. Pages belong to their
file tabs and remain separate from SQL results.

DuckDB streams typed Arrow batches through a one-batch channel. Browse requests
one extra row to detect the next page without a COUNT scan. There is no hidden
row-count ceiling: a 10,000-row Limit returns up to 10,000 matching rows. A page
that exceeds the 16 MiB retained-buffer budget fails explicitly and asks for a
smaller Limit. The GUI presents each completed page. Sorting/filtering precede
pagination; sorting ties use source order. Deep offsets can require rescanning,
and files must remain unchanged for consistent navigation.

SQL currently retains the bounded preview (8,192 rows / 16 MiB, with an outer
8,193-row query cap). Complete SQL result streaming and bounded disk caching are
planned separately; incremental GUI presentation is not yet implemented.

Each query uses a temporary in-memory connection, at most four execution threads,
a 512 MB DuckDB memory limit, and a private temporary spill directory capped at
2 GB. These engine limits are not process-wide memory guarantees. A shared gate
serializes query engines. Cancellation interrupts DuckDB, and cursor drop joins
the worker and releases its connection and spill directory.

CSV delimiter/header/type inference uses a bounded sample (20,480 rows). Manual
CSV options and non-UTF-8 input conversion are not implemented; parsing failures
are displayed. SQL is validated as one query using sqlparser's DuckDB dialect;
some DuckDB-specific syntax may not yet be accepted. This is a local SQL viewer,
not a sandbox for untrusted SQL. Extensions are bundled for Parquet; automatic
extension downloading is disabled.

The Rust DuckDB dependency is pinned to `1.20000.1` with bundled native code and
Parquet support; Arrow uses version 60. Building requires a C++ toolchain and the
first build takes longer. The deterministic mock backend remains available for
tests but is no longer the application's default.

## Workspace

- `rowglide-app`: typed Arrow results, query contracts and cancellation.
- `rowglide-core`: DuckDB file queries, bounded streaming, and a mock backend for tests.
- `rowglide-ui`: GPUI Kit components, interactions and application entry point.

## UI layout

`rowglide-ui/src` uses a small set of modules:

- `main.rs`: native startup and backend injection.
- `workspace/mod.rs`: shared view state, event wiring, and main layout.
- `workspace/controls.rs`: file tabs, browse toolbar, SQL editor, and status bar.
- `workspace/query.rs`: query execution, cancellation, and session switching.
- `workspace/files.rs`: native file selection and background schema inspection.
- `workspace/state.rs`: per-file browse/SQL state and bounded result cache.
- `workspace/inspector.rs`: selected-cell panel and nested-value tree.
- `table.rs`: virtualized table delegate and sort events.
- `cells.rs`: Arrow value summaries and nested-value access.

Workspace owns shared GUI controls and the active task. Each `TabState` retains
browse inputs, SQL drafts, mode, results and inspector state. Switching a tab or
mode cancels pending work and restores its session. SQL mode disables browse
filters and column sorting. Up to three inactive previews are retained alongside
the active result, each bounded to 16 MiB. Eviction keeps inputs;
use **Reload result** to rerun the last applied query. Scroll positions, column
widths and editor undo history are not yet saved per tab.
Table rendering and cell helpers remain independent of workspace coordination.

## Checks

Generate API and internal implementation documentation with:

```sh
cargo doc --workspace --no-deps --document-private-items
```

The generated crate pages describe shared contracts, backend worker lifetimes,
Browse page boundaries and per-tab UI state. Rust types and named functions,
including private helpers, carry English documentation alongside their code.

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See [dev.md](dev.md) for progress, known limitations and the next stages. Version management uses **jj**; each independent modification is its own change.


## Just commands

Run `just` to list commands: `just dev`, `just check`, `just test`, `just fmt`,
`just fmt-check`, `just lint`, `just ci`, and `just bundle-macos`.
`just ci` runs formatting, compilation, tests and Clippy. Workspace Clippy rules
ban `unwrap()` and `expect()` in both application and test code; `just lint`
treats every Clippy warning as an error.

## Build configuration

Rust 1.99 or newer is the current supported development baseline. Project metadata is defined only in the root Cargo.toml. Each crate
inherits build settings and strict workspace lints. Project code forbids
unsafe code, unnecessary qualifications and unreachable public items.

Build an optimized executable with `cargo build --release -p rowglide-ui`. The
release profile uses optimization level 3, Thin LTO, one codegen unit and strips
debug information. Panic unwinding is retained.

## License

Copyright 2026 Yousa-Mirage. Licensed under Apache-2.0; see [LICENSE](LICENSE).
