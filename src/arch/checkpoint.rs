//! Checkpoints: retained snapshots of the model with the reason it changed, so the model keeps a
//! history of why the architecture changed. Each checkpoint is a commit on [`HISTORY`] in the
//! source repository, never on the working branch: its tree holds the vault's recognized files at
//! their vault-relative paths, its parent is the previous checkpoint, and its message is the
//! `--doing` line followed by `Checkpoint`, `Revision`, `Proposal`, `Source`, `Uncommitted`,
//! `Writer` and `Operation` trailers (`Proposal`, `Source`, `Uncommitted` and `Operation` only
//! when there is one). Every journaled write retains one, and `model retain` retains the current
//! state with a reason of its own. A checkpoint holds the current state, or with `Proposal` one
//! proposal with the manifest and config its load reads. Commits are built with Git plumbing, so
//! the checkout's HEAD, index and working tree never change; every worktree of the repository
//! shares the one history ref. With `commit.gpgSign` set, checkpoints are signed with the user's
//! signing program and key; a write's checkpoint is committed, and signed, before the write
//! changes the vault.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::request::{INPUT_MAX, check_doing, check_operation, check_writer};
use super::revision::Revision;
use super::state::{self, Operation};
use super::vault::{self, FileWrite, Model, Scope};
use super::{Code, Refusal, ids, revision};
use crate::collect::TempDir;
use crate::files::hash;
use crate::git;

mod publish;
pub use publish::{DEFAULT_REMOTE, Destination, Published, configure, destination, fetch, push};

/// The history ref, one per repository.
pub const HISTORY: &str = "refs/boundrail/history";
/// The journal kind of a `model retain` record.
const KIND: &str = "retain";
/// The shortest commit prefix `model read --checkpoint` accepts.
const PREFIX_MIN: usize = 4;
/// The only file mode a checkpoint tree holds.
const MODE: &str = "100644";
/// The mode of an executable file, which a committed vault may hold.
const EXECUTABLE: &str = "100755";
/// What a checkpoint's `Uncommitted` change check leaves out: Boundrail's own folder, whose model
/// the checkpoint holds.
const OWN_FOLDER: &str = ":(top,exclude).boundrail";
/// How often a write's checkpoint is built again on a history tip that moved meanwhile.
const ATTEMPTS: usize = 3;

/// One checkpoint, read back from its commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 8 base62 characters, allocated among the checkpoint IDs only.
    pub id: String,
    pub commit: String,
    /// Why the model changed: the `--doing` line of `model retain`.
    pub doing: String,
    /// The revision digest of the model it holds.
    pub revision: String,
    /// The checkout's HEAD commit when it was retained; `None` on an unborn branch.
    pub source: Option<String>,
    pub writer: String,
    pub operation_id: Option<String>,
    /// The commit time, ISO 8601.
    pub time: String,
    /// The ID of the checkpoint before it.
    pub parent: Option<String>,
    /// The proposal it holds; `None` for the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// Whether the checkout had changes outside `.boundrail` that `source` lacks when it was
    /// retained. Checkpoints retained before this was recorded read `false`.
    #[serde(default)]
    pub uncommitted: bool,
    /// Whether this repository holds `source`, read when the checkpoint is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_status: Option<SourceStatus>,
    /// Whether the history remote holds it, read from refs when the checkpoint is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<Publication>,
}

/// Whether the code a checkpoint was drawn against can be read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceStatus {
    /// The source commit is in this repository and the checkout had no other change.
    Available,
    /// The source commit is here, but the checkout also had uncommitted changes it lacks.
    Uncommitted,
    /// No source commit was recorded, or this repository does not hold it.
    Unavailable,
}

impl SourceStatus {
    /// The name it serializes as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Uncommitted => "uncommitted",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Where a checkpoint stands against the history remote, judged from local refs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Publication {
    /// Neither published nor pending.
    SavedLocally,
    /// A push holding it started and has not been confirmed.
    Pending,
    /// The remote's history held it at the last push or fetch.
    Published,
}

impl Publication {
    /// The name it serializes as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SavedLocally => "saved-locally",
            Self::Pending => "pending",
            Self::Published => "published",
        }
    }
}

/// Retains the current model of the vault folder `vault` as a new checkpoint. `writer` defaults
/// to the Git author name. With `expected`, refuses `STALE_REVISION` unless the model's revision
/// is that digest. Refuses `NOTHING_TO_RETAIN` when the latest checkpoint of the current state
/// holds this revision with the same `doing`; a new reason retains the same model again.
/// Repeating `operation` with the same writer, `doing` and `expected` returns the checkpoint it
/// made, from this worktree's journal or, from another worktree, the history ref.
pub fn retain(
    vault: &Path,
    doing: &str,
    writer: Option<&str>,
    operation: Option<&str>,
    expected: Option<&str>,
) -> Result<Checkpoint> {
    check_doing(doing)?;
    operation.map(check_operation).transpose()?;
    let root = checkout(vault)?;
    let writer = match writer {
        Some(writer) => writer.to_owned(),
        None => author(&root)?,
    };
    check_writer(&writer)?;
    let state = state::lock(vault)?;
    // The tip read here is what the new commit builds on and what the ref must still hold: it
    // may be a commit without checkpoint trailers, which `history` skips, or a tag.
    let read = history(&root, None)?;
    let history = &read.list;
    let mut request = json!({ "kind": KIND, "writer": writer, "doing": doing });
    if let Some(expected) = expected {
        request["expected_revision"] = expected.into();
    }
    let journal = Operation::start(&state.dir, operation, KIND, &request, |c: &Checkpoint| {
        Ok(history.iter().any(|h| h.commit == c.commit))
    })?;
    if let Operation::Done(checkpoint) = journal {
        return annotated(&root, as_held(history, checkpoint));
    }
    if let Some(found) = retained_elsewhere(history, operation, &writer, doing, expected)? {
        journal.done(&state.dir, found)?;
        return annotated(&root, found.clone());
    }
    state::refuse_interrupted(&state.dir)?;
    let model = vault::load_dir(vault.to_path_buf())?;
    if let Some(expected) = expected
        && expected != model.revision.digest
    {
        return Err(Refusal::new(
            Code::StaleRevision,
            "the model changed since the expected revision; read it again before retaining",
        )
        .with_details(json!({ "current": model.revision.digest }))
        .into());
    }
    let snapshot = Snapshot::read(vault, None)?;
    if snapshot.revision != model.revision.digest {
        return Err(changed_meanwhile(&snapshot.revision));
    }
    let latest = history.iter().find(|c| c.proposal.is_none());
    if let Some(latest) = latest
        && latest.revision == model.revision.digest
        && latest.doing == doing
    {
        return Err(Refusal::new(
            Code::NothingToRetain,
            format!(
                "checkpoint {} already holds this model with the same --doing; give a new reason or edit the model first",
                latest.id
            ),
        )
        .with_details(json!({ "checkpoint": latest.id, "commit": latest.commit }))
        .into());
    }
    let tree = write_tree(&root, vault, &snapshot, &BTreeMap::new())?;
    let draft = Draft::new(&root, tree, &snapshot, doing, &writer, operation)?;
    let checkpoint = draft.commit(&read)?;
    journal.pending(&state.dir, &checkpoint)?;
    update_ref(&root, &checkpoint.commit, read.old(), doing)?;
    journal.done(&state.dir, &checkpoint)?;
    Ok(fresh(checkpoint))
}

