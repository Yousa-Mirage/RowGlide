//! Display helpers that preserve the underlying Arrow values.

use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float64Array, Int64Array, LargeListArray,
    LargeStringArray, ListArray, MapArray, StringArray, StringViewArray, StructArray,
};

/// Format a compact cell preview, truncating strings to 240 characters.
pub(crate) fn summary(array: &ArrayRef, row: usize) -> String {
    if row >= array.len() {
        return String::new();
    }

    if array.is_null(row) {
        return "null".into();
    }

    if let Some(a) = array.as_any().downcast_ref::<StringArray>() {
        return a.value(row).chars().take(240).collect();
    }

    if let Some(a) = array.as_any().downcast_ref::<LargeStringArray>() {
        return a.value(row).chars().take(240).collect();
    }
    if let Some(a) = array.as_any().downcast_ref::<StringViewArray>() {
        return a.value(row).chars().take(240).collect();
    }
    if let Some(a) = array.as_any().downcast_ref::<LargeListArray>() {
        return format!("[ ]  {} items", a.value_length(row));
    }
    if let Some(a) = array.as_any().downcast_ref::<FixedSizeListArray>() {
        return format!("[ ]  {} items", a.value_length());
    }
    if let Some(a) = array.as_any().downcast_ref::<MapArray>() {
        return format!("{{ }}  {} entries", a.value_length(row));
    }
    if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
        return a.value(row).to_string();
    }

    if let Some(a) = array.as_any().downcast_ref::<Float64Array>() {
        return format!("{:.2}", a.value(row));
    }

    if let Some(a) = array.as_any().downcast_ref::<BooleanArray>() {
        return a.value(row).to_string();
    }

    if let Some(a) = array.as_any().downcast_ref::<ListArray>() {
        return format!("[ ]  {} items", a.value_length(row));
    }

    if let Some(a) = array.as_any().downcast_ref::<StructArray>() {
        return format!("{{ }}  {} fields", a.num_columns());
    }

    arrow_cast::display::array_value_to_string(array.as_ref(), row)
        .map(|text| text.chars().take(240).collect())
        .unwrap_or_else(|_| format!("{:?}", array.data_type()))
}

/// Expose struct fields or up to 200 list items for the inspector.
pub(crate) fn children(array: &ArrayRef, row: usize) -> Vec<(String, ArrayRef, usize)> {
    if row >= array.len() || array.is_null(row) {
        return vec![];
    }

    if let Some(a) = array.as_any().downcast_ref::<StructArray>() {
        return a
            .fields()
            .iter()
            .zip(a.columns())
            .map(|(f, a)| (f.name().clone(), a.clone(), row))
            .collect();
    }

    if let Some(a) = array.as_any().downcast_ref::<ListArray>() {
        let values = a.value(row);
        return (0..values.len().min(200))
            .map(|i| (format!("[{i}]"), values.clone(), i))
            .collect();
    }

    let values = if let Some(a) = array.as_any().downcast_ref::<LargeListArray>() {
        Some(a.value(row))
    } else if let Some(a) = array.as_any().downcast_ref::<FixedSizeListArray>() {
        Some(a.value(row))
    } else {
        array
            .as_any()
            .downcast_ref::<MapArray>()
            .map(|a| std::sync::Arc::new(a.value(row)) as ArrayRef)
    };
    if let Some(values) = values {
        return (0..values.len().min(200))
            .map(|i| (format!("[{i}]"), values.clone(), i))
            .collect();
    }

    vec![]
}

pub(crate) fn type_label(data_type: &arrow_schema::DataType) -> &'static str {
    use arrow_schema::DataType;

    match data_type {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "Text",
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => "Integer",
        DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => "Decimal",
        DataType::Boolean => "Boolean",
        DataType::List(_) | DataType::LargeList(_) | DataType::FixedSizeList(_, _) => "List",
        DataType::Map(_, _) => "Map",
        DataType::Date32 | DataType::Date64 => "Date",
        DataType::Timestamp(_, _) => "Timestamp",
        DataType::Struct(_) => "Object",
        _ => "Value",
    }
}
