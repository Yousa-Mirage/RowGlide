//! SQL validation and parameterized browse queries.

use crate::{Query, QueryError};
use arrow_schema::{DataType, Schema};
use duckdb::types::Value;
use sqlparser::{ast::Statement, dialect::DuckDbDialect, parser::Parser};

/// Quote one SQL identifier, doubling embedded double quotes.
///
/// The result is suitable for a column name, not a literal value or a SQL fragment;
/// filter values continue to use bound parameters.
pub(super) fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Build SQL and ordered bind values for a Browse or explicit SQL request.
///
/// Browse filters string columns, applies a source-schema sort with source-order
/// tie breaking, then honors the requested Limit/Offset. SQL is parsed as one
/// SELECT/WITH query and serialized without an additional row limit; Browse
/// fields do not alter that SQL path.
///
/// Returns errors for invalid SQL, unsupported statement shapes, missing sort
/// columns or ranges outside signed 64-bit integers. This validator is not a
/// sandbox for untrusted SQL; DuckDB remains the execution authority.
pub(super) fn build(query: &Query, schema: &Schema) -> Result<(String, Vec<Value>), QueryError> {
    if let Some(sql) = &query.sql {
        let statements = Parser::parse_sql(&DuckDbDialect {}, sql)
            .map_err(|error| QueryError::Invalid(error.to_string()))?;
        let [Statement::Query(select)] = statements.as_slice() else {
            return Err(QueryError::Invalid("Enter one SELECT or WITH query".into()));
        };
        // Execute exactly the validated query semantics, including its own LIMIT/OFFSET.
        return Ok((select.to_string(), vec![]));
    }

    let mut sql = "SELECT * FROM current_file".to_string();
    let mut params = Vec::new();
    if !query.search.is_empty() {
        let columns = schema
            .fields()
            .iter()
            .filter(|field| {
                matches!(
                    field.data_type(),
                    DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
                )
            })
            .map(|field| format!("contains(lower({}), lower(?))", identifier(field.name())))
            .collect::<Vec<_>>();
        if columns.is_empty() {
            sql.push_str(" WHERE false");
        } else {
            sql.push_str(" WHERE ");
            sql.push_str(&columns.join(" OR "));
            params.extend(columns.iter().map(|_| Value::Text(query.search.clone())));
        }
    }
    if let Some(sort) = query.sort {
        let field = schema
            .fields()
            .get(sort.column)
            .ok_or_else(|| QueryError::Invalid("Sort column is no longer available".into()))?;
        sql.push_str(&format!(
            " ORDER BY {} {} NULLS LAST, row_number() OVER ()",
            identifier(field.name()),
            if sort.descending { "DESC" } else { "ASC" }
        ));
        // Preserve source order among equal values across independent pages.
    }
    // Honor the requested range after filtering and global sorting.
    if let Some(limit) = query.limit {
        sql.push_str(" LIMIT ?");
        params.push(Value::BigInt(
            i64::try_from(limit).map_err(|_| QueryError::Invalid("Limit is too large".into()))?,
        ));
    }
    sql.push_str(" OFFSET ?");
    params
        .push(Value::BigInt(i64::try_from(query.offset).map_err(
            |_| QueryError::Invalid("Offset is too large".into()),
        )?));
    Ok((sql, params))
}
