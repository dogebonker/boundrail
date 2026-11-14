//! Flow collections in the vault: their validation and their read views.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::json;

use super::{Model, Problems, check_file, check_name, check_text};
use crate::arch::types::{
    BRANCHES_MIN, Collection, DESCRIPTION_MAX, FLOW_STEPS_MAX, Flow, LABEL_MAX, Module,
    NESTING_MAX, REPEAT_MIN, SUMMARY_MAX, Step, walk,
};
use crate::arch::{Code, Refusal, ids, one_line};

/// The modules of a model and the source and target of each relationship, by qualified ID: what
/// a message step may reference.
pub(crate) struct Ends {
    modules: BTreeSet<String>,
    relationships: BTreeMap<String, (String, String)>,
}

impl Ends {
    pub(crate) fn new<'a>(model: &str, modules: impl IntoIterator<Item = &'a Module>) -> Self {
        let mut ends = Self {
            modules: BTreeSet::new(),
            relationships: BTreeMap::new(),
        };
        for module in modules {
            let from = ids::qualify(model, &module.id);
            for r in &module.outgoing {
                let id = ids::qualify(model, &r.id);
                ends.relationships.insert(id, (from.clone(), r.to.clone()));
            }
            ends.modules.insert(from);
        }
        ends
    }
}

/// Why a message step's references do not hold in `ends`: a module or relationship that does not
/// exist, or a relationship between other modules than the step's.
pub(crate) fn message_problems(ends: &Ends, step: &Step) -> Vec<String> {
    let Step::Message {
        from,
        to,
        relationship,
        reply,
        ..
    } = step
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (field, module) in [("from", from), ("to", to)] {
        if !ends.modules.contains(module) {
            out.push(format!("{field} {module} is not a module"));
        }
    }
    let Some(r) = relationship else {
        return out;
    };
    match ends.relationships.get(r) {
        None => out.push(format!("relationship {r} is not a relationship")),
        Some((source, target)) => {
            let (source_wanted, target_wanted) = if *reply { (to, from) } else { (from, to) };
            if (source, target) != (source_wanted, target_wanted) {
                out.push(format!(
                    "relationship {r} runs from {source} to {target}, not from {source_wanted} to {target_wanted}"
                ));
            }
        }
    }
    out
}

/// Every collection's own fields, flows and steps. References are judged only with the model ID.
pub(super) fn check(
    model: Option<&str>,
    modules: &[(&str, &Module)],
    collections: &[(&str, &Collection)],
    problems: &mut Problems,
) {
    let ends = model.map(|m| Ends::new(m, modules.iter().map(|(_, module)| *module)));
    for (file, collection) in collections {
        let id = collection.id.as_str();
        check_file(
            problems,
            file,
            id,
            collection.schema_version,
            &collection.name,
        );
        let description = collection.description.as_deref();
        check_text(
            problems,
            file,
            id,
            "description",
            description,
            Some(DESCRIPTION_MAX),
            true,
        );
        check_former_names(problems, file, id, &collection.former_names);
        for flow in &collection.flows {
            check_flow(file, flow, ends.as_ref(), problems);
        }
    }
}

fn check_flow(file: &str, flow: &Flow, ends: Option<&Ends>, problems: &mut Problems) {
    let id = flow.id.as_str();
    check_name(problems, file, id, &flow.name);
    if let Some(summary) = &flow.summary
        && !one_line(summary, SUMMARY_MAX)
    {
        let message = format!("summary needs one line of 1 to {SUMMARY_MAX} characters");
        problems.push(file, id, message);
    }
    let description = flow.description.as_deref();
    check_text(
        problems,
        file,
        id,
        "description",
        description,
        Some(DESCRIPTION_MAX),
        true,
    );
    check_former_names(problems, file, id, &flow.former_names);
    let steps = walk(&flow.steps);
    if steps.len() > FLOW_STEPS_MAX {
        let message = format!("has {} steps, more than {FLOW_STEPS_MAX}", steps.len());
        problems.push(file, id, message);
    }
    for (step, depth) in steps {
        check_step(file, step, depth, ends, problems);
    }
}

