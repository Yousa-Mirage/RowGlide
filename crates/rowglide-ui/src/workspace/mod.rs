//! Workspace state, event wiring, and top-level layout.

mod controls;
mod inspector;
mod query;
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
use rowglide_app::{Backend, Cancellation, Query};
use std::{collections::HashSet, sync::Arc};

/// Owns view state and coordinates background queries.
pub(crate) struct Workspace {
    // Dataset and active query.
    backend: Arc<dyn Backend>,
    tabs: Vec<TabState>,
    active_tab: usize,
    query: Query,

    // Persistent GPUI controls and subscriptions.
    table: Entity<TableState<Grid>>,
    search: Entity<InputState>,
    limit: Entity<InputState>,
    offset: Entity<InputState>,
    sql: Entity<EditorState>,
    _subscriptions: Vec<Subscription>,

    // Background request lifecycle and status.
    task: Option<Task<()>>,
    cancellation: Cancellation,
    generation: u64,
    busy: bool,
    status: String,
    error: bool,

    // Optional panels and selected-cell expansion.
    sql_open: bool,
    selected: Option<(String, ArrayRef, usize)>,
    expanded: HashSet<String>,
}

impl Workspace {
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

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter name or city…"));
        let limit = cx.new(|cx| InputState::new(window, cx).placeholder("All"));
        let offset = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let sql = cx.new(|cx| {
            EditorState::new(window, cx).default_value("SELECT * FROM current_file LIMIT 100;")
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &events,
                window,
                |this, _, event: &SortRequested, window, cx| {
                    if this.active_tab().mode == TabMode::Sql {
                        return;
                    }
                    this.active_tab_mut().browse.sort = event.0;
                    this.query = this.active_tab().active_query();

                    this.run(window, cx);
                },
            ),
            cx.subscribe(&table, |this, table, event, cx| {
                if let TableEvent::SelectCell(row, col) = event {
                    this.selected = table.read(cx).delegate().preview.as_ref().and_then(|p| {
                        p.row(*row).map(|(batch, r)| {
                            (
                                batch.schema().field(*col).name().clone(),
                                batch.column(*col).clone(),
                                r,
                            )
                        })
                    });
                    this.expanded.clear();
                    this.expanded.insert("value".into());
                    cx.notify();
                }
            }),
        ];

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
            generation: 0,
            busy: false,
            status: "Ready".into(),
            error: false,
            sql_open: false,
            selected: None,
            expanded: HashSet::new(),
        };

        if !this.tabs.is_empty() {
            this.query = this.active_tab().active_query();
            this.run(window, cx);
        }
        this
    }

    fn active_tab(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }

    fn active_tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

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
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.tabs.is_empty() {
            return div().child("No files open").into_any_element();
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
                    .child(self.render_tabs(cx)),
            )
            .child(content)
            .into_any_element()
    }
}
