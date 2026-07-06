// src/memory/mod.rs

pub mod buffer;
pub mod core;
pub mod paths;
pub mod rollup;
pub mod search;

pub use paths::MemoryPaths;

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) fn ensure_dir(dir: &Path) -> Result<(), MemoryError> {
    fs::create_dir_all(dir).map_err(|source| MemoryError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn ensure_dir_creates_nested_directories() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a").join("b").join("memory");
        assert!(!nested.exists());
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_dir_is_idempotent_on_existing_directory() {
        let root = tempdir().unwrap();
        ensure_dir(root.path()).unwrap();
        ensure_dir(root.path()).unwrap();
        assert!(root.path().is_dir());
    }
}
