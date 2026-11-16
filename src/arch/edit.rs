//! `model edit`: one batch of model changes, applied in order in memory and
//! validated as a whole, then written through a journal under the edit lock so an interrupted
//! write can be completed and every operation replays its receipt. Every journaled write retains
//! a checkpoint of what it wrote before its record is done; the checkpoint is committed, and
//! signed, before the write changes the vault.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(debug_assertions)]
use std::fs;
use std::iter;
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::checkpoint::{self, Checkpoint};
use super::claims;
use super::request::{
    OPERATION_MAX, OPERATION_PATTERN, Target, WRITER_MAX, check_doing, check_model,
    check_operation, check_writer, invalid, parse,
};
use super::revision::{self, Revision};
use super::state::{self, Locked};
use super::types::{
    Anchor, Binding, Collection, Emphasis, Module, Picture, Relationship, SCHEMA_VERSION,
    SavedView, Step, Traffic, Walkthrough, walk,
};
use super::vault::{self, FileWrite, Model, Scope, relationships};
use super::{Code, Refusal, ids, proposal};

mod flow;
mod picture;
mod view;
mod walkthrough;

pub use flow::{
    CollectionField, CollectionSet, FlowField, FlowSet, NewBranch, NewCollection, NewFlow, NewStep,
    StepField, StepSet,
};
pub use picture::{NewPicture, PictureSet};
pub use view::{NamedField, NewView, ViewSet};
pub use walkthrough::{
    BeatField, BeatSet, NewBeat, NewReading, NewWalkthrough, ReadingSet, WalkthroughField,
    WalkthroughSet,
};

const CHANGES_MAX: usize = 1000;
/// Debug builds stop after writing the journal when this is set, so tests can simulate a crash.
#[cfg(debug_assertions)]
const STOP_AFTER_JOURNAL: &str = "BOUNDRAIL_TEST_STOP_AFTER_JOURNAL";
/// Debug builds stop after writing the vault, before (`before`) or after (`after`) retaining the
/// write's checkpoint, when this is set, so tests can simulate a crash there.
#[cfg(debug_assertions)]
const STOP_AT_CHECKPOINT: &str = "BOUNDRAIL_TEST_STOP_AT_CHECKPOINT";
/// Debug builds append a newline to this vault-relative file after planning, so tests can
/// simulate a write from outside the lock while an edit is planned.
#[cfg(debug_assertions)]
const TOUCH_BEFORE_PLAN_CHECK: &str = "BOUNDRAIL_TEST_TOUCH_BEFORE_PLAN_CHECK";

/// The `model edit` request: changes to the current state, or with `--proposal` to a proposal,
/// applied in order as one batch.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub target: Target,
    /// The revision digest the changes were written against.
    pub expected_revision: String,
    /// Repeating an operation ID with the same request replays its receipt.
    #[schemars(length(min = 1, max = OPERATION_MAX), pattern(OPERATION_PATTERN))]
    pub operation_id: String,
    #[schemars(length(min = 1, max = WRITER_MAX))]
    pub writer: String,
    #[schemars(length(min = 1, max = CHANGES_MAX))]
    pub changes: Vec<Change>,
    /// The writer's claim covering every subject the edit touches. Without one, the edit may not
    /// touch a subject that another writer's active claim holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
}

/// One change. A module subject or reference is a qualified ID, a local ID, or a unique current
/// or former name, modules added earlier in the batch included; a collection, flow or walkthrough
/// subject is a qualified or local ID or a unique name, likewise; a relationship, step, branch or
/// beat subject is a qualified or local ID. A step's or beat's position is its index in its list,
/// the end when absent. A picture or view subject is a qualified or local ID or a unique name.
/// Walkthrough, picture and view changes apply to the current state only.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Change {
    AddModule {
        module: NewModule,
    },
    EditModule {
        subject: String,
        #[serde(default)]
        set: ModuleSet,
        #[serde(default)]
        clear: Vec<ModuleField>,
    },
    /// Removes the module and the relationships touching it that the batch does not move.
    RemoveModule {
        subject: String,
    },
    AddRelationship {
        source: String,
        relationship: NewRelationship,
    },
    EditRelationship {
        subject: String,
        /// Moves the relationship, keeping its ID, to this module's outgoing relationships.
        source: Option<String>,
        #[serde(default)]
        set: RelationshipSet,
        #[serde(default)]
        clear: Vec<RelationshipField>,
    },
    RemoveRelationship {
        subject: String,
    },
    AddCollection {
        collection: NewCollection,
    },
    EditCollection {
        subject: String,
        #[serde(default)]
        set: CollectionSet,
        #[serde(default)]
        clear: Vec<CollectionField>,
    },
    /// Removes the collection with its flows.
    RemoveCollection {
        subject: String,
    },
    /// Adds a flow, with the steps it starts with, to a collection.
    AddFlow {
        collection: String,
        flow: NewFlow,
    },
    EditFlow {
        subject: String,
        #[serde(default)]
        set: FlowSet,
        #[serde(default)]
        clear: Vec<FlowField>,
    },
    RemoveFlow {
        subject: String,
    },
    /// Moves a flow with its steps, keeping their IDs, to a collection, at `order` when given.
    MoveFlow {
        subject: String,
        collection: String,
        order: Option<u32>,
    },
    /// Adds a step to the flow's own steps, or to those of the branch or loop `parent` names.
    AddStep {
        flow: String,
        parent: Option<String>,
        position: Option<usize>,
        step: NewStep,
    },
    /// Sets or clears a step's fields; only a message has fields besides its label.
    EditStep {
        subject: String,
        #[serde(default)]
        set: StepSet,
        #[serde(default)]
        clear: Vec<StepField>,
    },
    /// Removes the step with the steps inside it.
    RemoveStep {
        subject: String,
    },
    /// Moves a step with the steps inside it, keeping their IDs, to where `add-step` would put a
    /// new one; a step cannot move inside itself.
    MoveStep {
        subject: String,
        flow: String,
        parent: Option<String>,
        position: Option<usize>,
    },
    /// Adds a branch to a decision.
    AddBranch {
        decision: String,
        position: Option<usize>,
        branch: NewBranch,
    },
    /// Removes the branch with its steps; a decision keeps at least two.
    RemoveBranch {
        subject: String,
    },
    /// Adds a walkthrough with the beats it starts with.
    AddWalkthrough {
        walkthrough: NewWalkthrough,
    },
    EditWalkthrough {
        subject: String,
        #[serde(default)]
        set: WalkthroughSet,
        #[serde(default)]
        clear: Vec<WalkthroughField>,
    },
    /// Removes the walkthrough with its beats.
    RemoveWalkthrough {
        subject: String,
    },
    AddBeat {
        walkthrough: String,
        position: Option<usize>,
        beat: NewBeat,
    },
    /// Sets or clears a beat's title, narration and reading fields.
    EditBeat {
        subject: String,
        #[serde(default)]
        set: BeatSet,
        #[serde(default)]
        clear: Vec<BeatField>,
    },
    RemoveBeat {
        subject: String,
    },
    /// Moves a beat, keeping its ID, to `position` of a walkthrough, this one or another. Within
    /// the same walkthrough, `position` counts the beats after this one is taken out.
    MoveBeat {
        subject: String,
        walkthrough: String,
        position: Option<usize>,
    },
    /// Adds a picture: a reading drawn to a file in the repository.
    AddPicture {
        picture: NewPicture,
    },
    /// Sets or clears a picture's name, order, description, output and reading fields.
    EditPicture {
        subject: String,
        #[serde(default)]
        set: PictureSet,
        #[serde(default)]
        clear: Vec<NamedField>,
    },
    /// Removes the picture's recipe; the file it drew stays.
    RemovePicture {
        subject: String,
    },
    /// Adds a saved view: a named reading that writes no file.
    AddView {
        view: NewView,
    },
    /// Sets or clears a saved view's name, order, description and reading fields.
    EditView {
        subject: String,
        #[serde(default)]
        set: ViewSet,
        #[serde(default)]
        clear: Vec<NamedField>,
    },
    RemoveView {
        subject: String,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewModule {
    pub name: String,
    pub kind: String,
    pub order: u32,
    pub parent: Option<String>,
    pub responsibility: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    pub binding: Option<Binding>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModuleSet {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub order: Option<u32>,
    pub parent: Option<String>,
    pub responsibility: Option<String>,
    pub description: Option<String>,
    pub groups: Option<Vec<String>>,
    pub binding: Option<Binding>,
}

/// A module field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModuleField {
    Parent,
    Responsibility,
    Description,
    Groups,
    Binding,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRelationship {
    pub to: String,
    pub kind: String,
    pub order: u32,
    pub label: Option<String>,
    pub description: Option<String>,
    pub emphasis: Option<Emphasis>,
    pub traffic: Option<Traffic>,
    pub anchor: Option<Anchor>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipSet {
    pub to: Option<String>,
    pub kind: Option<String>,
    pub order: Option<u32>,
    pub label: Option<String>,
    pub description: Option<String>,
    pub emphasis: Option<Emphasis>,
    pub traffic: Option<Traffic>,
    pub anchor: Option<Anchor>,
}

/// A relationship field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipField {
    Label,
    Description,
    Emphasis,
    Traffic,
    Anchor,
}

impl ModuleField {
    fn is_set(self, set: &ModuleSet) -> bool {
        match self {
            Self::Parent => set.parent.is_some(),
            Self::Responsibility => set.responsibility.is_some(),
            Self::Description => set.description.is_some(),
            Self::Groups => set.groups.is_some(),
            Self::Binding => set.binding.is_some(),
        }
    }
}

impl RelationshipField {
    fn is_set(self, set: &RelationshipSet) -> bool {
        match self {
            Self::Label => set.label.is_some(),
            Self::Description => set.description.is_some(),
            Self::Emphasis => set.emphasis.is_some(),
            Self::Traffic => set.traffic.is_some(),
            Self::Anchor => set.anchor.is_some(),
        }
    }
}

/// What an edit did; `model edit` prints it inside the output envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub operation_id: String,
    pub writer: String,
    pub doing: String,
    pub before: Revision,
    pub after: Revision,
    pub subjects: Subjects,
    pub allocated: Vec<Allocation>,
    /// The checkpoint the write retained; receipts written before every write retained one have
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Checkpoint>,
}

/// Qualified IDs of the modules, relationships, collections, flows, walkthroughs, pictures and
/// views the edit added, changed or removed. A module counts as changed when its file changed,
/// including its outgoing relationships, and a collection when its file changed, including its
/// flows. Steps, branches and beats are not listed; the flow or walkthrough holding them counts
/// as changed.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Subjects {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Allocation {
    pub name: String,
    pub id: String,
}