/// The checkpoint of one journaled write: drafted from the files the write plans, and committed
/// on the history tip before the write once [`Prepared::build`] ran.
pub(super) struct Prepared {
    draft: Draft,
    /// The commit built before the write, with the history tip it was built on.
    built: Option<(Option<Tip>, Checkpoint)>,
}

impl Prepared {
    /// Commits the checkpoint on the history tip, signing it when `commit.gpgSign` is set, so a
    /// missing Git identity or a failing signer refuses before the write changes anything.
    pub(super) fn build(&mut self) -> Result<()> {
        let root = &self.draft.root;
        for ident in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            git::text(root, &["var", ident])?;
        }
        let read = history(root, None)?;
        let checkpoint = self.draft.commit(&read)?;
        self.built = Some((read.tip, checkpoint));
        Ok(())
    }
}

/// Drafts the checkpoint of the journaled write `operation` by `writer` saying `doing`: the
/// current state, or the proposal `proposal`, as the vault folder `vault` holds it once `writes`
/// bring the files of the write's scope to the planned revision `after`. The files the write
/// leaves are read from the vault once and nothing is written there. Refuses `MODEL_INVALID` for
/// a file larger than [`INPUT_MAX`], which no checkpoint reader accepts, and `STALE_REVISION` for
/// a vault file that no longer holds its planned digest.
pub(super) fn draft_write(
    vault: &Path,
    proposal: Option<&str>,
    writes: &[FileWrite],
    after: &Revision,
    doing: &str,
    writer: &str,
    operation: &str,
) -> Result<Prepared> {
    let root = checkout(vault)?;
    let snapshot = Snapshot::planned(vault, proposal, after)?;
    let written: BTreeMap<&str, &str> = (writes.iter())
        .filter_map(|w| Some((w.path.as_str(), w.content.as_deref()?)))
        .collect();
    let tree = write_tree(&root, vault, &snapshot, &written)?;
    let draft = Draft::new(&root, tree, &snapshot, doing, writer, Some(operation))?;
    Ok(Prepared { draft, built: None })
}

