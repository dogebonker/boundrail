//! Every Git subprocess boundrail runs, hardened so a repository's config never names a program
//! Git would run on our behalf (fsmonitor, hooks). The exceptions are the signing program, which
//! signs `model retain`'s checkpoints when the user opts into `commit.gpgSign`, and the clean and
//! process filter drivers repository config defines, which run when Git content-checks a
//! working-tree entry, for example in `model footprint`. Writing `.git/config` already allows
//! running code, so neither widens what a repository can do. The third exception is outside Git:
//! language servers, which run only when `model check` or `model inspect` gets
//! `--language-servers`. The committed `.boundrail/language-servers.json` only names them: an
//! allowlisted server runs its own name, found in an absolute `PATH` folder outside the checkout,
//! with fixed arguments, and any other program, argument or initialization option comes from the
//! user's config outside the checkout. Each command line is printed to stderr before it starts, and
//! the server sees only the absolute folders of `PATH`. A server still runs code from the
//! checkout it opens: the TypeScript server runs the workspace's `tsserver.js` under
//! `node_modules` and its `tsconfig.json` plugins, rust-analyzer runs build scripts and proc
//! macros and reads a committed `rust-analyzer.toml`, and jdtls runs Gradle and Maven imports.
//! So `--language-servers` is a per-run opt-in for trusted checkouts only, never for untrusted
//! pull request heads in CI. The rasterizers `model pictures --write` runs for PNG pictures are no
//! exception: Boundrail picks them from a fixed list (`resvg`, `rsvg-convert`, `magick`), finds
//! them in absolute `PATH` folders outside the checkout, and runs them in a private temporary
//! folder holding only the SVG it drew; the repository names none of them. `model history push`
//! and `model history fetch` reach a remote the user configured and names by its `git remote`
//! name, never a URL or option, so Git runs the transport that config names (`ssh`,
//! `core.sshCommand`, `remote.<name>.uploadpack` and `receivepack`, credential helpers): the
//! same accepted scope as the signing program, and only on that explicit command. Hooks stay off
//! there too, `pre-push` and `reference-transaction` included.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    checked(root, args, index, &[])
}

/// `run` with `env` set for Git, such as the dates a copied commit keeps.
pub fn run_with_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
    checked(root, args, None, env)
}

fn checked(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
) -> Result<Vec<u8>> {
    let out = output(root, args, index, env)?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            subcommand(args),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// The finished `git -C root args` process, whatever its exit status.
fn output(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
) -> Result<Output> {
    let mut command = Command::new("git");
    command.envs(env.iter().copied());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index).args([
            "-c",
            "core.splitIndex=false",
            "-c",
            "core.untrackedCache=true",
        ]);
    }
    command
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
        .context("running git (is it installed?)")
}

/// Whether the commit `ancestor` is `descendant` or one of its ancestors. Git answers with exit
/// status 1 for no; any other failure, such as an unknown object, is an error.
pub fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let args = ["--is-ancestor", "--end-of-options", ancestor, descendant];
    merge_base(root, &args)
}

/// Whether the commits `a` and `b` share an ancestor, answered like [`is_ancestor`].
pub fn have_merge_base(root: &Path, a: &str, b: &str) -> Result<bool> {
    merge_base(root, &["--end-of-options", a, b])
}

/// Whether `git merge-base args` answers yes (exit 0) or no (exit 1).
fn merge_base(root: &Path, args: &[&str]) -> Result<bool> {
    let args = [&["merge-base"][..], args].concat();
    let out = output(root, &args, None, &[])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git merge-base: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
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

/// The absolute Git directory every worktree of the repository around `root` shares.
pub fn common_dir(root: &Path) -> Result<PathBuf> {
    let path = text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
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
