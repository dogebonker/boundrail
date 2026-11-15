//! Publishing the history: `model history push` and `model history fetch` move
//! [`HISTORY`](super::HISTORY) to or from the same ref on a configured remote, fast-forward only,
//! so a clone or CI job that fetches it resolves checkpoint readings. Both are network operations
//! the user starts explicitly; Git runs them with the hardening of `crate::git`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use super::{HISTORY, history, locations, parse, ref_target, tip, update_ref, walk};
use crate::arch::{Code, Refusal};
use crate::git;

/// The remote `--remote` names when absent.
pub const DEFAULT_REMOTE: &str = "origin";
/// Where a fetch lands before it is checked; deleted after every fetch.
const FETCHED: &str = "refs/boundrail/fetched";
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
}

/// Pushes the history ref to `remote`, a configured remote name, when the remote's ref is absent
/// or an ancestor of the local one. Refuses `HISTORY_DIVERGED` otherwise: fetch first.
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
    if !holds(&root, &commit, theirs.as_deref())? {
        return Err(diverged(remote, theirs.as_deref(), Some(&object), true));
    }
    // The object checked above is what lands, even if the local ref moves meanwhile.
    let refspec = refspec(&object, HISTORY);
    let args = [
        "push",
        "--no-follow-tags",
        "--recurse-submodules=no",
        "--end-of-options",
        remote,
        &refspec,
    ];
    if let Err(error) = git::run(&root, &args) {
        // The remote moved between the check and the push.
        let now = remote_tip(&root, remote)?;
        if !holds(&root, &commit, now.as_deref())? {
            return Err(diverged(remote, now.as_deref(), Some(&object), true));
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

/// Fetches `remote`'s history ref into the local one when the local ref is absent or an ancestor
/// of the remote's; a local ref already ahead stays. The remote's ref lands on a scratch ref
/// first and must be a commit whose first-parent commits this repository lacks all carry
/// checkpoint trailers (`MODEL_INVALID`); the local ref then moves by compare-and-swap. Refuses
/// `HISTORY_DIVERGED` when neither holds the other.
pub fn fetch(start: &Path, remote: &str) -> Result<Published> {
    let (_, root) = locations(start)?;
    check_remote(&root, remote)?;
    if remote_tip(&root, remote)?.is_none() {
        return Ok(unchanged(remote, tip(&root)?));
    }
    let args = [
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--end-of-options",
        remote,
        &refspec(&format!("+{HISTORY}"), FETCHED),
    ];
    let taken = git::run(&root, &args).and_then(|_| take(&root, remote));
    git::run(&root, &["update-ref", "-d", FETCHED])?;
    taken
}

/// Moves the history ref to the fetched one, after checking it, recomputing when a checkpoint
/// landed locally meanwhile.
fn take(root: &Path, remote: &str) -> Result<Published> {
    let object = ref_target(root, FETCHED)?.context("the fetched history ref is missing")?;
    let commit = peel(root, &object).ok_or_else(|| foreign(remote, &object, "is not a commit"))?;
    let mut lost = None;
    for _ in 0..ATTEMPTS {
        let local = tip(root)?;
        let ours = local.as_deref().and_then(|o| peel(root, o));
        if local.as_deref() == Some(object.as_str()) {
            return Ok(unchanged(remote, local));
        }
        if let Some(ours) = &ours {
            if git::is_ancestor(root, &commit, ours)? {
                return Ok(unchanged(remote, local));
            }
            if !git::is_ancestor(root, ours, &commit)? {
                return Err(diverged(remote, Some(&object), local.as_deref(), false));
            }
        }
        refuse_foreign(root, remote, &commit, ours.as_deref())?;
        let doing = format!("fetch the history from {remote}");
        match update_ref(root, &object, local.as_deref(), &doing) {
            Ok(()) => {
                let new = walk(root, &commit, None)?;
                let new = new.iter().take_while(|c| ours.as_ref() != Some(&c.commit));
                return Ok(Published {
                    checkpoints: new.map(|c| c.id.clone()).collect(),
                    ..moved(remote, local, object)
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

/// Refuses `MODEL_INVALID` when a first-parent commit from `commit` back to the local history
/// `ours` lacks checkpoint trailers: the remote ref holds something `model retain` never made.
fn refuse_foreign(root: &Path, remote: &str, commit: &str, ours: Option<&str>) -> Result<()> {
    let exclude = ours.map(|ours| format!("^{ours}"));
    let mut args = vec![
        "log",
        "--first-parent",
        "--no-show-signature",
        "--format=%H%x1f%cI%x1f%B%x1e",
        "--end-of-options",
        commit,
    ];
    args.extend(exclude.as_deref());
    let log = git::text(root, &args)?;
    let mut entries = log.split('\x1e').filter(|e| !e.trim().is_empty());
    match entries.find(|entry| parse(entry).is_none()) {
        Some(entry) => {
            let found = entry.trim_start().split('\x1f').next().unwrap_or_default();
            Err(foreign(remote, found, "is not a checkpoint"))
        }
        None => Ok(()),
    }
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

/// `HISTORY_DIVERGED` for a push (`pushing`) or fetch that cannot fast-forward.
fn diverged(
    remote: &str,
    theirs: Option<&str>,
    ours: Option<&str>,
    pushing: bool,
) -> anyhow::Error {
    let message = if pushing {
        format!(
            "the history on {remote} holds checkpoints this repository lacks; run `boundrail model history fetch --remote {remote}` first"
        )
    } else {
        format!(
            "the local history and the one on {remote} each hold checkpoints the other lacks; neither can fast-forward"
        )
    };
    Refusal::new(Code::HistoryDiverged, message)
        .with_details(json!({ "remote": remote, "remote_tip": theirs, "local_tip": ours }))
        .into()
}
