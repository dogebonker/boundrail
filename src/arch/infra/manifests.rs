//! Workspace manifests: npm workspace packages, Cargo path dependencies and Go `replace`
//! directives to local paths.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

use super::{End, Infra, Lines, Var, normalize, read_file};
use crate::arch::types::Evidence;
use crate::modules::{dir_of, join};
use crate::package_splits::workspaces;

pub(super) const PACKAGE_JSON: &str = "package.json";
pub(super) const CARGO_TOML: &str = "Cargo.toml";
pub(super) const GO_MOD: &str = "go.mod";
/// The file whose presence at the root declares a pnpm workspace.
const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";
const NPM_DEPENDENCIES: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];
const CARGO_DEPENDENCIES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A workspace manifest: its package name (npm only) and its dependencies, each a variable
/// named after the dependency whose value is the package name (`by_name`) or the local path.
#[derive(Debug)]
pub(super) struct Manifest {
    pub(super) file: String,
    name: Option<String>,
    deps: Vec<Var>,
    by_name: bool,
}

/// The Cargo or Go manifest `file`.
pub(super) fn parse(file: &str, text: &str) -> Option<Manifest> {
    match file.rsplit('/').next()? {
        CARGO_TOML => cargo_toml(file, text),
        GO_MOD => Some(go_mod(file, text)),
        _ => None,
    }
}

/// The npm workspace members of the checkout at `root`, as the workspace inventory finds them,
/// when the root declares a workspace (`workspaces` in its `package.json`, or
/// `pnpm-workspace.yaml`). A name two members share is left out.
pub(super) fn npm(root: &Path) -> Vec<Manifest> {
    let declared = read_file(root, PACKAGE_JSON)
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|package| package.get("workspaces").is_some())
        || root.join(PNPM_WORKSPACE).is_file();
    let Some(inventory) = declared.then(|| workspaces::discover(root).ok()).flatten() else {
        return Vec::new();
    };
    let mut names: HashMap<&str, usize> = HashMap::new();
    for name in inventory.packages.iter().filter_map(|p| p.name.as_deref()) {
        *names.entry(name).or_default() += 1;
    }
    (inventory.packages.iter())
        .filter_map(|package| {
            let dir = if package.root == "." {
                ""
            } else {
                &package.root
            };
            let file = join(dir, PACKAGE_JSON);
            let mut manifest = package_json(&file, &read_file(root, &file)?)?;
            manifest.name = manifest
                .name
                .filter(|name| names.get(name.as_str()) == Some(&1));
            Some(manifest)
        })
        .collect()
}

fn package_json(file: &str, text: &str) -> Option<Manifest> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let lines = Lines::new(text);
    let tables = (NPM_DEPENDENCIES.iter()).filter_map(|t| parsed.get(t)?.as_object());
    let deps = (tables.flat_map(Map::keys))
        .map(|name| Var {
            name: name.clone(),
            value: name.clone(),
            file: file.to_owned(),
            line: lines.key(1, name),
        })
        .collect();
    let name = parsed
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(Manifest {
        file: file.to_owned(),
        name,
        deps,
        by_name: true,
    })
}

/// A Cargo manifest's path dependencies, in its dependency tables, the workspace's and each
/// target's.
fn cargo_toml(file: &str, text: &str) -> Option<Manifest> {
    let parsed: Value = toml::from_str(text).ok()?;
    let lines = Lines::new(text);
    let tables_of = |v: &Value| -> Vec<Value> {
        (CARGO_DEPENDENCIES.iter())
            .filter_map(|t| v.get(t).cloned())
            .collect()
    };
    let mut tables = tables_of(&parsed);
    tables.extend(
        parsed
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .cloned(),
    );
    let targets = parsed.get("target").and_then(Value::as_object);
    for target in targets.into_iter().flat_map(Map::values) {
        tables.extend(tables_of(target));
    }
    let deps = (tables.iter().filter_map(Value::as_object).flatten())
        .filter_map(|(name, spec)| {
            let path = spec.get("path")?.as_str()?;
            Some(Var {
                name: name.clone(),
                value: path.to_owned(),
                file: file.to_owned(),
                line: lines
                    .find(1, |l| l.contains("path") && l.contains(path))
                    .unwrap_or(1),
            })
        })
        .collect();
    Some(Manifest {
        file: file.to_owned(),
        name: None,
        deps,
        by_name: false,
    })
}