/// Retains `prepared`, the checkpoint of a journaled write, once the write reached the vault. When
/// the history already holds a checkpoint of that operation, writer, state, revision and doing
/// line, that one is returned, so finishing an interrupted write never retains it twice. The
/// commit built before the write lands while the history tip is the one it was built on; on a tip
/// that moved it is committed, and signed, again, up to three times. Refuses as `model retain`
/// does (`SIGNING_FAILED`, `HISTORY_MOVED`). The caller holds the vault lock.
pub(super) fn retain_write(prepared: Prepared) -> Result<Checkpoint> {
    let Prepared { draft, mut built } = prepared;
    let root = &draft.root;
    let mut lost = None;
    for _ in 0..ATTEMPTS {
        let read = history(root, None)?;
        if let Some(found) = read.list.iter().find(|c| draft.made(c)) {
            return annotated(root, found.clone());
        }
        let checkpoint = match &built {
            Some((tip, checkpoint)) if *tip == read.tip => checkpoint.clone(),
            _ => {
                let checkpoint = draft.commit(&read)?;
                built = Some((read.tip.clone(), checkpoint.clone()));
                checkpoint
            }
        };
        match update_ref(root, &checkpoint.commit, read.old(), &draft.doing) {
            Ok(()) => return Ok(fresh(checkpoint)),
            Err(e) if is_code(&e, Code::HistoryMoved) => lost = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(lost.context("the history ref kept moving")?)
}

/// `checkpoint`, a finished write's stored one, as the history now holds it (see [`as_held`]),
/// with its statuses read now.
pub(super) fn held(vault: &Path, checkpoint: Checkpoint) -> Result<Checkpoint> {
    let root = checkout(vault)?;
    let list = history(&root, None)?.list;
    annotated(&root, as_held(&list, checkpoint))
}

/// Whether a checkpoint of the proposal `proposal`, or of the current state, holds the
/// vault-relative `path`: a proposal's holds the manifest, the config and its own folder.
fn holds(proposal: Option<&str>, path: &str) -> bool {
    match proposal {
        None => !vault::is_proposal_path(path),
        Some(id) => {
            path == vault::MANIFEST || path == vault::CONFIG || path.starts_with(&vault::prefix(id))
        }
    }
}

/// What a checkpoint holds: each vault file it takes by vault-relative path with its SHA-256,
/// the revision of the state it holds, and the proposal it holds.
struct Snapshot {
    files: BTreeMap<String, String>,
    revision: String,
    proposal: Option<String>,
}

impl Snapshot {
    /// The current state of the vault folder `dir`, or the proposal `proposal` with the manifest
    /// and config, as the folder holds them now. The revision covers what a load of that state
    /// covers.
    fn read(dir: &Path, proposal: Option<&str>) -> Result<Self> {
        let Some(id) = proposal else {
            let files = vault::file_digests(dir, &Scope::Current)?;
            let revision = revision::of_digests(files.clone()).digest;
            return Ok(Self {
                files,
                revision,
                proposal: None,
            });
        };
        let own = vault::file_digests(dir, &Scope::Proposal(id.to_owned()))?;
        let revision = revision::of_digests(own.clone()).digest;
        let mut files = own;
        let current = vault::file_digests(dir, &Scope::Current)?;
        files.extend(
            current
                .into_iter()
                .filter(|(path, _)| holds(Some(id), path)),
        );
        Ok(Self {
            files,
            revision,
            proposal: Some(id.to_owned()),
        })
    }

    /// The current state, or the proposal `proposal` with the manifest and config, as the planned
    /// revision `after` of a write's scope holds them. A proposal write's scope lacks the manifest
    /// and config, which it never changes, so those are read from the vault folder `dir`.
    fn planned(dir: &Path, proposal: Option<&str>, after: &Revision) -> Result<Self> {
        let mut files: BTreeMap<String, String> = (after.files.iter())
            .filter(|f| holds(proposal, &f.path))
            .map(|f| (f.path.clone(), f.sha256.clone()))
            .collect();
        let Some(id) = proposal else {
            let revision = revision::of_digests(files.clone()).digest;
            return Ok(Self {
                files,
                revision,
                proposal: None,
            });
        };
        let own = vault::prefix(id);
        let revision = revision::of_digests(
            (files.iter())
                .filter(|(path, _)| path.starts_with(&own))
                .map(|(path, digest)| (path.clone(), digest.clone()))
                .collect(),
        )
        .digest;
        for path in [vault::MANIFEST, vault::CONFIG] {
            if !files.contains_key(path)
                && let Some(bytes) = read_regular(dir, path)?
            {
                files.insert(path.to_owned(), hash(&bytes));
            }
        }
        Ok(Self {
            files,
            revision,
            proposal: Some(id.to_owned()),
        })
    }
}

/// A checkpoint to commit: the tree of what it holds and what its message records, read once
/// however often it is committed again on a history tip that moved.
struct Draft {
    root: PathBuf,
    tree: String,
    revision: String,
    proposal: Option<String>,
    /// The checkout's HEAD commit; `None` on an unborn branch.
    source: Option<String>,
    uncommitted: bool,
    sign: bool,
    doing: String,
    writer: String,
    operation: Option<String>,
}

impl Draft {
    /// The draft of the checkpoint of `snapshot` whose tree is `tree`, reading the checkout's
    /// HEAD, whether it has uncommitted changes and whether to sign from the checkout `root`.
    fn new(
        root: &Path,
        tree: String,
        snapshot: &Snapshot,
        doing: &str,
        writer: &str,
        operation: Option<&str>,
    ) -> Result<Self> {
        let source = source(root)?;
        let uncommitted = source.is_some() && uncommitted(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            tree,
            revision: snapshot.revision.clone(),
            proposal: snapshot.proposal.clone(),
            source,
            uncommitted,
            sign: signing(root)?,
            doing: doing.to_owned(),
            writer: writer.to_owned(),
            operation: operation.map(str::to_owned),
        })
    }

    /// The checkpoint commit after the tip of the history `read`, with an ID none of its
    /// checkpoints holds in any case, read back. Nothing points at it yet, so a failure leaves
    /// only unreachable objects.
    fn commit(&self, read: &History) -> Result<Checkpoint> {
        let id = ids::allocate(|candidate| {
            (read.list.iter()).any(|c| c.id.eq_ignore_ascii_case(candidate))
        })?;
        let parent = read.tip.as_ref().map(|(_, commit)| commit.as_str());
        let message = self.message(&id)?;
        let commit = commit_tree(&self.root, &self.tree, &message, parent, self.sign, &[])?;
        walk(&self.root, &commit, Some(1))?
            .pop()
            .context("reading back the checkpoint commit")
    }

    fn message(&self, id: &str) -> Result<String> {
        let mut message = format!(
            "{}\n\nCheckpoint: {id}\nRevision: {}\n",
            self.doing, self.revision
        );
        if let Some(proposal) = &self.proposal {
            writeln!(message, "Proposal: {proposal}")?;
        }
        if let Some(source) = &self.source {
            writeln!(message, "Source: {source}")?;
            if self.uncommitted {
                writeln!(message, "Uncommitted: true")?;
            }
        }
        writeln!(message, "Writer: {}", self.writer)?;
        if let Some(operation) = &self.operation {
            writeln!(message, "Operation: {operation}")?;
        }
        Ok(message)
    }

    /// Whether `checkpoint` is the one this draft retains: the same operation, writer, state,
    /// revision and doing line.
    fn made(&self, checkpoint: &Checkpoint) -> bool {
        checkpoint.operation_id == self.operation
            && checkpoint.writer == self.writer
            && checkpoint.proposal == self.proposal
            && checkpoint.revision == self.revision
            && checkpoint.doing == self.doing
    }
}

/// `checkpoint`, just committed, with the statuses [`annotate`] would read without asking Git:
/// its source is the checkout's HEAD, and no push or fetch has seen it yet.
fn fresh(mut checkpoint: Checkpoint) -> Checkpoint {
    checkpoint.source_status = Some(match (&checkpoint.source, checkpoint.uncommitted) {
        (None, _) => SourceStatus::Unavailable,
        (Some(_), true) => SourceStatus::Uncommitted,
        (Some(_), false) => SourceStatus::Available,
    });
    checkpoint.publication = Some(Publication::SavedLocally);
    checkpoint
}

/// Whether `error` is a refusal with `code`.
fn is_code(error: &anyhow::Error, code: Code) -> bool {
    error
        .downcast_ref::<Refusal>()
        .is_some_and(|r| r.code == code)
}

/// `STALE_REVISION` for vault files that changed outside the lock while a checkpoint was taken.
fn changed_meanwhile(current: &str) -> anyhow::Error {
    Refusal::new(
        Code::StaleRevision,
        "vault files changed while the checkpoint was written; retry",
    )
    .with_details(json!({ "current": current }))
    .into()
}

/// The checkpoints of the repository around `start`, newest first, at most `limit`; none when
/// the history ref does not exist. The checkout needs no vault.
pub fn list(start: &Path, limit: Option<usize>) -> Result<Vec<Checkpoint>> {
    let (_, root) = locations(start)?;
    let mut list = history(&root, limit)?.list;
    annotate(&root, &mut list)?;
    Ok(list)
}

/// Whether the repository around `start` holds the history ref. A clone does not fetch it unless
/// asked to.
pub fn history_is_local(start: &Path) -> Result<bool> {
    let (_, root) = locations(start)?;
    Ok(tip(&root)?.is_some())
}

/// The checkpoint `which` names (its ID, else a unique commit prefix of at least 4 hex digits)
/// and the model it holds, the current state or its proposal, loaded from its tree without
/// touching the checkout, which needs no vault. The model's `dir` is where the checkout's vault
/// folder is.
pub fn load(start: &Path, which: &str) -> Result<(Checkpoint, Model)> {
    load_files(start, which).map(|(checkpoint, model, _)| (checkpoint, model))
}

/// [`load`], with the bytes of every file of the checkpoint's tree.
pub fn load_files(start: &Path, which: &str) -> Result<(Checkpoint, Model, Blobs)> {
    let (vault, root) = locations(start)?;
    let checkpoint = annotated(&root, find(history(&root, None)?.list, which)?)?;
    let files = blobs(&root, &files(&root, &checkpoint)?)?;
    let temp = TempDir::new("checkpoint")?;
    let dir = temp.path().join("model");
    put(&dir, files.iter())?;
    let place = format!(
        "checkpoint {} (commit {})",
        checkpoint.id, checkpoint.commit
    );
    let found = json!({ "checkpoint": checkpoint.id, "commit": checkpoint.commit });
    let loaded = match &checkpoint.proposal {
        Some(id) => vault::load_proposal(dir, id),
        None => vault::load_dir(dir),
    };
    let mut model = loaded.map_err(|error| located(error, &place, &found))?;
    model.dir = vault;
    Ok((checkpoint, model, files))
}

/// A `MODEL_INVALID` refusal of a model read from a tree, renamed after `place` instead of the
/// temporary folder it was read from, with `found` added to its details.
pub(super) fn located(
    mut error: anyhow::Error,
    place: &str,
    found: &serde_json::Value,
) -> anyhow::Error {
    if let Some(refusal) = error.downcast_mut::<Refusal>()
        && refusal.code == Code::ModelInvalid
        && let (Some(details), Some(found)) = (refusal.details.as_mut(), found.as_object())
    {
        let count = details["problems"].as_array().map_or(0, Vec::len);
        refusal.message = format!("the model in {place} has {count} problem(s)");
        for (key, value) in found {
            details[key] = value.clone();
        }
    }
    error
}

/// The current state of the model committed at `commit` in the checkout around `start`, or
/// `None` when that commit holds no manifest there. Only the files a current-state load reads
/// are read; proposals and anything else in the committed vault are left out. Refuses
/// `MODEL_INVALID` for a vault file that is not a regular file or is too large, as a checkpoint
/// read does, for a file directly in a vault folder named like a vault file but not by a
/// well-formed local ID, and for a model that does not load.
pub fn load_committed(start: &Path, commit: &str) -> Result<Option<Model>> {
    let (vault, root) = locations(start)?;
    let prefix = vault
        .strip_prefix(&root)?
        .to_string_lossy()
        .replace('\\', "/")
        + "/";
    let invalid = |path: &str, reason: &str| -> anyhow::Error {
        let shown = path.escape_debug();
        let message = format!("commit {commit} holds `{prefix}{shown}`, which {reason}");
        let details = json!({ "commit": commit, "path": path });
        Refusal::new(Code::ModelInvalid, message)
            .with_details(details)
            .into()
    };
    let listing = git::run(&root, &["ls-tree", "-r", "-l", "-z", commit, "--", &prefix])?;
    let mut files = Vec::new();
    for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let (meta, path) = tree_entry(entry)?;
        let path = path.strip_prefix(&prefix).unwrap_or(path);
        if vault::is_misnamed(path) {
            return Err(invalid(path, "is not named by a well-formed local ID"));
        }
        if !vault::is_vault_file(path) {
            continue;
        }
        let [MODE | EXECUTABLE, "blob", object, size] = meta.as_slice() else {
            return Err(invalid(path, "is not a regular file"));
        };
        let size: u64 = size.parse().context("parsing a git ls-tree size")?;
        if size > INPUT_MAX {
            return Err(invalid(path, "is too large"));
        }
        let size = usize::try_from(size)?;
        files.push((path.to_owned(), (*object).to_owned(), size));
    }
    if !files.iter().any(|(path, _, _)| path == "model.json") {
        return Ok(None);
    }
    let temp = TempDir::new("committed")?;
    let dir = temp.path().join("model");
    put(&dir, blobs(&root, &files)?.iter())?;
    let place = format!("commit {commit}");
    let found = json!({ "commit": commit });
    let mut model = vault::load_dir(dir).map_err(|error| located(error, &place, &found))?;
    model.dir = vault;
    Ok(Some(model))
}

