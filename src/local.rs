//! Local filesystem operations. These run on the UI thread: they are fast
//! enough that blocking is imperceptible, unlike anything that crosses the
//! network.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::types::{EntryKind, FileEntry, kind_from_mode, perm_string, sort_entries};

pub fn list_dir(path: &Path) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let rd = fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))?;
    for item in rd {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().to_string();
        // lstat: we want to show symlinks as symlinks, not as their targets.
        let meta = match fs::symlink_metadata(item.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mode = meta.permissions().mode();
        let kind = kind_from_mode(mode);
        let (link_target, points_to_dir) = if kind == EntryKind::Symlink {
            let target = fs::read_link(item.path())
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            let resolves_to_dir = fs::metadata(item.path())
                .map(|m| m.is_dir())
                .unwrap_or(false);
            (target, resolves_to_dir)
        } else {
            (None, false)
        };
        out.push(FileEntry {
            name,
            kind,
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            perms: perm_string(mode),
            link_target,
            points_to_dir,
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir(path).with_context(|| format!("mkdir {}", path.display()))
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).with_context(|| format!("rename {}", from.display()))
}

pub fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("rm -r {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("rm {}", path.display()))
    }
}

/// Total byte count of a file or directory tree, used to size progress bars.
pub fn tree_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(path) {
        for item in rd.flatten() {
            total += tree_size(&item.path());
        }
    }
    total
}

/// Expand a leading `~` and make the path absolute, but do not resolve
/// symlinks — users expect the path they typed.
pub fn expand(input: &str) -> PathBuf {
    let expanded = if input == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else if let Some(rest) = input.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(rest)
    } else {
        PathBuf::from(input)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(expanded)
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    // Best-effort: a failure here should never abort a transfer.
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777));
}
