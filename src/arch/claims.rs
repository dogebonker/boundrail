//! Claims: machine-local reservations that keep the agents and people editing one checkout out of
//! each other's way. A claim names its writer and the subjects it holds: qualified module and
//! relationship IDs, and the model ID, which holds the root level (adding, removing or moving a
//! root module). A module claim also holds everything inside the module: its parts, their parts,
//! and the relationships they own.
//!
//! Claims are cooperative coordination, not authorization: writers name themselves, and anyone
//! may revoke a claim. They live in `<git dir>/boundrail/claims.json`, so they are never committed
//! and each worktree has its own; separate worktrees do not see each other's claims. They change
//! only under the vault lock; readers rely on the atomic replace instead. A subject that an edit
//! later removes stays in the claim: removed IDs are retired and never allocated again, so it
//! simply never matches.

use std::collections::{BTreeMap, BTreeSet};
use std::iter;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::request::{Target, WRITER_MAX, check_doing, check_model, check_writer, invalid, parse};
use super::types::Relationship;
use super::vault::{self, Model};
use super::{Code, Refusal, ids, one_line, state};

const REASON_MAX: usize = 200;
const SUBJECTS_MAX: usize = 1000;
const FILE: &str = "claims.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub writer: String,
    pub reason: String,
    /// The `--doing` line of the `model claim` that recorded it.
    pub doing: String,
    /// Sorted qualified module and relationship IDs, and the model ID.
    pub subjects: Vec<String>,
    /// Unix seconds.
    pub created: u64,
    /// A revoked claim blocks nobody and stays listed until someone releases it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revocation {
    pub by: String,
    pub reason: String,
    /// Unix seconds.
    pub at: u64,
}

/// The `model claim` request.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    pub target: Target,
    #[schemars(length(min = 1, max = WRITER_MAX))]
    pub writer: String,
    #[schemars(length(min = 1, max = REASON_MAX))]
    pub reason: String,
    /// The model ID, which holds the root level; module IDs or unique current or former names,
    /// each holding the module and everything inside it; and relationship IDs. IDs may be
    /// qualified or local.
    #[schemars(length(min = 1, max = SUBJECTS_MAX))]
    pub subjects: Vec<String>,
}

/// The `model release` request: a writer gives up its own claim, or anyone clears a revoked one.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub claim: String,
    #[schemars(length(min = 1, max = WRITER_MAX))]
    pub writer: String,
}

/// `claims.json`.
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    claims: Vec<Claim>,
}

/// Records a claim from `input`, a [`ClaimRequest`] as JSON, for the vault folder `vault`.
pub fn claim(vault: &Path, input: &[u8], doing: &str) -> Result<Claim> {
    check_doing(doing)?;
    let request: ClaimRequest = parse(input, "claim")?;
    check_writer(&request.writer)?;
    check_reason(&request.reason)?;
    if request.subjects.is_empty() || request.subjects.len() > SUBJECTS_MAX {
        return Err(invalid(format!(
            "subjects needs 1 to {SUBJECTS_MAX} entries"
        )));
    }
    let state = state::lock(vault)?;
    state::refuse_interrupted(&state.dir)?;
    let model = vault::load_dir(vault.to_path_buf())?;
    check_model(&model, &request.target)?;
    let relationships = vault::relationships(&model.modules);
    let subjects = request
        .subjects
        .iter()
        .map(|subject| resolve(&model, &relationships, subject))
        .collect::<Result<BTreeSet<String>>>()?;
    let mut ledger = read(&state.dir)?;
    let up = containers(&model);
    // Holding a module holds what is inside it, so claims on either side of a containment clash.
    refuse_conflicts(
        &ledger.claims,
        &request.writer,
        &subjects,
        |held: &str, wanted: &str| covers(&up, held, wanted) || covers(&up, wanted, held),
    )?;
    let id = ids::allocate(|candidate| {
        ledger
            .claims
            .iter()
            .any(|c| c.id.eq_ignore_ascii_case(candidate))
    })?;
    let claim = Claim {
        id,
        writer: request.writer,
        reason: request.reason,
        doing: doing.to_owned(),
        subjects: subjects.into_iter().collect(),
        created: now(),
        revoked: None,
    };
    ledger.claims.push(claim.clone());
    write(&state.dir, &ledger)?;
    Ok(claim)
}

