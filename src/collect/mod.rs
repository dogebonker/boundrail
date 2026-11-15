//! Collectors turn a tool's output into files and file edges inside the scanned root.

pub mod barrels;
pub mod depcruise;
pub mod elixir;
pub mod excludes;
pub mod go;
pub mod grimp;
pub mod imports;
pub mod incremental;
pub mod java;
pub mod native;
pub mod rust;
pub mod typescript_resolution;

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::model::{Collector, Edge, Language, NodeMetadata};

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Reference,
    Native,
    Depcruise,
}

impl Engine {
    pub fn is_reference(&self) -> bool {
        *self == Self::Reference
    }
}

pub struct RawGraph {
    pub files: Vec<String>,
    pub edges: Vec<Edge>,
    pub collector: Collector,
    pub metadata: std::collections::BTreeMap<String, NodeMetadata>,
    pub imports: Option<Vec<crate::model::ImportRecord>>,
    /// Host globals used without a declaration; native JavaScript/TypeScript only.
    pub globals: Option<Vec<crate::model::GlobalUse>>,
}

/// Extensions dependency-cruiser files must have to count as source; imports of anything else
/// (stylesheets, JSON, images) are dropped.
pub const TS_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

// Kept for callers constructing collector-specific discovery globs.
pub use excludes::{GENERATED_EXCLUDES as DEFAULT_EXCLUDES, TEST_EXCLUDES};

/// Files a root marker makes count for Go in language detection.
const GO_MARKERS: &[&str] = &["go.mod", "go.work"];

impl RawGraph {
    /// Drops excluded files and every edge that does not join two kept files.
    pub fn exclude(mut self, excludes: &GlobSet) -> RawGraph {
        self.files.retain(|f| !excludes.is_match(f));
        let kept: HashSet<&str> = self.files.iter().map(String::as_str).collect();
        self.edges
            .retain(|e| kept.contains(e.from.as_str()) && kept.contains(e.to.as_str()));
        self
    }
}

/// `*` stays within one path segment, `**` crosses any number.
pub fn globset(globs: &[String]) -> Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for glob in globs {
        set.add(
            GlobBuilder::new(glob)
                .literal_separator(true)
                .build()
                .with_context(|| format!("glob {glob}"))?,
        );
    }
    Ok(set.build()?)
}

/// A new directory under the system temporary directory, removed with its contents on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    /// `archgraph-<prefix>-<pid>-<nanos>-<sequence>`: the sequence keeps two directories created
    /// in one clock tick apart.
    pub fn new(prefix: &str) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "archgraph-{prefix}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        // Only the user may read what goes in, such as a copy of the Git index.
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        (builder.create(&path)).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

thread_local! { static REQUEST_TIMEOUT: std::cell::Cell<Option<Option<Duration>>> = const { std::cell::Cell::new(None) }; }
pub fn request_timeout(timeout: Option<Duration>) {
    REQUEST_TIMEOUT.with(|t| t.set(Some(timeout)));
}
/// Default timeout for library callers without a request override.
pub static TOOL_TIMEOUT: OnceLock<Option<Duration>> = OnceLock::new();

/// The tool timeout of this thread's request, else the default; `None` waits forever.
pub fn tool_timeout() -> Option<Duration> {
    REQUEST_TIMEOUT
        .with(|t| t.get())
        .unwrap_or_else(|| TOOL_TIMEOUT.get().copied().flatten())
}

/// `run_with_input` with no input.
pub fn run(command: &mut Command) -> Result<Output> {
    run_with_input(command, Vec::new())
}

