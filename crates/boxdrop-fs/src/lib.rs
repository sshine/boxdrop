//! Filesystem access: scanning, watching, atomic replacement and reflinks.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use boxdrop_core::{ContentManifest, EntryKind, Fingerprint, Hash32, MIN_BLOCK};
use walkdir::WalkDir;

pub mod watch;

pub use watch::{WatchEvent, WatchKind, Watcher};

/// One file, directory or symlink found by a scan. `blocks` holds the file's
/// bytes for small and medium files; the engine stores them in the CAS.
#[derive(Clone, Debug)]
pub struct ScannedEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ns: i64,
    pub mode: u32,
    pub content_id: Option<Hash32>,
    pub manifest: Option<ContentManifest>,
    pub symlink_target: Option<String>,
    pub blocks: Vec<Vec<u8>>,
    pub fingerprint: Fingerprint,
}

impl ScannedEntry {
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    pub ignore: Vec<String>,
}

impl ScanOptions {
    pub fn with_defaults() -> Self {
        Self {
            ignore: [
                ".git",
                ".hg",
                ".svn",
                "target",
                "node_modules",
                ".direnv",
                ".boxdrop",
                ".boxdrop-tmp",
            ]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        }
    }

    pub fn ignored(&self, relative: &str) -> bool {
        let mut components = relative.split('/');
        components.any(|component| {
            component.starts_with(".boxdrop-tmp")
                || component.starts_with(".boxdrop-tmp-")
                || self.ignore.iter().any(|pattern| {
                    pattern == component
                        || component.ends_with(pattern.trim_start_matches('*'))
                        || (pattern.starts_with("*.") && component.ends_with(&pattern[1..]))
                })
        })
    }
}

pub fn scan_root(root: &Path, options: &ScanOptions) -> Result<Vec<ScannedEntry>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for item in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                // A file can vanish between readdir and stat. That is normal in a
                // live tree and the next scan will settle it.
                if error.io_error().is_some() {
                    continue;
                }
                return Err(error.into());
            }
        };
        let absolute = item.path();
        let relative = match absolute.strip_prefix(root) {
            Ok(path) if path.as_os_str().is_empty() => continue,
            Ok(path) => normalize_relative(path)?,
            Err(_) => continue,
        };
        if options.ignored(&relative) {
            if item.file_type().is_dir() {
                // WalkDir cannot prune from here, but ignoring the path is enough:
                // its children are ignored by the same prefix rule.
            }
            continue;
        }
        let metadata = match fs::symlink_metadata(absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            entries.push(ScannedEntry {
                path: relative,
                kind: EntryKind::Directory,
                size: 0,
                mtime_ns: mtime_ns(&metadata),
                mode: mode(&metadata),
                content_id: None,
                manifest: None,
                symlink_target: None,
                blocks: Vec::new(),
                fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::Directory),
            });
        } else if file_type.is_symlink() {
            let target = fs::read_link(absolute)?;
            entries.push(ScannedEntry {
                path: relative,
                kind: EntryKind::Symlink,
                size: 0,
                mtime_ns: mtime_ns(&metadata),
                mode: mode(&metadata),
                content_id: None,
                manifest: None,
                symlink_target: Some(target.to_string_lossy().into_owned()),
                blocks: Vec::new(),
                fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::Symlink),
            });
        } else if file_type.is_file() {
            let bytes = match fs::read(absolute) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let (manifest, blocks) = ContentManifest::from_bytes(&bytes);
            entries.push(ScannedEntry {
                path: relative,
                kind: EntryKind::File,
                size: bytes.len() as u64,
                mtime_ns: mtime_ns(&metadata),
                mode: mode(&metadata),
                content_id: Some(manifest.content_id),
                manifest: Some(manifest),
                symlink_target: None,
                blocks,
                fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::File),
            });
        }
    }
    Ok(entries)
}

/// The fingerprint fields of a single path, without reading its content.
pub fn fingerprint(path: &Path) -> Result<Option<Fingerprint>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let kind = if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    Ok(Some(Fingerprint::from_metadata(&metadata, kind)))
}

