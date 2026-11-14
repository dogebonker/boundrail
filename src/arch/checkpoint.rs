//! Checkpoints: retained snapshots of the model with the reason it changed, so the model keeps a
//! history of why the architecture changed. Each checkpoint is a commit on [`HISTORY`] in the
//! source repository, never on the working branch: its tree holds the vault's recognized files at
//! their vault-relative paths, its parent is the previous checkpoint, and its message is the
//! `--doing` line followed by `Checkpoint`, `Revision`, `Source`, `Writer` and `Operation`
//! trailers (`Source` and `Operation` only when there is one). Commits are built with Git
//! plumbing, so the checkout's HEAD, index and working tree never change; every worktree of the
//! repository shares the one history ref. With `commit.gpgSign` set, checkpoints are signed with
//! the user's signing program and key.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::request::{INPUT_MAX, check_doing, check_operation, check_writer};
use super::state::{self, Operation};
use super::vault::{self, Model};
use super::{Code, Refusal, ids, revision};
use crate::collect::TempDir;
use crate::git;

/// The history ref, one per repository.
pub const HISTORY: &str = "refs/boundrail/history";
/// The journal kind of a `model retain` record.
const KIND: &str = "retain";
/// The shortest commit prefix `model read --checkpoint` accepts.
const PREFIX_MIN: usize = 4;
/// The only file mode a checkpoint tree holds.
const MODE: &str = "100644";

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
}

/// Retains the current model of the vault folder `vault` as a new checkpoint. `writer` defaults
/// to the Git author name. With `expected`, refuses `STALE_REVISION` unless the model's revision
/// is that digest. Repeating `operation` with the same writer, `doing` and `expected` returns the
/// checkpoint it made, from this worktree's journal or, from another worktree, the history ref.
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
    let (tip, history) = history(&root, None)?;
    let mut request = json!({ "kind": KIND, "writer": writer, "doing": doing });
    if let Some(expected) = expected {
        request["expected_revision"] = expected.into();
    }
    let journal = Operation::start(&state.dir, operation, KIND, &request, |c: &Checkpoint| {
        Ok(history.iter().any(|h| h.commit == c.commit))
    })?;
    if let Operation::Done(checkpoint) = journal {
        return Ok(checkpoint);
    }
    if let Some(found) = retained_elsewhere(&history, operation, &writer, doing, expected)? {
        journal.done(&state.dir, found)?;
        return Ok(found.clone());
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
    let latest = history.first();
    if let Some(latest) = latest
        && latest.revision == model.revision.digest
    {
        return Err(Refusal::new(
            Code::NothingToRetain,
            format!(
                "the model has not changed since checkpoint {}; edit it before retaining again",
                latest.id
            ),
        )
        .with_details(json!({ "checkpoint": latest.id, "commit": latest.commit }))
        .into());
    }
    let id =
        ids::allocate(|candidate| history.iter().any(|c| c.id.eq_ignore_ascii_case(candidate)))?;
    let message = message(&root, &model, &id, doing, &writer, operation)?;
    let parent = tip.as_ref().map(|(_, commit)| commit.as_str());
    let commit = write_commit(&root, &model, &message, parent)?;
    let checkpoint = walk(&root, &commit, Some(1))?
        .pop()
        .context("reading back the checkpoint commit")?;
    journal.pending(&state.dir, &checkpoint)?;
    let old = tip.as_ref().map(|(object, _)| object.as_str());
    update_ref(&root, &commit, old, doing)?;
    journal.done(&state.dir, &checkpoint)?;
    Ok(checkpoint)
}

/// The checkpoints of the repository around `start`, newest first, at most `limit`; none when
/// the history ref does not exist. The checkout needs no vault.
pub fn list(start: &Path, limit: Option<usize>) -> Result<Vec<Checkpoint>> {
    let (_, root) = locations(start)?;
    Ok(history(&root, limit)?.1)
}

