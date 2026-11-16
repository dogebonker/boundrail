//! `model reconcile`: brings a draft up to date with a current state that moved since it was
//! drawn, by a three-way merge of its base, the draft and the current state (`super::merge`).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use super::{Writing, open, predecessor_of, refuse_compared, refuse_stale, start};
use crate::arch::edit::{self, Plan, Receipt};
use crate::arch::merge::{self, Conflict};
use crate::arch::types::{Collection, Module, Proposal};
use crate::arch::vault::{self, Model, Scope};
use crate::arch::{Code, Refusal, claims, revision};

/// Merges into the draft `subject` names what the current state changed since the draft's base,
/// and makes the current state its new base and predecessor, in one journaled write. `expected`
/// is the current revision the caller read and `compared` the proposal's. Refuses
/// `STALE_REVISION` when either is at another revision, `RECONCILE_NO_BASE` for a draft drawn
/// before bases existed, `RECONCILE_CONFLICT` when both sides changed one subject, and
/// `MODEL_INVALID` when the merged draft does not load. Claims apply to what the merge changes
/// in the draft, and to the draft.
pub fn reconcile(
    vault: &Path,
    writing: &Writing<'_>,
    subject: &str,
    expected: &str,
    compared: &str,
) -> Result<(Receipt, Option<Scope>)> {
    let (writer, operation) = start(vault, writing)?;
    let request = json!({"kind": "reconcile", "writer": writer, "proposal": subject, "expected_revision": expected, "expected_proposal_revision": compared, "claim": writing.claim});
    edit::journaled(vault, &operation, request, |_, dir, fingerprint| {
        let current = vault::load_dir(vault.to_path_buf())?;
        let (drawn, proposal) = open(vault, subject)?;
        refuse_stale(&current, expected)?;
        refuse_compared(&drawn, compared)?;
        let base = vault::load_base(&drawn)?.ok_or_else(|| no_base(&proposal.id))?;
        let (modules, collections) = merge::merge(
            &current.manifest.id,
            (&base.0, &base.1),
            (&drawn.modules, &drawn.collections),
            (&current.modules, &current.collections),
            &current.manifest.retired_ids,
        )
        .map_err(|conflicts| conflicted(&proposal.id, &conflicts))?;
        let merged = Merged {
            current: &current,
            drawn: &drawn,
            base: (&base.0, &base.1),
            modules: &modules,
            collections: &collections,
        };
        let diff = edit::diff(&drawn, &modules, &collections, &[], &[], &[], &[]);
        let mut touched = diff.affected.clone();
        touched.insert(drawn.qualify(&proposal.id));
        claims::check_edit(dir, &drawn, &writer, writing.claim, &touched, &diff.moves)?;
        let plan = merged.plan(proposal, diff, (writer.clone(), operation.clone()), writing)?;
        Ok(plan.record(fingerprint))
    })
}

/// A draft merged with the current state, ready to be written.
struct Merged<'a> {
    current: &'a Model,
    drawn: &'a Model,
    /// The draft's base before reconciling.
    base: (&'a [Module], &'a [Collection]),
    modules: &'a [Module],
    collections: &'a [Collection],
}

impl Merged<'_> {
    /// The writes of the merged draft by `diff`, its new base (the current state) and its
    /// `proposal.json` caught up with the current state.
    fn plan(
        &self,
        proposal: Proposal,
        diff: edit::Diff<'_>,
        (writer, operation_id): (String, String),
        writing: &Writing<'_>,
    ) -> Result<Plan> {
        let current = self.current;
        let proposal = caught_up(proposal, current, &diff.retired)?;
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
        let now = (&current.modules[..], &current.collections[..]);
        writes.extend(vault::base_writes(&proposal.id, now, Some(self.base))?);
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
            (&[], &[], &[]),
        );
        if problems.is_empty() {
            return Ok(());
        }
        let id = &proposal.id;
        Err(Refusal::new(
            Code::ModelInvalid,
            format!(
                "merging proposal {id} with the current state leaves {} problem(s); edit the proposal so the merge holds, then reconcile again",
                problems.len()
            ),
        )
        .with_details(json!({ "proposal": id, "cause": "merge", "problems": problems }))
        .into())
    }
}

/// `proposal` after reconciling with `current`: the current state as predecessor, a base, and
/// the IDs the merge removed from it (`removed`) on its dropped list, less those the current
/// state already retired.
fn caught_up(mut proposal: Proposal, current: &Model, removed: &[String]) -> Result<Proposal> {
    let retired: BTreeSet<&String> = current.manifest.retired_ids.iter().collect();
    proposal.dropped.extend(removed.iter().cloned());
    proposal.dropped.retain(|id| !retired.contains(id));
    proposal.dropped.sort();
    proposal.dropped.dedup();
    proposal.predecessor = predecessor_of(current)?;
    proposal.base = true;
    Ok(proposal)
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
            "proposal {id} and the current state both changed {} subject(s); edit the proposal to match one side, then reconcile again",
            conflicts.len()
        ),
    )
    .with_details(json!({ "proposal": id, "conflicts": conflicts }))
    .into()
}
