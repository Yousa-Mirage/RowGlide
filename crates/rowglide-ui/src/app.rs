//! Workspace layout, query lifecycle, and cell inspection.

use crate::{
    cells,
    grid::{Grid, GridEvents, SortRequested},
};
use arrow_array::ArrayRef;
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, EditorState, Input, InputState},
    tab::{Tab, TabBar},
    table::{DataTable, TableEvent, TableState},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_app::{Backend, Cancellation, Preview, Query, Source};
use std::{collections::HashSet, sync::Arc, time::Instant};

/// Owns view state and coordinates background queries.
pub(crate) struct Workspace {
    backend: Arc<dyn Backend>,
    sources: Vec<Source>,
    query: Query,
    table: Entity<TableState<Grid>>,
    search: Entity<InputState>,
    limit: Entity<InputState>,
    offset: Entity<InputState>,
    sql: Entity<EditorState>,
    _subscriptions: Vec<Subscription>,
    task: Option<Task<()>>,
    cancellation: Cancellation,
    generation: u64,
    busy: bool,
    status: String,
    error: bool,
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
                    this.query.sort = event.0;
                    this.query.sql = None;

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
            sources: backend.sources(),
            backend,
            query: Query::default(),
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

        this.run(window, cx);
        this
    }

    // Validate browse controls before replacing the active query.
    fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let limit = self.limit.read(cx).value();
        let offset = self.offset.read(cx).value();
        let parse = |value: &str| value.trim().parse::<usize>();
        let limit = if limit.trim().is_empty() {
            Ok(None)
        } else {
            parse(&limit).map(Some)
        };
        let offset = if offset.trim().is_empty() {
            Ok(0)
        } else {
            parse(&offset)
        };

        match (limit, offset) {
            (Ok(limit), Ok(offset)) => {
                self.query.limit = limit;
                self.query.offset = offset;
                self.query.search = self.search.read(cx).value().to_string();
                self.query.sql = None;
                self.run(window, cx);
            }
            _ => {
                self.error = true;
                self.status =
                    "Limit and Offset must be non-negative whole numbers within range".into();
                cx.notify();
            }
        }
    }

    fn reset_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.limit
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.offset
            .update(cx, |state, cx| state.set_value("0", window, cx));
    }

    // Replace the active task and collect a bounded preview off the UI thread.
    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.cancellation = Cancellation::default();
        self.generation += 1;
        let generation = self.generation;

        self.busy = true;
        self.error = false;
        self.status = "Loading demo data…".into();
        self.selected = None;

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = true;
            table.delegate_mut().preview = None;
            table.refresh(cx);
            cx.notify();
        });

        let backend = self.backend.clone();
        let query = self.query.clone();
        let cancel = self.cancellation.clone();
        let work = cx.background_executor().spawn(async move {
            let started = Instant::now();
            let result = backend
                .query(query, cancel.clone())
                .and_then(|cursor| Preview::collect(cursor, &cancel));
            (result, started.elapsed())
        });

        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let (result, elapsed) = work.await;
            let _ = this.update(cx, |this, cx| {
                // Discard results from cancelled or superseded queries.
                if generation != this.generation {
                    return;
                }

                this.busy = false;
                match result {
                    Ok(preview) => {
                        this.status = format!(
                            "{} rows{} · {:.0} ms",
                            preview.total_rows.unwrap_or(preview.rows),
                            if preview.truncated {
                                " · bounded preview"
                            } else {
                                ""
                            },
                            elapsed.as_secs_f64() * 1000.
                        );
                        this.table.update(cx, |table, cx| {
                            let grid = table.delegate_mut();
                            grid.preview = Some(preview);
                            grid.loading = false;
                            grid.sort = this.query.sort;
                            table.refresh(cx);
                        });
                    }
                    Err(error) => {
                        this.error = true;
                        this.status = error.to_string();
                        this.table.update(cx, |table, cx| {
                            table.delegate_mut().loading = false;
                            table.delegate_mut().preview = None;
                            table.refresh(cx);
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.generation += 1;
        self.busy = false;
        self.status = "Cancelled · run a query to load data".into();

        self.table.update(cx, |table, cx| {
            table.delegate_mut().loading = false;
            cx.notify();
        });
        cx.notify();
    }

    // Render an expandable cell tree with a fixed nesting depth limit.
    fn tree(
        &self,
        label: String,
        array: ArrayRef,
        row: usize,
        path: String,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = cells::children(&array, row);
        let open = self.expanded.contains(&path);
        let has_children = !children.is_empty() && depth < 12;
        let click_path = path.clone();
        let mut node = div().flex().flex_col().child(
            div()
                .id(SharedString::from(path.clone()))
                .px_3()
                .py_2()
                .ml(px(depth as f32 * 12.))
                .rounded_md()
                .hover(|s| s.bg(rgb(0xf0f3f2)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded.remove(&click_path) {
                        this.expanded.insert(click_path.clone());
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} {}",
                            if has_children {
                                if open { "▾" } else { "▸" }
                            } else {
                                "·"
                            },
                            label
                        )),
                )
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .child(cells::summary(&array, row)),
                ),
        );

        if open && has_children {
            for (i, (label, child, row)) in children.into_iter().enumerate() {
                node =
                    node.child(self.tree(label, child, row, format!("{path}/{i}"), depth + 1, cx));
            }
        }

        node.into_any_element()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = rgb(0xe4e8e7);

        // File tabs switch datasets and reset browse controls.
        let tabs = TabBar::new("open-file-tabs")
            .folder()
            .selected_index(
                self.sources
                    .iter()
                    .position(|source| source.id == self.query.source)
                    .unwrap_or(0),
            )
            .children(
                self.sources
                    .iter()
                    .map(|source| Tab::new().label(source.name.clone())),
            )
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                let Some(source) = this.sources.get(*index) else {
                    return;
                };
                if source.id == this.query.source {
                    return;
                }
                this.query = Query {
                    source: source.id,
                    ..Default::default()
                };
                this.reset_inputs(window, cx);

                this.run(window, cx);
            }));

        // Browse toolbar: filter, row range, reset, and SQL toggle.
        let mut content = div().flex_1().min_w_0().min_h_0().flex().flex_col().child(
            div()
                .px_5()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(border)
                .flex_shrink_0()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.))
                        .max_w(px(320.))
                        .child(Input::new(&self.search).small()),
                )
                .child(div().text_xs().child("Limit"))
                .child(
                    div()
                        .w(px(80.))
                        .flex_shrink_0()
                        .child(Input::new(&self.limit).small()),
                )
                .child(div().text_xs().child("Offset"))
                .child(
                    div()
                        .w(px(80.))
                        .flex_shrink_0()
                        .child(Input::new(&self.offset).small()),
                )
                .child(
                    Button::new("filter")
                        .small()
                        .label("Apply")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply_filter(window, cx);
                        })),
                )
                .child(
                    Button::new("reset")
                        .small()
                        .ghost()
                        .label("Reset")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.query = Query {
                                source: this.query.source,
                                ..Default::default()
                            };
                            this.reset_inputs(window, cx);

                            this.run(window, cx);
                        })),
                )
                .child(div().flex_1())
                .child(
                    Button::new("sql-toggle")
                        .small()
                        .label(if self.sql_open { "Hide SQL" } else { "SQL" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sql_open = !this.sql_open;
                            cx.notify();
                        })),
                ),
        );

        // Optional SQL editor with explicit demo presets.
        if self.sql_open {
            content = content.child(div().p_4().border_b_1().border_color(border).bg(rgb(0xf9faf9))
                .child(div().flex().items_center().justify_between().mb_3().child(div().text_xs().text_color(cx.theme().muted_foreground).child("SQL · DEMO PRESETS ONLY"))
                    .child(Button::new("run").small().primary().label("Run query").on_click(cx.listener(|this,_,window,cx| { this.query.search.clear();
                                        this.search.update(cx, |state,cx| state.set_value("",window,cx)); this.query.sort = None; this.query.sql = Some(this.sql.read(cx).value().to_string()); this.run(window,cx); }))))
                .child(Editor::new(&self.sql).h(px(95.)))
                .child(div().mt_2().text_xs().text_color(cx.theme().muted_foreground).child("Supported: SELECT * FROM current_file LIMIT 100;   /   SELECT * FROM current_file WHERE false;")));
        }

        // Virtualized result table fills the available space.
        let mut body = div().flex_1().min_h_0().flex().child(
            div().flex_1().min_w_0().h_full().child(
                DataTable::new(&self.table)
                    .stripe(false)
                    .bordered(false)
                    .small(),
            ),
        );

        // Side panel shows the selected cell and its nested values.
        if let Some((name, array, row)) = self.selected.clone() {
            body = body.child(
                div()
                    .w(px(272.))
                    .flex_shrink_0()
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .p_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Cell inspector"),
                            )
                            .child(
                                Button::new("close-inspector")
                                    .small()
                                    .ghost()
                                    .label("×")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .pb_3()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                name,
                                cells::type_label(array.data_type())
                            )),
                    )
                    .child(
                        div()
                            .id("inspector-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(self.tree(name, array, row, "value".into(), 0, cx)),
                    ),
            );
        }

        // Bottom status bar: query status, cancellation, and sample-data label.
        content = content.child(body).child(
            div()
                .min_h(px(28.))
                .px_5()
                .py(px(2.))
                .border_t_1()
                .border_color(border)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(if self.error {
                            rgb(0xb34c46)
                        } else {
                            rgb(0x6c7b73)
                        })
                        .child(self.status.clone()),
                )
                .when(self.busy, |s| {
                    s.child(
                        Button::new("cancel")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x6c7b73))
                        .child("SAMPLE DATA"),
                ),
        );

        // Root layout stacks file tabs above the browsing workspace.
        div()
            .id("workspace")
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x263b32))
            .font_family(".SystemUIFont")
            .flex()
            .flex_col()
            .child(div().flex_shrink_0().bg(rgb(0xf5f7f6)).px_3().child(tabs))
            .child(content)
    }
}
