//! Deterministic, typed demo data. No files are read and no SQL engine is embedded.
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, StructArray,
    builder::{ListBuilder, StringBuilder},
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use rowglide_app::{Backend, BatchCursor, Cancellation, Query, QueryError, Source};
use std::sync::Arc;

pub const PREVIEW_SQL: &str = "SELECT * FROM current_file LIMIT 100;";
pub const EMPTY_SQL: &str = "SELECT * FROM current_file WHERE false;";
const NAMES: [&str; 8] = [
    "Olivia Chen",
    "Noah Williams",
    "Emma Zhang",
    "Liam Patel",
    "林晓雨",
    "Amelia Garcia",
    "Oliver Kim",
    "Sophia Martin",
];
const REGIONS: [&str; 4] = ["Shanghai", "London", "Tokyo", "San Francisco"];

#[derive(Default)]
pub struct MockBackend;
impl Backend for MockBackend {
    fn sources(&self) -> Vec<Source> {
        vec![
            Source {
                id: 0,
                name: "customers.parquet".into(),
                format: "PARQUET",
                rows: 50000,
                description: "Customer profiles with nested attributes",
            },
            Source {
                id: 1,
                name: "orders.csv".into(),
                format: "CSV",
                rows: 2400,
                description: "Order activity · scalar columns only",
            },
            Source {
                id: 2,
                name: "empty.parquet".into(),
                format: "PARQUET",
                rows: 0,
                description: "An empty dataset with a complete schema",
            },
        ]
    }
    fn query(
        &self,
        query: Query,
        cancellation: Cancellation,
    ) -> Result<Box<dyn BatchCursor>, QueryError> {
        cancellation.check()?;
        let source = self
            .sources()
            .into_iter()
            .find(|s| s.id == query.source)
            .ok_or_else(|| QueryError::Invalid("Unknown demo dataset".into()))?;
        let nested = source.id != 1;
        let mut ids = Vec::new();
        let needle = query.search.to_lowercase();
        for id in 0..source.rows {
            if id % 256 == 0 {
                cancellation.check()?;
            }
            if needle.is_empty()
                || name(id).to_lowercase().contains(&needle)
                || region(id).to_lowercase().contains(&needle)
            {
                ids.push(id);
            }
        }
        if let Some(sort) = query.sort {
            if sort.column >= 5 {
                return Err(QueryError::Invalid(
                    "Nested columns cannot be sorted in the demo".into(),
                ));
            }
            ids.sort_by(|a, b| {
                let ordering = match sort.column {
                    0 => a.cmp(b),
                    1 => name(*a).cmp(name(*b)),
                    2 => region(*a).cmp(region(*b)),
                    3 => amount(*a)
                        .partial_cmp(&amount(*b))
                        .unwrap_or(std::cmp::Ordering::Equal),
                    _ => active(*a).cmp(&active(*b)),
                };
                // Nulls stay last in either direction.
                if sort.column == 3 && (amount(*a).is_none() || amount(*b).is_none()) {
                    amount(*a)
                        .is_none()
                        .cmp(&amount(*b).is_none())
                        .then(a.cmp(b))
                } else {
                    (if sort.descending {
                        ordering.reverse()
                    } else {
                        ordering
                    })
                    .then(a.cmp(b))
                }
            });
        }
        if let Some(sql) = query.sql {
            match sql.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().trim_end_matches(';') {
                "select * from current_file limit 100" => ids.truncate(100),
                "select * from current_file where false" => ids.clear(),
                _ => return Err(QueryError::Invalid("Demo SQL only. Try SELECT * FROM current_file LIMIT 100; or SELECT * FROM current_file WHERE false;".into())),
            }
        }
        cancellation.check()?;
        Ok(Box::new(MockCursor {
            schema: schema(nested),
            ids,
            offset: 0,
            nested,
            cancellation,
        }))
    }
}
fn name(id: usize) -> &'static str {
    NAMES[id % NAMES.len()]
}
fn region(id: usize) -> &'static str {
    REGIONS[(id / 3) % REGIONS.len()]
}
fn amount(id: usize) -> Option<f64> {
    (!id.is_multiple_of(17)).then_some(((id * 7919) % 90000) as f64 / 100.)
}
fn active(id: usize) -> bool {
    !id.is_multiple_of(3)
}
fn schema(nested: bool) -> SchemaRef {
    let mut fields = vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("city", DataType::Utf8, false),
        Field::new("amount", DataType::Float64, true),
        Field::new("active", DataType::Boolean, false),
    ];
    if nested {
        fields.push(Field::new(
            "tags",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ));
        fields.push(Field::new(
            "profile",
            DataType::Struct(
                vec![
                    Field::new("plan", DataType::Utf8, false),
                    Field::new("visits", DataType::Int64, false),
                    Field::new("verified", DataType::Boolean, false),
                ]
                .into(),
            ),
            false,
        ));
    }
    Arc::new(Schema::new(fields))
}
struct MockCursor {
    schema: SchemaRef,
    ids: Vec<usize>,
    offset: usize,
    nested: bool,
    cancellation: Cancellation,
}
impl BatchCursor for MockCursor {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    fn total_rows(&self) -> Option<usize> {
        Some(self.ids.len())
    }
    fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
        self.cancellation.check()?;
        if self.offset == self.ids.len() {
            return Ok(None);
        }
        let end = (self.offset + 256).min(self.ids.len());
        let ids = &self.ids[self.offset..end];
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from_iter_values(
                ids.iter().map(|id| *id as i64 + 10001),
            )),
            Arc::new(StringArray::from_iter_values(
                ids.iter().map(|id| name(*id)),
            )),
            Arc::new(StringArray::from_iter_values(
                ids.iter().map(|id| region(*id)),
            )),
            Arc::new(Float64Array::from_iter(ids.iter().map(|id| amount(*id)))),
            Arc::new(BooleanArray::from_iter(
                ids.iter().map(|id| Some(active(*id))),
            )),
        ];
        if self.nested {
            let mut tags = ListBuilder::new(StringBuilder::new());
            for id in ids {
                if id.is_multiple_of(13) {
                    tags.append(false);
                } else {
                    if !id.is_multiple_of(11) {
                        tags.values().append_value("customer");
                        tags.values()
                            .append_value(if active(*id) { "engaged" } else { "new" });
                    }
                    tags.append(true);
                }
            }
            arrays.push(Arc::new(tags.finish()));
            let profile = StructArray::from(vec![
                (
                    Arc::new(Field::new("plan", DataType::Utf8, false)),
                    Arc::new(StringArray::from_iter_values(
                        ids.iter()
                            .map(|id| if id % 2 == 0 { "Pro" } else { "Starter" }),
                    )) as ArrayRef,
                ),
                (
                    Arc::new(Field::new("visits", DataType::Int64, false)),
                    Arc::new(Int64Array::from_iter_values(
                        ids.iter().map(|id| (id % 70) as i64),
                    )) as ArrayRef,
                ),
                (
                    Arc::new(Field::new("verified", DataType::Boolean, false)),
                    Arc::new(BooleanArray::from_iter(
                        ids.iter().map(|id| Some(active(*id))),
                    )) as ArrayRef,
                ),
            ]);
            arrays.push(Arc::new(profile));
        }
        self.offset = end;
        Ok(Some(RecordBatch::try_new(self.schema.clone(), arrays)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rowglide_app::{Preview, Sort};
    #[test]
    fn sorting_precedes_preview_limit_and_nulls_stay_last() -> Result<(), Box<dyn std::error::Error>>
    {
        let cancel = Cancellation::default();
        let query = Query {
            sort: Some(Sort {
                column: 0,
                descending: true,
            }),
            ..Default::default()
        };
        let preview = Preview::collect(MockBackend.query(query, cancel.clone())?, &cancel)?;
        assert_eq!(preview.rows, 8192);
        assert_eq!(preview.total_rows, Some(50000));
        assert!(preview.truncated);
        let ids = preview.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or("id column must be Int64")?;
        assert_eq!(ids.value(0), 60000);
        for descending in [false, true] {
            let query = Query {
                sort: Some(Sort {
                    column: 3,
                    descending,
                }),
                ..Default::default()
            };
            let mut cursor = MockBackend.query(query, cancel.clone())?;
            let mut seen_null = false;
            while let Some(batch) = cursor.next_batch()? {
                use arrow_array::Array;
                let values = batch
                    .column(3)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .ok_or("amount column must be Float64")?;
                for i in 0..values.len() {
                    if values.is_null(i) {
                        seen_null = true;
                    } else {
                        assert!(!seen_null);
                    }
                }
            }
        }
        Ok(())
    }
    #[test]
    fn cancellation_empty_schema_and_sql_errors() -> Result<(), Box<dyn std::error::Error>> {
        let cancel = Cancellation::default();
        let mut cursor = MockBackend.query(Query::default(), cancel.clone())?;
        cancel.cancel();
        assert!(matches!(cursor.next_batch(), Err(QueryError::Cancelled)));
        let mut empty = MockBackend.query(
            Query {
                source: 2,
                ..Default::default()
            },
            Cancellation::default(),
        )?;
        assert_eq!(empty.schema().fields().len(), 7);
        assert!(empty.next_batch()?.is_none());
        assert!(
            MockBackend
                .query(
                    Query {
                        sql: Some("DELETE FROM current_file".into()),
                        ..Default::default()
                    },
                    Cancellation::default()
                )
                .is_err()
        );
        Ok(())
    }
}
