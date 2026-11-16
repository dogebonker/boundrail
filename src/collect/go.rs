//! Go module discovery and the environment the native Go collector runs `go list` in.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use globset::GlobSet;

/// The environment of the native collector's `go list`: the local toolchain only (a go.mod
/// `toolchain` line is ignored, a `go` line newer than the toolchain fails), no network, no
/// go.work, no cgo, and GOFLAGS replaced, so no `-toolexec` from the user's go env applies and
/// go.mod and go.sum are never rewritten.
pub const ENV: &[(&str, &str)] = &[
    ("GOTOOLCHAIN", "local"),
    ("GOFLAGS", "-mod=readonly"),
    ("CGO_ENABLED", "0"),
    ("GOWORK", "off"),
    ("GOPROXY", "off"),
];

/// The directories under `root` holding a go.mod with a `module` line, root-relative (`.` for
/// the root) and sorted, outside what the go command skips (`vendor`, `testdata`, names starting
/// with `.` or `_`), node_modules and the `excludes` (matched against `<dir>/go.mod`); and one
/// warning per go.mod skipped for having no module line.
pub fn module_dirs(root: &Path, excludes: &GlobSet) -> Result<(Vec<String>, Vec<String>)> {
    let (mut dirs, mut warnings) = (Vec::new(), Vec::new());
    let mut stack = vec![".".to_owned()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = crate::modules::join(&rel, &name);
            if entry.file_type()?.is_dir() {
                if !name.starts_with(['.', '_'])
                    && !["vendor", "testdata", "node_modules"].contains(&name.as_str())
                {
                    stack.push(path);
                }
            } else if name == "go.mod" && !excludes.is_match(&path) {
                let text = fs::read_to_string(entry.path()).unwrap_or_default();
                let module = text.lines().any(|l| {
                    l.trim()
                        .strip_prefix("module")
                        .is_some_and(|r| r.starts_with([' ', '\t', '"']))
                });
                if module {
                    dirs.push(rel.clone());
                } else {
                    warnings.push(format!("skipping {path}: it has no module line"));
                }
            }
        }
    }
    dirs.sort();
    warnings.sort();
    Ok((dirs, warnings))
}
