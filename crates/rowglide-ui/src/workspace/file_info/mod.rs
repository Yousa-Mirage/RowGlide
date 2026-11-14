//! Per-file panel preferences, cancellable detail loading and compact Parquet presentation.

mod render;
mod table;

use super::Workspace;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::*;
use rowglide_core::{Cancellation, ParquetRequest, ParquetSection, Source};
use table::{MetadataGrid, value_text};

/// Lightweight preferences saved independently for every file, separate from Browse/SQL mode.
#[derive(Default)]
pub(super) struct InfoPreferences {
    /// Whether this tab presents file information instead of the selected-cell inspector.
    pub(super) open: bool,
    /// None selects Overview, which needs no additional query.
    section: Option<ParquetSection>,
    /// Zero-based metadata record offset; unrelated to the data toolbar's Offset input.
    offset: usize,
}

impl InfoPreferences {
    /// Change metadata categories and reset pagination only when the category actually changes.
    fn select(&mut self, section: Option<ParquetSection>) {
        if self.section != section {
            self.section = section;
            self.offset = 0;
        }
    }
}

/// Identity for one detail request; a later request can never overwrite another file or page.
#[derive(Clone, Copy, PartialEq, Eq)]
struct InfoRequestKey {
    /// Backend-local source ID, independent of active tab position and query mode.
    source: usize,
    /// Incremented whenever panel work is cancelled or replaced.
    revision: u64,
    /// Validated section and offset captured before starting background work.
    request: ParquetRequest,
}

impl InfoRequestKey {
    /// Accept a completion only while the same request and source remain visible.
    /// Overview/close clears the current identity; retry and page changes advance its revision.
    fn is_current(self, current: Option<Self>, source: usize, open: bool) -> bool {
        open && current == Some(self) && source == self.source
    }
}

/// One active metadata page and its worker lifecycle; inactive tabs retain only preferences.
pub(super) struct InfoView {
    /// Dedicated virtualized table, never shared with Browse/SQL buffers or scrolling.
    table: Entity<TableState<MetadataGrid>>,
    /// Selected page-local row and column for full-value preview/copy.
    selected: Option<(usize, usize)>,
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
    /// Brief confirmation after copying the complete selected value.
    copied: bool,
    /// Keeps metadata cell selection wired to this view's preview.
    _selection: Subscription,
}

