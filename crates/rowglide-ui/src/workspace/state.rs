//! Per-file browse and SQL sessions with bounded result retention.

use arrow_array::ArrayRef;
use rowglide_core::{BrowsePage, Preview, Query, QueryError, QueryResult, Source};
use std::{collections::HashSet, sync::Arc};

/// Select which independent query and result session a file tab exposes.
///
/// Browse uses source filtering/sorting and explicit pages; SQL uses the editor's
/// last executed statement and its own result schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabMode {
    /// Structured source filtering, sorting and explicit page navigation.
    Browse,
    /// User-authored query draft, last execution and independent result schema.
    Sql,
}

/// Identity of the only background request allowed to update the active view.
///
/// Generation changes invalidate replaced/cancelled requests. Tab index and mode
/// prevent cross-session writes; indices remain valid identifiers only while tabs
/// are not removed or reordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RequestKey {
    /// Monotonic workspace request revision, also advanced on cancellation/restoration.
    pub(super) generation: u64,
    /// Active tab index at submission; valid while tabs are not removed or reordered.
    pub(super) tab: usize,
    /// Browse or SQL session that submitted this request.
    pub(super) mode: TabMode,
}

/// Editable Browse text kept independently from the last applied query.
///
/// Values may be incomplete or invalid. Parsing occurs on Apply/navigation checks;
/// switching tabs preserves the original strings rather than silently correcting them.
pub(super) struct BrowseDraft {
    /// Unapplied filter text, preserved exactly while editing.
    pub(super) search: String,
    /// Editable page-size text, including blanks or invalid drafts.
    pub(super) limit: String,
    /// Editable zero-based start text, including blanks or invalid drafts.
    pub(super) offset: String,
}

impl Default for BrowseDraft {
    /// Initialize empty search text, the default 1,000-row page size and offset zero.
    fn default() -> Self {
        Self {
            search: String::new(),
            limit: BrowsePage::default().limit().to_string(),
            offset: "0".into(),
        }
    }
}

impl BrowseDraft {
    /// Parse range drafts into a validated positive-limit Browse page.
    ///
    /// Whitespace is ignored; blank Limit/Offset resolve to 1,000/0. Returns errors for
    /// invalid integers, zero page sizes or ranges exceeding the lookahead-safe bound.
    /// The original draft strings are not modified.
    pub(super) fn page(&self) -> Result<BrowsePage, QueryError> {
        let parse = |text: &str, default: usize| {
            if text.trim().is_empty() {
                Ok(default)
            } else {
                text.trim().parse::<usize>().map_err(|_| QueryError::Invalid(
                    "Limit must be a positive whole number; Offset must be a non-negative whole number".into(),
                ))
            }
        };
        BrowsePage::new(
            parse(&self.limit, BrowsePage::default().limit())?,
            parse(&self.offset, 0)?,
        )
    }

    /// Check whether the current text describes the applied Browse query.
    ///
    /// Search text must match exactly; range text is compared by parsed numeric value.
    /// Invalid ranges or pending changes return false and disable page navigation.
    pub(super) fn matches_query(&self, query: &Query) -> bool {
        self.search == query.search
            && self.page().is_ok_and(|page| {
                Some(page.limit()) == query.limit && page.offset() == query.offset
            })
    }
}

/// Completed Browse page metadata kept separately from the active table buffers.
///
/// Stores the requested range, actual displayed row count and one-row lookahead
/// result. It belongs to one tab's Browse result and is cleared on replacement or
/// eviction so navigation cannot reuse stale next-page information.
#[derive(Clone, Copy)]
pub(super) struct PageSummary {
    /// Validated page requested by the completed Browse operation.
    pub(super) range: BrowsePage,
    /// Actual retained rows, at most the requested page size.
    pub(super) rows: usize,
    /// Whether lookahead fetched a row beyond the displayed page.
    pub(super) has_next: bool,
}

impl PageSummary {
    /// Format a one-based displayed row range without claiming a total result count.
    ///
    /// Empty pages report zero rows and their requested offset. Nonempty pages show
    /// End of results only when lookahead found no further row.
    pub(super) fn label(self) -> String {
        if self.rows == 0 {
            format!("0 rows · Offset {}", self.range.offset())
        } else {
            format!(
                "Rows {}–{} · {} rows{}",
                self.range.offset() + 1,
                self.range.offset() + self.rows,
                self.rows,
                if self.has_next {
                    ""
                } else {
                    " · End of results"
                },
            )
        }
    }
}

