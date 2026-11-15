//! Proposals in the vault: `proposals/<id>/` holds a proposal's `proposal.json` beside its own
//! modules and collections. The paths, loads and writes that only proposals have.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use serde::de::DeserializeOwned;

use super::{
    FileWrite, Model, PROPOSAL_FOLDERS, Problems, SCHEMA_VERSION, check_name, check_text,
    collection_file, file_json, invalid, is_link, linked, listed, load_dir, load_scope,
    module_file, parse_message, plan_writes,
};
use crate::arch::types::{Collection, DESCRIPTION_MAX, Module, Proposal, ProposalStatus};
use crate::arch::{Code, Refusal, ids, state};
use crate::history::refuse_symlink;

/// The folder of proposals, one folder per proposal ID, and the file each one holds.
pub(super) const PROPOSALS: &str = "proposals";
pub(crate) const PROPOSAL: &str = "proposal.json";
/// A proposal's base folder, with the slash its paths continue with.
pub(crate) const BASE: &str = "base/";

/// The vault files a revision covers and a journaled write may change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// The manifest, the config, and the current state's modules and collections.
    #[default]
    Current,
    /// One proposal's folder: its `proposal.json`, modules and collections.
    Proposal(String),
    /// Adopting a proposal: the current state and that proposal's folder.
    Adoption(String),
    /// Reconciling a proposal with the current state: both, as for adoption, though only the
    /// proposal's folder is written.
    Reconcile(String),
}

impl Scope {
    /// Whether it covers the current state's files.
    pub fn current(&self) -> bool {
        !matches!(self, Self::Proposal(_))
    }

    /// The proposal whose folder it covers.
    pub fn proposal(&self) -> Option<&str> {
        match self {
            Self::Current => None,
            Self::Proposal(id) | Self::Adoption(id) | Self::Reconcile(id) => Some(id),
        }
    }
}

/// `proposals/<id>/`, the folder of a proposal's files.
pub(crate) fn prefix(id: &str) -> String {
    format!("{PROPOSALS}/{id}/")
}

/// `proposals/<id>/base/`, the folder of the predecessor snapshot a proposal is reconciled against.
pub(super) fn base_prefix(id: &str) -> String {
    format!("{}{BASE}", prefix(id))
}

/// Whether a proposal-relative path is inside its `base/` folder.
pub(super) fn is_base(rest: &str) -> bool {
    rest.starts_with(BASE)
}

/// `proposals/<id>/proposal.json`.
pub(crate) fn proposal_file(id: &str) -> String {
    format!("{}{PROPOSAL}", prefix(id))
}

/// The proposal ID and proposal-relative rest of a path inside a proposal folder.
pub(super) fn split_proposal(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix(PROPOSALS)?
        .strip_prefix('/')?
        .split_once('/')
}

/// Whether a vault-relative path is inside a proposal folder.
pub(crate) fn is_proposal_path(path: &str) -> bool {
    split_proposal(path).is_some()
}

/// A vault-relative path without the `proposals/<id>/` of a proposal's files.
pub(super) fn unprefixed(path: &str) -> &str {
    split_proposal(path).map_or(path, |(_, rest)| rest)
}

/// Loads the proposal `subject` names, its local or qualified ID, from the vault folder `dir`:
/// its modules and collections with the vault's manifest and config. Its revision covers the
/// proposal's own files.
pub fn load_proposal(dir: PathBuf, subject: &str) -> Result<Model> {
    let id = subject.split_once('#').map_or(subject, |(_, local)| local);
    if !is_proposal(&dir, id)? {
        return Err(proposal_not_found(subject));
    }
    let model = load_scope(dir, Some(id))?;
    if subject.contains('#') && model.qualify(id) != subject {
        return Err(proposal_not_found(subject));
    }
    Ok(model)
}

/// The proposal `state` names, when a proposal has that local ID, else the current state.
pub fn load_state(dir: PathBuf, state: &str) -> Result<Model> {
    if is_proposal(&dir, state)? {
        load_proposal(dir, state)
    } else {
        load_dir(dir)
    }
}

