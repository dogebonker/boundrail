//! Claims: machine-local reservations that keep the agents and people editing one checkout out of
//! each other's way. A claim names its writer and the subjects it holds: qualified module and
//! relationship IDs, qualified collection, flow, walkthrough and picture IDs, and the model ID,
//! which holds the root level (adding, removing or moving a root module, adding or removing a
//! collection, a walkthrough or a picture). A module claim also holds everything inside the
//! module: its parts, their parts, and the relationships they own. A collection claim holds its
//! flows, a flow claim holds its steps, and a walkthrough claim holds its beats. A proposal
//! claim holds everything inside the proposal. IDs are model-wide, so a claim on a module holds
//! it in the current state and in every proposal; inside a proposal, the proposal's ID holds the
//! root level in place of the model ID.
//!
//! Claims are cooperative coordination, not authorization: writers name themselves, and anyone
//! may revoke a claim. They live in `<git dir>/boundrail/claims.json`, so they are never committed
//! and each worktree has its own; separate worktrees do not see each other's claims. They change
//! only under the vault lock; readers rely on the atomic replace instead. A subject that an edit
//! later removes stays in the claim. The current state retires the IDs it removes and a proposal
//! keeps them in its `dropped` list, so neither allocates them again; an ID that only another
//! state holds, or one a proposal edit both added and removed, can match again only through a
//! random allocation picking it.

use std::collections::{BTreeMap, BTreeSet};
use std::iter;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::request::{
    OPERATION_MAX, OPERATION_PATTERN, Target, WRITER_MAX, check_doing, check_model,
    check_operation, check_writer, invalid, parse,
};
use super::state::{self, Operation};
use super::types::{ProposalStatus, Relationship};
use super::vault::{self, Model};
use super::{Code, Refusal, ids, one_line};

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
    /// each holding the module and everything inside it; relationship IDs; collection, flow,
    /// walkthrough and picture IDs, each holding what is inside it; and proposal IDs, each
    /// holding the whole proposal. IDs may be qualified or local. Names resolve in the state
    /// `target` names: the current state, or a proposal by its ID.
    #[schemars(length(min = 1, max = SUBJECTS_MAX))]
    pub subjects: Vec<String>,
    /// Repeating an operation ID with the same request returns the claim it recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = OPERATION_MAX), pattern(OPERATION_PATTERN))]
    pub operation_id: Option<String>,
}

/// The `model release` request: a writer gives up its own claim, or anyone clears a revoked one.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub claim: String,
    #[schemars(length(min = 1, max = WRITER_MAX))]
    pub writer: String,
    /// Repeating an operation ID with the same request returns the claim it released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = OPERATION_MAX), pattern(OPERATION_PATTERN))]
    pub operation_id: Option<String>,
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
    request
        .operation_id
        .as_deref()
        .map(check_operation)
        .transpose()?;
    let state = state::lock(vault)?;
    let mut ledger = read(&state.dir)?;
    // A claim recorded pending took effect when the ledger holds it.
    let operation = Operation::start(
        &state.dir,
        request.operation_id.as_deref(),
        "claim",
        &request,
        |claim: &Claim| Ok(ledger.claims.iter().any(|c| c.id == claim.id)),
    )?;
    if let Operation::Done(claim) = operation {
        return Ok(claim);
    }
    state::refuse_interrupted(&state.dir)?;
    let model = vault::load_state(vault.to_path_buf(), &request.target.state)?;
    check_model(&model, &request.target)?;
    let relationships = vault::relationships(&model.modules);
    let subjects = request
        .subjects
        .iter()
        .map(|subject| resolve(&model, &relationships, subject))
        .collect::<Result<BTreeSet<String>>>()?;
    // Holding a module holds what is inside it, so claims on either side of a containment clash,
    // in the target and inside every other draft.
    let target = model.proposal.as_ref().map(|p| p.id.clone());
    for scope in iter::once(model).chain(drafts(vault, target.as_deref())?) {
        let up = containers(&scope);
        refuse_conflicts(
            &ledger.claims,
            &request.writer,
            &subjects,
            |held: &str, wanted: &str| covers(&up, held, wanted) || covers(&up, wanted, held),
        )?;
    }
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
    operation.pending(&state.dir, &claim)?;
    ledger.claims.push(claim.clone());
    write(&state.dir, &ledger)?;
    operation.done(&state.dir, &claim)?;
    Ok(claim)
}

