//! Machine-local state under the Git directory, outside the committed vault:
//! `<git dir>/boundrail/` holds the vault lock, the edit journal and the claims.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::request::INPUT_MAX;
use super::{Code, Refusal};
use crate::files::{hash, refuse_symlink, write_atomic};

const STATE: &str = "boundrail";
const LOCK: &str = "model.lock";
/// The journal folder: one record per operation, and the `pending` folder of edits that have
/// not finished. Records differ by their `kind`: `edit`, `claim` or `release`.
const OPERATIONS: &str = "operations";
const PENDING: &str = "pending";
/// The status of a finished operation's record.
const DONE: &str = "done";
/// How long taking the vault lock retries before it refuses: long enough to outlast a reader's
/// brief shared-lock probe, short enough that a held lock refuses promptly.
const LOCK_WAIT: Duration = Duration::from_millis(250);
const LOCK_RETRY: Duration = Duration::from_millis(10);
/// The largest journal record. A pending record holds every file its write makes, so adopting a
/// large proposal needs far more than a request's [`INPUT_MAX`].
#[cfg(not(test))]
const RECORD_MAX: u64 = 256 * 1024 * 1024;
#[cfg(test)]
const RECORD_MAX: u64 = 2 * INPUT_MAX;

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

/// Creates the state folder and takes the vault lock, retrying for [`LOCK_WAIT`] and then
/// refusing when another writer holds it.
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
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Locked { dir, _lock: file }),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Refusal::new(
                    Code::VaultLocked,
                    "another writer holds the model lock; retry when it finishes",
                )
                .with_details(json!({ "lock": path }))
                .into());
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", path.display()));
            }
        }
    }
}

/// A state file parsed from JSON, `None` when it does not exist. A linked file or one larger
/// than [`INPUT_MAX`] is refused rather than read.
pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    read_at_most(path, INPUT_MAX)
}

/// A journal record, as [`read`] reads a state file but up to [`RECORD_MAX`] bytes.
pub(super) fn read_record<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    read_at_most(path, RECORD_MAX)
}

fn read_at_most<T: DeserializeOwned>(path: &Path, max: u64) -> Result<Option<T>> {
    refuse_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= max,
        "{} is larger than {max} bytes",
        path.display()
    );
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Replaces a state file with `value` as pretty JSON, refusing one that [`read`] could not read.
pub(super) fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let text = text(value)?;
    ensure!(
        text.len() as u64 <= INPUT_MAX,
        "{} would be larger than {INPUT_MAX} bytes",
        path.display()
    );
    write_text(path, &text)
}

fn text(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    write_atomic(path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

/// `value` as a journal record's text, refusing `MODEL_INVALID` for one that [`read_record`]
/// could not read back.
fn record_text(value: &impl Serialize) -> Result<String> {
    let text = text(value)?;
    let size = text.len() as u64;
    if size > RECORD_MAX {
        return Err(Refusal::new(
            Code::ModelInvalid,
            format!(
                "the write's journal record would be {size} bytes, more than the {RECORD_MAX} a record may hold"
            ),
        )
        .with_details(json!({ "bytes": size, "max": RECORD_MAX }))
        .into());
    }
    Ok(text)
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
    let record = match read_record::<Value>(&done_path(dir, operation))? {
        Some(record) => Some(record),
        None => read_record::<Value>(&pending_path(dir, operation))?,
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
    let text = record_text(record)?;
    let folder = dir.join(OPERATIONS).join(PENDING);
    for path in [&dir.join(OPERATIONS), &folder] {
        refuse_symlink(path)?;
    }
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    write_text(&pending_path(dir, operation), &text)?;
    // The folders may be new, so their entries in the parents are flushed too.
    for path in [&folder, &dir.join(OPERATIONS), dir] {
        sync_dir(path)?;
    }
    Ok(())
}

/// Writes the record of `operation` in `operations/` durably.
pub(super) fn write_record(dir: &Path, operation: &str, record: &impl Serialize) -> Result<()> {
    let text = record_text(record)?;
    let folder = dir.join(OPERATIONS);
    refuse_symlink(&folder)?;
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    write_text(&done_path(dir, operation), &text)?;
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

/// A claim, release or retain request's record in the journal, `operations/<operation_id>.json`,
/// so a retry returns the recorded output instead of acting twice. The record is written
/// `pending` with the output before the change and `done` after; it never blocks edits.
pub(super) enum Operation<T> {
    /// No operation ID: nothing is recorded.
    Untracked,
    /// A first attempt, or a retry of one whose change never landed.
    New {
        operation: String,
        kind: &'static str,
        fingerprint: String,
    },
    /// Already done: the recorded output.
    Done(T),
}

#[derive(Deserialize)]
struct Record<T> {
    status: String,
    output: T,
}

impl<T: Serialize + DeserializeOwned> Operation<T> {
    /// The operation `operation` of `request`, a `kind` request, in the state folder `dir`. A
    /// pending record counts as done when `landed` says its change took effect.
    pub(super) fn start(
        dir: &Path,
        operation: Option<&str>,
        kind: &'static str,
        request: &impl Serialize,
        landed: impl FnOnce(&T) -> Result<bool>,
    ) -> Result<Self> {
        let Some(operation) = operation else {
            return Ok(Self::Untracked);
        };
        let (fingerprint, record) = recorded(dir, operation, request)?;
        if let Some(record) = record {
            let record: Record<T> = serde_json::from_value(record)
                .with_context(|| format!("parsing the journal record of {operation}"))?;
            match record.status.as_str() {
                DONE => return Ok(Self::Done(record.output)),
                PENDING if landed(&record.output)? => return Ok(Self::Done(record.output)),
                PENDING => {}
                status => {
                    bail!("the journal record of {operation} has the unknown status {status}")
                }
            }
        }
        Ok(Self::New {
            operation: operation.to_owned(),
            kind,
            fingerprint,
        })
    }

    /// Records `output` as about to take effect.
    pub(super) fn pending(&self, dir: &Path, output: &T) -> Result<()> {
        self.record(dir, PENDING, output)
    }

    /// Records `output` as done.
    pub(super) fn done(&self, dir: &Path, output: &T) -> Result<()> {
        self.record(dir, DONE, output)
    }

    fn record(&self, dir: &Path, status: &str, output: &T) -> Result<()> {
        let Self::New {
            operation,
            kind,
            fingerprint,
        } = self
        else {
            return Ok(());
        };
        write_record(
            dir,
            operation,
            &json!({"request_sha256": fingerprint, "kind": kind, "status": status, "output": output}),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::TempDir;

    fn record(bytes: u64) -> Value {
        json!({ "writes": "x".repeat(usize::try_from(bytes).unwrap()) })
    }

    #[test]
    fn journal_records_hold_more_than_a_request_up_to_their_own_limit() {
        let temp = TempDir::new("state-record").unwrap();
        let dir = temp.path();
        write_pending(dir, "large", &record(INPUT_MAX + 1)).unwrap();
        let path = pending_path(dir, "large");
        assert!(read::<Value>(&path).is_err());
        assert_eq!(
            read_record::<Value>(&path).unwrap(),
            Some(record(INPUT_MAX + 1))
        );

        let error = write_pending(dir, "huge", &record(RECORD_MAX)).unwrap_err();
        let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
        assert_eq!(refusal.code, Code::ModelInvalid);
        let size = text(&record(RECORD_MAX)).unwrap().len();
        assert!(
            refusal.message.contains(&format!("{size} bytes")),
            "{refusal}"
        );
        assert!(!pending_path(dir, "huge").exists());
    }
}
