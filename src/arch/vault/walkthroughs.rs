//! Walkthroughs in the vault: their validation and their read views. A walkthrough belongs to the
//! current state; its beats may read a proposal or a checkpoint, which change on their own, so
//! only references into the current state must exist.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;

use super::{CURRENT_ONLY, MANIFEST, file_json, revision};
use super::{Contents, Model, Problems, check_file, check_text, pick};
use crate::arch::types::{
    BEATS_MAX, Beat, BeatReading, Collection, DESCRIPTION_MAX, LABEL_MAX, Module, NARRATION_MAX,
    READING_DEPTH_MAX, ReadingState, ReadingView, Walkthrough,
};
use crate::arch::{Refusal, ids, one_line};
use crate::files::hash;

/// The qualified IDs of `modules` and of the flows in `collections`, as readings name them.
pub(crate) fn reading_ids<'a>(
    model: &str,
    modules: impl IntoIterator<Item = &'a Module>,
    collections: impl IntoIterator<Item = &'a Collection>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let modules = (modules.into_iter())
        .map(|m| ids::qualify(model, &m.id))
        .collect();
    let flows = (collections.into_iter())
        .flat_map(|c| &c.flows)
        .map(|f| ids::qualify(model, &f.id))
        .collect();
    (modules, flows)
}

/// The qualified module and flow IDs of the current state, which current-state readings name.
pub(super) struct Known {
    model: String,
    modules: BTreeSet<String>,
    flows: BTreeSet<String>,
}

impl Known {
    /// What the readings of `contents` may name; `None` without the model ID, when references
    /// are not judged.
    pub(super) fn new(model: Option<&str>, contents: Contents<'_>) -> Option<Self> {
        model.map(|model| {
            let modules = contents.modules.iter().map(|(_, m)| *m);
            let collections = contents.collections.iter().map(|(_, c)| *c);
            let (modules, flows) = reading_ids(model, modules, collections);
            Known {
                model: model.to_owned(),
                modules,
                flows,
            }
        })
    }
}

/// Every walkthrough's own fields and beats. References are judged only with the model ID.
pub(super) fn check(model: Option<&str>, contents: Contents<'_>, problems: &mut Problems) {
    let known = Known::new(model, contents);
    for (file, walkthrough) in contents.walkthroughs {
        let id = walkthrough.id.as_str();
        let (version, name) = (walkthrough.schema_version, &walkthrough.name);
        check_file(problems, file, id, version, name);
        let description = walkthrough.description.as_deref();
        let max = Some(DESCRIPTION_MAX);
        check_text(problems, file, id, "description", description, max, true);
        let count = walkthrough.beats.len();
        if count > BEATS_MAX {
            problems.push(
                file,
                id,
                format!("has {count} beats, more than {BEATS_MAX}"),
            );
        }
        for beat in &walkthrough.beats {
            check_beat(file, beat, known.as_ref(), problems);
        }
    }
}

fn check_beat(file: &str, beat: &Beat, known: Option<&Known>, problems: &mut Problems) {
    let id = beat.id.as_str();
    if !one_line(&beat.title, LABEL_MAX) {
        let message = format!("title needs 1 to {LABEL_MAX} characters and no control characters");
        problems.push(file, id, message);
    }
    if beat.narration.trim().is_empty() {
        problems.push(file, id, "narration is empty");
    }
    let narration = Some(beat.narration.as_str());
    check_text(
        problems,
        file,
        id,
        "narration",
        narration,
        Some(NARRATION_MAX),
        true,
    );
    for message in reading_problems(&beat.reading, known) {
        problems.push(file, id, message);
    }
}

/// Why a reading does not hold: fields that do not go together, as `model render` refuses them,
/// a focus listed twice, and references. Into the current state they must exist; into a proposal
/// or checkpoint they must be well formed.
pub(super) fn reading_problems(reading: &BeatReading, known: Option<&Known>) -> Vec<String> {
    let mut out = shape_problems(reading);
    let mut seen = BTreeSet::new();
    for module in &reading.focus {
        if !seen.insert(module) {
            out.push(format!("focus lists {module} more than once"));
        }
    }
    if let Some(known) = known {
        out.extend(reference_problems(reading, known));
    }
    out
}