/// A journal record under `<git dir>/boundrail/operations/`: `pending/<operation_id>.json` until
/// the edit finishes, then `<operation_id>.json`. A pending record holds every vault write, so a
/// replay or `model recover` can finish an interrupted edit; a done or abandoned record keeps
/// only the receipt.
#[derive(Serialize, Deserialize)]
pub(super) struct Record {
    /// SHA-256 of the canonical request; `--doing` is not part of it.
    request_sha256: String,
    /// `edit`, `propose`, `adopt` or `shelve`; claim and release records share the journal folder.
    kind: String,
    /// The vault files the revisions cover; records written before proposals cover the current
    /// state.
    #[serde(default)]
    scope: Scope,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writes: Option<Vec<FileWrite>>,
    output: Receipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Done,
    /// `model recover --abandon` dropped it unfinished; replaying it refuses.
    Abandoned,
}

/// What `model recover --abandon` dropped.
#[derive(Debug)]
pub struct Abandoned {
    pub operation_id: String,
    /// Vault files the operation had already written: they hold its planned content.
    pub landed: Vec<String>,
}

/// An edit waiting in the journal.
#[derive(Debug)]
pub struct Interrupted {
    pub operation_id: String,
    /// A writer holds the vault lock: the edit is still running rather than interrupted.
    pub in_progress: bool,
}

/// Applies `input`, an [`EditRequest`] as JSON, to the current state of the vault folder `vault`,
/// or to the proposal with local or qualified ID `proposal`. A repeated operation returns its
/// stored receipt, finishing it first when it was interrupted; the scope is the interrupted
/// record's when this call finished one.
pub fn run(
    vault: &Path,
    input: &[u8],
    doing: &str,
    proposal: Option<&str>,
) -> Result<(Receipt, Option<Scope>)> {
    check_doing(doing)?;
    let request: EditRequest = parse(input, "edit")?;
    check_request(&request)?;
    let operation = request.operation_id.clone();
    journaled(vault, &operation, request, |request, dir, fingerprint| {
        let model = vault::load_target(vault.to_path_buf(), proposal)?;
        check_target(&model, &request)?;
        let claim = request.claim.clone();
        let (record, affected, moves) = plan(&model, request, doing, fingerprint)?;
        let writer = &record.output.writer;
        claims::check_edit(dir, &model, writer, claim.as_deref(), &affected, &moves)?;
        Ok(record)
    })
}

/// A write planned under the vault lock: an edit, or drawing, adopting or shelving a proposal.
pub(super) struct Plan {
    pub(super) kind: &'static str,
    pub(super) scope: Scope,
    pub(super) operation_id: String,
    pub(super) writer: String,
    pub(super) doing: String,
    /// The revision of the files `scope` covers that the plan was made from.
    pub(super) before: Revision,
    pub(super) writes: Vec<FileWrite>,
    pub(super) subjects: Subjects,
    pub(super) allocated: Vec<Allocation>,
}

impl Plan {
    /// The pending journal record of this plan, for a request with SHA-256 `fingerprint`.
    pub(super) fn record(self, fingerprint: String) -> Record {
        let after = self.before.updated(
            (self.writes.iter())
                .map(|w| (w.path.as_str(), w.content.as_deref().map(str::as_bytes))),
        );
        Record {
            request_sha256: fingerprint,
            kind: self.kind.to_owned(),
            scope: self.scope,
            status: Status::Pending,
            before: Some(self.before.digest.clone()),
            after: Some(after.digest.clone()),
            writes: Some(self.writes),
            output: Receipt {
                operation_id: self.operation_id,
                writer: self.writer,
                doing: self.doing,
                before: self.before,
                after,
                subjects: self.subjects,
                allocated: self.allocated,
                checkpoint: None,
            },
        }
    }
}