/// The checkpoint `which` names (its ID, else a unique commit prefix of at least 4 hex digits)
/// and the model it holds, loaded from its tree without touching the checkout, which needs no
/// vault. The model's `dir` is where the checkout's vault folder is.
pub fn load(start: &Path, which: &str) -> Result<(Checkpoint, Model)> {
    let (vault, root) = locations(start)?;
    let checkpoint = find(history(&root, None)?.1, which)?;
    let files = files(&root, &checkpoint)?;
    // `git show` prints each blob's bytes back to back, unconverted, so one call reads them all.
    let mut args = vec!["show", "--no-textconv", "--no-color"];
    args.extend(files.iter().map(|(_, object, _)| object.as_str()));
    let bytes = git::run(&root, &args)?;
    let temp = TempDir::new("checkpoint")?;
    let dir = temp.path().join("model");
    let mut rest = bytes.as_slice();
    for (path, _, size) in &files {
        ensure!(
            rest.len() >= *size,
            "git show returned fewer bytes than the tree lists"
        );
        let (content, tail) = rest.split_at(*size);
        let file = dir.join(path);
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        }
        fs::write(&file, content).with_context(|| format!("writing {}", file.display()))?;
        rest = tail;
    }
    ensure!(
        rest.is_empty(),
        "git show returned more bytes than the tree lists"
    );
    let mut model = vault::load_dir(dir).map_err(|mut error| {
        if let Some(refusal) = error.downcast_mut::<Refusal>()
            && refusal.code == Code::ModelInvalid
            && let Some(details) = refusal.details.as_mut()
        {
            let count = details["problems"].as_array().map_or(0, Vec::len);
            refusal.message = format!(
                "the model in checkpoint {} (commit {}) has {count} problem(s)",
                checkpoint.id, checkpoint.commit
            );
            details["checkpoint"] = json!(checkpoint.id);
            details["commit"] = json!(checkpoint.commit);
        }
        error
    })?;
    model.dir = vault;
    Ok((checkpoint, model))
}

/// The vault folder of the checkout around `start`, whether or not it exists, and the checkout.
fn locations(start: &Path) -> Result<(PathBuf, PathBuf)> {
    let vault = vault::path(start)?;
    let root = checkout(&vault)?;
    Ok((vault, root))
}

/// The checkout of a vault folder, `<checkout>/.boundrail/model`.
fn checkout(vault: &Path) -> Result<PathBuf> {
    vault
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("the model vault has no checkout around it")
}

