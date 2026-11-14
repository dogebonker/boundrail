//! Infrastructure evidence: the deployable units that docker-compose, Kubernetes and Terraform
//! files declare, the configured addresses by which one unit reaches another, and the build
//! dependencies between workspace packages. The parsers are pure functions over file contents;
//! [`read`] finds the files in a checkout and links what they declare.

mod compose;
mod kubernetes;
mod manifests;
mod terraform;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::segments;
use super::types::{Binding, Evidence};
use crate::collect::excludes::{Options, Policy};
use crate::model::Language;
use crate::modules::{dir_of, join};

/// Base compose file names, read wherever compose files are read.
const COMPOSE_NAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];
/// `docker-compose*.yml` and `docker-compose*.yaml`: overrides besides the base names.
const COMPOSE_PREFIX: &str = "docker-compose";
/// How many folders deep below the checkout root compose files are read.
const COMPOSE_MAX_DEPTH: usize = 2;
/// Folders whose YAML files, at any depth below them, are read as Kubernetes manifests.
const K8S_DIRS: [&str; 4] = ["k8s", "kubernetes", "deploy", "manifests"];
/// Helm charts hold templates, not manifests: nothing below this folder is read.
const HELM_DIR: &str = "charts";
const YAML_EXTENSIONS: [&str; 2] = [".yaml", ".yml"];
const TERRAFORM_EXTENSION: &str = ".tf";
/// Manifests read as candidate files; `package.json` packages come from the workspace inventory.
const PATH_MANIFESTS: [&str; 2] = [manifests::CARGO_TOML, manifests::GO_MOD];
/// Dependency folders never searched, besides hidden ones. Everything else the walk finds goes
/// through the scan's exclusion policy.
const DEPENDENCY_DIRS: [&str; 3] = ["node_modules", "vendor", "target"];
const WALK_MAX_DEPTH: usize = 8;
/// Variable name endings whose value may be a host alone, without a scheme or port.
const HOST_VARIABLES: [&str; 4] = ["HOST", "HOSTNAME", "ADDR", "ADDRESS"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitKind {
    ComposeService,
    K8sWorkload,
    TerraformResource,
}

/// A deployable unit an infrastructure file declares.
#[derive(Debug, Clone, Serialize)]
pub struct Unit {
    pub kind: UnitKind,
    /// The service or workload name, or `TYPE.NAME` of a Terraform resource.
    pub name: String,
    /// The file declaring it; the base file of a compose service several files declare.
    pub file: String,
    /// The directory a compose service builds from.
    pub build: Option<String>,
}

/// One end of a hop: a unit by its index, or a package directory.
#[derive(Debug)]
pub enum End {
    Unit(usize),
    Dir(String),
}

/// A unit that names another's address, or a package that depends on another.
#[derive(Debug)]
pub struct Hop {
    pub from: End,
    pub to: End,
    /// `configured-path` or `build-dependency`.
    pub evidence: Evidence,
    /// Where the address or dependency is written.
    pub file: String,
    pub line: usize,
    /// The variable, attribute or dependency that holds it.
    pub variable: String,
    /// The host, Terraform reference, package name or path as written.
    pub address: String,
}

/// The units and hops of one checkout.
#[derive(Debug, Default)]
pub struct Infra {
    pub units: Vec<Unit>,
    pub hops: Vec<Hop>,
    /// The folders of the workspace packages read.
    pub packages: Vec<String>,
    /// The compose, Kubernetes and Terraform files read and parsed.
    pub files: HashSet<String>,
    /// Why the files could not be read; nothing else is set then.
    pub error: Option<String>,
    /// Each unit's index by kind, folder and name.
    keys: HashMap<(UnitKind, String, String), usize>,
}

/// The infrastructure as modules.
pub(super) struct Joined<'i, 'x> {
    /// Each unit's owner.
    pub(super) owners: Vec<Option<&'i str>>,
    /// The owners of the workspace package folders.
    pub(super) packages: Vec<&'i str>,
    /// Each hop between two different modules, with its source's and its target's owner.
    pub(super) hops: Vec<(&'i str, &'i str, &'x Hop)>,
}

/// A variable and its value where a file sets it.
#[derive(Debug, Clone)]
struct Var {
    name: String,
    value: String,
    file: String,
    line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Compose,
    Kubernetes,
    Terraform,
    Manifest,
}

/// How the collector reads the repository path `path`, if at all.
fn classify(path: &str) -> Option<FileKind> {
    let mut dirs = segments(path);
    let name = dirs.pop()?;
    let yaml = YAML_EXTENSIONS.iter().any(|e| name.ends_with(e));
    if yaml && (COMPOSE_NAMES.contains(&name) || name.starts_with(COMPOSE_PREFIX)) {
        return (dirs.len() <= COMPOSE_MAX_DEPTH).then_some(FileKind::Compose);
    }
    if yaml && !dirs.contains(&HELM_DIR) && dirs.iter().any(|d| K8S_DIRS.contains(d)) {
        return Some(FileKind::Kubernetes);
    }
    if name.ends_with(TERRAFORM_EXTENSION) {
        return Some(FileKind::Terraform);
    }
    PATH_MANIFESTS.contains(&name).then_some(FileKind::Manifest)
}

/// Whether a file named `name` could be a candidate, before its folder is known.
fn candidate_name(name: &str) -> bool {
    YAML_EXTENSIONS.iter().any(|e| name.ends_with(e))
        || name.ends_with(TERRAFORM_EXTENSION)
        || PATH_MANIFESTS.contains(&name)
}

/// The kind of unit `binding` names: a symbol in a compose, Kubernetes or Terraform file of this
/// repository that the collector reads, the service, workload or `TYPE.NAME` it declares.
fn unit_kind(binding: &Binding) -> Option<UnitKind> {
    if binding.repo.is_some() || binding.symbol.is_none() {
        return None;
    }
    match classify(&binding.path)? {
        FileKind::Compose => Some(UnitKind::ComposeService),
        FileKind::Kubernetes => Some(UnitKind::K8sWorkload),
        FileKind::Terraform => Some(UnitKind::TerraformResource),
        FileKind::Manifest => None,
    }
}

/// Whether `binding` names an infrastructure unit rather than code.
pub(super) fn is_unit_binding(binding: &Binding) -> bool {
    unit_kind(binding).is_some()
}

impl Infra {
    /// The infrastructure of these units, hops and package folders.
    pub(super) fn new(units: Vec<Unit>, hops: Vec<Hop>, packages: Vec<String>) -> Self {
        let mut infra = Self {
            units,
            hops,
            packages,
            ..Self::default()
        };
        for (at, unit) in infra.units.iter().enumerate() {
            let key = (unit.kind, dir_of(&unit.file).to_owned(), unit.name.clone());
            infra.keys.entry(key).or_insert(at);
        }
        infra
    }

    /// No infrastructure, because reading it failed with `error`.
    pub fn failed(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    /// The unit a unit binding names: one of the binding file's kind, declared in its folder
    /// (a compose project or a Terraform module) under the binding's symbol.
    pub(super) fn bound(&self, binding: &Binding) -> Option<usize> {
        let kind = unit_kind(binding)?;
        let path = segments(&binding.path).join("/");
        let key = (kind, dir_of(&path).to_owned(), binding.symbol.clone()?);
        self.keys.get(&key).copied()
    }

    /// The infrastructure as modules. A unit belongs to the first module of `bound`, as
    /// (qualified ID, binding), bound to it, else a compose service to the owner of the
    /// directory it builds from; package folders belong to their `dir_owner`.
    pub(super) fn join<'i, 'b>(
        &self,
        bound: impl IntoIterator<Item = (&'i str, &'b Binding)>,
        dir_owner: impl Fn(&str) -> Option<&'i str>,
    ) -> Joined<'i, '_> {
        let mut owners = vec![None; self.units.len()];
        for (id, binding) in bound {
            if let Some(unit) = self.bound(binding) {
                owners[unit].get_or_insert(id);
            }
        }
        for (owner, unit) in owners.iter_mut().zip(&self.units) {
            if owner.is_none() {
                *owner = unit.build.as_deref().and_then(&dir_owner);
            }
        }
        let end = |end: &End| match end {
            End::Unit(unit) => owners[*unit],
            End::Dir(dir) => dir_owner(dir),
        };
        let hops = (self.hops.iter())
            .filter_map(|hop| {
                let (from, to) = (end(&hop.from)?, end(&hop.to)?);
                (from != to).then_some((from, to, hop))
            })
            .collect();
        let packages = self.packages.iter().filter_map(|d| dir_owner(d)).collect();
        Joined {
            owners,
            packages,
            hops,
        }
    }

    fn unit(&mut self, kind: UnitKind, name: &str, file: &str, build: Option<String>) -> usize {
        self.units.push(Unit {
            kind,
            name: name.to_owned(),
            file: file.to_owned(),
            build,
        });
        self.units.len() - 1
    }

    fn hop(&mut self, ends: (End, End), evidence: Evidence, var: &Var, address: &str) {
        self.hops.push(Hop {
            from: ends.0,
            to: ends.1,
            evidence,
            file: var.file.clone(),
            line: var.line,
            variable: var.name.clone(),
            address: address.to_owned(),
        });
    }
}

/// What the files of a checkout declare, before linking.
#[derive(Default)]
struct Declared {
    compose: Vec<(String, Vec<compose::Service>)>,
    kubernetes: Vec<kubernetes::Object>,
    terraform: Vec<(String, terraform::Resource)>,
    manifests: Vec<manifests::Manifest>,
}

/// The infrastructure of the checkout at `root`: the candidate files the scan's exclusion policy
/// keeps, generated folders included (tests, fixtures, examples and ignored files are left out),
/// and the `package.json` packages of the workspace inventory. A file that does not read or
/// parse, or lies past a symbolic link, is left out.
pub fn read(root: &Path) -> Result<Infra> {
    let mut found = Vec::new();
    walk(root, "", 0, &mut found);
    let options = Options {
        include_generated: true,
        ..Options::default()
    };
    let mut files = Policy::new(root, Language::Typescript, options)?.select(&found)?;
    // Base compose files first, so overrides merge into them in order.
    files.sort_by_cached_key(|path| (override_file(path), path.clone()));
    let mut declared = Declared::default();
    let mut env_files = HashMap::new();
    let mut parsed = HashSet::new();
    for path in &files {
        let Some(text) = read_file(root, path) else {
            continue;
        };
        match classify(path) {
            Some(FileKind::Compose) => {
                let Some(services) = compose::parse(path, &text) else {
                    continue;
                };
                let services = compose::with_env_files(root, services, &mut env_files);
                declared.compose.push((path.clone(), services));
            }
            Some(FileKind::Kubernetes) => {
                declared.kubernetes.extend(kubernetes::parse(path, &text))
            }
            Some(FileKind::Terraform) => {
                let resources = terraform::parse(path, &text).into_iter();
                (declared.terraform).extend(resources.map(|r| (path.clone(), r)));
            }
            Some(FileKind::Manifest) => {
                declared.manifests.extend(manifests::parse(path, &text));
                continue;
            }
            None => continue,
        }
        parsed.insert(path.clone());
    }
    declared.manifests.extend(manifests::npm(root));
    let mut infra = declared.link();
    infra.files = parsed;
    Ok(infra)
}

/// Whether `path` is a compose override rather than a base file.
fn override_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    classify(path) == Some(FileKind::Compose) && !COMPOSE_NAMES.contains(&name)
}

/// The text of the repository file `path`, or `None` when it does not read or a component of
/// its path is a symbolic link.
fn read_file(root: &Path, path: &str) -> Option<String> {
    let mut at = root.to_path_buf();
    for part in segments(path) {
        at.push(part);
        if fs::symlink_metadata(&at).ok()?.file_type().is_symlink() {
            return None;
        }
    }
    fs::read_to_string(at).ok()
}

/// Every file below `dir` that [`classify`] reads, skipping hidden and dependency folders and
/// following no symbolic link.
fn walk(root: &Path, dir: &str, depth: usize, found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let (Ok(kind), Some(name)) = (entry.file_type(), name.to_str()) else {
            continue;
        };
        if kind.is_dir() {
            let skipped = name.starts_with('.') || DEPENDENCY_DIRS.contains(&name);
            if !skipped && depth < WALK_MAX_DEPTH {
                walk(root, &join(dir, name), depth + 1, found);
            }
        } else if kind.is_file() && candidate_name(name) {
            let path = join(dir, name);
            if classify(&path).is_some() {
                found.push(path);
            }
        }
    }
}

impl Declared {
    fn link(self) -> Infra {
        let mut infra = Infra::default();
        compose::link(&mut infra, self.compose);
        kubernetes::link(&mut infra, &self.kubernetes);
        terraform::link(&mut infra, &self.terraform);
        manifests::link(&mut infra, &self.manifests);
        let packages = (self.manifests.iter())
            .map(|m| dir_of(&m.file).to_owned())
            .collect();
        Infra::new(infra.units, infra.hops, packages)
    }
}

/// A file's lines, split once.
struct Lines<'t>(Vec<&'t str>);

