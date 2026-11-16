//! History repair and restore. `model history correct` appends a checkpoint that corrects the
//! source an earlier one recorded, so the history only moves forward and keeps who corrected
//! what, when and why. `model export --with-history` carries the checkpoint chain as data, and
//! `model import --restore` recreates it on an empty history ref with new commits whose
//! `Restored-From` trailer names the originals.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::publish::{Parts, id_counts, refused, since};
use super::{
    Checkpoint, Draft, History, MODE, annotated, apply_correction, author, blobs, commit_tree,
    correctable, files, find, fresh, history, locations, parse, put, signing, update_ref, walk,
};
use crate::arch::request::{check_doing, check_operation, check_writer};
use crate::arch::{Code, Refusal, vault};
use crate::collect::TempDir;
use crate::files::hash;
use crate::git;

/// What `model history correct` made: the correction, and the checkpoint it corrects as readers
/// now see it.
#[derive(Debug, Serialize)]
pub struct Corrected {
    pub checkpoint: Checkpoint,
    pub corrected: Checkpoint,
}

/// Appends to the history of the repository around `start` a checkpoint by `writer` (default: the
/// Git author name) saying `doing` that corrects the source of the checkpoint `which` names to
/// `source`, a full hash of a commit this repository holds. It holds that checkpoint's tree,
/// revision and proposal, `Source: <source>` and `Corrects: <its ID>`; the ref moves by
/// compare-and-swap, so no checkpoint is rewritten, and the commit is signed when
/// `commit.gpgSign` is set. Repeating `operation` with the same arguments returns the correction
/// it made. Refuses `CHECKPOINT_NOT_FOUND` or `SUBJECT_AMBIGUOUS` for `which`, `REQUEST_INVALID`
/// for a source that is no such commit or a checkpoint that is itself a correction,
/// `NOTHING_TO_RETAIN` when the checkpoint already records that source, `OPERATION_REUSED` for an
/// operation that made something else, and `HISTORY_MOVED` or `SIGNING_FAILED` as retain does.
pub fn correct(
    start: &Path,
    which: &str,
    source: &str,
    doing: &str,
    writer: Option<&str>,
    operation: Option<&str>,
) -> Result<Corrected> {
    check_doing(doing)?;
    operation.map(check_operation).transpose()?;
    let (_, root) = locations(start)?;
    let writer = match writer {
        Some(writer) => writer.to_owned(),
        None => author(&root)?,
    };
    check_writer(&writer)?;
    let read = history(&root, None)?;
    let target = find(&read.list, which)?;
    let source = held_source(&root, source)?;
    let repeated =
        operation.and_then(|op| (read.list.iter()).find(|c| c.operation_id.as_deref() == Some(op)));
    if let Some(found) = repeated {
        if found.corrects.as_deref() != Some(target.id.as_str())
            || found.writer != writer
            || found.doing != doing
            || found.source.as_deref() != Some(source.as_str())
        {
            return Err(Refusal::new(
                Code::OperationReused,
                "this operation_id already made a different checkpoint; use a new operation_id",
            )
            .with_details(json!({ "operation_id": operation, "checkpoint": found.id }))
            .into());
        }
        return Ok(Corrected {
            checkpoint: annotated(&root, found.clone())?,
            corrected: annotated(&root, target)?,
        });
    }
    if let Some(corrected) = &target.corrects {
        return Err(Refusal::new(
            Code::RequestInvalid,
            "the checkpoint is a correction; correct the checkpoint it corrects (details.corrects) instead",
        )
        .with_details(json!({ "checkpoint": target.id, "corrects": corrected }))
        .into());
    }
    if target.source.as_deref() == Some(source.as_str()) && !target.uncommitted {
        return Err(Refusal::new(
            Code::NothingToRetain,
            "the checkpoint already records this source",
        )
        .with_details(json!({ "checkpoint": target.id, "source": source }))
        .into());
    }
    let peel = format!("{}^{{tree}}", target.commit);
    let tree = git::text(&root, &["rev-parse", "--verify", "--end-of-options", &peel])?;
    let draft = Draft {
        root: root.clone(),
        tree: tree.trim().to_owned(),
        revision: target.revision.clone(),
        proposal: target.proposal.clone(),
        source: Some(source),
        uncommitted: false,
        sign: signing(&root)?,
        doing: doing.to_owned(),
        writer,
        operation: operation.map(str::to_owned),
        corrects: Some(target.id.clone()),
    };
    let checkpoint = draft.commit(&read)?;
    update_ref(&root, &checkpoint.commit, read.old(), doing)?;
    let mut corrected = target;
    apply_correction(&mut corrected, &checkpoint);
    Ok(Corrected {
        checkpoint: fresh(checkpoint),
        corrected: annotated(&root, corrected)?,
    })
}

