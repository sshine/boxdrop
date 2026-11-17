//! Native filesystem capabilities.
//!
//! Snapshots and reflinks are accelerators, never requirements. Detection is
//! best effort: a missing `zfs` binary or an unavailable dataset simply means
//! the portable scanner is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeKind {
    Zfs,
    Btrfs,
    Reflink,
    Portable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub kind: NativeKind,
    pub reflink: bool,
    pub mount_point: PathBuf,
    pub fstype: String,
    pub source: String,
    pub zfs_dataset: Option<String>,
    pub btrfs_subvolume: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountInfo {
    mount_point: PathBuf,
    fstype: String,
    source: String,
    super_options: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeSnapshot {
    pub name: String,
    pub reference: String,
}

pub fn detect(root: &Path) -> Result<Capabilities> {
    let root = root
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", root.display()))?;
    let mount = mount_info(&root)?;
    let fstype = mount.fstype.clone();
    let kind = match fstype.as_str() {
        "zfs" => NativeKind::Zfs,
        "btrfs" => NativeKind::Btrfs,
        _ => {
            if probe_reflink(&root) {
                NativeKind::Reflink
            } else {
                NativeKind::Portable
            }
        }
    };
    let zfs_dataset = (kind == NativeKind::Zfs).then(|| mount.source.clone());
    let btrfs_subvolume = if kind == NativeKind::Btrfs {
        btrfs_subvolume(&root).ok().flatten()
    } else {
        None
    };
    Ok(Capabilities {
        kind,
        reflink: probe_reflink(&root),
        mount_point: mount.mount_point,
        fstype,
        source: mount.source,
        zfs_dataset,
        btrfs_subvolume,
    })
}

/// Determine whether `FICLONE` works in this directory. The probe uses two
/// temporary files and removes them immediately.
pub fn probe_reflink(directory: &Path) -> bool {
    let Ok(temp) = tempfile::tempdir_in(directory) else {
        return false;
    };
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    if fs::write(&source, b"boxdrop reflink probe").is_err() {
        return false;
    }
    crate::reflink_or_copy(&source, &destination).unwrap_or(false)
}

pub fn create_snapshot(root: &Path, name: &str) -> Result<NativeSnapshot> {
    validate_name(name)?;
    let capabilities = detect(root)?;
    match capabilities.kind {
        NativeKind::Zfs => {
            let dataset = capabilities
                .zfs_dataset
                .context("zfs dataset unknown for this root")?;
            let reference = format!("{dataset}@{name}");
            run("zfs", &["snapshot", &reference])?;
            Ok(NativeSnapshot {
                name: name.to_owned(),
                reference,
            })
        }
        NativeKind::Btrfs => {
            let subvolume = capabilities
                .btrfs_subvolume
                .context("btrfs subvolume unknown for this root")?;
            let parent = root.parent().unwrap_or(root);
            let directory = parent.join(".boxdrop-snapshots");
            fs::create_dir_all(&directory)?;
            let destination = directory.join(name);
            run(
                "btrfs",
                &[
                    "subvolume",
                    "snapshot",
                    "-r",
                    &subvolume,
                    destination.to_string_lossy().as_ref(),
                ],
            )?;
            Ok(NativeSnapshot {
                name: name.to_owned(),
                reference: destination.to_string_lossy().into_owned(),
            })
        }
        NativeKind::Reflink | NativeKind::Portable => {
            bail!(
                "native snapshots are not available on {}",
                capabilities.fstype
            )
        }
    }
}

pub fn changed_paths(root: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    validate_name(from)?;
    validate_name(to)?;
    let capabilities = detect(root)?;
    match capabilities.kind {
        NativeKind::Zfs => {
            let dataset = capabilities
                .zfs_dataset
                .context("zfs dataset unknown for this root")?;
            let output = run_output(
                "zfs",
                &[
                    "diff",
                    "-H",
                    &format!("{dataset}@{from}"),
                    &format!("{dataset}@{to}"),
                ],
            )?;
            let mut paths = Vec::new();
            for line in output.lines() {
                let mut fields = line.splitn(4, '\t');
                let _change = fields.next();
                let path = fields.nth(2).unwrap_or_default();
                if let Some(relative) = strip_mount(path, &capabilities.mount_point) {
                    paths.push(relative);
                }
            }
            Ok(paths)
        }
        NativeKind::Btrfs => {
            let parent = root.parent().unwrap_or(root);
            let from_snapshot = parent.join(".boxdrop-snapshots").join(from);
            let to_snapshot = parent.join(".boxdrop-snapshots").join(to);
            let generation = btrfs_generation(&from_snapshot)?
                .context("could not read the source snapshot generation")?;
            let output = run_output(
                "btrfs",
                &[
                    "subvolume",
                    "find-new",
                    to_snapshot.to_string_lossy().as_ref(),
                    &generation.to_string(),
                ],
            )?;
            let mut paths = Vec::new();
            for line in output.lines() {
                if let Some(path) = line.rsplit_once("file ").map(|(_, path)| path) {
                    paths.push(path.trim_start_matches("./").to_owned());
                }
            }
            Ok(paths)
        }
        NativeKind::Reflink | NativeKind::Portable => {
            bail!(
                "native change lists are not available on {}",
                capabilities.fstype
            )
        }
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        bail!("invalid snapshot name: {name}");
    }
    Ok(())
}

fn mount_info(root: &Path) -> Result<MountInfo> {
    let contents = fs::read_to_string("/proc/self/mountinfo").context("reading mountinfo")?;
    let mut best: Option<MountInfo> = None;
    for line in contents.lines() {
        let (left, right) = match line.split_once(" - ") {
            Some(parts) => parts,
            None => continue,
        };
        let left_fields = left.split_whitespace().collect::<Vec<_>>();
        let right_fields = right.split_whitespace().collect::<Vec<_>>();
        if left_fields.len() < 5 || right_fields.len() < 3 {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount(left_fields[4]));
        if !root.starts_with(&mount_point) {
            continue;
        }
        let candidate = MountInfo {
            mount_point,
            fstype: right_fields[0].to_owned(),
            source: right_fields[1].to_owned(),
            super_options: right_fields[2].to_owned(),
        };
        if best.as_ref().is_none_or(|current| {
            candidate.mount_point.components().count() > current.mount_point.components().count()
        }) {
            best = Some(candidate);
        }
    }
    best.context("no mount entry covers this root")
}

fn unescape_mount(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn btrfs_subvolume(root: &Path) -> Result<Option<String>> {
    let output = run_output(
        "btrfs",
        &["subvolume", "show", root.to_string_lossy().as_ref()],
    )?;
    for line in output.lines() {
        if let Some(path) = line.trim().strip_prefix("Path:") {
            return Ok(Some(path.trim().to_owned()));
        }
    }
    Ok(None)
}

fn btrfs_generation(subvolume: &Path) -> Result<Option<u64>> {
    let output = run_output(
        "btrfs",
        &["subvolume", "show", subvolume.to_string_lossy().as_ref()],
    )?;
    for line in output.lines() {
        if let Some(value) = line.trim().strip_prefix("Generation:") {
            return Ok(value.trim().parse().ok());
        }
    }
    Ok(None)
}

fn strip_mount(path: &str, mount_point: &Path) -> Option<String> {
    let path = Path::new(path);
    path.strip_prefix(mount_point).ok().map(|relative| {
        relative
            .to_string_lossy()
            .trim_start_matches('/')
            .to_owned()
    })
}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn run_output(program: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn detects_the_test_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let capabilities = detect(dir.path()).unwrap();
        assert!(matches!(
            capabilities.kind,
            NativeKind::Portable | NativeKind::Reflink
        ));
        assert!(!capabilities.mount_point.as_os_str().is_empty());
    }

    #[test]
    fn portable_snapshots_fail_with_an_explanation() {
        let dir = tempfile::tempdir().unwrap();
        let capabilities = detect(dir.path()).unwrap();
        if matches!(
            capabilities.kind,
            NativeKind::Portable | NativeKind::Reflink
        ) {
            assert!(create_snapshot(dir.path(), "one").is_err());
        }
    }

    #[test]
    fn rejects_unsafe_snapshot_names() {
        assert!(validate_name("good.name-1").is_ok());
        assert!(validate_name("../bad").is_err());
        assert!(validate_name("bad name").is_err());
    }
}
