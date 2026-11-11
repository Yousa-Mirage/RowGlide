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

- `rowglide-core`: typed Arrow results, query contracts and cancellation.
- `rowglide-backend`: deterministic mock backend and pull-based batches.
- `rowglide-ui`: GPUI Kit components, interactions and application entry point.

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
