//! Startup SQL cache policy and validated environment overrides.

use rowglide_core::{CacheLimits, QueryError};
use std::{env, ffi::OsStr};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

/// Detect total RAM once and apply explicit cache overrides before opening the GUI.
/// Detection failure uses 512 MiB; malformed overrides fail startup with a named error.
/// Only RAM is refreshed, avoiding an unnecessary system-wide process scan.
pub(crate) fn cache_limits() -> Result<CacheLimits, QueryError> {
    let total = match System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    ) {
        Ok(system) => Some(system.total_memory()),
        Err(error) => {
            eprintln!("Cannot detect system RAM; using a 512 MiB result cache: {error}");
            None
        }
    };
    configured_limits(
        total,
        env::var_os("ROWGLIDE_MEMORY_LIMIT_MIB").as_deref(),
        env::var_os("ROWGLIDE_DISK_LIMIT_GIB").as_deref(),
    )
}

/// Combine automatic defaults with positive integer overrides, checking byte-size overflow.
/// Explicit memory overrides may exceed the automatic cap; they remain cache budgets, not RSS caps.
fn configured_limits(
    total: Option<u64>,
    memory: Option<&OsStr>,
    disk: Option<&OsStr>,
) -> Result<CacheLimits, QueryError> {
    let mut limits = CacheLimits::for_total_memory(total);
    if let Some(bytes) = parse_bytes("ROWGLIDE_MEMORY_LIMIT_MIB", memory, 1 << 20)? {
        limits.memory_bytes = usize::try_from(bytes).map_err(|_| {
            QueryError::Invalid(
                "ROWGLIDE_MEMORY_LIMIT_MIB exceeds this platform's address range".into(),
            )
        })?;
    }
    if let Some(bytes) = parse_bytes("ROWGLIDE_DISK_LIMIT_GIB", disk, 1 << 30)? {
        limits.disk_bytes = bytes;
    }
    Ok(limits)
}

/// Parse an optional positive integer in fixed binary units without silently using defaults.
/// Invalid encoding, non-integer text, zero and multiplication overflow are explicit errors.
fn parse_bytes(name: &str, value: Option<&OsStr>, unit: u64) -> Result<Option<u64>, QueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bytes = value
        .to_str()
        .and_then(|value| {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            value
                .parse::<u64>()
                .ok()?
                .checked_mul(unit)
                .filter(|bytes| *bytes > 0)
        })
        .ok_or_else(|| {
            QueryError::Invalid(format!(
                "{name} must be a positive integer within the supported byte range"
            ))
        })?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Automatic policy scales with RAM while explicit overrides replace only their own field.
    #[test]
    fn automatic_and_explicit_limits() -> Result<(), QueryError> {
        let automatic = configured_limits(Some(64 << 30), None, None)?;
        assert_eq!(automatic.memory_bytes as u64, 4 << 30);
        assert_eq!(automatic.disk_bytes, 32 << 30);
        let fallback = configured_limits(None, None, None)?;
        assert_eq!(fallback.memory_bytes, 512 << 20);
        let explicit = configured_limits(
            Some(8 << 30),
            Some(OsStr::new("8192")),
            Some(OsStr::new("64")),
        )?;
        assert_eq!(explicit.memory_bytes as u64, 8 << 30);
        assert_eq!(explicit.disk_bytes, 64 << 30);
        let disk_only = configured_limits(Some(8 << 30), None, Some(OsStr::new("8")))?;
        assert_eq!(disk_only.memory_bytes, 2 << 30);
        assert_eq!(disk_only.disk_bytes, 8 << 30);
        Ok(())
    }

    /// Malformed and overflowing settings fail by name rather than silently lowering budgets.
    #[test]
    fn invalid_overrides_fail_explicitly() {
        for value in [
            "",
            "0",
            "-1",
            "+1",
            "1.5",
            " 4",
            "4GiB",
            "18446744073709551615",
            "18446744073709551616",
        ] {
            let value = Some(OsStr::new(value));
            assert!(
                configured_limits(None, value, None)
                    .is_err_and(|error| error.to_string().contains("ROWGLIDE_MEMORY_LIMIT_MIB"))
            );
            assert!(
                configured_limits(None, None, value)
                    .is_err_and(|error| error.to_string().contains("ROWGLIDE_DISK_LIMIT_GIB"))
            );
        }
    }
}
