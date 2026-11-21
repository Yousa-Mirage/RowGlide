//! Display helpers that preserve the underlying Arrow values.

use crate::i18n::{tr, tr_string};
use gpui_kit::SharedString;

use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float64Array, Int64Array, LargeListArray,
    LargeStringArray, ListArray, MapArray, StringArray, StringViewArray, StructArray,
};

/// Format one cell for the grid and inspector without changing its Arrow value.
///
/// `row` is an index within `array`; out-of-range rows yield an empty string and
/// nulls yield `null`. Text/fallback strings are limited to 240 Unicode characters,
/// Float64 values use two decimal places, and nested values show counts. Unsupported
/// scalar types use Arrow formatting, falling back to the type name on failure.
/// This is a display summary, not a lossless value export.
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
        return tr_string!("cell.items", count = a.value_length(row));
    }
    if let Some(a) = array.as_any().downcast_ref::<FixedSizeListArray>() {
        return tr_string!("cell.items", count = a.value_length());
    }
    if let Some(a) = array.as_any().downcast_ref::<MapArray>() {
        return tr_string!("cell.entries", count = a.value_length(row));
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
        return tr_string!("cell.items", count = a.value_length(row));
    }

    if let Some(a) = array.as_any().downcast_ref::<StructArray>() {
        return tr_string!("cell.fields", count = a.num_columns());
    }

    arrow_cast::display::array_value_to_string(array.as_ref(), row)
        .map(|text| text.chars().take(240).collect())
        .unwrap_or_else(|_| format!("{:?}", array.data_type()))
}

/// Expose the immediate typed children of one nested cell for the inspector.
///
/// Each tuple contains a display label, an owned shared array reference and a row
/// index into that array. Struct fields preserve the parent row index; list/map
/// children use indices in the selected value slice. Lists and map entries are
/// limited to 200 children. Null, invalid or unsupported scalar cells have none.
/// Map entries remain key/value structs; dictionary encoding is not treated as a map.
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

/// Map an Arrow data type to a short, user-facing column/inspector label.
///
/// Related numeric and text representations share labels. Unlisted types use
/// `Value`; the original Arrow schema still retains all type details.
pub(crate) fn type_label(data_type: &arrow_schema::DataType) -> SharedString {
    use arrow_schema::DataType;

    match data_type {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => tr!("types.text"),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            tr!("types.integer")
        }
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            tr!("types.unsigned_integer")
        }
        DataType::Float16 | DataType::Float32 | DataType::Float64 => tr!("types.float"),
        DataType::Decimal32(_, _)
        | DataType::Decimal64(_, _)
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => tr!("types.decimal"),
        DataType::Boolean => tr!("types.boolean"),
        DataType::List(_)
        | DataType::LargeList(_)
        | DataType::ListView(_)
        | DataType::LargeListView(_)
        | DataType::FixedSizeList(_, _) => {
            tr!("types.list")
        }
        DataType::Map(_, _) => tr!("types.map"),
        DataType::Date32 | DataType::Date64 => tr!("types.date"),
        DataType::Timestamp(_, _) => tr!("types.timestamp"),
        DataType::Struct(_) => tr!("types.struct"),
        _ => tr!("types.value"),
    }
}