fn held_source(root: &Path, source: &str) -> Result<String> {
    let refuse = |reason: &str| -> anyhow::Error {
        Refusal::new(Code::RequestInvalid, format!("--source {reason}"))
            .with_details(json!({ "source": source }))
            .into()
    };
    if !git::is_full_hash(source) {
        return Err(refuse("must be a full 40- or 64-hex commit hash"));
    }
    git::resolve(root, source)
        .map_err(|_| refuse("names no commit this repository holds; fetch it first"))
}

/// One checkpoint of an exported history: its original commit, its author and committer dates as
/// Git records them (`<seconds> <zone>`), its message, and each file of its tree by vault-relative
/// path with the SHA-256 of its text in [`Chain::blobs`].
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bundled {
    pub commit: String,
    pub author_date: String,
    pub committer_date: String,
    pub message: String,
    pub files: BTreeMap<String, String>,
}

/// A checkpoint history as data: its checkpoints oldest first, and the text of every file they
/// hold by its SHA-256, each text once.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub checkpoints: Vec<Bundled>,
    pub blobs: BTreeMap<String, String>,
}

/// The history of the repository around `start` as data, `None` when the history ref does not
/// exist. Refuses `MODEL_INVALID` for a first-parent commit a fetch refuses and a checkpoint tree
/// a checkpoint read refuses.
pub fn export_history(start: &Path) -> Result<Option<Chain>> {
    let (_, root) = locations(start)?;
    let History { tip, list } = history(&root, None)?;
    let Some((_, commit)) = tip else {
        return Ok(None);
    };
    let refuse = |object: &str, reason: &str| -> anyhow::Error {
        Refusal::new(
            Code::ModelInvalid,
            format!("the history holds a commit that {reason}, which a restore refuses; export without --with-history"),
        )
        .with_details(json!({ "object": object }))
        .into()
    };
    let checked = since(&root, &commit, None, &list, refuse)?;
    let mut chain = Chain::default();
    for checkpoint in checked.iter().rev() {
        let parts = Parts::read(&root, &checkpoint.commit)?;
        let mut held = BTreeMap::new();
        for (path, bytes) in blobs(&root, &files(&root, checkpoint)?)?.iter() {
            let text = std::str::from_utf8(bytes)
                .with_context(|| format!("{path} of checkpoint {} is not UTF-8", checkpoint.id))?;
            let digest = hash(bytes);
            (chain.blobs.entry(digest.clone())).or_insert_with(|| text.to_owned());
            held.insert(path.to_owned(), digest);
        }
        chain.checkpoints.push(Bundled {
            commit: checkpoint.commit.clone(),
            author_date: parts.author_date,
            committer_date: parts.committer_date,
            message: parts.message,
            files: held,
        });
    }
    Ok(Some(chain))
}

/// A history rebuilt from a [`Chain`]: its commits made and checked, unreachable until
/// [`Rebuilt::land`].
pub struct Rebuilt {
    root: PathBuf,
    tip: Option<String>,
    /// The checkpoints it holds, newest first.
    pub checkpoints: Vec<Checkpoint>,
}

impl Rebuilt {
    /// Moves the history ref, which must not exist, to the rebuilt tip; `HISTORY_MOVED` when it
    /// appeared meanwhile.
    pub fn land(&self) -> Result<()> {
        match &self.tip {
            Some(tip) => update_ref(&self.root, tip, None, "restore the history from a bundle"),
            None => Ok(()),
        }
    }
}

