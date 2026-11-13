# Rowglide

A native data browser built with Rust and [GPUI Kit](https://github.com/longbridge/gpui-kit).

## Run

```sh
cargo run -p rowglide-app --bin rowglide
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

SQL executes the submitted query without an added row limit; only its own
LIMIT/OFFSET govern the result. Background workers continuously write typed Arrow
batches to a private temporary IPC file and publish available rows incrementally.
Scrolling reads generated rows through a bounded memory cache; it does not trigger
query pagination. Missing batches load asynchronously with a temporary placeholder.

Each SQL result has a 64 MiB Arrow cache, a 2 GiB temporary-file budget and a
65,536-batch index budget. Exceeding a budget is an explicit failure, never successful
truncation. Cancelled or failed queries retain committed rows marked **Partial result**.
Transport, decoding, inspector references and engine allocations are additional to
the cache budget. Releasing a result cancels its workers and removes its temporary
file after they exit.

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

- `rowglide-app`: executable entry point; selects and constructs the backend.
- `rowglide-ui`: UI library; GPUI startup, window layout and interaction state.
- `rowglide-core`: shared models, backend contracts, pagination and engine implementations.

Dependencies flow from app to UI/core, and from UI to core. Core never depends
on app or GUI crates. App injects a concrete backend through
`rowglide_ui::run(Arc<dyn Backend>)`; UI consumes the shared contract.

## Code layout

`rowglide-app/src/main.rs` is the executable composition root.

`rowglide-core/src` groups the domain and backend responsibilities:

- `lib.rs`: public exports for consumers.
- `model.rs`: source metadata, query descriptions and retained Arrow results.
- `backend.rs`: backend/cursor interfaces, cancellation and shared errors.
- `pagination.rs`: validated Browse ranges and bounded page collection.
- `session.rs`: cancellable, capacity-one query events and background cursor cleanup.
- `result/`: continuous Arrow IPC storage, bounded memory cache and asynchronous rereads.
- `duckdb/`: private file registry, SQL construction and Arrow worker transport.
- `mock.rs`: deterministic fixture backend and its behavior tests.

`rowglide-ui/src` encapsulates the native presentation:

- `lib.rs`: public `run` entry, GPUI initialization, theme and window creation.
- `workspace/mod.rs`: shared view state, event wiring, and main layout.
- `workspace/controls.rs`: file tabs, browse toolbar, SQL editor, and status bar.
- `workspace/query.rs`: query execution, cancellation, and session switching.
- `workspace/sql.rs`: streamed SQL progress, cache notifications and terminal status.
- `workspace/files.rs`: native file selection and background schema inspection.
- `workspace/state.rs`: per-file browse/SQL state and bounded result cache.
- `workspace/inspector.rs`: selected-cell panel and nested-value tree.
- `table.rs`: virtualized table delegate and sort events.
- `cells.rs`: Arrow value summaries and nested-value access.

Workspace owns shared GUI controls and the active task. Each `TabState` retains
browse inputs, SQL drafts, mode, results and inspector state. Switching a tab or
mode cancels pending work and restores its session. SQL mode disables browse
filters and column sorting. Up to three inactive results are retained alongside
the active result. Browse pages retain at most 16 MiB; SQL results use the per-result
cache budgets above. These are not global process quotas. Eviction releases result
buffers and temporary files while keeping inputs;
use **Reload result** to rerun the last applied query. Scroll positions, column
widths and editor undo history are not yet saved per tab.
Table rendering and cell helpers remain independent of workspace coordination.

Core uses `thiserror` for `QueryError`. Arrow, I/O, DuckDB and SQL parsing
failures retain typed causes; cache I/O also keeps its storage context. Cancellation
remains distinct from failure. UI status snapshots format errors for display.

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

Rust 1.99 or newer is the current supported development baseline. Root Cargo.toml
stores project information in `[workspace.metadata.project]`; these values are
descriptive metadata, not automatically applied Cargo package fields.
`[workspace.package]` contains only the version, edition, Rust version and publish
settings actually inherited by the crates. Each crate also inherits strict
workspace lints. Project code forbids
unsafe code, unnecessary qualifications and unreachable public items.

Build an optimized executable with `cargo build --release -p rowglide-app`. The
release profile uses optimization level 3, Thin LTO, one codegen unit and strips
debug information. Panic unwinding is retained.

## License

Copyright 2026 Yousa-Mirage. Licensed under Apache-2.0; see [LICENSE](LICENSE).