/// Runs `command` with `input` on its stdin and its stdout and stderr captured, killing it at
/// `TOOL_TIMEOUT`. The input is written from another thread, so a tool that exits before reading
/// all of it reports its own error output; a write error surfaces only when the tool succeeded.
pub fn run_with_input(command: &mut Command, input: Vec<u8>) -> Result<Output> {
    fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<std::io::Result<Vec<u8>>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = thread::spawn(move || stdin.write_all(&input));
    let stdout = read_all(child.stdout.take().expect("piped stdout"));
    let stderr = read_all(child.stderr.take().expect("piped stderr"));
    let status = match tool_timeout() {
        None => child.wait()?,
        Some(timeout) => {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    // Only the direct child is killed; a process it started may keep the pipes
                    // open, so the readers are left running rather than joined.
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "{} timed out after {} s",
                        command.get_program().to_string_lossy(),
                        timeout.as_secs()
                    );
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let collect = |reader: JoinHandle<std::io::Result<Vec<u8>>>| -> Result<Vec<u8>> {
        reader
            .join()
            .map_err(|_| anyhow!("tool output reader panicked"))?
            .context("reading tool output")
    };
    let output = Output {
        status,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    };
    let written = writer
        .join()
        .map_err(|_| anyhow!("tool input writer panicked"))?;
    if output.status.success() {
        written.context("writing tool input")?;
    }
    Ok(output)
}

/// Whether `path` is a `/`-separated path inside a root: not empty or absolute, without a `.`,
/// `..` or empty segment, a backslash, a colon or a control character. `allow_root` admits `.`.
pub fn is_relative_path(path: &str, allow_root: bool) -> bool {
    (allow_root && path == ".")
        || (!path.is_empty()
            && !path.contains(['\\', ':'])
            && !path.chars().any(char::is_control)
            && path.split('/').all(|part| !matches!(part, "" | "." | "..")))
}

/// The program `--<flag> <value>` names: a path made absolute, so it does not depend on the
/// working directory a child runs in, else a name the spawn looks up on PATH.
pub fn resolve_executable(flag: &str, value: impl AsRef<Path>) -> Result<PathBuf> {
    let value = value.as_ref();
    if !value
        .to_string_lossy()
        .contains(['/', std::path::MAIN_SEPARATOR])
    {
        return Ok(value.to_owned());
    }
    fs::canonicalize(value)
        .with_context(|| format!("--{flag} {}: no such executable", value.display()))
}

/// The absolute folders of `PATH`.
fn absolute_dirs() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    (std::env::split_paths(&path))
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// `PATH` without its relative folders, for a child that must not find a program the checkout
/// plants.
pub fn absolute_path() -> std::ffi::OsString {
    std::env::join_paths(absolute_dirs()).unwrap_or_default()
}

/// The program a bare `command` names: the first executable file of that name in an absolute
/// folder of `PATH`, `.exe` added on Windows, that does not lie inside `checkout` once links are
/// resolved. Relative folders and folders inside the checkout are skipped, so a checkout cannot
/// plant the program.
pub fn resolve_program(command: &str, checkout: &Path) -> Result<PathBuf> {
    program_in(absolute_dirs(), command, checkout)
        .ok_or_else(|| anyhow!("{command} is not on PATH outside the checkout"))
}

fn program_in(dirs: Vec<PathBuf>, command: &str, checkout: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_owned()
    };
    let checkout = fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let outside = |path: &Path| fs::canonicalize(path).is_ok_and(|p| !p.starts_with(&checkout));
    (dirs.into_iter())
        .map(|dir| dir.join(&name))
        .find(|candidate| executable(candidate) && outside(candidate))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// `src` when the root has one, else the root itself.
pub fn source_dir(root: &Path) -> &'static str {
    if root.join("src").is_dir() {
        "src"
    } else {
        "."
    }
}

/// The language with the most source files after the default excludes: TypeScript and Python
/// under `source_dir`, Java/Rust/Go/Elixir under the root. Rust and Go require root manifests.
/// A tie keeps TypeScript, then Python, Go, Rust, Java and Elixir.
pub fn detect_language(root: &Path, source_dir: &str) -> Result<Language> {
    detect_language_with_options(root, source_dir, excludes::Options::default())
}

