//! Bounded, cancellable serialization of original Arrow cells for clipboard use.

use crate::Cancellation;
use arrow_array::{types::*, *};
use arrow_cast::display::array_value_to_string;
use arrow_schema::DataType;
use std::io::{self, Write};

/// Select raw scalar text or JSON; nested values always use JSON.
#[derive(Clone, Copy, Debug)]
pub enum CopyFormat {
    /// Strings are unquoted; other scalars retain their full textual value.
    Value,
    /// Strings, decimals, temporal values and nonfinite floats are JSON strings.
    Json,
}

/// Hard limits checked while writing; an error never returns partial output.
#[derive(Clone, Copy, Debug)]
pub struct CopyLimits {
    /// Maximum UTF-8 output length, including JSON escapes.
    pub max_bytes: usize,
    /// Maximum nested container/dictionary depth, with the root at zero.
    pub max_depth: usize,
}

impl Default for CopyLimits {
    /// Allow up to 16 MiB of text and 64 nested levels per explicit copy.
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Copy-specific failures, kept separate from query execution state.
#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    /// Cancellation preserves the shared query cancellation cause.
    #[error(transparent)]
    Query(#[from] crate::QueryError),
    /// Cancellation, output limit, or malformed input prevented a complete value.
    #[error("{0}")]
    Invalid(String),
    /// Arrow could not format the selected scalar.
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    /// JSON escaping failed, including a bounded sink refusing more output.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The output sink refused a write because of cancellation or its byte limit.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Output buffer that checks cancellation and capacity before every append.
struct Output<'a> {
    /// Bytes of the complete value under construction.
    bytes: Vec<u8>,
    /// User-independent safety bounds.
    limits: CopyLimits,
    /// Worker cancellation token borrowed for this serialization.
    cancel: &'a Cancellation,
}

impl Write for Output<'_> {
    /// Append all bytes or fail before growing beyond the configured budget.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.cancel.check().map_err(io::Error::other)?;
        if bytes.len() > self.limits.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("Cell exceeds clipboard size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    /// No external stream needs flushing.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize one original value without display truncation or numeric rounding.
///
/// Maps use arrays of key/value objects, preserving duplicate and non-string keys.
/// Binary uses hexadecimal with a `0x` prefix. JSON decimals and temporal values
/// are strings; finite numbers retain Arrow's exact textual representation.
/// Unsupported types, duplicate struct names, invalid indices and exceeded limits
/// return errors. Call on a worker for potentially large values.
pub fn serialize_cell(
    array: &dyn Array,
    row: usize,
    format: CopyFormat,
    limits: CopyLimits,
    cancel: &Cancellation,
) -> Result<String, CopyError> {
    let mut output = Output {
        bytes: Vec::new(),
        limits,
        cancel,
    };
    output.value(array, row, matches!(format, CopyFormat::Json), 0)?;
    String::from_utf8(output.bytes).map_err(|e| CopyError::Invalid(e.to_string()))
}

impl Output<'_> {
    /// Write a scalar as raw text or escaped JSON text.
    fn text(&mut self, value: &str, json: bool) -> Result<(), CopyError> {
        if json {
            serde_json::to_writer(self, value)?;
        } else {
            self.write_all(value.as_bytes())?;
        }
        Ok(())
    }

    /// Serialize one array element with explicit type handling and recursion limits.
    fn value(
        &mut self,
        array: &dyn Array,
        row: usize,
        json: bool,
        depth: usize,
    ) -> Result<(), CopyError> {
        self.cancel.check()?;
        if row >= array.len() || depth > self.limits.max_depth {
            return Err(CopyError::Invalid(
                "Invalid cell index or clipboard nesting limit exceeded".into(),
            ));
        }
        if array.is_null(row) {
            self.write_all(b"null")?;
            return Ok(());
        }
        macro_rules! down {
            ($ty:ty) => {
                array
                    .as_any()
                    .downcast_ref::<$ty>()
                    .ok_or_else(|| CopyError::Invalid("Arrow type mismatch".into()))?
            };
        }
        match array.data_type() {
            DataType::Utf8 => self.text(down!(StringArray).value(row), json)?,
            DataType::LargeUtf8 => self.text(down!(LargeStringArray).value(row), json)?,
            DataType::Utf8View => self.text(down!(StringViewArray).value(row), json)?,
            DataType::Binary => self.binary(down!(BinaryArray).value(row), json)?,
            DataType::LargeBinary => self.binary(down!(LargeBinaryArray).value(row), json)?,
            DataType::BinaryView => self.binary(down!(BinaryViewArray).value(row), json)?,
            DataType::FixedSizeBinary(_) => {
                self.binary(down!(FixedSizeBinaryArray).value(row), json)?
            }
            DataType::List(_) => self.list(down!(ListArray).value(row).as_ref(), depth)?,
            DataType::LargeList(_) => {
                self.list(down!(LargeListArray).value(row).as_ref(), depth)?
            }
            DataType::ListView(_) => self.list(down!(ListViewArray).value(row).as_ref(), depth)?,
            DataType::LargeListView(_) => {
                self.list(down!(LargeListViewArray).value(row).as_ref(), depth)?
            }
            DataType::FixedSizeList(_, _) => {
                self.list(down!(FixedSizeListArray).value(row).as_ref(), depth)?
            }
            DataType::Struct(fields) => {
                let value = down!(StructArray);
                let mut names = std::collections::HashSet::new();
                self.write_all(b"{")?;
                for (index, field) in fields.iter().enumerate() {
                    if !names.insert(field.name()) {
                        return Err(CopyError::Invalid(
                            "Duplicate struct field name cannot be represented as a JSON object"
                                .into(),
                        ));
                    }
                    if index > 0 {
                        self.write_all(b",")?;
                    }
                    self.text(field.name(), true)?;
                    self.write_all(b":")?;
                    self.value(value.column(index).as_ref(), row, true, depth + 1)?;
                }
                self.write_all(b"}")?;
            }
            DataType::Map(_, _) => {
                let entries = down!(MapArray).value(row);
                self.write_all(b"[")?;
                for index in 0..entries.len() {
                    if index > 0 {
                        self.write_all(b",")?;
                    }
                    self.write_all(b"{\"key\":")?;
                    self.value(entries.column(0).as_ref(), index, true, depth + 1)?;
                    self.write_all(b",\"value\":")?;
                    self.value(entries.column(1).as_ref(), index, true, depth + 1)?;
                    self.write_all(b"}")?;
                }
                self.write_all(b"]")?;
            }
            DataType::Dictionary(key, _) => {
                macro_rules! dictionary {
                    ($ty:ty) => {{
                        let dict = down!(DictionaryArray<$ty>);
                        let key = dict
                            .key(row)
                            .ok_or_else(|| CopyError::Invalid("Missing dictionary key".into()))?;
                        self.value(dict.values().as_ref(), key, json, depth + 1)?;
                    }};
                }
                match key.as_ref() {
                    DataType::Int8 => dictionary!(Int8Type),
                    DataType::Int16 => dictionary!(Int16Type),
                    DataType::Int32 => dictionary!(Int32Type),
                    DataType::Int64 => dictionary!(Int64Type),
                    DataType::UInt8 => dictionary!(UInt8Type),
                    DataType::UInt16 => dictionary!(UInt16Type),
                    DataType::UInt32 => dictionary!(UInt32Type),
                    DataType::UInt64 => dictionary!(UInt64Type),
                    _ => return Err(CopyError::Invalid("Unsupported dictionary key".into())),
                }
            }
            DataType::Union(_, _) | DataType::RunEndEncoded(_, _) => {
                return Err(CopyError::Invalid(format!(
                    "Unsupported clipboard type: {}",
                    array.data_type()
                )));
            }
            datatype => {
                let text = array_value_to_string(array, row)?;
                let number = matches!(
                    datatype,
                    DataType::Boolean
                        | DataType::Int8
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::UInt8
                        | DataType::UInt16
                        | DataType::UInt32
                        | DataType::UInt64
                        | DataType::Float16
                        | DataType::Float32
                        | DataType::Float64
                );
                let finite = !matches!(
                    text.as_str(),
                    "NaN" | "inf" | "-inf" | "Infinity" | "-Infinity"
                );
                self.text(&text, json && !(number && finite))?;
            }
        }
        Ok(())
    }

    /// Write all list elements in order without constructing an intermediate JSON tree.
    fn list(&mut self, values: &dyn Array, depth: usize) -> Result<(), CopyError> {
        self.write_all(b"[")?;
        for row in 0..values.len() {
            if row > 0 {
                self.write_all(b",")?;
            }
            self.value(values, row, true, depth + 1)?;
        }
        self.write_all(b"]")?;
        Ok(())
    }

    /// Encode binary incrementally so even a large blob cannot bypass the byte budget.
    fn binary(&mut self, bytes: &[u8], json: bool) -> Result<(), CopyError> {
        if json {
            self.write_all(b"\"")?;
        }
        self.write_all(b"0x")?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            self.write_all(&[HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])?;
        }
        if json {
            self.write_all(b"\"")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
