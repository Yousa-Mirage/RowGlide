//! Table adapter for typed previews and column-sort events.

use crate::cells;
use gpui_kit::component::{
    ActiveTheme,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::*;
use rowglide_app::{Preview, Sort};

#[derive(Clone)]
pub(crate) struct SortRequested(pub(crate) Option<Sort>);

pub(crate) struct Grid {
    pub(crate) preview: Option<Preview>,
    pub(crate) loading: bool,
    pub(crate) sort: Option<Sort>,
    pub(crate) events: Entity<GridEvents>,
}

pub(crate) struct GridEvents;

impl EventEmitter<SortRequested> for GridEvents {}

impl TableDelegate for Grid {
    fn columns_count(&self, _: &App) -> usize {
        self.preview.as_ref().map_or(0, |p| p.schema.fields().len())
    }

    fn rows_count(&self, _: &App) -> usize {
        self.preview.as_ref().map_or(0, |p| p.rows)
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let Some(field) = self
            .preview
            .as_ref()
            .and_then(|preview| preview.schema.fields().get(index))
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

        // Only the first five scalar columns support sorting in the mock schema.
        if index < 5 {
            column = column.sort(match self.sort.filter(|sort| sort.column == index) {
                Some(sort) if sort.descending => ColumnSort::Descending,
                Some(_) => ColumnSort::Ascending,
                None => ColumnSort::Default,
            });
        }

        column
    }

    fn perform_sort(
        &mut self,

        column: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
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

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = self
            .preview
            .as_ref()
            .and_then(|p| p.row(row))
            .map(|(b, r)| cells::summary(b.column(col), r))
            .unwrap_or_default();
        let color = if value == "null" {
            cx.theme().muted_foreground
        } else if col >= 5 {
            rgb(0x52776a).into()
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

    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        self.preview
            .as_ref()?
            .row(row)
            .map(|(b, r)| cells::summary(b.column(col), r).into())
    }

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
            .child("No rows to display · try a different filter")
    }
}
