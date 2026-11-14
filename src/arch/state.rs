//! Machine-local state under the Git directory, outside the committed vault:
//! `<git dir>/boundrail/` holds the vault lock, the edit journal and the claims.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::request::INPUT_MAX;
use super::{Code, Refusal};
use crate::history::{refuse_symlink, write_atomic};
use crate::sync_rules::hash;

const STATE: &str = "boundrail";
const LOCK: &str = "model.lock";
/// The journal folder: one record per operation, and the `pending` folder of edits that have
/// not finished. Records differ by their `kind`: `edit`, `claim` or `release`.
const OPERATIONS: &str = "operations";
const PENDING: &str = "pending";

/// `<git dir>/boundrail` for the vault folder `vault`, which may not exist yet.
pub(super) fn dir(vault: &Path) -> Result<PathBuf> {
    let dir = crate::git::git_dir(vault)?.join(STATE);
    refuse_symlink(&dir)?;
    Ok(dir)
}

/// The state folder, held under the vault lock for as long as this value lives.
pub(super) struct Locked {
    pub(super) dir: PathBuf,
    /// The OS lock on `model.lock`; dropping the file releases it.
    _lock: File,
}

/// Creates the state folder and takes the vault lock, refusing when another writer holds it.
pub(super) fn lock(vault: &Path) -> Result<Locked> {
    let dir = dir(vault)?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(LOCK);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Locked { dir, _lock: file }),
        Err(TryLockError::WouldBlock) => Err(Refusal::new(
            Code::VaultLocked,
            "another writer holds the model lock; retry when it finishes",
        )
        .with_details(json!({ "lock": path }))
        .into()),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// A state file parsed from JSON, `None` when it does not exist. A linked file or one larger
/// than [`INPUT_MAX`] is refused rather than read.
pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    refuse_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(INPUT_MAX + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= INPUT_MAX,
        "{} is larger than {INPUT_MAX} bytes",
        path.display()
    );
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Replaces a state file with `value` as pretty JSON, refusing one that [`read`] could not read.
pub(super) fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value)? + "\n";
    ensure!(
        text.len() as u64 <= INPUT_MAX,
        "{} would be larger than {INPUT_MAX} bytes",
        path.display()
    );
    write_atomic(path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

/// The IDs of the operations waiting in the journal of `dir`, in order: records in the pending
/// folder whose operation has no done record. A pending record left beside a done one (a crash
/// between writing one and removing the other) counts as done.
pub(super) fn pending(dir: &Path) -> Result<Vec<String>> {
    let folder = dir.join(OPERATIONS).join(PENDING);
    let mut operations = Vec::new();
    match fs::read_dir(&folder) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Some(operation) = path.file_stem().and_then(|s| s.to_str())
                    && fs::symlink_metadata(done_path(dir, operation)).is_err()
                {
                    operations.push(operation.to_owned());
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", folder.display())),
    }
    operations.sort();
    Ok(operations)
}

/// `operations/pending/<operation>.json`: an edit planned but not yet finished.
pub(super) fn pending_path(dir: &Path, operation: &str) -> PathBuf {
    dir.join(OPERATIONS)
        .join(PENDING)
        .join(format!("{operation}.json"))
}

/// `operations/<operation>.json`: a finished or abandoned operation.
fn done_path(dir: &Path, operation: &str) -> PathBuf {
    dir.join(OPERATIONS).join(format!("{operation}.json"))
}

/// The SHA-256 of `request` and the journal record of `operation` as JSON: the record in
/// `operations/` when there is one, else the pending one. Refuses `OPERATION_REUSED` when the
/// record was written for another request.
pub(super) fn recorded(
    dir: &Path,
    operation: &str,
    request: &impl Serialize,
) -> Result<(String, Option<Value>)> {
    let fingerprint = hash(&serde_json::to_vec(request)?);
    let record = match read::<Value>(&done_path(dir, operation))? {
        Some(record) => Some(record),
        None => read::<Value>(&pending_path(dir, operation))?,
    };
    match record {
        Some(record) if record["request_sha256"] != fingerprint => Err(Refusal::new(
            Code::OperationReused,
            "this operation_id already applied a different request; use a new operation_id",
        )
        .with_details(json!({ "operation_id": operation }))
        .into()),
        record => Ok((fingerprint, record)),
    }
}

/// Writes the pending record of `operation` and makes it durable before any vault file changes.
pub(super) fn write_pending(dir: &Path, operation: &str, record: &impl Serialize) -> Result<()> {
    let folder = dir.join(OPERATIONS).join(PENDING);
    for path in [&dir.join(OPERATIONS), &folder] {
        refuse_symlink(path)?;
    }
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    write(&pending_path(dir, operation), record)?;
    // The folders may be new, so their entries in the parents are flushed too.
    for path in [&folder, &dir.join(OPERATIONS), dir] {
        sync_dir(path)?;
    }
    Ok(())
}

/// Writes the record of `operation` in `operations/` durably.
pub(super) fn write_record(dir: &Path, operation: &str, record: &impl Serialize) -> Result<()> {
    let folder = dir.join(OPERATIONS);
    refuse_symlink(&folder)?;
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    write(&done_path(dir, operation), record)?;
    sync_dir(&folder)?;
    sync_dir(dir)
}

/// Writes the done record of `operation`, then removes its pending record if it has one.
pub(super) fn finish(dir: &Path, operation: &str, record: &impl Serialize) -> Result<()> {
    write_record(dir, operation, record)?;
    let path = pending_path(dir, operation);
    match fs::remove_file(&path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Whether a writer holds the vault lock of the state folder `dir` now. It never creates the
/// lock file, and the shared probe it takes is released at once.
pub(super) fn locked(dir: &Path) -> Result<bool> {
    let path = dir.join(LOCK);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// Flushes a folder's entries to disk, so a file renamed into or removed from it survives a crash.
pub(super) fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|folder| folder.sync_all())
        .with_context(|| format!("syncing {}", path.display()))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// `RECOVERY_REQUIRED` while an interrupted edit waits in the journal of `dir`.
pub(super) fn refuse_interrupted(dir: &Path) -> Result<()> {
    if let Some(operation) = pending(dir)?.into_iter().next() {
        return Err(Refusal::new(
            Code::RecoveryRequired,
            format!(
                "operation {operation} was interrupted; run `boundrail model recover` to finish it"
            ),
        )
        .with_details(json!({ "operation_id": operation }))
        .into());
    }
    Ok(())
}