/// Runs the journaled write `operation` of `request`. A repeated operation returns its stored
/// receipt, finishing it first when it was interrupted; otherwise `plan` plans it under the vault
/// lock, given the request, the state folder and the request's SHA-256, and the plan is written
/// through the journal. The scope is the record's when this call finished an interrupted write.
pub(super) fn journaled<R: Serialize>(
    vault: &Path,
    operation: &str,
    request: R,
    plan: impl FnOnce(R, &Path, String) -> Result<Record>,
) -> Result<(Receipt, Option<Scope>)> {
    let journal = Journal::open(vault)?;
    let (fingerprint, record) = journal.read(operation, &request)?;
    if let Some(record) = record {
        return match record.status {
            Status::Done => {
                let mut receipt = record.output;
                if let Some(stored) = receipt.checkpoint.take() {
                    receipt.checkpoint = Some(checkpoint::held(vault, stored)?);
                }
                Ok((receipt, None))
            }
            Status::Pending => {
                let scope = record.scope.clone();
                complete(vault, &journal, record).map(|receipt| (receipt, Some(scope)))
            }
            Status::Abandoned => Err(Refusal::new(
                Code::OperationAbandoned,
                "this operation was abandoned; plan the change again with a new operation_id",
            )
            .with_details(json!({ "operation_id": operation }))
            .into()),
        };
    }
    state::refuse_interrupted(&journal.state.dir)?;
    let record = plan(request, &journal.state.dir, fingerprint)?;
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os(TOUCH_BEFORE_PLAN_CHECK) {
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(vault.join(path))?
            .write_all(b"\n")?;
    }
    // A file written outside the lock since the model was read: the plan is stale.
    let (changed, current) = changed(vault, &record)?;
    if !changed.is_empty() {
        let current = revision::of_digests(current).digest;
        return Err(Refusal::new(
            Code::StaleRevision,
            "vault files changed while the edit was planned; read the model again and rebase the changes",
        )
        .with_details(json!({ "paths": changed, "current": current }))
        .into());
    }
    // A write the journal could never apply, or whose checkpoint could not be committed and
    // signed, refuses before it is recorded.
    vault::check_writes(vault, record.writes.as_deref().unwrap_or_default())?;
    let mut prepared = drafted(vault, &record)?;
    prepared.build()?;
    journal.write_pending(&record)?;
    #[cfg(debug_assertions)]
    if std::env::var_os(STOP_AFTER_JOURNAL).is_some() {
        anyhow::bail!("stopped after writing the journal because {STOP_AFTER_JOURNAL} is set");
    }
    apply(vault, &journal, record, Some(prepared)).map(|receipt| (receipt, None))
}

/// An interrupted write `model recover` finished: its kind, the vault files it covers, and its
/// receipt.
#[derive(Debug, Serialize)]
pub struct Completed {
    pub kind: String,
    pub scope: Scope,
    #[serde(flatten)]
    pub receipt: Receipt,
}

/// Finishes every interrupted write of the vault folder `vault`. At most one can be pending,
/// since `refuse_interrupted` stops new writes while one waits, so no record's `changed` check
/// ever sees another record's writes.
pub fn recover(vault: &Path) -> Result<Vec<Completed>> {
    let journal = Journal::open(vault)?;
    journal
        .pending()?
        .into_iter()
        .map(|record| {
            let (kind, scope) = (record.kind.clone(), record.scope.clone());
            let receipt = complete(vault, &journal, record)?;
            Ok(Completed {
                kind,
                scope,
                receipt,
            })
        })
        .collect()
}

/// Drops the interrupted edit `operation` without writing the vault, so other writes can go on.
pub fn abandon(vault: &Path, operation: &str) -> Result<Abandoned> {
    check_operation(operation)?;
    let journal = Journal::open(vault)?;
    let dir = &journal.state.dir;
    let record = if state::pending(dir)?.iter().any(|p| p == operation) {
        state::read_record::<Record>(&state::pending_path(dir, operation))?
    } else {
        None
    };
    let Some(mut record) = record else {
        return Err(invalid(format!(
            "no interrupted operation has the ID `{operation}`"
        )));
    };
    let (before, after, current) = snapshot(vault, &record)?;
    let now = |path: &str| current.get(path).map(String::as_str);
    if current.len() == after.len() && after.iter().all(|(path, d)| now(path) == Some(*d)) {
        return Err(invalid(format!(
            "operation {operation} already wrote every file; run `boundrail model recover` to finish it"
        )));
    }
    let landed = record
        .writes
        .iter()
        .flatten()
        .map(|w| w.path.as_str())
        .filter(|path| {
            let planned = after.get(path).copied();
            planned != before.get(path).copied() && now(path) == planned
        })
        .map(str::to_owned)
        .collect();
    record.status = Status::Abandoned;
    record.before = None;
    record.after = None;
    record.writes = None;
    journal.finish(&record)?;
    Ok(Abandoned {
        operation_id: operation.to_owned(),
        landed,
    })
}

/// The edit waiting in the journal of the state folder `state` (see [`super::state_dir`]), if
/// any. It creates nothing; it probes the vault lock with a brief shared lock, which a writer
/// taking the lock meanwhile waits out.
pub fn interrupted(state: &Path) -> Result<Option<Interrupted>> {
    let Some(operation_id) = state::pending(state)?.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(Interrupted {
        operation_id,
        in_progress: state::locked(state)?,
    }))
}

fn check_request(request: &EditRequest) -> Result<()> {
    check_operation(&request.operation_id)?;
    if request.changes.is_empty() || request.changes.len() > CHANGES_MAX {
        return Err(invalid(format!("changes needs 1 to {CHANGES_MAX} entries")));
    }
    check_writer(&request.writer)
}

fn check_target(model: &Model, request: &EditRequest) -> Result<()> {
    check_model(model, &request.target)?;
    if let Some(drawn) = &model.proposal {
        proposal::refuse_closed(drawn)?;
    }
    if request.expected_revision != model.revision.digest {
        return Err(Refusal::new(
            Code::StaleRevision,
            "the model changed since the expected revision; read it again and rebase the changes",
        )
        .with_details(json!({ "current": model.revision.digest }))
        .into());
    }
    Ok(())
}

