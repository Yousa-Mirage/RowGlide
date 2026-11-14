//! One-time column measurement from headers and the first retained Arrow batch.

use crate::cells;
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema};
use gpui_kit::{Pixels, px};
use rowglide_core::{Cancellation, QueryError};
use std::collections::HashSet;

/// Build the exact name/type label shared by rendering and width measurement.
pub(crate) fn header(field: &Field) -> String {
    format!(
        "{} · {}",
        field.name(),
        cells::type_label(field.data_type())
    )
}

/// Whether Browse exposes a sort indicator for this Arrow field.
pub(crate) fn sortable(field: &Field) -> bool {
    !matches!(
        field.data_type(),
        DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Struct(_)
            | DataType::Map(_, _)
    )
}

/// Measure one batch's displayed summaries, including nulls and nested-value counts.
/// The caller supplies font measurement, allowing background shaping and pure tests.
/// Widths include cell padding and optional sort indicators, and stay within 64–480 px.
/// Repeated summaries are measured once per column; a capped column stops scanning.
/// No subsequent batch or full-value serialization participates in sizing. Cancellation
/// is checked between columns and cells and propagates without publishing partial widths.
pub(crate) fn measure_columns(
    schema: &Schema,
    batch: Option<&RecordBatch>,
    sorting: bool,
    cancel: &Cancellation,
    mut measure: impl FnMut(&str) -> Pixels,
) -> Result<Vec<Pixels>, QueryError> {
    schema
        .fields()
        .iter()
        .enumerate()
        .map(|(column, field)| {
            cancel.check()?;
            let sort_width = if sorting && sortable(field) {
                px(20.)
            } else {
                px(0.)
            };
            let mut width = measure(&header(field)) + px(24.) + sort_width;
            let mut seen = HashSet::new();
            if let Some(array) = batch.and_then(|batch| batch.columns().get(column)) {
                for row in 0..array.len() {
                    cancel.check()?;
                    if width >= px(480.) {
                        break;
                    }
                    let label = cells::summary(array, row);
                    if seen.insert(label.clone()) {
                        width = width.max(measure(&label) + px(24.));
                    }
                }
            }
            Ok(width.clamp(px(64.), px(480.)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{ArrayRef, StringArray};
    use std::{error::Error, sync::Arc};

    /// Headers, nulls and bounded long strings determine widths from the supplied batch only.
    #[test]
    fn first_batch_widths_include_headers_and_cap_long_values() -> Result<(), Box<dyn Error>> {
        let schema = Arc::new(Schema::new(vec![Field::new("name", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec![
                Some("a sufficiently wide value"),
                None,
            ])) as ArrayRef],
        )?;
        let measured = measure_columns(
            &schema,
            Some(&batch),
            true,
            &Cancellation::default(),
            |text| px(text.chars().count() as f32 * 10.),
        )?;
        assert_eq!(measured, vec![px(274.)]);
        let empty = measure_columns(&schema, None, true, &Cancellation::default(), |text| {
            px(text.chars().count() as f32 * 10.)
        })?;
        assert_eq!(
            empty,
            vec![px(
                header(schema.field(0)).chars().count() as f32 * 10. + 44.
            )]
        );
        let long = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec!["x".repeat(1000)]))],
        )?;
        assert_eq!(
            measure_columns(
                &schema,
                Some(&long),
                false,
                &Cancellation::default(),
                |text| px(text.len() as f32 * 10.)
            )?,
            vec![px(480.)]
        );
        Ok(())
    }

    /// Repeated values avoid redundant shaping; later rows are unnecessary after the cap.
    #[test]
    fn repeated_values_and_capped_columns_bound_measurement() -> Result<(), Box<dyn Error>> {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec![
                None,
                None,
                Some("repeat"),
                Some("repeat"),
            ]))],
        )?;
        let mut labels = Vec::new();
        measure_columns(
            &schema,
            Some(&batch),
            false,
            &Cancellation::default(),
            |text| {
                labels.push(text.to_owned());
                px(40.)
            },
        )?;
        assert_eq!(
            labels,
            vec![header(schema.field(0)), "null".into(), "repeat".into()]
        );
        let mut calls = 0;
        assert_eq!(
            measure_columns(
                &schema,
                Some(&batch),
                true,
                &Cancellation::default(),
                |_| {
                    calls += 1;
                    px(1000.)
                }
            )?,
            vec![px(480.)]
        );
        assert_eq!(calls, 1);
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            measure_columns(&schema, Some(&batch), false, &cancel, |_| px(40.)),
            Err(QueryError::Cancelled)
        ));
        Ok(())
    }
}
