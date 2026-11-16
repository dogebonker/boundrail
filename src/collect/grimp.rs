//! The Python package the native Python collector scans.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use globset::GlobSet;

use crate::modules::join;

/// `explicit`, else the single package directly under `source_dir`: a directory with an
/// `__init__.py` or, for a namespace package, with `.py` files, and not excluded.
pub fn package(
    root: &Path,
    source_dir: &str,
    explicit: Option<&str>,
    excludes: &GlobSet,
) -> Result<String> {
    if let Some(package) = explicit {
        return Ok(package.to_owned());
    }
    let dir = root.join(source_dir);
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // An excluded directory excludes every file in it, so a probe file stands for it.
        if !path.is_dir()
            || name.starts_with('.')
            || excludes.is_match(join(source_dir, &format!("{name}/x.py")))
        {
            continue;
        }
        let mut pending = vec![path.clone()];
        let mut has_python = false;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory)?.filter_map(|entry| entry.ok()) {
                let kind = entry.file_type()?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if kind.is_file() && entry.path().extension().is_some_and(|x| x == "py") {
                    has_python = true;
                    break;
                }
                if kind.is_dir()
                    && !name.starts_with('.')
                    && !["node_modules", "vendor", "__pycache__"].contains(&name.as_ref())
                {
                    pending.push(entry.path());
                }
            }
            if has_python {
                break;
            }
        }
        if has_python {
            found.push(name);
        }
    }
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!(
            "no Python package (directory with .py files) in {}",
            dir.display()
        ),
        many => bail!(
            "several Python packages in {}: {}",
            dir.display(),
            many.join(", ")
        ),
    }
}