fn proposal_not_found(subject: &str) -> anyhow::Error {
    Refusal::new(
        Code::SubjectNotFound,
        format!("no proposal has the ID `{subject}`"),
    )
    .into()
}

/// The `proposal.json` of the proposal folder `id`, read alone without the proposal's other
/// files. A linked folder or file is refused rather than read.
pub(crate) fn read_proposal(dir: &Path, id: &str) -> Result<Proposal> {
    let folder = dir.join(PROPOSALS);
    for path in [&folder, &folder.join(id)] {
        refuse_symlink(path)?;
    }
    state::read(&dir.join(proposal_file(id)))?
        .with_context(|| format!("{} is missing", proposal_file(id)))
}

/// Whether the vault folder `dir` holds a proposal folder, linked or not, named `id`. A linked
/// `proposals` folder refuses `MODEL_INVALID`.
pub(crate) fn is_proposal(dir: &Path, id: &str) -> Result<bool> {
    refuse_linked(dir)?;
    Ok(ids::is_local(id) && fs::symlink_metadata(dir.join(PROPOSALS).join(id)).is_ok())
}

/// `MODEL_INVALID` when the `proposals` folder of the vault folder `dir` is a symbolic link,
/// which could point reads outside the checkout.
fn refuse_linked(dir: &Path) -> Result<()> {
    if is_link(&dir.join(PROPOSALS)) {
        return Err(linked(PROPOSALS));
    }
    Ok(())
}

