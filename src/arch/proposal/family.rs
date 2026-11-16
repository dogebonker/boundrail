//! Proposal families: a child draft is drawn from a parent draft, which stays its predecessor
//! while it is a draft. Where a draft stands against its predecessor, and the order a family
//! reconciles in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use super::{list, proposal_of};
use crate::arch::types::{Predecessor, Proposal, ProposalStatus};
use crate::arch::vault::{self, Model};
use crate::arch::{Code, Refusal};

/// Where a draft stands against its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// Its predecessor holds what it held when the draft was drawn or last reconciled.
    UpToDate,
    /// Its predecessor moved since; `model reconcile` merges what it gained.
    Behind,
    /// Its parent was shelved, or adopted and the current state has moved on since; `model
    /// reconcile` merges it onto the current state.
    Orphaned,
}

/// What a draft's predecessor is now.
pub(super) enum Upstream {
    /// The current state: the draft has no parent.
    Current,
    /// The parent draft, loaded.
    Draft(Model),
    /// The parent, loaded, adopted and still the current state.
    Adopted(Model),
    /// The parent was adopted and the current state has moved on since.
    Replaced,
    /// The parent was shelved or is gone.
    Orphaned,
}

/// What the predecessor of `proposal`, a proposal of the vault folder `vault` whose current state
/// is `current`, is now.
pub(super) fn upstream(vault: &Path, current: &Model, proposal: &Proposal) -> Result<Upstream> {
    let Some(parent) = &proposal.parent else {
        return Ok(Upstream::Current);
    };
    if !vault::is_proposal(vault, parent)? {
        return Ok(Upstream::Orphaned);
    }
    let model = vault::load_proposal(vault.to_path_buf(), parent)?;
    let drawn = proposal_of(&model)?;
    Ok(match drawn.status {
        ProposalStatus::Draft => Upstream::Draft(model),
        ProposalStatus::Adopted if still_current(current, &model, &drawn) => {
            Upstream::Adopted(model)
        }
        ProposalStatus::Adopted => Upstream::Replaced,
        ProposalStatus::Shelved => Upstream::Orphaned,
    })
}

/// Whether the adopted proposal `drawn`, loaded as `model`, still is the current state, unchanged.
pub(super) fn still_current(current: &Model, model: &Model, drawn: &Proposal) -> bool {
    let state = current.manifest.current.as_ref().map(|s| &s.id);
    drawn.adopted_as.as_ref() == state
        && model.modules == current.modules
        && model.collections == current.collections
}

/// Whether the predecessor `proposal` records is `upstream` as it stands now.
pub(super) fn fresh(current: &Model, proposal: &Proposal, upstream: &Upstream) -> Result<bool> {
    let recorded = &proposal.predecessor.revision;
    Ok(match upstream {
        Upstream::Current => {
            *recorded == current.predecessor_digest()?
                || *recorded == current.earlier_predecessor_digest()
        }
        Upstream::Draft(parent) | Upstream::Adopted(parent) => *recorded == parent.content_digest(),
        Upstream::Replaced | Upstream::Orphaned => false,
    })
}

/// Where `proposal`, of the vault folder `vault` whose current state is `current`, stands;
/// `None` for an adopted or shelved proposal.
pub fn standing(vault: &Path, current: &Model, proposal: &Proposal) -> Result<Option<Standing>> {
    if proposal.status != ProposalStatus::Draft {
        return Ok(None);
    }
    let upstream = upstream(vault, current, proposal)?;
    Ok(Some(match upstream {
        Upstream::Replaced | Upstream::Orphaned => Standing::Orphaned,
        _ if fresh(current, proposal, &upstream)? => Standing::UpToDate,
        _ => Standing::Behind,
    }))
}

/// The predecessor a child of the draft `parent` records.
pub(super) fn predecessor_of_draft(parent: &Model) -> Result<Predecessor> {
    Ok(Predecessor {
        state: Some(proposal_of(parent)?.id),
        revision: parent.content_digest(),
    })
}

/// `PARENT_NOT_ADOPTED` for adopting `proposal`, a child of the draft `parent`.
pub(super) fn parent_not_adopted(proposal: &Proposal, parent: &str) -> anyhow::Error {
    let id = &proposal.id;
    Refusal::new(
        Code::ParentNotAdopted,
        format!("proposal {id} is a child of the draft {parent}; adopt {parent} first"),
    )
    .with_details(json!({ "proposal": id, "parent": parent }))
    .into()
}

/// The draft `root` of the vault folder `vault` and every draft descending from it through
/// drafts, parents before their children and siblings in ID order.
pub(super) fn family(vault: &Path, root: &str) -> Result<Vec<String>> {
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for listed in list(vault)? {
        if let Ok(p) = listed.proposal
            && p.status == ProposalStatus::Draft
            && let Some(parent) = p.parent
        {
            children.entry(parent).or_default().push(listed.id);
        }
    }
    let (mut order, mut seen) = (Vec::new(), BTreeSet::new());
    let mut stack = vec![root.to_owned()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(kids) = children.remove(&id) {
            stack.extend(kids.into_iter().rev());
        }
        order.push(id);
    }
    Ok(order)
}