/// The blobs of a tree, read back to back: each file's vault-relative path and size, and their
/// bytes in that order.
pub struct Blobs {
    files: Vec<(String, usize)>,
    bytes: Vec<u8>,
}

impl Blobs {
    /// Each file's vault-relative path and bytes.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        let mut rest = self.bytes.as_slice();
        self.files.iter().map(move |(path, size)| {
            let (content, tail) = rest.split_at(*size);
            rest = tail;
            (path.as_str(), content)
        })
    }
}

/// The blobs `files` lists as (vault-relative path, object, size). One `git show` prints each
/// blob's bytes back to back, unconverted, so one call reads them all.
fn blobs(root: &Path, files: &[(String, String, usize)]) -> Result<Blobs> {
    let mut args = vec!["show", "--no-textconv", "--no-color"];
    args.extend(files.iter().map(|(_, object, _)| object.as_str()));
    let bytes = git::run(root, &args)?;
    let listed: usize = files.iter().map(|(_, _, size)| size).sum();
    ensure!(
        bytes.len() == listed,
        "git show returned {} bytes, not the {listed} the tree lists",
        bytes.len()
    );
    let files = (files.iter())
        .map(|(path, _, size)| (path.clone(), *size))
        .collect();
    Ok(Blobs { files, bytes })
}

/// Writes `files`, (vault-relative path, bytes), under the fresh folder `dir`, which a load then
/// reads. The paths are vault file paths, so none leaves `dir`.
pub(super) fn put<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<()> {
    for (path, content) in files {
        let file = dir.join(path);
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        }
        fs::write(&file, content).with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(())
}

