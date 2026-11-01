//! Private file and directory permissions for ClipStack's own storage.
//!
//! Clipboard history routinely contains secrets, so the storage dir is
//! `0700` and every file in it is `0600`.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const DIR_MODE: u32 = 0o700;
pub const FILE_MODE: u32 = 0o600;

/// What `tighten_dir` changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TightenReport {
    pub dir_changed: bool,
    pub files_changed: usize,
    pub skipped: usize,
}

/// create_dir_all; newly created leaf gets 0700; existing leaf is chmod'ed to 0700.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(path)?;
    // `mode` only applies to dirs created here; fix a pre-existing leaf too
    if fs::metadata(path)?.permissions().mode() & 0o777 != DIR_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// OpenOptions with .mode(0o600) (applied only on create).
pub fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.mode(FILE_MODE);
    options
}

/// chmod dir 0700 and every regular file directly inside it 0600 when group/other bits
/// are set. Uses symlink_metadata (never follows symlinks), is not recursive, and skips
/// entries not owned by the dir's owner. Returns what changed so the first run can log it once.
pub fn tighten_dir(path: &Path) -> io::Result<TightenReport> {
    let mut report = TightenReport::default();

    // The dir goes first: once it is 0700 nobody else can swap its entries
    // between the symlink_metadata check and the chmod below.
    let dir_meta = fs::symlink_metadata(path)?;
    if !dir_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a directory: {}", path.display()),
        ));
    }
    if dir_meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
        report.dir_changed = true;
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if !meta.file_type().is_file() {
            continue; // symlinks, subdirs, sockets: never followed or touched
        }
        if meta.uid() != dir_meta.uid() {
            report.skipped += 1;
            continue;
        }
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(FILE_MODE))?;
            report.files_changed += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn ensure_private_dir_creates_0700() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("a").join("clipd");
        ensure_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(mode(&dir), DIR_MODE);
    }

    #[test]
    fn ensure_private_dir_fixes_existing_0755() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("clipd");
        fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o755);
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), DIR_MODE);
    }

    #[test]
    fn private_file_options_creates_0600() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("f.txt");
        let mut file = private_file_options()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(b"secret").unwrap();
        assert_eq!(mode(&path), FILE_MODE);
    }

    #[test]
    fn tighten_dir_fixes_world_readable_files() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("clipd");
        fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o755);
        for name in ["index.json", "1_000.txt"] {
            fs::write(dir.join(name), "x").unwrap();
            chmod(&dir.join(name), 0o644);
        }
        fs::write(dir.join("already.txt"), "x").unwrap();
        chmod(&dir.join("already.txt"), 0o600);

        let report = tighten_dir(&dir).unwrap();
        assert_eq!(
            report,
            TightenReport { dir_changed: true, files_changed: 2, skipped: 0 }
        );
        assert_eq!(mode(&dir), DIR_MODE);
        assert_eq!(mode(&dir.join("index.json")), FILE_MODE);
        assert_eq!(mode(&dir.join("1_000.txt")), FILE_MODE);

        // A second pass has nothing left to do
        assert_eq!(tighten_dir(&dir).unwrap(), TightenReport::default());
    }

    #[test]
    fn tighten_dir_does_not_follow_symlinks() {
        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "not ours").unwrap();
        chmod(&outside, 0o644);
        let dir = tmp.path().join("clipd");
        fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link.txt")).unwrap();
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        chmod(&sub, 0o755);

        let report = tighten_dir(&dir).unwrap();
        assert_eq!(report.files_changed, 0);
        assert_eq!(mode(&outside), 0o644, "symlink target must be untouched");
        assert_eq!(mode(&sub), 0o755, "not recursive");
    }
}