/// Removes the claim a [`ReleaseRequest`] names. A live claim needs its own writer; a revoked one
/// blocks nobody, so any writer may clear it.
pub fn release(vault: &Path, input: &[u8]) -> Result<Claim> {
    let request: ReleaseRequest = parse(input, "release")?;
    check_writer(&request.writer)?;
    let state = state::lock(vault)?;
    state::refuse_interrupted(&state.dir)?;
    let mut ledger = read(&state.dir)?;
    let at = find(&ledger.claims, &request.claim)?;
    if ledger.claims[at].revoked.is_none() {
        check_holder(&ledger.claims[at], &request.writer)?;
    }
    let claim = ledger.claims.remove(at);
    write(&state.dir, &ledger)?;
    Ok(claim)
}

/// Marks a claim revoked, so it stops blocking anyone; the first revocation stays.
pub fn revoke(vault: &Path, claim: &str, writer: &str, reason: &str) -> Result<Claim> {
    check_writer(writer)?;
    check_reason(reason)?;
    let state = state::lock(vault)?;
    state::refuse_interrupted(&state.dir)?;
    let mut ledger = read(&state.dir)?;
    let at = find(&ledger.claims, claim)?;
    if ledger.claims[at].revoked.is_none() {
        ledger.claims[at].revoked = Some(Revocation {
            by: writer.to_owned(),
            reason: reason.to_owned(),
            at: now(),
        });
        write(&state.dir, &ledger)?;
    }
    Ok(ledger.claims.swap_remove(at))
}

/// Every recorded claim: active ones and revoked ones not yet released.
pub fn list(vault: &Path) -> Result<Vec<Claim>> {
    Ok(read(&state::dir(vault)?)?.claims)
}

/// The claims that block other writers.
pub fn active(vault: &Path) -> Result<Vec<Claim>> {
    let mut claims = list(vault)?;
    claims.retain(|c| c.revoked.is_none());
    Ok(claims)
}

/// The claims among `claims` that hold any of `subjects`, directly or through a container.
pub fn holding(model: &Model, claims: Vec<Claim>, subjects: &[&str]) -> Vec<Claim> {
    let up = containers(model);
    claims
        .into_iter()
        .filter(|c| {
            subjects
                .iter()
                .any(|s| c.subjects.iter().any(|held| covers(&up, held, s)))
        })
        .collect()
}

/// Refuses an edit by `writer` of `model` touching `affected` while another writer's active
/// claim holds any of it, and, when the edit names `claim`, unless that is the writer's live
/// claim holding all of it. `dir` is the state folder, held under the vault lock.
pub(super) fn check_edit(
    dir: &Path,
    model: &Model,
    writer: &str,
    claim: Option<&str>,
    affected: &BTreeSet<String>,
) -> Result<()> {
    let ledger = read(dir)?;
    let up = containers(model);
    let held_by = |held: &str, subject: &str| covers(&up, held, subject);
    let Some(id) = claim else {
        return refuse_conflicts(&ledger.claims, writer, affected, held_by);
    };
    let claim = &ledger.claims[find(&ledger.claims, id)?];
    check_holder(claim, writer)?;
    if let Some(revoked) = &claim.revoked {
        return Err(Refusal::new(
            Code::ClaimRevoked,
            format!(
                "claim {id} was revoked by {}: {}; release it and claim again",
                revoked.by, revoked.reason
            ),
        )
        .with_details(json!({ "claim": id, "revoked": revoked }))
        .into());
    }
    // Claims never overlap across writers when recorded; this guards a hand-edited ledger.
    refuse_conflicts(&ledger.claims, writer, affected, held_by)?;
    let missing: Vec<&String> = affected
        .iter()
        .filter(|s| !claim.subjects.iter().any(|held| held_by(held, s)))
        .collect();
    if !missing.is_empty() {
        return Err(Refusal::new(
            Code::ClaimIncomplete,
            format!(
                "claim {id} does not cover {} subject(s) the edit touches; claim them too",
                missing.len()
            ),
        )
        .with_details(json!({ "claim": id, "missing": missing }))
        .into());
    }
    Ok(())
}