/// Recreates `chain` in the repository around `start`: each checkpoint, oldest first, committed
/// on the one before with its tree, message and dates and a `Restored-From` trailer naming its
/// original commit (replacing an earlier one), signed when `commit.gpgSign` is set. Nothing points
/// at the commits yet. Refuses `BUNDLE_INVALID` for a text that does not match its digest, a file
/// whose text is missing or whose path no checkpoint holds, a commit or date that is not what Git
/// writes, and a commit a fetch refuses, all checked before anything is signed, and
/// `MODEL_INVALID` for a tree a checkpoint read refuses.
pub fn rebuild(start: &Path, chain: &Chain) -> Result<Rebuilt> {
    let (_, root) = locations(start)?;
    if let Some((digest, _)) = (chain.blobs.iter()).find(|(d, t)| hash(t.as_bytes()) != **d) {
        return Err(invalid(
            "a file does not match its digest",
            json!({ "digest": digest }),
        ));
    }
    for bundled in &chain.checkpoints {
        let dates = [&bundled.author_date, &bundled.committer_date];
        if !git::is_full_hash(&bundled.commit) || !dates.iter().all(|d| is_date(d)) {
            let message = "a checkpoint's commit or dates are not what Git writes";
            return Err(invalid(message, json!({ "commit": bundled.commit })));
        }
        let stray = (bundled.files.iter()).find(|(path, digest)| {
            !chain.blobs.contains_key(*digest)
                || !(vault::is_vault_file(path) || vault::is_proposal_vault_file(path))
        });
        if let Some((path, _)) = stray {
            let message =
                "a checkpoint holds a path no checkpoint holds or a file it does not carry";
            return Err(invalid(
                message,
                json!({ "commit": bundled.commit, "path": path }),
            ));
        }
    }
    let messages: Vec<String> = (chain.checkpoints.iter())
        .map(|bundled| restored(&bundled.message, &bundled.commit))
        .collect();
    let mut parsed = Vec::with_capacity(messages.len());
    for (bundled, message) in chain.checkpoints.iter().zip(&messages).rev() {
        let checkpoint = parse(&format!("{}\x1f\x1f{message}", bundled.commit));
        let checkpoint = checkpoint.ok_or_else(|| {
            let message = "it holds a commit that is not a checkpoint";
            invalid(message, json!({ "commit": bundled.commit }))
        })?;
        parsed.push(checkpoint);
    }
    let known = id_counts(&parsed);
    for checkpoint in &parsed {
        if let Some(reason) = refused(checkpoint, &known) {
            let message = format!("it holds a commit that {reason}");
            return Err(invalid(&message, json!({ "commit": checkpoint.commit })));
        }
    }
    let objects = write_blobs(&root, &chain.blobs)?;
    let trees: Vec<String> = (chain.checkpoints.iter())
        .map(|bundled| tree_of(&root, &bundled.files, &objects))
        .collect::<Result<_>>()?;
    let last = parsed.len().saturating_sub(1);
    let held: Vec<usize> = (correctable(&parsed).into_iter())
        .filter(|&(at, target)| trees[last - at] == trees[last - target])
        .map(|(at, _)| at)
        .collect();
    let unheld =
        (parsed.iter().enumerate()).find(|(at, c)| c.corrects.is_some() && !held.contains(at));
    if let Some((_, checkpoint)) = unheld {
        let message = "it holds a correction a fetch refuses";
        return Err(invalid(message, json!({ "commit": checkpoint.commit })));
    }
    let sign = signing(&root)?;
    let mut tip: Option<String> = None;
    let mut originals = BTreeMap::new();
    for ((bundled, tree), message) in chain.checkpoints.iter().zip(&trees).zip(&messages) {
        let env = [
            ("GIT_AUTHOR_DATE", bundled.author_date.as_str()),
            ("GIT_COMMITTER_DATE", bundled.committer_date.as_str()),
        ];
        let commit = commit_tree(&root, tree, message, tip.as_deref(), sign, &env)?;
        originals.insert(commit.clone(), bundled.commit.clone());
        tip = Some(commit);
    }
    let Some(commit) = &tip else {
        return Ok(Rebuilt {
            root,
            tip,
            checkpoints: Vec::new(),
        });
    };
    let checkpoints = walk(&root, commit, None)?;
    let refuse = |object: &str, reason: &str| {
        let message = format!("it holds a commit that {reason}");
        invalid(&message, json!({ "commit": originals.get(object) }))
    };
    since(&root, commit, None, &checkpoints, refuse)?;
    for checkpoint in &checkpoints {
        files(&root, checkpoint)?;
    }
    Ok(Rebuilt {
        root,
        tip,
        checkpoints,
    })
}