/// Persistent queries, drafts and result snapshots for one registered file.
///
/// Browse and SQL own separate applied state. Inactive results live here; the
/// active result's Arrow buffers move into the shared table. Page metadata remains
/// with the tab. Drafts survive cancellation, mode switches and cache eviction.
pub(super) struct TabState {
    /// Registered file metadata shared by both modes.
    pub(super) source: Source,
    /// Last applied Browse query, independent of pending input text.
    pub(super) browse: Query,
    /// Saved SQL editor draft; execution uses last_sql instead.
    pub(super) sql: String,
    /// Session currently exposed when this file tab is active.
    pub(super) mode: TabMode,
    /// Saved Browse control strings, including unapplied changes.
    pub(super) drafts: BrowseDraft,
    /// Most recently submitted SQL text, or None before the first execution.
    pub(super) last_sql: Option<String>,
    /// Browse-only page metadata, retained buffers and inspector state.
    pub(super) browse_result: ResultState,
    /// SQL-only result buffers and inspector state.
    pub(super) sql_result: ResultState,
}

impl TabState {
    /// Create an uninitialized Browse tab for a registered source.
    ///
    /// Starts at the default first page and seeds the SQL editor with a 100-row example.
    /// Neither Browse nor SQL executes until the workspace activates/runs the session.
    pub(super) fn new(source: Source) -> Self {
        Self {
            browse: Query {
                source: source.id,
                limit: Some(BrowsePage::default().limit()),
                ..Query::default()
            },
            source,
            sql: "SELECT * FROM current_file LIMIT 100;".into(),
            mode: TabMode::Browse,
            drafts: BrowseDraft::default(),
            last_sql: None,
            browse_result: ResultState::default(),
            sql_result: ResultState::default(),
        }
    }

    /// Borrow the saved result state corresponding to this tab's active mode.
    pub(super) fn result_mut(&mut self) -> &mut ResultState {
        match self.mode {
            TabMode::Browse => &mut self.browse_result,
            TabMode::Sql => &mut self.sql_result,
        }
    }

    /// Determine whether `next` (Next) or its opposite (Previous) is currently usable.
    ///
    /// Requires Browse mode, no in-flight query and drafts matching the applied query.
    /// Next needs completed lookahead evidence; Previous only needs a positive applied
    /// offset, allowing recovery from empty or failed pages.
    pub(super) fn can_turn_page(&self, next: bool, busy: bool, draft: &BrowseDraft) -> bool {
        if self.mode != TabMode::Browse || busy || !draft.matches_query(&self.browse) {
            return false;
        }
        if next {
            self.browse_result.page.is_some_and(|page| page.has_next)
        } else {
            self.browse.offset > 0
        }
    }

    /// Clone the applied Browse query or rebuild a query from the last executed SQL.
    ///
    /// SQL drafts are excluded until Run is pressed. Before any SQL execution, the SQL
    /// field is `None`; callers must not automatically run that placeholder session.
    pub(super) fn active_query(&self) -> Query {
        match self.mode {
            TabMode::Browse => self.browse.clone(),
            TabMode::Sql => Query {
                source: self.source.id,
                sql: self.last_sql.clone(),
                ..Query::default()
            },
        }
    }
}

/// One mode's retained result, inspector state, status and cache bookkeeping.
///
/// `preview` and `sql_result` move into the active table while displayed and are
/// absent after eviction or before a result exists. `initialized` distinguishes a never-run session
/// from one requiring explicit reload. Page metadata is meaningful only for Browse.
pub(super) struct ResultState {
    /// Inactive result buffers; moved into the shared table while active.
    pub(super) preview: Option<Preview>,
    /// Retained SQL result and temporary file, independently owned from the Browse page.
    pub(super) sql_result: Option<Arc<QueryResult>>,
    /// Completed Browse navigation metadata; absent for SQL and invalidated pages.
    pub(super) page: Option<PageSummary>,
    /// Saved user-facing status text restored with this session.
    pub(super) status: String,
    /// Whether the saved status is displayed as an error.
    pub(super) error: bool,
    /// Inspector column label, shared Arrow array and row within that array.
    pub(super) selected: Option<(String, ArrayRef, usize)>,
    /// Inspector node paths currently expanded in this session.
    pub(super) expanded: HashSet<String>,
    /// Workspace generation at last save, used to choose cached-result eviction.
    pub(super) touched: u64,
    /// Whether a request has been started; prevents automatic reload after eviction.
    pub(super) initialized: bool,
}

impl Default for ResultState {
    /// Initialize an unqueried result session with Ready status and no retained data.
    fn default() -> Self {
        Self {
            preview: None,
            sql_result: None,
            page: None,
            status: "Ready".into(),
            error: false,
            selected: None,
            expanded: HashSet::new(),
            touched: 0,
            initialized: false,
        }
    }
}

