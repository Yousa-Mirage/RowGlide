//! Engine-independent scalar predicates, Arrow capabilities and strict value validation.

pub(crate) mod evaluate;
mod parse;
pub use parse::parse_filter_value;

use arrow_schema::{DataType, Schema};

/// Scalar families supported by the first filter builder; dictionary values are unwrapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    /// UTF-8 text, including views and large strings.
    Text,
    /// Signed integer with its source bit width.
    Integer,
    /// Unsigned integer, including the full UInt64 domain.
    Unsigned,
    /// Floating-point values; filter literals must be finite.
    Float,
    /// Exact fixed-point decimal with source precision and scale.
    Decimal,
    /// Boolean truth values.
    Boolean,
    /// Calendar date without a time of day.
    Date,
    /// Timestamp whose source type determines unit and timezone requirements.
    Timestamp,
}

/// A comparison against a non-null, typed literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    /// Exact equality; text comparison is case-sensitive.
    Equals,
    /// Inequality; SQL NULL values still do not match.
    NotEquals,
    /// Strictly greater than the literal.
    GreaterThan,
    /// Greater than or equal to the literal.
    GreaterThanOrEqual,
    /// Strictly less than the literal.
    LessThan,
    /// Less than or equal to the literal.
    LessThanOrEqual,
    /// Case-insensitive literal substring, without wildcard interpretation.
    Contains,
    /// Case-insensitive literal prefix.
    StartsWith,
    /// Case-insensitive literal suffix.
    EndsWith,
}

/// Typed filter literal; decimal and temporal strings are strictly validated before execution.
#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    /// Raw text, including empty strings and intentional surrounding whitespace.
    Text(String),
    /// Signed integer, checked against the source width.
    Integer(i64),
    /// Unsigned integer, never converted through i64.
    Unsigned(u64),
    /// Finite floating-point literal.
    Float(f64),
    /// Selected boolean value.
    Boolean(bool),
    /// Exact canonical decimal text, without exponent syntax.
    Decimal(String),
    /// ISO calendar date.
    Date(String),
    /// ISO timestamp; zoned values normalize to an explicit UTC suffix.
    Timestamp(String),
}