/// The stored form of `subject`: the model ID, or a qualified module or relationship ID.
/// Modules resolve first, as in an edit, so a name never shadows a relationship ID by accident.
fn resolve(
    model: &Model,
    relationships: &BTreeMap<&str, (&str, &Relationship)>,
    subject: &str,
) -> Result<String> {
    let id = &model.manifest.id;
    if subject == id {
        return Ok(subject.to_owned());
    }
    match vault::find(id, &model.modules, subject) {
        Ok(module) => Ok(model.qualify(&module.id)),
        Err(refusal) if refusal.code == Code::SubjectNotFound => {
            let local = ids::local(id, subject);
            if relationships.contains_key(local) {
                return Ok(model.qualify(local));
            }
            Err(Refusal::new(
                Code::SubjectNotFound,
                format!("no model, module or relationship has the ID or name `{subject}`"),
            )
            .into())
        }
        Err(refusal) => Err(refusal.into()),
    }
}

/// The container of each module and relationship, by qualified ID: a module's parent, a
/// relationship's owning module.
fn containers(model: &Model) -> BTreeMap<String, String> {
    let parents = model
        .modules
        .iter()
        .filter_map(|m| Some((model.qualify(&m.id), m.parent.clone()?)));
    let owners = vault::relationships(&model.modules)
        .into_iter()
        .map(|(id, (owner, _))| (model.qualify(id), model.qualify(owner)));
    parents.chain(owners).collect()
}

/// Whether holding `held` holds `subject`: it is the subject or one of its containers.
fn covers(up: &BTreeMap<String, String>, held: &str, subject: &str) -> bool {
    iter::successors(Some(subject), |s| up.get(*s).map(String::as_str))
        .take(up.len() + 1)
        .any(|s| s == held)
}

/// `CLAIM_CONFLICT` when another writer's active claim holds any of `subjects`, as `overlaps`
/// (held subject, wanted subject) decides.
fn refuse_conflicts(
    claims: &[Claim],
    writer: &str,
    subjects: &BTreeSet<String>,
    overlaps: impl Fn(&str, &str) -> bool,
) -> Result<()> {
    let conflicts: Vec<Value> = claims
        .iter()
        .filter(|c| c.revoked.is_none() && c.writer != writer)
        .filter_map(|c| {
            let blocked: Vec<&String> = subjects
                .iter()
                .filter(|s| c.subjects.iter().any(|held| overlaps(held, s)))
                .collect();
            (!blocked.is_empty()).then(
                || json!({"id": c.id, "writer": c.writer, "reason": c.reason, "subjects": blocked}),
            )
        })
        .collect();
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(Refusal::new(
        Code::ClaimConflict,
        format!(
            "{} claim(s) of other writers hold these subjects; wait for their release or ask a person to revoke them",
            conflicts.len()
        ),
    )
    .with_details(json!({ "conflicts": conflicts }))
    .into())
}

fn find(claims: &[Claim], id: &str) -> Result<usize> {
    claims.iter().position(|c| c.id == id).ok_or_else(|| {
        Refusal::new(Code::ClaimNotFound, format!("no claim has the ID `{id}`"))
            .with_details(json!({ "claim": id }))
            .into()
    })
}

fn check_holder(claim: &Claim, writer: &str) -> Result<()> {
    if claim.writer != writer {
        return Err(Refusal::new(
            Code::ClaimNotHeld,
            format!(
                "claim {} belongs to {}, not {writer}",
                claim.id, claim.writer
            ),
        )
        .with_details(json!({ "claim": claim.id, "writer": claim.writer }))
        .into());
    }
    Ok(())
}

fn check_reason(reason: &str) -> Result<()> {
    if one_line(reason, REASON_MAX) {
        return Ok(());
    }
    Err(invalid(format!(
        "reason needs one line of 1 to {REASON_MAX} characters"
    )))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read(dir: &Path) -> Result<Ledger> {
    Ok(state::read(&dir.join(FILE))?.unwrap_or_default())
}

fn write(dir: &Path, ledger: &Ledger) -> Result<()> {
    state::write(&dir.join(FILE), ledger)
}
