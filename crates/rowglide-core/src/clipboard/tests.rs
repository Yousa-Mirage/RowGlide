//! Clipboard serialization contracts independent of GUI and query execution.

use super::{CopyFormat, CopyLimits, serialize_cell};
use crate::Cancellation;
use arrow_array::{
    Array, ArrayRef, BinaryArray, Decimal128Array, Float64Array, Int64Array, ListArray,
    StringArray, StructArray, types::Int64Type,
};
use arrow_schema::{DataType, Field};
use std::sync::Arc;

/// Serialize using normal bounds for tests concerned with value fidelity.
fn copy(array: &dyn Array, row: usize, format: CopyFormat) -> Result<String, super::CopyError> {
    serialize_cell(
        array,
        row,
        format,
        CopyLimits::default(),
        &Cancellation::default(),
    )
}

/// Full strings, empty strings and null remain distinct, including escaped characters.
#[test]
fn strings_are_complete_and_json_is_escaped() -> Result<(), Box<dyn std::error::Error>> {
    let text = format!("\n\"{}\"\t", "你好".repeat(1000));
    let values = StringArray::from(vec![Some(text.as_str()), Some(""), None]);
    assert_eq!(copy(&values, 0, CopyFormat::Value)?, text);
    assert_eq!(
        serde_json::from_str::<String>(&copy(&values, 0, CopyFormat::Json)?)?,
        text
    );
    assert_eq!(copy(&values, 1, CopyFormat::Json)?, "\"\"");
    assert_eq!(copy(&values, 2, CopyFormat::Value)?, "null");
    Ok(())
}

/// Numeric copying never uses display rounding; decimals keep their exact scale.
#[test]
fn numbers_keep_precision() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        copy(&Int64Array::from(vec![i64::MAX]), 0, CopyFormat::Json)?,
        i64::MAX.to_string()
    );
    assert_eq!(
        copy(&Float64Array::from(vec![1.23456789]), 0, CopyFormat::Json)?,
        "1.23456789"
    );
    let decimal =
        Decimal128Array::from(vec![1234567890123456789]).with_precision_and_scale(20, 4)?;
    assert_eq!(
        copy(&decimal, 0, CopyFormat::Json)?,
        "\"123456789012345.6789\""
    );
    let nonfinite = copy(&Float64Array::from(vec![f64::NAN]), 0, CopyFormat::Json)?;
    assert_eq!(serde_json::from_str::<String>(&nonfinite)?, "NaN");
    Ok(())
}

/// Nested arrays and fields retain nulls, order and original names.
#[test]
fn nested_values_are_json() -> Result<(), Box<dyn std::error::Error>> {
    let lists = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![Some(vec![Some(7), None])]);
    let structure = StructArray::from(vec![(
        Arc::new(Field::new("items", lists.data_type().clone(), false)),
        Arc::new(lists) as ArrayRef,
    )]);
    assert_eq!(
        copy(&structure, 0, CopyFormat::Value)?,
        "{\"items\":[7,null]}"
    );
    let dictionary: arrow_array::DictionaryArray<arrow_array::types::Int8Type> =
        vec![Some("hello"), None].into_iter().collect();
    assert_eq!(copy(&dictionary, 0, CopyFormat::Json)?, "\"hello\"");
    assert_eq!(copy(&dictionary, 1, CopyFormat::Json)?, "null");
    assert_eq!(
        copy(
            &BinaryArray::from(vec![&[0u8, 255][..]]),
            0,
            CopyFormat::Json
        )?,
        "\"0x00ff\""
    );
    Ok(())
}

/// Bounds account for JSON escaping; cancellation and invalid indices return no text.
#[test]
fn limits_and_cancellation_reject_incomplete_output() {
    let values = StringArray::from(vec!["\n\n"]);
    let cancel = Cancellation::default();
    assert!(
        serialize_cell(
            &values,
            0,
            CopyFormat::Json,
            CopyLimits {
                max_bytes: 5,
                max_depth: 64
            },
            &cancel
        )
        .is_err()
    );
    assert!(copy(&values, 1, CopyFormat::Value).is_err());
    let lists = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![Some(vec![Some(7)])]);
    assert!(
        serialize_cell(
            &lists,
            0,
            CopyFormat::Json,
            CopyLimits {
                max_bytes: 100,
                max_depth: 0
            },
            &cancel
        )
        .is_err()
    );
    cancel.cancel();
    assert!(
        serialize_cell(
            &values,
            0,
            CopyFormat::Value,
            CopyLimits::default(),
            &cancel
        )
        .is_err()
    );
}

/// Duplicate field names cannot silently overwrite each other in a JSON consumer.
#[test]
fn duplicate_struct_names_are_rejected() {
    let values = Arc::new(Int64Array::from(vec![1])) as ArrayRef;
    let fields = vec![
        (
            Arc::new(Field::new("x", DataType::Int64, false)),
            values.clone(),
        ),
        (Arc::new(Field::new("x", DataType::Int64, false)), values),
    ];
    assert!(copy(&StructArray::from(fields), 0, CopyFormat::Json).is_err());
}

/// Map encoding preserves duplicate numeric keys instead of coercing keys to object names.
#[test]
fn maps_preserve_duplicate_non_string_keys() -> Result<(), Box<dyn std::error::Error>> {
    use arrow_array::builder::{Int64Builder, MapBuilder, StringBuilder};
    let mut builder = MapBuilder::new(None, Int64Builder::new(), StringBuilder::new());
    for value in ["a", "b"] {
        builder.keys().append_value(3);
        builder.values().append_value(value);
    }
    builder.append(true)?;
    assert_eq!(
        copy(&builder.finish(), 0, CopyFormat::Json)?,
        "[{\"key\":3,\"value\":\"a\"},{\"key\":3,\"value\":\"b\"}]"
    );
    Ok(())
}

/// Date values remain explicit strings in JSON, avoiding ambiguous epoch numbers.
#[test]
fn dates_are_quoted_iso_text() -> Result<(), super::CopyError> {
    assert_eq!(
        copy(
            &arrow_array::Date32Array::from(vec![0]),
            0,
            CopyFormat::Json
        )?,
        "\"1970-01-01\""
    );
    Ok(())
}