/// Applies the batch in memory, validates the result and plans its writes as a pending record,
/// returned with the subjects a claim must cover and the modules that move. A proposal retires
/// nothing: it adds the IDs it removes to its `dropped` list, and adoption retires what the
/// adopted state drops.
fn plan(
    model: &Model,
    request: EditRequest,
    doing: &str,
    fingerprint: String,
) -> Result<(Record, BTreeSet<String>, Vec<claims::Move>)> {
    let EditRequest {
        operation_id,
        writer,
        changes,
        ..
    } = request;
    let mut batch = Batch::new(model)?;
    for (index, change) in changes.into_iter().enumerate() {
        batch.apply(index, change)?;
    }
    batch.finish()?;
    let diff = diff(
        model,
        &batch.modules,
        &batch.collections,
        &batch.walkthroughs,
        &batch.pictures,
        &batch.views,
        &batch.fresh,
    );

    // A proposal keeps the IDs it held before the batch and removes, so it never allocates
    // them again; IDs the batch both allocates and removes were never written.
    let drawn = model.proposal.clone().map(|mut drawn| {
        let removed = diff.retired.iter().filter(|id| !batch.fresh.contains(id));
        drawn.dropped.extend(removed.cloned());
        drawn.dropped.sort();
        drawn.dropped.dedup();
        drawn
    });
    let drawn = drawn.as_ref();
    let retired = if drawn.is_none() {
        diff.retired.as_slice()
    } else {
        &[]
    };
    let mut manifest = model.manifest.clone();
    manifest.retired_ids.extend(retired.iter().cloned());
    let mut problems = vault::problems(
        &manifest,
        &model.config,
        drawn,
        &batch.modules,
        &batch.collections,
        (&batch.walkthroughs, &batch.pictures, &batch.views),
    );
    if !problems.is_empty() {
        for problem in &mut problems {
            problem.change = batch.touched.get(&problem.subject).copied();
        }
        return Err(vault::invalid(&model.dir, problems));
    }

    let manifest = (!retired.is_empty()).then_some(&manifest);
    let writes = vault::plan_writes(
        manifest,
        &diff.write,
        &diff.write_collections,
        &diff.write_walkthroughs,
        &diff.write_pictures,
        &diff.write_views,
        &diff.remove,
    )?;
    let (scope, writes) = match drawn {
        Some(p) => {
            let mut writes = vault::in_proposal(&p.id, writes);
            if model.proposal.as_ref() != Some(p) {
                writes.push(vault::proposal_write(p)?);
            }
            (Scope::Proposal(p.id.clone()), writes)
        }
        None => (Scope::Current, writes),
    };
    let plan = Plan {
        kind: "edit",
        scope,
        operation_id,
        writer,
        doing: doing.to_owned(),
        before: model.revision.clone(),
        writes,
        subjects: diff.subjects,
        allocated: batch.allocated,
    };
    Ok((plan.record(fingerprint), diff.affected, diff.moves))
}

/// Finishes a pending record, refusing `RECOVERY_CONFLICT` without writing when a vault file
/// changed since the edit was planned: applying the plan then would overwrite that change.
fn complete(vault: &Path, journal: &Journal, record: Record) -> Result<Receipt> {
    let (changed, _) = changed(vault, &record)?;
    if !changed.is_empty() {
        let operation = &record.output.operation_id;
        return Err(Refusal::new(
            Code::RecoveryConflict,
            format!(
                "vault files changed since operation {operation} was planned; restore them, or run `boundrail model recover --abandon {operation}` to drop it"
            ),
        )
        .with_details(json!({ "operation_id": operation, "paths": changed }))
        .into());
    }
    apply(vault, journal, record, None)
}

/// The vault paths whose current content is neither what `record` found before planning nor what
/// it plans, with the current digests they were judged by. Outside its writes the two agree, so
/// any change there counts.
fn changed(vault: &Path, record: &Record) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    let (before, after, current) = snapshot(vault, record)?;
    let paths: BTreeSet<&str> = current
        .keys()
        .map(String::as_str)
        .chain(before.keys().copied())
        .chain(after.keys().copied())
        .collect();
    let changed = paths
        .into_iter()
        .filter(|path| {
            let now = current.get(*path).map(String::as_str);
            now != before.get(path).copied() && now != after.get(path).copied()
        })
        .map(str::to_owned)
        .collect();
    Ok((changed, current))
}

/// File digests by path: what `record` found before planning, what it plans, and what the vault
/// holds now.
fn snapshot<'a>(vault: &Path, record: &'a Record) -> Result<Digests<'a>> {
    let digests = |revision: &'a Revision| {
        revision
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.sha256.as_str()))
            .collect()
    };
    Ok((
        digests(&record.output.before),
        digests(&record.output.after),
        vault::file_digests(vault, &record.scope)?,
    ))
}

type Digests<'a> = (
    BTreeMap<&'a str, &'a str>,
    BTreeMap<&'a str, &'a str>,
    BTreeMap<String, String>,
);

/// Applies a pending record's writes, checks every vault file reached its planned digest,
/// retains the write's checkpoint, `prepared` before the write or else drafted now, and marks the
/// record done, without loading the model: a newer build may judge a record an older one planned
/// otherwise. Every failure here may follow a vault write, so it is an error rather than a
/// refusal, except a checkpoint that was not retained, which refuses `RECOVERY_REQUIRED` with its
/// cause.
fn apply(
    vault: &Path,
    journal: &Journal,
    mut record: Record,
    prepared: Option<checkpoint::Prepared>,
) -> Result<Receipt> {
    let operation = record.output.operation_id.clone();
    let not_complete = |e: anyhow::Error| {
        anyhow!("{e:#}").context(format!(
            "operation {operation} is not complete; run `boundrail model recover` to finish it"
        ))
    };
    let written = (|| -> Result<()> {
        vault::apply_writes(vault, record.writes.as_deref().unwrap_or_default())?;
        let reached = revision::of_digests(vault::file_digests(vault, &record.scope)?);
        let planned = &record.output.after;
        ensure!(
            reached == *planned,
            "the vault revision is {} after writing, not the planned {}",
            reached.digest,
            planned.digest
        );
        Ok(())
    })();
    written.map_err(not_complete)?;
    #[cfg(debug_assertions)]
    stop_at_checkpoint("before")?;
    let prepared = match prepared {
        Some(prepared) => Ok(prepared),
        None => drafted(vault, &record),
    };
    let retained = prepared.and_then(checkpoint::retain_write);
    record.output.checkpoint = Some(retained.map_err(|e| unretained(&operation, &e))?);
    #[cfg(debug_assertions)]
    stop_at_checkpoint("after")?;
    record.status = Status::Done;
    record.before = None;
    record.after = None;
    record.writes = None;
    journal.finish(&record).map_err(not_complete)?;
    Ok(record.output)
}

/// The checkpoint `record` retains, drafted from the files it plans.
fn drafted(vault: &Path, record: &Record) -> Result<checkpoint::Prepared> {
    let output = &record.output;
    checkpoint::draft_write(
        vault,
        checkpointed(&record.scope),
        record.writes.as_deref().unwrap_or_default(),
        &output.after,
        &output.doing,
        &output.writer,
        &output.operation_id,
    )
}

/// The proposal a write of `scope` retains a checkpoint of: the one a proposal write or a
/// reconcile changes. An adoption's checkpoint holds the current state it makes.
fn checkpointed(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::Proposal(id) | Scope::Reconcile(id) => Some(id),
        Scope::Current | Scope::Adoption(_) => None,
    }
}

/// `RECOVERY_REQUIRED` for the write `operation`, which wrote the vault but did not retain its
/// checkpoint because of `cause`; finishing it again retains the checkpoint.
fn unretained(operation: &str, cause: &anyhow::Error) -> anyhow::Error {
    let (message, details) = match cause.downcast_ref::<Refusal>() {
        Some(refusal) => (
            refusal.message.clone(),
            json!({ "code": refusal.code, "message": refusal.message, "details": refusal.details }),
        ),
        None => {
            let message = format!("{cause:#}");
            (message.clone(), json!({ "message": message }))
        }
    };
    Refusal::new(
        Code::RecoveryRequired,
        format!(
            "operation {operation} wrote the vault but did not retain its checkpoint: {message}; fix that, then run `boundrail model recover` to retain it"
        ),
    )
    .with_details(json!({ "operation_id": operation, "cause": details }))
    .into()
}

