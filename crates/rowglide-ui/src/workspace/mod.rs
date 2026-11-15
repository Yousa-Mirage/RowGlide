//! Workspace state, event wiring, and top-level layout.

mod actions;
mod clipboard;
mod controls;
mod file_info;
mod files;
mod freshness;
mod inspector;
mod memory;
mod query;
mod sizing;
mod sql;
mod state;
#[cfg(test)]
mod tests;

use crate::i18n::{tr, tr_string};
use crate::theme;

use state::{TabMode, TabState};

use crate::table::{Grid, GridEvents, SortRequested};
use arrow_array::ArrayRef;
use gpui_kit::component::{
    ActiveTheme, Sizable,
    input::{EditorState, InputState},
    table::{DataTable, TableEvent, TableState},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_core::{Backend, BrowsePage, Cancellation, Query};
use std::{collections::HashSet, sync::Arc};

/// Identifies which right-side panel a divider drag resizes.
#[derive(Clone)]
enum ResizePanel {
    /// Selected-cell preview, with its own remembered width.
    Inspector,
    /// Parquet metadata, independent of the cell preview width.
    FileInfo,
}

impl Render for ResizePanel {
    /// The divider provides drag feedback without a floating preview.
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Owns the active GUI controls and coordinates independent per-file sessions.
///
/// Tabs retain Browse/SQL queries, drafts and inactive results; the table owns the
/// active result. Background tasks use the injected Backend and request identities
/// so cancelled or superseded work cannot overwrite the visible session.
pub(crate) struct Workspace {
    // Dataset and active query.
    /// Shared engine-independent backend injected by the application entry point.
    backend: Arc<dyn Backend>,
    /// One SQL result budget shared by all open tabs.
    results: Arc<rowglide_core::ResultStore>,
    /// Open file sessions in Folder Tabs order.
    tabs: Vec<TabState>,
    /// Index of the selected session; only accessed when tabs is nonempty.
    active_tab: usize,
    /// Applied request for the active tab and mode; excludes pending drafts.
    query: Query,

    // Persistent GPUI controls and subscriptions.
    /// Shared virtualized table entity holding the active result buffers.
    table: Entity<TableState<Grid>>,
    /// Stable focus target for opening files, including the empty workspace.
    open_focus: FocusHandle,
    /// One keyboard entry point shared by the currently active file tab.
    tabs_focus: FocusHandle,
    /// Horizontal file-tab scroll position, used to reveal keyboard selections.
    tabs_scroll: ScrollHandle,
    /// Horizontal metadata-tab scroll position for narrow detail panels.
    info_tabs_scroll: ScrollHandle,
    /// One keyboard entry point for the active metadata section.
    info_tabs_focus: FocusHandle,
    /// Editable Browse text filter control.
    search: Entity<InputState>,
    /// Editable Browse page-size control.
    limit: Entity<InputState>,
    /// Editable Browse start-offset control.
    offset: Entity<InputState>,
    /// Shared SQL editor whose text is saved per tab.
    sql: Entity<EditorState>,
    /// Owned event/observation registrations kept alive for the workspace lifetime.
    _subscriptions: Vec<Subscription>,
    /// Latest whole-process resident memory sample in bytes, or unavailable.
    memory_bytes: Option<u64>,
    /// Owns periodic sampling independently of queries; dropping the workspace cancels it.
    _memory_task: Task<()>,
    /// One serial metadata poller, cancelled when the workspace is dropped.
    _source_watch_task: Task<()>,
    /// Explicit reload completion; old visible results survive until success.
    reload_task: Option<Task<()>>,
    /// Cancels pending engine admission when reloading stops or the workspace closes.
    reload_cancel: Cancellation,
    /// Source being reloaded; prevents duplicate reloads and handles tab closure.
    reloading: Option<usize>,
    /// Invalidates metadata samples started before a reload or source closure.
    source_revision: u64,

    /// Copy worker and feedback, independent of query lifecycle and per-tab state.
    clipboard: clipboard::ClipboardState,

    // Background request lifecycle and status.
    /// UI completion task for the current query; dropping it cancels the completion future.
    task: Option<Task<()>>,
    /// One first-batch sizing task, dropped when the active session or query changes.
    sizing_task: Option<sizing::SizingTask>,
    /// One-way cancellation token for the active query and its worker.
    cancellation: Cancellation,
    /// Independent cancellation token for native file inspection work.
    opening_cancel: Cancellation,
    /// Revision used to reject obsolete query completions and stamp cache recency.
    generation: u64,
    /// Whether a query is awaiting completion; file inspection has a separate lifecycle.
    busy: bool,
    /// Current user-facing progress, range, cancellation or failure message.
    status: String,
    /// Whether the current status should use the error color.
    error: bool,

    // Optional panels and selected-cell expansion.
    /// Independent Parquet metadata presentation and cancellation lifecycle.
    file_info: file_info::InfoView,
    /// Whether to render the SQL panel; synchronized with the active tab mode.
    sql_open: bool,
    /// Selected column label, retained Arrow array and row for the inspector.
    selected: Option<(String, ArrayRef, usize)>,
    /// Preferred inspector width, shared across cells and retained while the panel is closed.
    inspector_width: Pixels,
    /// Preferred metadata panel width, retained across sections, files and panel closure.
    file_info_width: Pixels,
    /// Clicked SQL cell awaiting an asynchronous cache read; cleared on session changes.
    pending_cell: Option<(usize, usize)>,
    /// Expanded inspector node paths for the active result.
    expanded: HashSet<String>,
}

impl Workspace {
    /// Create the shared controls, virtualized table and event subscriptions.
    ///
    /// Wraps existing backend sources in tab state and automatically loads the first
    /// Browse page when available. Sorting resets the applied offset; double-clicking a cell
    /// retains the Arrow array needed by the inspector. Empty registries start in the
    /// file-opening view. File inspection and query execution use background work.
    /// Nonempty startup paths enter the shared file-opening flow after controls exist.
    /// Registers a window appearance subscription so later OS light/dark changes refresh
    /// the active theme without rebuilding the workspace.
    pub(crate) fn new(
        backend: Arc<dyn Backend>,
        results: Arc<rowglide_core::ResultStore>,
        files: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.new(|_| GridEvents);
        let table = cx.new(|cx| {
            TableState::new(
                Grid {
                    widths: None,
                    preview: None,
                    sql_result: None,
                    sql_snapshot: None,
                    loading: true,
                    sort: None,
                    events: events.clone(),
                    sortable: true,
                },
                window,
                cx,
            )
            .cell_selectable(true)
            .loop_selection(false)
            .col_movable(false)
        });

        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("filter.placeholder")));
        let limit = cx.new(|cx| {
            InputState::new(window, cx).default_value(BrowsePage::default().limit().to_string())
        });
        let offset = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let sql = cx.new(|cx| {
            EditorState::new(window, cx).default_value("SELECT * FROM current_file LIMIT 100;")
        });

        let mut subscriptions = vec![
            cx.subscribe(
                &events,
                |this, _, event: &crate::clipboard::CopyRequested, cx| {
                    this.copy_cell(event.clone(), cx);
                },
            ),
            cx.subscribe_in(
                &events,
                window,
                |this, _, event: &SortRequested, window, cx| {
                    if this.tabs.is_empty()
                        || this.active_tab().mode == TabMode::Sql
                        || this.active_tab().changed
                    {
                        return;
                    }
                    this.active_tab_mut().browse.sort = event.0;
                    this.active_tab_mut().browse.offset = 0;
                    this.active_tab_mut().drafts.offset = "0".into();
                    this.offset
                        .update(cx, |state, cx| state.set_value("0", window, cx));
                    this.query = this.active_tab().active_query();

                    this.run(window, cx);
                },
            ),
            cx.subscribe(&table, |this, table, event, cx| {
                if this.tabs.is_empty() {
                    return;
                }
                if let TableEvent::ColumnWidthsChanged(widths) = event {
                    this.sizing_task = None;
                    this.active_tab_mut().result_mut().widths = Some(widths.clone());
                    table.update(cx, |table, _| {
                        table.delegate_mut().widths = Some(widths.clone())
                    });
                }
                if let TableEvent::DoubleClickedCell(row, col) = event {
                    this.inspect_cell(*row, *col, cx);
                }
            }),
        ];
        // Recompute navigation availability when browse drafts change.
        subscriptions.extend([
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&limit, |_, _, cx| cx.notify()),
            cx.observe(&offset, |_, _, cx| cx.notify()),
        ]);
        // Follow OS light/dark changes for the full lifetime of this workspace.
        subscriptions.push(theme::observe_system_theme(window));

        let mut this = Self {
            tabs: backend.sources().into_iter().map(TabState::new).collect(),
            active_tab: 0,
            query: Query::default(),
            backend,
            results,
            table,
            open_focus: cx.focus_handle().tab_stop(true),
            tabs_focus: cx.focus_handle().tab_stop(true),
            tabs_scroll: ScrollHandle::new(),
            info_tabs_scroll: ScrollHandle::new(),
            info_tabs_focus: cx.focus_handle().tab_stop(true),
            search,
            limit,
            offset,
            sql,
            _subscriptions: subscriptions,
            memory_bytes: None,
            _memory_task: Self::watch_memory(cx),
            _source_watch_task: Self::watch_sources(cx),
            reload_task: None,
            reload_cancel: Cancellation::default(),
            reloading: None,
            source_revision: 0,
            clipboard: clipboard::ClipboardState::default(),
            task: None,
            sizing_task: None,
            cancellation: Cancellation::default(),
            opening_cancel: Cancellation::default(),
            generation: 0,
            busy: false,
            status: tr_string!("common.ready"),
            error: false,
            sql_open: false,
            file_info: file_info::InfoView::new(window, cx),
            selected: None,
            inspector_width: px(320.),
            file_info_width: px(480.),
            pending_cell: None,
            expanded: HashSet::new(),
        };

        if !this.tabs.is_empty() {
            this.query = this.active_tab().active_query();
            this.sync_controls_from_tab(window, cx);
            this.run(window, cx);
        }
        this.focus_content(window, cx);
        this.open_paths(files, window, cx);
        this
    }

    /// Borrow the selected tab for rendering or query coordination.
    ///
    /// # Panics
    ///
    /// Panics if there is no active tab. Callers must guard the empty-workspace state
    /// and maintain `active_tab` as a valid index into `tabs`.
    fn active_tab(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }

    /// Mutably borrow the selected tab while preserving the workspace's index invariant.
    ///
    /// # Panics
    ///
    /// Panics if `active_tab` is outside `tabs`; callers must handle the empty-workspace
    /// view before invoking tab-specific operations.
    fn active_tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    /// Replace shared input/editor text with the active tab's saved drafts.
    ///
    /// This synchronizes controls only; it neither applies drafts to the backend query
    /// nor starts a request. Call after changing the active tab or normalized drafts.
    pub(super) fn sync_controls_from_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        self.search.update(cx, |state, cx| {
            state.set_value(tab.drafts.search.clone(), window, cx)
        });
        self.limit.update(cx, |state, cx| {
            state.set_value(tab.drafts.limit.clone(), window, cx)
        });
        self.offset.update(cx, |state, cx| {
            state.set_value(tab.drafts.offset.clone(), window, cx)
        });
        self.sql
            .update(cx, |state, cx| state.set_value(tab.sql.clone(), window, cx));
    }
}

