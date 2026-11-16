//! The flow changes of a `model edit` batch: collections, flows, steps and branches, and what the
//! batch changed in them.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Batch, Diff, at, both, rename, request_invalid};
use crate::arch::types::{
    Anchor, Branch, Collection, Emphasis, Flow, SCHEMA_VERSION, SUMMARY_MAX, Step, find_step,
};
use crate::arch::vault::{self, Model};
use crate::arch::{Code, Refusal, ids};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCollection {
    pub name: String,
    pub order: u32,
    pub description: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionSet {
    pub name: Option<String>,
    pub order: Option<u32>,
    pub description: Option<String>,
}

/// A collection field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollectionField {
    Description,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewFlow {
    pub name: String,
    pub order: u32,
    /// Required: an `add-flow` without a non-blank summary refuses `FLOW_SUMMARY_REQUIRED`.
    #[schemars(required, length(min = 1, max = SUMMARY_MAX))]
    pub summary: Option<String>,
    pub description: Option<String>,
    /// Modules the sequence view draws first, in order, referenced like module subjects.
    #[serde(default)]
    #[schemars(extend("uniqueItems" = true))]
    pub participants: Vec<String>,
    #[serde(default)]
    pub steps: Vec<NewStep>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowSet {
    pub name: Option<String>,
    pub order: Option<u32>,
    /// A blank summary refuses `FLOW_SUMMARY_REQUIRED`.
    pub summary: Option<String>,
    pub description: Option<String>,
    /// Replaces the participant list; modules are referenced like module subjects.
    #[schemars(extend("uniqueItems" = true))]
    pub participants: Option<Vec<String>>,
}

/// A flow field `clear` may empty; required fields cannot be cleared. Clearing the summary
/// refuses `FLOW_SUMMARY_REQUIRED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowField {
    Summary,
    Description,
    Participants,
}

/// A step to add, with the steps inside it. Modules are referenced like module subjects, a
/// relationship by its qualified or local ID.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewStep {
    Message {
        from: String,
        to: String,
        label: String,
        relationship: Option<String>,
        #[serde(default)]
        reply: bool,
        emphasis: Option<Emphasis>,
        repeat: Option<u32>,
        note: Option<String>,
        anchor: Option<Anchor>,
    },
    Decision {
        label: String,
        branches: Vec<NewBranch>,
    },
    Loop {
        label: String,
        #[serde(default)]
        steps: Vec<NewStep>,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewBranch {
    pub label: String,
    #[serde(default)]
    pub steps: Vec<NewStep>,
}

/// New values for a step. Only `label` applies to a decision or a loop.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSet {
    pub label: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub relationship: Option<String>,
    pub reply: Option<bool>,
    pub emphasis: Option<Emphasis>,
    pub repeat: Option<u32>,
    pub note: Option<String>,
    pub anchor: Option<Anchor>,
}

/// A message field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepField {
    Relationship,
    Reply,
    Emphasis,
    Repeat,
    Note,
    Anchor,
}

impl CollectionField {
    fn is_set(self, set: &CollectionSet) -> bool {
        match self {
            Self::Description => set.description.is_some(),
        }
    }
}

impl FlowField {
    fn is_set(self, set: &FlowSet) -> bool {
        match self {
            Self::Summary => set.summary.is_some(),
            Self::Description => set.description.is_some(),
            Self::Participants => set.participants.is_some(),
        }
    }
}

impl StepField {
    fn is_set(self, set: &StepSet) -> bool {
        match self {
            Self::Relationship => set.relationship.is_some(),
            Self::Reply => set.reply.is_some(),
            Self::Emphasis => set.emphasis.is_some(),
            Self::Repeat => set.repeat.is_some(),
            Self::Note => set.note.is_some(),
            Self::Anchor => set.anchor.is_some(),
        }
    }
}

impl StepSet {
    fn has_message_fields(&self) -> bool {
        self.from.is_some()
            || self.to.is_some()
            || self.relationship.is_some()
            || self.reply.is_some()
            || self.emphasis.is_some()
            || self.repeat.is_some()
            || self.note.is_some()
            || self.anchor.is_some()
    }
}