/// Stops when [`STOP_AT_CHECKPOINT`] names `point`.
#[cfg(debug_assertions)]
fn stop_at_checkpoint(point: &str) -> Result<()> {
    if std::env::var_os(STOP_AT_CHECKPOINT).is_some_and(|v| v == point) {
        anyhow::bail!("stopped {point} the checkpoint because {STOP_AT_CHECKPOINT} is set");
    }
    Ok(())
}

/// What the batch, or an adopted proposal, changed against the loaded model.
pub(super) struct Diff<'a> {
    /// Module files to write: added and changed modules.
    pub(super) write: Vec<&'a Module>,
    /// Collection files to write: added and changed collections.
    pub(super) write_collections: Vec<&'a Collection>,
    /// Walkthrough files to write: added and changed walkthroughs.
    pub(super) write_walkthroughs: Vec<&'a Walkthrough>,
    /// Picture files to write: added and changed pictures.
    pub(super) write_pictures: Vec<&'a Picture>,
    /// View files to write: added and changed views.
    pub(super) write_views: Vec<&'a SavedView>,
    /// Vault-relative paths of module, collection, walkthrough, picture and view files to delete.
    pub(super) remove: Vec<String>,
    /// IDs leaving the model, allocated-then-removed ones included; they stay reserved.
    pub(super) retired: Vec<String>,
    pub(super) subjects: Subjects,
    /// The pre-existing subjects a claim must cover, and the model's root subject for the root
    /// level.
    pub(super) affected: BTreeSet<String>,
    /// Pre-existing modules that change parent.
    pub(super) moves: Vec<claims::Move>,
}

pub(super) fn diff<'a>(
    model: &'a Model,
    new: &'a [Module],
    collections: &'a [Collection],
    walkthroughs: &'a [Walkthrough],
    pictures: &'a [Picture],
    views: &'a [SavedView],
    fresh: &[String],
) -> Diff<'a> {
    let (old_modules, new_modules) = (by_id(&model.modules), by_id(new));
    let (old_relationships, new_relationships) =
        (relationships(&model.modules), relationships(new));
    let grouped: BTreeSet<&str> = (collections.iter().flat_map(Collection::ids))
        .chain(walkthroughs.iter().flat_map(Walkthrough::ids))
        .chain(pictures.iter().map(|p| p.id.as_str()))
        .chain(views.iter().map(|v| v.id.as_str()))
        .collect();
    let live = |id: &str| {
        new_modules.contains_key(id) || new_relationships.contains_key(id) || grouped.contains(id)
    };
    let mut diff = Diff {
        write: Vec::new(),
        write_collections: Vec::new(),
        write_walkthroughs: Vec::new(),
        write_pictures: Vec::new(),
        write_views: Vec::new(),
        remove: Vec::new(),
        retired: Vec::new(),
        subjects: Subjects::default(),
        affected: BTreeSet::new(),
        moves: Vec::new(),
    };
    for (id, module) in &new_modules {
        match old_modules.get(id) {
            None => diff.subjects.added.push(model.qualify(id)),
            Some(before) if before != module => diff.subjects.changed.push(model.qualify(id)),
            Some(_) => continue,
        }
        diff.write.push(module);
    }
    for (id, relationship) in &new_relationships {
        match old_relationships.get(id) {
            None => diff.subjects.added.push(model.qualify(id)),
            Some(before) if before != relationship => diff.subjects.changed.push(model.qualify(id)),
            Some(_) => {}
        }
    }
    for id in old_modules.keys().chain(old_relationships.keys()) {
        if !live(id) {
            diff.subjects.removed.push(model.qualify(id));
            diff.retired.push((*id).to_owned());
        }
    }
    diff.remove = old_modules
        .keys()
        .filter(|id| !new_modules.contains_key(*id))
        .map(|id| vault::module_file(id))
        .collect();
    let mut touched: Vec<Option<String>> = Vec::new();
    flow::diff_collections(model, collections, &mut diff, &mut touched);
    let told = &mut touched;
    diff.write_walkthroughs =
        walkthrough::diff_current_only(model, &model.walkthroughs, walkthroughs, &mut diff, told);
    diff.write_pictures =
        walkthrough::diff_current_only(model, &model.pictures, pictures, &mut diff, told);
    diff.write_views = walkthrough::diff_current_only(model, &model.views, views, &mut diff, told);
    diff.retired
        .extend(fresh.iter().filter(|id| !live(id)).cloned());
    for list in [
        &mut diff.subjects.added,
        &mut diff.subjects.changed,
        &mut diff.subjects.removed,
    ] {
        list.sort();
    }

    // The subjects a claim must cover, `None` standing for the root level that the model ID
    // holds. A module or relationship is touched when it changes or leaves. So is a module
    // gaining or losing a part, or the root level for a root module, and the old and new source
    // and target of each relationship added, changed, moved or removed. So is every module whose
    // current or former name an added or renamed module takes, since the name would then stop
    // naming it alone. Collections and flows are touched as `flow::diff_collections` says, and
    // walkthroughs, pictures and views as `walkthrough::diff_current_only` says. IDs
    // this batch allocates are left out: allocation skips every ID the edited state holds or
    // reserves, so a claim holds one only by a random clash with an ID that only another state
    // holds (a proposal's, or for a proposal edit the current state's).
    let module_ids: BTreeSet<&str> = old_modules
        .keys()
        .chain(new_modules.keys())
        .copied()
        .collect();
    for id in module_ids {
        let (before, after) = (old_modules.get(id), new_modules.get(id));
        if before == after {
            continue;
        }
        touched.push(Some(model.qualify(id)));
        let parent = |module: Option<&&Module>| module.map(|m| m.parent.clone());
        if parent(before) != parent(after) {
            touched.extend(parent(before).into_iter().chain(parent(after)));
            if let (Some(_), Some(after)) = (before, after) {
                diff.moves.push(claims::Move {
                    module: model.qualify(id),
                    above: above(&model.manifest.id, &new_modules, after),
                });
            }
        }
        if let Some(named) = after.filter(|a| before.is_none_or(|b| b.name != a.name)) {
            touched.extend(
                model
                    .modules
                    .iter()
                    .filter(|m| vault::holds_name(&m.name, &m.former_names, &named.name))
                    .map(|m| Some(model.qualify(&m.id))),
            );
        }
    }
    let relationship_ids: BTreeSet<&str> = old_relationships
        .keys()
        .chain(new_relationships.keys())
        .copied()
        .collect();
    for id in relationship_ids {
        let (before, after) = (old_relationships.get(id), new_relationships.get(id));
        if before == after {
            continue;
        }
        touched.push(Some(model.qualify(id)));
        for (owner, relationship) in before.into_iter().chain(after) {
            touched.push(Some(model.qualify(owner)));
            touched.push(Some(relationship.to.clone()));
        }
    }
    let existed = |id: &str| {
        let local = ids::local(&model.manifest.id, id);
        old_modules.contains_key(local)
            || old_relationships.contains_key(local)
            || model.find_grouping(local).is_some()
    };
    diff.affected = touched
        .into_iter()
        .filter_map(|subject| match subject {
            None => Some(model.root()),
            Some(id) => existed(&id).then_some(id),
        })
        .collect();
    // A batch that only retires IDs, adding and removing a module, still rewrites the manifest.
    if diff.affected.is_empty() && !diff.retired.is_empty() {
        diff.affected.insert(model.root());
    }
    diff
}