impl FilterValue {
    /// Recover editable literal text without adding display quotes or changing precision.
    pub fn input(&self) -> String {
        match self {
            Self::Text(s) | Self::Decimal(s) | Self::Date(s) | Self::Timestamp(s) => s.clone(),
            Self::Integer(v) => v.to_string(),
            Self::Unsigned(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Boolean(v) => v.to_string(),
        }
    }
}

/// A top-level source-schema condition; the complete list is combined with AND.
#[derive(Clone, Debug, PartialEq)]
pub enum FilterCondition {
    /// Compare a column to a typed, non-null value.
    Compare {
        /// Index into the registered source schema, never a SQL result schema.
        column: usize,
        /// Operation supported by the decoded Arrow type.
        op: CompareOp,
        /// Literal retaining its numeric/text/temporal semantics.
        value: FilterValue,
    },
    /// Test nullness without a value field.
    IsNull {
        /// Index into the registered source schema.
        column: usize,
        /// True for IS NOT NULL, false for IS NULL.
        negated: bool,
    },
}

impl FilterCondition {
    /// Return the referenced top-level source column.
    pub fn column(&self) -> usize {
        match self {
            Self::Compare { column, .. } | Self::IsNull { column, .. } => *column,
        }
    }
}

/// Validation failures shared by the editor, real backend and mock evaluator.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    /// The source has been reloaded since the request was constructed.
    #[error("Source schema changed; rebuild the filters")]
    SchemaChanged,
    /// A referenced source column no longer exists.
    #[error("Filter column {0} is no longer available")]
    Column(usize),
    /// The Arrow type cannot be filtered without losing semantics.
    #[error("This column type is not supported by Filters; use SQL")]
    UnsupportedType,
    /// The selected operation is not meaningful for the column family.
    #[error("This operator is not supported for the selected column")]
    Operator,
    /// Literal syntax, range or precision is invalid for the source type.
    #[error("Expected {0}")]
    Value(&'static str),
    /// An externally constructed literal has the wrong value variant.
    #[error("Filter value type does not match the column")]
    ValueType,
    /// Nonempty Quick Search was requested for a source without text columns.
    #[error("This file has no text columns")]
    NoTextColumns,
    /// Preserve the failed row number and typed validation cause.
    #[error("Filter {index}: {source}")]
    Condition {
        /// One-based editor row, independent of the source column index.
        index: usize,
        /// Specific validation failure for this row.
        #[source]
        source: Box<FilterError>,
    },
}

/// Recursively unwrap dictionary encoding; dictionary is not a key/value Map.
pub fn filter_data_type(data_type: &DataType) -> &DataType {
    match data_type {
        DataType::Dictionary(_, value) => filter_data_type(value),
        other => other,
    }
}

/// Classify only types with an exact first-version DuckDB mapping.
pub fn filter_kind(data_type: &DataType) -> Option<FilterKind> {
    use DataType::*;
    Some(match filter_data_type(data_type) {
        Utf8 | LargeUtf8 | Utf8View => FilterKind::Text,
        Int8 | Int16 | Int32 | Int64 => FilterKind::Integer,
        UInt8 | UInt16 | UInt32 | UInt64 => FilterKind::Unsigned,
        Float32 | Float64 => FilterKind::Float,
        Decimal32(p, s) | Decimal64(p, s) | Decimal128(p, s) | Decimal256(p, s)
            if *p > 0 && *p <= 38 && *s >= 0 && (*s as u8) <= *p =>
        {
            FilterKind::Decimal
        }
        Boolean => FilterKind::Boolean,
        Date32 | Date64 => FilterKind::Date,
        Timestamp(_, _) => FilterKind::Timestamp,
        _ => return None,
    })
}

/// List valid non-null comparisons for a type; null tests are separate conditions.
pub fn filter_operators(data_type: &DataType) -> &'static [CompareOp] {
    use CompareOp::*;
    match filter_kind(data_type) {
        Some(FilterKind::Text) => &[Contains, StartsWith, EndsWith, Equals, NotEquals],
        Some(FilterKind::Boolean) => &[Equals, NotEquals],
        Some(_) => &[
            Equals,
            NotEquals,
            GreaterThan,
            GreaterThanOrEqual,
            LessThan,
            LessThanOrEqual,
        ],
        None => &[],
    }
}

/// Validate all conditions against source types and return canonical literals in input order.
/// Backends call this even for already-parsed UI requests; illegal variants cannot bypass parsing.
pub fn validate_filters(
    schema: &Schema,
    filters: &[FilterCondition],
) -> Result<Vec<FilterCondition>, FilterError> {
    filters
        .iter()
        .enumerate()
        .map(|(index, condition)| {
            validate_condition(schema, condition).map_err(|source| FilterError::Condition {
                index: index + 1,
                source: Box::new(source),
            })
        })
        .collect()
}