fn shape_problems(reading: &BeatReading) -> Vec<String> {
    let mut out = Vec::new();
    let view = reading.view.as_str();
    match (reading.view, &reading.flow) {
        (ReadingView::Architecture, Some(_)) => {
            out.push("the architecture view draws no flow".to_owned());
        }
        (ReadingView::Sequence | ReadingView::Flowchart, flow) => {
            if flow.is_none() {
                out.push(format!("the {view} view needs a flow"));
            }
            if reading.scope.is_some() || reading.depth.is_some() {
                out.push("scope and depth apply to the architecture view only".to_owned());
            }
        }
        (ReadingView::Architecture, None) => {}
    }
    if reading
        .depth
        .is_some_and(|d| !(1..=READING_DEPTH_MAX).contains(&d))
    {
        out.push(format!("depth needs 1 to {READING_DEPTH_MAX}"));
    }
    match &reading.against {
        Some(ReadingState::Checkpoint(_)) => out.push(
            "against names a checkpoint; compare with the current state or a proposal".to_owned(),
        ),
        Some(against) if *against == reading.state => {
            out.push("against names the state being drawn".to_owned());
        }
        // `state` is the state drawn, so it must be the newer of the two.
        Some(against)
            if matches!(
                (&reading.state, against),
                (ReadingState::Checkpoint(_), _)
                    | (ReadingState::Current, ReadingState::Proposal(_))
            ) =>
        {
            out.push("put the newer state in state".to_owned());
        }
        _ => {}
    }
    out
}

fn reference_problems(reading: &BeatReading, known: &Known) -> Vec<String> {
    let mut out = Vec::new();
    let qualified = |id: &str| {
        id.split_once('#')
            .is_some_and(|(model, local)| model == known.model && ids::is_local(local))
    };
    for state in std::iter::once(&reading.state).chain(&reading.against) {
        match state {
            ReadingState::Proposal(p) if !qualified(p) => {
                out.push(format!("proposal {p} is not a qualified ID of this model"));
            }
            ReadingState::Checkpoint(c) if !ids::is_local(c) => {
                out.push(format!("checkpoint {c} is not 8 base62 characters"));
            }
            _ => {}
        }
    }
    let current = reading.state.is_current();
    let modules = reading.scope.iter().map(|s| ("scope", s));
    let focus = reading.focus.iter().map(|f| ("focus", f));
    for (field, module) in modules.chain(focus) {
        if current && !known.modules.contains(module) {
            out.push(format!("{field} {module} is not a module"));
        } else if !current && !qualified(module) {
            out.push(format!(
                "{field} {module} is not a qualified ID of this model"
            ));
        }
    }
    if let Some(flow) = &reading.flow {
        if current && !known.flows.contains(flow) {
            out.push(format!("flow {flow} is not a flow"));
        } else if !current && !qualified(flow) {
            out.push(format!("flow {flow} is not a qualified ID of this model"));
        }
    }
    out
}

/// The position among `walkthroughs` of the walkthrough `subject` names: its qualified or local
/// ID, else its unique name.
pub(crate) fn walkthrough_position(
    model: &str,
    walkthroughs: &[Walkthrough],
    subject: &str,
) -> Result<usize, Refusal> {
    let items = (walkthroughs.iter().enumerate())
        .map(|(w, x)| (w, x.id.as_str(), x.name.as_str(), &[][..]));
    pick(model, "walkthrough", items, subject)
}

/// A walkthrough as `model read` lists it, without its beats.
#[derive(Debug, Serialize)]
pub struct WalkthroughEntry<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub beats: usize,
}

/// A walkthrough with its beats, every ID in it qualified.
#[derive(Debug, Serialize)]
pub struct WalkthroughView<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub beats: Vec<BeatView<'a>>,
}

#[derive(Debug, Serialize)]
pub struct BeatView<'a> {
    pub id: String,
    pub title: &'a str,
    pub narration: &'a str,
    pub reading: &'a BeatReading,
}

impl Model {
    /// The digest a proposal drawn from this current state records as its predecessor: the
    /// revision without the walkthrough and picture files, which a proposal neither holds nor
    /// replaces, and with `model.json` in canonical form without its retired IDs, which removing a
    /// beat, walkthrough or picture grows.
    pub fn predecessor_digest(&self) -> Result<String> {
        let mut manifest = self.manifest.clone();
        manifest.retired_ids.clear();
        let canonical = hash(file_json(&manifest)?.as_bytes());
        Ok(self.digest_without_current_only(Some(canonical)))
    }