/// The checkpoint an earlier attempt of `operation` from another worktree left on the shared
/// history ref, since each worktree keeps its own journal. Refuses `OPERATION_REUSED` when it
/// was made for another writer or reason, or from another revision than `expected`.
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
    if found.writer != writer
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
/// `MODEL_INVALID` for a tree that holds anything `model retain` never writes: a path that is not
/// a vault file, another mode or object type, a file larger than [`INPUT_MAX`], or no manifest.
fn files(root: &Path, checkpoint: &Checkpoint) -> Result<Vec<(String, String, usize)>> {
    let invalid = |path: &str, reason: &str| -> anyhow::Error {
        Refusal::new(
            Code::ModelInvalid,
            format!(
                "checkpoint {} (commit {}) holds `{path}`, which {reason}",
                checkpoint.id, checkpoint.commit
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
        let entry = std::str::from_utf8(entry).context("a checkpoint path is not UTF-8")?;
        // `<mode> <type> <object> <padded size>\t<path>`
        let (meta, path) = entry.split_once('\t').context("parsing git ls-tree")?;
        let meta: Vec<&str> = meta.split_whitespace().collect();
        let [mode, kind, object, size] = meta.as_slice() else {
            return Err(invalid(path, "is not a file"));
        };
        if !vault::is_vault_file(path) {
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
    if !files.iter().any(|(path, _, _)| path == "model.json") {
        return Err(invalid("model.json", "is missing"));
    }
    Ok(files)
}

/// The name of the Git author identity, which `commit-tree` needs anyway.
fn author(root: &Path) -> Result<String> {
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

fn message(
    root: &Path,
    model: &Model,
    id: &str,
    doing: &str,
    writer: &str,
    operation: Option<&str>,
) -> Result<String> {
    let mut message = format!(
        "{doing}\n\nCheckpoint: {id}\nRevision: {}\n",
        model.revision.digest
    );
    if let Some(source) = source(root)? {
        writeln!(message, "Source: {source}")?;
    }
    writeln!(message, "Writer: {writer}")?;
    if let Some(operation) = operation {
        writeln!(message, "Operation: {operation}")?;
    }
    Ok(message)
}

/// The checkpoint commit of `model` after the commit `parent`, signed when the user set
/// `commit.gpgSign`. Nothing points at it yet, so a failure leaves only unreachable objects.
fn write_commit(root: &Path, model: &Model, message: &str, parent: Option<&str>) -> Result<String> {
    let tree = write_tree(root, model)?;
    let sign = git::text(
        root,
        &[
            "config",
            "--type=bool",
            "--default=false",
            "--get",
            "commit.gpgSign",
        ],
    )?
    .trim()
        == "true";
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    if sign {
        args.push("-S");
    }
    match git::text(root, &args) {
        Ok(commit) => Ok(commit.trim().to_owned()),
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

/// The tree of the vault's recognized files, written to the object store through a disposable
/// index. Refuses `MODEL_INVALID` for a file larger than [`INPUT_MAX`], which no checkpoint
/// reads back, and `STALE_REVISION` when a file changed outside the lock after the model loaded.
fn write_tree(root: &Path, model: &Model) -> Result<String> {
    let paths: Vec<&str> = model
        .revision
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    for path in &paths {
        let file = model.dir.join(path);
        let size = fs::symlink_metadata(&file)
            .with_context(|| format!("reading {}", file.display()))?
            .len();
        if size > INPUT_MAX {
            return Err(Refusal::new(
                Code::ModelInvalid,
                format!("`{path}` is larger than {INPUT_MAX} bytes, more than a checkpoint holds"),
            )
            .with_details(json!({ "path": path }))
            .into());
        }
    }
    let mut args = vec!["hash-object", "-w", "--no-filters", "--"];
    args.extend(&paths);
    let objects = git::text(&model.dir, &args)?;
    let now = revision::of_digests(vault::file_digests(&model.dir)?);
    if now.digest != model.revision.digest {
        return Err(Refusal::new(
            Code::StaleRevision,
            "vault files changed while the checkpoint was written; retry",
        )
        .with_details(json!({ "current": now.digest }))
        .into());
    }
    let entries: Vec<String> = objects
        .lines()
        .zip(&paths)
        .map(|(object, path)| format!("{MODE},{object},{path}"))
        .collect();
    ensure!(
        entries.len() == paths.len(),
        "git hash-object wrote {} objects for {} files",
        entries.len(),
        paths.len()
    );
    let temp = TempDir::new("checkpoint-index")?;
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

/// The history ref's [`Tip`] and the checkpoints from it, newest first, at most `limit`.
fn history(root: &Path, limit: Option<usize>) -> Result<(Option<Tip>, Vec<Checkpoint>)> {
    let Some(object) = tip(root)? else {
        return Ok((None, Vec::new()));
    };
    let peel = format!("{object}^{{commit}}");
    let commit = git::text(root, &["rev-parse", "--verify", "--quiet", &peel])?
        .trim()
        .to_owned();
    let list = walk(root, &commit, limit)?;
    Ok((Some((object, commit)), list))
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
/// `--doing` and its other lines `Key: value` trailers.
fn parse(entry: &str) -> Option<Checkpoint> {
    let mut fields = entry.trim_start_matches('\n').splitn(3, '\x1f');
    let (commit, time, message) = (fields.next()?, fields.next()?, fields.next()?);
    let mut lines = message.lines();
    let doing = lines.next()?.to_owned();
    let trailers: BTreeMap<&str, &str> = lines.filter_map(|l| l.split_once(": ")).collect();
    let value = |key: &str| trailers.get(key).map(|v| (*v).to_owned());
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, args: &[&str]) -> String {
        git::text(root, args).unwrap().trim().to_owned()
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
        assert!(parse("abc\x1ft\x1fa plain commit\n").is_none());
    }
}
