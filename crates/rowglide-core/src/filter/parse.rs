//! Strict literal parsing; no locale, timezone guessing, rounding or float-based decimals.

use super::{FilterError, FilterKind, FilterValue, filter_data_type, filter_kind};
use arrow_schema::{DataType, TimeUnit};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Timelike};

/// Parse an editor literal according to the decoded source type, validating its entire input.
/// Text preserves whitespace. Other families accept surrounding whitespace but no trailing junk.
pub fn parse_filter_value(data_type: &DataType, input: &str) -> Result<FilterValue, FilterError> {
    let data_type = filter_data_type(data_type);
    let kind = filter_kind(data_type).ok_or(FilterError::UnsupportedType)?;
    let trimmed = input.trim();
    Ok(match kind {
        FilterKind::Text => FilterValue::Text(input.into()),
        FilterKind::Integer => {
            let value = trimmed
                .parse::<i64>()
                .map_err(|_| FilterError::Value("a signed integer within the column range"))?;
            let valid = match data_type {
                DataType::Int8 => i8::try_from(value).is_ok(),
                DataType::Int16 => i16::try_from(value).is_ok(),
                DataType::Int32 => i32::try_from(value).is_ok(),
                _ => true,
            };
            if !valid {
                return Err(FilterError::Value(
                    "a signed integer within the column range",
                ));
            }
            FilterValue::Integer(value)
        }
        FilterKind::Unsigned => {
            let value = trimmed
                .parse::<u64>()
                .map_err(|_| FilterError::Value("an unsigned integer within the column range"))?;
            let valid = match data_type {
                DataType::UInt8 => u8::try_from(value).is_ok(),
                DataType::UInt16 => u16::try_from(value).is_ok(),
                DataType::UInt32 => u32::try_from(value).is_ok(),
                _ => true,
            };
            if !valid {
                return Err(FilterError::Value(
                    "an unsigned integer within the column range",
                ));
            }
            FilterValue::Unsigned(value)
        }
        FilterKind::Float => {
            let value = trimmed
                .parse::<f64>()
                .map_err(|_| FilterError::Value("a finite number"))?;
            if !value.is_finite()
                || matches!(data_type, DataType::Float32) && !(value as f32).is_finite()
            {
                return Err(FilterError::Value(
                    "a finite number within the column range",
                ));
            }
            FilterValue::Float(value)
        }
        FilterKind::Boolean => FilterValue::Boolean(match trimmed {
            "true" => true,
            "false" => false,
            _ => return Err(FilterError::Value("true or false")),
        }),
        FilterKind::Decimal => {
            let (precision, scale) = match data_type {
                DataType::Decimal32(p, s)
                | DataType::Decimal64(p, s)
                | DataType::Decimal128(p, s)
                | DataType::Decimal256(p, s) => (*p, *s),
                _ => return Err(FilterError::UnsupportedType),
            };
            FilterValue::Decimal(decimal(trimmed, precision, scale)?)
        }
        FilterKind::Date => {
            if !date_shape(trimmed) {
                return Err(FilterError::Value("an ISO date: YYYY-MM-DD"));
            }
            NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
                .map_err(|_| FilterError::Value("a valid calendar date"))?;
            FilterValue::Date(trimmed.into())
        }
        FilterKind::Timestamp => FilterValue::Timestamp(timestamp(trimmed, data_type)?),
    })
}

/// Validate fixed-width ASCII calendar syntax before asking chrono to check the date.
fn date_shape(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// Normalize decimal magnitude at the exact source scale, rejecting any nonzero excess digits.
fn decimal(input: &str, precision: u8, scale: i8) -> Result<String, FilterError> {
    let error = || FilterError::Value("an exact decimal within the column precision and scale");
    let negative = input.starts_with('-');
    let unsigned = input.strip_prefix(['-', '+']).unwrap_or(input);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(error());
    }
    let scale = usize::try_from(scale).map_err(|_| error())?;
    if fraction
        .get(scale..)
        .is_some_and(|tail| tail.bytes().any(|b| b != b'0'))
    {
        return Err(error());
    }
    let whole = whole.trim_start_matches('0');
    let mut fraction = fraction.chars().take(scale).collect::<String>();
    fraction.extend(std::iter::repeat_n(
        '0',
        scale.saturating_sub(fraction.len()),
    ));
    let digits = format!("{whole}{fraction}");
    if digits.trim_start_matches('0').len() > usize::from(precision)
        || whole.len() > usize::from(precision).saturating_sub(scale)
    {
        return Err(error());
    }
    let sign = if negative && digits.bytes().any(|b| b != b'0') {
        "-"
    } else {
        ""
    };
    let whole = if whole.is_empty() { "0" } else { whole };
    Ok(if scale == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    })
}