/// The local IDs of the proposal folders in the vault folder `dir`, sorted. A linked
/// `proposals` folder refuses `MODEL_INVALID`.
pub fn proposals(dir: &Path) -> Result<Vec<String>> {
    refuse_linked(dir)?;
    let mut found = Vec::new();
    match fs::read_dir(dir.join(PROPOSALS)) {
        Ok(entries) => {
            for entry in entries {
                if let Some(name) = entry?.file_name().to_str()
                    && ids::is_local(name)
                {
                    found.push(name.to_owned());
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {PROPOSALS}")),
    }
    found.sort();
    Ok(found)
}

/// `writes` moved into the folder of the proposal `id`.
pub(crate) fn in_proposal(id: &str, mut writes: Vec<FileWrite>) -> Vec<FileWrite> {
    let prefix = prefix(id);
    for write in &mut writes {
        write.path.insert_str(0, &prefix);
    }
    writes
}

/// The write that stores `proposal` as its folder's `proposal.json`.
pub(crate) fn proposal_write(proposal: &Proposal) -> Result<FileWrite> {
    Ok(FileWrite {
        path: proposal_file(&proposal.id),
        content: Some(file_json(proposal)?),
    })
}

/// The modules and collections a proposal's `base/` folder holds, sorted by ID, or `None` for a
/// proposal without a base. The base is parsed and each file must be named by its item's local
/// ID (`MODEL_INVALID`), so its writes never leave the folder; it is otherwise a copy of what the
/// predecessor held, validated when a bundle brings it in.
pub fn load_base(model: &Model) -> Result<Option<(Vec<Module>, Vec<Collection>)>> {
    let Some(proposal) = model.proposal.as_ref().filter(|p| p.base) else {
        return Ok(None);
    };
    let base = base_prefix(&proposal.id);
    let mut problems = Problems::default();
    let (dir, folders) = (&model.dir, PROPOSAL_FOLDERS);
    let mut modules = base_files(dir, &base, folders[0], module_id, &mut problems)?;
    let mut collections = base_files(dir, &base, folders[1], collection_id, &mut problems)?;
    if !problems.0.is_empty() {
        return Err(invalid(&model.dir, problems.0));
    }
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    collections.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Some((modules, collections)))
}

/// The items of `folder` in the base folder `base`, each named by its local ID, `id`.
fn base_files<T: DeserializeOwned>(
    dir: &Path,
    base: &str,
    (folder, suffix): (&str, &str),
    id: fn(&T) -> &str,
    problems: &mut Problems,
) -> Result<Vec<T>> {
    let mut items = Vec::new();
    for path in listed(dir, base, folder, suffix, problems)? {
        let bytes = fs::read(dir.join(&path)).with_context(|| format!("reading {path}"))?;
        match serde_json::from_slice::<T>(&bytes) {
            Ok(item) if !ids::is_local(id(&item)) => {
                problems.push(&path, &path, "ID is not 8 base62 characters");
            }
            Ok(item) if path != format!("{base}{folder}/{}{suffix}", id(&item)) => {
                problems.push(&path, id(&item), "file name does not match its ID");
            }
            Ok(item) => items.push(item),
            Err(e) => problems.push(
                &path,
                &path,
                format!("does not parse: {}", parse_message(&e)),
            ),
        }
    }
    Ok(items)
}

/// The writes that make the base of the proposal `id` hold `base`, given the base it holds now,
/// `old`: the files that are new or differ, and the deletions of files `base` no longer has.
pub(crate) fn base_writes(
    id: &str,
    (modules, collections): (&[Module], &[Collection]),
    old: Option<(&[Module], &[Collection])>,
) -> Result<Vec<FileWrite>> {
    let (old_modules, old_collections) = old.unwrap_or_default();
    let (modules, mut removed) = changed(modules, old_modules, module_id, module_file);
    let (collections, gone) = changed(collections, old_collections, collection_id, collection_file);
    removed.extend(gone);
    let mut writes = plan_writes(None, &modules, &collections, &[], &[], &removed)?;
    let base = base_prefix(id);
    for write in &mut writes {
        write.path.insert_str(0, &base);
    }
    Ok(writes)
}

/// The items of `new` that `old` lacks or holds otherwise, and the files (by `file`) of the
/// items of `old` whose ID `new` no longer holds.
fn changed<'a, T: PartialEq>(
    new: &'a [T],
    old: &[T],
    id: fn(&T) -> &str,
    file: fn(&str) -> String,
) -> (Vec<&'a T>, Vec<String>) {
    let before: BTreeMap<&str, &T> = old.iter().map(|item| (id(item), item)).collect();
    let after: BTreeSet<&str> = new.iter().map(id).collect();
    let fresh = (new.iter())
        .filter(|item| before.get(id(item)) != Some(item))
        .collect();
    let gone = (before.keys())
        .filter(|known| !after.contains(*known))
        .map(|known| file(known))
        .collect();
    (fresh, gone)
}

fn module_id(module: &Module) -> &str {
    &module.id
}

fn collection_id(collection: &Collection) -> &str {
    &collection.id
}

/// A proposal file's own fields: version, folder name, text, the IDs it names, and whether only
/// an adopted proposal names the state it became.
pub(super) fn check_proposal(file: &str, proposal: &Proposal, problems: &mut Problems) {
    let id = proposal.id.as_str();
    if proposal.schema_version != SCHEMA_VERSION {
        let found = proposal.schema_version;
        problems.push(
            file,
            id,
            format!("schema_version {found} is not {SCHEMA_VERSION}"),
        );
    }
    if file != proposal_file(id) {
        problems.push(file, id, format!("folder name does not match ID {id}"));
    }
    check_name(problems, file, id, &proposal.name);
    let description = proposal.description.as_deref();
    check_text(
        problems,
        file,
        id,
        "description",
        description,
        Some(DESCRIPTION_MAX),
        true,
    );
    for named in proposal
        .predecessor
        .state
        .iter()
        .chain(&proposal.adopted_as)
    {
        if !ids::is_local(named) {
            problems.push(file, named, "state ID is not 8 base62 characters");
        }
    }
    for dropped in &proposal.dropped {
        if !ids::is_local(dropped) {
            problems.push(file, dropped, "dropped ID is not 8 base62 characters");
        }
    }
    if (proposal.status == ProposalStatus::Adopted) != proposal.adopted_as.is_some() {
        problems.push(
            file,
            id,
            "an adopted proposal names the state it became in adopted_as, and only an adopted one",
        );
    }
}
