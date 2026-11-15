//! Publishing the history: `model history push` and `model history fetch` move
//! [`HISTORY`](super::HISTORY) to or from the same ref on a configured remote, so a clone or CI job
//! that fetches it resolves checkpoint readings. A push only fast-forwards the remote; a fetch
//! fast-forwards the local ref, or replays the local checkpoints onto the fetched history when
//! the two diverged. Both are network operations the user starts explicitly; Git runs them with
//! the hardening of `crate::git`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use super::{
    Checkpoint, HISTORY, commit_tree, history, locations, parse, ref_target, signing, tip,
    update_ref, walk,
};
use crate::arch::request::{check_doing, check_operation, check_writer};
use crate::arch::{Code, Refusal, ids};
use crate::git;

/// The remote `--remote` names when absent.
pub const DEFAULT_REMOTE: &str = "origin";
/// The namespace of the per-run refs a remote's history lands on before it is checked.
const FETCHING: &str = "refs/boundrail/fetching";
/// How often a fetch recomputes after a checkpoint landed locally while it ran.
const ATTEMPTS: usize = 3;

/// What a push or fetch moved.
#[derive(Debug, Serialize)]
pub struct Published {
    pub remote: String,
    #[serde(rename = "ref")]
    pub reference: &'static str,
    /// The object the updated side's ref held before, `None` when it was absent.
    pub before: Option<String>,
    /// The object it holds after.
    pub after: Option<String>,
    /// Whether the ref moved.
    pub updated: bool,
    /// The IDs of the checkpoints it gained, newest first.
    pub checkpoints: Vec<String>,
    /// The IDs of the local checkpoints a fetch replayed onto the fetched history, newest first.
    pub replayed: Vec<String>,
}

/// Pushes the history ref to `remote`, a configured remote name, when the remote's ref is absent
/// or an ancestor of the local one, and every first-parent commit it adds is one a fetch takes
/// (`MODEL_INVALID` otherwise). Refuses `HISTORY_DIVERGED` when the remote holds checkpoints this
/// repository lacks, saying whether the remote is only ahead or the two diverged. The push
/// leases the remote tip it checked against, so it moves only from that tip.
pub fn push(start: &Path, remote: &str) -> Result<Published> {
    let (_, root) = locations(start)?;
    check_remote(&root, remote)?;
    let (local, list) = history(&root, None)?;
    let theirs = remote_tip(&root, remote)?;
    let Some((object, commit)) = local else {
        return Ok(unchanged(remote, theirs));
    };
    if theirs.as_deref() == Some(object.as_str()) {
        return Ok(unchanged(remote, theirs));
    }
    let known = id_counts(&list);
    if !holds(&root, &commit, theirs.as_deref())? {
        return Err(refused_push(&root, remote, (&object, &commit), &known)?);
    }
    let base = theirs.as_deref().and_then(|theirs| peel(&root, theirs));
    let refuse = |object: &str, reason: &str| unpublishable(remote, object, reason);
    since(&root, &commit, base.as_deref(), &known, refuse)?;
    // The object checked above is what lands, even if the local ref moves meanwhile.
    let refspec = refspec(&object, HISTORY);
    let lease = format!(
        "--force-with-lease={HISTORY}:{}",
        theirs.as_deref().unwrap_or("")
    );
    let args = [
        "push",
        "--no-follow-tags",
        "--recurse-submodules=no",
        &lease,
        "--end-of-options",
        remote,
        &refspec,
    ];
    if let Err(error) = git::run(&root, &args) {
        // The remote moved between the check and the push.
        let now = remote_tip(&root, remote)?;
        if !holds(&root, &commit, now.as_deref())? {
            return Err(refused_push(&root, remote, (&object, &commit), &known)?);
        }
        return Err(error);
    }
    let new = list
        .iter()
        .take_while(|c| theirs.as_ref() != Some(&c.commit));
    Ok(Published {
        checkpoints: new.map(|c| c.id.clone()).collect(),
        ..moved(remote, theirs, object)
    })
}

/// The `HISTORY_DIVERGED` refusal of a push of the local history, the ref holding `object` that
/// peels to `commit` with the checkpoint IDs `known`, judged against the remote's history fetched
/// onto a per-run ref: with no local checkpoints the remote lacks, it is only ahead.
fn refused_push(
    root: &Path,
    remote: &str,
    (object, commit): (&str, &str),
    known: &BTreeMap<String, usize>,
) -> Result<anyhow::Error> {
    fetched(root, remote, |theirs, fetched_commit| {
        if !git::have_merge_base(root, commit, fetched_commit)? {
            return Ok(unrelated(remote));
        }
        let local = since(root, commit, Some(fetched_commit), known, |o, r| {
            unpublishable(remote, o, r)
        })?;
        let local_only: Vec<&str> = local.iter().map(|c| c.id.as_str()).collect();
        Ok(diverged(remote, theirs, object, &local_only))
    })
}

