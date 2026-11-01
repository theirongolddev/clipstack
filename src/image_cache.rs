//! Image entries as files, for programs that want a path rather than bytes
//! (the omarchy-shell overlay's thumbnails): `<runtime>/clipstack-images/<id>.png`.
//! The dir is 0700 and each file 0600, written to a temp file and renamed
//! so a reader never sees half a PNG. The runtime dir is a tmpfs, so the
//! cache is gone at logout; files whose entry is gone are removed sooner, by
//! `delete`, `clear`, `get --image-path` and the daemon's expiry tick.

use crate::storage::{ClipEntry, EntryId, Kind, Storage, StorageError};
use crate::{daemon, perms};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The cache dir's name inside the runtime dir.
pub const DIR_NAME: &str = "clipstack-images";

/// `<runtime>/clipstack-images`.
pub fn dir() -> Result<PathBuf> {
    Ok(daemon::runtime_dir()?.join(DIR_NAME))
}

/// Make sure `<dir>/<id>.png` holds image `entry` and return its path. A
/// file of the right size is trusted (ids are never reused); anything else
/// is rewritten. Text entries are `StorageError::NotImage` (exit 1).
pub fn ensure(storage: &Storage, entry: &ClipEntry) -> Result<PathBuf> {
    if entry.kind != Kind::Image {
        return Err(StorageError::NotImage { id: entry.id }.into());
    }
    let dir = dir()?;
    perms::ensure_private_dir(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    prune_in(&dir, &storage.image_ids()?);

    let path = file_path(&dir, entry.id);
    let cached = std::fs::symlink_metadata(&path)
        .is_ok_and(|meta| meta.is_file() && usize::try_from(meta.len()).ok() == Some(entry.size));
    if !cached {
        let png = storage.load_image(entry.id)?;
        write_atomically(&dir, &path, &png).with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(path)
}

/// Remove the cached files of entries that are no longer images in
/// `storage` (deleted, cleared, expired). Never fails: a cache that can't be
/// cleaned is only logged, and a missing dir is left missing.
pub fn prune(storage: &Storage) {
    if let Ok(dir) = dir() {
        prune_dir(storage, &dir);
    }
}

/// `prune` of the cache at `dir` (the daemon passes the dir it resolved at
/// startup).
pub fn prune_dir(storage: &Storage, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    match storage.image_ids() {
        Ok(live) => prune_in(dir, &live),
        Err(e) => eprintln!("[images] cannot list image entries to clean the cache: {e:#}"),
    }
}

/// Remove the cached file of entry `id`, if any.
pub fn remove(id: EntryId) {
    let Ok(dir) = dir() else { return };
    match std::fs::remove_file(file_path(&dir, id)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            eprintln!("[images] cannot remove the cached image of id:{id}: {e}");
        }
        _ => {}
    }
}

fn file_path(dir: &Path, id: EntryId) -> PathBuf {
    dir.join(format!("{id}.png"))
}

/// Remove every `<id>.png` in `dir` whose id is not in `live`; other names
/// are left alone.
fn prune_in(dir: &Path, live: &HashSet<EntryId>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(cached_id) else { continue };
        if !live.contains(&id)
            && let Err(e) = std::fs::remove_file(entry.path())
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("[images] cannot remove {}: {e}", entry.path().display());
        }
    }
}

/// The id in a cache file name `<id>.png` (digits only).
fn cached_id(name: &str) -> Option<EntryId> {
    let digits = name.strip_suffix(".png")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Write `bytes` to a 0600 temp file in `dir`, then rename it to `path`.
fn write_atomically(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    // A temp file left by a crashed run with the same pid
    let _ = std::fs::remove_file(&tmp);
    let written = perms::private_file_options()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_ids_are_digits_dot_png() {
        assert_eq!(cached_id("42.png"), Some(42));
        for name in [".png", "x.png", "+4.png", "4.png.tmp-1", ".4.png.tmp-9", "4.PNG", "notes.txt"] {
            assert_eq!(cached_id(name), None, "{name}");
        }
    }

    #[test]
    fn prune_removes_only_files_of_gone_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in ["1.png", "2.png", "notes.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        prune_in(dir.path(), &HashSet::from([2]));
        let mut left: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["2.png", "notes.txt"]);
    }

    #[test]
    fn atomic_write_is_private_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("7.png");
        std::fs::write(&path, b"old").unwrap();
        write_atomically(dir.path(), &path, b"new bytes").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new bytes");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
