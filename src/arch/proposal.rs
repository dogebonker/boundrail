//! Proposals: drafts of a future architecture, each drawn against the current state it would
//! replace (its predecessor). A proposal lives in `proposals/<id>/` beside the current state, in
//! the same file formats, with its own revision. Adopting one makes it the current state;
//! shelving one sets it aside. Adopted and shelved proposals read only. A planning model has no
//! current state, so its work happens in proposals until the first adoption.
//!
//! Drawing, adopting and shelving are journaled writes like an edit: one operation, written
//! through the edit journal under the vault lock, replayed by its operation ID.

use std::collections::BTreeSet;
use std::iter;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use super::edit::{self, Allocation, Plan, Receipt, Subjects};
use super::request::{check_doing, check_operation, check_writer};
use super::revision::Revision;
use super::types::{
    Manifest, Module, Predecessor, Proposal, ProposalStatus, SCHEMA_VERSION, State,
};
use super::vault::{self, Model, Scope};
use super::{Code, Refusal, checkpoint, claims, ids, revision};

/// The name `--from` and `--against` give the current state.
pub const CURRENT: &str = "current";

/// What every proposal write names besides its own arguments.
pub struct Writing<'a> {
    /// One line saying why, 1 to 140 characters.
    pub doing: &'a str,
    /// Who writes; defaults to the Git author name.
    pub writer: Option<&'a str>,
    /// Repeating an operation ID with the same arguments returns the same receipt.
    pub operation_id: &'a str,
    /// The writer's claim covering what the write touches.
    pub claim: Option<&'a str>,
}