/// Fetches `remote`'s history ref into the local one. A local ref that is absent or an ancestor
/// of the remote's fast-forwards; one already holding it stays; one that diverged from it gets its
/// local checkpoints replayed onto the remote's. The remote's ref lands on a per-run ref first
/// and must be a commit whose first-parent commits this repository lacks are checkpoints `model
/// retain` could have written (`MODEL_INVALID`); the local ref then moves by compare-and-swap.
pub fn fetch(start: &Path, remote: &str) -> Result<Published> {
    let (_, root) = locations(start)?;
    check_remote(&root, remote)?;
    if remote_tip(&root, remote)?.is_none() {
        return Ok(unchanged(remote, tip(&root)?));
    }
    fetched(&root, remote, |object, commit| {
        take(&root, remote, object, commit)
    })
}

/// Runs `then` with `remote`'s history ref fetched onto a per-run ref, given the object it holds
/// and the commit that peels to (`MODEL_INVALID` when it is none). The ref is deleted after on
/// every path; when only that deletion fails, a warning says so. An empty `--refmap` keeps a
/// configured `remote.<name>.fetch` from moving any other ref.
fn fetched<T>(root: &Path, remote: &str, then: impl FnOnce(&str, &str) -> Result<T>) -> Result<T> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = format!("{FETCHING}/{}-{nanos}", std::process::id());
    let args = [
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--refmap=",
        "--end-of-options",
        remote,
        &refspec(&format!("+{HISTORY}"), &scratch),
    ];
    let result = git::run(root, &args).and_then(|_| {
        let object = ref_target(root, &scratch)?.context("the fetched history ref is missing")?;
        let commit =
            peel(root, &object).ok_or_else(|| foreign(remote, &object, "is not a commit"))?;
        then(&object, &commit)
    });
    match git::run(root, &["update-ref", "-d", &scratch]) {
        Err(error) if result.is_ok() => {
            eprintln!("warning: deleting the scratch ref {scratch} failed: {error:#}");
            result
        }
        _ => result,
    }
}

