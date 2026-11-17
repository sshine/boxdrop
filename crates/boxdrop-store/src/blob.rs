use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use boxdrop_core::Hash32;

/// A sharded, content-addressed file store.
///
/// Writes use a temporary file in the same directory and rename into place, so
/// a reader never observes a partial chunk. A chunk is immutable once written.
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, hash: Hash32) -> PathBuf {
        let hex = hash.to_hex();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(hex)
    }

    pub fn contains(&self, hash: Hash32) -> Result<bool> {
        Ok(self.path_for(hash).is_file())
    }

    pub fn put(&self, hash: Hash32, bytes: &[u8]) -> Result<()> {
        if Hash32::of(bytes) != hash {
            bail!("refusing to store a chunk under the wrong hash");
        }
        let path = self.path_for(hash);
        if path.is_file() {
            return Ok(());
        }
        let parent = path
            .parent()
            .context("chunk path has no parent")?
            .to_path_buf();
        fs::create_dir_all(&parent)
            .with_context(|| format!("creating blob directory {}", parent.display()))?;
        let tmp = parent.join(format!(".{}.tmp", hash.short()));
        {
            let mut file = fs::File::create(&tmp)
                .with_context(|| format!("creating temporary blob {}", tmp.display()))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)
            .with_context(|| format!("renaming blob into {}", path.display()))?;
        if let Ok(directory) = fs::File::open(&parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    pub fn get(&self, hash: Hash32) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        let bytes = fs::read(&path).with_context(|| format!("reading blob {}", path.display()))?;
        if Hash32::of(&bytes) != hash {
            bail!("blob {} is corrupt", path.display());
        }
        Ok(bytes)
    }

    pub fn remove(&self, hash: Hash32) -> Result<()> {
        let path = self.path_for(hash);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
