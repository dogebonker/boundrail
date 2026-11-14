//! The on-disk vault `.boundrail/model/`: discovery, creation, loading with validation, and
//! the read views of the loaded model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::revision::{self, Revision};
use super::types::{
    Anchor, Binding, Collection, DESCRIPTION_MAX, Emphasis, LABEL_MAX, Manifest, Module,
    RESPONSIBILITY_MAX, Relationship, SCHEMA_VERSION, State, VaultConfig,
};
use super::{Code, Refusal, ids, one_line, state};
use crate::history::{refuse_symlink, write_atomic};
use crate::sync_rules::hash;

mod flows;

pub use flows::{CollectionView, FlowEntry, FlowView, Found};
pub(super) use flows::{Ends, collection_position, flow_position, message_problems};

/// Boundrail's state folder at the checkout root, and the vault folder inside it.
const STATE: &str = ".boundrail";
const VAULT: &str = "model";
const MANIFEST: &str = "model.json";
const CONFIG: &str = "config.yaml";
const MODULES: &str = "modules";
const MODULE_SUFFIX: &str = ".module.json";
const COLLECTIONS: &str = "collections";
const COLLECTION_SUFFIX: &str = ".collection.json";
/// The folders of per-ID vault files, each with the suffix its files end in.
const FOLDERS: [(&str, &str); 2] = [(MODULES, MODULE_SUFFIX), (COLLECTIONS, COLLECTION_SUFFIX)];
const CURRENT: &str = "Current";
/// The longest model, module, collection or flow name.
const NAME_MAX: usize = 120;

#[derive(Debug)]
pub struct Model {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub config: VaultConfig,
    /// Sorted by local ID.
    pub modules: Vec<Module>,
    /// Sorted by local ID.
    pub collections: Vec<Collection>,
    pub revision: Revision,
}

/// One reason a vault does not load: the vault-relative file and the ID or key it concerns.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub file: String,
    pub subject: String,
    pub message: String,
    /// The index of the edit change that last touched `subject`, when an edit caused it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<usize>,
}

#[derive(Default)]
struct Problems(Vec<Problem>);

