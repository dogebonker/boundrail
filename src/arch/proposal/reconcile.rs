//! `model reconcile`: brings a draft up to date with a predecessor that moved since it was drawn
//! or last reconciled, the current state or a child's parent draft, by a three-way merge of its
//! base, the draft and the predecessor (`super::merge`). Conflicts settle by the draft's side,
//! the predecessor's or a replacement; a merge that leaves a part without the container the
//! predecessor removed, or keeps a part the predecessor removed, needs a confirmation. A family
//! reconcile goes on through the draft's descendants, parents first.

use std::collections::{BTreeMap, BTreeSet};
use std::iter;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::family::{self, Upstream, upstream};
use super::{Writing, open, predecessor_of, proposal_of, refuse_compared, refuse_stale, start};
use crate::arch::edit::{self, Plan, Receipt};
use crate::arch::merge::{self, Choice, Conflict, Item, Merge, Settlement, Side};
use crate::arch::request::{OPERATION_MAX, invalid};
use crate::arch::types::{Collection, Module, Predecessor, Proposal, Relationship};
use crate::arch::vault::{self, Model, Scope};
use crate::arch::{Code, Refusal, claims, ids, revision};

/// What a reconcile names besides the draft and the writing.
#[derive(Debug, Clone, Copy)]
pub struct Reconciling<'a> {
    /// The current state's revision the caller read.
    pub expected: &'a str,
    /// The draft's revision the caller read.
    pub compared: &'a str,
    /// How the draft's conflicting subjects settle.
    pub settle: &'a [Settle],
    /// The confirmations given, `INHERITANCE_DROPPED`, `PART_RECREATED` and
    /// `PARENT_CHANGES_DROPPED`.
    pub confirm: &'a [Confirm],
}

/// One `--settle`: a conflicting subject and what it takes.
#[derive(Debug, Clone)]
pub struct Settle {
    /// The subject's qualified or local ID.
    pub subject: String,
    pub take: Take,
}

/// What a settled subject takes.
#[derive(Debug, Clone)]
pub enum Take {
    /// The draft's side.
    Mine,
    /// The predecessor's side.
    Theirs,
    /// This replacement: a module file without relationships, a relationship, or a collection
    /// file, holding the subject's ID.
    Edited(Value),
}

/// One `--confirm`: a confirmation code, for every subject or the one named.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub code: Code,
    /// The qualified or local ID of the one subject it confirms.
    pub subject: Option<String>,
}

/// One draft of a family reconcile.
#[derive(Debug, Serialize)]
pub struct Member {
    /// The local ID.
    pub proposal: String,
    pub operation_id: String,
    /// Whether it was reconciled; a descendant already up to date is left alone.
    pub reconciled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// The scope of an interrupted write this call finished.
    #[serde(skip)]
    pub finished: Option<Scope>,
}

/// Merges into the draft `subject` names what its predecessor changed since the draft's base,
/// and makes the predecessor as it is now its new base, in one journaled write. The predecessor
/// is the parent draft for a child of a draft, else the current state; a child whose parent was
/// adopted or shelved merges onto the current state and leaves the family. Refuses
/// `STALE_REVISION` when either revision `asked` names is another, `RECONCILE_NO_BASE` for a draft
/// drawn before bases existed, `REQUEST_INVALID` for a settlement of a subject not in conflict or
/// a replacement that does not parse as the subject, `RECONCILE_CONFLICT` for the conflicts no
/// settlement decides, `PARENT_CHANGES_DROPPED`, `INHERITANCE_DROPPED` and `PART_RECREATED`
/// without their confirmation, and `MODEL_INVALID` when the merged draft does not load. Claims
/// apply to what the merge changes in the draft, and to the draft.
pub fn reconcile(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    asked: &Reconciling<'_>,
) -> Result<(Receipt, Option<Scope>)> {
    reconciled(vault, writing, subject, asked, false)
}

/// What [`reconciled`] gives a descendant already up to date: nothing to write.
#[derive(Debug)]
struct UpToDate;

