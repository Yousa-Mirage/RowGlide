//! UI-only localization, system-language selection and presentation boundaries.
//!
//! Catalogs are embedded by rust-i18n at compile time. Initialize once before GPUI
//! constructs controls; no translation files or locale environment changes are needed.

use gpui_kit::SharedString;
use rowglide_core::QueryError;
use std::borrow::Cow;

/// Translate an embedded message for GPUI, retaining static catalog text without copying.
macro_rules! tr {
    ($($args:tt)*) => { $crate::i18n::shared(rust_i18n::t!($($args)*)) };
}
pub(crate) use tr;

/// Translate directly into owned text for errors, retained state and non-GPUI helpers.
macro_rules! tr_string {
    ($($args:tt)*) => { rust_i18n::t!($($args)*).into_owned() };
}
pub(crate) use tr_string;

/// Keep borrowed catalog text static and convert interpolated owned text for GPUI.
/// `SharedString::from(Cow)` copies in this GPUI version; `new_static` explicitly
/// preserves the borrowed pointer. Owned text uses GPUI's normal storage conversion.
pub(crate) fn shared(text: Cow<'static, str>) -> SharedString {
    match text {
        Cow::Borrowed(text) => SharedString::new_static(text),
        Cow::Owned(text) => SharedString::from(text),
    }
}

/// Select Chinese only when the system's first preferred language is Chinese.
/// Region/script variants and Unix encoding suffixes are accepted; missing or other
/// languages use English. Lower-priority system preferences are deliberately ignored.
fn supported_locale(preferred: Option<&str>) -> &'static str {
    let language = preferred
        .unwrap_or("")
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("");
    if language.eq_ignore_ascii_case("zh") {
        "zh-CN"
    } else {
        "en"
    }
}

/// Choose the desktop language before constructing controls and synchronize GPUI Kit.
/// sys-locale reads the native first preference on macOS/Windows and locale settings
/// on Unix. Chinese variants share the initial Simplified Chinese catalog.
pub(crate) fn initialize() {
    let locale = supported_locale(sys_locale::get_locale().as_deref());
    rust_i18n::set_locale(locale);
    gpui_kit::component::set_locale(locale);
}

/// Localize typed error context while preserving engine and application error details.
/// Core owns errors and remains locale-independent; opaque backend messages are kept
/// verbatim rather than translated by brittle string matching.
pub(crate) fn query_error(error: &QueryError) -> String {
    match error {
        QueryError::Filter(error) => filter_error(error),
        QueryError::SourceChanged => tr_string!("files.changed"),
        QueryError::Cancelled => tr_string!("errors.cancelled"),
        QueryError::Data(source) => tr_string!("errors.arrow", error = source),
        QueryError::Io(source) => tr_string!("errors.io", error = source),
        QueryError::Parquet(source) => tr_string!("errors.parquet", error = source),
        QueryError::DuckDb(source) => tr_string!("errors.duckdb", error = source),
        QueryError::Sql(source) => tr_string!("errors.sql", error = source),
        QueryError::CacheIo(source) => tr_string!("errors.cache", error = source),
        QueryError::CacheRead(source) => {
            tr_string!("errors.cache_read", error = query_error(source))
        }
        QueryError::Invalid(message) | QueryError::Backend(message) => message.clone(),
    }
}