/// Moves the history ref to the fetched `object`, which peels to `commit`, or to the local
/// checkpoints replayed onto it, after checking it, recomputing when a checkpoint landed locally
/// meanwhile.
fn take(root: &Path, remote: &str, object: &str, commit: &str) -> Result<Published> {
    let known = id_counts(&walk(root, commit, None)?);
    let mut lost = None;
    for _ in 0..ATTEMPTS {
        let local = tip(root)?;
        let ours = local.as_deref().and_then(|o| peel(root, o));
        if local.as_deref() == Some(object) {
            return Ok(unchanged(remote, local));
        }
        if let Some(ours) = &ours
            && git::is_ancestor(root, commit, ours)?
        {
            return Ok(unchanged(remote, local));
        }
        let gained = since(root, commit, ours.as_deref(), &known, |o, r| {
            foreign(remote, o, r)
        })?;
        let (new, replayed) = match &ours {
            Some(ours) if !git::is_ancestor(root, ours, commit)? => {
                replay(root, remote, commit, ours, &known)?
            }
            _ => (object.to_owned(), Vec::new()),
        };
        let doing = format!("fetch the history from {remote}");
        match update_ref(root, &new, local.as_deref(), &doing) {
            Ok(()) => {
                return Ok(Published {
                    checkpoints: gained.into_iter().map(|c| c.id).collect(),
                    replayed,
                    ..moved(remote, local, new)
                });
            }
            // A retain moved the ref meanwhile: judge the fetched history against it again.
            Err(e)
                if e.downcast_ref::<Refusal>()
                    .is_some_and(|r| r.code == Code::HistoryMoved) =>
            {
                lost = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(lost.context("the history ref kept moving")?)
}

/// Replays the checkpoints the local history `ours` holds and the fetched `commit` lacks onto
/// `commit`, oldest first, keeping each one's tree, message and dates: the new tip and the IDs
/// replayed, newest first. `theirs` counts the fetched history's checkpoint IDs. Refuses
/// `HISTORY_DIVERGED` when the two share no commit or a replayed ID is one the fetched history
/// holds in any case, and `MODEL_INVALID` for a local commit no fetch would take.
fn replay(
    root: &Path,
    remote: &str,
    commit: &str,
    ours: &str,
    theirs: &BTreeMap<String, usize>,
) -> Result<(String, Vec<String>)> {
    if !git::have_merge_base(root, ours, commit)? {
        return Err(unrelated(remote));
    }
    let known = id_counts(&walk(root, ours, None)?);
    let local = since(root, ours, Some(commit), &known, |o, r| {
        unpublishable(remote, o, r)
    })?;
    let clashing: Vec<&str> = (local.iter())
        .filter(|c| theirs.contains_key(&c.id.to_ascii_lowercase()))
        .map(|c| c.id.as_str())
        .collect();
    if !clashing.is_empty() {
        return Err(clash(remote, &clashing));
    }
    let sign = signing(root)?;
    let mut tip = commit.to_owned();
    for checkpoint in local.iter().rev() {
        tip = copy(root, &checkpoint.commit, &tip, sign)?;
    }
    Ok((tip, local.into_iter().map(|c| c.id).collect()))
}

/// The commit `original` copied onto `parent`: its tree, message and author and committer dates,
/// signed when `sign` says so, as `model retain` signs.
fn copy(root: &Path, original: &str, parent: &str, sign: bool) -> Result<String> {
    let raw = git::text(root, &["cat-file", "commit", original])?;
    let (head, message) = raw.split_once("\n\n").context("parsing a commit")?;
    let field = |key: &str| {
        head.lines()
            .find_map(|line| line.strip_prefix(key))
            .with_context(|| format!("commit {original} has no {key}"))
    };
    // An identity line ends with `> <seconds> <zone>`, the date Git reads back.
    let date = |key: &str| -> Result<String> {
        let line = field(key)?;
        let (_, date) = line
            .rsplit_once("> ")
            .context("parsing a commit identity")?;
        Ok(date.to_owned())
    };
    let env = [
        ("GIT_AUTHOR_DATE", date("author ")?),
        ("GIT_COMMITTER_DATE", date("committer ")?),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    commit_tree(root, field("tree ")?, message, Some(parent), sign, &env)
}

/// Each checkpoint ID of `list` in ASCII lowercase, with how many checkpoints hold it.
fn id_counts(list: &[Checkpoint]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for checkpoint in list {
        *counts
            .entry(checkpoint.id.to_ascii_lowercase())
            .or_default() += 1;
    }
    counts
}

/// The checkpoints along first parents from `commit` that `base` does not reach, newest first.
/// Each must be one `model retain` could have written, else `refuse` gets its commit and why: it
/// has at most one parent, carries checkpoint trailers, its `--doing`, `Writer` and `Operation`
/// pass retain's checks, and its ID is a well-formed local ID that `known`, the counts of the
/// checkpoint IDs from `commit`, holds once in any case.
fn since(
    root: &Path,
    commit: &str,
    base: Option<&str>,
    known: &BTreeMap<String, usize>,
    refuse: impl Fn(&str, &str) -> anyhow::Error,
) -> Result<Vec<Checkpoint>> {
    let exclude = base.map(|base| format!("^{base}"));
    let mut merges = vec!["rev-list", "--first-parent", "--min-parents=2", "-n1"];
    merges.extend(["--end-of-options", commit]);
    merges.extend(exclude.as_deref());
    if let Some(merge) = git::text(root, &merges)?.lines().next() {
        return Err(refuse(merge, "is a merge, which model retain never writes"));
    }
    let mut args = vec!["log", "--first-parent", "--no-show-signature"];
    args.extend(["--format=%H%x1f%cI%x1f%B%x1e", "--end-of-options", commit]);
    args.extend(exclude.as_deref());
    let log = git::text(root, &args)?;
    let mut found = Vec::new();
    for entry in log.split('\x1e').filter(|e| !e.trim().is_empty()) {
        let object = entry.trim_start().split('\x1f').next().unwrap_or_default();
        let checkpoint = parse(entry).ok_or_else(|| refuse(object, "is not a checkpoint"))?;
        let id = checkpoint.id.to_ascii_lowercase();
        if !ids::is_local(&id) || known.get(&id) != Some(&1) {
            return Err(refuse(object, "has a malformed or repeated checkpoint ID"));
        }
        let operation = checkpoint.operation_id.as_deref();
        if check_doing(&checkpoint.doing).is_err()
            || check_writer(&checkpoint.writer).is_err()
            || operation.map(check_operation).transpose().is_err()
        {
            return Err(refuse(object, "has a trailer value `model retain` refuses"));
        }
        found.push(checkpoint);
    }
    Ok(found)
}

/// The commit `object` peels to, `None` when it is not one.
fn peel(root: &Path, object: &str) -> Option<String> {
    let peeled = format!("{object}^{{commit}}");
    let found = git::text(root, &["rev-parse", "--verify", "--quiet", &peeled]).ok()?;
    Some(found.trim().to_owned())
}

/// Whether the local history commit `commit` holds the remote tip `theirs`: it is absent, or a
/// commit already here that `commit` descends from.
fn holds(root: &Path, commit: &str, theirs: Option<&str>) -> Result<bool> {
    let Some(theirs) = theirs else {
        return Ok(true);
    };
    match peel(root, theirs) {
        Some(theirs) => git::is_ancestor(root, &theirs, commit),
        // A tip this repository lacks is no ancestor of its history.
        None => Ok(false),
    }
}

/// The refspec that updates `to` from `from`.
fn refspec(from: &str, to: &str) -> String {
    format!("{from}:{to}")
}

fn unchanged(remote: &str, at: Option<String>) -> Published {
    Published {
        remote: remote.to_owned(),
        reference: HISTORY,
        after: at.clone(),
        before: at,
        updated: false,
        checkpoints: Vec::new(),
        replayed: Vec::new(),
    }
}

fn moved(remote: &str, before: Option<String>, after: String) -> Published {
    Published {
        remote: remote.to_owned(),
        reference: HISTORY,
        before,
        after: Some(after),
        updated: true,
        checkpoints: Vec::new(),
        replayed: Vec::new(),
    }
}

/// Refuses `REQUEST_INVALID` unless `remote` is a name `git remote` lists, so a URL or an option
/// never reaches Git as a remote.
fn check_remote(root: &Path, remote: &str) -> Result<()> {
    let listed = git::text(root, &["remote"])?;
    let remotes: Vec<&str> = listed.lines().collect();
    if !remote.starts_with('-') && remotes.contains(&remote) {
        return Ok(());
    }
    Err(Refusal::new(
        Code::RequestInvalid,
        "--remote is not a configured remote; `git remote` lists them",
    )
    .with_details(json!({ "remote": remote, "remotes": remotes }))
    .into())
}

/// The object `remote`'s history ref holds, `None` when it has none.
fn remote_tip(root: &Path, remote: &str) -> Result<Option<String>> {
    let listing = git::text(root, &["ls-remote", "--end-of-options", remote, HISTORY])?;
    Ok(listing.lines().find_map(|line| {
        let (object, name) = line.split_once('\t')?;
        (name == HISTORY).then(|| object.to_owned())
    }))
}

/// `MODEL_INVALID` for a fetched history whose `object` is not what `model retain` writes.
fn foreign(remote: &str, object: &str, reason: &str) -> anyhow::Error {
    Refusal::new(
        Code::ModelInvalid,
        format!("the history fetched from the remote holds an object that {reason}; the local history did not move"),
    )
    .with_details(json!({ "remote": remote, "object": object }))
    .into()
}

/// `MODEL_INVALID` for a local history commit `object` a fetch would refuse, so it is neither
/// pushed nor replayed.
fn unpublishable(remote: &str, object: &str, reason: &str) -> anyhow::Error {
    Refusal::new(
        Code::ModelInvalid,
        format!(
            "the local history holds a commit that {reason}, which a fetch refuses; nothing moved"
        ),
    )
    .with_details(json!({ "remote": remote, "object": object }))
    .into()
}

/// `HISTORY_DIVERGED` for a push the remote's history `theirs` does not descend to: only ahead
/// when `local`, the local checkpoint IDs it lacks, is empty.
fn diverged(remote: &str, theirs: &str, ours: &str, local: &[&str]) -> anyhow::Error {
    let message = if local.is_empty() {
        format!(
            "the history on {remote} is ahead of the local one; run `boundrail model history fetch --remote {remote}` first"
        )
    } else {
        format!(
            "the local history and the one on {remote} diverged: {} local checkpoint(s) are not on {remote}; run `boundrail model history fetch --remote {remote}`, which replays them onto its history, then push again",
            local.len()
        )
    };
    let details =
        json!({ "remote": remote, "remote_tip": theirs, "local_tip": ours, "local_only": local });
    Refusal::new(Code::HistoryDiverged, message)
        .with_details(details)
        .into()
}

/// `HISTORY_DIVERGED` for a local history and the one on `remote` that share no commit, so no
/// fetch replays one onto the other.
fn unrelated(remote: &str) -> anyhow::Error {
    Refusal::new(
        Code::HistoryDiverged,
        format!("the local history and the one on {remote} share no commit, so neither can take the other's checkpoints; the local history did not move"),
    )
    .with_details(json!({ "remote": remote, "related": false }))
    .into()
}

/// `HISTORY_DIVERGED` for a fetch whose local checkpoints cannot be replayed: the fetched history
/// holds the `clashing` IDs in some case.
fn clash(remote: &str, clashing: &[&str]) -> anyhow::Error {
    Refusal::new(
        Code::HistoryDiverged,
        format!(
            "the local history and the one on {remote} diverged, and {} local checkpoint ID(s) are taken there, so they cannot be replayed; the local history did not move",
            clashing.len()
        ),
    )
    .with_details(json!({ "remote": remote, "clashing": clashing }))
    .into()
}
