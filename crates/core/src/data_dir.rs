use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Environment variable that overrides the platform data directory.
pub const DATA_DIR_ENV: &str = "RYUUJI_DATA_DIR";

/// How the data directory was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataDirSource {
    PlatformDefault,
    /// `RYUUJI_DATA_DIR` was set.
    EnvOverride,
    /// Handed in directly through [`DataDir::at`].
    Explicit,
}

/// The directory Ryuuji keeps its files in. Constructing one guarantees the
/// root and its `logs/` subdirectory exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
    source: DataDirSource,
}

#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    #[error("no platform data directory is available; set {DATA_DIR_ENV}")]
    NoPlatformDir,
    #[error("could not create {}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl DataDir {
    /// `RYUUJI_DATA_DIR` when set and non-empty, else the platform-local
    /// application data directory.
    pub fn resolve() -> Result<DataDir, DataDirError> {
        let platform = directories::ProjectDirs::from("dev", "umaru", "Ryuuji")
            .map(|dirs| dirs.data_local_dir().to_path_buf());
        DataDir::from_parts(std::env::var_os(DATA_DIR_ENV), platform)
    }

    /// Pure selection between an environment override and the platform
    /// default; an empty override counts as unset.
    pub fn from_parts(
        env_override: Option<OsString>,
        platform_default: Option<PathBuf>,
    ) -> Result<DataDir, DataDirError> {
        let (root, source) = match env_override.filter(|value| !value.is_empty()) {
            Some(value) => (PathBuf::from(value), DataDirSource::EnvOverride),
            None => (
                platform_default.ok_or(DataDirError::NoPlatformDir)?,
                DataDirSource::PlatformDefault,
            ),
        };
        DataDir::create(root, source)
    }

    /// Uses `root` as the data directory, creating it and `logs/`.
    pub fn at(root: impl Into<PathBuf>) -> Result<DataDir, DataDirError> {
        DataDir::create(root.into(), DataDirSource::Explicit)
    }

    fn create(root: PathBuf, source: DataDirSource) -> Result<DataDir, DataDirError> {
        let dir = DataDir { root, source };
        create_dir(&dir.root)?;
        create_dir(&dir.logs())?;
        Ok(dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source(&self) -> DataDirSource {
        self.source
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub(crate) fn library_db(&self) -> PathBuf {
        self.root.join("library.sqlite")
    }

    pub(crate) fn settings_file(&self) -> PathBuf {
        self.root.join("settings.toml")
    }
}

fn create_dir(path: &Path) -> Result<(), DataDirError> {
    fs::create_dir_all(path).map_err(|source| DataDirError::Create {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_platform_default() {
        let tmp = tempfile::tempdir().unwrap();
        let wanted = tmp.path().join("override");
        let platform = tmp.path().join("platform");
        let dir = DataDir::from_parts(Some(wanted.clone().into()), Some(platform.clone())).unwrap();
        assert_eq!(dir.root(), wanted);
        assert_eq!(dir.source(), DataDirSource::EnvOverride);
        assert!(!platform.exists());
    }

    #[test]
    fn empty_override_counts_as_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let platform = tmp.path().join("platform");
        let dir = DataDir::from_parts(Some(OsString::new()), Some(platform.clone())).unwrap();
        assert_eq!(dir.root(), platform);
        assert_eq!(dir.source(), DataDirSource::PlatformDefault);
    }

    #[test]
    fn nothing_to_choose_from_is_an_error() {
        assert!(matches!(
            DataDir::from_parts(None, None),
            Err(DataDirError::NoPlatformDir)
        ));
    }

    #[test]
    fn at_creates_root_and_logs_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("nested").join("data");
        let dir = DataDir::at(&root).unwrap();
        assert!(root.is_dir());
        assert!(dir.logs().is_dir());
        assert_eq!(dir.source(), DataDirSource::Explicit);
        assert_eq!(DataDir::at(&root).unwrap(), dir);
    }
}