pub fn detect_language_with_options(
    root: &Path,
    source_dir: &str,
    options: excludes::Options,
) -> Result<Language> {
    let generated = options.no_defaults || options.include_generated;
    let has_includes = !options.includes.is_empty();
    let java_output = globset(&excludes::output_globs(Language::Java))?;
    let rust_output = globset(&excludes::output_globs(Language::Rust))?;
    let elixir_output = globset(&excludes::output_globs(Language::Elixir))?;
    let policy = excludes::Policy::new(root, Language::Typescript, options)?;
    let discovery_excludes = globset(policy.patterns())?;
    let cargo = root.join("Cargo.toml").is_file();
    let mix = root.join("mix.exs").is_file();
    let (mut ts, mut py, mut rust, mut java, mut go) = (0, 0, 0, 0, 0);
    let mut elixir = 0;
    let mut paths = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(&entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                if name != ".git"
                    && (has_includes || !discovery_excludes.is_match(format!("{rel}/_")))
                {
                    dirs.push(entry.path());
                }
                continue;
            }
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            paths.push(rel);
        }
    }
    let has_go = GO_MARKERS.iter().any(|m| root.join(m).is_file());
    for rel in policy.select(&paths)? {
        if mix && !generated && !policy.included(&rel) && elixir_output.is_match(&rel) {
            continue;
        }
        let path = root.join(&rel);
        match rel.rsplit_once('.').map(|(_, ext)| ext) {
            Some("ex" | "exs")
                if generated || policy.included(&rel) || !elixir_output.is_match(&rel) =>
            {
                elixir += 1
            }
            Some("py") if path.starts_with(root.join(source_dir)) => py += 1,
            Some("rs")
                if cargo && (generated || policy.included(&rel) || !rust_output.is_match(&rel)) =>
            {
                rust += 1
            }
            Some("java") if generated || policy.included(&rel) || !java_output.is_match(&rel) => {
                java += 1
            }
            Some("go")
                if has_go
                    && !rel
                        .split('/')
                        .any(|segment| segment.starts_with('_') || segment == "testdata") =>
            {
                go += 1
            }
            Some(ext)
                if TS_EXTENSIONS.contains(&ext) && path.starts_with(root.join(source_dir)) =>
            {
                ts += 1
            }
            _ => {}
        }
    }
    match (ts, py, go, rust, java, elixir) {
        (0, 0, 0, 0, 0, 0) => bail!(
            "no .ts/.js, .py, .java, .ex/.exs, .rs (with Cargo.toml) or .go (with go.mod) files under {}; pass --language",
            root.join(source_dir).display()
        ),
        _ if elixir > ts && elixir > py && elixir > go && elixir > rust && elixir > java => {
            Ok(Language::Elixir)
        }
        _ if java > ts && java > py && java > go && java > rust => Ok(Language::Java),
        _ if rust > ts && rust > py && rust > go => Ok(Language::Rust),
        _ if go > ts && go > py => Ok(Language::Go),
        _ if py > ts => Ok(Language::Python),
        _ => Ok(Language::Typescript),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn programs_resolve_in_absolute_path_folders_outside_the_checkout() {
        use std::os::unix::fs::PermissionsExt;
        let elsewhere = TempDir::new("resolve-elsewhere").unwrap();
        assert!(
            resolve_program("sh", elsewhere.path())
                .unwrap()
                .is_absolute()
        );
        assert!(resolve_program("boundrail-no-such-program", elsewhere.path()).is_err());
        let path = absolute_path();
        assert!(std::env::split_paths(&path).all(|dir| dir.is_absolute()));

        let checkout = TempDir::new("resolve-checkout").unwrap();
        let (inside, outside) = (checkout.path().join("bin"), elsewhere.path().join("bin"));
        for dir in [&inside, &outside] {
            fs::create_dir(dir).unwrap();
            fs::write(dir.join("tool"), "#!/bin/sh\n").unwrap();
            fs::set_permissions(dir.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = program_in(
            vec![inside.clone(), outside.clone()],
            "tool",
            checkout.path(),
        );
        assert_eq!(found, Some(outside.join("tool")));
        assert_eq!(program_in(vec![inside], "tool", checkout.path()), None);
    }

    #[test]
    fn a_tool_exiting_before_reading_its_input_reports_its_own_error() {
        let input = vec![b'x'; 1 << 20];
        let output = run_with_input(
            Command::new("sh").args(["-c", "echo 'helper failed early' >&2; exit 3"]),
            input.clone(),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "helper failed early"
        );
        let echoed = run_with_input(&mut Command::new("cat"), input.clone()).unwrap();
        assert_eq!(echoed.stdout, input);
    }

    #[test]
    fn relative_paths_stay_inside_the_root() {
        for path in ["a", "a/b.ts", "a/.hidden/b", "a..b"] {
            assert!(is_relative_path(path, false), "{path:?}");
        }
        for path in [
            "", ".", "..", "../a", "a/..", "a/./b", "a//b", "a/", "/a", "a\\b", "C:/a", "a:b",
            "a\u{1}b", "a\0b", "a\nb",
        ] {
            assert!(!is_relative_path(path, false), "{path:?}");
        }
        assert!(is_relative_path(".", true));
        assert!(!is_relative_path("./a", true));
    }

    #[test]
    fn temp_dirs_created_together_are_distinct_and_removed_on_drop() {
        let (first, second) = (TempDir::new("test").unwrap(), TempDir::new("test").unwrap());
        assert_ne!(first.path(), second.path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(first.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "only the user may read it");
        }
        let path = first.path().to_owned();
        fs::write(path.join("file"), "").unwrap();
        drop(first);
        assert!(!path.exists());
    }
}
