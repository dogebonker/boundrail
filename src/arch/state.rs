//! Machine-local state under the Git directory, outside the committed vault:
//! `<git dir>/boundrail/` holds the vault lock, the edit journal and the claims.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::request::INPUT_MAX;
use super::{Code, Refusal};
use crate::history::{refuse_symlink, write_atomic};

const STATE: &str = "boundrail";
const LOCK: &str = "model.lock";
/// The journal folder: one record per edit operation.
pub(super) const OPERATIONS: &str = "operations";

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
pub(super) fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value)? + "\n";
    ensure!(
        text.len() as u64 <= INPUT_MAX,
        "{} would be larger than {INPUT_MAX} bytes",
        path.display()
    );
    write_atomic(path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

/// The journal record paths in `dir`, in operation ID order.
pub(super) fn operations(dir: &Path) -> Result<Vec<PathBuf>> {
    let journal = dir.join(OPERATIONS);
    let mut paths: Vec<PathBuf> = match fs::read_dir(&journal) {
        Ok(entries) => entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension()? == "json").then_some(path)
            })
            .collect(),
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", journal.display())),
    };
    paths.sort();
    Ok(paths)
}

/// `RECOVERY_REQUIRED` while an interrupted edit waits in the journal of `dir`.
pub(super) fn refuse_interrupted(dir: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct Record {
        status: String,
        output: Output,
    }
    #[derive(Deserialize)]
    struct Output {
        operation_id: String,
    }
    for path in operations(dir)? {
        if let Some(record) = read::<Record>(&path)?
            && record.status == "pending"
        {
            let operation = record.output.operation_id;
            return Err(Refusal::new(
                Code::RecoveryRequired,
                format!(
                    "operation {operation} was interrupted; run `boundrail model recover` to finish it"
                ),
            )
            .with_details(json!({ "operation_id": operation }))
            .into());
        }
    }
    Ok(())
}