/// The qualified IDs of `module`'s containers among `modules`, nearest first. The batch is not
/// validated yet, so a containment cycle must not loop forever.
fn above(model: &str, modules: &BTreeMap<&str, &Module>, module: &Module) -> Vec<String> {
    iter::successors(module.parent.as_deref(), |up| {
        modules
            .get(ids::local(model, up))
            .and_then(|m| m.parent.as_deref())
    })
    .take(modules.len())
    .map(str::to_owned)
    .collect()
}

fn by_id(modules: &[Module]) -> BTreeMap<&str, &Module> {
    modules.iter().map(|m| (m.id.as_str(), m)).collect()
}

/// The model while a batch applies. Removed modules stay until `finish`, so a later change can
/// still move their relationships or reparent their children.
struct Batch<'a> {
    model: &'a str,
    modules: Vec<Module>,
    collections: Vec<Collection>,
    walkthroughs: Vec<Walkthrough>,
    pictures: Vec<Picture>,
    views: Vec<SavedView>,
    /// Whether the batch edits a proposal, which holds no walkthroughs, pictures or views.
    in_proposal: bool,
    removed: BTreeSet<String>,
    /// Local IDs of relationships the batch removed or gave another source or target.
    moved: BTreeSet<String>,
    /// Every ID the model holds or reserves, every proposal folder's name, and those allocated
    /// so far, in ASCII lowercase.
    taken: BTreeSet<String>,
    fresh: Vec<String>,
    allocated: Vec<Allocation>,
    edited: BTreeSet<String>,
    /// The last change that touched each module, relationship, collection, flow, step, branch,
    /// walkthrough, beat, picture or view, by local ID.
    touched: BTreeMap<String, usize>,
}

impl<'a> Batch<'a> {
    fn new(model: &'a Model) -> Result<Self> {
        let taken = proposal::taken(&model.dir, &[model])?;
        Ok(Self {
            model: &model.manifest.id,
            modules: model.modules.clone(),
            collections: model.collections.clone(),
            walkthroughs: model.walkthroughs.clone(),
            pictures: model.pictures.clone(),
            views: model.views.clone(),
            in_proposal: model.proposal.is_some(),
            removed: BTreeSet::new(),
            moved: BTreeSet::new(),
            taken,
            fresh: Vec::new(),
            allocated: Vec::new(),
            edited: BTreeSet::new(),
            touched: BTreeMap::new(),
        })
    }

    fn apply(&mut self, index: usize, change: Change) -> Result<()> {
        match change {
            Change::AddModule { module } => self.add_module(index, module),
            Change::EditModule {
                subject,
                set,
                clear,
            } => self.edit_module(index, &subject, set, &clear),
            Change::RemoveModule { subject } => self.remove_module(index, &subject),
            Change::AddRelationship {
                source,
                relationship,
            } => self.add_relationship(index, &source, relationship),
            Change::EditRelationship {
                subject,
                source,
                set,
                clear,
            } => self.edit_relationship(index, &subject, source.as_deref(), set, &clear),
            Change::RemoveRelationship { subject } => self.remove_relationship(index, &subject),
            Change::AddCollection { collection } => self.add_collection(index, collection),
            Change::EditCollection {
                subject,
                set,
                clear,
            } => self.edit_collection(index, &subject, set, &clear),
            Change::RemoveCollection { subject } => self.remove_collection(index, &subject),
            Change::AddFlow { collection, flow } => self.add_flow(index, &collection, flow),
            Change::EditFlow {
                subject,
                set,
                clear,
            } => self.edit_flow(index, &subject, set, &clear),
            Change::RemoveFlow { subject } => self.remove_flow(index, &subject),
            Change::MoveFlow {
                subject,
                collection,
                order,
            } => self.move_flow(index, &subject, &collection, order),
            Change::AddStep {
                flow,
                parent,
                position,
                step,
            } => self.add_step(index, &flow, parent.as_deref(), position, step),
            Change::EditStep {
                subject,
                set,
                clear,
            } => self.edit_step(index, &subject, set, &clear),
            Change::RemoveStep { subject } => self.take_step(index, &subject).map(drop),
            Change::MoveStep {
                subject,
                flow,
                parent,
                position,
            } => self.move_step(index, &subject, &flow, parent.as_deref(), position),
            Change::AddBranch {
                decision,
                position,
                branch,
            } => self.add_branch(index, &decision, position, branch),
            Change::RemoveBranch { subject } => self.remove_branch(index, &subject),
            Change::AddWalkthrough { walkthrough } => {
                self.apply_walkthrough(index, |b| b.add_walkthrough(index, walkthrough))
            }
            Change::EditWalkthrough {
                subject,
                set,
                clear,
            } => {
                self.apply_walkthrough(index, |b| b.edit_walkthrough(index, &subject, set, &clear))
            }
            Change::RemoveWalkthrough { subject } => {
                self.apply_walkthrough(index, |b| b.remove_walkthrough(index, &subject))
            }
            Change::AddBeat {
                walkthrough,
                position,
                beat,
            } => self.apply_walkthrough(index, |b| b.add_beat(index, &walkthrough, position, beat)),
            Change::EditBeat {
                subject,
                set,
                clear,
            } => self.apply_walkthrough(index, |b| b.edit_beat(index, &subject, set, &clear)),
            Change::RemoveBeat { subject } => {
                self.apply_walkthrough(index, |b| b.take_beat(index, &subject).map(drop))
            }
            Change::MoveBeat {
                subject,
                walkthrough,
                position,
            } => self.apply_walkthrough(index, |b| {
                b.move_beat(index, &subject, &walkthrough, position)
            }),
            Change::AddPicture { picture } => {
                self.apply_current(index, "pictures", |b| b.add_picture(index, picture))
            }
            Change::EditPicture {
                subject,
                set,
                clear,
            } => self.apply_current(index, "pictures", |b| {
                b.edit_picture(index, &subject, set, &clear)
            }),
            Change::RemovePicture { subject } => self.apply_current(index, "pictures", |b| {
                b.remove_named(index, &subject, |b| &mut b.pictures)
            }),
            Change::AddView { view } => {
                self.apply_current(index, "views", |b| b.add_view(index, view))
            }
            Change::EditView {
                subject,
                set,
                clear,
            } => self.apply_current(index, "views", |b| {
                b.edit_named(index, &subject, set, &clear, |b| &mut b.views)
                    .map(drop)
            }),
            Change::RemoveView { subject } => self.apply_current(index, "views", |b| {
                b.remove_named(index, &subject, |b| &mut b.views)
            }),
        }
    }

    fn qualify(&self, at: usize) -> String {
        ids::qualify(self.model, &self.modules[at].id)
    }

    fn allocate(&mut self) -> Result<String> {
        let id = ids::allocate(|candidate| self.taken.contains(candidate))?;
        self.taken.insert(id.to_ascii_lowercase());
        self.fresh.push(id.clone());
        Ok(id)
    }

    /// A fresh ID for what change `index` adds, reported as allocated for `name`.
    fn new_id(&mut self, index: usize, name: String) -> Result<String> {
        let id = self.allocate()?;
        self.allocated.push(Allocation {
            name,
            id: ids::qualify(self.model, &id),
        });
        self.touched.insert(id.clone(), index);
        Ok(id)
    }

