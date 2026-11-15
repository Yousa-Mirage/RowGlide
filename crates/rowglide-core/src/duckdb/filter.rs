//! Parameterized Browse predicates; source types alone determine SQL casts.

use super::sql::identifier;
use crate::{
    CompareOp, FilterCondition, FilterError, FilterKind, FilterValue, Query, QueryError,
    filter_data_type, filter_kind, validate_filters,
};
use arrow_schema::{DataType, Schema, TimeUnit};
use duckdb::types::Value;

/// Append a parenthesized Quick Search and ordered AND conditions with matching parameters.
/// Validation precedes construction; no user value or arbitrary type name becomes SQL text.
pub(super) fn predicates(
    query: &Query,
    schema: &Schema,
) -> Result<(Vec<String>, Vec<Value>), QueryError> {
    let filters = validate_filters(schema, &query.filters)?;
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let search = query.search.trim();
    if !search.is_empty() {
        let columns = schema
            .fields()
            .iter()
            .filter(|field| filter_kind(field.data_type()) == Some(FilterKind::Text))
            .map(|field| format!("contains(lower({}), lower(?))", identifier(field.name())))
            .collect::<Vec<_>>();
        if columns.is_empty() {
            return Err(FilterError::NoTextColumns.into());
        }
        params.extend(columns.iter().map(|_| Value::Text(search.into())));
        clauses.push(format!("({})", columns.join(" OR ")));
    }
    for condition in filters {
        let field = &schema.fields()[condition.column()];
        let name = identifier(field.name());
        match condition {
            FilterCondition::IsNull { negated, .. } => clauses.push(format!(
                "{name} IS {}NULL",
                if negated { "NOT " } else { "" }
            )),
            FilterCondition::Compare { op, value, .. } => {
                let (bound, expression) = parameter(value, filter_data_type(field.data_type()))?;
                let sql = match op {
                    CompareOp::Contains => format!("contains(lower({name}), lower({expression}))"),
                    CompareOp::StartsWith => {
                        format!("starts_with(lower({name}), lower({expression}))")
                    }
                    CompareOp::EndsWith => format!("ends_with(lower({name}), lower({expression}))"),
                    _ => format!(
                        "{name} {} {expression}",
                        match op {
                            CompareOp::Equals => "=",
                            CompareOp::NotEquals => "!=",
                            CompareOp::GreaterThan => ">",
                            CompareOp::GreaterThanOrEqual => ">=",
                            CompareOp::LessThan => "<",
                            CompareOp::LessThanOrEqual => "<=",
                            _ => return Err(FilterError::Operator.into()),
                        }
                    ),
                };
                clauses.push(sql);
                params.push(bound);
            }
        }
    }
    Ok((clauses, params))
}

/// Bind a validated literal with exact decimal/temporal conversion determined by its source.
fn parameter(value: FilterValue, data_type: &DataType) -> Result<(Value, String), QueryError> {
    let mut expression = "?".to_owned();
    let value = match value {
        FilterValue::Text(value) => Value::Text(value),
        FilterValue::Integer(value) => Value::BigInt(value),
        FilterValue::Unsigned(value) => Value::UBigInt(value),
        FilterValue::Float(value) => Value::Double(value),
        FilterValue::Boolean(value) => Value::Boolean(value),
        FilterValue::Decimal(value) => {
            let (p, s) = match data_type {
                DataType::Decimal32(p, s)
                | DataType::Decimal64(p, s)
                | DataType::Decimal128(p, s)
                | DataType::Decimal256(p, s) => (*p, *s),
                _ => return Err(FilterError::ValueType.into()),
            };
            expression = format!("CAST(? AS DECIMAL({p},{s}))");
            Value::Text(value)
        }
        FilterValue::Date(value) => {
            expression = "CAST(? AS DATE)".into();
            Value::Text(value)
        }
        FilterValue::Timestamp(value) => {
            let DataType::Timestamp(unit, zone) = data_type else {
                return Err(FilterError::ValueType.into());
            };
            let target = if zone.is_some() {
                "TIMESTAMPTZ"
            } else {
                match unit {
                    TimeUnit::Second => "TIMESTAMP_S",
                    TimeUnit::Millisecond => "TIMESTAMP_MS",
                    TimeUnit::Microsecond => "TIMESTAMP",
                    TimeUnit::Nanosecond => "TIMESTAMP_NS",
                }
            };
            // DuckDB zoned timestamps have microsecond precision. Never discard finer literals.
            if zone.is_some()
                && chrono::DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| FilterError::Value("an ISO timestamp"))?
                    .timestamp_subsec_nanos()
                    % 1000
                    != 0
            {
                return Err(FilterError::Value(
                    "a zoned timestamp with at most microsecond precision",
                )
                .into());
            }
            expression = format!("CAST(? AS {target})");
            Value::Text(value)
        }
    };
    Ok((value, expression))
}