/// Removes the claim a [`ReleaseRequest`] names. A live claim needs its own writer; a revoked one
/// blocks nobody, so any writer may clear it.
pub fn release(vault: &Path, input: &[u8]) -> Result<Claim> {
    let request: ReleaseRequest = parse(input, "release")?;
    check_writer(&request.writer)?;
    request
        .operation_id
        .as_deref()
        .map(check_operation)
        .transpose()?;
    let state = state::lock(vault)?;
    let mut ledger = read(&state.dir)?;
    // A release recorded pending took effect when the ledger no longer holds the claim.
    let operation = Operation::start(
        &state.dir,
        request.operation_id.as_deref(),
        "release",
        &request,
        |claim: &Claim| Ok(!ledger.claims.iter().any(|c| c.id == claim.id)),
    )?;
    if let Operation::Done(claim) = operation {
        return Ok(claim);
    }
    state::refuse_interrupted(&state.dir)?;
    let at = find(&ledger.claims, &request.claim)?;
    if ledger.claims[at].revoked.is_none() {
        check_holder(&ledger.claims[at], &request.writer)?;
    }
    let claim = ledger.claims.remove(at);
    operation.pending(&state.dir, &claim)?;
    write(&state.dir, &ledger)?;
    operation.done(&state.dir, &claim)?;
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

/// The claims that block other writers, from the state folder `state` (see
/// [`super::state_dir`]).
pub fn active(state: &Path) -> Result<Vec<Claim>> {
    let mut claims = read(state)?.claims;
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

/// A module an edit moves to another parent.
pub(super) struct Move {
    /// Its qualified ID.
    pub(super) module: String,
    /// The qualified IDs of its containers after the edit, nearest first.
    pub(super) above: Vec<String>,
}

/// Refuses an edit by `writer` of `model` touching `affected` while another writer's active
/// claim holds any of it, and, when the edit names `claim`, unless that is the writer's live
/// claim holding all of it. It also refuses a move in `moves` that would put one writer's claim
/// inside another's. `dir` is the state folder, held under the vault lock.
pub(super) fn check_edit(
    dir: &Path,
    model: &Model,
    writer: &str,
    claim: Option<&str>,
    affected: &BTreeSet<String>,
    moves: &[Move],
) -> Result<()> {
    let ledger = read(dir)?;
    let up = containers(model);
    let held_by = |held: &str, subject: &str| covers(&up, held, subject);
    refuse_nesting(&ledger.claims, &up, moves)?;
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
    // Claims are checked for overlap when recorded, but a draft that did not load then, or a
    // ledger or vault changed by hand, can leave two overlapping; the edit refuses anyway.
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

/// The stored form of `subject`: the model ID, or a qualified module, relationship, collection,
/// flow, walkthrough, picture or view ID. Collection, flow, walkthrough, picture and view IDs
/// resolve before module names, as in `model read`.
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
    let local = ids::local(id, subject);
    if model.find_grouping(local).is_some() || vault::is_proposal(&model.dir, local)? {
        return Ok(model.qualify(local));
    }
    match vault::find(id, &model.modules, subject) {
        Ok(module) => Ok(model.qualify(&module.id)),
        Err(refusal) if refusal.code == Code::SubjectNotFound => {
            if relationships.contains_key(local) {
                return Ok(model.qualify(local));
            }
            Err(Refusal::new(
                Code::SubjectNotFound,
                format!(
                    "no model, proposal, module, relationship, collection, flow, walkthrough, picture or view has the ID or name `{subject}`"
                ),
            )
            .into())
        }
        Err(refusal) => Err(refusal.into()),
    }
}

/// Every draft proposal of the vault folder `vault` but the claim's target `skip`, loaded, so
/// claims clash by the containment of any draft: a module inside another there, or anything
/// inside the draft itself. Adopted and shelved proposals read only and are skipped, by the
/// status their `proposal.json` records. A draft that does not load is skipped with a warning;
/// an edit inside that draft refuses anyway.
// ponytail: one draft load per draft per claim, under the lock; keep a containment index of
// the drafts when vaults hold many large ones.
fn drafts(vault: &Path, skip: Option<&str>) -> Result<Vec<Model>> {
    let mut drafts = Vec::new();
    for id in vault::proposals(vault)? {
        if skip == Some(id.as_str()) {
            continue;
        }
        let loaded = vault::read_proposal(vault, &id).and_then(|proposal| {
            (proposal.status == ProposalStatus::Draft)
                .then(|| vault::load_proposal(vault.to_path_buf(), &id))
                .transpose()
        });
        match loaded {
            Ok(draft) => drafts.extend(draft),
            Err(error) => eprintln!(
                "warning: proposal {id} does not load, so claims are not checked against it: {error:#}"
            ),
        }
    }
    Ok(drafts)
}

/// The container of each module, relationship and flow, by qualified ID: a module's parent, a
/// relationship's owning module, a flow's collection. In a proposal, the proposal contains its
/// root modules and its collections.
fn containers(model: &Model) -> BTreeMap<String, String> {
    let root = model.proposal.as_ref().map(|_| model.root());
    let parents = model.modules.iter().filter_map(|m| {
        let parent = m.parent.clone().or_else(|| root.clone())?;
        Some((model.qualify(&m.id), parent))
    });
    let held = model
        .collections
        .iter()
        .filter_map(|c| Some((model.qualify(&c.id), root.clone()?)));
    let owners = vault::relationships(&model.modules)
        .into_iter()
        .map(|(id, (owner, _))| (model.qualify(id), model.qualify(owner)));
    let collections = model.collections.iter().flat_map(|c| {
        c.flows
            .iter()
            .map(|f| (model.qualify(&f.id), model.qualify(&c.id)))
    });
    parents
        .chain(owners)
        .chain(collections)
        .chain(held)
        .collect()
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

/// `CLAIM_CONFLICT` when a move takes a subject one writer's active claim holds (the module, a
/// module inside it, or a relationship they own) under a module another writer's active claim
/// holds. Claims never overlap across writers when recorded; this keeps a move from making them.
fn refuse_nesting(claims: &[Claim], up: &BTreeMap<String, String>, moves: &[Move]) -> Result<()> {
    let active: Vec<&Claim> = claims.iter().filter(|c| c.revoked.is_none()).collect();
    let mut conflicts = Vec::new();
    for moved in moves {
        for inner in &active {
            let inside: Vec<&String> = inner
                .subjects
                .iter()
                .filter(|s| covers(up, &moved.module, s))
                .collect();
            if inside.is_empty() {
                continue;
            }
            for outer in active.iter().filter(|c| c.writer != inner.writer) {
                let above: Vec<&String> = outer
                    .subjects
                    .iter()
                    .filter(|s| moved.above.contains(s))
                    .collect();
                if !above.is_empty() {
                    conflicts.push(json!({
                        "module": moved.module,
                        "inside": {"id": inner.id, "writer": inner.writer, "subjects": inside},
                        "above": {"id": outer.id, "writer": outer.writer, "subjects": above},
                    }));
                }
            }
        }
    }
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(Refusal::new(
        Code::ClaimConflict,
        "the move would put one writer's claim inside another writer's claim; release one of them first",
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

pub(super) fn now() -> u64 {
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
