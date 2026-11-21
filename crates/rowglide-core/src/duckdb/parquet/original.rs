//! Parquet footer declarations for the named fields shown in the schema table.

use parquet::{
    basic::{ConvertedType, LogicalType, Repetition, Type as PhysicalType},
    schema::types::{Type, TypePtr},
};

/// Skip list and map encoding wrappers, retaining genuine fields of element structs.
/// Legacy LIST encodings can use the repeated group itself as the struct element.
pub(super) fn named_fields(mut node: &Type) -> &[TypePtr] {
    while let Type::GroupType { fields, .. } = node {
        if is_map(node) && fields.len() == 1 {
            node = &fields[0];
            continue;
        }
        if is_list(node) && fields.len() == 1 {
            let repeated = &fields[0];
            let Type::GroupType { fields: items, .. } = repeated.as_ref() else {
                return &[];
            };
            let [item] = items.as_slice() else {
                return items;
            };
            let legacy_tuple = repeated.name() == "array"
                || repeated.name().strip_suffix("_tuple") == Some(node.name());
            if legacy_tuple && !is_list(repeated) && !has_single_repeated_child(repeated) {
                return items;
            }
            node = item;
            continue;
        }
        return fields;
    }
    &[]
}

/// Return whether a footer group carries either Parquet list annotation form.
fn is_list(node: &Type) -> bool {
    node.get_basic_info().logical_type_ref() == Some(&LogicalType::List)
        || node.get_basic_info().converted_type() == ConvertedType::LIST
}

/// Return whether a footer group carries either Parquet map annotation form.
fn is_map(node: &Type) -> bool {
    matches!(
        node.get_basic_info().logical_type_ref(),
        Some(LogicalType::Map)
    ) || matches!(
        node.get_basic_info().converted_type(),
        ConvertedType::MAP | ConvertedType::MAP_KEY_VALUE
    )
}

/// Return whether a group contains exactly one repeated child.
fn has_single_repeated_child(node: &Type) -> bool {
    let Type::GroupType { fields, .. } = node else {
        return false;
    };
    matches!(fields.as_slice(), [child] if child.get_basic_info().has_repetition()
        && child.get_basic_info().repetition() == Repetition::REPEATED)
}

/// Format the storage type, preserving fixed byte lengths and annotation parameters.
pub(super) fn label(node: &Type) -> String {
    let storage = match node {
        Type::GroupType { .. } => "GROUP".into(),
        Type::PrimitiveType {
            physical_type: PhysicalType::FIXED_LEN_BYTE_ARRAY,
            type_length,
            ..
        } => format!("FIXED_LEN_BYTE_ARRAY({type_length})"),
        Type::PrimitiveType { physical_type, .. } => physical_type.to_string(),
    };
    let info = node.get_basic_info();
    let annotation = match info.logical_type_ref() {
        Some(LogicalType::Decimal(d)) => format!("DECIMAL({},{})", d.precision, d.scale),
        Some(LogicalType::Integer(i)) => format!("INTEGER({},{})", i.bit_width, i.is_signed),
        Some(LogicalType::Time(t)) => format!("TIME({:?},{})", t.unit, t.is_adjusted_to_u_t_c),
        Some(LogicalType::Timestamp(t)) => {
            format!("TIMESTAMP({:?},{})", t.unit, t.is_adjusted_to_u_t_c)
        }
        Some(LogicalType::Geometry(g)) => g
            .crs
            .as_ref()
            .map_or_else(|| "GEOMETRY".into(), |crs| format!("GEOMETRY({crs})")),
        Some(LogicalType::Geography(g)) => {
            let algorithm = g.algorithm.unwrap_or_default();
            g.crs.as_ref().map_or_else(
                || format!("GEOGRAPHY({algorithm})"),
                |crs| format!("GEOGRAPHY({algorithm},{crs})"),
            )
        }
        Some(LogicalType::Variant(v)) => v
            .specification_version
            .map_or_else(|| "VARIANT".into(), |n| format!("VARIANT({n})")),
        Some(LogicalType::_Unknown { field_id }) => format!("UNKNOWN({field_id})"),
        Some(unit) => format!("{unit:?}").to_ascii_uppercase(),
        None => match info.converted_type() {
            ConvertedType::NONE => return storage,
            ConvertedType::DECIMAL if node.is_primitive() => {
                format!("DECIMAL({},{})", node.get_precision(), node.get_scale())
            }
            other => other.to_string(),
        },
    };
    format!("{storage} ({annotation})")
}