    /// The position of the module `subject` names among those not removed.
    fn module(&self, index: usize, subject: &str) -> Result<usize> {
        let live = (self.modules.iter().enumerate()).filter(|(_, m)| !self.removed.contains(&m.id));
        let (at, _) = vault::find_keyed(self.model, live, subject).map_err(at(index))?;
        Ok(at)
    }

    /// The owning module's position and the relationship's position in its outgoing list.
    fn relationship(&self, index: usize, subject: &str) -> Result<(usize, usize)> {
        let local = ids::local(self.model, subject);
        self.modules
            .iter()
            .enumerate()
            .find_map(|(m, module)| {
                let r = module.outgoing.iter().position(|r| r.id == local)?;
                Some((m, r))
            })
            .ok_or_else(|| {
                at(index)(Refusal::new(
                    Code::SubjectNotFound,
                    format!("no relationship has the ID `{subject}`"),
                ))
                .into()
            })
    }

    /// Records the edit of `id`, refusing a second edit of the same subject.
    fn edit_once(&mut self, index: usize, id: &str) -> Result<()> {
        if !self.edited.insert(id.to_owned()) {
            return Err(request_invalid(
                index,
                format!(
                    "{} is already edited by an earlier change; combine the edits",
                    ids::qualify(self.model, id)
                ),
            ));
        }
        Ok(())
    }

    fn reference(&self, index: usize, subject: Option<&str>) -> Result<Option<String>> {
        subject
            .map(|s| self.module(index, s).map(|at| self.qualify(at)))
            .transpose()
    }

    fn add_module(&mut self, index: usize, new: NewModule) -> Result<()> {
        let parent = self.reference(index, new.parent.as_deref())?;
        let id = self.new_id(index, new.name.clone())?;
        self.modules.push(Module {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            kind: new.kind,
            order: new.order,
            parent,
            responsibility: new.responsibility,
            description: new.description,
            groups: new.groups,
            binding: new.binding,
            former_names: Vec::new(),
            outgoing: Vec::new(),
        });
        Ok(())
    }

