//! Every Git subprocess boundrail runs, hardened so a repository's config never names a program
//! Git would run on our behalf (fsmonitor, hooks, transport), and so Git never discovers a bare
//! repository implicitly (`safe.bareRepository=explicit`): a bare repository planted in a
//! checkout, such as one a reference or binding leads into, cannot supply its config. Git never
//! fetches a missing object lazily from a promisor remote (`GIT_NO_LAZY_FETCH`, Git 2.45 and
//! later), and every command but the transport of `model history push` and `model history
//! fetch` runs with `GIT_ALLOW_PROTOCOL` empty, which allows no protocol whatever the
//! repository's `protocol.*` config says, so older Git cannot reach a remote either. The
//! exceptions are the signing program, which signs the checkpoints `model retain` writes and
//! those `model history fetch` replays when the user opts into `commit.gpgSign`, and the clean
//! and process filter drivers repository config defines, which run when Git content-checks a
//! working-tree entry, for example in `model footprint`. Writing `.git/config` already allows
//! running code, so neither widens what a repository can do. The third exception is outside Git:
//! language servers, which run only when `model check` or `model inspect` gets
//! `--language-servers`, or when the checkout root, once links resolve, lies at or below a folder
//! the user's config outside the checkout lists in `trusted` and the run does not pass
//! `--no-language-servers`; the committed list can never turn them on, and a user's config inside
//! the checkout is refused. The committed `.boundrail/language-servers.json` only names them: an
//! allowlisted server runs its own name, found in an absolute `PATH` folder outside the checkout,
//! with fixed arguments, and any other program, argument or initialization option comes from the
//! user's config. Each command line is printed to stderr before it starts, and the server sees
//! only the absolute folders of `PATH`. A server still runs code from the checkout it opens: the
//! TypeScript server runs the workspace's `tsserver.js` under `node_modules` and its
//! `tsconfig.json` plugins, rust-analyzer runs build scripts and proc macros and reads a committed
//! `rust-analyzer.toml`, jdtls runs Gradle and Maven imports, clangd reads a committed `.clangd`
//! and `compile_commands.json` and passes their flags to its clang frontend, sourcekit-lsp 6 and
//! later background-indexes, building the package and running its macros and build-tool plugins,
//! and csharp-ls runs MSBuild design-time targets and NuGet restore, which reaches the network
//! and runs the `.targets` of packages. So language servers are for trusted checkouts only, never
//! for untrusted pull request heads in CI: a machine that checks those must not list their folder
//! as trusted, and the GitHub Action passes `--no-language-servers`. The rasterizers `model
//! pictures --write` runs for PNG pictures are no exception: Boundrail picks them from a fixed
//! list (`resvg`, `rsvg-convert`, `magick`), finds
//! them in absolute `PATH` folders outside the checkout, and runs them in a private temporary
//! folder holding only the SVG it drew; the repository names none of them. The text-to-speech
//! programs and ffmpeg `model record` runs are the same accepted scope: fixed lists (`say`,
//! `espeak-ng`, `piper`; `ffmpeg`), found the same way, with arguments Boundrail builds, run
//! without a shell in a private temporary folder holding only their inputs, the narration passed
//! in a file or on stdin, never as an argument. So is the agent `model explain` runs: a program
//! the user chose from a fixed list (`claude`, `codex`), by `--agent` or the user's
//! `boundrail/explain.json` outside the checkout, else the first found, never by the repository.
//! It is found and run like the rasterizers, with fixed arguments, no shell and its prompt on
//! stdin, in a private temporary folder under the tool timeout, and it keeps the user's
//! environment, which it needs to reach its provider. It runs only on that explicit command, or
//! for the browser view when the user started `model view --allow-explain`, and it receives the
//! model's text and the question as data between marker lines, holding no environment value or
//! token. Its answer goes only to the user. `claude` runs with no tools and no MCP servers;
//! `codex` runs in its read-only sandbox, which still lets it read any file on disk and put it in
//! the answer, so `claude` is the one to prefer. `model history push`
//! and `model history fetch` reach a remote the user configured and names by its `git remote`
//! name, never a URL or option, so Git runs the transport that config names (`ssh`,
//! `core.sshCommand`, `remote.<name>.uploadpack` and `receivepack`, credential helpers): the
//! same accepted scope as the signing program, and only on that explicit command. Hooks stay off
//! there too, `pre-push` and `reference-transaction` included.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};

use anyhow::{Context, Result, anyhow, bail};

use crate::arch::vault::{STATE, VAULT};

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
    checked(root, args, index, &[], None, false)
}

/// `run_with_index` with `input` on Git's standard input, such as the paths `--stdin-paths`
/// reads.
pub fn run_with_input(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    input: Vec<u8>,
) -> Result<Vec<u8>> {
    checked(root, args, index, &[], Some(input), false)
}

/// `run` with `env` set for Git, such as the dates a copied commit keeps.
pub fn run_with_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
    checked(root, args, None, env, None, false)
}

/// `run` for `git push`, `git fetch` and `git ls-remote` in `model history push` and `model
/// history fetch`, the only Git commands allowed a transport.
pub fn run_transport(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    checked(root, args, None, &[], None, true)
}

