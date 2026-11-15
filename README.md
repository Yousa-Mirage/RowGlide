# Rowglide

A native data browser built with Rust and [GPUI Kit](https://github.com/longbridge/gpui-kit).

## Welcome and recent files

The empty workspace shows a welcome page with **Open files** and up to five
recent files. Each row shows the file name and parent directory; click it to reopen
through the normal background file-opening workflow. Closing the last tab returns
to this page, and file drops work here too.

Successful opens move canonical paths to the front, removing duplicates. History
persists in `rowglide/recent-files.json` under the platform's local user data
directory. Only file paths are stored. Missing files remain listed and report the
normal opening error when selected; failed opens do not enter the history.
History I/O runs in the background and does not prevent opening files if it fails.

## External file changes

One background task checks opened files every three seconds using file size and
modification time (plus device/inode on Unix). No content hashing, filesystem
watcher dependency or automatic data reload is involved.

Changed or inaccessible files get a **Changed** tab label and a persistent warning.
Retained results remain viewable, active reads are cancelled when detected, and
new queries require **Reload file**. Reload inspects the source in the background;
failure keeps the old data and warning. Success refreshes schema/source metadata,
resets to the first Browse page, clears old result sessions and preserves the SQL
draft without executing it.

The backend also checks freshness after engine admission and before successful
query completion. These are best-effort checks, not snapshot isolation: metadata
polling can miss edits that preserve observed attributes and cannot prevent an
external writer from changing a file during a scan. Avoid querying files while
another program is actively writing them.

## Download builds

The **Build binaries** GitHub Actions workflow builds every pushed tag. It also
supports **Actions → Build binaries → Run workflow** once the workflow is on the
default branch. Ordinary branch pushes do not trigger it.

After a successful job, download its artifact from the workflow run's **Artifacts**
section. Artifacts are retained for 30 days and contain a second archive:

| Platform | Artifact | Contents |
| --- | --- | --- |
| Linux x64 | `rowglide-x86_64-unknown-linux-gnu` | `.tar.gz` with `rowglide` |
| Windows x64 | `rowglide-x86_64-pc-windows-msvc` | `.zip` with `rowglide.exe` |
| macOS Apple Silicon | `rowglide-aarch64-apple-darwin` | `.tar.gz` with `Rowglide.app` |
| macOS Intel | `rowglide-x86_64-apple-darwin` | `.tar.gz` with `Rowglide.app` |

Extract both archives before running. Unix archives retain executable permissions.
Each package includes the license, this README, and `build-info.json` identifying
the source commit and tag/branch. Builds use stable Rust, `Cargo.lock`, and the
release profile; they do not publish a GitHub Release.

These are unsigned builds without installers or macOS notarization. Linux builds
target Ubuntu 24.04 and need compatible system libraries and graphics drivers;
they are not self-contained AppImages. Windows may require the Microsoft Visual
C++ Redistributable for x64. Cross-platform startup still needs manual validation.

## Run

```sh
cargo run -p rowglide-app --bin rowglide
```

Pass file paths to open them in tabs at startup:

```sh
rowglide "data/example.parquet" "data/another file.pq"
rowglide -- -starts-with-a-hyphen.parquet
cargo run -p rowglide-app --bin rowglide -- "data/example.parquet"
```

With no paths, the usual empty window opens. Paths are inspected in the background;
duplicates reuse tabs, the last successfully opened file becomes active, and file
errors are shown in the UI. `--help` and `--version` exit without starting the GUI.
Startup arguments must be valid UTF-8; invalid encoding is reported without
replacing filename characters or opening any files.
On Windows, the executable is `rowglide.exe`; selecting it through **Open with**
can now pass a file path to the application. Each launch creates its own instance;
this does not register file associations or forward files to an existing window.

macOS development bundle (requires Rust and Xcode command-line tools):

```sh
sh scripts/bundle-macos.sh
open target/Rowglide.app
```

Use **Open files…** to select local `.parquet`, `.pq`, or `.csv` files. Files open
in folder tabs with independent Browse and SQL sessions. Browse supports text
search across string columns, scalar sorting, Limit and Offset. SQL supports a
single SELECT/WITH query against `current_file`, including aggregates and joins.
The status bar shows whole-process resident memory (RSS), sampled in the background
once per second and displayed in MiB/GiB. An unavailable reading appears as `—`.

## Theme

Rowglide follows the operating system's light or dark appearance. The matching
GPUI Kit theme is applied before the workspace is created, and changing the
system appearance updates the open window immediately. There is no separate
in-app theme selector or persisted override; app-owned surfaces and GPUI Kit
controls use the same active semantic theme.

## Copying cells

Right-click a table cell or an inspector node and choose **Copy value** or
**Copy as JSON**. Copying uses the original Arrow value, including full strings
and numeric precision; it never copies the truncated display summary. Metadata's
**Copy full value** button uses the same background serialization path.

- Value mode leaves scalar strings unquoted; JSON mode escapes and quotes them.
- Lists become JSON arrays; structs become objects. Maps become arrays of
  `{"key": ..., "value": ...}` pairs to preserve duplicate and non-string keys.
- Dictionary values are decoded. Null becomes `null`; empty strings remain empty
  in value mode and `""` in JSON mode.
- Binary becomes reversible `0x` hexadecimal, including metadata binary values
  whose preview may show readable UTF-8. Dates/times, durations, decimals and
  nonfinite floats use Arrow text, quoted in JSON mode. Finite numbers retain
  full precision; JSON consumers must support large integers to preserve them.
- Unsupported types and duplicate struct field names produce an explicit error.

Serialization runs off the GUI thread, with a 16 MiB output limit and maximum
nesting depth of 64. SQL cache misses await a dedicated batch reply, with a 30-second
copy timeout. Menus retain the original target across tab/query changes. A new
copy cancels the previous copy; closing the workspace cancels pending work.
Failure leaves the clipboard unchanged. Copy progress and errors appear separately
from query status; values are never silently truncated.

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
LIMIT/OFFSET govern the result. Background workers continuously pull typed Arrow
batches and publish retained rows incrementally. Small results stay in memory and
create no result file. All tabs share a `ResultStore`: pressure first evicts
reloadable batches, then incrementally spills least-recently-used results into
private IPC files. Scrolling reads generated rows, without triggering query
pagination; cache misses are loaded asynchronously.

The application-wide SQL memory budget defaults to `min(4 GiB, total RAM / 4)`;
unavailable RAM detection falls back to 512 MiB. The shared IPC disk budget is
32 GiB. These are per-application-instance limits, separate from DuckDB execution
memory and spill. The status bar continues to show only whole-process RSS.

Optional startup overrides use positive integers in binary units:

- `ROWGLIDE_MEMORY_LIMIT_MIB=4096`: retain up to 4 GiB across SQL results.
- `ROWGLIDE_DISK_LIMIT_GIB=32`: allow up to 32 GiB of SQL IPC storage.

Overrides can exceed automatic defaults; malformed or overflowing values fail
startup explicitly. After spilling, newly produced batches go directly to disk;
read-back caches retain at most 64 MiB per result (or one larger batch), subject
to the shared budget. Memory-only batches remain protected until persisted.

Each result has a 65,536-batch index ceiling. Shared byte budgets fail explicitly,
never by successful truncation. Cancelled or failed queries retain committed rows
marked **Partial result**. Transport, decoding, inspector references, Browse pages
and engine allocations are additional to the retained-buffer budget. Releasing a
result cancels its workers; file bytes, including partial writes, remain charged
until background deletion succeeds. Failed cleanup retains its charge for the
store lifetime.

SQL result files and DuckDB spill directories live under the per-user cache
directory, in `rowglide/temporary-v1`. Each resource holds an OS file lock for its
entire lifetime. Startup recovery runs in the background and removes abandoned
resources only after acquiring their locks; other running instances remain safe.
A root lock coordinates creation and cleanup, including interrupted creation.
Released resources are deleted in the background. Failed deletion is logged and
can be retried at the next startup; an abrupt exit can leave files until then.

Set `ROWGLIDE_CACHE_DIR` to choose a different cache location; Rowglide creates
its `temporary-v1` subdirectory there. This is a dedicated scratch location, not
a place to store source files. Recovery ignores unrelated names and symbolic links.
Old unmarked files in the system temporary directory are not automatically deleted.

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

- `rowglide-app`: executable entry point; parses startup paths and selects and constructs the backend.
- `rowglide-ui`: UI library; GPUI startup, window layout and interaction state.
- `rowglide-core`: shared models, backend contracts, pagination and engine implementations.

Dependencies flow from app to UI/core, and from UI to core. Core never depends
on app or GUI crates. App injects a concrete backend and shared result store through
`rowglide_ui::run(backend, results, files)`; UI consumes the shared contracts and
opens startup paths through the same background flow as its file picker. Only app
depends on argh, using `#[derive(FromArgs)]` with only the help feature enabled.

## Code layout

`rowglide-app/src/main.rs` is the executable composition root. `arguments.rs`
parses startup paths without filesystem access, and `config.rs` validates cache
budgets and environment overrides.

`rowglide-core/src` groups the domain and backend responsibilities:

- `lib.rs`: public exports for consumers.
- `model.rs`: source metadata, query descriptions and retained Arrow results.
- `backend.rs`: backend/cursor interfaces, cancellation and shared errors.
- `recent.rs`: bounded recent-file history and atomic preference persistence.
- `pagination.rs`: validated Browse ranges and bounded page collection.
- `parquet.rs`: Parquet summary/detail contracts.
- `session.rs`: cancellable, capacity-one query events and background cursor cleanup.
- `result/`: shared memory-first result storage, incremental IPC spill and asynchronous rereads.
- `temporary.rs`: leased scratch directories, background deletion and crash recovery.
- `clipboard.rs`: bounded, cancellable Arrow value serialization.
- `duckdb/`: private file registry, SQL construction, Arrow worker transport and Parquet details.
- `mock.rs`: deterministic fixture backend and its behavior tests.

`rowglide-ui/src` encapsulates the native presentation:

- `lib.rs`: public `run` entry, GPUI initialization, theme and window creation.
- `theme.rs`: system light/dark synchronization and window appearance observer.
- `workspace/mod.rs`: shared view state, event wiring, and main layout.
- `workspace/controls.rs`: file tabs, browse toolbar, SQL editor, and status bar.
- `workspace/query.rs`: query execution, cancellation, and session switching.
- `workspace/sql.rs`: streamed SQL progress, cache notifications and terminal status.
- `workspace/files.rs`: native file picker, startup paths and background schema inspection.
- `workspace/welcome.rs`: welcome layout and asynchronous recent-file loading.
- `workspace/freshness.rs`: lightweight source checks, persistent warnings and explicit reload.
- `workspace/state.rs`: per-file browse/SQL state and bounded result cache.
- `workspace/inspector.rs`: selected-cell panel and nested-value tree.
- `workspace/file_info/`: Parquet detail panel and metadata table.
- `clipboard.rs` and `workspace/clipboard.rs`: copy targets/menus and independent copy task.
- `workspace/memory.rs` and `workspace/sizing.rs`: RSS sampling and first-batch column sizing.
- `table.rs` and `table/sizing.rs`: virtualized table delegate and column measurement.
- `cells.rs`: Arrow value summaries and nested-value access.
- `i18n.rs`: system language selection and translation boundaries.

Workspace owns shared GUI controls and the active task. Each `TabState` retains
browse inputs, SQL drafts, mode, results and inspector state. Switching a tab or
mode cancels pending work and restores its session. SQL mode disables browse
filters and column sorting. Up to three inactive results are retained alongside
the active result. Browse pages retain at most 16 MiB; SQL results use the per-result
cache budgets above. These are not global process quotas. Eviction releases result
buffers and temporary files while keeping inputs;
use **Reload result** to rerun the last applied query. Column widths are saved
independently for each tab's Browse and SQL session; scroll positions and editor
undo history are not yet saved per tab.
Table rendering and cell helpers remain independent of workspace coordination.

Core uses `thiserror` for `QueryError`. Arrow, I/O, DuckDB and SQL parsing
failures retain typed causes; cache I/O also keeps its storage context. Cancellation
remains distinct from failure. UI status snapshots format errors for display.
Workspace dependencies use `thiserror 2`, `dirs 7` and `sysinfo 0.40`. Older
versions may coexist in Cargo.lock when required by upstream dependencies.

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

Version management uses **jj**; each independent modification is its own change.
Local development notes are intentionally not version-controlled.


## Just commands

Run `just` to list the current recipes. `just dev` starts the application,
`just release` runs the release build, `just test` runs `cargo test --all`,
`just lint` (aliases `check` and `fmt`) formats the workspace and runs strict
Clippy, `just document` (alias `doc`) builds public documentation, and
`just bundle-macos` (alias `mac`) creates the local macOS bundle.

`just lint` rewrites formatting with `cargo fmt --all`; it is not a check-only
target. For a non-mutating validation, use the cargo commands under
[Checks](#checks). Workspace Clippy rules ban `unwrap()` and `expect()` in both
application and test code.

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

## Parquet inspection

Opening a Parquet file reads its exact row count, row-group count, file size and
producer/version from the footer, alongside the Arrow schema. This does not run
a data COUNT scan. Top-level Arrow columns and Parquet leaf columns are distinct.

The backend exposes paged original schema, row-group column chunks and raw file
key/value metadata. Pages retain at most 100 records and 8 MiB of Arrow buffers;
oversized metadata fails explicitly. Missing statistics stay null and minimum/maximum
exactness flags are preserved. DuckDB may still parse the full footer internally.
Overview information is captured at opening; detail requests read the current file.
Files should remain unchanged while open. CSV inspection remains unchanged.

Use **File info** in the toolbar to open the right-side panel. **Overview** shows
opening-time file facts; **Schema**, **Row groups**, and **Metadata** load detail
pages only when selected. The status bar labels original file rows/columns as
**Source**, separately from the current Browse range or SQL result. Metadata
pagination is independent of the data toolbar's Limit/Offset.

The panel uses a virtualized table with horizontal scrolling for wide metadata.
Select a detail cell to preview up to 2,000 characters and **Copy full value** to
copy its complete content. Valid UTF-8 blobs display/copy as text; other blobs use
reversible `0x` hexadecimal. Null and empty values remain distinct. File tabs retain
their chosen panel section/page during the session, while only the active detail
page is cached. Close/switch cancels old requests; errors offer **Retry** without
replacing Browse/SQL results. The panel shares the right-hand area with the cell
inspector: double-clicking a data cell opens the inspector. A single click only
changes the table selection.


## Async coordination

Core uses Tokio `sync` and `macros`, plus tokio-util's `CancellationToken`.
UI futures continue to run on GPUI. No Tokio runtime is created: synchronization
and `select!` are runtime-independent, while UI deadlines use GPUI timers.
The existing synchronous backend workers use `futures-lite::block_on` as a bridge
to these waits. Waiting threads park instead of repeatedly sleeping; blocking
DuckDB, disk decoding and serialization remain outside the GUI thread.

- **Independent result subscriptions:** `QueryResult::subscribe()` returns a
  `ResultUpdates` observer. Each observer coalesces changes independently, including
  cache-only changes after query completion. Subscribe before taking a snapshot.
  Releasing the result wakes subscriptions with an explicit closed status.
- **Awaited cache reads:** `row()` stays nonblocking for virtualized rendering.
  `row_async()` returns a specific original batch through a `oneshot` reply; copying
  no longer polls every 25 ms. Requests share a capacity-eight queue and duplicate
  batch requests share one read. At most eight explicit waits per result may be
  active; excess requests fail clearly. Replies retain their batch across eviction.
  Read errors preserve their typed cause and finish all queued waiters.
- **Cancellation:** `Cancellation::until_cancelled()` races an operation against
  cancellation. Cancelling or dropping a copy wait releases its admission slot and
  reply without cancelling the query. Generation cancellation leaves retained
  partial results readable; result disposal cancels remaining read waits. Already
  running synchronous disk decoding finishes at its normal cancellation boundaries.
- **Engine admission:** a single FIFO `Semaphore` covers file inspection, metadata
  and data queries. Cancelled requests leave the queue without opening a connection
  or borrowing another query's interrupt handle. Synchronous backend workers remain
  allocated while waiting; this change removes polling, not those workers.
- **Transport and interruption:** bounded stream/session sends and cursor receives
  wait for capacity/data or cancellation without timer polling. Explicit EOF/error
  events and receiver-drop cleanup remain intact. The DuckDB monitor sleeps until
  cancellation or disposal; after cancellation it repeats interrupts every 10 ms
  until disposal to cover prepare/execute races.

Result budgets, file leases and successful-delete accounting remain unchanged.
Ordinary short metadata locks stay synchronous and are never held across `await`.
If a future feature needs Tokio runtime services, app should own and inject that
runtime; no runtime-dependent timeout or spawn API is used on GPUI tasks here.


## Keyboard controls

Use **Cmd** on macOS and **Ctrl** on Windows/Linux:

| Shortcut | Action |
| --- | --- |
| Cmd/Ctrl+C | Copy the focused data grid's selected cell in full |
| Cmd/Ctrl+Shift+C | Copy the selected cell as JSON |
| Cmd/Ctrl+F | Focus the Browse filter without applying it; does not switch modes |
| Cmd/Ctrl+Enter | Run the current SQL draft in SQL mode |
| Cmd/Ctrl+W | Close the active file; the last tab leaves the empty window |
| Arrow keys | Move the main grid's cell selection without wrapping at edges |
| Enter / Space | Open the selected grid cell in the inspector |
| Escape | Dismiss a menu first, then one detail panel, then the grid selection |
| Tab / Shift+Tab | Move between visible, enabled controls, including leaving the SQL editor |

Input/editor text copying and arrow navigation remain native. The SQL editor keeps
its own Find behavior. File tabs and File info sections each have one Tab stop;
Left/Right and Home/End switch the active tab within that group. Closing a panel
returns focus to the data grid. Nested inspector disclosure buttons support
Enter/Space and expose their expanded state. Labels follow the application's language.

The UI tests enable `gpui-kit/test-support` only as a development dependency and
exercise real headless key and pointer dispatch. They cover focus traversal,
clipboard serialization, tab closure, panel dismissal and accessible names. Native
screen readers, OS IME/clipboard integration and visual focus-ring appearance still
require platform testing.


## Drag and drop files

Drop one or more local files anywhere in the workspace content area, including the
empty window, to open them. A border and hint mark the drop target while hovering;
leaving the window does not open anything. Files use the same background opener as
the picker and startup arguments: duplicate paths reuse existing tabs, supported
types are validated by the backend, and directories are rejected without recursion.
The last successful file becomes active. Failed files are reported in a dismissible
notification even when other files in the same drop open successfully. A newer open
request cancels pending inspection from the previous request.