fn check_step(file: &str, step: &Step, depth: usize, ends: Option<&Ends>, problems: &mut Problems) {
    let id = step.id();
    check_label(problems, file, id, step.label());
    match step {
        Step::Message { repeat, note, .. } => {
            if repeat.is_some_and(|n| n < REPEAT_MIN) {
                problems.push(file, id, format!("repeat needs at least {REPEAT_MIN}"));
            }
            let note = note.as_deref();
            check_text(
                problems,
                file,
                id,
                "note",
                note,
                Some(DESCRIPTION_MAX),
                true,
            );
            for message in ends.map(|e| message_problems(e, step)).unwrap_or_default() {
                problems.push(file, id, message);
            }
            return;
        }
        Step::Decision { branches, .. } => {
            if branches.len() < BRANCHES_MIN {
                let message = format!("a decision needs at least {BRANCHES_MIN} branches");
                problems.push(file, id, message);
            }
            for branch in branches {
                check_label(problems, file, &branch.id, &branch.label);
            }
        }
        Step::Loop { .. } => {}
    }
    if depth >= NESTING_MAX {
        let message = format!("nests more than {NESTING_MAX} decisions and loops");
        problems.push(file, id, message);
    }
}

/// Former names follow the module's rule: one line without control characters.
fn check_former_names(problems: &mut Problems, file: &str, subject: &str, names: &[String]) {
    for name in names {
        check_text(
            problems,
            file,
            subject,
            "former name",
            Some(name),
            None,
            false,
        );
    }
}

fn check_label(problems: &mut Problems, file: &str, subject: &str, label: &str) {
    if !one_line(label, LABEL_MAX) {
        let message = format!("label needs 1 to {LABEL_MAX} characters and no control characters");
        problems.push(file, subject, message);
    }
}

pub(crate) fn holds_name(current: &str, former_names: &[String], name: &str) -> bool {
    current == name || former_names.iter().any(|n| n == name)
}

/// What `items` (key, local ID, name, former names) holds for the one item `subject` names: its
/// qualified or local ID, else its unique current or former name. `what` names the kind of item
/// in a refusal.
fn pick<'a, K: Copy>(
    model: &str,
    what: &str,
    items: impl Iterator<Item = (K, &'a str, &'a str, &'a [String])> + Clone,
    subject: &str,
) -> Result<K, Refusal> {
    let local = ids::local(model, subject);
    let mut found: Vec<(K, &str)> = items
        .clone()
        .filter(|(_, id, _, _)| *id == local)
        .map(|(key, id, _, _)| (key, id))
        .collect();
    if found.is_empty() {
        found = items
            .filter(|(_, _, name, former)| holds_name(name, former, subject))
            .map(|(key, id, _, _)| (key, id))
            .collect();
    }
    match found.as_slice() {
        [(key, _)] => Ok(*key),
        [] => Err(Refusal::new(
            Code::SubjectNotFound,
            format!("no {what} has the ID or name `{subject}`"),
        )),
        _ => {
            let listed: Vec<String> = found
                .iter()
                .map(|(_, id)| ids::qualify(model, id))
                .collect();
            Err(Refusal::new(
                Code::SubjectAmbiguous,
                format!("{} {what}s match `{subject}`; name one by ID", found.len()),
            )
            .with_details(json!({ "matches": listed })))
        }
    }
}

/// The position among `collections` of the collection `subject` names: its qualified or local
/// ID, else its unique current or former name.
pub(crate) fn collection_position(
    model: &str,
    collections: &[Collection],
    subject: &str,
) -> Result<usize, Refusal> {
    let items = (collections.iter().enumerate())
        .map(|(c, x)| (c, x.id.as_str(), x.name.as_str(), x.former_names.as_slice()));
    pick(model, "collection", items, subject)
}

/// The positions among `collections` of the collection holding the flow `subject` names, and of
/// the flow in it: its qualified or local ID, else its unique current or former name.
pub(crate) fn flow_position(
    model: &str,
    collections: &[Collection],
    subject: &str,
) -> Result<(usize, usize), Refusal> {
    let items = collections.iter().enumerate().flat_map(|(c, collection)| {
        (collection.flows.iter().enumerate()).map(move |(f, flow)| {
            let former = flow.former_names.as_slice();
            ((c, f), flow.id.as_str(), flow.name.as_str(), former)
        })
    });
    pick(model, "flow", items, subject)
}

#[derive(Debug, Serialize)]
pub struct CollectionView<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub flows: Vec<FlowEntry<'a>>,
}

/// A flow as a collection lists it, without its steps.
#[derive(Debug, Serialize)]
pub struct FlowEntry<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<&'a str>,
}

/// A flow with its steps, every ID in it qualified.
#[derive(Debug, Serialize)]
pub struct FlowView<'a> {
    pub id: String,
    pub name: &'a str,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    pub steps: Vec<Step>,
}

/// A collection or a flow with the collection holding it.
pub enum Found<'a> {
    Collection(&'a Collection),
    Flow(&'a Collection, &'a Flow),
}

