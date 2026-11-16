//! The walkthrough changes of a `model edit` batch: walkthroughs and their beats, and what the
//! batch changed in them. Walkthroughs belong to the current state; a proposal edit refuses them.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::flow::insert;
use super::{Batch, Diff, at, both, request_invalid};
use crate::arch::types::{
    Beat, BeatReading, ReadingState, ReadingView, SCHEMA_VERSION, Walkthrough,
};
use crate::arch::vault::{self, Model};
use crate::arch::{Code, Refusal, ids};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWalkthrough {
    pub name: String,
    pub order: u32,
    pub description: Option<String>,
    #[serde(default)]
    pub beats: Vec<NewBeat>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalkthroughSet {
    pub name: Option<String>,
    pub order: Option<u32>,
    pub description: Option<String>,
}

/// A walkthrough field `clear` may empty; required fields cannot be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalkthroughField {
    Description,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewBeat {
    pub title: String,
    pub narration: String,
    pub reading: NewReading,
}

/// A reading to store. In the current state, modules are referenced like module subjects and the
/// flow by its ID or unique name; in a proposal or checkpoint, by qualified or local ID. A
/// proposal is named by its qualified or local ID.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewReading {
    pub view: ReadingView,
    #[serde(default)]
    pub state: ReadingState,
    pub against: Option<ReadingState>,
    pub scope: Option<String>,
    pub depth: Option<u32>,
    pub flow: Option<String>,
    /// Kept in order, each module once.
    #[serde(default)]
    pub focus: Vec<String>,
}

/// New values for a beat and its reading; references resolve in the state the beat reads after
/// the change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeatSet {
    pub title: Option<String>,
    pub narration: Option<String>,
    pub view: Option<ReadingView>,
    pub state: Option<ReadingState>,
    pub against: Option<ReadingState>,
    pub scope: Option<String>,
    pub depth: Option<u32>,
    pub flow: Option<String>,
    pub focus: Option<Vec<String>>,
}

/// A reading field `clear` empties; clearing `state` reads the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BeatField {
    State,
    Against,
    Scope,
    Depth,
    Flow,
    Focus,
}

impl WalkthroughField {
    fn is_set(self, set: &WalkthroughSet) -> bool {
        match self {
            Self::Description => set.description.is_some(),
        }
    }
}

impl BeatField {
    pub(super) fn is_set(self, set: &BeatSet) -> bool {
        match self {
            Self::State => set.state.is_some(),
            Self::Against => set.against.is_some(),
            Self::Scope => set.scope.is_some(),
            Self::Depth => set.depth.is_some(),
            Self::Flow => set.flow.is_some(),
            Self::Focus => set.focus.is_some(),
        }
    }
}