/// Validate a full ISO timestamp and normalize aware values to UTC without precision loss.
fn timestamp(input: &str, data_type: &DataType) -> Result<String, FilterError> {
    let DataType::Timestamp(unit, zone) = data_type else {
        return Err(FilterError::UnsupportedType);
    };
    let error =
        || FilterError::Value("an exact ISO timestamp with the column's timezone and precision");
    if input.len() < 19
        || !input.is_ascii()
        || !date_shape(&input[..10])
        || &input[10..11] != "T"
        || &input[13..14] != ":"
        || &input[16..17] != ":"
        || ![&input[11..13], &input[14..16], &input[17..19]]
            .iter()
            .all(|s| s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(error());
    }
    let rest = &input[19..];
    let rest = if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return Err(error());
        }
        &fraction[digits..]
    } else {
        rest
    };
    if zone.is_none() && !rest.is_empty()
        || zone.is_some()
            && !(rest == "Z"
                || rest.len() == 6
                    && (rest.starts_with('+') || rest.starts_with('-'))
                    && &rest[3..4] == ":")
    {
        return Err(error());
    }
    let utc = if zone.is_some() {
        DateTime::parse_from_rfc3339(input)
            .map_err(|_| error())?
            .to_utc()
    } else {
        NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f")
            .map_err(|_| error())?
            .and_utc()
    };
    let divisor = match unit {
        TimeUnit::Second => 1_000_000_000,
        TimeUnit::Millisecond => 1_000_000,
        TimeUnit::Microsecond => 1000,
        TimeUnit::Nanosecond => 1,
    };
    if utc.nanosecond() >= 1_000_000_000 || utc.nanosecond() % divisor != 0 {
        return Err(error());
    }
    let ticks = i128::from(utc.timestamp()) * (1_000_000_000 / i128::from(divisor))
        + i128::from(utc.nanosecond() / divisor);
    i64::try_from(ticks).map_err(|_| error())?;
    Ok(if zone.is_some() {
        utc.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    } else {
        utc.naive_utc().format("%Y-%m-%dT%H:%M:%S%.f").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All parser boundaries reject overflow, junk, nonfinite values and calendar mistakes.
    #[test]
    fn rejects_invalid_literals() {
        for (kind, text) in [
            (DataType::Int8, "128"),
            (DataType::UInt8, "256"),
            (DataType::UInt64, "-1"),
            (DataType::Int64, "1.0"),
            (DataType::Float64, "NaN"),
            (DataType::Float32, "1e40"),
            (DataType::Boolean, "yes"),
            (DataType::Date32, "2026-02-29"),
            (DataType::Date32, "2026-1-01"),
            (DataType::Decimal128(4, 2), "100.00"),
            (DataType::Decimal128(4, 2), "1e1"),
            (
                DataType::Timestamp(TimeUnit::Second, None),
                "2026-01-01T00:00:00.1",
            ),
            (
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                "2026-01-01T00:00:00",
            ),
            (
                DataType::Timestamp(TimeUnit::Nanosecond, None),
                "9999-01-01T00:00:00",
            ),
        ] {
            assert!(parse_filter_value(&kind, text).is_err(), "{kind:?}: {text}");
        }
        assert_eq!(
            parse_filter_value(
                &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                "2026-01-01T01:30:00+01:30"
            ),
            Ok(FilterValue::Timestamp("2026-01-01T00:00:00Z".into()))
        );
    }
}