/// Translate structural predicate errors; preserve exact parser expectations as diagnostics.
fn filter_error(error: &rowglide_core::FilterError) -> String {
    use rowglide_core::FilterError;
    match error {
        FilterError::SchemaChanged => tr_string!("filters.schema"),
        FilterError::Column(column) => tr_string!("filters.bad_column", column = column + 1),
        FilterError::UnsupportedType => tr_string!("filters.unsupported"),
        FilterError::Operator => tr_string!("filters.bad_operator"),
        FilterError::Value(detail) => tr_string!("filters.bad_value", detail = detail),
        FilterError::ValueType => tr_string!("filters.value_type"),
        FilterError::NoTextColumns => tr_string!("filters.no_text"),
        FilterError::Condition { index, source } => tr_string!(
            "filters.condition",
            index = index,
            error = filter_error(source)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::supported_locale;
    use serde_json::Value;
    use std::borrow::Cow;
    use std::collections::{BTreeMap, BTreeSet};

    /// Borrowed messages, including long catalog entries, retain their original storage.
    #[test]
    fn shared_catalog_text_preserves_borrowed_storage() -> Result<(), &'static str> {
        for locale in ["en", "zh-CN"] {
            let text = rust_i18n::t!("info.schema_hint", locale = locale);
            let Cow::Borrowed(original) = text else {
                return Err("Static catalog lookup unexpectedly allocated");
            };
            let translated = tr!("info.schema_hint", locale = locale);
            assert_eq!(translated.as_str(), original);
            assert_eq!(translated.as_ptr(), original.as_ptr());
            assert_eq!(translated.clone().as_ptr(), original.as_ptr());
        }
        Ok(())
    }

    /// Interpolated translations own their output after temporary inputs are released.
    /// The owned-string boundary preserves the same translated text without GPUI conversion.
    #[test]
    fn interpolated_text_survives_temporary_inputs() {
        let translated = {
            let name = String::from("temporary-name-long-enough-to-exceed-inline-storage.parquet");
            tr!("files.close", locale = "zh-CN", name = name)
        };
        assert_eq!(
            translated.as_str(),
            "关闭 temporary-name-long-enough-to-exceed-inline-storage.parquet"
        );
        assert_eq!(
            tr_string!("files.close", locale = "zh-CN", name = "中文.parquet"),
            "关闭 中文.parquet"
        );
        assert_eq!(tr!("common.ready", locale = "fr").as_str(), "Ready");
    }

    /// Flatten catalog leaves so translated keys can be compared without relying on order.
    fn messages(value: &Value, prefix: &str, leaves: &mut BTreeMap<String, String>) {
        match value {
            Value::Object(entries) => {
                for (key, value) in entries {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    messages(value, &path, leaves);
                }
            }
            Value::String(message) => {
                leaves.insert(prefix.to_string(), message.clone());
            }
            _ => {}
        }
    }

    /// Extract named interpolation parameters without interpreting user-provided data.
    fn parameters(message: &str) -> BTreeSet<&str> {
        message
            .split("%{")
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect()
    }

    /// Every embedded key has matching English/Chinese text and interpolation parameters.
    /// Explicit locale lookups also verify that rust-i18n actually loads both catalogs.
    #[test]
    fn catalogs_have_matching_keys_and_parameters() -> Result<(), serde_json::Error> {
        let mut english = BTreeMap::new();
        let mut chinese = BTreeMap::new();
        messages(
            &serde_json::from_str(include_str!("../locales/en.json"))?,
            "",
            &mut english,
        );
        messages(
            &serde_json::from_str(include_str!("../locales/zh-CN.json"))?,
            "",
            &mut chinese,
        );
        assert_eq!(
            english.keys().collect::<Vec<_>>(),
            chinese.keys().collect::<Vec<_>>()
        );
        for (key, text) in &english {
            let translated = &chinese[key];
            assert!(!text.trim().is_empty(), "Empty English message: {key}");
            assert!(
                !translated.trim().is_empty(),
                "Empty Chinese message: {key}"
            );
            assert_eq!(
                parameters(text),
                parameters(translated),
                "Parameters differ: {key}"
            );
            assert_eq!(rust_i18n::t!(key.as_str(), locale = "en"), text.as_str());
            assert_eq!(
                rust_i18n::t!(key.as_str(), locale = "zh-CN"),
                translated.as_str()
            );
        }
        Ok(())
    }

    /// Use only the first native preference and normalize Chinese region/script variants.
    #[test]
    fn system_language_selection() {
        for locale in [
            "zh",
            "zh-CN",
            "zh_TW",
            "zh-Hans",
            "zh-Hant-HK",
            "ZH_cn.UTF-8",
            " zh_CN@variant ",
        ] {
            assert_eq!(supported_locale(Some(locale)), "zh-CN");
        }
        for locale in [
            None,
            Some(""),
            Some("en-US"),
            Some("ja"),
            Some("zhish"),
            Some("C"),
            Some("POSIX"),
        ] {
            assert_eq!(supported_locale(locale), "en");
        }
    }

    /// Translate explicit locales without mutating process-global language state.
    /// Interpolated user data stays verbatim and an unsupported locale falls back to English.
    #[test]
    fn explicit_catalogs_and_fallback_preserve_values() {
        assert_eq!(rust_i18n::t!("common.ready", locale = "en"), "Ready");
        assert_eq!(rust_i18n::t!("common.ready", locale = "zh-CN"), "就绪");
        assert_eq!(rust_i18n::t!("common.ready", locale = "fr"), "Ready");
        assert_eq!(
            rust_i18n::t!("files.close", locale = "zh-CN", name = "中文.parquet"),
            "关闭 中文.parquet"
        );
        assert_eq!(
            rust_i18n::t!(
                "sql.failed",
                locale = "zh-CN",
                rows = 12,
                error = "raw diagnostic"
            ),
            "已加载 12 行 · 部分结果 · raw diagnostic"
        );
    }
}