    fn edit_module(
        &mut self,
        index: usize,
        subject: &str,
        set: ModuleSet,
        clear: &[ModuleField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let at = self.module(index, subject)?;
        let id = self.modules[at].id.clone();
        self.edit_once(index, &id)?;
        let parent = self.reference(index, set.parent.as_deref())?;
        let module = &mut self.modules[at];
        if let Some(name) = set.name {
            rename(&mut module.name, &mut module.former_names, name);
        }
        if let Some(kind) = set.kind {
            module.kind = kind;
        }
        if let Some(order) = set.order {
            module.order = order;
        }
        if parent.is_some() {
            module.parent = parent;
        }
        if set.responsibility.is_some() {
            module.responsibility = set.responsibility;
        }
        if set.description.is_some() {
            module.description = set.description;
        }
        if let Some(groups) = set.groups {
            module.groups = groups;
        }
        if set.binding.is_some() {
            module.binding = set.binding;
        }
        for field in clear {
            match field {
                ModuleField::Parent => module.parent = None,
                ModuleField::Responsibility => module.responsibility = None,
                ModuleField::Description => module.description = None,
                ModuleField::Groups => module.groups.clear(),
                ModuleField::Binding => module.binding = None,
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    fn remove_module(&mut self, index: usize, subject: &str) -> Result<()> {
        let at = self.module(index, subject)?;
        let id = self.modules[at].id.clone();
        self.touched.insert(id.clone(), index);
        self.removed.insert(id);
        Ok(())
    }

    fn add_relationship(&mut self, index: usize, source: &str, new: NewRelationship) -> Result<()> {
        let from = self.module(index, source)?;
        let target = self.module(index, &new.to)?;
        let name = format!(
            "relationship from {} to {}",
            self.modules[from].name, self.modules[target].name
        );
        let id = self.new_id(index, name)?;
        let to = self.qualify(target);
        self.modules[from].outgoing.push(Relationship {
            id,
            to,
            kind: new.kind,
            order: new.order,
            label: new.label,
            description: new.description,
            emphasis: new.emphasis,
            traffic: new.traffic,
            anchor: new.anchor,
        });
        Ok(())
    }

    fn edit_relationship(
        &mut self,
        index: usize,
        subject: &str,
        source: Option<&str>,
        set: RelationshipSet,
        clear: &[RelationshipField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let (from, at) = self.relationship(index, subject)?;
        let id = self.modules[from].outgoing[at].id.clone();
        self.edit_once(index, &id)?;
        let owner = source
            .map(|s| self.module(index, s))
            .transpose()?
            .unwrap_or(from);
        let to = self.reference(index, set.to.as_deref())?;
        if owner != from || to.is_some() {
            self.moved.insert(id.clone());
        }
        // Only a transfer moves the relationship, so an edit keeps its place in the file.
        let at = if owner == from {
            at
        } else {
            let moved = self.modules[from].outgoing.remove(at);
            self.modules[owner].outgoing.push(moved);
            self.modules[owner].outgoing.len() - 1
        };
        let relationship = &mut self.modules[owner].outgoing[at];
        if let Some(to) = to {
            relationship.to = to;
        }
        if let Some(kind) = set.kind {
            relationship.kind = kind;
        }
        if let Some(order) = set.order {
            relationship.order = order;
        }
        if set.label.is_some() {
            relationship.label = set.label;
        }
        if set.description.is_some() {
            relationship.description = set.description;
        }
        if set.emphasis.is_some() {
            relationship.emphasis = set.emphasis;
        }
        if set.traffic.is_some() {
            relationship.traffic = set.traffic;
        }
        if set.anchor.is_some() {
            relationship.anchor = set.anchor;
        }
        for field in clear {
            match field {
                RelationshipField::Label => relationship.label = None,
                RelationshipField::Description => relationship.description = None,
                RelationshipField::Emphasis => relationship.emphasis = None,
                RelationshipField::Traffic => relationship.traffic = None,
                RelationshipField::Anchor => relationship.anchor = None,
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    fn remove_relationship(&mut self, index: usize, subject: &str) -> Result<()> {
        let (from, at) = self.relationship(index, subject)?;
        let relationship = self.modules[from].outgoing.remove(at);
        self.moved.insert(relationship.id.clone());
        self.touched.insert(relationship.id, index);
        Ok(())
    }

    /// Drops removed modules and every relationship from or to them, refusing when a surviving
    /// module still names a removed parent.
    fn finish(&mut self) -> Result<()> {
        let gone: BTreeSet<String> = self
            .removed
            .iter()
            .map(|id| ids::qualify(self.model, id))
            .collect();
        let orphans: Vec<String> = self
            .modules
            .iter()
            .filter(|m| !self.removed.contains(&m.id))
            .filter(|m| m.parent.as_ref().is_some_and(|p| gone.contains(p)))
            .map(|m| ids::qualify(self.model, &m.id))
            .collect();
        if !orphans.is_empty() {
            return Err(Refusal::new(
                Code::ChildrenOrphaned,
                format!(
                    "{} module(s) would lose their parent; remove or reparent them in the same batch",
                    orphans.len()
                ),
            )
            .with_details(json!({ "children": orphans }))
            .into());
        }
        self.modules.retain(|m| !self.removed.contains(&m.id));
        for module in &mut self.modules {
            module.outgoing.retain(|r| !gone.contains(&r.to));
        }
        self.refuse_flow_references(&gone)?;
        self.refuse_walkthrough_references()?;
        self.refuse_picture_references()?;
        self.refuse_view_references()
    }

    /// Refuses when a message step the batch did not edit references a module the batch removes,
    /// or a relationship it removes or moves, and the reference no longer holds, or when a flow it
    /// did not edit lists a removed module as a participant. A step or flow the batch edited is
    /// judged by validation instead.
    fn refuse_flow_references(&self, gone: &BTreeSet<String>) -> Result<()> {
        if gone.is_empty() && self.moved.is_empty() {
            return Ok(());
        }
        let ends = vault::Ends::new(self.model, &self.modules);
        let moved: BTreeSet<String> = (self.moved.iter())
            .map(|id| ids::qualify(self.model, id))
            .collect();
        let local = |subject: &String| ids::local(self.model, subject).to_owned();
        // The changes that removed or moved what each broken step or flow references.
        let mut causes: BTreeSet<usize> = BTreeSet::new();
        let mut steps = Vec::new();
        for (step, _) in (self.collections.iter())
            .flat_map(|c| &c.flows)
            .flat_map(|f| walk(&f.steps))
        {
            let Step::Message {
                id,
                from,
                to,
                relationship,
                ..
            } = step
            else {
                continue;
            };
            let broken: Vec<&String> = [from, to]
                .into_iter()
                .filter(|m| gone.contains(*m))
                .chain(relationship.iter().filter(|r| moved.contains(*r)))
                .collect();
            if broken.is_empty()
                || self.edited.contains(id)
                || vault::message_problems(&ends, step).is_empty()
            {
                continue;
            }
            causes.extend(
                broken
                    .into_iter()
                    .filter_map(|s| self.touched.get(&local(s)).copied()),
            );
            steps.push(ids::qualify(self.model, id));
        }
        let mut flows = Vec::new();
        for flow in self.collections.iter().flat_map(|c| &c.flows) {
            let broken: Vec<&String> = (flow.participants.iter())
                .filter(|m| gone.contains(*m))
                .collect();
            if broken.is_empty() || self.edited.contains(&flow.id) {
                continue;
            }
            causes.extend(
                broken
                    .into_iter()
                    .filter_map(|s| self.touched.get(&local(s)).copied()),
            );
            flows.push(ids::qualify(self.model, &flow.id));
        }
        if steps.is_empty() && flows.is_empty() {
            return Ok(());
        }
        let refusal = Refusal::new(
            Code::ReferencedByFlow,
            format!(
                "{} flow step(s) and {} flow participant list(s) reference modules or relationships the batch removes or moves; edit or remove them in the same batch",
                steps.len(),
                flows.len()
            ),
        )
        .with_details(json!({ "steps": steps, "flows": flows }));
        Err(match causes.into_iter().collect::<Vec<_>>().as_slice() {
            [index] => at(*index)(refusal),
            _ => refusal,
        }
        .into())
    }
}

/// A rename keeps the old name as a former name; renaming back to a former name drops it.
fn rename(current: &mut String, former_names: &mut Vec<String>, name: String) {
    if name == *current {
        return;
    }
    former_names.retain(|n| *n != name);
    let old = std::mem::replace(current, name);
    if !former_names.contains(&old) {
        former_names.push(old);
    }
}

/// Names change `index` in a refusal's message and details.
fn at(index: usize) -> impl Fn(Refusal) -> Refusal {
    move |mut refusal| {
        refusal.message = format!("changes[{index}]: {}", refusal.message);
        let mut details = refusal.details.take().unwrap_or_else(|| json!({}));
        details["change"] = index.into();
        refusal.with_details(details)
    }
}

fn request_invalid(index: usize, message: String) -> anyhow::Error {
    at(index)(Refusal::new(Code::RequestInvalid, message)).into()
}

/// A change that sets and clears the same field, named as the request spells it.
fn both(index: usize, field: impl Serialize) -> anyhow::Error {
    let name = serde_json::to_value(field)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    request_invalid(index, format!("sets and clears `{name}`"))
}

/// The journal in `<git dir>/boundrail/operations`, read and written under the vault lock.
struct Journal {
    state: Locked,
}

impl Journal {
    fn open(vault: &Path) -> Result<Self> {
        Ok(Self {
            state: state::lock(vault)?,
        })
    }

    /// The SHA-256 of `request` and the record of `operation`, refusing `OPERATION_REUSED` when
    /// the record holds another request.
    fn read(&self, operation: &str, request: &impl Serialize) -> Result<(String, Option<Record>)> {
        let (fingerprint, record) = state::recorded(&self.state.dir, operation, request)?;
        let record = record
            .map(|record| {
                serde_json::from_value(record)
                    .with_context(|| format!("parsing the journal record of {operation}"))
            })
            .transpose()?;
        Ok((fingerprint, record))
    }

    /// Every pending record, in operation ID order.
    fn pending(&self) -> Result<Vec<Record>> {
        let dir = &self.state.dir;
        let mut pending = Vec::new();
        for operation in state::pending(dir)? {
            if let Some(record) = state::read_record(&state::pending_path(dir, &operation))? {
                pending.push(record);
            }
        }
        Ok(pending)
    }

    /// Fails, before any vault write, when the record is too large to read back.
    fn write_pending(&self, record: &Record) -> Result<()> {
        state::write_pending(&self.state.dir, &record.output.operation_id, record)
    }

    fn finish(&self, record: &Record) -> Result<()> {
        state::finish(&self.state.dir, &record.output.operation_id, record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::types::{Manifest, Predecessor, Proposal, ProposalStatus, State, VaultConfig};
    use crate::collect::TempDir;

    #[test]
    fn a_proposal_edit_never_allocates_an_id_it_dropped_or_a_proposal_folder_name() {
        let temp = TempDir::new("edit-taken").unwrap();
        std::fs::create_dir_all(temp.path().join("proposals/Folder01")).unwrap();
        let model = Model {
            dir: temp.path().to_path_buf(),
            manifest: Manifest {
                schema_version: SCHEMA_VERSION,
                id: "ModelId1".to_owned(),
                name: "Shop".to_owned(),
                current: Some(State {
                    id: "State001".to_owned(),
                    name: "Current".to_owned(),
                }),
                retired_ids: vec!["Retired1".to_owned()],
            },
            config: VaultConfig::default(),
            modules: Vec::new(),
            collections: Vec::new(),
            walkthroughs: Vec::new(),
            pictures: Vec::new(),
            views: Vec::new(),
            revision: revision::of(iter::empty()),
            // After the proposal removed Billing, which its predecessor holds.
            proposal: Some(Proposal {
                schema_version: SCHEMA_VERSION,
                id: "Propose1".to_owned(),
                name: "Next".to_owned(),
                description: None,
                status: ProposalStatus::Draft,
                predecessor: Predecessor {
                    state: Some("State001".to_owned()),
                    revision: String::new(),
                },
                created: 0,
                adopted_as: None,
                dropped: vec!["Billing1".to_owned()],
                base: false,
            }),
        };
        let batch = Batch::new(&model).unwrap();
        for id in ["billing1", "retired1", "folder01", "propose1", "modelid1"] {
            assert!(batch.taken.contains(id), "{id} is free: {:?}", batch.taken);
        }
    }
}