/// One `git ls-tree -l -z` entry, `<mode> <type> <object> <padded size>\t<path>`, as its
/// fields before the tab and its path.
fn tree_entry(entry: &[u8]) -> Result<(Vec<&str>, &str)> {
    let entry = std::str::from_utf8(entry).context("a tree path is not UTF-8")?;
    let (meta, path) = entry.split_once('\t').context("parsing git ls-tree")?;
    Ok((meta.split_whitespace().collect(), path))
}

/// The vault folder of the checkout around `start`, whether or not it exists, and the checkout.
fn locations(start: &Path) -> Result<(PathBuf, PathBuf)> {
    let vault = vault::path(start)?;
    let root = checkout(&vault)?;
    Ok((vault, root))
}

/// The checkout of a vault folder, `<checkout>/.boundrail/model`.
pub fn checkout(vault: &Path) -> Result<PathBuf> {
    vault
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("the model vault has no checkout around it")
}

/// The checkpoint an earlier attempt of `operation` from another worktree left on the shared
/// history ref, since each worktree keeps its own journal. Refuses `OPERATION_REUSED` when it
/// was made for another writer or reason, from another revision than `expected`, or holds a
/// proposal.
fn retained_elsewhere<'a>(
    history: &'a [Checkpoint],
    operation: Option<&str>,
    writer: &str,
    doing: &str,
    expected: Option<&str>,
) -> Result<Option<&'a Checkpoint>> {
    let Some(found) = history
        .iter()
        .find(|c| operation.is_some() && c.operation_id.as_deref() == operation)
    else {
        return Ok(None);
    };
    if found.proposal.is_some()
        || found.writer != writer
        || found.doing != doing
        || expected.is_some_and(|e| e != found.revision)
    {
        return Err(Refusal::new(
            Code::OperationReused,
            "this operation_id already retained a different checkpoint; use a new operation_id",
        )
        .with_details(json!({ "operation_id": operation, "checkpoint": found.id }))
        .into());
    }
    Ok(Some(found))
}

/// `checkpoint`, a journaled retain's or write's output, as `history` now holds it: a fetch that replayed
/// it moved it to another commit with the same ID, operation and revision. Any other match is
/// not this checkpoint, so the recorded one stands.
fn as_held(history: &[Checkpoint], checkpoint: Checkpoint) -> Checkpoint {
    let held = history.iter().find(|h| {
        h.id == checkpoint.id
            && h.operation_id == checkpoint.operation_id
            && h.revision == checkpoint.revision
    });
    held.cloned().unwrap_or(checkpoint)
}

fn find(history: Vec<Checkpoint>, which: &str) -> Result<Checkpoint> {
    let prefix = which.to_ascii_lowercase();
    let hex = prefix.len() >= PREFIX_MIN && prefix.bytes().all(|b| b.is_ascii_hexdigit());
    let by_id: Vec<Checkpoint> = history
        .iter()
        .filter(|c| c.id.eq_ignore_ascii_case(which))
        .cloned()
        .collect();
    let mut matches: Vec<Checkpoint> = if by_id.is_empty() && hex {
        history
            .into_iter()
            .filter(|c| c.commit.starts_with(&prefix))
            .collect()
    } else {
        by_id
    };
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(Refusal::new(
            Code::CheckpointNotFound,
            format!("no checkpoint has the ID or commit `{which}`"),
        )
        .with_details(json!({ "checkpoint": which }))
        .into()),
        n => {
            let listed: Vec<_> = matches
                .iter()
                .map(|c| json!({ "id": c.id, "commit": c.commit }))
                .collect();
            Err(Refusal::new(
                Code::SubjectAmbiguous,
                format!("{n} checkpoints match `{which}`; name one by ID or a longer commit"),
            )
            .with_details(json!({ "matches": listed }))
            .into())
        }
    }
}

/// The files of a checkpoint's tree as (vault-relative path, object, size). Refuses
/// `MODEL_INVALID` for a tree that holds anything a retain never writes: a path that is not a
/// vault file (for a proposal's checkpoint, not the manifest, the config or a file of that
/// proposal's folder), another mode or object type, a file larger than [`INPUT_MAX`], no
/// manifest, or for a proposal's checkpoint no `proposal.json`.
fn files(root: &Path, checkpoint: &Checkpoint) -> Result<Vec<(String, String, usize)>> {
    let invalid = |path: &str, reason: &str| -> anyhow::Error {
        Refusal::new(
            Code::ModelInvalid,
            format!(
                "checkpoint {} (commit {}) holds `{}`, which {reason}",
                checkpoint.id,
                checkpoint.commit,
                path.escape_debug()
            ),
        )
        .with_details(
            json!({ "checkpoint": checkpoint.id, "commit": checkpoint.commit, "path": path }),
        )
        .into()
    };
    let listing = git::run(root, &["ls-tree", "-r", "-l", "-z", &checkpoint.commit])?;
    let mut files = Vec::new();
    for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let (meta, path) = tree_entry(entry)?;
        let [mode, kind, object, size] = meta.as_slice() else {
            return Err(invalid(path, "is not a file"));
        };
        let held = match &checkpoint.proposal {
            Some(id) => match path.strip_prefix(&vault::prefix(id)) {
                Some(rest) => vault::is_proposal_file(rest),
                None => path == vault::MANIFEST || path == vault::CONFIG,
            },
            None => vault::is_vault_file(path),
        };
        if !held {
            return Err(invalid(path, "is not a vault file"));
        }
        if *mode != MODE || *kind != "blob" {
            return Err(invalid(path, "is not a regular file"));
        }
        let size: u64 = size.parse().context("parsing a git ls-tree size")?;
        if size > INPUT_MAX {
            return Err(invalid(path, "is too large"));
        }
        files.push((
            path.to_owned(),
            (*object).to_owned(),
            usize::try_from(size)?,
        ));
    }
    let required = (checkpoint.proposal.as_deref().map(vault::proposal_file))
        .into_iter()
        .chain([vault::MANIFEST.to_owned()]);
    for path in required {
        if !files.iter().any(|(held, _, _)| *held == path) {
            return Err(invalid(&path, "is missing"));
        }
    }
    Ok(files)
}

