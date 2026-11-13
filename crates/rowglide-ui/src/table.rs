//! Table adapter for Browse pages, streamed SQL results and column-sort events.

use crate::cells;
use arrow_array::RecordBatch;
use arrow_schema::{DataType, SchemaRef};
use gpui_kit::component::{
    ActiveTheme,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::*;
use rowglide_core::{Preview, QueryResult, ResultSnapshot, Sort};
use std::sync::Arc;

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
    /// Event source used to request backend sorting from the workspace.
    pub(crate) events: Entity<GridEvents>,
}

/// Lightweight GPUI event entity forwarding sort requests to the workspace.
///
/// Separating the event source from TableState allows the workspace to subscribe
/// to user sorting without coupling the delegate to query execution.
pub(crate) struct GridEvents;

impl EventEmitter<SortRequested> for GridEvents {}

impl Grid {
    /// Return the active result schema, including an empty SQL query's schema.
    fn schema(&self) -> Option<SchemaRef> {
        self.sql_snapshot
            .as_ref()
            .and_then(|state| state.schema.clone())
            .or_else(|| self.preview.as_ref().map(|preview| preview.schema.clone()))
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

    /// Whether either mode retains a result, including empty or partially loaded SQL data.
    pub(crate) fn has_result(&self) -> bool {
        self.preview.is_some() || self.sql_result.is_some()
    }
}

impl TableDelegate for Grid {
    /// Return the retained result's schema width, or zero before a result exists.
    ///
    /// An empty result with a schema still exposes its columns.
    fn columns_count(&self, _: &App) -> usize {
        self.schema().map_or(0, |schema| schema.fields().len())
    }

    /// Return the number of retained display rows, never a source or estimated total.
    fn rows_count(&self, _: &App) -> usize {
        self.sql_snapshot.as_ref().map_or_else(
            || self.preview.as_ref().map_or(0, |p| p.rows),
            |state| state.rows,
        )
    }

    /// Build one header from its schema name, compact type label and current sort.
    ///
    /// Nested columns and SQL results disable header sorting. An unavailable index gets
    /// a nonselectable placeholder header while results are being replaced.
    fn column(&self, index: usize, _: &App) -> Column {
        let schema = self.schema();
        let Some(field) = schema
            .as_ref()
            .and_then(|schema| schema.fields().get(index))
        else {
            return Column::new(format!("unavailable-{index}"), "Unavailable")
                .selectable(false)
                .movable(false);
        };

        let mut column = Column::new(
            field.name().clone(),
            format!(
                "{} · {}",
                field.name(),
                cells::type_label(field.data_type())
            ),
        )
        .width(px(if index == 1 { 200. } else { 150. }))
        .movable(false);

        // Nested values have no column-header ordering; SQL can extract a scalar.
        if self.sortable
            && !matches!(
                field.data_type(),
                DataType::List(_)
                    | DataType::LargeList(_)
                    | DataType::FixedSizeList(_, _)
                    | DataType::Struct(_)
                    | DataType::Map(_, _)
            )
        {
            column = column.sort(match self.sort.filter(|sort| sort.column == index) {
                Some(sort) if sort.descending => ColumnSort::Descending,
                Some(_) => ColumnSort::Ascending,
                None => ColumnSort::Default,
            });
        }

        column
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
    /// `row` is relative to the retained page/result and `col` is a schema column index
    /// supplied by TableState. Missing rows render blank; summaries equal to `null` use
    /// the muted color. Rendering performs no backend or disk access.
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = self
            .row(row)
            .map(|(b, r)| cells::summary(b.column(col), r))
            .unwrap_or_else(|| {
                if self.sql_result.is_some() {
                    "Loading…".into()
                } else {
                    String::new()
                }
            });
        let color = if value == "null" {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        };

        div()
            .text_sm()
            .text_color(color)
            .overflow_hidden()
            .text_ellipsis()
            .child(value)
    }

    /// Return the same compact summary used by the visible cell as a shared label.
    ///
    /// Returns `None` when the result or row is unavailable. The column index is supplied
    /// by TableState and must be within the result schema. Text can be truncated by
    /// `cells::summary`; this is not a full-value serialization.
    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        self.row(row)
            .map(|(b, r)| cells::summary(b.column(col), r).into())
    }

    /// Render the centered empty-state hint for Browse or SQL mode.
    ///
    /// The message depends on whether sorting is enabled; query failures and more
    /// specific status information are shown separately by the workspace.
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
            .child(if self.sortable {
                "No rows to display · try a different filter"
            } else {
                "No SQL rows to display · run a query"
            })
    }
}