impl<'t> Lines<'t> {
    fn new(text: &'t str) -> Self {
        Self(text.lines().collect())
    }

    /// The 1-based number of the first line at or after line `from` that `wanted` accepts.
    fn find(&self, from: usize, wanted: impl Fn(&str) -> bool) -> Option<usize> {
        (self.0.iter().enumerate())
            .skip(from.saturating_sub(1))
            .find(|(_, line)| wanted(line))
            .map(|(at, _)| at + 1)
    }

    /// The first line at or after line `from` that sets `key`, else `from`.
    fn key(&self, from: usize, key: &str) -> usize {
        self.find(from, |line| sets(line, key)).unwrap_or(from)
    }
}

/// Whether `line` sets `key`: `key:` or `key=` at its start, after any list dash, quotes
/// allowed around the key.
fn sets(line: &str, key: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix('-').map_or(line, str::trim_start);
    let line = line.strip_prefix(['"', '\'']).unwrap_or(line);
    let Some(rest) = line.strip_prefix(key) else {
        return false;
    };
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest).trim_start();
    rest.starts_with([':', '='])
}

/// `relative` resolved against the repository directory `dir`, or `None` when it is absolute
/// or leaves the repository.
fn normalize(dir: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') {
        return None;
    }
    let mut parts = segments(dir);
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// A YAML or JSON scalar as text; `None` for null, maps and lists.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Whether the variable `name` may hold a host alone.
fn host_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    HOST_VARIABLES.iter().any(|suffix| name.ends_with(suffix))
}