fn invalid(message: &str, details: Value) -> anyhow::Error {
    Refusal::new(
        Code::BundleInvalid,
        format!("the bundle's history is not valid: {message}"),
    )
    .with_details(details)
    .into()
}

const ZONE_LEN: usize = "+hhmm".len();

fn is_date(date: &str) -> bool {
    let Some((seconds, zone)) = date.split_once(' ') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(seconds) && zone.len() == ZONE_LEN && zone.starts_with(['+', '-']) && digits(&zone[1..])
}

fn restored(message: &str, original: &str) -> String {
    let mut lines = message.lines();
    let mut text = format!("{}\n", lines.next().unwrap_or_default());
    for line in lines.filter(|line| !line.starts_with("Restored-From: ")) {
        text.push_str(line);
        text.push('\n');
    }
    text + &format!("Restored-From: {original}\n")
}

fn write_blobs(root: &Path, blobs: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    if blobs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let temp = TempDir::new("restore-blobs")?;
    let names: Vec<String> = (0..blobs.len()).map(|n| n.to_string()).collect();
    put(
        temp.path(),
        names
            .iter()
            .map(String::as_str)
            .zip(blobs.values().map(String::as_bytes)),
    )?;
    let paths: String = (names.iter())
        .map(|name| format!("{}\n", temp.path().join(name).display()))
        .collect();
    let args = ["hash-object", "-w", "--no-filters", "--stdin-paths"];
    let objects = git::run_with_input(root, &args, None, paths.into_bytes())?;
    let objects = String::from_utf8(objects).context("git hash-object output is not UTF-8")?;
    let objects: BTreeMap<String, String> = (blobs.keys().cloned())
        .zip(objects.lines().map(str::to_owned))
        .collect();
    anyhow::ensure!(
        objects.len() == blobs.len(),
        "git hash-object wrote {} objects for {} files",
        objects.len(),
        blobs.len()
    );
    Ok(objects)
}

fn tree_of(
    root: &Path,
    files: &BTreeMap<String, String>,
    objects: &BTreeMap<String, String>,
) -> Result<String> {
    let temp = TempDir::new("restore-tree")?;
    let index = temp.path().join("index");
    let mut entries = String::new();
    for (path, digest) in files {
        write!(entries, "{MODE} {}\t{path}\0", objects[digest])?;
    }
    let args = ["update-index", "-z", "--index-info"];
    git::run_with_input(root, &args, Some(&index), entries.into_bytes())?;
    let tree = git::run_with_index(root, &["write-tree"], Some(&index))?;
    Ok(String::from_utf8(tree)
        .context("git write-tree output is not UTF-8")?
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_messages_name_one_original_and_dates_are_what_git_writes() {
        let message = "drawn\n\nCheckpoint: Ckpt0001\nRestored-From: old\nWriter: w\n";
        assert_eq!(
            restored(message, "abc"),
            "drawn\n\nCheckpoint: Ckpt0001\nWriter: w\nRestored-From: abc\n"
        );
        assert!(is_date("1700000000 +0100"));
        for date in ["", "now", "1700000000", "17 +01", "x +0000", "1 0000"] {
            assert!(!is_date(date), "{date}");
        }
    }
}
