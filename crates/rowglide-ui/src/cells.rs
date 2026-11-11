//! Display helpers that preserve the underlying Arrow values.
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, ListArray, StringArray, StructArray,
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
    format!("{:?}", array.data_type())
}

/// Expose struct fields or up to 200 list items for the inspector.
pub(crate) fn children(array: &ArrayRef, row: usize) -> Vec<(String, ArrayRef, usize)> {
    if array.is_null(row) {
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
    vec![]
}

pub(crate) fn type_label(data_type: &arrow_schema::DataType) -> &'static str {
    use arrow_schema::DataType;
    match data_type {
        DataType::Utf8 => "Text",
        DataType::Int64 => "Integer",
        DataType::Float64 => "Decimal",
        DataType::Boolean => "Boolean",
        DataType::List(_) => "List",
        DataType::Struct(_) => "Object",
        _ => "Value",
    }
}
