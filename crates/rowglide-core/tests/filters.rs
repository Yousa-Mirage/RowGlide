//! Typed predicate execution against real Parquet, including exact values and SQL isolation.

use arrow_array::{Array, UInt64Array};
use rowglide_core::{
    Backend, BrowsePage, Cancellation, CompareOp, DuckDbBackend, FilterCondition, FilterError,
    FilterValue, Query, QueryError,
};
use std::{error::Error, path::Path};

/// Create a typed Parquet fixture with null/empty text and exact numeric/temporal columns.
fn fixture(path: &Path) -> Result<(), Box<dyn Error>> {
    let conn = duckdb::Connection::open_in_memory()?;
    conn.execute(
        "SET VARIABLE output = ?",
        [path.to_str().ok_or("Invalid path")?],
    )?;
    conn.execute_batch("COPY (SELECT * FROM (VALUES
      (1::UBIGINT, 'London', 'X_100%', 100.01::DECIMAL(38,2), DATE '2026-01-01', TIMESTAMP_NS '2026-01-01 01:02:03.123456789', true),
      (18446744073709551615::UBIGINT, 'LONDON', '', 100000000000000000000000000000000000.02::DECIMAL(38,2), DATE '2026-02-01', TIMESTAMP_NS '2026-01-01 01:02:03.123456790', false),
      (3::UBIGINT, 'Paris', NULL, 1::DECIMAL(38,2), DATE '2025-01-01', TIMESTAMP_NS '2025-01-01 00:00:00', NULL)
    ) t(id, city, note, amount, created, ts, active)) TO (getvariable('output')) (FORMAT PARQUET)")?;
    Ok(())
}

/// Execute a Browse request and return source IDs without relying on formatting.
fn ids(backend: &dyn Backend, query: Query) -> Result<Vec<u64>, Box<dyn Error>> {
    let page = BrowsePage::new(query.limit.unwrap_or(100), query.offset)?.load(
        backend,
        query,
        &Cancellation::default(),
    )?;
    let mut ids = Vec::new();
    for batch in page.preview.batches {
        let values = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or("Expected UInt64")?;
        ids.extend((0..values.len()).map(|row| values.value(row)));
    }
    Ok(ids)
}

/// Build a comparison in source-schema coordinates.
fn compare(column: usize, op: CompareOp, value: FilterValue) -> FilterCondition {
    FilterCondition::Compare { column, op, value }
}

/// Bound predicates preserve exact decimals, UInt64, nanoseconds, nulls and text literal semantics.
#[test]
fn exact_types_and_nulls() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("typed.parquet");
    fixture(&path)?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    for (condition, expected) in [
        (
            compare(0, CompareOp::Equals, FilterValue::Unsigned(u64::MAX)),
            vec![u64::MAX],
        ),
        (
            compare(1, CompareOp::Contains, FilterValue::Text("london".into())),
            vec![1, u64::MAX],
        ),
        (
            compare(1, CompareOp::Equals, FilterValue::Text("London".into())),
            vec![1],
        ),
        (
            compare(2, CompareOp::Contains, FilterValue::Text("_100%".into())),
            vec![1],
        ),
        (
            compare(2, CompareOp::Equals, FilterValue::Text("".into())),
            vec![u64::MAX],
        ),
        (
            compare(2, CompareOp::NotEquals, FilterValue::Text("".into())),
            vec![1],
        ),
        (
            FilterCondition::IsNull {
                column: 2,
                negated: false,
            },
            vec![3],
        ),
        (
            FilterCondition::IsNull {
                column: 2,
                negated: true,
            },
            vec![1, u64::MAX],
        ),
        (
            compare(
                3,
                CompareOp::Equals,
                FilterValue::Decimal("100000000000000000000000000000000000.02".into()),
            ),
            vec![u64::MAX],
        ),
        (
            compare(
                4,
                CompareOp::GreaterThanOrEqual,
                FilterValue::Date("2026-02-01".into()),
            ),
            vec![u64::MAX],
        ),
        (
            compare(
                5,
                CompareOp::Equals,
                FilterValue::Timestamp("2026-01-01T01:02:03.123456789".into()),
            ),
            vec![1],
        ),
        (
            compare(6, CompareOp::Equals, FilterValue::Boolean(false)),
            vec![u64::MAX],
        ),
    ] {
        assert_eq!(
            ids(
                &backend,
                Query {
                    source: source.id,
                    filters: vec![condition.clone()],
                    ..Query::default()
                }
            )?,
            expected,
            "{condition:?}"
        );
    }
    Ok(())
}

/// Parentheses, ordered binds and pagination preserve AND semantics; explicit SQL ignores Browse.
#[test]
fn predicates_precede_paging_and_sql_is_independent() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("typed.parquet");
    fixture(&path)?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let query = Query {
        source: source.id,
        search: " London ".into(),
        filters: vec![compare(6, CompareOp::Equals, FilterValue::Boolean(false))],
        limit: Some(1),
        ..Query::default()
    };
    assert_eq!(ids(&backend, query.clone())?, vec![u64::MAX]);
    assert!(ids(&backend, Query { offset: 1, ..query })?.is_empty());
    let cursor = backend.query(
        Query {
            source: source.id,
            sql: Some("SELECT * FROM current_file".into()),
            schema_revision: 99,
            filters: vec![FilterCondition::IsNull {
                column: 999,
                negated: false,
            }],
            search: "missing".into(),
            limit: Some(0),
            offset: 999,
            ..Query::default()
        },
        Cancellation::default(),
    )?;
    assert_eq!(
        rowglide_core::Preview::collect(cursor, &Cancellation::default())?.rows,
        3
    );
    Ok(())
}

/// Missing text columns and hostile column names produce explicit validation or literal matching.
#[test]
fn no_text_and_quoted_names() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("number.csv");
    std::fs::write(&path, "number\n1\n2\n")?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert!(matches!(
        backend.query(
            Query {
                source: source.id,
                search: "1".into(),
                ..Query::default()
            },
            Cancellation::default()
        ),
        Err(QueryError::Filter(FilterError::NoTextColumns))
    ));
    Ok(())
}
