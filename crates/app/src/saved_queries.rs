//! File-backed saved queries.
//!
//! Each query is an ordinary UTF-8 `.sql` file beneath one private app-owned
//! directory. Relative paths provide folders without introducing a metadata
//! database or a proprietary file format.

// The rename/delete/root API surface is broader than the current navigator
// consumes; it is the module's public contract, not dead code.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use crate::storage;

pub type Result<T> = std::result::Result<T, SavedQueryError>;

#[derive(Debug)]
pub enum SavedQueryError {
    InvalidPath(PathBuf),
    Io { path: PathBuf, source: io::Error },
}

impl std::fmt::Display for SavedQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(
                formatter,
                "saved-query path must be a relative path ending in .sql: {}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "could not access {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SavedQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath(_) => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SavedQuery {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct SavedQueryStore {
    root: PathBuf,
}

impl SavedQueryStore {
    /// Open the application saved-query directory, creating it when needed.
    pub fn open_default() -> Result<Self> {
        Self::open(storage::app_data_dir().join("saved_queries"))
    }

    /// Open a saved-query directory at an explicit path.
    ///
    /// This is also useful for portable profiles and isolated tests.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        storage::ensure_private_dir(&root).map_err(|source| io_error(&root, source))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Recursively list `.sql` files, ordered by their relative paths.
    pub fn list(&self) -> Result<Vec<SavedQuery>> {
        storage::ensure_private_dir(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        let mut queries = Vec::new();
        self.list_directory(&self.root, &mut queries)?;
        queries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(queries)
    }

    /// Read a saved query without following symlinks.
    pub fn read(&self, relative_path: impl AsRef<Path>) -> Result<String> {
        let relative_path = validate_query_path(relative_path.as_ref())?;
        let path = self.resolve_existing_file(&relative_path)?;
        fs::read_to_string(&path).map_err(|source| io_error(&path, source))
    }

    /// Atomically create or replace a saved query.
    pub fn write(&self, relative_path: impl AsRef<Path>, sql: &str) -> Result<()> {
        let relative_path = validate_query_path(relative_path.as_ref())?;
        self.prepare_parent(&relative_path)?;
        let path = self.root.join(&relative_path);
        storage::atomic_write(&path, sql.as_bytes()).map_err(|source| io_error(&path, source))
    }

    /// Atomically rename a query. Existing destinations are never overwritten.
    pub fn rename(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<()> {
        let from = validate_query_path(from.as_ref())?;
        let to = validate_query_path(to.as_ref())?;
        if from == to {
            self.resolve_existing_file(&from)?;
            return Ok(());
        }

        let source = self.resolve_existing_file(&from)?;
        self.prepare_parent(&to)?;
        let destination = self.root.join(&to);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(io_error(
                    &destination,
                    io::Error::new(io::ErrorKind::AlreadyExists, "saved query already exists"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&destination, source)),
        }

        fs::rename(&source, &destination).map_err(|error| io_error(&source, error))?;
        storage::harden_private_file(&destination)
            .map_err(|source| io_error(&destination, source))?;
        sync_parent(&source)?;
        if source.parent() != destination.parent() {
            sync_parent(&destination)?;
        }
        Ok(())
    }

    /// Delete a saved query without following symlinks.
    pub fn delete(&self, relative_path: impl AsRef<Path>) -> Result<()> {
        let relative_path = validate_query_path(relative_path.as_ref())?;
        let path = self.resolve_existing_file(&relative_path)?;
        fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
        sync_parent(&path)
    }

    fn list_directory(&self, directory: &Path, queries: &mut Vec<SavedQuery>) -> Result<()> {
        let entries = fs::read_dir(directory).map_err(|source| io_error(directory, source))?;
        for entry in entries {
            let entry = entry.map_err(|source| io_error(directory, source))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(io_error(
                    &path,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "symbolic links are not allowed in saved queries",
                    ),
                ));
            }
            if file_type.is_dir() {
                storage::ensure_private_dir(&path).map_err(|source| io_error(&path, source))?;
                self.list_directory(&path, queries)?;
            } else if file_type.is_file() && path.extension() == Some(OsStr::new("sql")) {
                storage::harden_private_file(&path)
                    .map_err(|source| io_error(&path, source))?;
                let relative_path = path
                    .strip_prefix(&self.root)
                    .expect("listed paths remain inside the saved-query root")
                    .to_path_buf();
                queries.push(SavedQuery {
                    path: relative_path,
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                });
            }
        }
        Ok(())
    }

    fn resolve_existing_file(&self, relative_path: &Path) -> Result<PathBuf> {
        self.verify_parent(relative_path)?;
        let path = self.root.join(relative_path);
        storage::harden_private_file(&path).map_err(|source| io_error(&path, source))?;
        Ok(path)
    }

    fn prepare_parent(&self, relative_path: &Path) -> Result<()> {
        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        storage::ensure_private_child_dirs(&self.root, parent)
            .map_err(|source| io_error(&self.root.join(parent), source))
    }

    fn verify_parent(&self, relative_path: &Path) -> Result<()> {
        storage::ensure_private_dir(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        let mut current = self.root.clone();
        for component in parent.components() {
            let Component::Normal(component) = component else {
                return Err(SavedQueryError::InvalidPath(relative_path.to_path_buf()));
            };
            current.push(component);
            let metadata = fs::symlink_metadata(&current)
                .map_err(|source| io_error(&current, source))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io_error(
                    &current,
                    io::Error::new(io::ErrorKind::InvalidInput, "not a regular directory"),
                ));
            }
            storage::ensure_private_dir(&current)
                .map_err(|source| io_error(&current, source))?;
        }
        Ok(())
    }
}

fn validate_query_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.extension() != Some(OsStr::new("sql")) {
        return Err(SavedQueryError::InvalidPath(path.to_path_buf()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) if !component.is_empty() => normalized.push(component),
            _ => return Err(SavedQueryError::InvalidPath(path.to_path_buf())),
        }
    }

    if normalized.as_os_str().is_empty() {
        Err(SavedQueryError::InvalidPath(path.to_path_buf()))
    } else {
        Ok(normalized)
    }
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory"),
        )
    })?;
    storage::sync_directory(parent).map_err(|source| io_error(parent, source))
}

