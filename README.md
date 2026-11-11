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

The current prototype uses **sample data only**. Folder tabs represent demo datasets; no local files are opened. It includes folder-style file tabs, a virtualized table, resizable columns, scalar-column sorting, name/city filtering, a List/Struct inspector, and a SQL editor with two supported demo queries:

```sql
SELECT * FROM current_file LIMIT 100;
SELECT * FROM current_file WHERE false;
```

Queries run off the UI thread, with cancellation and stale-result rejection. Preview retention is capped at 8,192 rows / 16 MiB; this is not yet an infinite-loading result cache. Sorting and filtering happen before the preview limit.

## Workspace

- `rowglide-app`: typed Arrow results, query contracts and cancellation.
- `rowglide-core`: deterministic mock backend and pull-based batches.
- `rowglide-ui`: GPUI Kit components, interactions and application entry point.

## UI layout

`rowglide-ui/src` uses a small set of modules:

- `main.rs`: native startup and backend injection.
- `workspace/mod.rs`: shared view state, event wiring, and main layout.
- `workspace/controls.rs`: file tabs, browse toolbar, SQL editor, and status bar.
- `workspace/query.rs`: query execution, cancellation, and session switching.
- `workspace/state.rs`: per-file browse/SQL state and bounded result cache.
- `workspace/inspector.rs`: selected-cell panel and nested-value tree.
- `table.rs`: virtualized table delegate and sort events.
- `cells.rs`: Arrow value summaries and nested-value access.

Workspace owns shared GUI controls and the active task. Each `TabState` retains
browse inputs, SQL drafts, mode, results and inspector state. Switching a tab or
mode cancels pending work and restores its session. SQL mode disables browse
filters and column sorting. Up to three inactive previews are retained alongside
the active preview, each bounded to 8,192 rows / 16 MiB. Eviction keeps inputs;
use **Reload result** to rerun the last applied query. Scroll positions, column
widths and editor undo history are not yet saved per tab.
Table rendering and cell helpers remain independent of workspace coordination.

## Checks

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
