use std::path::{Path, PathBuf};

/// Configuration, data and cache roots for one application profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilePaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub portable: bool,
}

impl ProfilePaths {
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn layout_file(&self) -> PathBuf {
        self.config_dir.join("layout.json")
    }

    pub fn recovery_db(&self) -> PathBuf {
        self.data_dir.join("recovery.sqlite")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    /// Creates the directories required for a working profile.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }
}

/// Resolves the active profile.
///
/// A `portable` marker next to the executable switches config/data/cache into
/// `./data`, matching the portable ZIP distribution. A missing marker uses the
/// per-user directories reported by `directories`.
pub fn resolve_profile() -> std::io::Result<ProfilePaths> {
    let executable = std::env::current_exe()?;
    let executable_dir = executable
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let portable_marker = executable_dir.join("portable");
    if portable_marker.exists() {
        let root = executable_dir.join("data");
        return Ok(ProfilePaths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            portable: true,
        });
    }

    let dirs = directories::ProjectDirs::from("dev", "notepust", "Notepust").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the system did not provide a user profile directory",
        )
    })?;

    Ok(ProfilePaths {
        config_dir: dirs.config_dir().to_path_buf(),
        data_dir: dirs.data_local_dir().to_path_buf(),
        cache_dir: dirs.cache_dir().to_path_buf(),
        portable: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_recovery_paths_are_named() {
        let paths = ProfilePaths {
            config_dir: PathBuf::from("c"),
            data_dir: PathBuf::from("d"),
            cache_dir: PathBuf::from("e"),
            portable: false,
        };
        assert_eq!(paths.settings_file(), PathBuf::from("c/settings.toml"));
        assert_eq!(paths.recovery_db(), PathBuf::from("d/recovery.sqlite"));
    }
}