fn checked(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
    input: Option<Vec<u8>>,
    transport: bool,
) -> Result<Vec<u8>> {
    let out = output(root, args, index, env, input, transport)?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            subcommand(args),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

const PRIVATE_HISTORY: &str = "history.git";

/// The private history around `path`, the one bare repository Boundrail names explicitly:
/// `<folder>/.boundrail/history.git`, which `model init --private` creates without a template
/// beside the vault `<folder>/.boundrail/model`, for the nearest folder at or above `path`
/// holding one. It counts only when `.boundrail`, the history and the vault are folders, not
/// symbolic links, that on Unix the effective user owns (the owner of a folder this process
/// creates in the system temporary folder), and no folder at or above `path` holds `.git`; so
/// one committed into a checkout or planted by another user is never used, and its config is
/// trusted as a checkout's `.git/config` is. Every Git command Boundrail runs at or below that
/// folder uses it by an explicit `--git-dir`. The answer is kept per canonical path for the
/// process, until [`init_private_history`] creates a history.
pub fn private_history(path: &Path) -> Option<PathBuf> {
    let path = fs::canonicalize(path).ok()?;
    let mut known = known_histories()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    known
        .entry(path)
        .or_insert_with_key(|p| find_history(p))
        .clone()
}

fn known_histories() -> &'static Mutex<HashMap<PathBuf, Option<PathBuf>>> {
    static KNOWN: OnceLock<Mutex<HashMap<PathBuf, Option<PathBuf>>>> = OnceLock::new();
    KNOWN.get_or_init(Mutex::default)
}

fn find_history(path: &Path) -> Option<PathBuf> {
    let mut found = None;
    for folder in path.ancestors() {
        if enclosed(folder) {
            return None;
        }
        let state = folder.join(STATE);
        let history = state.join(PRIVATE_HISTORY);
        if found.is_none()
            && [&state, &history, &state.join(VAULT)]
                .into_iter()
                .all(owned_dir)
        {
            found = Some(history);
        }
    }
    found
}

fn owned_dir(path: &PathBuf) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && owned(&m))
}

#[cfg(unix)]
fn owned(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    static EUID: OnceLock<Option<u32>> = OnceLock::new();
    let euid = EUID.get_or_init(|| {
        let probe = crate::collect::TempDir::new("owner").ok()?;
        fs::metadata(probe.path()).ok().map(|m| m.uid())
    });
    *euid == Some(metadata.uid())
}

#[cfg(not(unix))]
fn owned(_: &fs::Metadata) -> bool {
    true
}

fn enclosed(folder: &Path) -> bool {
    fs::symlink_metadata(folder.join(".git")).is_ok()
}

/// The folder at or above `path` holding `.git`, the top of the checkout around it.
pub fn enclosing_checkout(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|f| enclosed(f))
        .map(Path::to_path_buf)
}

/// The private history at or above `path` that a `.git` at or above its folder absorbs into that
/// checkout, so Boundrail ignores it, with that checkout's folder.
pub fn absorbed_history(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let path = fs::canonicalize(path).ok()?;
    let (folder, history) = path.ancestors().find_map(|folder| {
        let history = folder.join(STATE).join(PRIVATE_HISTORY);
        history.is_dir().then_some((folder, history))
    })?;
    enclosing_checkout(folder).map(|checkout| (history, checkout))
}

/// Creates the bare repository of the private history in `<folder>/.boundrail`, with no
/// template, so it holds no hooks or other files Git would copy in.
pub fn init_private_history(folder: &Path) -> Result<PathBuf> {
    let history = folder.join(STATE).join(PRIVATE_HISTORY);
    let path = history.to_str().context("the folder is not UTF-8")?;
    run(folder, &["init", "--bare", "--quiet", "--template=", path])?;
    known_histories()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    Ok(history)
}

/// The finished `git -C root args` process, whatever its exit status.
fn output(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
    input: Option<Vec<u8>>,
    transport: bool,
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
    if !transport {
        command.env("GIT_ALLOW_PROTOCOL", "");
    }
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        // Read-only queries must not rewrite the index.
        .env(
            "GIT_OPTIONAL_LOCKS",
            if index.is_some() { "1" } else { "0" },
        )
        .arg("-C")
        .arg(root);
    if let Some(history) = private_history(root) {
        command.arg("--git-dir").arg(history);
    }
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "safe.bareRepository=explicit",
            "-c",
            if index.is_some() {
                "diff.autoRefreshIndex=true"
            } else {
                "diff.autoRefreshIndex=false"
            },
        ])
        .args(args);
    let Some(input) = input else {
        return command.output().context("running git (is it installed?)");
    };
    let mut child = (command.stdin(Stdio::piped()).stdout(Stdio::piped()))
        .stderr(Stdio::piped())
        .spawn()
        .context("running git (is it installed?)")?;
    let mut stdin = child.stdin.take().context("git's standard input")?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().context("running git")?;
    let written = writer
        .join()
        .map_err(|_| anyhow!("writing git's input panicked"))?;
    if out.status.success() {
        written.context("writing git's input")?;
    }
    Ok(out)
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
    let out = output(root, &args, None, &[], None, false)?;
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
