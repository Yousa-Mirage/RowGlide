//! Scalar Arrow predicate evaluation for deterministic fixtures, not a SQL interpreter.

use super::{
    CompareOp, FilterCondition, FilterKind, FilterValue, filter_data_type, filter_kind,
    parse_filter_value, validate_filters,
};
use crate::{Cancellation, Query, QueryError};
use arrow_array::{Array, ArrayRef, Float32Array, Float64Array, RecordBatch};
use std::cmp::Ordering;

/// Return matching local row indices after validating and decoding one bounded Arrow batch.
/// Comparisons follow SQL null exclusion. Source filtering precedes sorting and pagination.
pub(crate) fn matching_rows(
    batch: &RecordBatch,
    query: &Query,
    cancel: &Cancellation,
) -> Result<Vec<usize>, QueryError> {
    let conditions = validate_filters(batch.schema().as_ref(), &query.filters)?;
    let arrays = batch
        .columns()
        .iter()
        .map(|array| {
            if array.data_type() != filter_data_type(array.data_type()) {
                arrow_cast::cast(array.as_ref(), filter_data_type(array.data_type()))
            } else {
                Ok(array.clone())
            }
        })
        .collect::<Result<Vec<ArrayRef>, _>>()?;
    let text_columns = arrays
        .iter()
        .enumerate()
        .filter(|(_, array)| filter_kind(array.data_type()) == Some(FilterKind::Text))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let search = fold(query.search.trim());
    if !search.is_empty() && text_columns.is_empty() {
        return Err(super::FilterError::NoTextColumns.into());
    }
    let mut rows = Vec::new();
    for row in 0..batch.num_rows() {
        if row % 256 == 0 {
            cancel.check()?;
        }
        let mut matches = search.is_empty();
        if !matches {
            for index in &text_columns {
                if !arrays[*index].is_null(row)
                    && fold(&arrow_cast::display::array_value_to_string(
                        arrays[*index].as_ref(),
                        row,
                    )?)
                    .contains(&search)
                {
                    matches = true;
                    break;
                }
            }
        }
        if !matches {
            continue;
        }
        for condition in &conditions {
            let array = arrays[condition.column()].as_ref();
            let pass = match condition {
                FilterCondition::IsNull { negated, .. } => array.is_null(row) != *negated,
                FilterCondition::Compare { op, value, .. } => {
                    !array.is_null(row) && compare(array, row, *op, value)?
                }
            };
            if !pass {
                matches = false;
                break;
            }
        }
        if matches {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Apply Unicode simple lowercase per codepoint, matching DuckDB's non-contextual lowercase.
fn fold(text: &str) -> String {
    text.chars()
        .map(|ch| ch.to_lowercase().next().unwrap_or(ch))
        .collect()
}

/// Match a complete Unicode string with SQL LIKE wildcards and no escape character.
/// A percent can absorb one more character on mismatch. Retaining only the latest
/// percent position avoids recursion or allocating a table for the whole input.
fn like(mut text: &str, mut pattern: &str) -> bool {
    let mut retry = None;
    while let Some(ch) = text.chars().next() {
        match pattern.chars().next() {
            Some('%') => {
                pattern = pattern.trim_start_matches('%');
                if pattern.is_empty() {
                    return true;
                }
                retry = Some((text, pattern));
            }
            Some(token) if token == '_' || token == ch => {
                text = &text[ch.len_utf8()..];
                pattern = &pattern[token.len_utf8()..];
            }
            _ => {
                let Some((previous, suffix)) = retry else {
                    return false;
                };
                let Some(skipped) = previous.chars().next() else {
                    return false;
                };
                text = &previous[skipped.len_utf8()..];
                pattern = suffix;
                retry = Some((text, suffix));
            }
        }
    }
    pattern.trim_start_matches('%').is_empty()
}

/// Compare a non-null Arrow scalar without converting exact numeric values through floats.
fn compare(
    array: &dyn Array,
    row: usize,
    op: CompareOp,
    value: &FilterValue,
) -> Result<bool, QueryError> {
    let text = arrow_cast::display::array_value_to_string(array, row)?;
    if let FilterValue::Text(expected) = value {
        return Ok(match op {
            CompareOp::Contains => fold(&text).contains(&fold(expected)),
            CompareOp::StartsWith => fold(&text).starts_with(&fold(expected)),
            CompareOp::Like => like(&text, expected),
            CompareOp::EndsWith => fold(&text).ends_with(&fold(expected)),
            CompareOp::Equals => text == *expected,
            CompareOp::NotEquals => text != *expected,
            _ => return Err(super::FilterError::Operator.into()),
        });
    }
    let ordering = match value {
        FilterValue::Float(expected) => {
            let actual = if let Some(array) = array.as_any().downcast_ref::<Float32Array>() {
                f64::from(array.value(row))
            } else if let Some(array) = array.as_any().downcast_ref::<Float64Array>() {
                array.value(row)
            } else {
                return Err(super::FilterError::ValueType.into());
            };
            if actual.is_nan() {
                Ordering::Greater
            } else {
                actual
                    .partial_cmp(expected)
                    .ok_or(super::FilterError::ValueType)?
            }
        }
        FilterValue::Timestamp(expected) => {
            let actual = text.replace(' ', "T");
            let actual = match parse_filter_value(array.data_type(), &actual)? {
                FilterValue::Timestamp(value) => value,
                _ => return Err(super::FilterError::ValueType.into()),
            };
            timestamp_ticks(&actual)?.cmp(&timestamp_ticks(expected)?)
        }
        _ => {
            let parsed = parse_filter_value(array.data_type(), &text)?;
            match (&parsed, value) {
                (FilterValue::Integer(a), FilterValue::Integer(b)) => a.cmp(b),
                (FilterValue::Unsigned(a), FilterValue::Unsigned(b)) => a.cmp(b),
                (FilterValue::Boolean(a), FilterValue::Boolean(b)) => a.cmp(b),
                (FilterValue::Date(a), FilterValue::Date(b)) => a.cmp(b),
                (FilterValue::Decimal(a), FilterValue::Decimal(b)) => {
                    decimal_integer(a)?.cmp(&decimal_integer(b)?)
                }
                _ => return Err(super::FilterError::ValueType.into()),
            }
        }
    };
    Ok(match op {
        CompareOp::Equals => ordering.is_eq(),
        CompareOp::NotEquals => !ordering.is_eq(),
        CompareOp::GreaterThan => ordering.is_gt(),
        CompareOp::GreaterThanOrEqual => !ordering.is_lt(),
        CompareOp::LessThan => ordering.is_lt(),
        CompareOp::LessThanOrEqual => !ordering.is_gt(),
        _ => return Err(super::FilterError::Operator.into()),
    })
}

/// Compare canonical same-scale decimal strings through a checked 128-bit scaled integer.
fn decimal_integer(text: &str) -> Result<i128, QueryError> {
    Ok(text
        .replace('.', "")
        .parse()
        .map_err(|_| super::FilterError::ValueType)?)
}

/// Compare validated timestamps as seconds/nanoseconds, retaining precision outside i64 nanoseconds.
fn timestamp_ticks(text: &str) -> Result<(i64, u32), QueryError> {
    let value = if text.ends_with('Z') {
        chrono::DateTime::parse_from_rfc3339(text)
            .map_err(|_| super::FilterError::ValueType)?
            .to_utc()
    } else {
        chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
            .map_err(|_| super::FilterError::ValueType)?
            .and_utc()
    };
    Ok((value.timestamp(), value.timestamp_subsec_nanos()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FilterCondition;

    /// Dictionary nulls follow decoded values; Float32 comparisons retain their binary precision.
    #[test]
    fn dictionary_nulls_and_float32() -> Result<(), Box<dyn std::error::Error>> {
        use arrow_array::{DictionaryArray, Int8Array, StringArray, types::Int8Type};
        use std::sync::Arc;
        let dictionary = DictionaryArray::<Int8Type>::try_new(
            Int8Array::from(vec![Some(0), Some(1), None, Some(2)]),
            Arc::new(StringArray::from(vec![Some("London"), None, Some("")])),
        )?;
        let batch = RecordBatch::try_from_iter([
            ("text", Arc::new(dictionary) as ArrayRef),
            (
                "float",
                Arc::new(Float32Array::from(vec![0.1, 0.2, 0.3, 0.4])) as ArrayRef,
            ),
        ])?;
        for (condition, expected) in [
            (
                FilterCondition::IsNull {
                    column: 0,
                    negated: false,
                },
                vec![1, 2],
            ),
            (
                FilterCondition::Compare {
                    column: 0,
                    op: CompareOp::Equals,
                    value: FilterValue::Text(String::new()),
                },
                vec![3],
            ),
            (
                FilterCondition::Compare {
                    column: 1,
                    op: CompareOp::Equals,
                    value: FilterValue::Float(0.1),
                },
                vec![],
            ),
            (
                FilterCondition::Compare {
                    column: 1,
                    op: CompareOp::Equals,
                    value: FilterValue::Float(f64::from(0.1_f32)),
                },
                vec![0],
            ),
        ] {
            let query = Query {
                filters: vec![condition],
                ..Query::default()
            };
            assert_eq!(
                matching_rows(&batch, &query, &Cancellation::default())?,
                expected
            );
        }
        let conn = duckdb::Connection::open_in_memory()?;
        let matches: bool = conn.query_row("SELECT 0.1::FLOAT = ?", [0.1_f64], |row| row.get(0))?;
        assert!(!matches);
        let query = Query {
            search: " london ".into(),
            ..Query::default()
        };
        assert_eq!(
            matching_rows(&batch, &query, &Cancellation::default())?,
            vec![0]
        );
        Ok(())
    }

    /// Compare the evaluator and DuckDB on identical Arrow batches across exact scalar families.
    #[test]
    fn arrow_predicates_match_duckdb() -> Result<(), Box<dyn std::error::Error>> {
        let conn = duckdb::Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE current_file AS SELECT * FROM (VALUES
            (0, 'İ ΟΣ London', 18446744073709551615::UBIGINT, 12345678901234567890.12::DECIMAL(30,2), TIMESTAMP_NS '2026-01-01 00:00:00.123456789', 'NaN'::DOUBLE, true, DATE '2026-01-01'),
            (1, '', 1::UBIGINT, -1::DECIMAL(30,2), TIMESTAMP_NS '2025-01-01 00:00:00', 'Infinity'::DOUBLE, false, DATE '2025-01-01'),
            (2, NULL, 2::UBIGINT, NULL::DECIMAL(30,2), NULL::TIMESTAMP_NS, NULL::DOUBLE, NULL::BOOLEAN, NULL::DATE)
        ) t(id, text, unsigned, amount, ts, float, active, day)")?;
        let mut stmt = conn.prepare("SELECT * FROM current_file")?;
        let batch = stmt.query_arrow([])?.next().ok_or("Missing fixture")?;
        for (column, op, value) in [
            (1, CompareOp::Like, FilterValue::Text("_ ΟΣ %on".into())),
            (1, CompareOp::Like, FilterValue::Text("%o%on".into())),
            (1, CompareOp::Like, FilterValue::Text("%london".into())),
            (1, CompareOp::Like, FilterValue::Text("London".into())),
            (1, CompareOp::Like, FilterValue::Text("%%".into())),
            (1, CompareOp::Like, FilterValue::Text(String::new())),
            (1, CompareOp::Contains, FilterValue::Text("i οσ".into())),
            (1, CompareOp::Equals, FilterValue::Text("".into())),
            (2, CompareOp::Equals, FilterValue::Unsigned(u64::MAX)),
            (
                3,
                CompareOp::GreaterThan,
                FilterValue::Decimal("12345678901234567890.11".into()),
            ),
            (
                4,
                CompareOp::Equals,
                FilterValue::Timestamp("2026-01-01T00:00:00.123456789".into()),
            ),
            (5, CompareOp::GreaterThan, FilterValue::Float(1.)),
            (6, CompareOp::NotEquals, FilterValue::Boolean(true)),
            (
                7,
                CompareOp::GreaterThan,
                FilterValue::Date("2025-01-01".into()),
            ),
        ] {
            let query = Query {
                filters: vec![FilterCondition::Compare { column, op, value }],
                ..Query::default()
            };
            let expected = matching_rows(&batch, &query, &Cancellation::default())?;
            let (sql, params) = crate::duckdb::test_filter_sql(&query, &batch.schema())?;
            let mut statement = conn.prepare(&sql)?;
            let actual = statement
                .query_map(duckdb::params_from_iter(params.iter()), |row| {
                    row.get::<_, usize>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(expected, actual, "{:?}", query.filters);
        }
        Ok(())
    }
}
