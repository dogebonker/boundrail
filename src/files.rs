//! Safe file writes and content digests shared by the model store and setup.
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};

pub(crate) fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Refuses a symbolic link at `path`, which would redirect a write outside its folder.
pub(crate) fn refuse_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!(
            "{} is a symbolic link; refusing to write through it",
            path.display()
        );
    }
    Ok(())
}

/// Replaces `path` through a temporary file and a rename, so an interrupted write never leaves
/// it truncated.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    refuse_symlink(path)?;
    // Devices and pipes (`-o /dev/null`) cannot be replaced; they are written in place.
    if fs::metadata(path).is_ok_and(|m| !m.is_file()) {
        return Ok(fs::write(path, bytes)?);
    }
    let temporary = path.with_extension(format!(
        "{}-{}.tmp",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        // Preserve private settings before their content enters the replacement file.
        match fs::metadata(path) {
            Ok(metadata) => file.set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// `root` joined with `relative`, refusing a symbolic link at any existing component.
pub fn safe_path(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "refusing symlink in installation path: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_lowercase_hex_sha256() {
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