impl Drop for Workspace {
    /// Request cancellation of the active query and any pending file inspection.
    ///
    /// This only signals their tokens; worker-owned resources perform their own cleanup
    /// as background tasks observe cancellation.
    fn drop(&mut self) {
        self.reload_cancel.cancel();
        self.cancellation.cancel();
        self.opening_cancel.cancel();
    }
}

impl Render for Workspace {
    /// Compose the empty-file view or the active file's complete workspace layout.
    ///
    /// The populated view places Folder Tabs above the toolbar, optional SQL editor,
    /// virtualized table, optional inspector and compact status/navigation bar. Query
    /// execution remains in event handlers rather than this render method.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self
            .keyboard_root(cx)
            .relative()
            .on_drop(cx.listener(Self::drop_files));
        if self.tabs.is_empty() {
            return root
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(tr!("files.empty"))
                        .child(self.render_open_button(cx))
                        .child(div().text_sm().child(self.status.clone())),
                )
                .child(
                    div()
                        .min_h(px(28.))
                        .px_5()
                        .py(px(2.))
                        .border_t_1()
                        .border_color(cx.theme().status_bar_border)
                        .bg(cx.theme().status_bar)
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(self.render_memory(cx)),
                )
                .child(self.render_file_drop_hint(cx))
                .into_any_element();
        }
        let mut content = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .when(self.active_tab().changed, |s| {
                s.child(self.render_source_warning(cx))
            });

        if self.sql_open {
            content = content.child(self.render_sql(cx));
        }

        // The table fills the remaining space beside the optional inspector.
        let mut body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizePanel>, _, cx| {
                    let maximum = (event.bounds.size.width * 0.65).max(px(1.));
                    let (width, minimum) = match event.drag(cx) {
                        ResizePanel::Inspector => (&mut this.inspector_width, px(240.)),
                        ResizePanel::FileInfo => (&mut this.file_info_width, px(320.)),
                    };
                    *width = (event.bounds.right() - event.event.position.x)
                        .clamp(minimum.min(maximum), maximum);
                    cx.notify();
                }),
            )
            .child(
                self.keyboard_grid(cx).flex_1().min_w_0().h_full().child(
                    DataTable::new(&self.table)
                        .accessibility_label(tr!("a11y.data_table"))
                        // Stop at the first and last row without exposing overscroll space.
                        .scroll_bounce(false)
                        .stripe(false)
                        .bordered(false)
                        .small(),
                ),
            );
        if self.active_tab().info.open && self.active_tab().source.parquet.is_some() {
            body = body.child(self.render_file_info(cx));
        } else if let Some((name, array, row)) = self.selected.clone() {
            body = body.child(self.render_inspector(name, array, row, cx));
        }

        content = content.child(body).child(self.render_status(cx));

        root.size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(".SystemUIFont")
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .bg(cx.theme().tab_bar)
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.render_tabs(cx)))
                    .child(self.render_open_button(cx)),
            )
            .child(content)
            .child(self.render_file_drop_hint(cx))
            .into_any_element()
    }
}

/// Register workspace key bindings after GPUI Kit registers its component bindings.
pub(crate) fn init(cx: &mut App) {
    actions::init(cx);
}
