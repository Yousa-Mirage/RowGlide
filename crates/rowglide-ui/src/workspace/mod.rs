//! Workspace state, event wiring, and top-level layout.

mod controls;
mod files;
mod inspector;
mod query;
mod sql;
mod state;

use state::{TabMode, TabState};

use crate::table::{Grid, GridEvents, SortRequested};
use arrow_array::ArrayRef;
use gpui_kit::component::{
    Sizable,
    input::{EditorState, InputState},
    table::{DataTable, TableEvent, TableState},
};
use gpui_kit::*;
use rowglide_core::{Backend, BrowsePage, Cancellation, Query};
use std::{collections::HashSet, sync::Arc};

/// Owns the active GUI controls and coordinates independent per-file sessions.
///
/// Tabs retain Browse/SQL queries, drafts and inactive results; the table owns the
/// active result. Background tasks use the injected Backend and request identities
/// so cancelled or superseded work cannot overwrite the visible session.
pub(crate) struct Workspace {
    // Dataset and active query.
    /// Shared engine-independent backend injected by the application entry point.
    backend: Arc<dyn Backend>,
    /// Open file sessions in Folder Tabs order.
    tabs: Vec<TabState>,
    /// Index of the selected session; only accessed when tabs is nonempty.
    active_tab: usize,
    /// Applied request for the active tab and mode; excludes pending drafts.
    query: Query,

    // Persistent GPUI controls and subscriptions.
    /// Shared virtualized table entity holding the active result buffers.
    table: Entity<TableState<Grid>>,
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

    // Background request lifecycle and status.
    /// UI completion task for the current query; dropping it cancels the completion future.
    task: Option<Task<()>>,
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
    /// Whether to render the SQL panel; synchronized with the active tab mode.
    sql_open: bool,
    /// Selected column label, retained Arrow array and row for the inspector.
    selected: Option<(String, ArrayRef, usize)>,
    /// Clicked SQL cell awaiting an asynchronous cache read; cleared on session changes.
    pending_cell: Option<(usize, usize)>,
    /// Expanded inspector node paths for the active result.
    expanded: HashSet<String>,
}

impl Workspace {
    /// Create the shared controls, virtualized table and event subscriptions.
    ///
    /// Wraps existing backend sources in tab state and automatically loads the first
    /// Browse page when available. Sorting resets the applied offset; cell selection
    /// retains the Arrow array needed by the inspector. Empty registries start in the
    /// file-opening view. File inspection and query execution use background work.
    pub(crate) fn new(
        backend: Arc<dyn Backend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.new(|_| GridEvents);
        let table = cx.new(|cx| {
            TableState::new(
                Grid {
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
            .col_movable(false)
        });

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter text columns…"));
        let limit = cx.new(|cx| {
            InputState::new(window, cx).default_value(BrowsePage::default().limit().to_string())
        });
        let offset = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let sql = cx.new(|cx| {
            EditorState::new(window, cx).default_value("SELECT * FROM current_file LIMIT 100;")
        });

        let mut subscriptions = vec![
            cx.subscribe_in(
                &events,
                window,
                |this, _, event: &SortRequested, window, cx| {
                    if this.active_tab().mode == TabMode::Sql {
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
                if let TableEvent::SelectCell(row, col) = event {
                    this.selected = table.read(cx).delegate().row(*row).map(|(batch, r)| {
                        (
                            batch.schema().field(*col).name().clone(),
                            batch.column(*col).clone(),
                            r,
                        )
                    });
                    this.pending_cell = this.selected.is_none().then_some((*row, *col));
                    this.expanded.clear();
                    this.expanded.insert("value".into());
                    cx.notify();
                }
            }),
        ];
        // Recompute navigation availability when browse drafts change.
        subscriptions.extend([
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&limit, |_, _, cx| cx.notify()),
            cx.observe(&offset, |_, _, cx| cx.notify()),
        ]);

        let mut this = Self {
            tabs: backend.sources().into_iter().map(TabState::new).collect(),
            active_tab: 0,
            query: Query::default(),
            backend,
            table,
            search,
            limit,
            offset,
            sql,
            _subscriptions: subscriptions,
            task: None,
            cancellation: Cancellation::default(),
            opening_cancel: Cancellation::default(),
            generation: 0,
            busy: false,
            status: "Ready".into(),
            error: false,
            sql_open: false,
            selected: None,
            pending_cell: None,
            expanded: HashSet::new(),
        };

        if !this.tabs.is_empty() {
            this.query = this.active_tab().active_query();
            this.sync_controls_from_tab(window, cx);
            this.run(window, cx);
        }
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
        if self.tabs.is_empty() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child("Open a Parquet or CSV file")
                .child(self.render_open_button(cx))
                .child(div().text_sm().child(self.status.clone()))
                .into_any_element();
        }
        let mut content = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx));

        if self.sql_open {
            content = content.child(self.render_sql(cx));
        }

        // The table fills the remaining space beside the optional inspector.
        let mut body = div().flex_1().min_h_0().flex().child(
            div().flex_1().min_w_0().h_full().child(
                DataTable::new(&self.table)
                    .stripe(false)
                    .bordered(false)
                    .small(),
            ),
        );
        if let Some((name, array, row)) = self.selected.clone() {
            body = body.child(self.render_inspector(name, array, row, cx));
        }

        content = content.child(body).child(self.render_status(cx));

        div()
            .id("workspace")
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x263b32))
            .font_family(".SystemUIFont")
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .bg(rgb(0xf5f7f6))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.render_tabs(cx)))
                    .child(self.render_open_button(cx)),
            )
            .child(content)
            .into_any_element()
    }
}