impl Model {
    /// The collections in order, each listing its flows in order.
    pub fn collection_views(&self) -> Vec<CollectionView<'_>> {
        let mut collections: Vec<&Collection> = self.collections.iter().collect();
        collections.sort_by_key(|c| (c.order, &c.name, &c.id));
        collections
            .into_iter()
            .map(|c| self.collection_view(c))
            .collect()
    }

    pub fn collection_view<'a>(&self, collection: &'a Collection) -> CollectionView<'a> {
        let mut flows: Vec<&Flow> = collection.flows.iter().collect();
        flows.sort_by_key(|f| (f.order, &f.name, &f.id));
        CollectionView {
            id: self.qualify(&collection.id),
            name: &collection.name,
            order: collection.order,
            description: collection.description.as_deref(),
            flows: flows
                .into_iter()
                .map(|f| FlowEntry {
                    id: self.qualify(&f.id),
                    name: &f.name,
                    order: f.order,
                    summary: f.summary.as_deref(),
                })
                .collect(),
        }
    }

    pub fn flow_view<'a>(&self, flow: &'a Flow) -> FlowView<'a> {
        let mut steps = flow.steps.clone();
        qualify_steps(&self.manifest.id, &mut steps);
        FlowView {
            id: self.qualify(&flow.id),
            name: &flow.name,
            order: flow.order,
            summary: flow.summary.as_deref(),
            description: flow.description.as_deref(),
            steps,
        }
    }

    /// The collection or flow whose qualified or local ID is `subject`.
    pub fn find_grouping(&self, subject: &str) -> Option<Found<'_>> {
        let local = ids::local(&self.manifest.id, subject);
        self.collections.iter().find_map(|c| {
            if c.id == local {
                return Some(Found::Collection(c));
            }
            c.flows
                .iter()
                .find(|f| f.id == local)
                .map(|f| Found::Flow(c, f))
        })
    }

    /// The flow `subject` names, its qualified or local ID or its unique current or former name,
    /// with its collection.
    pub fn flow(&self, subject: &str) -> Result<(&Collection, &Flow), Refusal> {
        let (c, f) = flow_position(&self.manifest.id, &self.collections, subject)?;
        let collection = &self.collections[c];
        Ok((collection, &collection.flows[f]))
    }
}