/// The name of the Git author identity, which `commit-tree` needs anyway.
pub(super) fn author(root: &Path) -> Result<String> {
    let ident = git::text(root, &["var", "GIT_AUTHOR_IDENT"])?;
    let name = ident.split(" <").next().unwrap_or_default().trim();
    ensure!(!name.is_empty(), "the Git author identity has no name");
    Ok(name.to_owned())
}

/// The checkout's HEAD commit, `None` on an unborn branch.
fn source(root: &Path) -> Result<Option<String>> {
    let error = match git::resolve(root, "HEAD") {
        Ok(commit) => return Ok(Some(commit)),
        Err(error) => error,
    };
    // Unborn: HEAD names a branch that does not exist yet. A detached or broken HEAD is an error.
    match git::text(root, &["symbolic-ref", "-q", "HEAD"]) {
        Ok(branch) if ref_target(root, branch.trim())?.is_none() => Ok(None),
        _ => Err(error),
    }
}

/// Whether the checkout has changes outside `.boundrail`, tracked or untracked, that HEAD
/// lacks.
fn uncommitted(root: &Path) -> Result<bool> {
    let args = ["status", "--porcelain", "-z", "--", ":/", OWN_FOLDER];
    Ok(!git::run(root, &args)?.is_empty())
}

/// Whether the user set `commit.gpgSign`, so checkpoints are signed.
fn signing(root: &Path) -> Result<bool> {
    let args = [
        "config",
        "--type=bool",
        "--default=false",
        "--get",
        "commit.gpgSign",
    ];
    Ok(git::text(root, &args)?.trim() == "true")
}

/// The commit of `tree` with `message` after the commit `parent`, with `env` set for Git, signed
/// when `sign` says so; any failure of a signed commit refuses `SIGNING_FAILED`.
fn commit_tree(
    root: &Path,
    tree: &str,
    message: &str,
    parent: Option<&str>,
    sign: bool,
    env: &[(&str, &str)],
) -> Result<String> {
    let mut args = vec!["commit-tree", tree, "-m", message];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    if sign {
        args.push("-S");
    }
    match git::run_with_env(root, &args, env) {
        Ok(commit) => Ok(String::from_utf8_lossy(&commit).trim().to_owned()),
        // Git's wording is localized, so any failure of a signed commit counts as the signer's.
        Err(error) if sign => {
            let text = format!("{error:#}");
            let stderr = text.strip_prefix("git commit-tree: ").unwrap_or(&text);
            Err(Refusal::new(
                Code::SigningFailed,
                "signing or writing the checkpoint failed; unlock the signing key, or set commit.gpgSign=false for this repository",
            )
            .with_details(json!({ "stderr": stderr.lines().next().unwrap_or_default() }))
            .into())
        }
        Err(error) => Err(error),
    }
}

/// The tree of the files of `snapshot`, each `written` one with the content given and every other
/// read from the vault folder `dir`, written to the object store through a disposable index; the
/// vault is never written. Refuses `MODEL_INVALID` for a file larger than [`INPUT_MAX`], which no
/// checkpoint reads back, and `STALE_REVISION` for a file read from `dir` that does not hold the
/// digest `snapshot` gives it, as when it changed outside the lock.
fn write_tree(
    root: &Path,
    dir: &Path,
    snapshot: &Snapshot,
    written: &BTreeMap<&str, &str>,
) -> Result<String> {
    let temp = TempDir::new("checkpoint-tree")?;
    let mut copies = Vec::with_capacity(snapshot.files.len());
    for (path, digest) in &snapshot.files {
        let bytes = match written.get(path.as_str()) {
            Some(content) => {
                check_size(path, u64::try_from(content.len())?)?;
                Some(Cow::Borrowed(content.as_bytes()))
            }
            None => read_regular(dir, path)?.map(Cow::Owned),
        };
        let Some(bytes) = bytes.filter(|bytes| hash(bytes) == *digest) else {
            let now = Snapshot::read(dir, snapshot.proposal.as_deref())?;
            return Err(changed_meanwhile(&now.revision));
        };
        put(temp.path(), [(path.as_str(), &*bytes)])?;
        let copy = temp.path().join(path);
        copies.push(
            copy.to_str()
                .context("the temporary folder is not UTF-8")?
                .to_owned(),
        );
    }
    let mut args = vec!["hash-object", "-w", "--no-filters", "--"];
    args.extend(copies.iter().map(String::as_str));
    let objects = git::text(root, &args)?;
    let entries: Vec<String> = objects
        .lines()
        .zip(snapshot.files.keys())
        .map(|(object, path)| format!("{MODE},{object},{path}"))
        .collect();
    ensure!(
        entries.len() == snapshot.files.len(),
        "git hash-object wrote {} objects for {} files",
        entries.len(),
        snapshot.files.len()
    );
    let index = temp.path().join("index");
    let mut args = vec!["update-index", "--add"];
    for entry in &entries {
        args.extend(["--cacheinfo", entry.as_str()]);
    }
    git::run_with_index(root, &args, Some(&index))?;
    let tree = git::run_with_index(root, &["write-tree"], Some(&index))?;
    Ok(String::from_utf8(tree)
        .context("git write-tree output is not UTF-8")?
        .trim()
        .to_owned())
}