impl std::fmt::Display for UpToDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the draft is up to date")
    }
}

impl std::error::Error for UpToDate {}

/// [`reconcile`], or for a `descendant` of a family reconcile, which names no revision of it,
/// refusing [`UpToDate`] when it is up to date.
fn reconciled(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    asked: &Reconciling<'_>,
    descendant: bool,
) -> Result<(Receipt, Option<Scope>)> {
    let (writer, operation) = start(vault, writing)?;
    let compared = (!descendant).then_some(asked.compared);
    let mut request = json!({"kind": "reconcile", "writer": writer, "proposal": subject, "expected_revision": asked.expected, "expected_proposal_revision": compared, "claim": writing.claim});
    if !asked.settle.is_empty() {
        request["settle"] = asked.settle.iter().map(settle_json).collect();
    }
    if !asked.confirm.is_empty() {
        let confirm = asked.confirm.iter();
        request["confirm"] = confirm.map(|c| json!([c.code, c.subject])).collect();
    }
    edit::journaled(vault, &operation, request, |_, dir, fingerprint| {
        let current = vault::load_dir(vault.to_path_buf())?;
        let (drawn, proposal) = open(vault, subject)?;
        let upstream = upstream(vault, &current, &proposal)?;
        if descendant && family::fresh(&current, &proposal, &upstream)? {
            return Err(UpToDate.into());
        }
        refuse_stale(&current, asked.expected)?;
        if !descendant {
            refuse_compared(&drawn, asked.compared)?;
        }
        let base = vault::load_base(&drawn)?.ok_or_else(|| no_base(&proposal.id))?;
        let (theirs, predecessor, parent) = match &upstream {
            Upstream::Draft(parent) => {
                let predecessor = family::predecessor_of_draft(parent)?;
                (parent, predecessor, proposal.parent.clone())
            }
            _ => (&current, predecessor_of(&current)?, None),
        };
        let model = &current.manifest.id;
        let sides = [
            (&base.0[..], &base.1[..]),
            (&drawn.modules[..], &drawn.collections[..]),
            (&theirs.modules[..], &theirs.collections[..]),
        ];
        let settlement = settlement(model, asked.settle, sides)?;
        let [b, o, t] = sides;
        let retired = &current.manifest.retired_ids;
        let merge = merge::merge(model, b, o, t, retired, &settlement);
        refuse_unsettled(&proposal.id, &settlement, &merge)?;
        if let Upstream::Orphaned = upstream {
            refuse_parent_dropped(model, &proposal.id, &merge.taken, asked.confirm)?;
        }
        let Merge {
            mut modules,
            collections,
            ..
        } = merge;
        confirmed(model, &proposal.id, (b.0, t.0), &mut modules, asked.confirm)?;
        let merged = Merged {
            current: &current,
            drawn: &drawn,
            base: (&base.0, &base.1),
            theirs: t,
            modules: &modules,
            collections: &collections,
        };
        let walkthroughs = &drawn.walkthroughs;
        let diff = edit::diff(&drawn, &modules, &collections, walkthroughs, &[], &[], &[]);
        let mut touched = diff.affected.clone();
        touched.insert(drawn.qualify(&proposal.id));
        claims::check_edit(dir, &drawn, &writer, writing.claim, &touched, &diff.moves)?;
        let caught = (predecessor, parent);
        let named = (writer.clone(), operation.clone());
        let plan = merged.plan(proposal, caught, diff, named, writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// Reconciles the draft `subject` names as [`reconcile`] does, then each draft descending from
/// it through drafts, parents first, against its parent as just reconciled: a descendant already
/// up to date is left alone. Each is its own journaled write; a descendant's operation ID is
/// `<operation ID>.<its local ID>`, its revision the one it holds when its turn comes, and
/// settlements and confirmations of every subject apply to the named draft only, those naming one
/// subject to every draft. Refuses `REQUEST_INVALID`, before any write, when the draft has
/// descendants and their operation IDs would pass [`OPERATION_MAX`]. A refusal stops the walk,
/// its `details.family` listing the drafts done before it, whose writes stay.
pub fn reconcile_family(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    asked: &Reconciling<'_>,
) -> Result<Vec<Member>> {
    let root = proposal_of(&vault::load_proposal(vault.to_path_buf(), subject)?)?.id;
    let family = family::family(vault, &root)?;
    let longest = writing.operation_id.len() + 1 + ids::LEN;
    if family.len() > 1 && longest > OPERATION_MAX {
        let most = OPERATION_MAX - 1 - ids::LEN;
        return Err(invalid(format!(
            "--family gives each descendant the operation ID `<operation_id>.<its local ID>`, so the operation ID needs at most {most} characters"
        )));
    }
    let (receipt, finished) = reconcile(vault, writing, &root, asked)?;
    let mut members = vec![Member {
        proposal: root,
        operation_id: writing.operation_id.to_owned(),
        reconciled: true,
        receipt: Some(receipt),
        finished,
    }];
    let confirm: Vec<Confirm> = (asked.confirm.iter())
        .filter(|c| c.subject.is_some())
        .cloned()
        .collect();
    let asked = Reconciling {
        settle: &[],
        confirm: &confirm,
        ..*asked
    };
    for id in family.into_iter().skip(1) {
        let operation_id = format!("{}.{id}", writing.operation_id);
        let writing = Writing {
            operation_id: &operation_id,
            ..*writing
        };
        let done = match reconciled(vault, &writing, &id, &asked, true) {
            Ok(done) => Some(done),
            Err(error) if error.is::<UpToDate>() => None,
            Err(error) => return Err(in_family(error, &members)),
        };
        let (receipt, finished) = done.unzip();
        members.push(Member {
            proposal: id,
            operation_id,
            reconciled: receipt.is_some(),
            receipt,
            finished: finished.flatten(),
        });
    }
    Ok(members)
}

/// `error` with the drafts a family reconcile finished before it in `details.family`, when it is
/// a refusal.
fn in_family(error: anyhow::Error, members: &[Member]) -> anyhow::Error {
    let mut refusal = match error.downcast::<Refusal>() {
        Ok(refusal) => refusal,
        Err(error) => return error,
    };
    let done: Vec<Value> = (members.iter())
        .map(|m| json!({"proposal": m.proposal, "reconciled": m.reconciled}))
        .collect();
    let details = refusal.details.get_or_insert_with(|| json!({}));
    details["family"] = json!(done);
    refusal.into()
}

/// A settlement as the request records it, so retrying with other choices is another request.
fn settle_json(settle: &Settle) -> Value {
    let (take, edited) = match &settle.take {
        Take::Mine => ("mine", None),
        Take::Theirs => ("theirs", None),
        Take::Edited(value) => ("edited", Some(value)),
    };
    json!({"subject": settle.subject, "take": take, "edited": edited})
}

/// The choices `settle` makes, by qualified subject, in the model `model`, each replacement
/// parsed as what `sides` (base, draft, predecessor) hold under its ID. Refuses
/// `REQUEST_INVALID` for a subject settled twice and a replacement that is not the subject.
fn settlement(model: &str, settle: &[Settle], sides: [Side<'_>; 3]) -> Result<Settlement> {
    let mut settlement = Settlement::new();
    for one in settle {
        let local = ids::local(model, &one.subject);
        let subject = ids::qualify(model, local);
        let choice = match &one.take {
            Take::Mine => Choice::Mine,
            Take::Theirs => Choice::Theirs,
            Take::Edited(value) => Choice::Edited(Box::new(edited(&subject, local, value, sides)?)),
        };
        if settlement.insert(subject.clone(), choice).is_some() {
            return Err(invalid(format!("--settle names {subject} twice")));
        }
    }
    Ok(settlement)
}

/// The replacement `value` for the subject `local`, of the kind `sides` hold it as.
fn edited(subject: &str, local: &str, value: &Value, sides: [Side<'_>; 3]) -> Result<Item> {
    let modules = || sides.iter().flat_map(|(m, _)| m.iter());
    if modules().any(|m| m.id == local) {
        let module = replacement::<Module>(subject, local, value, |m| &m.id)?;
        if !module.outgoing.is_empty() {
            return Err(invalid(format!(
                "the replacement for {subject} holds relationships; each relationship settles as its own subject, so leave `outgoing` empty"
            )));
        }
        return Ok(Item::Module(module));
    }
    if modules().flat_map(|m| &m.outgoing).any(|r| r.id == local) {
        let parsed = replacement::<Relationship>(subject, local, value, |r| &r.id);
        return parsed.map(Item::Relationship);
    }
    if sides
        .iter()
        .flat_map(|(_, c)| c.iter())
        .any(|c| c.id == local)
    {
        let parsed = replacement::<Collection>(subject, local, value, |c| &c.id);
        return parsed.map(Item::Collection);
    }
    Err(invalid(format!(
        "--settle {subject}=edited names no module, relationship or collection of the draft or its predecessor"
    )))
}

/// `value` parsed as a `T` whose ID, by `id`, is `local`.
fn replacement<T: DeserializeOwned>(
    subject: &str,
    local: &str,
    value: &Value,
    id: fn(&T) -> &String,
) -> Result<T> {
    let parsed: T = serde_json::from_value(value.clone()).map_err(|e| {
        let message = vault::parse_message(&e);
        invalid(format!(
            "the replacement for {subject} does not parse: {message}"
        ))
    })?;
    if id(&parsed) != local {
        return Err(invalid(format!(
            "the replacement for {subject} holds another ID"
        )));
    }
    Ok(parsed)
}

/// `REQUEST_INVALID` for a settlement of a subject `merge` did not have in conflict, then
/// `RECONCILE_CONFLICT` for the conflicts it has left.
fn refuse_unsettled(id: &str, settlement: &Settlement, merge: &Merge) -> Result<()> {
    let unused: Vec<&String> = (settlement.keys())
        .filter(|subject| !merge.settled.contains(*subject))
        .collect();
    if !unused.is_empty() {
        return Err(Refusal::new(
            Code::RequestInvalid,
            format!(
                "--settle names {} subject(s) that are not in conflict; settle only the subjects RECONCILE_CONFLICT lists",
                unused.len()
            ),
        )
        .with_details(json!({ "proposal": id, "subjects": unused }))
        .into());
    }
    if merge.conflicts.is_empty() {
        return Ok(());
    }
    Err(conflicted(id, &merge.conflicts))
}

/// A merged part whose container the predecessor removed: its index in the merged modules, its
/// qualified ID, the container's, and the nearest container above it the merge keeps.
struct Lift {
    index: usize,
    module: String,
    parent: String,
    to: Option<String>,
}

/// Refuses, without the confirmation `confirm` gives, a merge whose `merged` modules keep a part
/// whose container the predecessor removed since `base` (`INHERITANCE_DROPPED`) or a module the
/// predecessor removed (`PART_RECREATED`); `theirs` is what the predecessor holds now. Confirmed,
/// each such part moves to the nearest container above its old one that the merge keeps, else
/// to the root, and a kept module stays as merged.
fn confirmed(
    model: &str,
    id: &str,
    (base, theirs): (&[Module], &[Module]),
    merged: &mut [Module],
    confirm: &[Confirm],
) -> Result<()> {
    let local = |subject: &str| ids::local(model, subject).to_owned();
    let up: BTreeMap<&str, Option<String>> = (base.iter())
        .map(|m| (m.id.as_str(), m.parent.as_deref().map(local)))
        .collect();
    let held: BTreeSet<&str> = theirs.iter().map(|m| m.id.as_str()).collect();
    let removed = |id: &str| up.contains_key(id) && !held.contains(id);
    let kept: BTreeSet<String> = merged.iter().map(|m| m.id.clone()).collect();
    let mut lifts = Vec::new();
    for (index, module) in merged.iter().enumerate() {
        let Some(parent) = module.parent.as_deref().map(local) else {
            continue;
        };
        if kept.contains(&parent) || !removed(&parent) {
            continue;
        }
        let above = |id: &String| up.get(id.as_str()).cloned().flatten();
        let to = iter::successors(above(&parent), above)
            .take(base.len())
            .find(|id| kept.contains(id));
        lifts.push(Lift {
            index,
            module: ids::qualify(model, &module.id),
            parent: ids::qualify(model, &parent),
            to: to.map(|to| ids::qualify(model, &to)),
        });
    }
    let unconfirmed = |code: Code, subject: &str| unconfirmed(model, confirm, code, subject);
    let dropped: Vec<&Lift> = (lifts.iter())
        .filter(|lift| unconfirmed(Code::InheritanceDropped, &lift.module))
        .collect();
    if !dropped.is_empty() {
        let modules: Vec<Value> = (dropped.iter())
            .map(|l| json!({"module": l.module, "parent": l.parent, "lifted_to": l.to}))
            .collect();
        return Err(Refusal::new(
            Code::InheritanceDropped,
            format!(
                "merging proposal {id} keeps {} part(s) whose container its predecessor removed; confirm with `--confirm INHERITANCE_DROPPED` to move each to the nearest container left, or edit the proposal",
                modules.len()
            ),
        )
        .with_details(json!({ "proposal": id, "modules": modules }))
        .into());
    }
    let recreated: Vec<String> = (merged.iter())
        .filter(|m| removed(&m.id))
        .map(|m| ids::qualify(model, &m.id))
        .filter(|subject| unconfirmed(Code::PartRecreated, subject))
        .collect();
    if !recreated.is_empty() {
        return Err(Refusal::new(
            Code::PartRecreated,
            format!(
                "merging proposal {id} keeps {} module(s) its predecessor removed; confirm with `--confirm PART_RECREATED` to keep each, or settle it as theirs",
                recreated.len()
            ),
        )
        .with_details(json!({ "proposal": id, "modules": recreated }))
        .into());
    }
    for lift in lifts {
        merged[lift.index].parent = lift.to;
    }
    Ok(())
}

/// Whether `confirm` gives no `code` for `subject`, a qualified ID of the model `model`, nor for
/// every subject.
fn unconfirmed(model: &str, confirm: &[Confirm], code: Code, subject: &str) -> bool {
    !confirm.iter().any(|c| {
        c.code == code
            && (c.subject.as_deref())
                .is_none_or(|named| ids::qualify(model, ids::local(model, named)) == subject)
    })
}

/// Refuses `PARENT_CHANGES_DROPPED`, without the confirmation `confirm` gives, for merging the
/// draft `id`, whose parent was shelved or is gone, onto the current state: the subjects `taken`
/// it held as its parent left them take the current state's version, so what the parent changed
/// there is lost.
fn refuse_parent_dropped(
    model: &str,
    id: &str,
    taken: &BTreeSet<String>,
    confirm: &[Confirm],
) -> Result<()> {
    let dropped: Vec<&String> = (taken.iter())
        .filter(|subject| unconfirmed(model, confirm, Code::ParentChangesDropped, subject))
        .collect();
    if dropped.is_empty() {
        return Ok(());
    }
    Err(Refusal::new(
        Code::ParentChangesDropped,
        format!(
            "proposal {id} merges onto the current state without its shelved or missing parent, so {} subject(s) it holds as the parent left them take the current state's version; confirm with `--confirm PARENT_CHANGES_DROPPED`, or edit the proposal",
            dropped.len()
        ),
    )
    .with_details(json!({ "proposal": id, "subjects": dropped }))
    .into())
}

/// A draft merged with its predecessor, ready to be written.
struct Merged<'a> {
    current: &'a Model,
    drawn: &'a Model,
    /// The draft's base before reconciling.
    base: (&'a [Module], &'a [Collection]),
    /// The predecessor as it is now, the new base.
    theirs: Side<'a>,
    modules: &'a [Module],
    collections: &'a [Collection],
}

impl Merged<'_> {
    /// The writes of the merged draft by `diff`, its new base (the predecessor now) and its
    /// `proposal.json` caught up with the predecessor and parent `caught`.
    fn plan(
        &self,
        proposal: Proposal,
        caught: (Predecessor, Option<String>),
        diff: edit::Diff<'_>,
        (writer, operation_id): (String, String),
        writing: &Writing<'_>,
    ) -> Result<Plan> {
        let current = self.current;
        let proposal = caught_up(proposal, caught, current, &diff.retired);
        self.refuse_invalid(&proposal)?;
        let own = vault::plan_writes(
            None,
            &diff.write,
            &diff.write_collections,
            &[],
            &[],
            &[],
            &diff.remove,
        )?;
        let mut writes = vault::in_proposal(&proposal.id, own);
        writes.extend(vault::base_writes(
            &proposal.id,
            self.theirs,
            Some(self.base),
        )?);
        writes.push(vault::proposal_write(&proposal)?);
        let mut subjects = diff.subjects;
        subjects.changed.push(current.qualify(&proposal.id));
        subjects.changed.sort();
        let before = current
            .revision
            .files
            .iter()
            .chain(&self.drawn.revision.files);
        Ok(Plan {
            kind: "reconcile",
            scope: Scope::Reconcile(proposal.id),
            operation_id,
            writer,
            doing: writing.doing.to_owned(),
            before: revision::of_digests(
                before.map(|f| (f.path.clone(), f.sha256.clone())).collect(),
            ),
            writes,
            subjects,
            allocated: Vec::new(),
        })
    }

    /// `MODEL_INVALID` when the merged draft does not load: the merge combined changes that each
    /// side made validly, such as a relationship to a module the other side removed.
    fn refuse_invalid(&self, proposal: &Proposal) -> Result<()> {
        let current = self.current;
        let problems = vault::problems(
            &current.manifest,
            &current.config,
            Some(proposal),
            self.modules,
            self.collections,
            (&self.drawn.walkthroughs, &[], &[]),
        );
        if problems.is_empty() {
            return Ok(());
        }
        let id = &proposal.id;
        Err(Refusal::new(
            Code::ModelInvalid,
            format!(
                "merging proposal {id} with its predecessor leaves {} problem(s); edit the proposal so the merge holds, then reconcile again",
                problems.len()
            ),
        )
        .with_details(json!({ "proposal": id, "cause": "merge", "problems": problems }))
        .into())
    }
}

/// `proposal` after reconciling: the predecessor and parent `caught`, a base, and the IDs the
/// merge removed from it (`removed`) on its dropped list, less those the current state already
/// retired.
fn caught_up(
    mut proposal: Proposal,
    (predecessor, parent): (Predecessor, Option<String>),
    current: &Model,
    removed: &[String],
) -> Proposal {
    let retired: BTreeSet<&String> = current.manifest.retired_ids.iter().collect();
    proposal.dropped.extend(removed.iter().cloned());
    proposal.dropped.retain(|id| !retired.contains(id));
    proposal.dropped.sort();
    proposal.dropped.dedup();
    proposal.predecessor = predecessor;
    proposal.parent = parent;
    proposal.base = true;
    proposal
}

fn no_base(id: &str) -> anyhow::Error {
    Refusal::new(
        Code::ReconcileNoBase,
        format!(
            "proposal {id} was drawn before drafts kept a base to merge against; draw a new one with `boundrail model propose --from current`"
        ),
    )
    .with_details(json!({ "proposal": id }))
    .into()
}

fn conflicted(id: &str, conflicts: &[Conflict]) -> anyhow::Error {
    Refusal::new(
        Code::ReconcileConflict,
        format!(
            "proposal {id} and its predecessor both changed {} subject(s); settle each with `--settle <subject>=mine|theirs|edited:<file>`, or edit the proposal to match one side, then reconcile again",
            conflicts.len()
        ),
    )
    .with_details(json!({ "proposal": id, "conflicts": conflicts }))
    .into()
}