fn io_error(path: &Path, source: io::Error) -> SavedQueryError {
    SavedQueryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_support::TestDir;

    #[test]
    fn writes_reads_and_lists_ordinary_sql_files() {
        let temporary = TestDir::new("saved-queries");
        let store = SavedQueryStore::open(temporary.path().join("queries")).unwrap();

        store
            .write("reporting/daily.sql", "select * from daily_report;\n")
            .unwrap();
        store.write("scratch.sql", "select 1;").unwrap();

        assert_eq!(
            store.read("reporting/daily.sql").unwrap(),
            "select * from daily_report;\n"
        );
        let paths: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|query| query.path)
            .collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("reporting/daily.sql"),
                PathBuf::from("scratch.sql")
            ]
        );
    }

    #[test]
    fn write_atomically_replaces_an_existing_query() {
        let temporary = TestDir::new("saved-query-replace");
        let store = SavedQueryStore::open(temporary.path().join("queries")).unwrap();

        store.write("query.sql", "select 1;").unwrap();
        store.write("query.sql", "select 2;").unwrap();

        assert_eq!(store.read("query.sql").unwrap(), "select 2;");
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn rename_never_overwrites_an_existing_query() {
        let temporary = TestDir::new("saved-query-rename");
        let store = SavedQueryStore::open(temporary.path().join("queries")).unwrap();
        store.write("first.sql", "first").unwrap();
        store.write("second.sql", "second").unwrap();

        let error = store.rename("first.sql", "second.sql").unwrap_err();

        assert!(matches!(error, SavedQueryError::Io { .. }));
        assert_eq!(store.read("first.sql").unwrap(), "first");
        assert_eq!(store.read("second.sql").unwrap(), "second");
    }

    #[test]
    fn rename_and_delete_update_the_file_tree() {
        let temporary = TestDir::new("saved-query-actions");
        let store = SavedQueryStore::open(temporary.path().join("queries")).unwrap();
        store.write("old.sql", "select 1;").unwrap();

        store.rename("old.sql", "folder/new.sql").unwrap();
        assert_eq!(store.read("folder/new.sql").unwrap(), "select 1;");
        assert!(store.read("old.sql").is_err());

        store.delete("folder/new.sql").unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn rejects_paths_outside_the_store_and_non_sql_files() {
        let temporary = TestDir::new("saved-query-paths");
        let store = SavedQueryStore::open(temporary.path().join("queries")).unwrap();

        for path in ["../escape.sql", "/absolute.sql", "notes.txt", ""] {
            assert!(matches!(
                store.write(path, "select 1;"),
                Err(SavedQueryError::InvalidPath(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn directories_and_queries_have_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TestDir::new("saved-query-permissions");
        let root = temporary.path().join("queries");
        let store = SavedQueryStore::open(&root).unwrap();
        store.write("folder/query.sql", "select 1;").unwrap();

        assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(root.join("folder"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("folder/query.sql"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = TestDir::new("saved-query-symlink");
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root = temporary.path().join("queries");
        let store = SavedQueryStore::open(&root).unwrap();
        symlink(&outside, root.join("linked")).unwrap();

        assert!(store.write("linked/escape.sql", "select 1;").is_err());
        assert!(store.list().is_err());
        assert!(!outside.join("escape.sql").exists());
    }
}
