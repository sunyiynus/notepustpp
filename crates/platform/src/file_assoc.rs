//! Per-user Windows file-association registration.
//!
//! All writes stay below `HKCU\Software\Classes`, so registration never needs
//! administrator rights and never replaces the user's chosen default handler.
//! Each extension receives an `OpenWithProgids` entry, which adds Notepust to
//! the Explorer "Open with" list and to the Windows 10/11 default-app picker.
//! Selecting a default application remains the user's decision.

/// ProgID registered for Notepust documents.
pub const PROG_ID: &str = "Notepust.Document";

/// Executable name registered below `Applications`.
const APPLICATION_EXE: &str = "notepust.exe";

/// Friendly type name shown by Windows for [`PROG_ID`].
#[cfg(windows)]
const FRIENDLY_TYPE_NAME: &str = "Notepust Document";

/// Friendly application name shown in the Explorer "Open with" list.
#[cfg(windows)]
const FRIENDLY_APP_NAME: &str = "Notepust";

/// Extensions offered through `OpenWithProgids`.
///
/// Keep this list in sync with `installer/notepust.nsi`, which mirrors it for
/// the optional Explorer context-menu entry.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    ".txt",
    ".log",
    ".md",
    ".markdown",
    ".json",
    ".jsonc",
    ".toml",
    ".yaml",
    ".yml",
    ".ini",
    ".xml",
    ".html",
    ".htm",
    ".css",
    ".js",
    ".mjs",
    ".cjs",
    ".ts",
    ".tsx",
    ".jsx",
    ".py",
    ".rs",
    ".c",
    ".h",
    ".cpp",
    ".hpp",
    ".sql",
    ".sh",
    ".bash",
];

/// Result of a successful [`register_file_associations`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssociationReport {
    /// ProgID attached to every supported extension.
    pub prog_id: &'static str,
    /// Number of extensions that now list [`PROG_ID`] in `OpenWithProgids`.
    pub extensions: usize,
}

/// Error from file-association registration or removal.
#[derive(Debug, thiserror::Error)]
pub enum FileAssocError {
    /// The current platform has no Windows registry.
    #[error("file association registration is only supported on Windows")]
    Unsupported,

    /// The path of the running executable could not be resolved.
    #[error("could not resolve the running executable: {0}")]
    CurrentExecutable(#[source] std::io::Error),

    /// A registry call returned a non-success Win32 error code.
    #[cfg(windows)]
    #[error("{operation} failed with Win32 error {code}")]
    Registry {
        /// API entry point that failed.
        operation: &'static str,
        /// Win32 error code returned by the API.
        code: u32,
    },
}

/// Registers Notepust in the current user's `Open with` lists.
///
/// The ProgID and the `Applications` entry point at the running executable.
/// Every supported extension gets `Notepust.Document` in its
/// `OpenWithProgids` key; default handlers and `UserChoice` are never touched.
pub fn register_file_associations() -> Result<AssociationReport, FileAssocError> {
    #[cfg(windows)]
    {
        win::register()
    }
    #[cfg(not(windows))]
    {
        Err(FileAssocError::Unsupported)
    }
}

/// Removes exactly the values and keys written by [`register_file_associations`].
///
/// Unrelated `OpenWithProgids` values and keys owned by other applications are
/// left untouched, and the extension keys themselves are never deleted.
pub fn unregister_file_associations() -> Result<(), FileAssocError> {
    #[cfg(windows)]
    {
        win::unregister()
    }
    #[cfg(not(windows))]
    {
        Err(FileAssocError::Unsupported)
    }
}

/// Command line used for `shell\open\command` and by packaging scripts.
///
/// The registry stores the exact native executable path. This string is a
/// display/documentation helper and falls back to `notepust.exe` when the
/// running path is unavailable.
pub fn association_command_line() -> String {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| APPLICATION_EXE.to_string());
    format!("\"{executable}\" \"%1\"")
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_NONE, REG_OPTION_NON_VOLATILE, REG_SZ,
        REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW,
        RegOpenKeyExW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    use super::{
        APPLICATION_EXE, AssociationReport, FRIENDLY_APP_NAME, FRIENDLY_TYPE_NAME, FileAssocError,
        PROG_ID, SUPPORTED_EXTENSIONS,
    };

    /// Owns one open registry handle and closes it on every return path.
    struct Key(HKEY);

    const CLASSES_ROOT: &str = r"Software\Classes";

    fn classes_key(subkey: &str) -> String {
        format!(r"{CLASSES_ROOT}\{subkey}")
    }

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    impl Key {
        /// Creates or opens `subkey` below `HKCU\Software\Classes`.
        fn create(subkey: &str) -> Result<Self, FileAssocError> {
            let subkey = wide(&classes_key(subkey));
            let mut key = HKEY::default();
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(subkey.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    None,
                    &mut key,
                    None,
                )
            };
            check("RegCreateKeyExW", status)?;
            Ok(Self(key))
        }

