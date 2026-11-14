//! The on-disk vault `.boundrail/model/`: discovery, creation, loading with validation, and
//! the read views of the loaded model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use super::revision::{self, Revision};
use super::types::{
    Anchor, Binding, DESCRIPTION_MAX, Emphasis, LABEL_MAX, Manifest, Module, RESPONSIBILITY_MAX,
    Relationship, SCHEMA_VERSION, State, VaultConfig,
};
use super::{Code, Refusal, ids};
use crate::history::{refuse_symlink, write_atomic};

/// Boundrail's state folder at the checkout root, and the vault folder inside it.
const STATE: &str = ".boundrail";
const VAULT: &str = "model";
const MANIFEST: &str = "model.json";
const CONFIG: &str = "config.yaml";
const MODULES: &str = "modules";
const MODULE_SUFFIX: &str = ".module.json";
const CURRENT: &str = "Current";
const NAME_MAX: usize = 120;

#[derive(Debug)]
pub struct Model {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub config: VaultConfig,
    /// Sorted by local ID.
    pub modules: Vec<Module>,
    pub revision: Revision,
}

/// One reason a vault does not load: the vault-relative file and the ID or key it concerns.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub file: String,
    pub subject: String,
    pub message: String,
}

#[derive(Default)]
struct Problems(Vec<Problem>);

impl Problems {
    fn push(&mut self, file: &str, subject: &str, message: impl Into<String>) {
        self.0.push(Problem {
            file: file.to_owned(),
            subject: subject.to_owned(),
            message: message.into(),
        });
    }
}

/// The top-level directory of the Git work tree containing `start`.
fn checkout_root(start: &Path) -> Result<PathBuf> {
    crate::git::toplevel(start).map_err(|error| {
        Refusal::new(
            Code::CheckoutNotFound,
            format!(
                "{} is not inside a Git checkout: {error:#}",
                start.display()
            ),
        )
        .into()
    })
}

/// Pretty JSON with a trailing newline, the form every vault JSON file is written in.
fn file_json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

/// A model name trimmed of surrounding space: non-empty, at most 120 characters, no controls.
fn valid_name(name: &str) -> Result<&str> {
    let name = name.trim();
    let count = name.chars().count();
    if count == 0 || count > NAME_MAX || name.chars().any(char::is_control) {
        return Err(Refusal::new(
            Code::NameInvalid,
            format!("a model name needs 1 to {NAME_MAX} characters and no control characters"),
        )
        .into());
    }
    Ok(name)
}

/// Creates the vault with a manifest, its current state and the default vocabulary.
pub fn init(start: &Path, name: &str) -> Result<Model> {
    let name = valid_name(name)?;
    let state = checkout_root(start)?.join(STATE);
    let dir = state.join(VAULT);
    refuse_symlink(&state)?;
    fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
    refuse_symlink(&dir)?;
    // Creating the folder claims the vault, so two concurrent inits cannot both write it.
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            return Err(Refusal::new(
                Code::VaultExists,
                format!("a model vault already exists at {}", dir.display()),
            )
            .into());
        }
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    let created = populate(&dir, name).and_then(|()| load_dir(dir.clone()));
    if created.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    created
}

fn populate(dir: &Path, name: &str) -> Result<()> {
    fs::create_dir(dir.join(MODULES)).with_context(|| format!("creating {MODULES}"))?;
    let id = ids::allocate(|_| false)?;
    let current = ids::allocate(|candidate| candidate == id)?;
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        id,
        name: name.to_owned(),
        current: State {
            id: current,
            name: CURRENT.to_owned(),
        },
        retired_ids: Vec::new(),
    };
    let config = serde_yaml_ng::to_string(&VaultConfig::default())?;
    write_atomic(&dir.join(CONFIG), config.as_bytes())?;
    write_atomic(&dir.join(MANIFEST), file_json(&manifest)?.as_bytes())
}

