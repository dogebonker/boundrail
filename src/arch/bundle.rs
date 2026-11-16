//! Bundles: one JSON file (schema `boundrail/bundle/v1`, `model schema model-bundle`) carrying
//! the current state, one proposal with its base, or one checkpoint's tree, so a model state
//! moves between clones without Git. `model export` writes one; `model import` reads one into a
//! new draft proposal, so review and adoption stay explicit and the current state never changes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::edit::{self, Receipt};
use super::proposal::{self, Writing};
use super::request::{INPUT_MAX, check_size};
use super::revision::Revision;
use super::types::{Collection, Module, Predecessor, Proposal, ProposalStatus, SCHEMA_VERSION};
use super::vault::{self, Model, Problem, Scope};
use super::{Code, Refusal, checkpoint, claims, ids};
use crate::collect::TempDir;
use crate::files::hash;

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
    /// Each file's text by path: vault-relative for the current state and a checkpoint,
    /// proposal-relative (`proposal.json`, `modules/...`, `base/modules/...`) for a proposal.
    pub files: BTreeMap<String, String>,
    /// The SHA-256 (lowercase hex) of each file's bytes, by the same paths.
    pub digests: BTreeMap<String, String>,
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
    },
    Checkpoint {
        checkpoint: String,
        commit: String,
        revision: String,
    },
}

/// What `model export` exports.
pub enum Exported<'a> {
    Current,
    Proposal(&'a str),
    Checkpoint(&'a str),
}

/// One exported file: its path in the bundle, SHA-256 and text.
type Text = (String, String, String);

/// The bundle of what `what` names in the checkout around `start`.
pub fn export(start: &Path, what: Exported<'_>) -> Result<Bundle> {
    let (model, source, files) = match what {
        Exported::Current => {
            let model = vault::load(start)?;
            let files = vault_files(&model.dir, &model.revision, "")?;
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
            };
            (model, source, files)
        }
        Exported::Checkpoint(which) => {
            let (found, model, blobs) = checkpoint::load_files(start, which)?;
            let files = (blobs.iter())
                .map(|(path, bytes)| text(path.to_owned(), hash(bytes), bytes.to_vec()))
                .collect::<Result<_>>()?;
            let (checkpoint, commit, revision) = (found.id, found.commit, found.revision);
            let source = Source::Checkpoint {
                checkpoint,
                commit,
                revision,
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
/// with the current state as predecessor and base. A proposal bundle gives its proposal's, with
/// that proposal's predecessor, base and dropped IDs, so adopting it after the current state
/// moved refuses `PREDECESSOR_MOVED` until `model reconcile` merges the two. `expected` is the
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
        let (predecessor, base) = match opened.drawn {
            Some(predecessor) => (
                predecessor,
                opened.base.as_ref().map(|(m, c)| (&m[..], &c[..])),
            ),
            None => (proposal::predecessor_of(&current)?, Some(now)),
        };
        let draft = Proposal {
            schema_version: SCHEMA_VERSION,
            id: ids::allocate(|candidate| taken.contains(candidate))?,
            name: name.clone().unwrap_or(opened.name),
            description: None,
            status: ProposalStatus::Draft,
            predecessor,
            created: claims::now(),
            adopted_as: None,
            dropped: opened.dropped,
            base: base.is_some(),
        };
        let root = BTreeSet::from([current.manifest.id.clone()]);
        claims::check_edit(dir, &current, &writer, writing.claim, &root, &[])?;
        let model = &opened.model;
        let contents = (&model.modules[..], &model.collections[..]);
        let named = (writer.clone(), operation.clone());
        let plan = proposal::drawing("import", &current, draft, contents, base, named, writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// A bundle read and loaded: the state it holds, the draft's default name, and for a proposal
/// its base, dropped IDs and predecessor (`drawn`).
struct Opened {
    model: Model,
    name: String,
    base: Option<(Vec<Module>, Vec<Collection>)>,
    dropped: Vec<String>,
    drawn: Option<Predecessor>,
}

/// How refusals name a bundle's files instead of the temporary folder they load from.
const PLACE: &str = "the bundle";

/// The bundle `input` checked against `current`'s vault and loaded through a temporary vault, so
/// every file passes the loader's validation.
fn open(input: &[u8], current: &Model) -> Result<Opened> {
    let bundle = parse_bundle(input)?;
    check(&bundle, &current.manifest.id)?;
    let Bundle { source, files, .. } = bundle;
    let temp = TempDir::new("bundle")?;
    let dir = temp.path().join("model");
    let found = json!({ "bundle": source });
    if let Source::Proposal { proposal: id, .. } = &source {
        return open_proposal(current, &dir, id, files, &found);
    }
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
        dropped: Vec::new(),
        drawn: None,
    })
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
    let base = vault::load_base(&model).map_err(|e| match problems_of(e) {
        Ok(problems) => base_invalid(id, problems),
        Err(e) => e,
    })?;
    if let Some((modules, collections)) = &base {
        let problems = vault::problems(
            &current.manifest,
            &current.config,
            Some(&drawn),
            modules,
            collections,
            &[],
            &[],
        );
        if !problems.is_empty() {
            return Err(base_invalid(id, problems));
        }
    }
    Ok(Opened {
        model,
        name: drawn.name,
        base,
        dropped: drawn.dropped,
        drawn: Some(drawn.predecessor),
    })
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
        _ => (vault::is_vault_file, &[vault::MANIFEST, vault::CONFIG]),
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
    if let Some(path) = required
        .iter()
        .find(|path| !bundle.files.contains_key(**path))
    {
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
        };
        let error = to_text(&bundle).unwrap_err();
        let refusal = error.downcast_ref::<Refusal>().expect("a refusal");
        assert_eq!(refusal.code, Code::BundleInvalid);
    }
}