impl Batch<'_> {
    /// Applies the walkthrough change `change`, refusing it in a proposal, which holds none.
    pub(super) fn apply_walkthrough(
        &mut self,
        index: usize,
        change: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.apply_current(index, "walkthroughs", change)
    }

    /// Applies `change` to what only the current state holds, `what`, refusing it in a proposal.
    pub(super) fn apply_current(
        &mut self,
        index: usize,
        what: &str,
        change: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        if self.in_proposal {
            let message =
                format!("{what} belong to the current state; edit them without --proposal");
            return Err(request_invalid(index, message));
        }
        change(self)
    }

    fn walkthrough_at(&self, index: usize, subject: &str) -> Result<usize> {
        let found = vault::walkthrough_position(self.model, &self.walkthroughs, subject);
        Ok(found.map_err(at(index))?)
    }

    /// The positions of the walkthrough holding the beat `subject` names and of the beat in it.
    fn beat_at(&self, index: usize, subject: &str) -> Result<(usize, usize)> {
        let id = ids::local(self.model, subject);
        (self.walkthroughs.iter().enumerate())
            .find_map(|(w, walkthrough)| {
                let b = walkthrough.beats.iter().position(|b| b.id == id)?;
                Some((w, b))
            })
            .ok_or_else(|| {
                let message = format!("no beat has the ID `{subject}`");
                at(index)(Refusal::new(Code::SubjectNotFound, message)).into()
            })
    }

    /// `subject` qualified with the model ID, without checking it names anything.
    fn qualified(&self, subject: &str) -> String {
        ids::qualify(self.model, ids::local(self.model, subject))
    }

    fn reading_state(&self, state: ReadingState) -> ReadingState {
        match state {
            ReadingState::Proposal(p) => ReadingState::Proposal(self.qualified(&p)),
            other => other,
        }
    }

    /// A module reference: resolved in the batch for the current state, else qualified.
    fn reading_module(&self, index: usize, current: bool, subject: &str) -> Result<String> {
        if current {
            self.module_id(index, subject)
        } else {
            Ok(self.qualified(subject))
        }
    }

    /// A flow reference: resolved in the batch for the current state, else qualified.
    fn reading_flow(&self, index: usize, current: bool, subject: &str) -> Result<String> {
        if !current {
            return Ok(self.qualified(subject));
        }
        let (c, f) = self.flow_at(index, subject)?;
        Ok(ids::qualify(self.model, &self.collections[c].flows[f].id))
    }

    /// The focus modules resolved, in order, each once.
    fn reading_focus(&self, index: usize, current: bool, focus: &[String]) -> Result<Vec<String>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for subject in focus {
            let id = self.reading_module(index, current, subject)?;
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        Ok(out)
    }

    pub(super) fn build_reading(&self, index: usize, new: NewReading) -> Result<BeatReading> {
        let state = self.reading_state(new.state);
        let current = state.is_current();
        let module = |s: String| self.reading_module(index, current, &s);
        let flow = |s: String| self.reading_flow(index, current, &s);
        Ok(BeatReading {
            view: new.view,
            against: new.against.map(|a| self.reading_state(a)),
            scope: new.scope.map(module).transpose()?,
            depth: new.depth,
            flow: new.flow.map(flow).transpose()?,
            focus: self.reading_focus(index, current, &new.focus)?,
            state,
        })
    }

    fn build_beat(&mut self, index: usize, new: NewBeat) -> Result<Beat> {
        let reading = self.build_reading(index, new.reading)?;
        Ok(Beat {
            id: self.new_id(index, format!("beat {}", new.title))?,
            title: new.title,
            narration: new.narration,
            reading,
        })
    }

    pub(super) fn add_walkthrough(&mut self, index: usize, new: NewWalkthrough) -> Result<()> {
        let id = self.new_id(index, new.name.clone())?;
        let beats = (new.beats.into_iter())
            .map(|beat| self.build_beat(index, beat))
            .collect::<Result<_>>()?;
        self.walkthroughs.push(Walkthrough {
            schema_version: SCHEMA_VERSION,
            id,
            name: new.name,
            order: new.order,
            description: new.description,
            beats,
        });
        Ok(())
    }

    pub(super) fn edit_walkthrough(
        &mut self,
        index: usize,
        subject: &str,
        set: WalkthroughSet,
        clear: &[WalkthroughField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let w = self.walkthrough_at(index, subject)?;
        let id = self.walkthroughs[w].id.clone();
        self.edit_once(index, &id)?;
        let walkthrough = &mut self.walkthroughs[w];
        if let Some(name) = set.name {
            walkthrough.name = name;
        }
        if let Some(order) = set.order {
            walkthrough.order = order;
        }
        if set.description.is_some() {
            walkthrough.description = set.description;
        }
        if clear.contains(&WalkthroughField::Description) {
            walkthrough.description = None;
        }
        self.touched.insert(id, index);
        Ok(())
    }

    pub(super) fn remove_walkthrough(&mut self, index: usize, subject: &str) -> Result<()> {
        let w = self.walkthrough_at(index, subject)?;
        let removed = self.walkthroughs.remove(w);
        self.touched.insert(removed.id, index);
        Ok(())
    }

    pub(super) fn add_beat(
        &mut self,
        index: usize,
        walkthrough: &str,
        position: Option<usize>,
        new: NewBeat,
    ) -> Result<()> {
        let w = self.walkthrough_at(index, walkthrough)?;
        let beat = self.build_beat(index, new)?;
        self.place_beat(index, w, position, beat)
    }

    /// Puts `beat` at `position` of the beats of the walkthrough at `w`, the end when absent.
    fn place_beat(
        &mut self,
        index: usize,
        w: usize,
        position: Option<usize>,
        beat: Beat,
    ) -> Result<()> {
        let walkthrough = &mut self.walkthroughs[w];
        self.touched.insert(walkthrough.id.clone(), index);
        self.touched.insert(beat.id.clone(), index);
        insert(index, &mut walkthrough.beats, position, beat)
    }

    /// Removes the beat `subject` names from its walkthrough.
    pub(super) fn take_beat(&mut self, index: usize, subject: &str) -> Result<Beat> {
        let (w, b) = self.beat_at(index, subject)?;
        let walkthrough = &mut self.walkthroughs[w];
        let beat = walkthrough.beats.remove(b);
        self.touched.insert(walkthrough.id.clone(), index);
        self.touched.insert(beat.id.clone(), index);
        Ok(beat)
    }

    pub(super) fn move_beat(
        &mut self,
        index: usize,
        subject: &str,
        walkthrough: &str,
        position: Option<usize>,
    ) -> Result<()> {
        let beat = self.take_beat(index, subject)?;
        let w = self.walkthrough_at(index, walkthrough)?;
        self.place_beat(index, w, position, beat)
    }

    pub(super) fn edit_beat(
        &mut self,
        index: usize,
        subject: &str,
        set: BeatSet,
        clear: &[BeatField],
    ) -> Result<()> {
        if let Some(field) = clear.iter().find(|f| f.is_set(&set)) {
            return Err(both(index, field));
        }
        let (w, b) = self.beat_at(index, subject)?;
        let id = self.walkthroughs[w].beats[b].id.clone();
        self.edit_once(index, &id)?;
        let mut reading = self.walkthroughs[w].beats[b].reading.clone();
        self.apply_reading(index, &mut reading, &set, clear)?;
        let beat = &mut self.walkthroughs[w].beats[b];
        if let Some(title) = set.title {
            beat.title = title;
        }
        if let Some(narration) = set.narration {
            beat.narration = narration;
        }
        beat.reading = reading;
        self.touched.insert(self.walkthroughs[w].id.clone(), index);
        self.touched.insert(id, index);
        Ok(())
    }

    /// Sets and clears the reading's fields. References already stored are kept as they are, so a
    /// reference the batch broke in an edited beat fails validation, naming this change.
    pub(super) fn apply_reading(
        &self,
        index: usize,
        reading: &mut BeatReading,
        set: &BeatSet,
        clear: &[BeatField],
    ) -> Result<()> {
        if let Some(state) = &set.state {
            reading.state = self.reading_state(state.clone());
        }
        if clear.contains(&BeatField::State) {
            reading.state = ReadingState::Current;
        }
        let current = reading.state.is_current();
        if let Some(view) = set.view {
            reading.view = view;
        }
        if let Some(against) = &set.against {
            reading.against = Some(self.reading_state(against.clone()));
        }
        if let Some(scope) = &set.scope {
            reading.scope = Some(self.reading_module(index, current, scope)?);
        }
        if set.depth.is_some() {
            reading.depth = set.depth;
        }
        if let Some(flow) = &set.flow {
            reading.flow = Some(self.reading_flow(index, current, flow)?);
        }
        if let Some(focus) = &set.focus {
            reading.focus = self.reading_focus(index, current, focus)?;
        }
        for field in clear {
            match field {
                BeatField::State => {}
                BeatField::Against => reading.against = None,
                BeatField::Scope => reading.scope = None,
                BeatField::Depth => reading.depth = None,
                BeatField::Flow => reading.flow = None,
                BeatField::Focus => reading.focus.clear(),
            }
        }
        Ok(())
    }

    /// Refuses when a current-state beat the batch did not edit names a module or flow the batch
    /// removed. Stored references held before the batch, so whatever no longer holds, it broke.
    /// A beat the batch edited is judged by validation instead.
    pub(super) fn refuse_walkthrough_references(&self) -> Result<()> {
        let beats = self.walkthroughs.iter().flat_map(|w| &w.beats);
        let readings = beats.map(|b| (b.id.as_str(), &b.reading));
        let refused = Refused {
            code: Code::ReferencedByWalkthrough,
            what: "walkthrough beat(s)",
            key: "beats",
        };
        self.refuse_reading_references(readings, &refused)
    }

    /// Refuses `refused.code` when a current-state reading among `readings` (by the local ID of
    /// what holds it) that the batch did not edit names a module or flow the batch removed,
    /// listing the holders in `details[refused.key]`.
    pub(super) fn refuse_reading_references<'r>(
        &self,
        readings: impl Iterator<Item = (&'r str, &'r BeatReading)>,
        refused: &Refused,
    ) -> Result<()> {
        let mut readings = readings.peekable();
        if readings.peek().is_none() {
            return Ok(());
        }
        let model = self.model;
        let (modules, flows) = vault::reading_ids(model, &self.modules, &self.collections);
        let mut causes: BTreeSet<usize> = BTreeSet::new();
        let mut holders = Vec::new();
        for (id, reading) in readings {
            if !reading.state.is_current() || self.edited.contains(id) {
                continue;
            }
            let named = reading.scope.iter().chain(&reading.focus);
            let broken: Vec<&String> = (named.filter(|m| !modules.contains(*m)))
                .chain(reading.flow.iter().filter(|f| !flows.contains(*f)))
                .collect();
            if broken.is_empty() {
                continue;
            }
            let cause = |s: &&String| self.touched.get(ids::local(model, s)).copied();
            causes.extend(broken.iter().filter_map(cause));
            holders.push(ids::qualify(model, id));
        }
        if holders.is_empty() {
            return Ok(());
        }
        let refusal = Refusal::new(
            refused.code,
            format!(
                "{} {} read modules or flows the batch removes; edit or remove them in the same batch",
                holders.len(),
                refused.what,
            ),
        )
        .with_details(json!({ refused.key: holders }));
        Err(match causes.into_iter().collect::<Vec<_>>().as_slice() {
            [index] => at(*index)(refusal),
            _ => refusal,
        }
        .into())
    }
}