    /// The predecessor digest drafts drawn before `predecessor_digest` left out retired IDs record:
    /// `model.json` as it is on disk.
    pub fn earlier_predecessor_digest(&self) -> String {
        self.digest_without_current_only(None)
    }

    fn digest_without_current_only(&self, manifest: Option<String>) -> String {
        let own = |path: &str| {
            let folder = path.split_once('/').map(|(f, _)| f);
            folder.is_some_and(|f| CURRENT_ONLY.contains(&f))
        };
        let files = (self.revision.files.iter())
            .filter(|f| !own(&f.path))
            .map(|f| {
                let sha256 = match &manifest {
                    Some(canonical) if f.path == MANIFEST => canonical.clone(),
                    _ => f.sha256.clone(),
                };
                (f.path.clone(), sha256)
            })
            .collect();
        revision::of_digests(files).digest
    }

    /// The walkthroughs in order, without their beats.
    pub fn walkthrough_entries(&self) -> Vec<WalkthroughEntry<'_>> {
        let mut walkthroughs: Vec<&Walkthrough> = self.walkthroughs.iter().collect();
        walkthroughs.sort_by_key(|w| (w.order, &w.name, &w.id));
        (walkthroughs.into_iter())
            .map(|w| WalkthroughEntry {
                id: self.qualify(&w.id),
                name: &w.name,
                order: w.order,
                description: w.description.as_deref(),
                beats: w.beats.len(),
            })
            .collect()
    }

    pub fn walkthrough_view<'a>(&self, walkthrough: &'a Walkthrough) -> WalkthroughView<'a> {
        WalkthroughView {
            id: self.qualify(&walkthrough.id),
            name: &walkthrough.name,
            order: walkthrough.order,
            description: walkthrough.description.as_deref(),
            beats: (walkthrough.beats.iter())
                .map(|b| BeatView {
                    id: self.qualify(&b.id),
                    title: &b.title,
                    narration: &b.narration,
                    reading: &b.reading,
                })
                .collect(),
        }
    }

    /// The walkthrough `subject` names, its qualified or local ID or its unique name.
    pub fn walkthrough(&self, subject: &str) -> Result<&Walkthrough, Refusal> {
        let at = walkthrough_position(&self.manifest.id, &self.walkthroughs, subject)?;
        Ok(&self.walkthroughs[at])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::arch::types::{Collection, Manifest, Module, VaultConfig, Walkthrough};

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "schema_version": 1, "id": M, "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap()
    }

    /// The problems of `walkthrough` alone in a model without modules or flows.
    fn alone(walkthrough: Value) -> Vec<String> {
        let walkthrough = serde_json::from_value(walkthrough).unwrap();
        let found = super::super::problems(
            &manifest(),
            &VaultConfig::default(),
            None,
            &[],
            &[],
            &[walkthrough],
            &[],
        );
        found.into_iter().map(|p| p.message).collect()
    }

    /// The (subject, message) of each problem of one walkthrough holding one beat reading
    /// `reading`, over modules Web and Api and the flow Checkout.
    fn problems_of(reading: Value) -> Vec<(String, String)> {
        let module = |id: &str| -> Module {
            serde_json::from_value(
                json!({"schema_version": 1, "id": id, "name": id, "kind": "service", "order": 0}),
            )
            .unwrap()
        };
        let collection: Collection = serde_json::from_value(json!({
            "schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0,
            "flows": [{"id": "Flow0001", "name": "Checkout", "order": 0}],
        }))
        .unwrap();
        let walkthrough: Walkthrough = serde_json::from_value(json!({
            "schema_version": 1, "id": "Walk0001", "name": "Tour", "order": 0,
            "beats": [{"id": "Beat0001", "title": "Start", "narration": "Here\nwe go", "reading": reading}],
        }))
        .unwrap();
        super::super::problems(
            &manifest(),
            &VaultConfig::default(),
            None,
            &[module("WebApp01"), module("ApiSrv01")],
            &[collection],
            &[walkthrough],
            &[],
        )
        .into_iter()
        .map(|p| {
            assert_eq!(p.file, "walkthroughs/Walk0001.walkthrough.json");
            (p.subject, p.message)
        })
        .collect()
    }

    fn messages(reading: Value) -> Vec<String> {
        problems_of(reading).into_iter().map(|(_, m)| m).collect()
    }

    #[test]
    fn current_readings_name_existing_modules_and_flows() {
        let good = json!({"view": "architecture", "scope": q("WebApp01"), "depth": 3,
            "focus": [q("ApiSrv01"), q("WebApp01")]});
        assert_eq!(problems_of(good), []);
        assert_eq!(
            problems_of(json!({"view": "sequence", "flow": q("Flow0001")})),
            []
        );
        let missing = json!({"view": "sequence", "flow": q("Gone0001"), "focus": [q("Gone0002")]});
        assert_eq!(
            messages(missing),
            [
                format!("focus {} is not a module", q("Gone0002")),
                format!("flow {} is not a flow", q("Gone0001")),
            ]
        );
        let (subject, _) =
            problems_of(json!({"view": "architecture", "scope": q("Gone0001")})).remove(0);
        assert_eq!(subject, "Beat0001");
    }

    #[test]
    fn other_states_need_well_formed_ids_only() {
        let elsewhere = json!({"view": "flowchart", "state": {"proposal": q("Propose1")},
            "flow": q("Gone0001"), "focus": [q("Gone0002")]});
        assert_eq!(problems_of(elsewhere), []);
        let checkpoint = json!({"view": "architecture", "state": {"checkpoint": "Check001"},
            "scope": q("Gone0001")});
        assert_eq!(problems_of(checkpoint), []);
        let bad = json!({"view": "architecture", "state": {"proposal": "Other001#Propose1"},
            "scope": "Gone0001", "against": {"checkpoint": "abc"}});
        assert_eq!(
            messages(bad),
            [
                "against names a checkpoint; compare with the current state or a proposal",
                "proposal Other001#Propose1 is not a qualified ID of this model",
                "checkpoint abc is not 8 base62 characters",
                "scope Gone0001 is not a qualified ID of this model",
            ]
        );
    }

    #[test]
    fn readings_keep_the_render_rules() {
        assert_eq!(
            messages(json!({"view": "sequence", "depth": 10})),
            [
                "the sequence view needs a flow",
                "scope and depth apply to the architecture view only",
                "depth needs 1 to 9",
            ]
        );
        assert_eq!(
            messages(json!({"view": "architecture", "flow": q("Flow0001"),
                "against": "current", "focus": [q("WebApp01"), q("WebApp01")]})),
            [
                "the architecture view draws no flow".to_owned(),
                "against names the state being drawn".to_owned(),
                format!("focus lists {} more than once", q("WebApp01")),
            ]
        );
        // `state` is the state drawn: the newer of the two.
        let newer = "put the newer state in state";
        for (state, against) in [
            (json!("current"), json!({"proposal": q("Propose1")})),
            (json!({"checkpoint": "Check001"}), json!("current")),
            (
                json!({"checkpoint": "Check001"}),
                json!({"proposal": q("Propose1")}),
            ),
        ] {
            let reading = json!({"view": "architecture", "state": state, "against": against});
            assert_eq!(messages(reading), [newer]);
        }
        let older = json!({"view": "architecture", "state": {"proposal": q("Propose1")},
            "against": "current"});
        assert_eq!(problems_of(older), []);
    }

    #[test]
    fn beats_keep_their_text_and_count_within_limits() {
        let mut walkthrough: Value = json!({
            "schema_version": 1, "id": "Walk0001", "name": "Tour", "order": 0,
            "description": "two\nlines are fine",
            "beats": [{"id": "Beat0001", "title": "two\nlines", "narration": " \n ",
                       "reading": {"view": "architecture"}}],
        });
        assert_eq!(
            alone(walkthrough.clone()),
            [
                "title needs 1 to 60 characters and no control characters",
                "narration is empty"
            ]
        );
        let beat = walkthrough["beats"][0].clone();
        let beats: Vec<Value> = (0..=crate::arch::types::BEATS_MAX)
            .map(|n| {
                let mut b = beat.clone();
                b["id"] = format!("B{n:07}").into();
                b["title"] = "ok".into();
                b["narration"] = "ok".into();
                b
            })
            .collect();
        walkthrough["beats"] = beats.into();
        assert_eq!(alone(walkthrough), ["has 201 beats, more than 200"]);
    }
}