impl InfoView {
    /// Construct an independent small table and subscribe to metadata-cell selection.
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(MetadataGrid::default(), window, cx)
                .cell_selectable(true)
                .col_movable(false)
        });
        let selection = cx.subscribe(&table, |this, _, event, cx| {
            if let TableEvent::SelectCell(row, column) = event {
                this.file_info.selected = Some((*row, *column));
                this.file_info.copied = false;
                cx.notify();
            }
        });
        Self {
            table,
            selected: None,
            cancel: Cancellation::default(),
            task: None,
            request: None,
            revision: 0,
            error: None,
            copied: false,
            _selection: selection,
        }
    }

    /// Cancel the old request and release its buffers without waiting for worker cleanup.
    fn clear(&mut self, cx: &mut Context<Workspace>) {
        self.cancel.cancel();
        self.task = None;
        self.cancel = Cancellation::default();
        self.revision += 1;
        self.request = None;
        self.selected = None;
        self.error = None;
        self.copied = false;
        self.table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table.delegate_mut().page = None;
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
    /// Toggle the active file's panel while preserving its last section and page preference.
    pub(super) fn toggle_file_info(&mut self, cx: &mut Context<Self>) {
        let preferences = &mut self.active_tab_mut().info;
        preferences.open = !preferences.open;
        self.refresh_file_info(cx);
    }

    /// Close details and release its page when the user chooses a data cell instead.
    pub(super) fn close_file_info(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.active_tab_mut().info.open = false;
        }
        self.file_info.clear(cx);
        cx.notify();
    }

    /// Restore per-file preferences or reload the active detail page with a fresh request ID.
    /// Overview has no worker. Other sections use bounded background reads, with failures
    /// confined to the panel. The main data query and its cancellation token are untouched.
    pub(super) fn refresh_file_info(&mut self, cx: &mut Context<Self>) {
        self.file_info.clear(cx);
        let tab = self.active_tab();
        if !tab.info.open || tab.source.parquet.is_none() {
            cx.notify();
            return;
        }
        let Some(section) = tab.info.section else {
            cx.notify();
            return;
        };
        let request = match ParquetRequest::new(section, tab.info.offset) {
            Ok(request) => request,
            Err(error) => {
                self.file_info.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let key = InfoRequestKey {
            source: tab.source.id,
            revision: self.file_info.revision,
            request,
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
            .spawn(async move { backend.parquet_metadata(key.source, key.request, cancel) });
        self.file_info.task = Some(cx.spawn(async move |this, cx| {
            let result = worker.await;
            let _ = this.update(cx, |this, cx| {
                if !key.is_current(
                    this.file_info.request,
                    this.active_tab().source.id,
                    this.active_tab().info.open,
                ) {
                    return;
                }
                this.file_info.table.update(cx, |table, cx| {
                    table.delegate_mut().loading = false;
                    match result {
                        Ok(page) => table.delegate_mut().page = Some(page),
                        Err(error) => this.file_info.error = Some(error.to_string()),
                    }
                    table.refresh(cx);
                    if table
                        .delegate()
                        .page
                        .as_ref()
                        .is_some_and(|page| page.preview.rows > 0)
                    {
                        table.scroll_to_row(0, cx);
                        table.scroll_to_col(0, cx);
                    }
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Select Overview or one footer relation, resetting the relation offset on category changes.
    fn select_info_section(&mut self, index: usize, cx: &mut Context<Self>) {
        let section = match index {
            1 => Some(ParquetSection::Schema),
            2 => Some(ParquetSection::RowGroups),
            3 => Some(ParquetSection::KeyValue),
            _ => None,
        };
        self.active_tab_mut().info.select(section);
        self.refresh_file_info(cx);
    }

    /// Navigate the current metadata relation only after lookahead confirms another page.
    fn turn_info_page(&mut self, next: bool, cx: &mut Context<Self>) {
        let grid = self.file_info.table.read(cx).delegate();
        if grid.loading || (next && !grid.page.as_ref().is_some_and(|page| page.has_next)) {
            return;
        }
        let offset = self.active_tab().info.offset;
        self.active_tab_mut().info.offset = if next {
            offset.saturating_add(ParquetRequest::PAGE_SIZE)
        } else {
            offset.saturating_sub(ParquetRequest::PAGE_SIZE)
        };
        self.refresh_file_info(cx);
    }

    /// Copy the entire selected text, scalar or reversible binary hex value, never its preview.
    fn copy_info_value(&mut self, cx: &mut Context<Self>) {
        let result = self
            .file_info
            .selected
            .and_then(|(row, col)| self.file_info.table.read(cx).delegate().cell(row, col))
            .map(|(array, row)| value_text(array, row, None));
        match result {
            Some(Ok(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.file_info.copied = true;
            }
            Some(Err(error)) => self.file_info.error = Some(error.to_string()),
            None => {}
        }
        cx.notify();
    }
}

/// Label source-file counts explicitly, keeping them distinct from filtered/query row counts.
pub(super) fn source_counts(source: &Source) -> Option<String> {
    let summary = source.parquet.as_ref()?;
    Some(format!(
        "Source {} rows · {} columns",
        source.rows.map_or("?".into(), |rows| rows.to_string()),
        summary.columns
    ))
}

#[cfg(test)]
mod tests {
    use super::{InfoPreferences, InfoRequestKey, source_counts};
    use rowglide_core::{ParquetRequest, ParquetSection};
    use rowglide_core::{ParquetSummary, Source};

    /// Per-tab section/page preferences stay independent; section changes reset only their page.
    #[test]
    fn preferences_and_request_identity() -> Result<(), rowglide_core::QueryError> {
        let mut first = InfoPreferences {
            open: true,
            section: Some(ParquetSection::Schema),
            offset: 100,
        };
        let second = InfoPreferences::default();
        first.select(Some(ParquetSection::Schema));
        assert_eq!(first.offset, 100);
        first.select(Some(ParquetSection::RowGroups));
        assert_eq!(first.offset, 0);
        assert!(!second.open && second.section.is_none());
        let key = InfoRequestKey {
            source: 1,
            revision: 1,
            request: ParquetRequest::new(ParquetSection::Schema, 0)?,
        };
        assert!(key.is_current(Some(key), 1, true));
        assert!(!key.is_current(None, 1, true));
        assert!(!key.is_current(Some(key), 1, false));
        assert!(!key.is_current(Some(key), 2, true));
        assert!(!key.is_current(Some(InfoRequestKey { revision: 2, ..key }), 1, true));
        assert!(key != InfoRequestKey { source: 2, ..key });
        assert!(
            key != InfoRequestKey {
                request: ParquetRequest::new(ParquetSection::Schema, 100)?,
                ..key
            }
        );
        Ok(())
    }

    /// Zero source rows remain exact, and source counts never derive from a filtered data page.
    #[test]
    fn source_counts_distinguish_zero_unknown_and_non_parquet() {
        let mut source = Source {
            id: 0,
            name: "empty.parquet".into(),
            format: "PARQUET",
            rows: Some(0),
            description: "Test",
            parquet: Some(ParquetSummary {
                file_bytes: 100,
                columns: 3,
                row_groups: 0,
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