/// Draws a draft named `name` from the current state, or from the proposal `from` names, in the
/// vault folder `vault`. Drawn from the current state, its predecessor is the current state and
/// revision; in a planning model the predecessor state is `None` and the draft starts empty. A
/// copy of a proposal inherits that proposal's predecessor and dropped IDs, since it holds what
/// that proposal drew; a copy of an adopted proposal needs it to still be the current state,
/// unchanged. It takes no revision: the draft records the predecessor it copied, which adoption
/// checks again.
pub fn propose(
    vault: &Path,
    writing: &Writing<'_>,
    name: &str,
    from: Option<&str>,
    description: Option<&str>,
) -> Result<(Receipt, Option<Scope>)> {
    let name = vault::valid_name(name)?.to_owned();
    let (writer, operation) = start(vault, writing)?;
    let from = from.filter(|from| *from != CURRENT);
    let request = json!({"kind": "propose", "writer": writer, "name": name, "from": from, "description": description});
    edit::journaled(vault, &operation, request, |_, _, fingerprint| {
        let current = vault::load_dir(vault.to_path_buf())?;
        let source = from
            .map(|from| vault::load_proposal(vault.to_path_buf(), from))
            .transpose()?;
        let (predecessor, dropped) = inherited(&current, source.as_ref())?;
        let taken = taken(vault, &[&current])?;
        let proposal = Proposal {
            schema_version: SCHEMA_VERSION,
            id: ids::allocate(|candidate| taken.contains(candidate))?,
            name: name.clone(),
            description: description.map(str::to_owned),
            status: ProposalStatus::Draft,
            predecessor,
            created: claims::now(),
            adopted_as: None,
            dropped,
        };
        let source = source.as_ref().unwrap_or(&current);
        let plan = drawing(source, proposal, writer.clone(), operation.clone(), writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// The predecessor of a draft copied from `source`, or drawn from `current` without one, and the
/// IDs the draft has dropped from it.
fn inherited(current: &Model, source: Option<&Model>) -> Result<(Predecessor, Vec<String>)> {
    let state = current.manifest.current.as_ref().map(|c| c.id.clone());
    let of_current = Predecessor {
        state: state.clone(),
        revision: current.predecessor_digest()?,
    };
    let Some((source, drawn)) = source.and_then(|s| Some((s, s.proposal.as_ref()?))) else {
        return Ok((of_current, Vec::new()));
    };
    if drawn.status != ProposalStatus::Adopted {
        return Ok((drawn.predecessor.clone(), drawn.dropped.clone()));
    }
    // An adopted proposal stands for the current state only while nothing has changed it since.
    if drawn.adopted_as == state
        && source.modules == current.modules
        && source.collections == current.collections
    {
        return Ok((of_current, Vec::new()));
    }
    Err(Refusal::new(
        Code::PredecessorMoved,
        format!(
            "proposal {} was adopted, and the current state has moved on from it; draw from the current state with `boundrail model propose --from current`",
            drawn.id
        ),
    )
    .with_details(json!({ "proposal": drawn.id, "adopted_as": drawn.adopted_as, "current": state }))
    .into())
}

/// The IDs a new proposal or state must not take, in ASCII lowercase: those `models` hold or
/// reserve and every proposal folder's. IDs are random, so this guards the names that matter
/// most; adopting a state that clashes with the current one refuses `MODEL_INVALID`.
pub(super) fn taken(vault: &Path, models: &[&Model]) -> Result<BTreeSet<String>> {
    let held = models.iter().flat_map(|m| m.ids()).map(str::to_owned);
    Ok(held
        .chain(vault::proposals(vault)?)
        .map(|id| id.to_ascii_lowercase())
        .collect())
}

/// The plan that writes `proposal` holding `source`'s modules and collections.
fn drawing(
    source: &Model,
    proposal: Proposal,
    writer: String,
    operation_id: String,
    writing: &Writing<'_>,
) -> Result<Plan> {
    let problems = vault::problems(
        &source.manifest,
        &source.config,
        Some(&proposal),
        &source.modules,
        &source.collections,
        &[],
        &[],
    );
    if !problems.is_empty() {
        return Err(vault::invalid(&source.dir, problems));
    }
    let modules: Vec<&Module> = source.modules.iter().collect();
    let collections: Vec<_> = source.collections.iter().collect();
    let mut writes = vec![vault::proposal_write(&proposal)?];
    let copies = vault::plan_writes(None, &modules, &collections, &[], &[], &[])?;
    writes.extend(vault::in_proposal(&proposal.id, copies));
    let id = source.qualify(&proposal.id);
    Ok(Plan {
        kind: "propose",
        scope: Scope::Proposal(proposal.id),
        operation_id,
        writer,
        doing: writing.doing.to_owned(),
        before: revision::of(iter::empty()),
        writes,
        subjects: Subjects {
            added: vec![id.clone()],
            ..Subjects::default()
        },
        allocated: vec![Allocation {
            name: proposal.name,
            id,
        }],
    })
}

/// Makes the draft `subject` names the current state: its files replace the current state's,
/// the new state gets a fresh ID and the proposal's name, the IDs it drops are retired, and the
/// proposal is marked adopted, all in one journaled write. `expected` is the current revision the
/// caller read, and `compared` the proposal revision. Refuses `STALE_REVISION` when either is at
/// another revision, and `PREDECESSOR_MOVED` when the current state moved since the proposal was
/// drawn. Claims on what the adoption changes apply as to an edit.
pub fn adopt(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    expected: &str,
    compared: &str,
) -> Result<(Receipt, Option<Scope>)> {
    let (writer, operation) = start(vault, writing)?;
    let request = json!({"kind": "adopt", "writer": writer, "proposal": subject, "expected_revision": expected, "expected_proposal_revision": compared, "claim": writing.claim});
    edit::journaled(vault, &operation, request, |_, dir, fingerprint| {
        let current = vault::load_dir(vault.to_path_buf())?;
        let (drawn, proposal) = open(vault, subject)?;
        refuse_moved(&current, &proposal, expected)?;
        refuse_compared(&drawn, compared)?;
        let taken = taken(vault, &[&current, &drawn])?;
        let state = ids::allocate(|candidate| taken.contains(candidate))?;
        // Walkthroughs and pictures belong to the current state and stay as they are.
        let diff = edit::diff(
            &current,
            &drawn.modules,
            &drawn.collections,
            &current.walkthroughs,
            &current.pictures,
            &[],
        );
        let id = current.qualify(&proposal.id);
        let mut touched = diff.affected.clone();
        touched.extend([current.manifest.id.clone(), id.clone()]);
        claims::check_edit(dir, &current, &writer, writing.claim, &touched, &diff.moves)?;
        let adoption = Adoption {
            current: &current,
            drawn: &drawn,
            proposal,
            state,
        };
        let plan = adoption.plan(diff, operation.clone(), writer.clone(), writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// A proposal about to become the current state `state`.
struct Adoption<'a> {
    current: &'a Model,
    drawn: &'a Model,
    proposal: Proposal,
    state: String,
}

impl Adoption<'_> {
    /// The writes that replace the current state's files with the proposal's, by `diff`, and
    /// mark the proposal adopted.
    fn plan(
        mut self,
        diff: edit::Diff<'_>,
        operation_id: String,
        writer: String,
        writing: &Writing<'_>,
    ) -> Result<Plan> {
        let (current, drawn) = (self.current, self.drawn);
        let manifest = adopted_manifest(
            current,
            drawn,
            &diff.retired,
            &self.proposal.name,
            &self.state,
        )?;
        let mut writes = vault::plan_writes(
            Some(&manifest),
            &diff.write,
            &diff.write_collections,
            &diff.write_walkthroughs,
            &diff.write_pictures,
            &diff.remove,
        )?;
        self.proposal.status = ProposalStatus::Adopted;
        self.proposal.adopted_as = Some(self.state.clone());
        writes.push(vault::proposal_write(&self.proposal)?);
        let mut subjects = diff.subjects;
        subjects.changed.push(current.qualify(&self.proposal.id));
        subjects.changed.sort();
        let before = current.revision.files.iter().chain(&drawn.revision.files);
        Ok(Plan {
            kind: "adopt",
            scope: Scope::Adoption(self.proposal.id),
            operation_id,
            writer,
            doing: writing.doing.to_owned(),
            before: revision::of_digests(
                before.map(|f| (f.path.clone(), f.sha256.clone())).collect(),
            ),
            writes,
            subjects,
            allocated: vec![Allocation {
                name: self.proposal.name,
                id: current.qualify(&self.state),
            }],
        })
    }
}

/// The manifest after adopting `drawn` as the state `state` named `name`, refusing
/// `MODEL_INVALID` when the adopted state would not load, the current state's walkthroughs and
/// pictures included: a beat or picture reading a module or flow the adopted state drops must be
/// edited first. The
/// state it replaces leaves the model with the IDs it drops, `retired`, and all stay reserved, as
/// do the IDs the proposal dropped.
fn adopted_manifest(
    current: &Model,
    drawn: &Model,
    retired: &[String],
    name: &str,
    state: &str,
) -> Result<Manifest> {
    let mut manifest = current.manifest.clone();
    let old = manifest.current.replace(State {
        id: state.to_owned(),
        name: name.to_owned(),
    });
    let dropped = drawn
        .proposal
        .iter()
        .flat_map(|p| p.dropped.iter().cloned());
    for id in retired
        .iter()
        .cloned()
        .chain(old.map(|s| s.id))
        .chain(dropped)
    {
        if !manifest.retired_ids.contains(&id) {
            manifest.retired_ids.push(id);
        }
    }
    let problems = vault::problems(
        &manifest,
        &current.config,
        None,
        &drawn.modules,
        &drawn.collections,
        &current.walkthroughs,
        &current.pictures,
    );
    if !problems.is_empty() {
        return Err(vault::invalid(&current.dir, problems));
    }
    Ok(manifest)
}

/// Sets the draft `subject` names aside: it stays, reading only. `expected` is the proposal
/// revision the caller read; another revision refuses `STALE_REVISION`.
pub fn shelve(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    expected: &str,
) -> Result<(Receipt, Option<Scope>)> {
    let (writer, operation) = start(vault, writing)?;
    let request = json!({"kind": "shelve", "writer": writer, "proposal": subject, "expected_revision": expected, "claim": writing.claim});
    edit::journaled(vault, &operation, request, |_, dir, fingerprint| {
        let (drawn, mut proposal) = open(vault, subject)?;
        refuse_compared(&drawn, expected)?;
        let id = drawn.qualify(&proposal.id);
        let touched = BTreeSet::from([id.clone()]);
        claims::check_edit(dir, &drawn, &writer, writing.claim, &touched, &[])?;
        proposal.status = ProposalStatus::Shelved;
        let plan = Plan {
            kind: "shelve",
            scope: Scope::Proposal(proposal.id.clone()),
            operation_id: operation.clone(),
            writer: writer.clone(),
            doing: writing.doing.to_owned(),
            before: drawn.revision.clone(),
            writes: vec![vault::proposal_write(&proposal)?],
            subjects: Subjects {
                changed: vec![id],
                ..Subjects::default()
            },
            allocated: Vec::new(),
        };
        Ok(plan.record(fingerprint))
    })
}

/// One proposal folder as `model proposals` lists it: its `proposal.json`, or why it does not
/// read.
pub struct Listed {
    /// The folder's local ID.
    pub id: String,
    pub proposal: Result<Proposal, String>,
}

/// Every proposal folder of the vault folder `vault`, in ID order, reading only each
/// `proposal.json`, so one broken proposal never hides the others.
pub fn list(vault: &Path) -> Result<Vec<Listed>> {
    Ok(vault::proposals(vault)?
        .into_iter()
        .map(|id| Listed {
            proposal: vault::read_proposal(vault, &id).map_err(|e| format!("{e:#}")),
            id,
        })
        .collect())
}

/// The current state's revision after the adoption `receipt` records: its revision after the
/// write, which covers the proposal's files too, without them.
pub fn current_revision(receipt: &Receipt) -> Revision {
    let files = receipt.after.files.iter();
    revision::of_digests(
        files
            .filter(|f| !vault::is_proposal_path(&f.path))
            .map(|f| (f.path.clone(), f.sha256.clone()))
            .collect(),
    )
}

/// `PROPOSAL_CLOSED` for an adopted or shelved proposal, which reads only.
pub(super) fn refuse_closed(proposal: &Proposal) -> Result<()> {
    let status = match proposal.status {
        ProposalStatus::Draft => return Ok(()),
        ProposalStatus::Adopted => "adopted",
        ProposalStatus::Shelved => "shelved",
    };
    let id = &proposal.id;
    Err(Refusal::new(
        Code::ProposalClosed,
        format!(
            "proposal {id} is {status} and reads only; draw a new one with `boundrail model propose --from current`"
        ),
    )
    .with_details(json!({ "proposal": id, "status": proposal.status }))
    .into())
}

/// `STALE_REVISION` when the caller read another revision of the proposal than `drawn`.
fn refuse_compared(drawn: &Model, expected: &str) -> Result<()> {
    let now = &drawn.revision.digest;
    if expected == now {
        return Ok(());
    }
    Err(Refusal::new(
        Code::StaleRevision,
        "the proposal is not at the expected revision; read it again first",
    )
    .with_details(json!({ "current": now }))
    .into())
}

/// The draft `subject` names, loaded, with its `proposal.json`; a closed one refuses.
fn open(vault: &Path, subject: &str) -> Result<(Model, Proposal)> {
    let drawn = vault::load_proposal(vault.to_path_buf(), subject)?;
    let proposal = drawn
        .proposal
        .clone()
        .context("a loaded proposal holds its proposal.json")?;
    refuse_closed(&proposal)?;
    Ok((drawn, proposal))
}

/// Refuses an adoption the caller planned against another current revision, or of a proposal
/// drawn against a current state that has moved since.
fn refuse_moved(current: &Model, proposal: &Proposal, expected: &str) -> Result<()> {
    let now = &current.revision.digest;
    if expected != now {
        return Err(Refusal::new(
            Code::StaleRevision,
            "the current state changed since the expected revision; read it again before adopting",
        )
        .with_details(json!({ "current": now }))
        .into());
    }
    // Walkthroughs and pictures change on their own beside a proposal, so its predecessor leaves
    // them out, and the retired IDs their removal grows. Drafts drawn before retired IDs were left
    // out record model.json as it was on disk.
    let drawn_from = current.predecessor_digest()?;
    let recorded = &proposal.predecessor.revision;
    if *recorded != drawn_from && *recorded != current.earlier_predecessor_digest() {
        return Err(Refusal::new(
            Code::PredecessorMoved,
            format!(
                "the current state moved since proposal {} was drawn; reconciling a proposal with a moved current state arrives in a later release, so draw a new proposal from the current state",
                proposal.id
            ),
        )
        .with_details(json!({
            "proposal": proposal.id,
            "predecessor": proposal.predecessor,
            "current": drawn_from,
        }))
        .into());
    }
    Ok(())
}

/// The writer, `writing.writer` or the Git author name, and the operation ID, after checking
/// them and `--doing`.
fn start(vault: &Path, writing: &Writing<'_>) -> Result<(String, String)> {
    check_doing(writing.doing)?;
    check_operation(writing.operation_id)?;
    let writer = match writing.writer {
        Some(writer) => writer.to_owned(),
        None => checkpoint::author(&checkpoint::checkout(vault)?)?,
    };
    check_writer(&writer)?;
    Ok((writer, writing.operation_id.to_owned()))
}