        /// Opens an existing key; a missing key is reported as `Ok(None)`.
        fn open(subkey: &str) -> Result<Option<Self>, FileAssocError> {
            let subkey = wide(&classes_key(subkey));
            let mut key = HKEY::default();
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(subkey.as_ptr()),
                    None,
                    KEY_WRITE,
                    &mut key,
                )
            };
            match status {
                ERROR_SUCCESS => Ok(Some(Self(key))),
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
                other => Err(registry_error("RegOpenKeyExW", other)),
            }
        }

        fn set(
            &self,
            name: Option<&str>,
            kind: REG_VALUE_TYPE,
            data: &[u8],
        ) -> Result<(), FileAssocError> {
            let name = name.map(wide);
            let name = name
                .as_deref()
                .map_or(PCWSTR::null(), |units| PCWSTR(units.as_ptr()));
            let data = (!data.is_empty()).then_some(data);
            let status = unsafe { RegSetValueExW(self.0, name, None, kind, data) };
            check("RegSetValueExW", status)
        }

        fn set_string(&self, name: Option<&str>, text: &str) -> Result<(), FileAssocError> {
            let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            self.set(name, REG_SZ, &utf16_bytes(&units))
        }

        /// Writes a quoted executable path surrounded by `prefix` and `suffix`
        /// without losing native UTF-16 path units to string conversion.
        fn set_path_string(
            &self,
            name: Option<&str>,
            prefix: &str,
            path: &Path,
            suffix: &str,
        ) -> Result<(), FileAssocError> {
            let mut units: Vec<u16> = prefix.encode_utf16().collect();
            units.extend(path.as_os_str().encode_wide());
            units.extend(suffix.encode_utf16());
            units.push(0);
            self.set(name, REG_SZ, &utf16_bytes(&units))
        }
    }

    pub(super) fn register() -> Result<AssociationReport, FileAssocError> {
        let executable = std::env::current_exe().map_err(FileAssocError::CurrentExecutable)?;

        let prog_id = Key::create(PROG_ID)?;
        prog_id.set_string(None, FRIENDLY_TYPE_NAME)?;
        prog_id.set_path_string(Some("DefaultIcon"), "\"", &executable, "\",0")?;
        let prog_id_command = Key::create(&format!(r"{PROG_ID}\shell\open\command"))?;
        prog_id_command.set_path_string(None, "\"", &executable, "\" \"%1\"")?;
        drop(prog_id_command);
        drop(prog_id);

        let application = format!(r"Applications\{APPLICATION_EXE}");
        let application_key = Key::create(&application)?;
        application_key.set_string(Some("FriendlyAppName"), FRIENDLY_APP_NAME)?;
        let application_command = Key::create(&format!(r"{application}\shell\open\command"))?;
        application_command.set_path_string(None, "\"", &executable, "\" \"%1\"")?;
        drop(application_command);
        let supported_types = Key::create(&format!(r"{application}\SupportedTypes"))?;
        for extension in SUPPORTED_EXTENSIONS {
            supported_types.set(Some(extension), REG_NONE, &[])?;
        }
        drop(supported_types);
        drop(application_key);

        for extension in SUPPORTED_EXTENSIONS {
            let key = Key::create(&format!(r"{extension}\OpenWithProgids"))?;
            key.set(Some(PROG_ID), REG_NONE, &[])?;
        }

        Ok(AssociationReport {
            prog_id: PROG_ID,
            extensions: SUPPORTED_EXTENSIONS.len(),
        })
    }

    pub(super) fn unregister() -> Result<(), FileAssocError> {
        delete_tree(PROG_ID)?;
        delete_tree(&format!(r"Applications\{APPLICATION_EXE}"))?;

        let prog_id = wide(PROG_ID);
        for extension in SUPPORTED_EXTENSIONS {
            let Some(key) = Key::open(&format!(r"{extension}\OpenWithProgids"))? else {
                continue;
            };
            let status = unsafe { RegDeleteValueW(key.0, PCWSTR(prog_id.as_ptr())) };
            match status {
                ERROR_SUCCESS | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => {}
                other => return Err(registry_error("RegDeleteValueW", other)),
            }
        }
        Ok(())
    }

    fn delete_tree(subkey: &str) -> Result<(), FileAssocError> {
        let subkey = wide(&classes_key(subkey));
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
            other => Err(registry_error("RegDeleteTreeW", other)),
        }
    }

    fn check(operation: &'static str, status: WIN32_ERROR) -> Result<(), FileAssocError> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(registry_error(operation, status))
        }
    }

    fn registry_error(operation: &'static str, status: WIN32_ERROR) -> FileAssocError {
        FileAssocError::Registry {
            operation,
            code: status.0,
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn utf16_bytes(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{PROG_ID, SUPPORTED_EXTENSIONS, association_command_line};

    #[test]
    fn supported_extensions_are_unique_dotted_and_lowercase() {
        let mut seen = HashSet::new();
        for extension in SUPPORTED_EXTENSIONS {
            assert!(
                extension.starts_with('.'),
                "{extension} must start with a dot"
            );
            assert!(
                extension.len() > 1,
                "{extension} must name a file type after the dot"
            );
            assert!(extension.is_ascii(), "{extension} must be ASCII");
            assert_eq!(
                *extension,
                extension.to_ascii_lowercase(),
                "{extension} must be lowercase"
            );
            assert!(seen.insert(*extension), "{extension} is listed twice");
        }
    }

    #[test]
    fn command_line_quotes_the_executable_and_placeholder() {
        assert!(PROG_ID.contains('.'));
        let command = association_command_line();
        assert!(command.starts_with('"'), "{command}");
        assert!(command.ends_with("\"%1\""), "{command}");
    }
}