/// The hosts `value` names, as whole host names. Parts split at whitespace, commas and
/// semicolons. A part counts as a URL's host (after `://`, past any user info, before the port
/// and path); as `host:port` with a numeric port when the value is that one part or holds a URL,
/// so `mongodb://a:1,b:2` names both hosts but `billing:3,search:1` none; or, with `bare`, as a
/// host alone. A part with `@` but no scheme, such as an email address, names none.
fn hosts(value: &str, bare: bool) -> Vec<&str> {
    let parts: Vec<&str> = (value.split(|c: char| c.is_whitespace() || c == ',' || c == ';'))
        .filter(|part| !part.is_empty())
        .collect();
    let ported = parts.len() == 1 || value.contains("://");
    (parts.into_iter())
        .filter_map(|part| {
            let (scheme, rest) = match part.split_once("://") {
                Some((_, rest)) => (true, rest),
                None if part.contains('@') => return None,
                None => (false, part),
            };
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let authority = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            let numeric =
                port.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
            let named = !host.is_empty()
                && (host.bytes())
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
            let counts = scheme || (numeric && ported) || (bare && port.is_none());
            (named && counts).then_some(host)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::TempDir;

    /// A hop as (from, to, file, line, variable, address), units by name, folders with `/`.
    pub(super) type Named = (String, String, String, usize, String, String);

    pub(super) fn hop_names(infra: &Infra) -> Vec<Named> {
        let end = |end: &End| match end {
            End::Unit(unit) => infra.units[*unit].name.clone(),
            End::Dir(dir) => format!("{dir}/"),
        };
        (infra.hops.iter())
            .map(|h| {
                let (file, variable) = (h.file.clone(), h.variable.clone());
                (
                    end(&h.from),
                    end(&h.to),
                    file,
                    h.line,
                    variable,
                    h.address.clone(),
                )
            })
            .collect()
    }

    pub(super) fn named(
        from: &str,
        to: &str,
        file: &str,
        line: usize,
        variable: &str,
        address: &str,
    ) -> Named {
        let owned = |s: &str| s.to_owned();
        (
            owned(from),
            owned(to),
            owned(file),
            line,
            owned(variable),
            owned(address),
        )
    }

    /// A variable as (name, value, file, line).
    pub(super) fn var_of(var: &Var) -> (&str, &str, &str, usize) {
        (&var.name, &var.value, &var.file, var.line)
    }

    pub(super) fn var(name: &str, value: &str, file: &str, line: usize) -> Var {
        Var {
            name: name.to_owned(),
            value: value.to_owned(),
            file: file.to_owned(),
            line,
        }
    }

    #[test]
    fn hosts_are_whole_names_of_urls_host_ports_and_host_variables() {
        assert_eq!(hosts("amqp://guest@rabbit:5672/vhost", false), ["rabbit"]);
        assert_eq!(hosts("http://abc:8080/b", false), ["abc"]);
        assert_eq!(hosts("db:5432", false), ["db"]);
        assert_eq!(hosts("mongodb://a:1,b:2", false), ["a", "b"]);
        assert_eq!(hosts("http://api:${PORT}/x", false), ["api"]);
        // Weights and email addresses are not hosts.
        assert!(hosts("billing:3,search:1", false).is_empty());
        assert!(hosts("help@billing", true).is_empty());
        assert!(hosts("user@b:5432", false).is_empty());
        // Without a scheme or a numeric port, a host counts only where a host is expected.
        assert!(hosts("redis", false).is_empty());
        assert_eq!(hosts("redis", true), ["redis"]);
        assert!(hosts("db:${PORT}", false).is_empty());
        assert!(hosts("/var/b:1", false).is_empty());
        assert!(host_variable("db_host") && host_variable("REDIS_ADDR"));
        assert!(!host_variable("QUEUE_URL"));
    }

    #[test]
    fn keys_are_set_only_at_the_start_of_a_line() {
        for line in ["API: x", "  - API=x", "\"API\": x", "API : x"] {
            assert!(sets(line, "API"), "{line}");
        }
        for line in ["  OTHER: http://API:1", "APIS: x", "# API: x", "x: API"] {
            assert!(!sets(line, "API"), "{line}");
        }
        let lines = Lines::new("a: 1\nb:\n  API_URL: x\nAPI: y\n");
        assert_eq!((lines.key(1, "API"), lines.key(5, "API")), (4, 5));
    }

    #[test]
    fn read_keeps_what_the_exclusion_policy_keeps_and_binds_units_by_folder_and_name() {
        let dir = TempDir::new("infra-read").unwrap();
        let write = |path: &str, text: &str| {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        };
        let compose = "services:\n  api:\n    env_file: api.env\n  db: {image: postgres}\n";
        write("docker-compose.yml", compose);
        write("api.env", "DATABASE_URL=postgres://u:p@db:5432/app\n");
        write("a/b/c/docker-compose.yml", "services:\n  deep: {}\n");
        write(
            "charts/x/templates/d.yaml",
            "kind: Deployment\nmetadata: {name: helm}\n",
        );
        write(
            "test/fixtures/main.tf",
            "resource \"null_resource\" \"x\" {}\n",
        );
        write("local/docker-compose.yml", "services:\n  pg: {}\n");
        write(".gitignore", "local/\n");
        write(
            "build/infra/main.tf",
            "resource \"aws_s3_bucket\" \"logs\" {}\n",
        );
        let infra = read(dir.path()).unwrap();
        let units: Vec<&str> = infra.units.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(units, ["api", "db", "aws_s3_bucket.logs"]);
        let url = "DATABASE_URL";
        assert_eq!(
            hop_names(&infra),
            [named("api", "db", "api.env", 1, url, "db")]
        );
        assert!(infra.packages.is_empty());

        let binding = |path: &str, symbol: &str| Binding {
            repo: None,
            path: path.to_owned(),
            symbol: Some(symbol.to_owned()),
        };
        assert_eq!(infra.bound(&binding("./docker-compose.yml", "db")), Some(1));
        assert_eq!(infra.bound(&binding("compose.yaml", "db")), Some(1));
        let other = binding("build/infra/other.tf", "aws_s3_bucket.logs");
        assert_eq!(infra.bound(&other), Some(2));
        assert_eq!(infra.bound(&binding("build/infra/main.tf", "logs")), None);
        assert!(!is_unit_binding(&binding("src/app.ts", "api")));
        assert!(!is_unit_binding(&binding(
            "a/b/c/docker-compose.yml",
            "deep"
        )));
        assert!(is_unit_binding(&binding("deploy/api.yml", "api")));
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_env_files_past_a_symbolic_link() {
        let dir = TempDir::new("infra-link").unwrap();
        let outside = TempDir::new("infra-outside").unwrap();
        fs::write(outside.path().join("real.env"), "DB=postgres://db:5432/x\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let compose = "services:\n  api:\n    env_file: linked/real.env\n  db: {}\n";
        fs::write(dir.path().join("compose.yaml"), compose).unwrap();
        let infra = read(dir.path()).unwrap();
        assert_eq!(infra.units.len(), 2);
        assert!(infra.hops.is_empty());
    }
}