impl Batch<'_> {
    /// The position of the collection `subject` names.
    fn collection_at(&self, index: usize, subject: &str) -> Result<usize> {
        let found = vault::collection_position(self.model, &self.collections, subject);
        Ok(found.map_err(at(index))?)
    }

    /// The positions of the flow `subject` names and of its collection.
    pub(super) fn flow_at(&self, index: usize, subject: &str) -> Result<(usize, usize)> {
        let found = vault::flow_position(self.model, &self.collections, subject);
        Ok(found.map_err(at(index))?)
    }

    /// The positions of the collection and flow holding a step that `wanted` accepts. `subject`
    /// and `what` name what was looked for in a refusal.
    fn holding(
        &self,
        index: usize,
        subject: &str,
        what: &str,
        wanted: impl Fn(&Step) -> bool,
    ) -> Result<(usize, usize)> {
        for (c, collection) in self.collections.iter().enumerate() {
            for (f, flow) in collection.flows.iter().enumerate() {
                if find_step(&flow.steps, &wanted).is_some() {
                    return Ok((c, f));
                }
            }
        }
        let refusal = Refusal::new(
            Code::SubjectNotFound,
            format!("no {what} has the ID `{subject}`"),
        );
        Err(at(index)(refusal).into())
    }

    /// The positions of the collection and flow holding the step `subject` names, with the
    /// step's local ID.
    fn step_at(&self, index: usize, subject: &str) -> Result<(usize, usize, String)> {
        let id = ids::local(self.model, subject);
        let (c, f) = self.holding(index, subject, "step", |step| step.id() == id)?;
        Ok((c, f, id.to_owned()))
    }

    /// The step with local ID `id` in the flow at `(c, f)`.
    fn step_mut(&mut self, (c, f): (usize, usize), id: &str) -> Result<&mut Step> {
        step_in(&mut self.collections[c].flows[f].steps, id)
            .with_context(|| format!("step {id} is not in the batch"))
    }

    pub(super) fn remove_collection(&mut self, index: usize, subject: &str) -> Result<()> {
        let c = self.collection_at(index, subject)?;
        let removed = self.collections.remove(c);
        // A beat reading one of its flows names the change that removed it.
        for flow in &removed.flows {
            self.touched.insert(flow.id.clone(), index);
        }
        self.touched.insert(removed.id, index);
        Ok(())
    }

    pub(super) fn remove_flow(&mut self, index: usize, subject: &str) -> Result<()> {
        let (c, f) = self.flow_at(index, subject)?;
        let removed = self.collections[c].flows.remove(f);
        self.touched.insert(removed.id, index);
        Ok(())
    }

    pub(super) fn add_collection(&mut self, index: usize, new: NewCollection) -> Result<()> {
        let id = self.new_id(index, new.name.clone())?;
        self.collections.push(Collection {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            order: new.order,
            description: new.description,
            former_names: Vec::new(),
            flows: Vec::new(),
        });
        Ok(())
    }

    pub(super) fn edit_collection(
        &mut self,
        index: usize,
        subject: &str,
        set: CollectionSet,
        clear: &[CollectionField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let c = self.collection_at(index, subject)?;
        let id = self.collections[c].id.clone();
        self.edit_once(index, &id)?;
        let collection = &mut self.collections[c];
        if let Some(name) = set.name {
            rename(&mut collection.name, &mut collection.former_names, name);
        }
        if let Some(order) = set.order {
            collection.order = order;
        }
        if set.description.is_some() {
            collection.description = set.description;
        }
        for field in clear {
            match field {
                CollectionField::Description => collection.description = None,
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    pub(super) fn add_flow(&mut self, index: usize, collection: &str, new: NewFlow) -> Result<()> {
        let c = self.collection_at(index, collection)?;
        if new.summary.as_deref().is_none_or(is_blank) {
            return Err(summary_required(index, &new.name));
        }
        let id = self.new_id(index, new.name.clone())?;
        let participants = self.module_ids(index, &new.participants)?;
        let steps = self.build_steps(index, new.steps)?;
        self.collections[c].flows.push(Flow {
            id,
            name: new.name,
            order: new.order,
            summary: new.summary,
            description: new.description,
            participants,
            former_names: Vec::new(),
            steps,
        });
        Ok(())
    }

    pub(super) fn edit_flow(
        &mut self,
        index: usize,
        subject: &str,
        set: FlowSet,
        clear: &[FlowField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let (c, f) = self.flow_at(index, subject)?;
        if clear.contains(&FlowField::Summary) || set.summary.as_deref().is_some_and(is_blank) {
            return Err(summary_required(index, subject));
        }
        let id = self.collections[c].flows[f].id.clone();
        self.edit_once(index, &id)?;
        let participants = set
            .participants
            .map(|list| self.module_ids(index, &list))
            .transpose()?;
        let flow = &mut self.collections[c].flows[f];
        if let Some(name) = set.name {
            rename(&mut flow.name, &mut flow.former_names, name);
        }
        if let Some(order) = set.order {
            flow.order = order;
        }
        if set.summary.is_some() {
            flow.summary = set.summary;
        }
        if set.description.is_some() {
            flow.description = set.description;
        }
        if let Some(participants) = participants {
            flow.participants = participants;
        }
        for field in clear {
            match field {
                FlowField::Summary => flow.summary = None,
                FlowField::Description => flow.description = None,
                FlowField::Participants => flow.participants.clear(),
            }
        }
        self.touched.insert(id, index);
        Ok(())
    }

    pub(super) fn move_flow(
        &mut self,
        index: usize,
        subject: &str,
        collection: &str,
        order: Option<u32>,
    ) -> Result<()> {
        let (c, f) = self.flow_at(index, subject)?;
        let target = self.collection_at(index, collection)?;
        let mut flow = self.collections[c].flows.remove(f);
        if let Some(order) = order {
            flow.order = order;
        }
        self.touched.insert(flow.id.clone(), index);
        self.collections[target].flows.push(flow);
        Ok(())
    }

    fn build_steps(&mut self, index: usize, steps: Vec<NewStep>) -> Result<Vec<Step>> {
        steps
            .into_iter()
            .map(|step| self.build_step(index, step))
            .collect()
    }

    /// The step `new` describes, with fresh IDs for it and the steps and branches inside it.
    fn build_step(&mut self, index: usize, new: NewStep) -> Result<Step> {
        Ok(match new {
            NewStep::Message {
                from,
                to,
                label,
                relationship,
                reply,
                emphasis,
                repeat,
                note,
                anchor,
            } => Step::Message {
                from: self.module_id(index, &from)?,
                to: self.module_id(index, &to)?,
                relationship: self.relationship_id(index, relationship.as_deref())?,
                id: self.new_id(index, format!("message {label}"))?,
                label,
                reply,
                emphasis,
                repeat,
                note,
                anchor,
            },
            NewStep::Decision { label, branches } => Step::Decision {
                id: self.new_id(index, format!("decision {label}"))?,
                branches: branches
                    .into_iter()
                    .map(|branch| self.build_branch(index, branch))
                    .collect::<Result<_>>()?,
                label,
            },
            NewStep::Loop { label, steps } => Step::Loop {
                id: self.new_id(index, format!("loop {label}"))?,
                steps: self.build_steps(index, steps)?,
                label,
            },
        })
    }

    fn build_branch(&mut self, index: usize, new: NewBranch) -> Result<Branch> {
        Ok(Branch {
            id: self.new_id(index, format!("branch {}", new.label))?,
            steps: self.build_steps(index, new.steps)?,
            label: new.label,
        })
    }

    pub(super) fn module_id(&self, index: usize, subject: &str) -> Result<String> {
        self.module(index, subject).map(|at| self.qualify(at))
    }

    fn module_ids(&self, index: usize, subjects: &[String]) -> Result<Vec<String>> {
        subjects.iter().map(|s| self.module_id(index, s)).collect()
    }

    fn relationship_id(&self, index: usize, subject: Option<&str>) -> Result<Option<String>> {
        subject
            .map(|s| {
                let (m, r) = self.relationship(index, s)?;
                Ok(ids::qualify(self.model, &self.modules[m].outgoing[r].id))
            })
            .transpose()
    }

    pub(super) fn add_step(
        &mut self,
        index: usize,
        flow: &str,
        parent: Option<&str>,
        position: Option<usize>,
        new: NewStep,
    ) -> Result<()> {
        let step = self.build_step(index, new)?;
        self.place(index, flow, parent, position, step)
    }

    pub(super) fn move_step(
        &mut self,
        index: usize,
        subject: &str,
        flow: &str,
        parent: Option<&str>,
        position: Option<usize>,
    ) -> Result<()> {
        let step = self.take_step(index, subject)?;
        if let Some(parent) = parent
            && step.holds(ids::local(self.model, parent))
        {
            let message = format!("a step cannot move inside itself: {parent} is in {subject}");
            return Err(request_invalid(index, message));
        }
        self.place(index, flow, parent, position, step)
    }

    /// Puts `step` at `position` of the flow's own steps or of the branch or loop `parent`.
    fn place(
        &mut self,
        index: usize,
        flow: &str,
        parent: Option<&str>,
        position: Option<usize>,
        step: Step,
    ) -> Result<()> {
        let (c, f) = self.flow_at(index, flow)?;
        let model = self.model;
        let flow = &mut self.collections[c].flows[f];
        self.touched.insert(flow.id.clone(), index);
        self.touched.insert(step.id().to_owned(), index);
        let list = match parent {
            None => &mut flow.steps,
            Some(parent) => {
                list_in(&mut flow.steps, ids::local(model, parent)).ok_or_else(|| {
                    let flow = ids::qualify(model, &flow.id);
                    let message = format!("flow {flow} has no branch or loop `{parent}`");
                    at(index)(Refusal::new(Code::SubjectNotFound, message))
                })?
            }
        };
        insert(index, list, position, step)
    }

    /// Removes the step `subject` names, with the steps inside it, from its flow.
    pub(super) fn take_step(&mut self, index: usize, subject: &str) -> Result<Step> {
        let (c, f, id) = self.step_at(index, subject)?;
        let flow = &mut self.collections[c].flows[f];
        self.touched.insert(flow.id.clone(), index);
        self.touched.insert(id.clone(), index);
        take(&mut flow.steps, &id).with_context(|| format!("step {id} is not in the batch"))
    }

    pub(super) fn edit_step(
        &mut self,
        index: usize,
        subject: &str,
        mut set: StepSet,
        clear: &[StepField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let (c, f, id) = self.step_at(index, subject)?;
        self.edit_once(index, &id)?;
        set.from = set.from.map(|s| self.module_id(index, &s)).transpose()?;
        set.to = set.to.map(|s| self.module_id(index, &s)).transpose()?;
        set.relationship = self.relationship_id(index, set.relationship.as_deref())?;
        self.touched.insert(id.clone(), index);
        let step = self.step_mut((c, f), &id)?;
        if !matches!(step, Step::Message { .. }) && (set.has_message_fields() || !clear.is_empty())
        {
            let message = format!("only a message step has fields besides its label: {subject}");
            return Err(request_invalid(index, message));
        }
        apply_set(step, set, clear);
        Ok(())
    }

    pub(super) fn add_branch(
        &mut self,
        index: usize,
        decision: &str,
        position: Option<usize>,
        new: NewBranch,
    ) -> Result<()> {
        let (c, f, id) = self.step_at(index, decision)?;
        let branch = self.build_branch(index, new)?;
        self.touched.insert(id.clone(), index);
        let Step::Decision { branches, .. } = self.step_mut((c, f), &id)? else {
            return Err(request_invalid(
                index,
                format!("{decision} is not a decision"),
            ));
        };
        insert(index, branches, position, branch)
    }

    pub(super) fn remove_branch(&mut self, index: usize, subject: &str) -> Result<()> {
        let branch = ids::local(self.model, subject).to_owned();
        let is_decision = |step: &Step| match step {
            Step::Decision { branches, .. } => branches.iter().any(|b| b.id == branch),
            _ => false,
        };
        let (c, f) = self.holding(index, subject, "branch", is_decision)?;
        let decision = find_step(&self.collections[c].flows[f].steps, &is_decision)
            .map(|step| step.id().to_owned())
            .with_context(|| format!("branch {branch} is not in the batch"))?;
        // A decision left with one branch fails validation, naming this change.
        self.touched.insert(decision.clone(), index);
        self.touched.insert(branch.clone(), index);
        if let Step::Decision { branches, .. } = self.step_mut((c, f), &decision)? {
            branches.retain(|b| b.id != branch);
        }
        Ok(())
    }
}

/// Sets and clears a message's fields, or a decision's or loop's label.
fn apply_set(step: &mut Step, set: StepSet, clear: &[StepField]) {
    let Step::Message {
        label,
        from,
        to,
        relationship,
        reply,
        emphasis,
        repeat,
        note,
        anchor,
        ..
    } = step
    else {
        if let (Step::Decision { label, .. } | Step::Loop { label, .. }, Some(new)) =
            (step, set.label)
        {
            *label = new;
        }
        return;
    };
    let fields = [(label, set.label), (from, set.from), (to, set.to)];
    for (field, value) in fields {
        if let Some(value) = value {
            *field = value;
        }
    }
    if set.relationship.is_some() {
        *relationship = set.relationship;
    }
    if let Some(value) = set.reply {
        *reply = value;
    }
    if set.emphasis.is_some() {
        *emphasis = set.emphasis;
    }
    if set.repeat.is_some() {
        *repeat = set.repeat;
    }
    if set.note.is_some() {
        *note = set.note;
    }
    if set.anchor.is_some() {
        *anchor = set.anchor;
    }
    for field in clear {
        match field {
            StepField::Relationship => *relationship = None,
            StepField::Reply => *reply = false,
            StepField::Emphasis => *emphasis = None,
            StepField::Repeat => *repeat = None,
            StepField::Note => *note = None,
            StepField::Anchor => *anchor = None,
        }
    }
}

fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `FLOW_SUMMARY_REQUIRED` for change `index`, which adds the flow `flow` without a summary or
/// clears or blanks its summary.
fn summary_required(index: usize, flow: &str) -> anyhow::Error {
    let message = format!("flow {flow} needs a summary: one line saying what the flow does");
    at(index)(Refusal::new(Code::FlowSummaryRequired, message)).into()
}

/// Inserts `item` at `position` of `list`, at its end when absent.
pub(super) fn insert<T>(
    index: usize,
    list: &mut Vec<T>,
    position: Option<usize>,
    item: T,
) -> Result<()> {
    let at = position.unwrap_or(list.len());
    if at > list.len() {
        let message = format!("position {at} is past the end of a list of {}", list.len());
        return Err(request_invalid(index, message));
    }
    list.insert(at, item);
    Ok(())
}

/// The step list of the branch or loop with local ID `parent`, at any depth of `steps`.
fn list_in<'s>(steps: &'s mut [Step], parent: &str) -> Option<&'s mut Vec<Step>> {
    for step in steps {
        for (id, list) in step.lists_mut() {
            if id == parent {
                return Some(list);
            }
            if let Some(found) = list_in(list, parent) {
                return Some(found);
            }
        }
    }
    None
}

/// The step with local ID `id`, at any depth of `steps`.
fn step_in<'s>(steps: &'s mut [Step], id: &str) -> Option<&'s mut Step> {
    for step in steps {
        if step.id() == id {
            return Some(step);
        }
        for (_, list) in step.lists_mut() {
            if let Some(found) = step_in(list, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Removes the step with local ID `id`, at any depth of `steps`.
fn take(steps: &mut Vec<Step>, id: &str) -> Option<Step> {
    if let Some(at) = steps.iter().position(|s| s.id() == id) {
        return Some(steps.remove(at));
    }
    steps
        .iter_mut()
        .find_map(|s| s.lists_mut().find_map(|(_, list)| take(list, id)))
}

/// Adds to `diff` what the batch changed in the collections: files to write and remove,
/// collection and flow subjects, and retired IDs. It adds to `touched` the subjects a claim must
/// cover: a collection whose own fields change, the root level when a collection comes or goes,
/// a flow that changes, the collections a flow enters or leaves, and every collection or flow
/// whose current or former name an added or renamed one takes. Steps and branches are covered by
/// their flow.
pub(super) fn diff_collections<'a>(
    model: &'a Model,
    new: &'a [Collection],
    diff: &mut Diff<'a>,
    touched: &mut Vec<Option<String>>,
) {
    let by_id = |list: &'a [Collection]| -> BTreeMap<&'a str, &'a Collection> {
        list.iter().map(|c| (c.id.as_str(), c)).collect()
    };
    let (old, now) = (by_id(&model.collections), by_id(new));
    for (id, collection) in &now {
        match old.get(id) {
            None => diff.subjects.added.push(model.qualify(id)),
            Some(before) if before != collection => diff.subjects.changed.push(model.qualify(id)),
            Some(_) => continue,
        }
        diff.write_collections.push(collection);
    }
    let live: BTreeSet<&str> = new.iter().flat_map(Collection::ids).collect();
    for (id, collection) in &old {
        if !now.contains_key(id) {
            diff.subjects.removed.push(model.qualify(id));
            diff.remove.push(vault::collection_file(id));
        }
        let gone = collection.ids().into_iter().filter(|id| !live.contains(id));
        diff.retired.extend(gone.map(str::to_owned));
    }
    let header = |c: Option<&&'a Collection>| c.map(|c| (&c.name, c.order, &c.description));
    for id in old.keys().chain(now.keys()).collect::<BTreeSet<_>>() {
        let (before, after) = (old.get(id), now.get(id));
        if header(before) != header(after) {
            touched.push(Some(model.qualify(id)));
        }
        if before.is_none() || after.is_none() {
            touched.push(None);
        }
        // A name an added or renamed collection takes would stop naming another alone.
        if let Some(named) = after.filter(|a| before.is_none_or(|b| b.name != a.name)) {
            let same =
                (old.values()).filter(|c| vault::holds_name(&c.name, &c.former_names, &named.name));
            touched.extend(same.map(|c| Some(model.qualify(&c.id))));
        }
    }
    diff_flows(model, &old, &now, diff, touched);
}

fn diff_flows(
    model: &Model,
    old: &BTreeMap<&str, &Collection>,
    now: &BTreeMap<&str, &Collection>,
    diff: &mut Diff<'_>,
    touched: &mut Vec<Option<String>>,
) {
    let (old, now) = (flows(old), flows(now));
    for id in old.keys().chain(now.keys()).collect::<BTreeSet<_>>() {
        let (before, after) = (old.get(id), now.get(id));
        if before == after {
            continue;
        }
        let subject = model.qualify(id);
        match (before, after) {
            (None, _) => diff.subjects.added.push(subject.clone()),
            (_, None) => diff.subjects.removed.push(subject.clone()),
            _ => diff.subjects.changed.push(subject.clone()),
        }
        touched.push(Some(subject));
        if let Some((_, named)) =
            after.filter(|(_, a)| before.is_none_or(|(_, b)| b.name != a.name))
        {
            let same = (old.iter())
                .filter(|(_, (_, f))| vault::holds_name(&f.name, &f.former_names, &named.name));
            touched.extend(same.map(|(id, _)| Some(model.qualify(id))));
        }
        let (left, entered) = (before.map(|(c, _)| *c), after.map(|(c, _)| *c));
        if left != entered {
            touched.extend(
                left.into_iter()
                    .chain(entered)
                    .map(|c| Some(model.qualify(c))),
            );
        }
    }
}

/// Every flow by local ID, with the local ID of its collection.
fn flows<'c>(
    collections: &BTreeMap<&'c str, &'c Collection>,
) -> BTreeMap<&'c str, (&'c str, &'c Flow)> {
    collections
        .values()
        .flat_map(|c| {
            c.flows
                .iter()
                .map(move |f| (f.id.as_str(), (c.id.as_str(), f)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::{Record, plan};
    use crate::arch::types::{Collection, Manifest, VaultConfig};
    use crate::arch::vault::Model;
    use crate::arch::{Code, Refusal, revision};

    const M: &str = "Model001";
    const ORDERS: &str = "Coll0001";
    const BILLING: &str = "Coll0002";
    const CHECKOUT: &str = "Flow0001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    /// Web calls Api (`Rel00001`), Api calls Db; Ops stands alone. Orders holds Checkout, which
    /// has no summary: Web asks Api (`Step0001`), decision `Decide01` with branches `Branch01`
    /// (Api asks Db, `Step0002`, anchored in `src/api.ts`) and `Branch02` (empty), then loop
    /// `Loop0001`. Billing is empty.
    fn model() -> Model {
        let module = |id: &str, name: &str, outgoing: Value| {
            serde_json::from_value(json!({
                "schema_version": 1, "id": id, "name": name, "kind": "service", "order": 0,
                "outgoing": outgoing,
            }))
            .unwrap()
        };
        let call =
            |id: &str, to: &str| json!([{"id": id, "to": q(to), "kind": "call", "order": 0}]);
        let message = |id: &str, from: &str, to: &str| json!({"kind": "message", "id": id, "from": q(from), "to": q(to), "label": "ask"});
        let mut ask = message("Step0001", "WebApp01", "ApiSrv01");
        ask["relationship"] = q("Rel00001").into();
        let mut store = message("Step0002", "ApiSrv01", "DbStore1");
        store["anchor"] = json!({"path": "src/api.ts", "symbol": "store"});
        let steps = json!([
            ask,
            {"kind": "decision", "id": "Decide01", "label": "paid?", "branches": [
                {"id": "Branch01", "label": "yes", "steps": [store]},
                {"id": "Branch02", "label": "no"},
            ]},
            {"kind": "loop", "id": "Loop0001", "label": "retry"},
        ]);
        let collections: Vec<Collection> = serde_json::from_value(json!([
            {"schema_version": 1, "id": ORDERS, "name": "Orders", "order": 0,
             "flows": [{"id": CHECKOUT, "name": "Checkout", "order": 0, "steps": steps}]},
            {"schema_version": 1, "id": BILLING, "name": "Billing", "order": 1},
        ]))
        .unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "schema_version": 1, "id": M, "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap();
        Model {
            dir: PathBuf::from("vault"),
            manifest,
            config: VaultConfig::default(),
            modules: vec![
                module("WebApp01", "Web", call("Rel00001", "ApiSrv01")),
                module("ApiSrv01", "Api", call("Rel00002", "DbStore1")),
                module("DbStore1", "Db", json!([])),
                module("OpsSvc01", "Ops", json!([])),
            ],
            collections,
            walkthroughs: Vec::new(),
            pictures: Vec::new(),
            views: Vec::new(),
            revision: revision::of(std::iter::empty()),
            proposal: None,
        }
    }

    fn plan_of(changes: Value) -> anyhow::Result<(Record, BTreeSet<String>)> {
        let request = serde_json::from_value(json!({
            "target": {"model": M, "state": "State001"}, "expected_revision": "unused",
            "operation_id": "op-1", "writer": "tester", "changes": changes,
        }))
        .unwrap();
        plan(&model(), request, "testing flows", String::new()).map(|(r, a, _)| (r, a))
    }

    fn refusal(changes: Value) -> (Code, Value) {
        let Err(error) = plan_of(changes) else {
            panic!("the plan succeeded");
        };
        let refusal = error.downcast_ref::<Refusal>().unwrap();
        (refusal.code, refusal.details.clone().unwrap_or_default())
    }

    fn written(record: &Record, path: &str) -> Option<String> {
        let writes = record.writes.as_deref().unwrap();
        writes.iter().find(|w| w.path == path)?.content.clone()
    }

    fn orders(record: &Record) -> Value {
        let path = format!("collections/{ORDERS}.collection.json");
        serde_json::from_str(&written(record, &path).unwrap()).unwrap()
    }

    fn qualified(locals: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = locals.iter().map(|id| q(id)).collect();
        ids.sort();
        ids
    }

    #[test]
    fn adding_a_collection_with_a_flow_and_steps_touches_only_the_root_level() {
        let (record, affected) = plan_of(json!([
            {"type": "add-collection", "collection": {"name": "Payments", "order": 2}},
            {"type": "add-flow", "collection": "Payments", "flow": {"name": "Refund", "order": 0, "summary": "Money goes back", "steps": [
                {"kind": "message", "from": "Web", "to": "Api", "label": "refund", "relationship": "Rel00001"},
                {"kind": "loop", "label": "poll", "steps": [
                    {"kind": "message", "from": "Api", "to": "Db", "label": "check", "repeat": 3},
                ]},
            ]}},
        ]))
        .unwrap();
        let receipt = &record.output;
        let names: Vec<&str> = receipt.allocated.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Payments",
                "Refund",
                "message refund",
                "loop poll",
                "message check"
            ]
        );
        assert_eq!(affected, BTreeSet::from([M.to_owned()]));
        let mut added = vec![
            receipt.allocated[0].id.clone(),
            receipt.allocated[1].id.clone(),
        ];
        added.sort();
        assert_eq!(receipt.subjects.added, added);
        let local = receipt.allocated[0].id.split_once('#').unwrap().1;
        let file = written(&record, &format!("collections/{local}.collection.json")).unwrap();
        let collection: Value = serde_json::from_str(&file).unwrap();
        let steps = &collection["flows"][0]["steps"];
        assert_eq!(steps[0]["from"], q("WebApp01"));
        assert_eq!(steps[0]["relationship"], q("Rel00001"));
        assert_eq!(steps[1]["steps"][0]["repeat"], 3);
    }

    #[test]
    fn each_change_touches_what_a_claim_must_cover() {
        let flow = [ORDERS, CHECKOUT];
        let cases: [(Value, &[&str], &[&str]); 8] = [
            (
                json!({"type": "edit-collection", "subject": "Orders", "set": {"name": "Sales"}}),
                &[ORDERS],
                &[ORDERS],
            ),
            (
                json!({"type": "edit-flow", "subject": "Checkout", "set": {"summary": "Buy"}}),
                &flow,
                &[CHECKOUT],
            ),
            (
                json!({"type": "move-flow", "subject": CHECKOUT, "collection": "Billing", "order": 4}),
                &[ORDERS, BILLING, CHECKOUT],
                &[ORDERS, BILLING, CHECKOUT],
            ),
            (
                json!({"type": "add-step", "flow": "Checkout", "parent": "Branch02",
                       "step": {"kind": "message", "from": "Api", "to": "Web", "label": "decline", "reply": true}}),
                &flow,
                &[CHECKOUT],
            ),
            (
                json!({"type": "edit-step", "subject": q("Step0001"), "set": {"label": "pay"}}),
                &flow,
                &[CHECKOUT],
            ),
            (
                json!({"type": "move-step", "subject": "Step0002", "flow": CHECKOUT, "parent": "Loop0001"}),
                &flow,
                &[CHECKOUT],
            ),
            (
                json!({"type": "add-branch", "decision": "Decide01", "position": 0, "branch": {"label": "later"}}),
                &flow,
                &[CHECKOUT],
            ),
            (
                json!({"type": "edit-collection", "subject": BILLING, "clear": ["description"]}),
                &[],
                &[],
            ),
        ];
        for (change, changed, affected) in cases {
            let (record, touched) = plan_of(json!([change])).unwrap();
            assert_eq!(
                record.output.subjects.changed,
                qualified(changed),
                "{change}"
            );
            assert_eq!(
                touched,
                qualified(affected).into_iter().collect(),
                "{change}"
            );
        }
    }

    #[test]
    fn removals_retire_every_id_inside() {
        let retired = |record: &Record| -> Vec<String> {
            let manifest: Manifest =
                serde_json::from_str(&written(record, "model.json").unwrap()).unwrap();
            let mut ids = manifest.retired_ids;
            ids.sort();
            ids
        };
        let (record, affected) =
            plan_of(json!([{"type": "remove-collection", "subject": ORDERS}])).unwrap();
        assert_eq!(
            record.output.subjects.removed,
            qualified(&[ORDERS, CHECKOUT])
        );
        let mut inside = vec![
            ORDERS, CHECKOUT, "Step0001", "Decide01", "Branch01", "Step0002", "Branch02",
            "Loop0001",
        ];
        inside.sort_unstable();
        assert_eq!(retired(&record), inside);
        let path = format!("collections/{ORDERS}.collection.json");
        assert_eq!(written(&record, &path), None);
        assert!(
            record
                .writes
                .as_deref()
                .unwrap()
                .iter()
                .any(|w| w.path == path)
        );
        let mut covered = qualified(&[ORDERS, CHECKOUT]);
        covered.push(M.to_owned());
        assert_eq!(affected, covered.into_iter().collect());

        let (record, affected) =
            plan_of(json!([{"type": "remove-step", "subject": "Decide01"}])).unwrap();
        assert_eq!(
            retired(&record),
            ["Branch01", "Branch02", "Decide01", "Step0002"]
        );
        assert_eq!(affected, BTreeSet::from([q(CHECKOUT)]));
        assert_eq!(
            orders(&record)["flows"][0]["steps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let (record, _) = plan_of(json!([{"type": "remove-flow", "subject": "Checkout"}])).unwrap();
        assert_eq!(record.output.subjects.removed, [q(CHECKOUT)]);
        assert_eq!(orders(&record)["flows"], json!([]));
    }

    #[test]
    fn steps_move_across_branches_but_not_inside_themselves() {
        let (record, _) = plan_of(json!([
            {"type": "move-step", "subject": "Step0002", "flow": "Checkout", "parent": "Branch02", "position": 0},
        ]))
        .unwrap();
        let branches = &orders(&record)["flows"][0]["steps"][1]["branches"];
        assert_eq!(branches[0]["steps"], json!([]));
        assert_eq!(branches[1]["steps"][0]["id"], "Step0002");

        let into_itself = json!([{"type": "move-step", "subject": "Decide01", "flow": CHECKOUT, "parent": "Branch01"}]);
        assert_eq!(refusal(into_itself).0, Code::RequestInvalid);
        let past_end = json!([{"type": "move-step", "subject": "Step0001", "flow": CHECKOUT, "parent": "Loop0001", "position": 1}]);
        let (code, details) = refusal(past_end);
        assert_eq!(
            (code, details["change"].as_u64()),
            (Code::RequestInvalid, Some(0))
        );
    }

    #[test]
    fn edit_step_sets_message_fields_and_only_the_label_of_a_decision() {
        let (record, _) = plan_of(json!([
            {"type": "edit-step", "subject": "Step0001", "set": {"label": "pay", "repeat": 2, "note": "card"}, "clear": ["relationship"]},
            {"type": "edit-step", "subject": "Decide01", "set": {"label": "settled?"}},
        ]))
        .unwrap();
        let steps = &orders(&record)["flows"][0]["steps"];
        assert_eq!(
            steps[0],
            json!({"kind": "message", "id": "Step0001", "from": q("WebApp01"), "to": q("ApiSrv01"),
                   "label": "pay", "repeat": 2, "note": "card"})
        );
        assert_eq!(steps[1]["label"], "settled?");

        for change in [
            json!({"type": "edit-step", "subject": "Decide01", "set": {"repeat": 2}}),
            json!({"type": "edit-step", "subject": "Loop0001", "clear": ["note"]}),
            json!({"type": "edit-step", "subject": "Step0001", "set": {"note": "x"}, "clear": ["note"]}),
            json!({"type": "add-branch", "decision": "Loop0001", "branch": {"label": "x"}}),
        ] {
            assert_eq!(refusal(json!([change])).0, Code::RequestInvalid, "{change}");
        }
        let twice = json!([
            {"type": "edit-flow", "subject": CHECKOUT, "set": {"name": "Buy"}},
            {"type": "edit-flow", "subject": "Buy", "set": {"order": 1}},
        ]);
        assert_eq!(refusal(twice).0, Code::RequestInvalid);
    }

    #[test]
    fn a_flow_needs_a_summary_when_added_and_keeps_it_when_edited() {
        let step = json!({"kind": "message", "from": "Web", "to": "Api", "label": "ask"});
        for flow in [
            json!({"name": "Refund", "order": 0, "steps": [step]}),
            json!({"name": "Refund", "order": 0, "summary": "  "}),
        ] {
            let change = json!({"type": "add-flow", "collection": "Billing", "flow": flow});
            let (code, details) = refusal(json!([change]));
            assert_eq!(
                (code, details["change"].as_u64()),
                (Code::FlowSummaryRequired, Some(0)),
                "{change}"
            );
        }
        for change in [
            json!({"type": "edit-flow", "subject": "Checkout", "clear": ["summary"]}),
            json!({"type": "edit-flow", "subject": "Checkout", "set": {"summary": ""}}),
        ] {
            assert_eq!(
                refusal(json!([change.clone()])).0,
                Code::FlowSummaryRequired,
                "{change}"
            );
        }
        plan_of(json!([{"type": "edit-flow", "subject": "Checkout", "set": {"order": 3}}]))
            .unwrap();
    }

    #[test]
    fn participants_name_modules_and_a_removed_one_refuses() {
        let (record, _) = plan_of(json!([
            {"type": "edit-flow", "subject": "Checkout", "set": {"participants": ["Db", "WebApp01"]}},
        ]))
        .unwrap();
        let flow = &orders(&record)["flows"][0];
        assert_eq!(flow["participants"], json!([q("DbStore1"), q("WebApp01")]));

        let (code, _) = refusal(json!([
            {"type": "edit-flow", "subject": "Checkout", "set": {"participants": ["Nobody"]}},
        ]));
        assert_eq!(code, Code::SubjectNotFound);
        let (code, details) = refusal(json!([
            {"type": "edit-flow", "subject": "Checkout", "set": {"participants": ["Ops", "Ops"]}},
        ]));
        assert_eq!(code, Code::ModelInvalid);
        assert_eq!(
            details["problems"][0]["message"],
            format!("participant {} is listed twice", q("OpsSvc01"))
        );

        let listed =
            json!({"type": "edit-flow", "subject": "Checkout", "set": {"participants": ["Ops"]}});
        let (code, details) = refusal(json!([
            listed,
            {"type": "remove-module", "subject": "Ops"},
        ]));
        assert_eq!(code, Code::ModelInvalid, "{details}");
        let (record, _) = plan_of(json!([listed])).unwrap();
        assert_eq!(
            orders(&record)["flows"][0]["participants"],
            json!([q("OpsSvc01")])
        );
    }

    #[test]
    fn a_message_step_takes_an_anchor_and_clears_it() {
        let anchor = json!({"path": "src/web.ts", "symbol": "ask"});
        let (record, _) = plan_of(json!([
            {"type": "edit-step", "subject": "Step0001", "set": {"anchor": anchor}},
            {"type": "add-step", "flow": "Checkout", "step": {"kind": "message", "from": "Api", "to": "Db", "label": "store", "anchor": anchor}},
        ]))
        .unwrap();
        let steps = &orders(&record)["flows"][0]["steps"];
        assert_eq!(steps[0]["anchor"], anchor);
        assert_eq!(steps[3]["anchor"], anchor);

        let (record, _) = plan_of(json!([
            {"type": "edit-step", "subject": "Step0002", "clear": ["anchor"]},
        ]))
        .unwrap();
        let cleared = &orders(&record)["flows"][0]["steps"][1]["branches"][0]["steps"][0];
        assert_eq!(cleared.get("anchor"), None, "{cleared}");
        let on_loop =
            json!({"type": "edit-step", "subject": "Loop0001", "set": {"anchor": anchor}});
        assert_eq!(refusal(json!([on_loop])).0, Code::RequestInvalid);
        let (code, details) = refusal(json!([
            {"type": "edit-step", "subject": "Step0001", "set": {"anchor": {"path": "../x", "symbol": "a"}}},
        ]));
        assert_eq!(code, Code::ModelInvalid);
        assert_eq!(
            details["problems"][0]["message"],
            "anchor path is not a relative path inside the repository"
        );
    }

    #[test]
    fn a_decision_keeps_two_branches() {
        let (code, details) = refusal(json!([{"type": "remove-branch", "subject": "Branch02"}]));
        assert_eq!(code, Code::ModelInvalid);
        assert_eq!(
            details["problems"],
            json!([{"file": format!("collections/{ORDERS}.collection.json"), "subject": "Decide01",
                    "message": "a decision needs at least 2 branches", "change": 0}])
        );
        let (record, _) = plan_of(json!([
            {"type": "add-branch", "decision": "Decide01", "branch": {"label": "maybe"}},
            {"type": "remove-branch", "subject": q("Branch01")},
        ]))
        .unwrap();
        let branches = &orders(&record)["flows"][0]["steps"][1]["branches"];
        assert_eq!(branches[0]["id"], "Branch02");
        assert_eq!(branches[1]["label"], "maybe");
    }

    #[test]
    fn removing_or_moving_what_a_step_references_refuses() {
        for (changes, steps) in [
            (
                json!([{"type": "remove-module", "subject": "Api"}]),
                vec![q("Step0001"), q("Step0002")],
            ),
            (
                json!([{"type": "remove-relationship", "subject": "Rel00001"}]),
                vec![q("Step0001")],
            ),
            (
                json!([{"type": "edit-relationship", "subject": "Rel00001", "source": "Ops"}]),
                vec![q("Step0001")],
            ),
        ] {
            let (code, details) = refusal(changes.clone());
            assert_eq!(code, Code::ReferencedByFlow, "{changes}");
            assert_eq!(details["steps"], json!(steps), "{changes}");
            assert_eq!(details["change"], 0, "{changes}");
        }
        plan_of(json!([
            {"type": "remove-relationship", "subject": "Rel00001"},
            {"type": "edit-step", "subject": "Step0001", "clear": ["relationship"]},
        ]))
        .unwrap();
        // A step the batch edits is judged by validation, not as a reference the batch broke.
        for changes in [
            json!([{"type": "edit-step", "subject": "Step0001", "set": {"relationship": "Rel00002"}}]),
            json!([
                {"type": "remove-relationship", "subject": "Rel00001"},
                {"type": "edit-step", "subject": "Step0001", "set": {"label": "pay"}},
            ]),
        ] {
            assert_eq!(refusal(changes.clone()).0, Code::ModelInvalid, "{changes}");
        }
    }
}