/// Qualifies the IDs of `steps` and of the steps and branches inside them.
fn qualify_steps(model: &str, steps: &mut [Step]) {
    for step in steps {
        match step {
            Step::Message { id, .. } => *id = ids::qualify(model, id),
            Step::Loop { id, steps, .. } => {
                *id = ids::qualify(model, id);
                qualify_steps(model, steps);
            }
            Step::Decision { id, branches, .. } => {
                *id = ids::qualify(model, id);
                for branch in branches {
                    branch.id = ids::qualify(model, &branch.id);
                    qualify_steps(model, &mut branch.steps);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::arch::types::{Collection, Manifest, Module, VaultConfig};

    const M: &str = "Model001";

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "schema_version": 1, "id": M, "name": "Shop",
            "current": {"id": "State001", "name": "Current"},
        }))
        .unwrap()
    }

    /// Web calls Api over `Rel00001`; Db stands alone.
    fn modules() -> Vec<Module> {
        let module = |id: &str, outgoing: Value| -> Module {
            serde_json::from_value(json!({
                "schema_version": 1, "id": id, "name": id, "kind": "service", "order": 0,
                "outgoing": outgoing,
            }))
            .unwrap()
        };
        vec![
            module(
                "WebApp01",
                json!([{"id": "Rel00001", "to": format!("{M}#ApiSrv01"), "kind": "call", "order": 0}]),
            ),
            module("ApiSrv01", json!([])),
            module("DbStore1", json!([])),
        ]
    }

    fn message(id: &str, from: &str, to: &str) -> Value {
        json!({"kind": "message", "id": id, "from": format!("{M}#{from}"), "to": format!("{M}#{to}"), "label": "call"})
    }

    /// The (subject, message) of each problem of one collection holding one flow of `steps`.
    fn problems_of(steps: Value) -> Vec<(String, String)> {
        let collection: Collection = serde_json::from_value(json!({
            "schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0,
            "flows": [{"id": "Flow0001", "name": "Checkout", "order": 0, "steps": steps}],
        }))
        .unwrap();
        super::super::problems(
            &manifest(),
            &VaultConfig::default(),
            None,
            &modules(),
            &[collection],
        )
        .into_iter()
        .map(|p| {
            assert_eq!(p.file, "collections/Coll0001.collection.json");
            (p.subject, p.message)
        })
        .collect()
    }

    #[test]
    fn messages_reference_existing_modules_and_agreeing_relationships() {
        let mut call = message("Step0001", "WebApp01", "ApiSrv01");
        call["relationship"] = format!("{M}#Rel00001").into();
        let mut reply = message("Step0002", "ApiSrv01", "WebApp01");
        reply["relationship"] = format!("{M}#Rel00001").into();
        reply["reply"] = true.into();
        reply["repeat"] = 2.into();
        assert_eq!(problems_of(json!([call, reply])), []);

        let mut against = message("Step0003", "ApiSrv01", "WebApp01");
        against["relationship"] = format!("{M}#Rel00001").into();
        let mut missing = message("Step0004", "WebApp01", "Gone0001");
        missing["relationship"] = format!("{M}#Gone0002").into();
        let found = problems_of(json!([against, missing]));
        assert_eq!(
            found,
            [
                (
                    "Step0003".to_owned(),
                    format!(
                        "relationship {M}#Rel00001 runs from {M}#WebApp01 to {M}#ApiSrv01, not from {M}#ApiSrv01 to {M}#WebApp01"
                    )
                ),
                (
                    "Step0004".to_owned(),
                    format!("to {M}#Gone0001 is not a module")
                ),
                (
                    "Step0004".to_owned(),
                    format!("relationship {M}#Gone0002 is not a relationship")
                ),
            ]
        );
    }

    #[test]
    fn steps_keep_their_text_counts_and_nesting_within_limits() {
        let mut bad = message("Step0001", "WebApp01", "DbStore1");
        bad["label"] = "two\nlines".into();
        bad["repeat"] = 1.into();
        let one_branch = json!({"kind": "decision", "id": "Decide01", "label": "paid?",
            "branches": [{"id": "Branch01", "label": "", "steps": []}]});
        let found = problems_of(json!([bad, one_branch]));
        let messages: Vec<&str> = found.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            messages,
            [
                "label needs 1 to 60 characters and no control characters",
                "repeat needs at least 2",
                "a decision needs at least 2 branches",
                "label needs 1 to 60 characters and no control characters",
            ]
        );
        assert_eq!(found[3].0, "Branch01");

        // Four loops nest; a fifth does not.
        let nest = |depth: usize| {
            (0..depth).fold(json!([]), |inner, n| {
                json!([{"kind": "loop", "id": format!("Loop000{n}"), "label": "again", "steps": inner}])
            })
        };
        assert_eq!(problems_of(nest(4)), []);
        assert_eq!(
            problems_of(nest(5)),
            [(
                "Loop0000".to_owned(),
                "nests more than 4 decisions and loops".to_owned()
            )]
        );

        let many: Vec<Value> = (0..=crate::arch::types::FLOW_STEPS_MAX)
            .map(|n| message(&format!("S{n:07}"), "WebApp01", "ApiSrv01"))
            .collect();
        assert_eq!(
            problems_of(json!(many)),
            [(
                "Flow0001".to_owned(),
                "has 501 steps, more than 500".to_owned()
            )]
        );
    }

    #[test]
    fn former_names_carry_no_control_characters() {
        let collection: Collection = serde_json::from_value(json!({
            "schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0,
            "former_names": ["Sales\u{1b}[31m"],
            "flows": [{"id": "Flow0001", "name": "Checkout", "order": 0,
                       "former_names": ["Buy", "Pay\nnow"]}],
        }))
        .unwrap();
        let found: Vec<(String, String)> = super::super::problems(
            &manifest(),
            &VaultConfig::default(),
            None,
            &modules(),
            &[collection],
        )
        .into_iter()
        .map(|p| (p.subject, p.message))
        .collect();
        let problem = |subject: &str| {
            (
                subject.to_owned(),
                "former name contains a control character".to_owned(),
            )
        };
        assert_eq!(found, [problem("Coll0001"), problem("Flow0001")]);
    }

    #[test]
    fn step_and_branch_ids_share_the_model_id_space() {
        let decision = json!({"kind": "decision", "id": "Decide01", "label": "paid?", "branches": [
            {"id": "WebApp01", "label": "yes"},
            {"id": "Decide01", "label": "no"},
        ]});
        let found = problems_of(json!([decision]));
        assert_eq!(
            found,
            [
                (
                    "WebApp01".to_owned(),
                    "ID is also used in modules/WebApp01.module.json".to_owned()
                ),
                (
                    "Decide01".to_owned(),
                    "ID is also used in collections/Coll0001.collection.json".to_owned()
                ),
            ]
        );
    }
}
