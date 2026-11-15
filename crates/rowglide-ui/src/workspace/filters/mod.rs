//! Per-tab predicate editing and applied-condition presentation.

mod render;
#[cfg(test)]
mod tests;

use super::{
    Workspace,
    state::{BrowseDraft, FilterDraft},
};
use crate::i18n::{tr, tr_string};
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use rowglide_core::{CompareOp, FilterCondition, FilterKind, filter_kind, filter_operators};

/// Active row input and its subscription; dropping it releases all editor listeners.
pub(super) struct FilterEditor {
    /// Stable identity from the saved draft, independent of list position.
    id: usize,
    /// Raw literal input; numeric parsing happens only on Apply.
    input: Entity<InputState>,
    /// Writes input changes back into the owning tab's draft.
    _subscription: Subscription,
}

/// One bounded menu selection, applied only to its captured source and row identity.
#[derive(Clone)]
enum Edit {
    /// Replace the column and reset incompatible operator/value drafts.
    Column(usize),
    /// Select a comparison or explicit null test.
    Operator(Option<CompareOp>, bool),
    /// Select a boolean without requiring typed text.
    Boolean(bool),
}

impl Workspace {
    /// Capture live value inputs, including edits whose observation callback has not run yet.
    pub(super) fn filter_drafts(&self, cx: &App) -> Vec<FilterDraft> {
        self.active_tab()
            .drafts
            .filters
            .iter()
            .cloned()
            .map(|mut row| {
                if let Some(editor) = self
                    .filter_editors
                    .iter()
                    .find(|editor| editor.id == row.id)
                {
                    row.value = editor.input.read(cx).value().to_string();
                }
                row
            })
            .collect()
    }

    /// Explain an empty successful page without claiming an unknown match count.
    pub(super) fn browse_empty_label(&self, query: &rowglide_core::Query) -> SharedString {
        if query.offset > 0 {
            tr!("filters.offset_empty")
        } else if !query.search.is_empty() || !query.filters.is_empty() {
            tr!("filters.no_match")
        } else {
            tr!("table.empty_browse")
        }
    }

    /// Whether the registered source contains any searchable decoded text column.
    pub(super) fn has_search_columns(&self) -> bool {
        self.active_tab()
            .source
            .schema
            .fields()
            .iter()
            .any(|field| filter_kind(field.data_type()) == Some(FilterKind::Text))
    }

