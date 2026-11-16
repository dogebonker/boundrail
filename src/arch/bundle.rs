//! Bundles: one JSON file (schema `boundrail/bundle/v1`, `model schema model-bundle`) carrying
//! the current state, one proposal with its base, or one checkpoint's tree, so a model state
//! moves between clones without Git. `model export` writes one; `model import` reads one into a
//! new draft proposal, so review and adoption stay explicit and the current state never changes.
//! `model export --with-history` adds the proposals and the checkpoint history, and `model import
//! --restore` recreates such a vault and history in a checkout that has neither.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::iter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::edit::{self, Receipt};
use super::proposal::{self, Writing};
use super::request::{INPUT_MAX, check_size};
use super::revision::Revision;
use super::types::{
    Collection, Module, Proposal, ProposalStatus, ReadingState, SCHEMA_VERSION, Walkthrough,
};
use super::vault::{self, Model, Problem, Scope};
use super::{Code, Refusal, checkpoint, claims, ids};
use crate::collect::TempDir;
use crate::files::{hash, refuse_symlink};

/// The `schema` every bundle names.
pub const SCHEMA: &str = "boundrail/bundle/v1";

/// A bundle of one model state's vault files.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// Always `boundrail/bundle/v1`.
    pub schema: String,
    pub model: BundleModel,
    pub source: Source,
    /// Each file's text by path: vault-relative for the current state (with `--with-history`
    /// its proposals' folders too) and a checkpoint, proposal-relative (`proposal.json`,
    /// `modules/...`, `base/modules/...`) for a proposal.
    pub files: BTreeMap<String, String>,
    /// The SHA-256 (lowercase hex) of each file's bytes, by the same paths.
    pub digests: BTreeMap<String, String>,
    /// The checkpoint history, exported with the current state by `--with-history`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<checkpoint::Chain>,
}

/// The model a bundle belongs to.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleModel {
    pub id: String,
    pub name: String,
}

/// The state a bundle was exported from, with its revision digest.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source {
    Current {
        /// The current state's local ID; `null` in a planning model.
        state: Option<String>,
        revision: String,
    },
    Proposal {
        proposal: String,
        name: String,
        revision: String,
        /// The ID of the checkpoint the proposal was exported from, when it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<String>,
        /// That checkpoint's commit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
    Checkpoint {
        checkpoint: String,
        commit: String,
        revision: String,
    },
}

