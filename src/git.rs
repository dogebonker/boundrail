//! Every Git subprocess boundrail runs, hardened so a repository's config never names a program
//! Git would run on our behalf (fsmonitor, hooks). The exceptions are the signing program, which
//! signs `model retain`'s checkpoints when the user opts into `commit.gpgSign`, and the clean and
//! process filter drivers repository config defines, which run when Git content-checks a
//! working-tree entry, for example in `model footprint`. Writing `.git/config` already allows
//! running code, so neither widens what a repository can do.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The references naming a repository's default branch, in the order they are tried.
pub const DEFAULT_BRANCHES: [&str; 3] = ["origin/HEAD", "main", "master"];

fn subcommand<'a>(args: &[&'a str]) -> &'a str {
    args.iter().find(|arg| !arg.starts_with('-')).unwrap_or(&"")
}

/// Standard output of `git -C root args`; a failed exit is an error carrying Git's stderr.
pub fn run(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_with_index(root, args, None)
}
/// A disposable index permits Git's own stat cache without changing the user's staging index,
/// so `git diff` refreshes it: an entry whose stat changed is content-checked, not listed.
pub fn run_with_index(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index).args([
            "-c",
            "core.splitIndex=false",
            "-c",
            "core.untrackedCache=true",
        ]);
    }
    let out = command
        // Read-only queries must not rewrite the index and trigger the daemon watcher.
        .env(
            "GIT_OPTIONAL_LOCKS",
            if index.is_some() { "1" } else { "0" },
        )
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            if index.is_some() {
                "diff.autoRefreshIndex=true"
            } else {
                "diff.autoRefreshIndex=false"
            },
        ])
        .args(args)
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            subcommand(args),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// `run`, requiring UTF-8 output.
pub fn text(root: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(run(root, args)?)
        .with_context(|| format!("git {} output is not UTF-8", subcommand(args)))
}

/// The canonical top-level directory of the work tree containing `root`.
pub fn toplevel(root: &Path) -> Result<PathBuf> {
    let path = text(root, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(path.trim_end_matches(['\n', '\r'])).context("resolving the Git root")
}

/// The absolute Git directory of the work tree containing `root`.
pub fn git_dir(root: &Path) -> Result<PathBuf> {
    let path = text(root, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(path.trim_end_matches(['\n', '\r'])))
}

/// The full hash of the commit `rev` names.
pub fn resolve(root: &Path, rev: &str) -> Result<String> {
    Ok(text(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
    )?
    .trim()
    .to_owned())
}

pub fn is_shallow(root: &Path) -> Result<bool> {
    Ok(text(root, &["rev-parse", "--is-shallow-repository"])?.trim() == "true")
}

/// A full SHA-1 or SHA-256 commit hash, never an abbreviation or a ref name.
pub fn is_full_hash(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && commit.bytes().all(|c| c.is_ascii_hexdigit())
}

/// A changed tracked path proves the whole checkout is dirty without enumerating its status.
/// A clean result is not proof of a clean checkout: callers must still check untracked/other paths.
pub fn has_tracked_changes(root: &Path, paths: &[String]) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    let paths: Vec<_> = paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    // Quiet/name-only diffs can flag stat-only changes; text numstat verifies content changes.
    let mut args = vec![
        "diff",
        "--numstat",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "HEAD",
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    let output = run(root, &args)
        .context("checking changed tracked files; use the complete Git status check on failure")?;
    let mut rows = output
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty());
    // Binary/metadata-only stats may also be stat-only; defer to the caller's full status check.
    Ok(!rows.clone().any(|row| row.starts_with(b"-\t-\t"))
        && rows.any(|row| !row.starts_with(b"0\t0\t")))
}