    /// Recreate active controls from saved raw drafts after session restoration.
    /// Each subscription is scoped to a source ID and stable row ID; old controls cannot
    /// mutate another tab after switching, deleting a row or reloading the source.
    pub(super) fn sync_filter_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_editors.clear();
        let source = self.active_tab().source.id;
        for row in self.active_tab().drafts.filters.clone() {
            let id = row.id;
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(row.value)
                    .placeholder(value_hint(&row.data_type))
            });
            let subscription = cx.observe(&input, move |this, input, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.source.id == source)
                    && let Some(row) = tab.drafts.filters.iter_mut().find(|row| row.id == id)
                {
                    row.value = input.read(cx).value().to_string();
                    cx.notify();
                }
            });
            self.filter_editors.push(FilterEditor {
                id,
                input,
                _subscription: subscription,
            });
        }
    }

    /// Add a condition using the first supported source column without a fixed count limit.
    pub(super) fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab_mut().drafts = self.browse_draft(cx);
        let Some((column, field)) = self
            .active_tab()
            .source
            .schema
            .fields()
            .iter()
            .enumerate()
            .find(|(_, field)| filter_kind(field.data_type()).is_some())
        else {
            return;
        };
        let data_type = field.data_type().clone();
        let kind = filter_kind(&data_type);
        let id = self.next_filter_id;
        self.next_filter_id += 1;
        self.active_tab_mut().drafts.filters.push(FilterDraft {
            id,
            column,
            data_type,
            op: Some(if kind == Some(FilterKind::Text) {
                CompareOp::Contains
            } else {
                CompareOp::Equals
            }),
            negated: false,
            value: if kind == Some(FilterKind::Boolean) {
                "true".into()
            } else {
                String::new()
            },
        });
        self.sync_filter_editors(window, cx);
        if let Some(editor) = self.filter_editors.last() {
            editor.input.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Apply a menu choice to a still-current row; never dispatch a query implicitly.
    fn edit_filter(
        &mut self,
        source: usize,
        id: usize,
        edit: Edit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.is_empty() || self.active_tab().source.id != source {
            return;
        }
        let schema = self.active_tab().source.schema.clone();
        let Some(row) = self
            .active_tab_mut()
            .drafts
            .filters
            .iter_mut()
            .find(|row| row.id == id)
        else {
            return;
        };
        match edit {
            Edit::Column(column) => {
                let Some(field) = schema.fields().get(column) else {
                    return;
                };
                let Some(kind) = filter_kind(field.data_type()) else {
                    return;
                };
                let previous = filter_kind(&row.data_type);
                row.column = column;
                row.data_type = field.data_type().clone();
                if row
                    .op
                    .is_some_and(|op| !filter_operators(&row.data_type).contains(&op))
                {
                    row.op = Some(CompareOp::Equals);
                }
                if previous != Some(kind) {
                    row.value = if kind == FilterKind::Boolean {
                        "true".into()
                    } else {
                        String::new()
                    };
                }
            }
            Edit::Operator(op, negated) => {
                row.op = op;
                row.negated = negated;
            }
            Edit::Boolean(value) => row.value = value.to_string(),
        }
        self.sync_filter_editors(window, cx);
        cx.notify();
    }

    /// Restore all Browse drafts from the last successful query and close the editor.
    /// This does not cancel an executing query; query cancellation is a separate action.
    pub(super) fn discard_filter_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.active_tab().browse.clone();
        let schema = self.active_tab().source.schema.clone();
        let mut filters = Vec::new();
        for condition in &query.filters {
            let Some(field) = schema.fields().get(condition.column()) else {
                continue;
            };
            let (op, negated, value) = match condition {
                FilterCondition::Compare { op, value, .. } => (Some(*op), false, value.input()),
                FilterCondition::IsNull { negated, .. } => (None, *negated, String::new()),
            };
            filters.push(FilterDraft {
                id: self.next_filter_id,
                column: condition.column(),
                data_type: field.data_type().clone(),
                op,
                negated,
                value,
            });
            self.next_filter_id += 1;
        }
        self.active_tab_mut().drafts = BrowseDraft {
            filters,
            search: query.search,
            limit: query
                .limit
                .unwrap_or(rowglide_core::BrowsePage::default().limit())
                .to_string(),
            offset: query.offset.to_string(),
        };
        self.active_tab_mut().filters_open = false;
        self.sync_controls_from_tab(window, cx);
        self.table.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Format applied predicates with quoted text, preserving NULL versus empty strings.
    /// The visual summary is clipped by layout; the tooltip retains the complete text.
    pub(super) fn applied_filters_label(&self) -> String {
        let tab = self.active_tab();
        let mut labels = Vec::new();
        if !tab.browse.search.is_empty() {
            labels.push(format!("{} {:?}", tr!("filters.search"), tab.browse.search));
        }
        for condition in &tab.browse.filters {
            let Some(field) = tab.source.schema.fields().get(condition.column()) else {
                continue;
            };
            let (op, value) = match condition {
                FilterCondition::Compare { op, value, .. } => (
                    operator_label(Some(*op), false),
                    match value {
                        rowglide_core::FilterValue::Text(text) => format!(" {text:?}"),
                        _ => format!(" {}", value.input()),
                    },
                ),
                FilterCondition::IsNull { negated, .. } => {
                    (operator_label(None, *negated), String::new())
                }
            };
            labels.push(format!("{} {}{}", field.name(), op, value));
        }
        if labels.is_empty() {
            String::new()
        } else {
            tr_string!("filters.applied", conditions = labels.join(" · "))
        }
    }
}

/// Localize comparisons while keeping familiar mathematical symbols compact.
fn operator_label(op: Option<CompareOp>, negated: bool) -> SharedString {
    match op {
        None if negated => tr!("filters.not_null"),
        None => tr!("filters.is_null"),
        Some(CompareOp::Equals) => "=".into(),
        Some(CompareOp::NotEquals) => "≠".into(),
        Some(CompareOp::GreaterThan) => ">".into(),
        Some(CompareOp::GreaterThanOrEqual) => "≥".into(),
        Some(CompareOp::LessThan) => "<".into(),
        Some(CompareOp::LessThanOrEqual) => "≤".into(),
        Some(CompareOp::Contains) => tr!("filters.contains"),
        Some(CompareOp::StartsWith) => tr!("filters.starts"),
        Some(CompareOp::EndsWith) => tr!("filters.ends"),
    }
}

/// Give exact temporal/timezone guidance without using localized strings as type logic.
fn value_hint(data_type: &arrow_schema::DataType) -> SharedString {
    match rowglide_core::filter_data_type(data_type) {
        arrow_schema::DataType::Date32 | arrow_schema::DataType::Date64 => "YYYY-MM-DD".into(),
        arrow_schema::DataType::Timestamp(_, Some(_)) => {
            "YYYY-MM-DDTHH:mm:ss[.ffffff]Z / ±HH:MM".into()
        }
        arrow_schema::DataType::Timestamp(_, None) => "YYYY-MM-DDTHH:mm:ss[.fraction]".into(),
        _ => tr!("filters.value"),
    }
}