/// Read one path and produce its manifest and blocks.
pub fn scan_path(root: &Path, relative: &str) -> Result<Option<ScannedEntry>> {
    let relative = normalize_relative(Path::new(relative))?;
    let absolute = safe_join(root, &relative)?;
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() {
        return Ok(Some(ScannedEntry {
            path: relative,
            kind: EntryKind::Directory,
            size: 0,
            mtime_ns: mtime_ns(&metadata),
            mode: mode(&metadata),
            content_id: None,
            manifest: None,
            symlink_target: None,
            blocks: Vec::new(),
            fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::Directory),
        }));
    }
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&absolute)?;
        return Ok(Some(ScannedEntry {
            path: relative,
            kind: EntryKind::Symlink,
            size: 0,
            mtime_ns: mtime_ns(&metadata),
            mode: mode(&metadata),
            content_id: None,
            manifest: None,
            symlink_target: Some(target.to_string_lossy().into_owned()),
            blocks: Vec::new(),
            fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::Symlink),
        }));
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&absolute)?;
    let (manifest, blocks) = ContentManifest::from_bytes(&bytes);
    Ok(Some(ScannedEntry {
        path: relative,
        kind: EntryKind::File,
        size: bytes.len() as u64,
        mtime_ns: mtime_ns(&metadata),
        mode: mode(&metadata),
        content_id: Some(manifest.content_id),
        manifest: Some(manifest),
        symlink_target: None,
        blocks,
        fingerprint: Fingerprint::from_metadata(&metadata, EntryKind::File),
    }))
}

/// Replace a regular file atomically, verifying every block and the final
/// content hash before the rename.
pub fn write_file_atomic(
    root: &Path,
    relative: &str,
    manifest: &ContentManifest,
    blocks: &[Vec<u8>],
    mode: u32,
) -> Result<()> {
    if blocks.len() != manifest.blocks.len() {
        bail!(
            "refusing to write {}: {} blocks supplied for {} hashes",
            relative,
            blocks.len(),
            manifest.blocks.len()
        );
    }
    let relative = normalize_relative(Path::new(relative))?;
    let destination = safe_join(root, &relative)?;
    let parent = destination.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".boxdrop-tmp-{}-{}",
        std::process::id(),
        Hash32::of(format!("{relative}-{}", now_nanos())).short()
    ));
    let mut hasher = blake3::Hasher::new();
    {
        let mut file =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        for (expected, bytes) in manifest.blocks.iter().zip(blocks) {
            if Hash32::of(bytes) != *expected {
                let _ = fs::remove_file(&tmp);
                bail!("block hash mismatch while writing {relative}");
            }
            hasher.update(bytes);
            file.write_all(bytes)?;
        }
        file.sync_all()?;
    }
    let actual = Hash32::new(*hasher.finalize().as_bytes());
    if actual != manifest.content_id {
        let _ = fs::remove_file(&tmp);
        bail!(
            "content hash mismatch while writing {relative}: expected {}, got {actual}",
            manifest.content_id
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode & 0o7777));
    }
    fs::rename(&tmp, &destination)
        .with_context(|| format!("renaming into {}", destination.display()))?;
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn write_symlink_atomic(root: &Path, relative: &str, target: &str) -> Result<()> {
    let relative = normalize_relative(Path::new(relative))?;
    let destination = safe_join(root, &relative)?;
    let parent = destination.parent().context("symlink has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".boxdrop-tmp-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, &destination)?;
    Ok(())
}

pub fn ensure_directory(root: &Path, relative: &str) -> Result<()> {
    let relative = normalize_relative(Path::new(relative))?;
    let destination = safe_join(root, &relative)?;
    fs::create_dir_all(destination)?;
    Ok(())
}

