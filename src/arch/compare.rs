//! `model compare`: what one state of the model adds, removes and changes against another, by ID.
//! Modules, relationships, collections, flows and steps are compared each on their own fields:
//! a module without its relationships, a collection without its flows, a flow without its
//! steps, a step without the steps inside it. A relationship's `from` is its owning module, a
//! flow's `collection` its collection, and a step's `flow` and `parent` the flow and the list
//! (flow, branch or loop) holding it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};

use super::ids;
use super::types::{Collection, Module, Step};

#[derive(Debug, Default, Serialize)]
pub struct Comparison {
    pub modules: Changes,
    pub relationships: Changes,
    pub collections: Changes,
    pub flows: Changes,
    pub steps: Changes,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub added: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub changed: Vec<Entry>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Qualified ID.
    pub id: String,
    /// Its name, or a step's or relationship's label, after the change or, when removed, before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The fields that differ, as the vault files spell them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

impl Comparison {
    /// The sections by plural noun, in reading order.
    pub fn sections(&self) -> [(&'static str, &Changes); 5] {
        [
            ("modules", &self.modules),
            ("relationships", &self.relationships),
            ("collections", &self.collections),
            ("flows", &self.flows),
            ("steps", &self.steps),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.sections()
            .iter()
            .all(|(_, c)| **c == Changes::default())
    }
}

/// What `after` changes against `before`, both modules and collections of the model `model`.
pub fn compare(
    model: &str,
    before: (&[Module], &[Collection]),
    after: (&[Module], &[Collection]),
) -> Comparison {
    let side = |f: fn(&str, &[Module], &[Collection]) -> Fields| {
        changes(
            model,
            f(model, before.0, before.1),
            f(model, after.0, after.1),
        )
    };
    Comparison {
        modules: side(|_, modules, _| by_id(modules, |m| (&m.id, fields(m, &["outgoing"])))),
        relationships: side(relationships),
        collections: side(|_, _, collections| {
            by_id(collections, |c| (&c.id, fields(c, &["flows"])))
        }),
        flows: side(flows),
        steps: side(steps),
    }
}

/// Each item's comparable fields by local ID.
type Fields = BTreeMap<String, Map<String, Value>>;

fn by_id<T>(items: &[T], entry: impl Fn(&T) -> (&String, Map<String, Value>)) -> Fields {
    items
        .iter()
        .map(|item| {
            let (id, fields) = entry(item);
            (id.clone(), fields)
        })
        .collect()
}

/// `value`'s fields as its file holds them, without `nested`. Every vault item is a JSON object
/// with string keys, so the conversion cannot fail.
fn fields(value: &impl Serialize, nested: &[&str]) -> Map<String, Value> {
    let Value::Object(mut fields) = serde_json::to_value(value).expect("serializes") else {
        unreachable!("a vault item serializes to a JSON object")
    };
    fields.retain(|key, _| !nested.contains(&key.as_str()));
    fields
}

fn relationships(model: &str, modules: &[Module], _: &[Collection]) -> Fields {
    let mut out = Fields::new();
    for module in modules {
        for relationship in &module.outgoing {
            let mut entry = fields(relationship, &[]);
            entry.insert("from".into(), ids::qualify(model, &module.id).into());
            out.insert(relationship.id.clone(), entry);
        }
    }
    out
}

fn flows(model: &str, _: &[Module], collections: &[Collection]) -> Fields {
    let mut out = Fields::new();
    for collection in collections {
        for flow in &collection.flows {
            let mut entry = fields(flow, &["steps"]);
            let held = ids::qualify(model, &collection.id);
            entry.insert("collection".into(), held.into());
            out.insert(flow.id.clone(), entry);
        }
    }
    out
}

fn steps(model: &str, _: &[Module], collections: &[Collection]) -> Fields {
    let mut out = Fields::new();
    for flow in collections.iter().flat_map(|c| &c.flows) {
        add_steps(model, &flow.id, &flow.id, &flow.steps, &mut out);
    }
    out
}

/// Adds `steps`, held by the list `parent` of `flow`, and the steps inside them to `out`.
fn add_steps(model: &str, flow: &str, parent: &str, steps: &[Step], out: &mut Fields) {
    for step in steps {
        let mut entry = fields(step, &["steps"]);
        // A decision keeps its branches' IDs and labels; their steps are steps of their own.
        if let Some(Value::Array(branches)) = entry.get_mut("branches") {
            for branch in branches.iter_mut().filter_map(Value::as_object_mut) {
                branch.remove("steps");
            }
        }
        entry.insert("flow".into(), ids::qualify(model, flow).into());
        entry.insert("parent".into(), ids::qualify(model, parent).into());
        out.insert(step.id().to_owned(), entry);
        for (list, inner) in step.lists() {
            add_steps(model, flow, list, inner, out);
        }
    }
}

fn changes(model: &str, before: Fields, after: Fields) -> Changes {
    let mut changes = Changes::default();
    let entry = |id: &str, fields: &Map<String, Value>, changed: Vec<String>| Entry {
        id: ids::qualify(model, id),
        name: ["name", "label"]
            .iter()
            .find_map(|key| fields.get(*key)?.as_str().map(str::to_owned)),
        fields: changed,
    };
    for id in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        match (before.get(id), after.get(id)) {
            (None, Some(now)) => changes.added.push(entry(id, now, Vec::new())),
            (Some(was), None) => changes.removed.push(entry(id, was, Vec::new())),
            (Some(was), Some(now)) if was != now => {
                let keys: BTreeSet<&String> = was.keys().chain(now.keys()).collect();
                let differ = (keys.into_iter())
                    .filter(|key| was.get(*key) != now.get(*key))
                    .cloned()
                    .collect();
                changes.changed.push(entry(id, now, differ));
            }
            _ => {}
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    fn modules(value: Value) -> Vec<Module> {
        let list = value.as_array().unwrap().iter().map(|m| {
            let mut module = json!({"schema_version": 1, "kind": "service", "order": 0});
            module
                .as_object_mut()
                .unwrap()
                .extend(m.as_object().unwrap().clone());
            serde_json::from_value(module).unwrap()
        });
        list.collect()
    }

    fn collections(flows: Value) -> Vec<Collection> {
        serde_json::from_value(json!([
            {"schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0, "flows": flows},
        ]))
        .unwrap()
    }

    fn message(id: &str, label: &str) -> Value {
        json!({"kind": "message", "id": id, "from": q("WebApp01"), "to": q("ApiSrv01"), "label": label})
    }

    #[test]
    fn equal_states_compare_empty() {
        let mods = modules(json!([{"id": "WebApp01", "name": "Web"}]));
        let cols = collections(json!([]));
        let comparison = compare(M, (&mods, &cols), (&mods, &cols));
        assert!(comparison.is_empty());
    }

    #[test]
    fn modules_and_relationships_report_added_removed_and_changed_fields() {
        let call = |to: &str| json!([{"id": "Rel00001", "to": q(to), "kind": "call", "order": 0}]);
        let before = modules(json!([
            {"id": "WebApp01", "name": "Web", "outgoing": call("ApiSrv01")},
            {"id": "ApiSrv01", "name": "Api"},
            {"id": "OldOne01", "name": "Old"},
        ]));
        let after = modules(json!([
            {"id": "WebApp01", "name": "Storefront", "kind": "app", "former_names": ["Web"]},
            {"id": "ApiSrv01", "name": "Api", "outgoing": call("NewOne01")},
            {"id": "NewOne01", "name": "New"},
        ]));
        let none = collections(json!([]));
        let comparison = compare(M, (&before, &none), (&after, &none));
        assert_eq!(
            comparison.modules,
            Changes {
                added: vec![Entry {
                    id: q("NewOne01"),
                    name: Some("New".into()),
                    fields: vec![]
                }],
                removed: vec![Entry {
                    id: q("OldOne01"),
                    name: Some("Old".into()),
                    fields: vec![]
                }],
                changed: vec![Entry {
                    id: q("WebApp01"),
                    name: Some("Storefront".into()),
                    fields: vec!["former_names".into(), "kind".into(), "name".into()],
                }],
            }
        );
        // The relationship moved to another source and target, keeping its ID.
        let moved = &comparison.relationships.changed;
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].id, q("Rel00001"));
        assert_eq!(moved[0].fields, ["from", "to"]);
        assert!(comparison.collections == Changes::default());
    }

    #[test]
    fn flows_and_steps_compare_on_their_own_fields_and_places() {
        let mods = modules(json!([]));
        let before = collections(
            json!([{"id": "Flow0001", "name": "Checkout", "order": 0, "steps": [
                message("Step0001", "ask"),
                {"kind": "loop", "id": "Loop0001", "label": "retry", "steps": [message("Step0002", "poll")]},
            ]}]),
        );
        let after = collections(
            json!([{"id": "Flow0001", "name": "Checkout", "order": 0, "summary": "Pays", "steps": [
                message("Step0001", "ask again"),
                {"kind": "loop", "id": "Loop0001", "label": "retry", "steps": []},
                message("Step0002", "poll"),
            ]}]),
        );
        let comparison = compare(M, (&mods, &before), (&mods, &after));
        assert_eq!(comparison.flows.changed.len(), 1);
        assert_eq!(comparison.flows.changed[0].fields, ["summary"]);
        let changed: Vec<(&str, &[String])> = (comparison.steps.changed.iter())
            .map(|e| (e.id.as_str(), e.fields.as_slice()))
            .collect();
        // The loop lost a step, but its own fields hold; the step it held moved out of it.
        assert_eq!(
            changed,
            [
                (q("Step0001").as_str(), ["label".to_owned()].as_slice()),
                (q("Step0002").as_str(), ["parent".to_owned()].as_slice()),
            ]
        );
        assert!(comparison.steps.added.is_empty() && comparison.steps.removed.is_empty());
        assert!(comparison.collections == Changes::default());
    }
}