/// What `model export` exports.
pub enum Exported<'a> {
    /// The current state, and with `history` its proposals and the checkpoint history.
    Current {
        history: bool,
    },
    Proposal(&'a str),
    Checkpoint(&'a str),
}

/// One exported file: its path in the bundle, SHA-256 and text.
type Text = (String, String, String);

/// The bundle of what `what` names in the checkout around `start`. A proposal's checkpoint
/// exports as that proposal does, with its own files. The current state with history takes the
/// files of every proposal, each of which must load, and every checkpoint (see
/// [`checkpoint::export_history`]).
pub fn export(start: &Path, what: Exported<'_>) -> Result<Bundle> {
    let mut history = None;
    let (model, source, files) = match what {
        Exported::Current { history: with } => {
            let model = vault::load(start)?;
            let mut files = vault_files(&model.dir, &model.revision, "")?;
            if with {
                for id in vault::proposals(&model.dir)? {
                    let drawn = vault::load_proposal(model.dir.clone(), &id)?;
                    files.extend(vault_files(&model.dir, &drawn.revision, "")?);
                }
                history = checkpoint::export_history(start)?;
            }
            let state = model.manifest.current.as_ref().map(|c| c.id.clone());
            let revision = model.revision.digest.clone();
            (model, Source::Current { state, revision }, files)
        }
        Exported::Proposal(subject) => {
            let model = vault::load_proposal(vault::locate(start)?, subject)?;
            let drawn = proposal::proposal_of(&model)?;
            let files = vault_files(&model.dir, &model.revision, &vault::prefix(&drawn.id))?;
            let revision = model.revision.digest.clone();
            let (proposal, name) = (drawn.id, drawn.name);
            let source = Source::Proposal {
                proposal,
                name,
                revision,
                checkpoint: None,
                commit: None,
            };
            (model, source, files)
        }
        Exported::Checkpoint(which) => {
            let (found, model, blobs) = checkpoint::load_files(start, which)?;
            let own = found.proposal.as_deref().map(vault::prefix);
            let files = (blobs.iter())
                .filter_map(|(path, bytes)| match &own {
                    Some(prefix) => path.strip_prefix(prefix.as_str()).map(|rest| (rest, bytes)),
                    None => Some((path, bytes)),
                })
                .map(|(path, bytes)| text(path.to_owned(), hash(bytes), bytes.to_vec()))
                .collect::<Result<_>>()?;
            let (checkpoint, commit, revision) = (found.id, found.commit, found.revision);
            let source = match &model.proposal {
                Some(drawn) => Source::Proposal {
                    proposal: drawn.id.clone(),
                    name: drawn.name.clone(),
                    revision,
                    checkpoint: Some(checkpoint),
                    commit: Some(commit),
                },
                None => Source::Checkpoint {
                    checkpoint,
                    commit,
                    revision,
                },
            };
            (model, source, files)
        }
    };
    let mut bundle = Bundle {
        schema: SCHEMA.to_owned(),
        model: BundleModel {
            id: model.manifest.id,
            name: model.manifest.name,
        },
        source,
        files: BTreeMap::new(),
        digests: BTreeMap::new(),
        history,
    };
    for (path, sha256, text) in files {
        bundle.digests.insert(path.clone(), sha256);
        bundle.files.insert(path, text);
    }
    Ok(bundle)
}

/// `bundle` as the JSON `model export` writes, refusing `BUNDLE_INVALID` when it is larger than
/// the [`INPUT_MAX`] bytes `model import` reads.
pub fn to_text(bundle: &Bundle) -> Result<String> {
    let text = serde_json::to_string_pretty(bundle)? + "\n";
    if text.len() as u64 > INPUT_MAX {
        return Err(Refusal::new(
            Code::BundleInvalid,
            format!(
                "the bundle is {} bytes, more than the {INPUT_MAX} bytes `model import` reads",
                text.len()
            ),
        )
        .with_details(json!({ "bytes": text.len(), "max": INPUT_MAX }))
        .into());
    }
    Ok(text)
}

/// Each file `revision` covers in the vault folder `dir`, by its path without `prefix`, with the
/// digest the load read. Refuses `STALE_REVISION` when a file no longer holds what was loaded.
fn vault_files(dir: &Path, revision: &Revision, prefix: &str) -> Result<Vec<Text>> {
    let mut files = Vec::with_capacity(revision.files.len());
    for file in &revision.files {
        let bytes =
            fs::read(dir.join(&file.path)).with_context(|| format!("reading {}", file.path))?;
        if hash(&bytes) != file.sha256 {
            let message = "vault files changed while they were exported; export again";
            return Err(Refusal::new(Code::StaleRevision, message)
                .with_details(json!({ "path": file.path }))
                .into());
        }
        let path = file.path.strip_prefix(prefix).unwrap_or(&file.path);
        files.push(text(path.to_owned(), file.sha256.clone(), bytes)?);
    }
    Ok(files)
}

fn text(path: String, sha256: String, bytes: Vec<u8>) -> Result<Text> {
    let text = String::from_utf8(bytes).with_context(|| format!("{path} is not UTF-8"))?;
    Ok((path, sha256, text))
}

/// Imports the bundle `input` as a new draft of the vault folder `vault`, named `name` or after
/// what the bundle holds. A current-state or checkpoint bundle gives its modules and collections,
/// with the current state as predecessor and base; the proposals and history a current-state
/// bundle carries are not imported. A proposal bundle gives its proposal's, its
/// walkthroughs, summary and documents too, with that proposal's predecessor, base and dropped
/// IDs, so adopting it after the current state moved refuses `PREDECESSOR_MOVED` until
/// `model reconcile` merges the two. A beat that read the bundled proposal reads the draft. `expected` is the
/// current revision the caller read. Refuses `MODEL_MISMATCH` for another model's bundle,
/// `BUNDLE_INVALID` for a bundle that is not well formed or whose base does not hold, and
/// `MODEL_INVALID` when its files do not load or the draft does not load in this vault.
pub fn import(
    vault: &Path,
    writing: &Writing<'_>,
    input: &[u8],
    expected: &str,
    name: Option<&str>,
) -> Result<(Receipt, Option<Scope>)> {
    let name = name.map(vault::valid_name).transpose()?.map(str::to_owned);
    let (writer, operation) = proposal::start(vault, writing)?;
    let request = json!({"kind": "import", "writer": writer, "name": name, "expected_revision": expected, "bundle": hash(input)});
    edit::journaled(vault, &operation, request, |_, dir, fingerprint| {
        let current = vault::load_dir(vault.to_path_buf())?;
        proposal::refuse_stale(&current, expected)?;
        let opened = open(input, &current)?;
        let taken = proposal::taken(vault, &[&current, &opened.model])?;
        let now = (&current.modules[..], &current.collections[..]);
        let id = ids::allocate(|candidate| taken.contains(candidate))?;
        let drawn = opened.drawn.as_ref();
        let (predecessor, base) = match drawn {
            Some(drawn) => (
                drawn.predecessor.clone(),
                opened.base.as_ref().map(|(m, c)| (&m[..], &c[..])),
            ),
            None => (proposal::predecessor_of(&current)?, Some(now)),
        };
        let walkthroughs = match drawn {
            Some(drawn) => {
                let (from, to) = (current.qualify(&drawn.id), current.qualify(&id));
                retold(&opened.model.walkthroughs, &from, &to)
            }
            None => Vec::new(),
        };
        let draft = Proposal {
            schema_version: SCHEMA_VERSION,
            id,
            name: name.clone().unwrap_or(opened.name),
            description: None,
            summary: drawn.and_then(|d| d.summary.clone()),
            documents: drawn.map(|d| d.documents.clone()).unwrap_or_default(),
            status: ProposalStatus::Draft,
            predecessor,
            parent: drawn.and_then(|d| d.parent.clone()),
            created: claims::now(),
            adopted_as: None,
            dropped: drawn.map(|d| d.dropped.clone()).unwrap_or_default(),
            base: base.is_some(),
        };
        let root = BTreeSet::from([current.manifest.id.clone()]);
        claims::check_edit(dir, &current, &writer, writing.claim, &root, &[])?;
        let model = &opened.model;
        let contents = (
            &model.modules[..],
            &model.collections[..],
            &walkthroughs[..],
        );
        let named = (writer.clone(), operation.clone());
        let plan = proposal::drawing("import", &current, draft, contents, base, named, writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// What `model import --restore` made: the vault with its model, current revision and proposals,
/// and the checkpoints the history ref holds, newest first.
#[derive(Debug, Serialize)]
pub struct Restored {
    pub vault: PathBuf,
    pub model: BundleModel,
    pub revision: Revision,
    pub proposals: Vec<String>,
    pub checkpoints: Vec<String>,
}

/// Restores the bundle `input` of a current state, as `model export` writes it, into the checkout
/// around `start`, which must have no vault (`VAULT_EXISTS`) and no history ref
/// (`HISTORY_DIVERGED`). The vault gets the bundle's files as they are, so the model, state,
/// module and every other ID stay, proposals included, and a bundled history is recreated (see
/// [`checkpoint::rebuild`]) and put on the ref. The bundle is checked as `import` checks one, its
/// current state must load at its source's revision, each proposal must load with its base, and
/// nothing is written before all of it holds. Refuses `BUNDLE_INVALID` for a bundle of a proposal
/// or a checkpoint.
pub fn restore(start: &Path, input: &[u8]) -> Result<Restored> {
    let dir = vault::path(start)?;
    if fs::symlink_metadata(&dir).is_ok() {
        return Err(vault_exists(&dir));
    }
    if checkpoint::history_is_local(start)? {
        return Err(Refusal::new(
            Code::HistoryDiverged,
            format!(
                "{} already holds a history; a restore needs a checkout without one",
                checkpoint::HISTORY
            ),
        )
        .into());
    }
    let bundle = parse_bundle(input)?;
    check(&bundle, &bundle.model.id)?;
    let Bundle {
        model,
        source,
        files,
        history,
        ..
    } = bundle;
    let Source::Current { revision, .. } = &source else {
        let message =
            "a restore needs a current-state bundle, exported without --proposal or --checkpoint";
        return Err(invalid(message, json!({ "bundle": source })));
    };
    let temp = TempDir::new("restore")?;
    let staged = temp.path().join("model");
    let files = bytes(files, "");
    checkpoint::put(&staged, slices(&files))?;
    let found = json!({ "bundle": source });
    let located = |e| checkpoint::located(e, PLACE, &found);
    let current = vault::load_dir(staged.clone()).map_err(located)?;
    if current.manifest.id != model.id || current.revision.digest != *revision {
        let message = "its manifest or revision is not the one its header names";
        return Err(invalid(message, json!({ "bundle": source })));
    }
    let proposals = vault::proposals(&staged)?;
    for id in &proposals {
        let drawn = vault::load_proposal(staged.clone(), id).map_err(located)?;
        checked_base(&current, &drawn, &proposal::proposal_of(&drawn)?)?;
    }
    let rebuilt = (history.as_ref())
        .map(|chain| checkpoint::rebuild(start, chain))
        .transpose()?;
    if let Some(state) = dir.parent() {
        refuse_symlink(state)?;
        fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
    }
    refuse_symlink(&dir)?;
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(vault_exists(&dir)),
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    let landed = checkpoint::put(&dir, slices(&files))
        .and_then(|()| rebuilt.as_ref().map_or(Ok(()), checkpoint::Rebuilt::land));
    if let Err(error) = landed {
        let _ = fs::remove_dir_all(&dir);
        return Err(error);
    }
    let checkpoints = (rebuilt.into_iter())
        .flat_map(|r| r.checkpoints)
        .map(|c| c.id)
        .collect();
    Ok(Restored {
        vault: dir,
        model,
        revision: current.revision,
        proposals,
        checkpoints,
    })
}

fn vault_exists(dir: &Path) -> anyhow::Error {
    Refusal::new(
        Code::VaultExists,
        format!(
            "a model vault already exists at {}; a restore needs a checkout without one",
            dir.display()
        ),
    )
    .into()
}

/// A bundle read and loaded: the state it holds, the draft's default name, and for a proposal
/// its base and the proposal itself (`drawn`).
struct Opened {
    model: Model,
    name: String,
    base: Option<(Vec<Module>, Vec<Collection>)>,
    drawn: Option<Proposal>,
}

/// How refusals name a bundle's files instead of the temporary folder they load from.
const PLACE: &str = "the bundle";

/// The bundle `input` checked against `current`'s vault and loaded through a temporary vault, so
/// every file passes the loader's validation.
fn open(input: &[u8], current: &Model) -> Result<Opened> {
    let bundle = parse_bundle(input)?;
    check(&bundle, &current.manifest.id)?;
    let Bundle {
        source, mut files, ..
    } = bundle;
    let temp = TempDir::new("bundle")?;
    let dir = temp.path().join("model");
    let found = json!({ "bundle": source });
    if let Source::Proposal { proposal: id, .. } = &source {
        return open_proposal(current, &dir, id, files, &found);
    }
    files.retain(|path, _| vault::is_vault_file(path));
    let files = bytes(files, "");
    checkpoint::put(&dir, slices(&files))?;
    let model = vault::load_dir(dir).map_err(|e| checkpoint::located(e, PLACE, &found))?;
    if model.manifest.id != current.manifest.id {
        return Err(mismatch(&model.manifest.id, &current.manifest.id));
    }
    let state = model.manifest.current.as_ref();
    let name = state.map_or(&model.manifest.name, |s| &s.name).clone();
    Ok(Opened {
        model,
        name,
        base: None,
        drawn: None,
    })
}

fn retold(walkthroughs: &[Walkthrough], from: &str, to: &str) -> Vec<Walkthrough> {
    let mut walkthroughs = walkthroughs.to_vec();
    let readings = walkthroughs.iter_mut().flat_map(|w| &mut w.beats);
    for reading in readings.map(|b| &mut b.reading) {
        for state in iter::once(&mut reading.state).chain(&mut reading.against) {
            if *state == ReadingState::Proposal(from.to_owned()) {
                *state = ReadingState::Proposal(to.to_owned());
            }
        }
    }
    walkthroughs
}

/// The bundle `input`, refused as every request is, but one that does not parse gets a fixed
/// message, since serde quotes the bundle's own text: its text, line and column go into the
/// details.
fn parse_bundle(input: &[u8]) -> Result<Bundle> {
    check_size(input, "import")?;
    serde_json::from_slice(input).map_err(|e| {
        let details =
            json!({ "error": vault::parse_message(&e), "line": e.line(), "column": e.column() });
        Refusal::new(
            Code::RequestInvalid,
            format!("the bundle does not parse as {SCHEMA}"),
        )
        .with_details(details)
        .into()
    })
}

/// A proposal bundle's `files`, loaded in the temporary vault `dir` with `current`'s manifest and
/// config as the proposal `id`, with its base checked by this vault's rules.
fn open_proposal(
    current: &Model,
    dir: &Path,
    id: &str,
    files: BTreeMap<String, String>,
    found: &Value,
) -> Result<Opened> {
    let manifest = fs::read(current.dir.join(vault::MANIFEST))?;
    let config = fs::read(current.dir.join(vault::CONFIG))?;
    let files = bytes(files, &vault::prefix(id));
    let vault_files = [
        (vault::MANIFEST, &manifest[..]),
        (vault::CONFIG, &config[..]),
    ];
    checkpoint::put(dir, vault_files.into_iter().chain(slices(&files)))?;
    let model = vault::load_proposal(dir.to_path_buf(), id)
        .map_err(|e| checkpoint::located(e, PLACE, found))?;
    let drawn = proposal::proposal_of(&model)?;
    let base = checked_base(current, &model, &drawn)?;
    Ok(Opened {
        model,
        name: drawn.name.clone(),
        base,
        drawn: Some(drawn),
    })
}

/// The base of the proposal `drawn` that `model` loaded, parsed and checked by the rules of
/// `current`'s vault (`BUNDLE_INVALID`).
fn checked_base(
    current: &Model,
    model: &Model,
    drawn: &Proposal,
) -> Result<Option<(Vec<Module>, Vec<Collection>)>> {
    let id = &drawn.id;
    let base = vault::load_base(model).map_err(|e| match problems_of(e) {
        Ok(problems) => base_invalid(id, problems),
        Err(e) => e,
    })?;
    if let Some((modules, collections)) = &base {
        let problems = vault::problems(
            &current.manifest,
            &current.config,
            Some(drawn),
            modules,
            collections,
            (&[], &[], &[]),
        );
        if !problems.is_empty() {
            return Err(base_invalid(id, problems));
        }
    }
    Ok(base)
}

/// The problems of a `MODEL_INVALID` refusal, or the error itself when it is something else.
fn problems_of(error: anyhow::Error) -> Result<Vec<Problem>, anyhow::Error> {
    let problems = (error.downcast_ref::<Refusal>())
        .filter(|r| r.code == Code::ModelInvalid)
        .and_then(|r| r.details.as_ref()?.get("problems").cloned())
        .and_then(|problems| serde_json::from_value(problems).ok());
    problems.ok_or(error)
}

/// `BUNDLE_INVALID` for a base of the proposal `id` with `problems`, named by their paths in the
/// bundle (`base/...`).
fn base_invalid(id: &str, mut problems: Vec<Problem>) -> anyhow::Error {
    let prefix = vault::prefix(id);
    for problem in &mut problems {
        let path = problem.file.strip_prefix(&prefix).unwrap_or(&problem.file);
        problem.file = if path.starts_with(vault::BASE) {
            path.to_owned()
        } else {
            format!("{}{path}", vault::BASE)
        };
    }
    let path = problems.first().map(|p| p.file.clone());
    Refusal::new(
        Code::BundleInvalid,
        format!("the bundle's base has {} problem(s)", problems.len()),
    )
    .with_details(json!({ "path": path, "problems": problems }))
    .into()
}

/// The bundle's `files` as bytes, with `prefix` before each path.
fn bytes(files: BTreeMap<String, String>, prefix: &str) -> Vec<(String, Vec<u8>)> {
    (files.into_iter())
        .map(|(path, text)| (format!("{prefix}{path}"), text.into_bytes()))
        .collect()
}

fn slices(files: &[(String, Vec<u8>)]) -> impl Iterator<Item = (&str, &[u8])> {
    files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
}

/// Refuses a bundle of another model than `model` (`MODEL_MISMATCH`), and one with another
/// schema, a path its source never holds, two paths differing only in case, a missing required
/// file, or a file whose digest does not match (`BUNDLE_INVALID`). The bundle's own text goes
/// into the details only, never into a message.
fn check(bundle: &Bundle, model: &str) -> Result<()> {
    if bundle.schema != SCHEMA {
        let message = format!("its schema is not {SCHEMA}");
        return Err(invalid(&message, json!({ "schema": bundle.schema })));
    }
    if bundle.model.id != model {
        return Err(mismatch(&bundle.model.id, model));
    }
    let (known, required): (fn(&str) -> bool, &[&str]) = match &bundle.source {
        Source::Proposal { proposal, .. } if ids::is_local(proposal) => {
            (vault::is_proposal_file, &[vault::PROPOSAL])
        }
        Source::Proposal { proposal, .. } => {
            let message = "its proposal ID is not 8 base62 characters";
            return Err(invalid(message, json!({ "proposal": proposal })));
        }
        Source::Current { .. } => (
            |path| vault::is_vault_file(path) || vault::is_proposal_vault_file(path),
            &[vault::MANIFEST, vault::CONFIG],
        ),
        Source::Checkpoint { .. } => (vault::is_vault_file, &[vault::MANIFEST, vault::CONFIG]),
    };
    let mut seen = BTreeSet::new();
    for path in bundle.files.keys() {
        if !known(path) {
            return Err(invalid(
                "it holds a path its source never holds",
                json!({ "path": path }),
            ));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(invalid(
                "two of its paths differ only in case",
                json!({ "path": path }),
            ));
        }
    }
    let proposals = (bundle.files.keys())
        .filter_map(|path| vault::split_proposal(path))
        .map(|(id, _)| vault::proposal_file(id));
    let missing = (required.iter().map(|path| (*path).to_owned()))
        .chain(proposals)
        .find(|path| !bundle.files.contains_key(path));
    if let Some(path) = missing {
        return Err(invalid(
            "it lacks a file its source needs",
            json!({ "path": path }),
        ));
    }
    if !bundle.files.keys().eq(bundle.digests.keys()) {
        return Err(invalid(
            "its files and digests name different paths",
            json!({}),
        ));
    }
    let wrong =
        (bundle.files.iter()).find(|(path, text)| hash(text.as_bytes()) != bundle.digests[*path]);
    if let Some((path, _)) = wrong {
        return Err(invalid(
            "a file does not match its digest",
            json!({ "path": path }),
        ));
    }
    Ok(())
}

fn invalid(message: &str, details: Value) -> anyhow::Error {
    Refusal::new(
        Code::BundleInvalid,
        format!("the bundle is not valid: {message}"),
    )
    .with_details(details)
    .into()
}

fn mismatch(found: &str, model: &str) -> anyhow::Error {
    Refusal::new(
        Code::ModelMismatch,
        format!("the bundle holds another model than this vault's model {model}"),
    )
    .with_details(json!({ "bundle_model": found, "model": model }))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_larger_than_import_reads_refuses_on_export() {
        let size = usize::try_from(INPUT_MAX).unwrap();
        let bundle = Bundle {
            schema: SCHEMA.to_owned(),
            model: BundleModel {
                id: "Model001".to_owned(),
                name: "Shop".to_owned(),
            },
            source: Source::Current {
                state: None,
                revision: String::new(),
            },
            files: BTreeMap::from([("model.json".to_owned(), "x".repeat(size))]),
            digests: BTreeMap::new(),
            history: None,
        };
        let error = to_text(&bundle).unwrap_err();
        let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
        assert_eq!(refusal.code, Code::BundleInvalid);
    }
}