impl Problems {
    fn push(&mut self, file: &str, subject: &str, message: impl Into<String>) {
        self.0.push(Problem {
            file: file.to_owned(),
            subject: subject.to_owned(),
            message: message.into(),
            change: None,
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
    if !one_line(name, NAME_MAX) {
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
    let current = ids::allocate(|candidate| candidate == id.to_ascii_lowercase())?;
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

/// The vault folder of the checkout around `start`, refusing when no vault exists there.
pub fn locate(start: &Path) -> Result<PathBuf> {
    let dir = checkout_root(start)?.join(STATE).join(VAULT);
    match fs::symlink_metadata(&dir) {
        Ok(_) => Ok(dir),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(not_found(&dir)),
        Err(e) => Err(e).with_context(|| format!("reading {}", dir.display())),
    }
}

fn not_found(dir: &Path) -> anyhow::Error {
    Refusal::new(
        Code::VaultNotFound,
        format!(
            "no model vault at {}; create one with `boundrail model init --name <name>`",
            dir.display()
        ),
    )
    .into()
}

/// Loads the vault of the checkout around `start`, refusing a model with any problem.
pub fn load(start: &Path) -> Result<Model> {
    load_dir(locate(start)?)
}

const LINKED: &str = "is a symbolic link";

/// The state, vault, modules and collections folders by their vault-relative paths.
fn folders(dir: &Path) -> [(&'static str, PathBuf); 4] {
    [
        ("..", dir.parent().unwrap_or(dir).to_path_buf()),
        (".", dir.to_path_buf()),
        (MODULES, dir.join(MODULES)),
        (COLLECTIONS, dir.join(COLLECTIONS)),
    ]
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Refuses to write through a linked state, vault, modules or collections folder.
fn refuse_links(dir: &Path) -> Result<()> {
    folders(dir)
        .iter()
        .try_for_each(|(_, path)| refuse_symlink(path))
}

/// Loads the vault folder `dir`, refusing a model with any problem.
pub fn load_dir(dir: PathBuf) -> Result<Model> {
    // A linked folder could point reads outside the checkout, so nothing is read through it.
    let mut problems = Problems::default();
    for (path, folder) in folders(&dir) {
        if is_link(&folder) {
            problems.push(path, path, LINKED);
        }
    }
    if !problems.0.is_empty() {
        return Err(invalid(&dir, problems.0));
    }
    // Each file is read once: the revision and the parsed model come from the same bytes.
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    for path in [MANIFEST, CONFIG] {
        let file = dir.join(path);
        match fs::symlink_metadata(&file) {
            Ok(meta) if meta.is_file() => {
                let bytes = fs::read(&file).with_context(|| format!("reading {path}"))?;
                inputs.push((path.to_owned(), bytes));
            }
            Ok(meta) if meta.file_type().is_symlink() => problems.push(path, path, LINKED),
            Ok(_) => problems.push(path, path, "is not a regular file"),
            Err(e) if e.kind() == ErrorKind::NotFound && path == MANIFEST => {
                return Err(not_found(&dir));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                problems.push(path, path, "file is missing");
            }
            Err(e) => return Err(e).with_context(|| format!("reading {path}")),
        }
    }
    for (folder, suffix) in FOLDERS {
        for path in listed(&dir, folder, suffix, &mut problems)? {
            let bytes = fs::read(dir.join(&path)).with_context(|| format!("reading {path}"))?;
            inputs.push((path, bytes));
        }
    }
    let revision = revision::of(inputs.iter().map(|(p, b)| (p.as_str(), b.as_slice())));

    let mut manifest = None;
    let mut config = None;
    let mut modules = Vec::new();
    let mut collections = Vec::new();
    for (path, bytes) in &inputs {
        let parsed = match (path.as_str(), vault_file(path)) {
            (MANIFEST, _) => serde_json::from_slice(bytes)
                .map(|m| manifest = Some(m))
                .map_err(|e| parse_message(&e)),
            (CONFIG, _) => serde_yaml_ng::from_slice(bytes)
                .map(|c| config = Some(c))
                .map_err(|e| parse_message(&e)),
            (_, Some((MODULES, _))) => serde_json::from_slice::<Module>(bytes)
                .map(|m| modules.push((path.as_str(), m)))
                .map_err(|e| parse_message(&e)),
            _ => serde_json::from_slice::<Collection>(bytes)
                .map(|c| collections.push((path.as_str(), c)))
                .map_err(|e| parse_message(&e)),
        };
        if let Err(message) = parsed {
            problems.push(
                path,
                file_subject(path),
                format!("does not parse: {message}"),
            );
        }
    }
    let pairs: Vec<(&str, &Module)> = modules.iter().map(|(f, m)| (*f, m)).collect();
    let groups: Vec<(&str, &Collection)> = collections.iter().map(|(f, c)| (*f, c)).collect();
    check(
        manifest.as_ref(),
        config.as_ref(),
        &pairs,
        &groups,
        &mut problems,
    );
    let problems = problems.0;
    match (manifest, config) {
        (Some(manifest), Some(config)) if problems.is_empty() => {
            let mut modules: Vec<Module> = modules.into_iter().map(|(_, m)| m).collect();
            modules.sort_by(|a, b| a.id.cmp(&b.id));
            let mut collections: Vec<Collection> =
                collections.into_iter().map(|(_, c)| c).collect();
            collections.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(Model {
                dir,
                manifest,
                config,
                modules,
                collections,
                revision,
            })
        }
        _ => Err(invalid(&dir, problems)),
    }
}

/// The vault-relative paths of the regular files in `folder` whose names end in `suffix`,
/// sorted. A linked or special file is a problem and is not read.
fn listed(dir: &Path, folder: &str, suffix: &str, problems: &mut Problems) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    match fs::read_dir(dir.join(folder)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !name.ends_with(suffix) {
                    continue;
                }
                // `file_type` does not follow links: a linked vault file is reported, not read.
                let kind = entry.file_type()?;
                let path = format!("{folder}/{name}");
                if kind.is_symlink() {
                    problems.push(&path, file_subject(&path), LINKED);
                } else if kind.is_file() {
                    paths.push(path);
                } else if !kind.is_dir() {
                    // As in `file_digests`, a folder named like a vault file is not one.
                    problems.push(&path, file_subject(&path), "is not a regular file");
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {folder}")),
    }
    paths.sort();
    Ok(paths)
}

/// `MODEL_INVALID` listing every problem.
pub(super) fn invalid(dir: &Path, problems: Vec<Problem>) -> anyhow::Error {
    Refusal::new(
        Code::ModelInvalid,
        format!(
            "the model at {} has {} problem{}",
            dir.display(),
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        ),
    )
    .with_details(json!({ "problems": problems }))
    .into()
}

/// A parser's message cut to its first line, with control characters escaped, so a refusal
/// never replays file content into a terminal.
pub(super) fn parse_message(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .fold(String::new(), |mut out, c| {
            if c.is_control() {
                out.extend(c.escape_debug());
            } else {
                out.push(c);
            }
            out
        })
}

/// Every problem of an in-memory model, as `load` would report it after writing the model.
pub(super) fn problems(
    manifest: &Manifest,
    config: &VaultConfig,
    modules: &[Module],
    collections: &[Collection],
) -> Vec<Problem> {
    let files: Vec<String> = modules.iter().map(|m| module_file(&m.id)).collect();
    let pairs: Vec<(&str, &Module)> = files.iter().map(String::as_str).zip(modules).collect();
    let files: Vec<String> = collections.iter().map(|c| collection_file(&c.id)).collect();
    let groups: Vec<(&str, &Collection)> =
        files.iter().map(String::as_str).zip(collections).collect();
    let mut problems = Problems::default();
    check(Some(manifest), Some(config), &pairs, &groups, &mut problems);
    problems.0
}

fn check(
    manifest: Option<&Manifest>,
    config: Option<&VaultConfig>,
    modules: &[(&str, &Module)],
    collections: &[(&str, &Collection)],
    problems: &mut Problems,
) {
    check_ids(manifest, modules, collections, problems);
    flows::check(
        manifest.map(|m| m.id.as_str()),
        modules,
        collections,
        problems,
    );
    for (file, module) in modules {
        check_module(file, module, config, problems);
    }
    if let Some(m) = manifest {
        check_text(
            problems,
            MANIFEST,
            &m.id,
            "name",
            Some(&m.name),
            None,
            false,
        );
        check_text(
            problems,
            MANIFEST,
            &m.current.id,
            "state name",
            Some(&m.current.name),
            None,
            false,
        );
        check_containment(&m.id, modules, problems);
    }
}

pub(super) fn module_file(id: &str) -> String {
    format!("{MODULES}/{id}{MODULE_SUFFIX}")
}

pub(super) fn collection_file(id: &str) -> String {
    format!("{COLLECTIONS}/{id}{COLLECTION_SUFFIX}")
}

/// Every relationship by its local ID, with the local ID of the module that owns it.
pub(super) fn relationships(modules: &[Module]) -> BTreeMap<&str, (&str, &Relationship)> {
    modules
        .iter()
        .flat_map(|m| {
            m.outgoing
                .iter()
                .map(move |r| (r.id.as_str(), (m.id.as_str(), r)))
        })
        .collect()
}

/// One vault file an edit writes: its vault-relative path and new contents, `None` to delete it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FileWrite {
    pub(super) path: String,
    pub(super) content: Option<String>,
}

/// The writes that store `manifest` (when given), `modules` and `collections` and delete the
/// `removed` vault files, in the order they apply: the manifest first, so retired IDs are
/// recorded before any file leaves, then the modules and collections, then the deletions.
pub(super) fn plan_writes(
    manifest: Option<&Manifest>,
    modules: &[&Module],
    collections: &[&Collection],
    removed: &[String],
) -> Result<Vec<FileWrite>> {
    let write = |path: String, content: String| FileWrite {
        path,
        content: Some(content),
    };
    let mut writes = Vec::with_capacity(modules.len() + collections.len() + removed.len() + 1);
    if let Some(manifest) = manifest {
        writes.push(write(MANIFEST.to_owned(), file_json(manifest)?));
    }
    for module in modules {
        writes.push(write(module_file(&module.id), file_json(module)?));
    }
    for collection in collections {
        writes.push(write(
            collection_file(&collection.id),
            file_json(collection)?,
        ));
    }
    writes.extend(removed.iter().map(|path| FileWrite {
        path: path.clone(),
        content: None,
    }));
    Ok(writes)
}

/// Applies `writes` in order. Each one sets a file's whole contents or deletes it, so a journal
/// can apply the same writes again after an interruption.
pub(super) fn apply_writes(dir: &Path, writes: &[FileWrite]) -> Result<()> {
    refuse_links(dir)?;
    for write in writes {
        let path = write.path.as_str();
        ensure!(
            path == MANIFEST || vault_file(path).is_some_and(|(_, id)| ids::is_local(id)),
            "`{path}` is not a vault file an edit writes"
        );
        let file = dir.join(path);
        match &write.content {
            Some(content) => {
                // The collections folder appears with the first collection.
                if let Some(folder) = file.parent() {
                    fs::create_dir_all(folder).with_context(|| format!("creating {path}"))?;
                }
                write_atomic(&file, content.as_bytes())
                    .with_context(|| format!("writing {path}"))?;
            }
            None => match fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {path}")),
            },
        }
    }
    // The renames and removals must reach the disk before the journal records the edit done.
    state::sync_dir(dir)?;
    for (folder, _) in FOLDERS {
        let folder = dir.join(folder);
        if folder.is_dir() {
            state::sync_dir(&folder)?;
        }
    }
    Ok(())
}

/// The SHA-256 of each recognized vault file by vault-relative path, read without validating the
/// model, which a partly applied edit may leave invalid. A linked or special file gets a marker
/// that no content hashes to.
pub(super) fn file_digests(dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut paths = vec![MANIFEST.to_owned(), CONFIG.to_owned()];
    for (folder, suffix) in FOLDERS {
        match fs::read_dir(dir.join(folder)) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    // As in `load_dir`, a folder named like a vault file is not one.
                    if let Some(name) = entry.file_name().to_str()
                        && name.ends_with(suffix)
                        && !entry.file_type()?.is_dir()
                    {
                        paths.push(format!("{folder}/{name}"));
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {folder}")),
        }
    }
    let mut digests = BTreeMap::new();
    for path in paths {
        let file = dir.join(&path);
        let digest = match fs::symlink_metadata(&file) {
            Ok(meta) if meta.is_file() => {
                hash(&fs::read(&file).with_context(|| format!("reading {path}"))?)
            }
            Ok(_) => "not a regular file".to_owned(),
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {path}")),
        };
        digests.insert(path, digest);
    }
    Ok(digests)
}

/// The folder and local ID of a module or collection file, by its vault-relative path.
fn vault_file(path: &str) -> Option<(&'static str, &str)> {
    FOLDERS.iter().find_map(|(folder, suffix)| {
        let id = path
            .strip_prefix(folder)?
            .strip_prefix('/')?
            .strip_suffix(suffix)?;
        Some((*folder, id))
    })
}

fn file_subject(path: &str) -> &str {
    vault_file(path).map_or(path, |(_, id)| id)
}

/// Every allocated ID (model, state, module, relationship, collection, flow, step, branch)
/// shares one namespace: each must be well formed, unique and not retired.
fn check_ids(
    manifest: Option<&Manifest>,
    modules: &[(&str, &Module)],
    collections: &[(&str, &Collection)],
    problems: &mut Problems,
) {
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
    for (file, collection) in collections {
        ids.extend(collection.ids().into_iter().map(|id| (*file, id)));
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

/// A module or collection file's version, file name and name.
fn check_file(problems: &mut Problems, file: &str, id: &str, schema_version: u32, name: &str) {
    if schema_version != SCHEMA_VERSION {
        problems.push(
            file,
            id,
            format!("schema_version {schema_version} is not {SCHEMA_VERSION}"),
        );
    }
    if file_subject(file) != id {
        problems.push(file, id, format!("file name does not match ID {id}"));
    }
    check_name(problems, file, id, name);
}

fn check_name(problems: &mut Problems, file: &str, id: &str, name: &str) {
    if !one_line(name, NAME_MAX) {
        problems.push(
            file,
            id,
            format!("name needs 1 to {NAME_MAX} characters and no control characters"),
        );
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
    check_file(problems, file, id, module.schema_version, &module.name);
    let text = |problems: &mut Problems, field: &str, value: Option<&str>, max, subject: &str| {
        check_text(problems, file, subject, field, value, max, false);
    };
    let description = |problems: &mut Problems, value: Option<&str>, subject: &str| {
        check_text(
            problems,
            file,
            subject,
            "description",
            value,
            Some(DESCRIPTION_MAX),
            true,
        );
    };
    text(problems, "kind", Some(&module.kind), None, id);
    text(
        problems,
        "responsibility",
        module.responsibility.as_deref(),
        Some(RESPONSIBILITY_MAX),
        id,
    );
    description(problems, module.description.as_deref(), id);
    for name in &module.former_names {
        text(problems, "former name", Some(name), None, id);
    }
    for group in &module.groups {
        text(problems, "group", Some(group), None, id);
    }
    if let Some(b) = &module.binding {
        text(problems, "binding repo", b.repo.as_deref(), None, id);
        text(problems, "binding path", Some(&b.path), None, id);
        check_path(problems, file, id, "binding path", &b.path);
        text(problems, "binding symbol", b.symbol.as_deref(), None, id);
    }
    for r in &module.outgoing {
        text(problems, "kind", Some(&r.kind), None, &r.id);
        text(
            problems,
            "label",
            r.label.as_deref(),
            Some(LABEL_MAX),
            &r.id,
        );
        description(problems, r.description.as_deref(), &r.id);
        if let Some(a) = &r.anchor {
            text(problems, "anchor repo", a.repo.as_deref(), None, &r.id);
            text(problems, "anchor path", Some(&a.path), None, &r.id);
            check_path(problems, file, &r.id, "anchor path", &a.path);
            text(problems, "anchor symbol", Some(&a.symbol), None, &r.id);
        }
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

/// Whether `path` is a code path relative to the repository root on every host: not empty, not
/// absolute, without a `..` segment, a backslash, or a `:` in its first segment (a drive or a
/// URL scheme), so it never names a file outside the repository.
pub(super) fn is_repo_path(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !first.contains(':')
        && !path.split('/').any(|s| s == "..")
}

fn check_path(problems: &mut Problems, file: &str, subject: &str, field: &str, path: &str) {
    if !is_repo_path(path) {
        problems.push(
            file,
            subject,
            format!("{field} is not a relative path inside the repository"),
        );
    }
}

/// An authored string within its length limit and free of control characters; a `multiline`
/// one may hold line breaks and tabs.
fn check_text(
    problems: &mut Problems,
    file: &str,
    subject: &str,
    field: &str,
    value: Option<&str>,
    max: Option<usize>,
    multiline: bool,
) {
    let Some(value) = value else {
        return;
    };
    let count = value.chars().count();
    if let Some(max) = max
        && count > max
    {
        problems.push(
            file,
            subject,
            format!("{field} has {count} characters, more than {max}"),
        );
    }
    if value
        .chars()
        .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
    {
        problems.push(
            file,
            subject,
            format!("{field} contains a control character"),
        );
    }
}

/// Parents and relationship targets name modules of this model, and containment has no cycle.
fn check_containment(model: &str, modules: &[(&str, &Module)], problems: &mut Problems) {
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

/// The module `subject` names in the model with ID `model`: its qualified or local ID, else its
/// unique current or former name.
pub fn find<'a, I>(model: &str, modules: I, subject: &str) -> Result<&'a Module, Refusal>
where
    I: IntoIterator<Item = &'a Module>,
    I::IntoIter: Clone,
{
    let keyed = modules.into_iter().map(|m| ((), m));
    find_keyed(model, keyed, subject).map(|(_, module)| module)
}

/// [`find`] among `modules` paired with keys, returning the key with the module.
pub(super) fn find_keyed<'a, K: Copy>(
    model: &str,
    modules: impl Iterator<Item = (K, &'a Module)> + Clone,
    subject: &str,
) -> Result<(K, &'a Module), Refusal> {
    let local = ids::local(model, subject);
    let by_id: Vec<(K, &Module)> = modules.clone().filter(|(_, m)| m.id == local).collect();
    let matches = if by_id.is_empty() {
        modules
            .filter(|(_, m)| m.name == subject || m.former_names.iter().any(|n| n == subject))
            .collect()
    } else {
        by_id
    };
    match matches.as_slice() {
        [found] => Ok(*found),
        [] => Err(Refusal::new(
            Code::SubjectNotFound,
            format!("no module has the ID or name `{subject}`"),
        )),
        _ => {
            let listed: Vec<_> = matches
                .iter()
                .map(|(_, m)| json!({"id": ids::qualify(model, &m.id), "name": m.name, "former_names": m.former_names}))
                .collect();
            Err(Refusal::new(
                Code::SubjectAmbiguous,
                format!(
                    "{} modules match `{subject}`; name one by ID",
                    matches.len()
                ),
            )
            .with_details(json!({ "matches": listed })))
        }
    }
}

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

    /// The module `subject` names, with its parts and the relationships that touch it.
    pub fn select(&self, subject: &str) -> Result<Selection<'_>> {
        let module = find(&self.manifest.id, &self.modules, subject)?;
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
    fn parse_messages_keep_one_line_and_escape_control_characters() {
        assert_eq!(
            parse_message(&"unknown field `a\u{1b}[31m`\nsecond line"),
            "unknown field `a\\u{1b}[31m`"
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
