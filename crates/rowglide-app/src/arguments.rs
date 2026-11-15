//! Declarative startup arguments; file inspection remains in the UI background flow.

use argh::{EarlyExit, FromArgs};
use std::{ffi::OsString, path::PathBuf};

/// Open local data files in Rowglide.
#[derive(FromArgs)]
struct Arguments {
    /// files to open in separate tabs (use -- before a filename starting with -)
    #[argh(positional, arg_name = "FILE")]
    files: Vec<PathBuf>,
    /// print the application version and exit
    #[argh(switch, short = 'V')]
    version: bool,
}

/// Parse file paths from arguments that include the executable name, without I/O.
///
/// Preserves path order and spelling. Argh accepts UTF-8 arguments, so invalid native
/// encoding is rejected explicitly instead of panicking or replacing characters.
/// `--` ends option parsing. Help and version return successful early exits;
/// invalid options or encoding return failed early exits for the entry point to
/// print before initializing backend resources or opening a window.
pub(super) fn files(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Vec<PathBuf>, EarlyExit> {
    let strings = arguments
        .into_iter()
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| EarlyExit {
            output: "File arguments must be valid UTF-8; no path was opened.".into(),
            status: Err(()),
        })?;
    let values: Vec<_> = strings.iter().map(String::as_str).collect();
    let parsed = Arguments::from_args(&["rowglide"], &values)?;
    if parsed.version {
        return Err(EarlyExit {
            output: format!("rowglide {}", env!("CARGO_PKG_VERSION")),
            status: Ok(()),
        });
    }
    Ok(parsed.files)
}

#[cfg(test)]
mod tests {
    use super::files;
    use argh::EarlyExit;
    use std::{ffi::OsString, path::PathBuf};

    /// Empty launches and UTF-8 paths preserve order, spaces, Unicode and duplicates.
    #[test]
    fn paths_are_preserved_without_filesystem_access() -> Result<(), EarlyExit> {
        assert!(files([OsString::from("rowglide")])?.is_empty());
        let paths = [
            "数据 sample.parquet",
            "../another.pq",
            "数据 sample.parquet",
        ];
        let args = std::iter::once("rowglide").chain(paths).map(OsString::from);
        assert_eq!(files(args)?, paths.map(PathBuf::from));
        Ok(())
    }

    /// The option delimiter makes flag-shaped filenames unambiguous.
    #[test]
    fn delimiter_allows_hyphenated_paths() -> Result<(), EarlyExit> {
        let args = ["rowglide", "--", "--help", "--version", "-data.parquet"].map(OsString::from);
        assert_eq!(
            files(args)?,
            ["--help", "--version", "-data.parquet"].map(PathBuf::from)
        );
        Ok(())
    }

    /// Help and version terminate successfully; unknown options remain argument errors.
    #[test]
    fn informational_options_and_invalid_flags_are_distinct() {
        for (option, success, expected) in [
            ("--help", true, "Usage:"),
            ("--version", true, env!("CARGO_PKG_VERSION")),
            ("-V", true, env!("CARGO_PKG_VERSION")),
            ("--unknown", false, "--unknown"),
        ] {
            assert!(matches!(
                files(["rowglide", option].map(OsString::from)),
                Err(exit) if exit.status.is_ok() == success && exit.output.contains(expected)
            ));
        }
    }

    /// Invalid Unix filename bytes are rejected instead of silently changing the path.
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_bytes_are_rejected() {
        use std::os::unix::ffi::OsStringExt;

        let path = OsString::from_vec(b"data-\xff.parquet".to_vec());
        assert!(matches!(
            files([OsString::from("rowglide"), path]),
            Err(exit) if exit.status.is_err() && exit.output.contains("UTF-8")
        ));
    }

    /// Unpaired Windows surrogates are rejected instead of silently changing the path.
    #[cfg(windows)]
    #[test]
    fn invalid_windows_path_is_rejected() {
        use std::os::windows::ffi::OsStringExt;

        let path = OsString::from_wide(&[0x0043, 0x003a, 0x005c, 0xd800, 0x002e, 0x0070, 0x0071]);
        assert!(matches!(
            files([OsString::from("rowglide"), path]),
            Err(exit) if exit.status.is_err() && exit.output.contains("UTF-8")
        ));
    }
}
