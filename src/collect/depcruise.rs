//! The source files a TypeScript workspace's package.json files publish.
use std::path::Path;

use super::TS_EXTENSIONS;

/// Build output folders a package.json points into; the sources sit without them.
const OUTPUT_DIRS: &[&str] = &["dist", "build", "lib", "out", "esm", "cjs"];
/// Output extensions, longest first, stripped to find the source a package.json path is built
/// from.
const OUTPUT_EXTENSIONS: &[&str] = &[
    ".d.mts", ".d.cts", ".d.ts", ".mjs", ".cjs", ".mts", ".cts", ".jsx", ".tsx", ".js", ".ts",
];

/// The scanned files that the package.json files under `root` publish through `exports`, `main`,
/// `module` and `types`: each path mapped from build output back to its source (output folder,
/// extension and a `src/` prefix tried), else to the one scanned file of the package with the
/// same path stem. node_modules and hidden folders are skipped, symbolic links not followed.
pub fn package_entries(root: &Path, files: &[String]) -> Vec<String> {
    let scanned: std::collections::BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let mut found = std::collections::BTreeSet::new();
    let mut dirs = vec![".".to_owned()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && name != "node_modules" && !name.starts_with('.') {
                dirs.push(crate::modules::join(&dir, &name));
            } else if kind.is_file() && name == "package.json" {
                let Some(json) = std::fs::read(entry.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                else {
                    continue;
                };
                let mut targets = Vec::new();
                for key in ["main", "module", "types", "typings", "exports"] {
                    strings(&json[key], &mut targets);
                }
                for t in targets {
                    if let Some(file) = source_of(&dir, &t, &scanned) {
                        found.insert(file.to_owned());
                    }
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Every string in a package.json field: `exports` nests subpaths and conditions.
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// The scanned file a package path (relative to the package folder `dir`) is built from.
fn source_of<'a>(
    dir: &str,
    target: &str,
    scanned: &std::collections::BTreeSet<&'a str>,
) -> Option<&'a str> {
    let target = target.strip_prefix("./").unwrap_or(target);
    if target.contains('*') || target.ends_with(".json") || target.starts_with(['/', '.']) {
        return None;
    }
    let mut stem = OUTPUT_EXTENSIONS
        .iter()
        .find_map(|ext| target.strip_suffix(ext))
        .unwrap_or(target);
    while let Some((first, rest)) = stem.split_once('/') {
        if !OUTPUT_DIRS.contains(&first) {
            break;
        }
        stem = rest;
    }
    for base in [
        crate::modules::join(dir, stem),
        crate::modules::join(dir, &format!("src/{stem}")),
    ] {
        for ext in TS_EXTENSIONS {
            if let Some(file) = scanned.get(format!("{base}.{ext}").as_str()) {
                return Some(file);
            }
        }
    }
    // The same stem anywhere in the package, when only one file has it.
    let tail = format!("/{stem}");
    let prefix = if dir == "." {
        String::new()
    } else {
        format!("{dir}/")
    };
    let mut same = scanned.iter().filter(|f| {
        f.strip_prefix(prefix.as_str()).is_some_and(|rel| {
            let path = format!("/{rel}");
            path.rsplit_once('.')
                .is_some_and(|(p, ext)| TS_EXTENSIONS.contains(&ext) && p.ends_with(&tail))
        })
    });
    match (same.next(), same.next()) {
        (Some(file), None) => Some(file),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::package_entries;
    use crate::collect::TempDir;

    #[test]
    fn package_json_entries_map_to_scanned_sources() {
        // Output paths (dist/, lib/, .mjs, .d.ts) map back to the sources they are built from; a
        // wildcard export and package.json itself map to nothing.
        let dir = TempDir::new("pkg-entries").unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{ "main": "./dist/index.cjs", "types": "./dist/index.d.ts",
                 "exports": { ".": { "import": { "types": "./dist/index.d.mts", "default": "./dist/index.mjs" } },
                              "./internal": "./internal.js",
                              "./adapters/http": "./dist/adapters/http/index.mjs",
                              "./package.json": "./package.json", "./*": "./*.js" } }"#,
        )
        .unwrap();
        fs::write(
            root.join("src/sub/package.json"),
            r#"{ "main": "./lib/main.js" }"#,
        )
        .unwrap();
        let files = [
            "src/a.ts",
            "src/adapters/http/index.ts",
            "src/index.ts",
            "src/internal.ts",
            "src/sub/main.ts",
        ]
        .map(str::to_owned);
        assert_eq!(
            package_entries(root, &files),
            [
                "src/adapters/http/index.ts",
                "src/index.ts",
                "src/internal.ts",
                "src/sub/main.ts"
            ]
        );
    }
}
