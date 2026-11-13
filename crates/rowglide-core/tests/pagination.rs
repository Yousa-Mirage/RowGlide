use arrow_array::Int64Array;
use duckdb::Connection;
use rowglide_core::{Backend, BrowsePage, Cancellation, DuckDbBackend, Preview, Query, Sort};

/// Read the retained first-column Int64 IDs in display order for page assertions.
///
/// Returns an error if a fixture unexpectedly has a different first-column type.
fn ids(preview: &Preview) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let mut ids = Vec::new();
    for batch in &preview.batches {
        let column = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or("Expected integer ID")?;
        ids.extend_from_slice(column.values());
    }
    Ok(ids)
}

/// Exercise Browse pagination against generated CSV and Parquet files.
///
/// Checks limits above 8,192, exact/empty final pages, filtered sort ties without
/// missing/duplicate IDs, backend requests without limits, and range overflow.
#[test]
fn real_files_have_complete_pages_and_stable_sorted_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let conn = Connection::open_in_memory()?;
    for format in ["CSV", "PARQUET"] {
        let path = temp.path().join(format!("pages.{}", format.to_lowercase()));
        conn.execute(
            &format!("COPY (SELECT i AS id, CASE WHEN i % 2 = 0 THEN 'London' ELSE 'Tokyo' END AS city FROM range(20000) t(i)) TO ? (FORMAT {format})"),
            [path.to_string_lossy().as_ref()],
        )?;
        let backend = DuckDbBackend::default();
        let source = backend.open(&path, Cancellation::default())?;
        let query = Query {
            source: source.id,
            ..Query::default()
        };

        // A user limit above 8,192 must be delivered in full.
        for (offset, expected, has_next) in
            [(0, 10000, true), (10000, 10000, false), (20000, 0, false)]
        {
            let page = BrowsePage::new(10000, offset)?.load(
                &backend,
                query.clone(),
                &Cancellation::default(),
            )?;
            assert_eq!(page.preview.rows, expected);
            assert_eq!(page.has_next, has_next);
            assert!(!page.preview.truncated);
            assert_eq!(
                ids(&page.preview)?,
                (offset as i64..(offset + expected) as i64).collect::<Vec<_>>()
            );
            assert_eq!(page.preview.schema.fields().len(), 2);
        }

        // All filtered sort keys tie; the source ordinal prevents duplicates.
        let filtered = Query {
            search: "London".into(),
            sort: Some(Sort {
                column: 1,
                descending: true,
            }),
            ..query.clone()
        };
        let mut combined = Vec::new();
        for (offset, expected, has_next) in [(0, 6000, true), (6000, 4000, false)] {
            let page = BrowsePage::new(6000, offset)?.load(
                &backend,
                filtered.clone(),
                &Cancellation::default(),
            )?;
            assert_eq!(page.preview.rows, expected);
            assert_eq!(page.has_next, has_next);
            combined.extend(ids(&page.preview)?);
        }
        assert_eq!(combined, (0..20000).step_by(2).collect::<Vec<_>>());

        // The backend itself also honors explicit limits and unlimited pulls.
        for limit in [Some(12000), None] {
            let mut cursor = backend.query(
                Query {
                    limit,
                    ..query.clone()
                },
                Cancellation::default(),
            )?;
            let mut rows = 0;
            while let Some(batch) = cursor.next_batch()? {
                rows += batch.num_rows();
            }
            assert_eq!(rows, limit.unwrap_or(20000));
        }
        assert!(
            backend
                .query(
                    Query {
                        limit: Some(usize::MAX),
                        ..query
                    },
                    Cancellation::default()
                )
                .is_err()
        );
    }
    Ok(())
}
