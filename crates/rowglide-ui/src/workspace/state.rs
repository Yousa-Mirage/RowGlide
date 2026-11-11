//! Per-file browse and SQL sessions with bounded result retention.

use arrow_array::ArrayRef;
use rowglide_app::{Preview, Query, Source};
use std::collections::HashSet;

/// The result mode controls which query state and result are active for a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabMode {
    Browse,
    Sql,
}

/// Identifies the only request allowed to update the active view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RequestKey {
    pub(super) generation: u64,
    pub(super) tab: usize,
    pub(super) mode: TabMode,
}

/// Editable text is independent of the last successfully applied browse query.
pub(super) struct BrowseDraft {
    pub(super) search: String,
    pub(super) limit: String,
    pub(super) offset: String,
}

impl Default for BrowseDraft {
    fn default() -> Self {
        Self {
            search: String::new(),
            limit: String::new(),
            offset: "0".into(),
        }
    }
}

/// Persistent state for one open file tab.
///
/// Input state is retained when switching tabs. Results are intentionally
/// bounded; evicted results require an explicit rerun.
pub(super) struct TabState {
    pub(super) source: Source,
    pub(super) browse: Query,
    pub(super) sql: String,
    pub(super) mode: TabMode,
    pub(super) drafts: BrowseDraft,
    pub(super) last_sql: Option<String>,
    pub(super) browse_result: ResultState,
    pub(super) sql_result: ResultState,
}

impl TabState {
    pub(super) fn new(source: Source) -> Self {
        Self {
            browse: Query {
                source: source.id,
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

    pub(super) fn result_mut(&mut self) -> &mut ResultState {
        match self.mode {
            TabMode::Browse => &mut self.browse_result,
            TabMode::Sql => &mut self.sql_result,
        }
    }

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

/// A mode's cached result and inspector state; inputs survive eviction.
pub(super) struct ResultState {
    pub(super) preview: Option<Preview>,
    pub(super) status: String,
    pub(super) error: bool,
    pub(super) selected: Option<(String, ArrayRef, usize)>,
    pub(super) expanded: HashSet<String>,
    pub(super) touched: u64,
    pub(super) initialized: bool,
}

impl Default for ResultState {
    fn default() -> Self {
        Self {
            preview: None,
            status: "Ready".into(),
            error: false,
            selected: None,
            expanded: HashSet::new(),
            touched: 0,
            initialized: false,
        }
    }
}

// At most three inactive previews plus the active preview (16 MiB each).
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
                .filter(|(_, result)| result.preview.is_some())
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
        result.selected = None;
        result.expanded.clear();
        result.status = "Preview released · run again to reload".into();
        result.error = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use rowglide_app::Sort;
    use std::sync::Arc;

    fn tab(id: usize) -> TabState {
        TabState::new(Source {
            id,
            name: format!("{id}.parquet"),
            format: "PARQUET",
            rows: 1,
            description: "Test",
        })
    }

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
}