/// A Go module's `replace` directives to a local path, alone or in a block.
fn go_mod(file: &str, text: &str) -> Manifest {
    let mut deps = Vec::new();
    let mut block = false;
    for (at, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or_default().trim();
        let directive = if block {
            block = line != ")";
            line
        } else if let Some(rest) = line.strip_prefix("replace").map(str::trim_start) {
            block = rest == "(";
            rest
        } else {
            continue;
        };
        let Some((module, target)) = directive.split_once("=>") else {
            continue;
        };
        let target = target.split_whitespace().next().unwrap_or_default();
        if target.starts_with("./") || target.starts_with("../") {
            deps.push(Var {
                name: module
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                value: target.to_owned(),
                file: file.to_owned(),
                line: at + 1,
            });
        }
    }
    Manifest {
        file: file.to_owned(),
        name: None,
        deps,
        by_name: false,
    }
}

/// A dependency on another workspace package, by its name or a local path, is a hop from the
/// depending package's folder to the dependency's.
pub(super) fn link(infra: &mut Infra, manifests: &[Manifest]) {
    let named: HashMap<&str, &str> = (manifests.iter())
        .filter_map(|m| Some((m.name.as_deref()?, dir_of(&m.file))))
        .collect();
    for manifest in manifests {
        let from = dir_of(&manifest.file);
        for dep in &manifest.deps {
            let to = if manifest.by_name {
                named.get(dep.value.as_str()).map(|dir| (*dir).to_owned())
            } else {
                normalize(from, &dep.value)
            };
            if let Some(to) = to.filter(|to| to != from) {
                let ends = (End::Dir(from.to_owned()), End::Dir(to));
                infra.hop(ends, Evidence::BuildDependency, dep, &dep.value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hop_names, named};
    use super::*;
    use crate::collect::TempDir;
    use std::fs;

    #[test]
    fn manifests_depend_on_packages_by_name_or_local_path() {
        let app = r#"{"name": "@x/app", "dependencies": {"@x/lib": "*", "react": "18"}}"#;
        let lib = r#"{"name": "@x/lib", "devDependencies": {"@x/app": "*"}}"#;
        let cargo = "[package]
name = \"cli\"

[dependencies]
core = { path = \"../core\" }
serde = \"1\"

[target.'cfg(unix)'.dependencies.sys]
path = \"../sys\"
";
        let go = "module example.com/svc

replace example.com/a => ../a // local
replace (
\texample.com/b v1.0.0 => ./b
\texample.com/c => example.com/d v1.2.0
)
";
        let mut manifests: Vec<Manifest> = [
            ("packages/app/package.json", app),
            ("packages/lib/package.json", lib),
        ]
        .iter()
        .filter_map(|(file, text)| package_json(file, text))
        .collect();
        manifests.extend(parse("crates/cli/Cargo.toml", cargo));
        manifests.extend(parse("svc/go.mod", go));
        let mut infra = Infra::default();
        link(&mut infra, &manifests);
        let (app, lib) = ("packages/app/package.json", "packages/lib/package.json");
        let (cargo, go) = ("crates/cli/Cargo.toml", "svc/go.mod");
        assert_eq!(
            hop_names(&infra),
            [
                named("packages/app/", "packages/lib/", app, 1, "@x/lib", "@x/lib"),
                named("packages/lib/", "packages/app/", lib, 1, "@x/app", "@x/app"),
                named("crates/cli/", "crates/core/", cargo, 5, "core", "../core"),
                named("crates/cli/", "crates/sys/", cargo, 9, "sys", "../sys"),
                named("svc/", "a/", go, 3, "example.com/a", "../a"),
                named("svc/", "svc/b/", go, 5, "example.com/b", "./b"),
            ]
        );
        assert!((infra.hops.iter()).all(|h| h.evidence == Evidence::BuildDependency));
    }

    #[test]
    fn npm_reads_declared_members_and_drops_shared_names() {
        let dir = TempDir::new("infra-npm").unwrap();
        let write = |path: &str, text: &str| {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        };
        write(
            "package.json",
            r#"{"name": "mono", "workspaces": ["packages/*"]}"#,
        );
        write(
            "packages/a/package.json",
            r#"{"name": "a", "dependencies": {"b": "*"}}"#,
        );
        write("packages/b/package.json", r#"{"name": "b"}"#);
        write("packages/c/package.json", r#"{"name": "b"}"#);
        write(
            "tools/x/package.json",
            r#"{"name": "x", "dependencies": {"a": "*"}}"#,
        );
        let found = npm(dir.path());
        let mut files: Vec<(&str, Option<&str>)> = (found.iter())
            .map(|m| (m.file.as_str(), m.name.as_deref()))
            .collect();
        files.sort();
        assert_eq!(
            files,
            [
                ("package.json", Some("mono")),
                ("packages/a/package.json", Some("a")),
                ("packages/b/package.json", None),
                ("packages/c/package.json", None),
            ]
        );
        let mut infra = Infra::default();
        link(&mut infra, &found);
        assert!(infra.hops.is_empty());

        // Without a declared workspace no package is a member.
        write("package.json", r#"{"name": "mono"}"#);
        assert!(npm(dir.path()).is_empty());
    }
}