pub fn remove_path(root: &Path, relative: &str) -> Result<()> {
    let relative = normalize_relative(Path::new(relative))?;
    let destination = safe_join(root, &relative)?;
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() => {
            let _ = fs::remove_dir(&destination);
        }
        Ok(_) => fs::remove_file(&destination)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Clone `source` to `destination` on a reflink-capable filesystem, falling
/// back to a plain copy. Returns whether a reflink was used.
pub fn reflink_or_copy(source: &Path, destination: &Path) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let input = fs::File::open(source)?;
        let output = fs::File::create(destination)?;
        let result = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
        if result == 0 {
            return Ok(true);
        }
    }
    fs::copy(source, destination)?;
    Ok(false)
}

pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = normalize_relative(Path::new(relative))?;
    let path = root.join(&relative);
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let parent = path
        .parent()
        .map(|parent| {
            parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf())
        })
        .unwrap_or_else(|| root.clone());
    if !parent.starts_with(&root) {
        bail!("path escapes root: {relative}");
    }
    Ok(path)
}

pub fn normalize_relative(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().context("path is not valid UTF-8")?;
                if value.is_empty() || value == "." {
                    continue;
                }
                parts.push(value.to_owned());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe path: {}", path.display());
            }
        }
    }
    Ok(parts.join("/"))
}

pub fn mtime_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(0)
}

pub fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0o644
    }
}

/// Set a file's modification time to a remote entry's timestamp. Without this,
/// every pull would look modified to the next scan and bounce back.
pub fn set_mtime(path: &Path, mtime_ns: i64) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let seconds = mtime_ns.div_euclid(1_000_000_000);
        let nanos = mtime_ns.rem_euclid(1_000_000_000);
        let times = [
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: nanos as libc::c_long,
            },
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: nanos as libc::c_long,
            },
        ];
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let result = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mtime_ns);
    }
    Ok(())
}

pub fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// A convenience view used by tests and the harness: the live tree as
/// `path -> content hash`.
pub fn snapshot_hashes(root: &Path) -> Result<BTreeMap<String, Option<Hash32>>> {
    let mut map = BTreeMap::new();
    for entry in scan_root(root, &ScanOptions::with_defaults())? {
        map.insert(entry.path, entry.content_id);
    }
    Ok(map)
}

pub const DEFAULT_BLOCK: u64 = MIN_BLOCK;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn scans_files_dirs_and_symlinks() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", dir.path().join("link")).unwrap();
        let entries = scan_root(dir.path(), &ScanOptions::with_defaults()).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == "sub" && entry.kind == EntryKind::Directory)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == "sub/a.txt" && entry.size == 5)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == "link" && entry.kind == EntryKind::Symlink)
        );
    }

    #[test]
    fn ignores_internal_and_vcs_directories() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join(".git/objects/x"), b"x").unwrap();
        fs::write(dir.path().join("target/x"), b"x").unwrap();
        fs::write(dir.path().join("keep"), b"x").unwrap();
        let entries = scan_root(dir.path(), &ScanOptions::with_defaults()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "keep");
    }

    #[test]
    fn atomic_write_round_trips_and_replaces() {
        let dir = TempDir::new().unwrap();
        let (manifest, blocks) = ContentManifest::from_bytes(b"one");
        write_file_atomic(dir.path(), "a/b.txt", &manifest, &blocks, 0o644).unwrap();
        assert_eq!(fs::read(dir.path().join("a/b.txt")).unwrap(), b"one");
        let (manifest, blocks) = ContentManifest::from_bytes(b"two");
        write_file_atomic(dir.path(), "a/b.txt", &manifest, &blocks, 0o644).unwrap();
        assert_eq!(fs::read(dir.path().join("a/b.txt")).unwrap(), b"two");
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let (manifest, blocks) = ContentManifest::from_bytes(b"x");
        assert!(write_file_atomic(dir.path(), "../escape", &manifest, &blocks, 0o644).is_err());
        assert!(normalize_relative(Path::new("/absolute")).is_err());
    }

    #[test]
    fn rejects_wrong_block_hash() {
        let dir = TempDir::new().unwrap();
        let (manifest, _) = ContentManifest::from_bytes(b"one");
        let blocks = vec![b"two".to_vec()];
        assert!(write_file_atomic(dir.path(), "a", &manifest, &blocks, 0o644).is_err());
    }
}
