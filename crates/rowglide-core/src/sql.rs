//! SQL validation and parameterized browse queries.

use arrow_schema::{DataType, Schema};
use duckdb::types::Value;
use rowglide_app::{Query, QueryError};
use sqlparser::{ast::Statement, dialect::DuckDbDialect, parser::Parser};

pub(crate) fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn build(query: &Query, schema: &Schema) -> Result<(String, Vec<Value>), QueryError> {
    if let Some(sql) = &query.sql {
        let statements = Parser::parse_sql(&DuckDbDialect {}, sql)
            .map_err(|error| QueryError::Invalid(error.to_string()))?;
        let [Statement::Query(select)] = statements.as_slice() else {
            return Err(QueryError::Invalid("Enter one SELECT or WITH query".into()));
        };
        // Serialize the parsed AST so trailing comments and semicolons cannot
        // escape the result wrapper. DuckDB remains the execution authority.
        return Ok((
            format!("SELECT * FROM ({select}) AS result LIMIT 8193"),
            vec![],
        ));
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
            " ORDER BY {} {} NULLS LAST",
            identifier(field.name()),
            if sort.descending { "DESC" } else { "ASC" }
        ));
    }
    // Push the preview limit into DuckDB, after filtering and global sorting.
    sql.push_str(" LIMIT ? OFFSET ?");
    params.push(Value::BigInt(query.limit.unwrap_or(8193).min(8193) as i64));
    params
        .push(Value::BigInt(i64::try_from(query.offset).map_err(
            |_| QueryError::Invalid("Offset is too large".into()),
        )?));
    Ok((sql, params))
}