/// The bytes of the vault file `path` of the vault folder `dir`, `None` when it is missing or not
/// a regular file. Refuses `MODEL_INVALID` for one larger than [`INPUT_MAX`] before reading it.
fn read_regular(dir: &Path, path: &str) -> Result<Option<Vec<u8>>> {
    let file = dir.join(path);
    match fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_file() => {
            check_size(path, meta.len())?;
            let bytes = fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            Ok(Some(bytes))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// `MODEL_INVALID` for the vault file `path` of `size` bytes when it is larger than
/// [`INPUT_MAX`], more than a checkpoint reader accepts.
fn check_size(path: &str, size: u64) -> Result<()> {
    if size <= INPUT_MAX {
        return Ok(());
    }
    Err(Refusal::new(
        Code::ModelInvalid,
        format!("`{path}` is larger than {INPUT_MAX} bytes, more than a checkpoint holds"),
    )
    .with_details(json!({ "path": path }))
    .into())
}

/// The object the ref `name` names, `None` when it does not exist. Only the ref itself counts,
/// not refs below it.
fn ref_target(root: &Path, name: &str) -> Result<Option<String>> {
    let out = git::text(
        root,
        &["for-each-ref", "--format=%(refname) %(objectname)", name],
    )?;
    Ok(out.lines().find_map(|line| {
        let (refname, object) = line.split_once(' ')?;
        (refname == name).then(|| object.to_owned())
    }))
}

fn tip(root: &Path) -> Result<Option<String>> {
    ref_target(root, HISTORY)
}

/// Moves the history ref from `old` (`None`: absent) to `new` only if it still holds `old`, with
/// a reflog entry so a forced move or deletion can be undone. Refuses `HISTORY_MOVED` when
/// another writer moved the ref meanwhile or holds its lock.
fn update_ref(root: &Path, new: &str, old: Option<&str>, doing: &str) -> Result<()> {
    let args = [
        "update-ref",
        "--create-reflog",
        "-m",
        doing,
        HISTORY,
        new,
        old.unwrap_or(""),
    ];
    let Err(error) = git::run(root, &args) else {
        return Ok(());
    };
    let current = tip(root)?;
    // The move landed although Git reported a failure.
    if current.as_deref() == Some(new) {
        return Ok(());
    }
    let message = if current.as_deref() != old {
        "another checkpoint landed while this one was written; retry"
    } else if format!("{error:#}").contains("cannot lock ref") {
        "another Git process holds the history ref's lock; retry when it finishes"
    } else {
        return Err(error);
    };
    Err(Refusal::new(Code::HistoryMoved, message)
        .with_details(json!({ "expected": old, "current": current }))
        .into())
}

/// What the history ref holds and the commit it peels to: the compare-and-swap's old value and
/// the next checkpoint's parent, which differ when the ref holds a tag.
type Tip = (String, String);

/// The history ref's [`Tip`] and the checkpoints from it, newest first.
struct History {
    /// `None` when the ref does not exist.
    tip: Option<Tip>,
    list: Vec<Checkpoint>,
}

impl History {
    /// What the ref holds, the compare-and-swap's old value; `None` when it does not exist.
    fn old(&self) -> Option<&str> {
        self.tip.as_ref().map(|(object, _)| object.as_str())
    }
}

/// The history ref's [`Tip`] and the checkpoints from it, newest first, at most `limit`.
fn history(root: &Path, limit: Option<usize>) -> Result<History> {
    let Some(object) = tip(root)? else {
        return Ok(History {
            tip: None,
            list: Vec::new(),
        });
    };
    let peel = format!("{object}^{{commit}}");
    let commit = git::text(root, &["rev-parse", "--verify", "--quiet", &peel])?
        .trim()
        .to_owned();
    let list = walk(root, &commit, limit)?;
    Ok(History {
        tip: Some((object, commit)),
        list,
    })
}

/// The checkpoints from `commit` back along first parents, newest first, at most `limit`. A
/// commit without a `Checkpoint` and `Revision` trailer is not a checkpoint and is skipped, so
/// parents are assigned over the whole walk before the limit applies.
// ponytail: it walks the whole history even with a limit; page the log if it reaches thousands.
fn walk(root: &Path, commit: &str, limit: Option<usize>) -> Result<Vec<Checkpoint>> {
    let args = [
        "log",
        "--first-parent",
        "--no-show-signature",
        "--format=%H%x1f%cI%x1f%B%x1e",
        commit,
    ];
    let log = git::text(root, &args)?;
    let mut list: Vec<Checkpoint> = log.split('\x1e').filter_map(parse).collect();
    let mut parent = None;
    for checkpoint in list.iter_mut().rev() {
        checkpoint.parent = parent.replace(checkpoint.id.clone());
    }
    if let Some(limit) = limit {
        list.truncate(limit);
    }
    Ok(list)
}

/// One `git log` entry of [`walk`]: commit, time and message, the message's first line the
/// `--doing` and its other lines `Key: value` trailers. A `Proposal` that is not a local ID, or
/// an `Uncommitted` other than `true` after a `Source`, is not one a retain writes, so the commit
/// is not a checkpoint.
fn parse(entry: &str) -> Option<Checkpoint> {
    let mut fields = entry.trim_start_matches('\n').splitn(3, '\x1f');
    let (commit, time, message) = (fields.next()?, fields.next()?, fields.next()?);
    let mut lines = message.lines();
    let doing = lines.next()?.to_owned();
    let trailers: BTreeMap<&str, &str> = lines.filter_map(|l| l.split_once(": ")).collect();
    let value = |key: &str| trailers.get(key).map(|v| (*v).to_owned());
    let proposal = value("Proposal");
    if proposal.as_deref().is_some_and(|p| !ids::is_local(p)) {
        return None;
    }
    let uncommitted = match trailers.get("Uncommitted") {
        None => false,
        Some(&"true") if trailers.contains_key("Source") => true,
        Some(_) => return None,
    };
    Some(Checkpoint {
        id: value("Checkpoint")?,
        commit: commit.to_owned(),
        doing,
        revision: value("Revision")?,
        source: value("Source"),
        writer: value("Writer").unwrap_or_default(),
        operation_id: value("Operation"),
        time: time.to_owned(),
        parent: None,
        proposal,
        uncommitted,
        source_status: None,
        publication: None,
    })
}

/// `checkpoint` with [`annotate`]'s statuses.
fn annotated(root: &Path, mut checkpoint: Checkpoint) -> Result<Checkpoint> {
    annotate(root, std::slice::from_mut(&mut checkpoint))?;
    Ok(checkpoint)
}

/// Sets each checkpoint's `source_status` and `publication` from what this repository holds now,
/// without the network. The source is available when the repository holds its commit, unless
/// the checkpoint was retained with uncommitted changes. A checkpoint is published when the ref
/// the last push or fetch left for the history remote reaches it, pending when the ref a push
/// leaves until the remote confirms it reaches it, and saved locally otherwise.
fn annotate(root: &Path, list: &mut [Checkpoint]) -> Result<()> {
    let sources: Vec<&str> = (list.iter())
        .filter_map(|c| c.source.as_deref())
        .filter(|s| git::is_full_hash(s))
        .collect();
    let present = if sources.is_empty() {
        BTreeSet::new()
    } else {
        reachable(root, &["--no-walk"], &sources)?
    };
    let remote = publish::destination_of(root)?.remote;
    let published = publish::published_ref(&remote);
    let published_set = reachable(root, &[], &[&published])?;
    let pushing = publish::pushing_ref(&remote);
    let pending_set = reachable(root, &[], &[&pushing, &format!("^{published}")])?;
    for checkpoint in list {
        let held = checkpoint
            .source
            .as_ref()
            .is_some_and(|s| present.contains(s));
        checkpoint.source_status = Some(match (held, checkpoint.uncommitted) {
            (false, _) => SourceStatus::Unavailable,
            (true, true) => SourceStatus::Uncommitted,
            (true, false) => SourceStatus::Available,
        });
        checkpoint.publication = Some(if published_set.contains(&checkpoint.commit) {
            Publication::Published
        } else if pending_set.contains(&checkpoint.commit) {
            Publication::Pending
        } else {
            Publication::SavedLocally
        });
    }
    Ok(())
}

/// The commits `git rev-list` with `options` lists for `revisions`, a missing object or ref
/// counting as none.
fn reachable(root: &Path, options: &[&str], revisions: &[&str]) -> Result<BTreeSet<String>> {
    let mut all = vec!["rev-list", "--ignore-missing"];
    all.extend(options);
    all.push("--end-of-options");
    all.extend(revisions);
    let out = git::text(root, &all)?;
    Ok(out.lines().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, args: &[&str]) -> String {
        git::text(root, args).unwrap().trim().to_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_committed_vault_file_must_be_a_regular_file() {
        let temp = TempDir::new("committed-link").unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        run_git(&root, &["init", "-q"]);
        let modules = root.join(".boundrail/model/modules");
        fs::create_dir_all(&modules).unwrap();
        fs::write(root.join(".boundrail/model/model.json"), "{}").unwrap();
        let link = modules.join("Link0001.module.json");
        std::os::unix::fs::symlink("/etc/hosts", link).unwrap();
        run_git(&root, &["add", "-A"]);
        let tree = run_git(&root, &["write-tree"]);
        let commit = run_git(&root, &["commit-tree", &tree, "-m", "linked"]);

        let error = load_committed(&root, &commit).unwrap_err();
        let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
        assert_eq!(refusal.code, Code::ModelInvalid);
        assert!(
            refusal.message.contains("is not a regular file"),
            "{}",
            refusal.message
        );
        assert!(refusal.message.contains(&commit), "{}", refusal.message);
    }

    #[test]
    fn a_committed_vault_file_must_be_named_by_a_local_id() {
        let temp = TempDir::new("committed-name").unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        run_git(&root, &["init", "-q"]);
        let modules = root.join(".boundrail/model/modules");
        fs::create_dir_all(modules.join("nested")).unwrap();
        fs::write(root.join(".boundrail/model/model.json"), "{}").unwrap();
        fs::write(modules.join("nested/Nested01.module.json"), "{}").unwrap();
        fs::write(modules.join("bad\u{1b}[31mname.module.json"), "{}").unwrap();
        run_git(&root, &["add", "-A"]);
        let tree = run_git(&root, &["write-tree"]);
        let commit = run_git(&root, &["commit-tree", &tree, "-m", "misnamed"]);

        let error = load_committed(&root, &commit).unwrap_err();
        let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
        assert_eq!(refusal.code, Code::ModelInvalid);
        let details = refusal.details.as_ref().unwrap();
        assert_eq!(details["path"], "modules/bad\u{1b}[31mname.module.json");
        assert!(!refusal.message.contains('\u{1b}'), "{}", refusal.message);
    }

    #[test]
    fn the_history_ref_moves_only_from_the_commit_it_holds() {
        let temp = TempDir::new("checkpoint-cas").unwrap();
        let root = temp.path();
        run_git(root, &["init", "-q"]);
        for (key, value) in [
            ("user.name", "tester"),
            ("user.email", "tester@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            run_git(root, &["config", key, value]);
        }
        let tree = run_git(root, &["write-tree"]);
        let first = run_git(root, &["commit-tree", &tree, "-m", "first"]);
        let second = run_git(root, &["commit-tree", &tree, "-p", &first, "-m", "second"]);

        update_ref(root, &first, None, "first").unwrap();
        assert_eq!(tip(root).unwrap().as_deref(), Some(first.as_str()));
        for old in [None, Some(second.as_str())] {
            let error = update_ref(root, &second, old, "second").unwrap_err();
            let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
            assert_eq!(refusal.code, Code::HistoryMoved);
            assert_eq!(tip(root).unwrap().as_deref(), Some(first.as_str()));
        }
        update_ref(root, &second, Some(&first), "second").unwrap();
        assert_eq!(tip(root).unwrap().as_deref(), Some(second.as_str()));
        // A move that already landed counts as done, though Git refuses the stale old value.
        update_ref(root, &second, Some(&first), "again").unwrap();
        let reflog = run_git(root, &["reflog", "--format=%gs", HISTORY]);
        assert_eq!(reflog, "second\nfirst");
    }

    #[test]
    fn log_entries_parse_into_checkpoints_and_skip_other_commits() {
        let entry = "\nabc123\x1f2026-11-14T10:00:00+00:00\x1fsplit billing\n\nCheckpoint: Ckpt0001\nRevision: r1\nWriter: a: b\nOperation: op-1\n";
        let checkpoint = parse(entry).unwrap();
        assert_eq!(checkpoint.id, "Ckpt0001");
        assert_eq!(checkpoint.doing, "split billing");
        assert_eq!(checkpoint.source, None);
        assert_eq!(checkpoint.writer, "a: b");
        assert_eq!(checkpoint.operation_id.as_deref(), Some("op-1"));
        assert_eq!((checkpoint.proposal, checkpoint.uncommitted), (None, false));
        assert!(parse("abc\x1ft\x1fa plain commit\n").is_none());
        let head = "\nabc\x1ft\x1fdrawn\n\nCheckpoint: Ckpt0002\nRevision: r2\nWriter: w\n";
        let checkpoint = parse(&format!(
            "{head}Proposal: Next0001\nSource: s\nUncommitted: true\n"
        ));
        let checkpoint = checkpoint.unwrap();
        assert_eq!(checkpoint.proposal.as_deref(), Some("Next0001"));
        assert!(checkpoint.uncommitted);
        for forged in [
            "Proposal: ../up\n",
            "Uncommitted: true\n",
            "Source: s\nUncommitted: 1\n",
        ] {
            assert!(parse(&format!("{head}{forged}")).is_none(), "{forged}");
        }
    }
}
