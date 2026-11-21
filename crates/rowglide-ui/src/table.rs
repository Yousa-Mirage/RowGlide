//! Table adapter for Browse pages, streamed SQL results, sorting and cell copying.

mod headers;
pub(crate) mod sizing;

use crate::i18n::{tr, tr_string};

use crate::cells;
use crate::clipboard::{
    CopyTarget, copy_menu,
    range::{CellRange, RangeCopy},
};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use gpui_kit::component::{
    ActiveTheme,
    menu::ContextMenuExt,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_core::{Preview, QueryResult, ResultSnapshot, Sort};
use std::{collections::BTreeSet, sync::Arc};

/// Column-header event requesting a new global Browse sort.
///
/// `Some` contains the source column and direction; `None` clears sorting. The
/// workspace handles the event by querying the backend rather than rearranging
/// only the currently retained rows.
#[derive(Clone)]
pub(crate) struct SortRequested(pub(crate) Option<Sort>);

/// GPUI table delegate backed by one retained typed result.
///
/// The workspace owns query execution and publishes Browse pages or SQL progress.
/// The delegate exposes rows, columns and visible-cell summaries to the virtualized
/// table. Cache misses enqueue bounded background reads without doing file I/O here.
pub(crate) struct Grid {
    /// Display coordinates of the current pointer selection, scoped to this result and layout.
    pub(crate) cell_range: Option<CellRange>,
    /// True between a left press on a data cell and its release.
    pub(crate) dragging_cells: bool,
    /// Visible schema indices in source order; None displays every column.
    pub(crate) visible: Option<Vec<usize>>,
    /// Selected visible fields in schema order, independent of the selected data cell.
    pub(crate) header_columns: BTreeSet<usize>,
    /// Display position of the last ordinary or toggle click, used for Shift ranges.
    pub(crate) header_anchor: Option<usize>,
    /// Changes when a different session or SQL execution takes ownership of the table.
    pub(crate) view_id: u64,
    /// Context-specific explanation for an empty successful Browse page.
    pub(crate) empty_label: SharedString,
    /// First-batch or manually adjusted widths; None until sizing completes for this session.
    pub(crate) widths: Option<Vec<Pixels>>,
    /// Active typed result, owned here rather than in the tab while displayed.
    pub(crate) preview: Option<Preview>,
    /// Active SQL cache handle; retains its disk file and provides nonblocking row access.
    pub(crate) sql_result: Option<Arc<QueryResult>>,
    /// Coalesced SQL progress snapshot used by rendering without repeated state locks.
    pub(crate) sql_snapshot: Option<ResultSnapshot>,
    /// Whether a replacement result is being loaded.
    pub(crate) loading: bool,
    /// Enables Browse header sorting; false in SQL mode.
    pub(crate) sortable: bool,
    /// Applied source-column sort used to render header indicators.
    pub(crate) sort: Option<Sort>,
    /// Event source forwarding backend sorting and captured copy requests.
    pub(crate) events: Entity<GridEvents>,
}

/// Lightweight GPUI event entity forwarding sort and copy requests to the workspace.
///
/// Separating the event source from TableState allows the workspace to subscribe
/// to user operations without coupling the delegate to query or clipboard execution.
pub(crate) struct GridEvents;

impl EventEmitter<SortRequested> for GridEvents {}

/// Visibility change targeting schema fields in the table session that opened the menu.
pub(crate) struct ColumnVisibility {
    /// Identifies the originating view so a stale menu cannot change another tab.
    pub(crate) view_id: u64,
    /// Unique positions in the complete Arrow schema.
    pub(crate) columns: BTreeSet<usize>,
    /// True restores the column at its original position.
    pub(crate) show: bool,
}

impl EventEmitter<ColumnVisibility> for GridEvents {}

impl Grid {
    /// Release both corners and ignore pointer motion until the next cell press.
    pub(crate) fn clear_cell_range(&mut self) {
        self.cell_range = None;
        self.dragging_cells = false;
    }

    /// Return the active result schema, including an empty SQL query's schema.
    pub(crate) fn schema(&self) -> Option<SchemaRef> {
        self.sql_snapshot
            .as_ref()
            .and_then(|state| state.schema.clone())
            .or_else(|| self.preview.as_ref().map(|preview| preview.schema.clone()))
    }

    /// Resolve a display position to the original Arrow field, including after columns are hidden.
    pub(crate) fn schema_index(&self, display: usize) -> Option<usize> {
        match &self.visible {
            Some(columns) => columns.get(display).copied(),
            None => (display < self.schema()?.fields().len()).then_some(display),
        }
    }

    /// Return the original position and field definition for a visible header.
    fn field(&self, display: usize) -> Option<(usize, arrow_schema::FieldRef)> {
        let index = self.schema_index(display)?;
        Some((index, self.schema()?.fields().get(index)?.clone()))
    }

    /// Width shared by the fixed row numbers and the horizontal scrollbar's left inset.
    pub(crate) fn gutter_width(&self, cx: &App) -> Pixels {
        let digits = self.rows_count(cx).max(1).ilog10() + 1;
        px(20. + 9. * digits.max(2) as f32)
    }

    /// Build a fixed-width gutter outside the horizontally scrolling data cells.
    /// Size both header and body from the retained row count, including streamed SQL rows.
    fn number_gutter(&self, label: SharedString, cx: &App) -> Div {
        div()
            .w(self.gutter_width(cx))
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .px_2()
            .border_r_1()
            .border_color(cx.theme().table_row_border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }

    /// Resolve a retained Browse row or request a missing SQL batch asynchronously.
    /// Returned batches share Arrow buffers; rendering never reads the disk file.
    pub(crate) fn row(&self, row: usize) -> Option<(RecordBatch, usize)> {
        if let Some(result) = &self.sql_result {
            result.row(row)
        } else {
            self.preview
                .as_ref()?
                .row(row)
                .map(|(batch, offset)| (batch.clone(), offset))
        }
    }

    /// Capture a full typed value, rejecting stale coordinates after a result replacement.
    pub(crate) fn copy_target(&self, row: usize, col: usize) -> Option<CopyTarget> {
        let col = self.schema_index(col)?;
        if let Some(result) = &self.sql_result {
            (row < self.sql_snapshot.as_ref()?.rows)
                .then(|| CopyTarget::Sql(result.clone(), row, col))
        } else {
            self.row(row)
                .map(|(batch, local)| CopyTarget::Cell(batch.column(col).clone(), local))
        }
    }

    /// Whether either mode retains a result, including empty or partially loaded SQL data.
    pub(crate) fn has_result(&self) -> bool {
        self.preview.is_some() || self.sql_result.is_some()
    }
}

impl TableDelegate for Grid {
    /// Return the retained result's visible column count, or zero before a result exists.
    ///
    /// An empty result with a schema still exposes its columns.
    fn columns_count(&self, _: &App) -> usize {
        self.schema().map_or(0, |schema| {
            self.visible
                .as_ref()
                .map_or(schema.fields().len(), Vec::len)
        })
    }

    /// Return the number of retained display rows, never a source or estimated total.
    fn rows_count(&self, _: &App) -> usize {
        self.sql_snapshot.as_ref().map_or_else(
            || self.preview.as_ref().map_or(0, |p| p.rows),
            |state| state.rows,
        )
    }

    /// Prefix the header with a gutter instead of inserting a synthetic schema column.
    fn render_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div().id("header").child(self.number_gutter("#".into(), cx))
    }

    /// Number visible rows within this result; leave viewport filler rows unnumbered.
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let label = if row < self.rows_count(cx) {
            (row + 1).to_string().into()
        } else {
            SharedString::default()
        };
        div().id(("row", row)).child(self.number_gutter(label, cx))
    }

    /// Build one header from its schema name, compact type label and current sort.
    ///
    /// Nested columns and SQL results disable header sorting. An unavailable index gets
    /// a nonselectable placeholder header while results are being replaced.
    fn column(&self, index: usize, _: &App) -> Column {
        let Some((index, field)) = self.field(index) else {
            return Column::new(format!("unavailable-{index}"), tr!("common.unavailable"))
                .selectable(false)
                .movable(false);
        };

        let mut column = Column::new(field.name().clone(), sizing::header(&field))
            .width(
                self.widths
                    .as_ref()
                    .and_then(|widths| widths.get(index))
                    .copied()
                    .unwrap_or(px(150.)),
            )
            .min_width(px(64.))
            .movable(false);

        // Nested values have no column-header ordering; SQL can extract a scalar.
        if self.sortable && sizing::sortable(&field) {
            column = column.sort(match self.sort.filter(|sort| sort.column == index) {
                Some(sort) if sort.descending => ColumnSort::Descending,
                Some(_) => ColumnSort::Ascending,
                None => ColumnSort::Default,
            });
        }

        column
    }

    /// Attach the native-style context menu to the name and type shown in the header.
    fn render_th(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.header_menu(index, cx)
    }

    /// Translate GPUI's column sort state into an engine-independent sort event.
    ///
    /// Ignores requests when sorting is disabled. The workspace applies the event to
    /// the full source query and reloads a page; this delegate does not reorder batches.
    fn perform_sort(
        &mut self,
        column: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !self.sortable {
            return;
        }
        let Some(column) = self.schema_index(column) else {
            return;
        };
        let sort = match sort {
            ColumnSort::Ascending => Some(Sort {
                column,
                descending: false,
            }),
            ColumnSort::Descending => Some(Sort {
                column,
                descending: true,
            }),
            _ => None,
        };

        // Delegate sorting to the backend so it covers the full result.
        self.events.update(cx, |_, cx| cx.emit(SortRequested(sort)));
    }

    /// Expose whether the workspace is currently replacing this table's result.
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    /// Render one visible cell using a compact Arrow summary and ellipsis overflow.
    ///
    /// `row` is relative to the retained page/result and `col` is a visible column index
    /// supplied by TableState. Missing SQL rows show a loading label; `null` summaries use
    /// the muted color. Each menu retains its original typed target. Rendering performs
    /// no synchronous backend or disk access.
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(source) = self.schema_index(col) else {
            return div().into_any_element();
        };
        let value = self
            .row(row)
            .map(|(b, r)| cells::summary(b.column(source), r))
            .unwrap_or_else(|| {
                if self.sql_result.is_some() {
                    tr_string!("common.loading")
                } else {
                    String::new()
                }
            });
        let color = if value == "null" {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        };

        let target = self.copy_target(row, col);
        let events = self.events.clone();
        let table = cx.weak_entity();
        let selected = self
            .cell_range
            .is_some_and(|range| range.includes(row, col));

        div()
            .id(SharedString::from(format!("cell-{row}-{col}")))
            .size_full()
            .text_sm()
            .when(selected || self.header_columns.contains(&source), |cell| {
                cell.bg(cx.theme().table_active)
            })
            .text_color(color)
            .overflow_hidden()
            .text_ellipsis()
            .child(value)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, event: &MouseDownEvent, window, cx| {
                    let point = (row, col);
                    let anchor = if event.modifiers.shift {
                        table
                            .delegate()
                            .cell_range
                            .map(|r| r.anchor)
                            .or_else(|| table.selected_cell())
                            .unwrap_or(point)
                    } else {
                        point
                    };
                    let grid = table.delegate_mut();
                    grid.clear_headers();
                    grid.cell_range = Some(CellRange { anchor, end: point });
                    grid.dragging_cells = true;
                    table.focus_handle(cx).focus(window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, cx| {
                    if table
                        .delegate()
                        .cell_range
                        .is_some_and(|r| !r.includes(row, col))
                    {
                        table.delegate_mut().clear_cell_range();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |table, event: &MouseMoveEvent, _, cx| {
                let grid = table.delegate_mut();
                if grid.dragging_cells
                    && event.pressed_button == Some(MouseButton::Left)
                    && let Some(range) = &mut grid.cell_range
                    && range.end != (row, col)
                {
                    range.end = (row, col);
                    cx.notify();
                }
            }))
            .context_menu(move |menu, _, cx| {
                let rectangle = table.upgrade().and_then(|table| {
                    let grid = table.read(cx).delegate();
                    if !grid.header_columns.contains(&source)
                        && !grid.cell_range.is_some_and(|r| r.includes(row, col))
                    {
                        return None;
                    }
                    RangeCopy::capture(grid, cx)
                });
                if let Some(range) = rectangle {
                    crate::clipboard::range_menu(menu, range, events.clone())
                } else {
                    copy_menu(menu, target.clone(), events.clone())
                }
            })
            .into_any_element()
    }

    /// Return the same compact summary used by the visible cell as a shared label.
    ///
    /// Returns `None` when the result or row is unavailable. The column index is supplied
    /// by TableState and must be within the result schema. Text can be truncated by
    /// `cells::summary`; this is not a full-value serialization.
    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        let col = self.schema_index(col)?;
        self.row(row)
            .map(|(b, r)| cells::summary(b.column(col), r).into())
    }

    /// Render the centered empty-state hint for Browse or SQL mode.
    ///
    /// Browse uses the successful applied page's predicate/range context. Before a
    /// result exists it prompts for execution; SQL has its own empty-result label.
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(if self.sortable && !self.has_result() {
                tr!("table.no_result")
            } else if self.sortable {
                self.empty_label.clone()
            } else {
                tr!("table.empty_sql")
            })
    }
}
