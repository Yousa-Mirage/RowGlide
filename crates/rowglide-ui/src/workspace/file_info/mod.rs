//! Per-file panel preferences, cancellable detail loading and compact Parquet presentation.

mod render;
mod table;

use crate::i18n::tr;

use super::Workspace;
use gpui_kit::component::table::TableState;
use gpui_kit::*;
use rowglide_core::{Cancellation, Source};
use table::MetadataGrid;

/// Lightweight preferences saved independently for every file, separate from Browse/SQL mode.
#[derive(Default)]
pub(super) struct InfoPreferences {
    /// Whether this tab presents file information instead of the selected-cell inspector.
    pub(super) open: bool,
    /// True selects Schema; false selects Overview.
    section: bool,
}

/// Identity for one detail request; a later request can never overwrite another file or request.
#[derive(Clone, Copy, PartialEq, Eq)]
struct InfoRequestKey {
    /// Backend-local source ID, independent of active tab position and query mode.
    source: usize,
    /// Incremented whenever panel work is cancelled or replaced.
    revision: u64,
}

impl InfoRequestKey {
    /// Accept a completion only while the same request and source remain visible.
    /// Overview/close clears the current identity; retries advance its revision.
    fn is_current(self, current: Option<Self>, source: usize, open: bool) -> bool {
        open && current == Some(self) && source == self.source
    }
}

/// One active schema and its worker lifecycle; inactive tabs retain only preferences.
pub(super) struct InfoView {
    /// Dedicated virtualized table, never shared with Browse/SQL buffers or scrolling.
    table: Entity<TableState<MetadataGrid>>,
    /// Independent cancellation, also observed while waiting for the shared engine gate.
    cancel: Cancellation,
    /// Owned UI completion future; dropping it discards obsolete results.
    task: Option<Task<()>>,
    /// Current request identity, cleared on close, source switch and section changes.
    request: Option<InfoRequestKey>,
    /// Monotonic revision distinguishing retries for the same section and source.
    revision: u64,
    /// Detail-only error shown with a retry action; never changes the main query status.
    error: Option<String>,
}

impl InfoView {
    /// Construct the schema table; cell selection stays local to the table.
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(MetadataGrid::default(), window, cx)
                .cell_selectable(true)
                // The delegate draws a fixed number gutter in place of the built-in row header.
                .row_header(false)
                .col_movable(false)
        });
        Self {
            table,
            cancel: Cancellation::default(),
            task: None,
            request: None,
            revision: 0,
            error: None,
        }
    }

    /// Cancel the old request and release its buffers without waiting for worker cleanup.
    fn clear(&mut self, cx: &mut Context<Workspace>) {
        self.cancel.cancel();
        self.task = None;
        self.cancel = Cancellation::default();
        self.revision += 1;
        self.request = None;
        self.error = None;
        self.table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table.delegate_mut().rows.clear();
            table.delegate_mut().loading = false;
            table.refresh(cx);
        });
    }
}

impl Drop for InfoView {
    /// Signal background metadata work when the containing workspace is released.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Workspace {
    /// Toggle the active file's panel while preserving its last section.
    pub(super) fn toggle_file_info(&mut self, cx: &mut Context<Self>) {
        let preferences = &mut self.active_tab_mut().info;
        preferences.open = !preferences.open;
        self.refresh_file_info(cx);
    }

    /// Close details and release its schema when the user chooses a data cell instead.
    pub(super) fn close_file_info(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.active_tab_mut().info.open = false;
        }
        self.file_info.clear(cx);
        cx.notify();
    }

    /// Restore per-file preferences or reload the active schema with a fresh request ID.
    /// Overview has no worker. Other sections use bounded background reads, with failures
    /// confined to the panel. The main data query and its cancellation token are untouched.
    pub(super) fn refresh_file_info(&mut self, cx: &mut Context<Self>) {
        self.file_info.clear(cx);
        let tab = self.active_tab();
        if !tab.info.open || tab.source.parquet.is_none() || tab.changed {
            cx.notify();
            return;
        }
        if !tab.info.section {
            cx.notify();
            return;
        }
        let key = InfoRequestKey {
            source: tab.source.id,
            revision: self.file_info.revision,
        };
        self.file_info.request = Some(key);
        self.file_info.table.update(cx, |table, cx| {
            table.delegate_mut().loading = true;
            cx.notify();
        });
        let cancel = self.file_info.cancel.clone();
        let backend = self.backend.clone();
        let worker = cx
            .background_executor()
            .spawn(async move { backend.parquet_metadata(key.source, cancel) });
        self.file_info.task = Some(cx.spawn(async move |this, cx| {
            let result = worker.await;
            let _ = this.update(cx, |this, cx| {
                if !this.tabs.get(this.active_tab).is_some_and(|tab| {
                    key.is_current(this.file_info.request, tab.source.id, tab.info.open)
                }) {
                    return;
                }
                this.file_info.table.update(cx, |table, cx| {
                    table.delegate_mut().loading = false;
                    match result {
                        Ok(rows) => table.delegate_mut().rows = rows,
                        Err(error) => this.file_info.error = Some(crate::i18n::query_error(&error)),
                    }
                    table.refresh(cx);
                    if !table.delegate().rows.is_empty() {
                        table.scroll_to_row(0, cx);
                        table.scroll_to_col(0, cx);
                    }
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Select the requested section and load its contents.
    fn select_info_section(&mut self, index: usize, cx: &mut Context<Self>) {
        let section = index == 1;
        self.active_tab_mut().info.section = section;
        self.refresh_file_info(cx);
    }
}

/// Label source-file counts explicitly, keeping them distinct from filtered/query row counts.
pub(super) fn source_counts(source: &Source) -> Option<SharedString> {
    let summary = source.parquet.as_ref()?;
    Some(tr!(
        "info.source_counts",
        rows = source.rows.map_or("?".into(), |rows| rows.to_string()),
        columns = summary.columns
    ))
}

#[cfg(test)]
mod tests {
    use super::{InfoRequestKey, source_counts};
    use rowglide_core::{ParquetSummary, Source};

    /// Only the currently visible source and request revision may publish a result.
    #[test]
    fn request_identity() {
        let key = InfoRequestKey {
            source: 1,
            revision: 1,
        };
        assert!(key.is_current(Some(key), 1, true));
        assert!(!key.is_current(None, 1, true));
        assert!(!key.is_current(Some(key), 1, false));
        assert!(!key.is_current(Some(key), 2, true));
        assert!(!key.is_current(Some(InfoRequestKey { revision: 2, ..key }), 1, true));
        assert!(key != InfoRequestKey { source: 2, ..key });
    }

    /// Zero source rows remain exact, and source counts never derive from a filtered data page.
    #[test]
    fn source_counts_distinguish_zero_unknown_and_non_parquet() {
        let mut source = Source {
            schema: std::sync::Arc::new(arrow_schema::Schema::empty()),
            schema_revision: 0,
            id: 0,
            name: "empty.parquet".into(),
            format: "PARQUET",
            rows: Some(0),
            description: "Test",
            parquet: Some(ParquetSummary {
                file_bytes: 100,
                columns: 3,
                created_by: None,
                format_version: 1,
            }),
        };
        assert_eq!(
            source_counts(&source).as_deref(),
            Some("Source 0 rows · 3 columns")
        );
        source.rows = None;
        assert_eq!(
            source_counts(&source).as_deref(),
            Some("Source ? rows · 3 columns")
        );
        source.parquet = None;
        assert_eq!(source_counts(&source), None);
    }
}