/// How `refuse_reading_references` refuses: its code, what the count counts, and the details key
/// listing them.
pub(super) struct Refused {
    pub(super) code: Code,
    pub(super) what: &'static str,
    pub(super) key: &'static str,
}

/// What the batch diff needs of the files only the current state holds: walkthroughs and
/// pictures.
pub(super) trait CurrentOnly: PartialEq {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    /// Its own ID and those of the items inside it.
    fn ids(&self) -> Vec<&str>;
    /// Its vault-relative file.
    fn file(id: &str) -> String;
}

impl CurrentOnly for Walkthrough {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn ids(&self) -> Vec<&str> {
        Walkthrough::ids(self).collect()
    }
    fn file(id: &str) -> String {
        vault::walkthrough_file(id)
    }
}

/// Adds to `diff` what the batch changed from `old` to `new`, walkthroughs or pictures: files to
/// remove, subjects and retired IDs, and returns the files to write. It adds to `touched` the
/// subjects a claim must cover: one that changes, what is inside it included, the root level when
/// one comes or goes, and every one whose name an added or renamed one takes.
pub(super) fn diff_current_only<'a, T: CurrentOnly>(
    model: &Model,
    old: &'a [T],
    new: &'a [T],
    diff: &mut Diff<'_>,
    touched: &mut Vec<Option<String>>,
) -> Vec<&'a T> {
    let by_id =
        |list: &'a [T]| -> BTreeMap<&'a str, &'a T> { list.iter().map(|w| (w.id(), w)).collect() };
    let (old, now) = (by_id(old), by_id(new));
    let live: BTreeSet<&str> = new.iter().flat_map(T::ids).collect();
    let mut write = Vec::new();
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
        match (before, after) {
            (_, Some(after)) => write.push(*after),
            (Some(_), None) => diff.remove.push(T::file(id)),
            (None, None) => {}
        }
        if before.is_none() || after.is_none() {
            touched.push(None);
        }
        if let Some(gone) = before {
            let retired = gone.ids().into_iter().filter(|i| !live.contains(i));
            diff.retired.extend(retired.map(str::to_owned));
        }
        // A name an added or renamed one takes would stop naming another alone.
        if let Some(named) = after.filter(|a| before.is_none_or(|b| b.name() != a.name())) {
            let same = old.values().filter(|w| w.name() == named.name());
            touched.extend(same.map(|w| Some(model.qualify(w.id()))));
        }
    }
    write
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::{Batch, Record, plan};
    use super::BeatField;
    use crate::arch::types::{Collection, Manifest, ReadingState, Walkthrough};
    use crate::arch::vault::Model;
    use crate::arch::{Code, Refusal, revision};

    const M: &str = "Model001";
    const TOUR: &str = "Walk0001";
    const OTHER: &str = "Walk0002";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    /// Web and Api; Orders holds Checkout. Tour has beats `Beat0001` (Web and Api, focus Api)
    /// and `Beat0002` (Checkout as a sequence); Other is empty.
    fn model() -> Model {
        let module = |id: &str, name: &str| {
            serde_json::from_value(
                json!({"schema_version": 1, "id": id, "name": name, "kind": "service", "order": 0}),
            )
            .unwrap()
        };
        let collections: Vec<Collection> = serde_json::from_value(json!([
            {"schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0,
             "flows": [{"id": "Flow0001", "name": "Checkout", "order": 0}]},
        ]))
        .unwrap();
        let walkthroughs: Vec<Walkthrough> = serde_json::from_value(json!([
            {"schema_version": 1, "id": TOUR, "name": "Tour", "order": 0, "beats": [
                {"id": "Beat0001", "title": "Shape", "narration": "Two services.",
                 "reading": {"view": "architecture", "focus": [q("ApiSrv01")]}},
                {"id": "Beat0002", "title": "Buying", "narration": "A purchase.",
                 "reading": {"view": "sequence", "flow": q("Flow0001")}},
            ]},
            {"schema_version": 1, "id": OTHER, "name": "Other", "order": 1},
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
            config: Default::default(),
            modules: vec![module("WebApp01", "Web"), module("ApiSrv01", "Api")],
            collections,
            walkthroughs,
            pictures: Vec::new(),
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
        plan(&model(), request, "testing walkthroughs", String::new()).map(|(r, a, _)| (r, a))
    }

    fn refusal(changes: Value) -> (Code, Value) {
        let Err(error) = plan_of(changes) else {
            panic!("the plan succeeded");
        };
        let refusal = error.downcast_ref::<Refusal>().unwrap();
        (refusal.code, refusal.details.clone().unwrap_or_default())
    }

    fn walkthrough(record: &Record, local: &str) -> Option<Value> {
        let path = format!("walkthroughs/{local}.walkthrough.json");
        let writes = record.writes.as_deref().unwrap();
        let write = writes.iter().find(|w| w.path == path)?;
        write
            .content
            .as_deref()
            .map(|c| serde_json::from_str(c).unwrap())
    }

    fn beat_ids(walkthrough: &Value) -> Vec<&str> {
        let beats = walkthrough["beats"].as_array().unwrap();
        beats.iter().map(|b| b["id"].as_str().unwrap()).collect()
    }

    fn qualified(locals: &[&str]) -> BTreeSet<String> {
        locals.iter().map(|id| q(id)).collect()
    }

    #[test]
    fn adding_a_walkthrough_with_beats_touches_only_the_root_level() {
        let (record, affected) = plan_of(json!([
            {"type": "add-walkthrough", "walkthrough": {"name": "Intro", "order": 2, "beats": [
                {"title": "Web", "narration": "The shop front.",
                 "reading": {"view": "architecture", "scope": "Web", "focus": ["Api", "Web", "Api"]}},
                {"title": "Next", "narration": "What changes.",
                 "reading": {"view": "flowchart", "state": {"proposal": "Propose1"},
                             "flow": "Flow0009", "against": "current"}},
            ]}},
        ]))
        .unwrap();
        let receipt = &record.output;
        let names: Vec<&str> = receipt.allocated.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Intro", "beat Web", "beat Next"]);
        assert_eq!(affected, BTreeSet::from([M.to_owned()]));
        assert_eq!(receipt.subjects.added, [receipt.allocated[0].id.clone()]);
        let local = receipt.allocated[0].id.split_once('#').unwrap().1;
        let written = walkthrough(&record, local).unwrap();
        assert_eq!(
            written["beats"][0]["reading"],
            json!({"view": "architecture", "scope": q("WebApp01"), "focus": [q("ApiSrv01"), q("WebApp01")]})
        );
        assert_eq!(
            written["beats"][1]["reading"],
            json!({"view": "flowchart", "state": {"proposal": q("Propose1")},
                   "against": "current", "flow": q("Flow0009")})
        );
    }

    #[test]
    fn each_change_touches_what_a_claim_must_cover() {
        let cases: [(Value, &[&str]); 5] = [
            (
                json!({"type": "edit-walkthrough", "subject": "Tour", "set": {"name": "Guide"}, "clear": ["description"]}),
                &[TOUR],
            ),
            (
                json!({"type": "edit-beat", "subject": q("Beat0001"), "set": {"title": "Overview", "depth": 1}, "clear": ["focus"]}),
                &[TOUR],
            ),
            (
                json!({"type": "add-beat", "walkthrough": OTHER, "position": 0, "beat":
                    {"title": "Only", "narration": "One beat.", "reading": {"view": "architecture"}}}),
                &[OTHER],
            ),
            (
                json!({"type": "move-beat", "subject": "Beat0002", "walkthrough": "Other"}),
                &[TOUR, OTHER],
            ),
            (
                json!({"type": "remove-beat", "subject": "Beat0001"}),
                &[TOUR],
            ),
        ];
        for (change, touched) in cases {
            let (record, affected) = plan_of(json!([change])).unwrap();
            let changed: BTreeSet<String> =
                record.output.subjects.changed.iter().cloned().collect();
            assert_eq!(changed, qualified(touched), "{change}");
            assert_eq!(affected, qualified(touched), "{change}");
        }
    }

    #[test]
    fn beats_move_within_and_across_walkthroughs() {
        let (record, _) =
            plan_of(json!([{"type": "move-beat", "subject": "Beat0002", "walkthrough": TOUR, "position": 0}]))
                .unwrap();
        assert_eq!(
            beat_ids(&walkthrough(&record, TOUR).unwrap()),
            ["Beat0002", "Beat0001"]
        );
        let (record, _) =
            plan_of(json!([{"type": "move-beat", "subject": "Beat0001", "walkthrough": OTHER}]))
                .unwrap();
        assert_eq!(beat_ids(&walkthrough(&record, TOUR).unwrap()), ["Beat0002"]);
        assert_eq!(
            beat_ids(&walkthrough(&record, OTHER).unwrap()),
            ["Beat0001"]
        );
        let past_end = json!([{"type": "move-beat", "subject": "Beat0001", "walkthrough": OTHER, "position": 1}]);
        assert_eq!(refusal(past_end).0, Code::RequestInvalid);
    }

    #[test]
    fn removing_a_walkthrough_retires_its_beats() {
        let (record, affected) =
            plan_of(json!([{"type": "remove-walkthrough", "subject": "Tour"}])).unwrap();
        assert_eq!(record.output.subjects.removed, [q(TOUR)]);
        let writes = record.writes.as_deref().unwrap();
        let manifest: Manifest = serde_json::from_str(
            writes
                .iter()
                .find(|w| w.path == "model.json")
                .unwrap()
                .content
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        let mut retired = manifest.retired_ids;
        retired.sort();
        assert_eq!(retired, ["Beat0001", "Beat0002", TOUR]);
        assert!(writes.iter().any(
            |w| w.path == format!("walkthroughs/{TOUR}.walkthrough.json") && w.content.is_none()
        ));
        assert_eq!(affected, BTreeSet::from([M.to_owned(), q(TOUR)]));
    }

    #[test]
    fn edit_beat_resolves_only_what_it_sets() {
        let model = model();
        let mut batch = Batch::new(&model).unwrap();
        let set =
            serde_json::from_value(json!({"state": {"proposal": "Propose1"}, "scope": "Gone0001"}))
                .unwrap();
        batch.edit_beat(0, "Beat0001", set, &[]).unwrap();
        let reading = &batch.walkthroughs[0].beats[0].reading;
        assert_eq!(reading.scope.as_deref(), Some(q("Gone0001").as_str()));
        assert_eq!(reading.focus, [q("ApiSrv01")]);
        let twice = json!([
            {"type": "edit-beat", "subject": "Beat0001", "set": {"title": "A"}},
            {"type": "edit-beat", "subject": "Beat0001", "set": {"title": "B"}},
        ]);
        assert_eq!(refusal(twice).0, Code::RequestInvalid);
        let both = json!([{"type": "edit-beat", "subject": "Beat0001", "set": {"focus": []}, "clear": ["focus"]}]);
        assert_eq!(refusal(both).0, Code::RequestInvalid);
        let unknown =
            json!([{"type": "edit-beat", "subject": "Beat0001", "set": {"scope": "Nowhere"}}]);
        assert_eq!(refusal(unknown).0, Code::SubjectNotFound);

        // Clearing the state reads the current state, where names resolve.
        let mut model = model;
        model.walkthroughs[0].beats[1].reading.state = ReadingState::Proposal(q("Propose1"));
        let mut batch = Batch::new(&model).unwrap();
        let set = serde_json::from_value(json!({"flow": "Checkout"})).unwrap();
        batch
            .edit_beat(0, "Beat0002", set, &[BeatField::State])
            .unwrap();
        let reading = &batch.walkthroughs[0].beats[1].reading;
        assert_eq!(reading.state, ReadingState::Current);
        assert_eq!(reading.flow.as_deref(), Some(q("Flow0001").as_str()));
    }

    #[test]
    fn removing_what_a_beat_reads_refuses() {
        for (changes, beats) in [
            (
                json!([{"type": "remove-module", "subject": "Api"}]),
                vec![q("Beat0001")],
            ),
            (
                json!([{"type": "remove-flow", "subject": "Checkout"}]),
                vec![q("Beat0002")],
            ),
            (
                json!([{"type": "remove-collection", "subject": "Orders"}]),
                vec![q("Beat0002")],
            ),
        ] {
            let (code, details) = refusal(changes.clone());
            assert_eq!(code, Code::ReferencedByWalkthrough, "{changes}");
            assert_eq!(details["beats"], json!(beats), "{changes}");
            assert_eq!(details["change"], 0, "{changes}");
        }
        plan_of(json!([
            {"type": "edit-beat", "subject": "Beat0001", "clear": ["focus"]},
            {"type": "remove-module", "subject": "Api"},
        ]))
        .unwrap();
        // A beat the batch edits is judged by validation, not as a reference the batch broke.
        let edited = json!([
            {"type": "remove-module", "subject": "Api"},
            {"type": "edit-beat", "subject": "Beat0001", "set": {"title": "Shape"}},
        ]);
        assert_eq!(refusal(edited).0, Code::ModelInvalid);
    }
}