/// Evict the least recently touched stored results until at most three remain.
///
/// The active result normally lives in the table and is not counted here. Eviction
/// releases Browse buffers, SQL file handles, inspector references and page metadata, keeping
/// drafts and applied queries for explicit reload. This bounds retained entries,
/// not all engine/transient allocations in the process.
pub(super) fn trim_cache(tabs: &mut [TabState]) {
    loop {
        let mut entries = tabs
            .iter()
            .enumerate()
            .flat_map(|(index, tab)| {
                [
                    (TabMode::Browse, &tab.browse_result),
                    (TabMode::Sql, &tab.sql_result),
                ]
                .into_iter()
                .filter(|(_, result)| result.preview.is_some() || result.sql_result.is_some())
                .map(move |(mode, result)| (result.touched, index, mode))
            })
            .collect::<Vec<_>>();
        if entries.len() <= 3 {
            break;
        }
        entries.sort_by_key(|entry| entry.0);
        let (_, index, mode) = entries[0];
        let tab = &mut tabs[index];
        let result = match mode {
            TabMode::Browse => &mut tab.browse_result,
            TabMode::Sql => &mut tab.sql_result,
        };
        result.preview = None;
        result.sql_result = None;
        result.page = None;
        result.selected = None;
        result.expanded.clear();
        result.status = "Result released · run again to reload".into();
        result.error = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use rowglide_core::Sort;
    use std::sync::Arc;

    /// Construct a minimal one-row source tab for pure state tests without GPUI setup.
    fn tab(id: usize) -> TabState {
        TabState::new(Source {
            id,
            name: format!("{id}.parquet"),
            format: "PARQUET",
            rows: Some(1),
            description: "Test",
        })
    }

    /// Verify that changing any request identity component makes a result stale.
    #[test]
    fn request_identity_rejects_other_tabs_modes_and_generations() {
        let current = RequestKey {
            generation: 7,
            tab: 2,
            mode: TabMode::Browse,
        };
        assert_ne!(
            current,
            RequestKey {
                generation: 6,
                ..current
            }
        );
        assert_ne!(current, RequestKey { tab: 1, ..current });
        assert_ne!(
            current,
            RequestKey {
                mode: TabMode::Sql,
                ..current
            }
        );
    }

    /// Check default/blank range parsing, invalid values and detection of unapplied edits.
    #[test]
    fn drafts_validate_page_sizes_and_require_apply() -> Result<(), QueryError> {
        let mut draft = BrowseDraft::default();
        let query = tab(0).browse;
        assert!(draft.matches_query(&query));
        draft.limit.clear();
        draft.offset.clear();
        assert_eq!(draft.page()?, BrowsePage::default());
        for invalid in ["0", "-1", "1.5", "abc", "18446744073709551616"] {
            draft.limit = invalid.into();
            assert!(draft.page().is_err());
            assert!(!draft.matches_query(&query));
        }
        draft.limit = " 10000 ".into();
        draft.offset = " 500 ".into();
        assert_eq!(draft.page()?, BrowsePage::new(10000, 500)?);
        assert!(!draft.matches_query(&query));
        draft.offset = "-1".into();
        assert!(draft.page().is_err());
        Ok(())
    }

    /// Exercise navigation eligibility through loading, pending edits, EOF and SQL switches.
    ///
    /// Also checks the displayed final-page range and isolation from a newly opened tab.
    #[test]
    fn navigation_obeys_completed_pages_pending_edits_and_mode() -> Result<(), QueryError> {
        let mut first = tab(0);
        let mut draft = BrowseDraft::default();
        assert!(!first.can_turn_page(false, false, &draft));
        assert!(!first.can_turn_page(true, false, &draft));
        first.browse_result.page = Some(PageSummary {
            range: BrowsePage::default(),
            rows: 1000,
            has_next: true,
        });
        assert!(first.can_turn_page(true, false, &draft));
        assert!(!first.can_turn_page(true, true, &draft));
        draft.search = "pending".into();
        assert!(!first.can_turn_page(true, false, &draft));
        draft.search.clear();

        first.browse.offset = 1000;
        draft.offset = "1000".into();
        first.browse_result.page = Some(PageSummary {
            range: BrowsePage::new(1000, 1000)?,
            rows: 50,
            has_next: false,
        });
        assert!(first.can_turn_page(false, false, &draft));
        assert!(!first.can_turn_page(true, false, &draft));
        first.mode = TabMode::Sql;
        assert!(!first.can_turn_page(false, false, &draft));
        first.mode = TabMode::Browse;
        assert!(first.can_turn_page(false, false, &draft));
        assert_eq!(
            first.browse_result.page.map(PageSummary::label).as_deref(),
            Some("Rows 1001–1050 · 50 rows · End of results")
        );
        assert!(tab(1).browse_result.page.is_none());
        Ok(())
    }

    /// Verify that SQL excludes Browse parameters while each mode keeps its own drafts.
    ///
    /// Switching back restores the applied Browse query, including sort and range,
    /// without applying invalid text or altering another file's session.
    #[test]
    fn modes_keep_applied_queries_and_drafts_separate() {
        let mut first = tab(4);
        first.browse.search = "London".into();
        first.browse.limit = Some(8);
        first.browse.offset = 2;
        first.browse.sort = Some(Sort {
            column: 0,
            descending: true,
        });
        first.drafts.limit = "invalid draft".into();
        first.sql = "unfinished SQL".into();
        first.last_sql = Some("SELECT * FROM current_file LIMIT 100;".into());
        first.mode = TabMode::Sql;
        let sql = first.active_query();
        assert_eq!(sql.source, 4);
        assert_eq!(sql.sql, first.last_sql);
        assert!(sql.search.is_empty());
        assert_eq!(sql.sort, None);
        assert_eq!(sql.limit, None);
        assert_eq!(sql.offset, 0);
        first.mode = TabMode::Browse;
        assert_eq!(first.active_query().search, "London");
        assert_eq!(first.active_query().limit, Some(8));
        assert_eq!(first.drafts.limit, "invalid draft");
        assert_eq!(first.sql, "unfinished SQL");
        let other = tab(9);
        assert!(other.active_query().search.is_empty());
        assert_eq!(other.mode, TabMode::Browse);
    }

    /// Verify that cache eviction releases both table and inspector Arrow references.
    ///
    /// Weak references detect retained-buffer leaks; assertions also ensure page
    /// metadata is cleared and editable SQL drafts survive eviction.
    #[test]
    fn eviction_releases_inspector_arrays_but_keeps_inputs()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut tabs = vec![tab(0), tab(1), tab(2), tab(3)];
        let values = Arc::new(Int64Array::from(vec![1]));
        let weak = Arc::downgrade(&values);
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
        for (index, tab) in tabs.iter_mut().enumerate() {
            tab.sql = format!("draft {index}");
            tab.browse_result.touched = index as u64;
            tab.browse_result.page = Some(PageSummary {
                range: BrowsePage::default(),
                rows: 1,
                has_next: false,
            });
            let array: ArrayRef = if index == 0 {
                values.clone()
            } else {
                Arc::new(Int64Array::from(vec![2]))
            };
            tab.browse_result.selected = Some(("id".into(), array.clone(), 0));
            tab.browse_result.preview = Some(Preview {
                schema: schema.clone(),
                batches: vec![RecordBatch::try_new(schema.clone(), vec![array])?],
                rows: 1,
                total_rows: Some(1),
                truncated: false,
            });
        }
        drop(values);
        trim_cache(&mut tabs);
        assert!(tabs[0].browse_result.preview.is_none());
        assert!(tabs[0].browse_result.page.is_none());
        assert!(tabs[0].browse_result.selected.is_none());
        assert!(weak.upgrade().is_none());
        assert_eq!(tabs[0].sql, "draft 0");
        assert!(
            tabs[1..]
                .iter()
                .all(|tab| tab.browse_result.preview.is_some())
        );
        Ok(())
    }

    /// SQL handles participate in the same eviction budget while drafts remain per tab.
    #[test]
    fn eviction_releases_sql_results() -> Result<(), QueryError> {
        let mut tabs = vec![tab(0), tab(1), tab(2), tab(3)];
        let mut handles = Vec::new();
        for (index, tab) in tabs.iter_mut().enumerate() {
            let result = Arc::new(QueryResult::start(
                Arc::new(rowglide_core::MockBackend),
                Query::default(),
                rowglide_core::Cancellation::default(),
                rowglide_core::ResultLimits::default(),
            )?);
            result.cancel();
            handles.push(Arc::downgrade(&result));
            tab.sql = format!("draft {index}");
            tab.sql_result.touched = index as u64;
            tab.sql_result.sql_result = Some(result);
        }
        trim_cache(&mut tabs);
        assert!(handles[0].upgrade().is_none());
        assert!(handles[1..].iter().all(|handle| handle.upgrade().is_some()));
        assert_eq!(tabs[0].sql, "draft 0");
        Ok(())
    }
}