/// Loads the vault of the checkout around `start`, refusing a model with any problem.
pub fn load(start: &Path) -> Result<Model> {
    load_dir(checkout_root(start)?.join(STATE).join(VAULT))
}

fn load_dir(dir: PathBuf) -> Result<Model> {
    // Each file is read once: the revision and the parsed model come from the same bytes.
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut problems = Problems::default();
    for path in [MANIFEST, CONFIG] {
        match fs::read(dir.join(path)) {
            Ok(bytes) => inputs.push((path.to_owned(), bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound && path == MANIFEST => {
                return Err(Refusal::new(
                    Code::VaultNotFound,
                    format!(
                        "no model vault at {}; create one with `boundrail model init --name <name>`",
                        dir.display()
                    ),
                )
                .into());
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                problems.push(path, path, "file is missing");
            }
            Err(e) => return Err(e).with_context(|| format!("reading {path}")),
        }
    }
    let modules_dir = dir.join(MODULES);
    let mut names = Vec::new();
    match fs::read_dir(&modules_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if let Some(name) = entry.file_name().to_str()
                    && name.ends_with(MODULE_SUFFIX)
                    && entry.file_type()?.is_file()
                {
                    names.push(name.to_owned());
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {MODULES}")),
    }
    names.sort();
    for name in names {
        let bytes = fs::read(modules_dir.join(&name))
            .with_context(|| format!("reading {MODULES}/{name}"))?;
        inputs.push((format!("{MODULES}/{name}"), bytes));
    }
    let revision = revision::of(inputs.iter().map(|(p, b)| (p.as_str(), b.as_slice())));

    let mut manifest = None;
    let mut config = None;
    let mut modules = Vec::new();
    for (path, bytes) in &inputs {
        let parsed = match path.as_str() {
            MANIFEST => match serde_json::from_slice(bytes) {
                Ok(m) => {
                    manifest = Some(m);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            CONFIG => match serde_yaml_ng::from_slice(bytes) {
                Ok(c) => {
                    config = Some(c);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            _ => match serde_json::from_slice::<Module>(bytes) {
                Ok(m) => {
                    modules.push((path.as_str(), m));
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
        };
        if let Err(message) = parsed {
            problems.push(
                path,
                file_subject(path),
                format!("does not parse: {message}"),
            );
        }
    }
    check_ids(manifest.as_ref(), &modules, &mut problems);
    for (file, module) in &modules {
        check_module(file, module, config.as_ref(), &mut problems);
    }
    if let Some(m) = &manifest {
        check_containment(&m.id, &modules, &mut problems);
    }
    let problems = problems.0;
    match (manifest, config) {
        (Some(manifest), Some(config)) if problems.is_empty() => {
            let mut modules: Vec<Module> = modules.into_iter().map(|(_, m)| m).collect();
            modules.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(Model {
                dir,
                manifest,
                config,
                modules,
                revision,
            })
        }
        _ => Err(Refusal::new(
            Code::ModelInvalid,
            format!(
                "the model at {} has {} problem{}",
                dir.display(),
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            ),
        )
        .with_details(json!({ "problems": problems }))
        .into()),
    }
}

fn file_subject(path: &str) -> &str {
    path.strip_prefix(&format!("{MODULES}/"))
        .and_then(|name| name.strip_suffix(MODULE_SUFFIX))
        .unwrap_or(path)
}

/// Every allocated ID (model, state, module, relationship) shares one namespace: each must be
/// well formed, unique and not retired.
fn check_ids(manifest: Option<&Manifest>, modules: &[(&str, Module)], problems: &mut Problems) {
    let retired: BTreeSet<&str> = manifest
        .map(|m| m.retired_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let mut ids: Vec<(&str, &str)> = Vec::new();
    if let Some(m) = manifest {
        if m.schema_version != SCHEMA_VERSION {
            problems.push(
                MANIFEST,
                &m.id,
                format!(
                    "schema_version {} is not {SCHEMA_VERSION}",
                    m.schema_version
                ),
            );
        }
        ids.push((MANIFEST, &m.id));
        ids.push((MANIFEST, &m.current.id));
        for id in &m.retired_ids {
            if !ids::is_local(id) {
                problems.push(MANIFEST, id, "retired ID is not 8 base62 characters");
            }
        }
    }
    for (file, module) in modules {
        ids.push((file, &module.id));
        ids.extend(module.outgoing.iter().map(|r| (*file, r.id.as_str())));
    }
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (file, id) in ids {
        if !ids::is_local(id) {
            problems.push(file, id, "ID is not 8 base62 characters");
        }
        if retired.contains(id) {
            problems.push(file, id, "ID is retired");
        }
        if let Some(first) = seen.insert(id, file) {
            problems.push(file, id, format!("ID is also used in {first}"));
        }
    }
}

/// One module's own fields: version, file name, lengths and vocabulary.
fn check_module(
    file: &str,
    module: &Module,
    config: Option<&VaultConfig>,
    problems: &mut Problems,
) {
    let id = module.id.as_str();
    if module.schema_version != SCHEMA_VERSION {
        problems.push(
            file,
            id,
            format!(
                "schema_version {} is not {SCHEMA_VERSION}",
                module.schema_version
            ),
        );
    }
    if file_subject(file) != id {
        problems.push(file, id, format!("file name does not match ID {id}"));
    }
    let mut limit = |field: &str, value: Option<&str>, max: usize, subject: &str| {
        if let Some(n) = value.map(|v| v.chars().count())
            && n > max
        {
            problems.push(
                file,
                subject,
                format!("{field} has {n} characters, more than {max}"),
            );
        }
    };
    limit(
        "responsibility",
        module.responsibility.as_deref(),
        RESPONSIBILITY_MAX,
        id,
    );
    limit(
        "description",
        module.description.as_deref(),
        DESCRIPTION_MAX,
        id,
    );
    for r in &module.outgoing {
        limit("label", r.label.as_deref(), LABEL_MAX, &r.id);
        limit(
            "description",
            r.description.as_deref(),
            DESCRIPTION_MAX,
            &r.id,
        );
    }
    let Some(config) = config else {
        return;
    };
    if !config.node_kinds.contains_key(&module.kind) {
        problems.push(
            file,
            id,
            format!("kind `{}` is not a node kind in {CONFIG}", module.kind),
        );
    }
    for group in &module.groups {
        if !config.groups.contains_key(group) {
            problems.push(
                file,
                id,
                format!("group `{group}` is not a group in {CONFIG}"),
            );
        }
    }
    for r in &module.outgoing {
        if !config.relationship_kinds.contains_key(&r.kind) {
            problems.push(
                file,
                &r.id,
                format!("kind `{}` is not a relationship kind in {CONFIG}", r.kind),
            );
        }
    }
}

/// Parents and relationship targets name modules of this model, and containment has no cycle.
fn check_containment(model: &str, modules: &[(&str, Module)], problems: &mut Problems) {
    let parents: BTreeMap<String, Option<&str>> = modules
        .iter()
        .map(|(_, module)| (ids::qualify(model, &module.id), module.parent.as_deref()))
        .collect();
    for (file, module) in modules {
        if let Some(parent) = &module.parent
            && !parents.contains_key(parent)
        {
            problems.push(file, &module.id, format!("parent {parent} is not a module"));
        }
        for r in &module.outgoing {
            if !parents.contains_key(&r.to) {
                problems.push(file, &r.id, format!("target {} is not a module", r.to));
            }
        }
        let start = ids::qualify(model, &module.id);
        let mut at = parents.get(&start).copied().flatten();
        for _ in 0..parents.len() {
            match at {
                Some(parent) if parent == start => {
                    problems.push(file, &module.id, "containment cycle through its parent");
                    break;
                }
                Some(parent) => at = parents.get(parent).copied().flatten(),
                None => break,
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ModuleView<'a> {
    pub id: String,
    pub name: &'a str,
    pub kind: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responsibility: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub groups: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<&'a Binding>,
    pub former_names: &'a [String],
    pub children: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RelationshipView<'a> {
    pub id: String,
    pub from: String,
    pub to: &'a str,
    pub kind: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<Emphasis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<&'a Anchor>,
}

/// The whole model: roots and modules in containment order, relationships by source module.
#[derive(Debug, Serialize)]
pub struct Reading<'a> {
    pub roots: Vec<String>,
    pub modules: Vec<ModuleView<'a>>,
    /// Containment depth of each entry in `modules`; roots are 0.
    #[serde(skip)]
    pub depths: Vec<usize>,
    pub relationships: Vec<RelationshipView<'a>>,
}

/// One module with its parts and the relationships that touch it.
#[derive(Debug, Serialize)]
pub struct Selection<'a> {
    pub module: ModuleView<'a>,
    pub parts: Vec<ModuleView<'a>>,
    pub incoming: Vec<RelationshipView<'a>>,
    pub outgoing: Vec<RelationshipView<'a>>,
}

type Children<'a> = BTreeMap<Option<&'a str>, Vec<&'a Module>>;

impl Model {
    pub fn qualify(&self, local: &str) -> String {
        ids::qualify(&self.manifest.id, local)
    }

    /// Modules keyed by their parent's qualified ID (`None` for roots), each list in order.
    fn children(&self) -> Children<'_> {
        let mut children: Children<'_> = BTreeMap::new();
        for module in &self.modules {
            children
                .entry(module.parent.as_deref())
                .or_default()
                .push(module);
        }
        for list in children.values_mut() {
            list.sort_by(|a, b| (a.order, &a.name, &a.id).cmp(&(b.order, &b.name, &b.id)));
        }
        children
    }

    fn view<'a>(&self, module: &'a Module, children: &Children<'_>) -> ModuleView<'a> {
        let id = self.qualify(&module.id);
        ModuleView {
            children: children
                .get(&Some(id.as_str()))
                .map(|list| list.iter().map(|m| self.qualify(&m.id)).collect())
                .unwrap_or_default(),
            id,
            name: &module.name,
            kind: &module.kind,
            order: module.order,
            parent: module.parent.as_deref(),
            responsibility: module.responsibility.as_deref(),
            description: module.description.as_deref(),
            groups: &module.groups,
            binding: module.binding.as_ref(),
            former_names: &module.former_names,
        }
    }

    fn relationship<'a>(&self, from: &Module, r: &'a Relationship) -> RelationshipView<'a> {
        RelationshipView {
            id: self.qualify(&r.id),
            from: self.qualify(&from.id),
            to: &r.to,
            kind: &r.kind,
            order: r.order,
            label: r.label.as_deref(),
            description: r.description.as_deref(),
            emphasis: r.emphasis,
            anchor: r.anchor.as_ref(),
        }
    }

    /// A module's outgoing relationships in order.
    fn sorted(module: &Module) -> Vec<&Relationship> {
        let mut relationships: Vec<&Relationship> = module.outgoing.iter().collect();
        relationships.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
        relationships
    }

    fn outgoing<'a>(&self, module: &'a Module) -> Vec<RelationshipView<'a>> {
        Self::sorted(module)
            .into_iter()
            .map(|r| self.relationship(module, r))
            .collect()
    }

    /// Modules in depth-first containment order, each with its depth.
    fn ordered<'a>(&self, children: &Children<'a>) -> Vec<(&'a Module, usize)> {
        let mut ordered = Vec::with_capacity(self.modules.len());
        let mut stack: Vec<(&Module, usize)> = children
            .get(&None)
            .map(|roots| roots.iter().rev().map(|m| (*m, 0)).collect())
            .unwrap_or_default();
        while let Some((module, depth)) = stack.pop() {
            ordered.push((module, depth));
            if let Some(list) = children.get(&Some(self.qualify(&module.id).as_str())) {
                stack.extend(list.iter().rev().map(|m| (*m, depth + 1)));
            }
        }
        ordered
    }

    pub fn read(&self) -> Reading<'_> {
        let children = self.children();
        let ordered = self.ordered(&children);
        Reading {
            roots: children
                .get(&None)
                .map(|list| list.iter().map(|m| self.qualify(&m.id)).collect())
                .unwrap_or_default(),
            relationships: ordered.iter().flat_map(|(m, _)| self.outgoing(m)).collect(),
            depths: ordered.iter().map(|(_, depth)| *depth).collect(),
            modules: ordered
                .iter()
                .map(|(m, _)| self.view(m, &children))
                .collect(),
        }
    }

    /// The module `subject` names: its qualified or local ID, else its current or a former name.
    pub fn select(&self, subject: &str) -> Result<Selection<'_>> {
        let by_id: Vec<&Module> = self
            .modules
            .iter()
            .filter(|m| m.id == subject || self.qualify(&m.id) == subject)
            .collect();
        let matches = if by_id.is_empty() {
            self.modules
                .iter()
                .filter(|m| m.name == subject || m.former_names.iter().any(|n| n == subject))
                .collect()
        } else {
            by_id
        };
        let module = match matches.as_slice() {
            [module] => *module,
            [] => {
                return Err(Refusal::new(
                    Code::SubjectNotFound,
                    format!("no module has the ID or name `{subject}`"),
                )
                .into());
            }
            _ => {
                let listed: Vec<_> = matches
                    .iter()
                    .map(|m| json!({"id": self.qualify(&m.id), "name": m.name, "former_names": m.former_names}))
                    .collect();
                return Err(Refusal::new(
                    Code::SubjectAmbiguous,
                    format!(
                        "{} modules match `{subject}`; name one by ID",
                        matches.len()
                    ),
                )
                .with_details(json!({ "matches": listed }))
                .into());
            }
        };
        let children = self.children();
        let id = self.qualify(&module.id);
        Ok(Selection {
            module: self.view(module, &children),
            parts: children
                .get(&Some(id.as_str()))
                .map(|list| list.iter().map(|m| self.view(m, &children)).collect())
                .unwrap_or_default(),
            incoming: self
                .ordered(&children)
                .into_iter()
                .flat_map(|(from, _)| {
                    Self::sorted(from)
                        .into_iter()
                        .filter(|r| r.to == id)
                        .map(move |r| self.relationship(from, r))
                })
                .collect(),
            outgoing: self.outgoing(module),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_files_use_a_fixed_field_order_and_omit_empty_options() {
        let module: Module = serde_json::from_str(
            r#"{"outgoing":[{"order":0,"kind":"call","to":"Mdl00001#Mod00002","id":"Rel00001"}],
                "name":"Billing","id":"Mod00001","kind":"service","order":2,"schema_version":1}"#,
        )
        .unwrap();
        assert_eq!(
            file_json(&module).unwrap(),
            r#"{
  "schema_version": 1,
  "id": "Mod00001",
  "name": "Billing",
  "kind": "service",
  "order": 2,
  "groups": [],
  "former_names": [],
  "outgoing": [
    {
      "id": "Rel00001",
      "to": "Mdl00001#Mod00002",
      "kind": "call",
      "order": 0
    }
  ]
}
"#
        );
    }

    #[test]
    fn model_names_are_trimmed_and_bounded() {
        assert_eq!(valid_name("  Shop ").unwrap(), "Shop");
        assert_eq!(valid_name(&"x".repeat(NAME_MAX)).unwrap().len(), NAME_MAX);
        for bad in ["", "   ", "Sh\nop", "Shop\t1", &"x".repeat(NAME_MAX + 1)] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
    }
}