/// Check one condition and normalize its value without changing text whitespace.
fn validate_condition(
    schema: &Schema,
    condition: &FilterCondition,
) -> Result<FilterCondition, FilterError> {
    let column = condition.column();
    let data_type = schema
        .fields()
        .get(column)
        .ok_or(FilterError::Column(column))?
        .data_type();
    filter_kind(data_type).ok_or(FilterError::UnsupportedType)?;
    match condition {
        FilterCondition::IsNull { .. } => Ok(condition.clone()),
        FilterCondition::Compare { op, value, .. } => {
            if !filter_operators(data_type).contains(op) {
                return Err(FilterError::Operator);
            }
            let parsed = parse_filter_value(data_type, &value.input())?;
            if std::mem::discriminant(&parsed) != std::mem::discriminant(value) {
                return Err(FilterError::ValueType);
            }
            Ok(FilterCondition::Compare {
                column,
                op: *op,
                value: parsed,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_schema::{DataType, Field, Schema, TimeUnit};

    /// Validate operator families, exact parsing and null-condition separation.
    #[test]
    fn validates_typed_conditions_without_string_coercion() -> Result<(), FilterError> {
        let schema = Schema::new(vec![
            Field::new("text", DataType::Utf8, true),
            Field::new("amount", DataType::UInt64, true),
            Field::new(
                "when",
                DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                true,
            ),
            Field::new(
                "nested",
                DataType::List(std::sync::Arc::new(Field::new(
                    "item",
                    DataType::Utf8,
                    true,
                ))),
                true,
            ),
        ]);
        let conditions = validate_filters(
            &schema,
            &[
                FilterCondition::Compare {
                    column: 0,
                    op: CompareOp::Contains,
                    value: FilterValue::Text(" London ".into()),
                },
                FilterCondition::Compare {
                    column: 1,
                    op: CompareOp::GreaterThanOrEqual,
                    value: FilterValue::Unsigned(u64::MAX),
                },
                FilterCondition::IsNull {
                    column: 0,
                    negated: false,
                },
            ],
        )?;
        assert_eq!(
            conditions[0],
            FilterCondition::Compare {
                column: 0,
                op: CompareOp::Contains,
                value: FilterValue::Text(" London ".into())
            }
        );
        assert!(
            validate_filters(
                &schema,
                &[FilterCondition::Compare {
                    column: 3,
                    op: CompareOp::Equals,
                    value: FilterValue::Text("x".into())
                }]
            )
            .is_err()
        );
        assert!(
            validate_filters(
                &schema,
                &[FilterCondition::Compare {
                    column: 1,
                    op: CompareOp::Equals,
                    value: FilterValue::Integer(1)
                }]
            )
            .is_err()
        );
        Ok(())
    }

    /// Preserve UInt64, decimal precision, empty text and timestamp timezone semantics.
    #[test]
    fn parses_boundaries_without_float_or_timezone_guessing() {
        assert_eq!(
            parse_filter_value(&DataType::UInt64, "18446744073709551615"),
            Ok(FilterValue::Unsigned(u64::MAX))
        );
        assert_eq!(
            parse_filter_value(&DataType::Utf8, "  "),
            Ok(FilterValue::Text("  ".into()))
        );
        assert_eq!(
            parse_filter_value(&DataType::Decimal128(12, 2), "001.20"),
            Ok(FilterValue::Decimal("1.20".into()))
        );
        assert!(parse_filter_value(&DataType::Decimal128(4, 2), "1.234").is_err());
        assert!(
            parse_filter_value(
                &DataType::Timestamp(TimeUnit::Microsecond, None),
                "2026-01-01T00:00:00Z"
            )
            .is_err()
        );
        assert!(
            parse_filter_value(
                &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                "2026-01-01T00:00:00.123456000Z"
            )
            .is_ok()
        );
    }

    /// Reject unsupported operators and invalid columns without imposing a condition count cap.
    #[test]
    fn rejects_invalid_filter_shape() {
        let schema = Schema::new(vec![Field::new("text", DataType::Utf8, true)]);
        assert!(matches!(
            validate_filters(
                &schema,
                &[FilterCondition::Compare {
                    column: 0,
                    op: CompareOp::GreaterThan,
                    value: FilterValue::Text("x".into())
                }]
            ),
            Err(FilterError::Condition { .. })
        ));
        assert!(matches!(
            validate_filters(
                &schema,
                &[FilterCondition::IsNull {
                    column: 9,
                    negated: false
                }]
            ),
            Err(FilterError::Condition { .. })
        ));
        let conditions = (0..64)
            .map(|_| FilterCondition::IsNull {
                column: 0,
                negated: false,
            })
            .collect::<Vec<_>>();
        assert_eq!(validate_filters(&schema, &conditions), Ok(conditions));
    }
}
