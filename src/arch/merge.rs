//! The three-way merge `model reconcile` runs: what a proposal changed since its base, against
//! what the current state changed since the same base, by ID.
//!
//! Granularity: a module's own fields (everything but `outgoing`) merge as one subject, each
//! relationship as a subject of its own with its owning module as a field, so one side adding a
//! relationship to a module the other side renamed merges cleanly. A collection merges as its
//! whole file, flows and steps included, since a flow or step move would need its own subject.
//! A subject one side changed and the other did not takes the changed side; a subject both sides
//! changed the same way takes it; a subject both changed differently is `both-changed`, and one
//! changed by one side and removed by the other is `changed-removed`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::ids;
use super::types::{Collection, Module, Relationship};

/// One side of the merge: its modules and collections.
pub(super) type Side<'a> = (&'a [Module], &'a [Collection]);

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct Conflict {
    /// The qualified ID of the module, relationship or collection.
    pub(super) subject: String,
    pub(super) kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Kind {
    BothChanged,
    ChangedRemoved,
    /// The merged proposal holds an ID the current state retired.
    Retired,
}

/// Merges `ours` (the proposal) and `theirs` (the current state), both changed from `base`, in
/// the model `model`. The current state's `retired` IDs may not stay in the result. Returns the
/// merged modules and collections sorted by ID, or every conflict sorted by subject.
pub(super) fn merge(
    model: &str,
    base: Side<'_>,
    ours: Side<'_>,
    theirs: Side<'_>,
    retired: &[String],
) -> Result<(Vec<Module>, Vec<Collection>), Vec<Conflict>> {
    let mut conflicts = Vec::new();
    let (b, o, t) = (modules(base.0), modules(ours.0), modules(theirs.0));
    let fields = three(model, &b, &o, &t, same_fields, &mut conflicts);
    let (b, o, t) = (owned(base.0), owned(ours.0), owned(theirs.0));
    let owned = three(model, &b, &o, &t, PartialEq::eq, &mut conflicts);
    let (b, o, t) = (
        collections(base.1),
        collections(ours.1),
        collections(theirs.1),
    );
    let collections: Vec<Collection> = three(model, &b, &o, &t, PartialEq::eq, &mut conflicts)
        .into_values()
        .cloned()
        .collect();
    let modules = rebuild(model, fields, owned, [ours.0, theirs.0], &mut conflicts);
    conflicts.extend(still_retired(model, &modules, &collections, retired));
    if conflicts.is_empty() {
        return Ok((modules, collections));
    }
    conflicts.sort();
    conflicts.dedup();
    Err(conflicts)
}

/// Items, borrowed, by local ID.
type Keyed<'a, V> = BTreeMap<&'a str, V>;

/// The merge of one kind of subject, `same` saying when two versions of it are equal. Only the
/// version picked is copied.
fn three<'a, V: Clone>(
    model: &str,
    base: &Keyed<'a, V>,
    ours: &Keyed<'a, V>,
    theirs: &Keyed<'a, V>,
    same: fn(&V, &V) -> bool,
    conflicts: &mut Vec<Conflict>,
) -> Keyed<'a, V> {
    let eq = |a: Option<&V>, b: Option<&V>| match (a, b) {
        (Some(a), Some(b)) => same(a, b),
        (a, b) => a.is_none() && b.is_none(),
    };
    let keys: BTreeSet<&&str> = (base.keys())
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    let mut merged = Keyed::new();
    for key in keys {
        let (b, o, t) = (base.get(key), ours.get(key), theirs.get(key));
        let pick = if eq(o, t) || eq(t, b) {
            o
        } else if eq(o, b) {
            t
        } else {
            let kind = if o.is_some() && t.is_some() {
                Kind::BothChanged
            } else {
                Kind::ChangedRemoved
            };
            let subject = ids::qualify(model, key);
            conflicts.push(Conflict { subject, kind });
            continue;
        };
        if let Some(value) = pick {
            merged.insert(*key, value.clone());
        }
    }
    merged
}

fn modules(modules: &[Module]) -> Keyed<'_, &Module> {
    modules.iter().map(|m| (m.id.as_str(), m)).collect()
}

fn collections(collections: &[Collection]) -> Keyed<'_, &Collection> {
    collections.iter().map(|c| (c.id.as_str(), c)).collect()
}

/// Relationships with the local ID of the module owning them.
fn owned(modules: &[Module]) -> Keyed<'_, (&str, &Relationship)> {
    let mut owned = Keyed::new();
    for module in modules {
        for r in &module.outgoing {
            owned.insert(r.id.as_str(), (module.id.as_str(), r));
        }
    }
    owned
}

/// Whether two modules hold the same fields, their relationships aside.
fn same_fields(a: &&Module, b: &&Module) -> bool {
    let Module {
        schema_version,
        id,
        name,
        kind,
        order,
        parent,
        responsibility,
        description,
        groups,
        binding,
        former_names,
        outgoing: _,
        references,
    } = a;
    (schema_version, id, name, kind, order, parent)
        == (
            &b.schema_version,
            &b.id,
            &b.name,
            &b.kind,
            &b.order,
            &b.parent,
        )
        && (responsibility, description, groups, binding, former_names)
            == (
                &b.responsibility,
                &b.description,
                &b.groups,
                &b.binding,
                &b.former_names,
            )
        && references == &b.references
}

/// Relationships in the order a module's file keeps them when the merge writes it.
fn in_order(relationships: &mut [&Relationship]) {
    relationships.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
}

/// The merged modules with their merged relationships. A relationship whose owner the merge
/// removed is `changed-removed`, unless that owner is in conflict itself. A module equal to one
/// side's, relationship order aside, is that side's as it stands, so an unchanged file is not
/// rewritten.
fn rebuild(
    model: &str,
    fields: Keyed<'_, &Module>,
    owned: Keyed<'_, (&str, &Relationship)>,
    sides: [&[Module]; 2],
    conflicts: &mut Vec<Conflict>,
) -> Vec<Module> {
    let mut outgoing: BTreeMap<&str, Vec<&Relationship>> = BTreeMap::new();
    for (id, (owner, relationship)) in owned {
        if fields.contains_key(owner) {
            outgoing.entry(owner).or_default().push(relationship);
            continue;
        }
        // A module in conflict already names the relationships it holds.
        let owner = ids::qualify(model, owner);
        if !conflicts.iter().any(|c| c.subject == owner) {
            let subject = ids::qualify(model, id);
            conflicts.push(Conflict {
                subject,
                kind: Kind::ChangedRemoved,
            });
        }
    }
    let sides = sides.map(modules);
    let held = |module: &Module, relationships: &[&Relationship]| {
        let mut own: Vec<&Relationship> = module.outgoing.iter().collect();
        in_order(&mut own);
        own == relationships
    };
    (fields.into_iter())
        .map(|(id, module)| {
            let mut relationships = outgoing.remove(id).unwrap_or_default();
            in_order(&mut relationships);
            let kept = (sides.iter()).find_map(|side| {
                side.get(id)
                    .filter(|m| same_fields(m, &module) && held(m, &relationships))
            });
            match kept {
                Some(kept) => (*kept).clone(),
                None => Module {
                    outgoing: relationships.into_iter().cloned().collect(),
                    ..module.clone()
                },
            }
        })
        .collect()
}

/// A `retired` conflict for every ID the merged state holds that the current state retired.
fn still_retired(
    model: &str,
    modules: &[Module],
    collections: &[Collection],
    retired: &[String],
) -> Vec<Conflict> {
    let retired: BTreeSet<&str> = retired.iter().map(String::as_str).collect();
    let held = (modules.iter())
        .flat_map(|m| std::iter::once(&m.id).chain(m.outgoing.iter().map(|r| &r.id)))
        .map(String::as_str)
        .chain(collections.iter().flat_map(Collection::ids));
    held.filter(|id| retired.contains(id))
        .map(|id| Conflict {
            subject: ids::qualify(model, id),
            kind: Kind::Retired,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    fn module(id: &str, name: &str, outgoing: Value) -> Module {
        serde_json::from_value(json!({"schema_version": 1, "id": id, "name": name,
            "kind": "service", "order": 0, "outgoing": outgoing}))
        .unwrap()
    }

    fn call(id: &str, to: &str, order: u32) -> Value {
        json!({"id": id, "to": q(to), "kind": "call", "order": order})
    }

    fn collection(id: &str, name: &str) -> Collection {
        serde_json::from_value(
            json!({"schema_version": 1, "id": id, "name": name, "order": 0, "flows": []}),
        )
        .unwrap()
    }

    fn run(
        base: &[Module],
        ours: &[Module],
        theirs: &[Module],
    ) -> Result<(Vec<Module>, Vec<Collection>), Vec<Conflict>> {
        merge(M, (base, &[]), (ours, &[]), (theirs, &[]), &[])
    }

    fn names(modules: &[Module]) -> Vec<(&str, &str)> {
        modules
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str()))
            .collect()
    }

    fn conflict(subject: &str, kind: Kind) -> Conflict {
        Conflict {
            subject: q(subject),
            kind,
        }
    }

    #[test]
    fn changes_one_side_made_apply_and_both_sides_may_add() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let ours = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "Bee", json!([])),
            module("ModC0001", "C", json!([])),
        ];
        let theirs = [
            module("ModB0001", "B", json!([])),
            module("ModD0001", "D", json!([])),
        ];
        let (merged, _) = run(&base, &ours, &theirs).unwrap();
        assert_eq!(
            names(&merged),
            [("ModB0001", "Bee"), ("ModC0001", "C"), ("ModD0001", "D")]
        );
    }

    #[test]
    fn the_same_change_on_both_sides_merges_and_different_ones_conflict() {
        let base = [module("ModB0001", "B", json!([]))];
        let same = [module("ModB0001", "Bee", json!([]))];
        assert_eq!(
            names(&run(&base, &same, &same).unwrap().0),
            [("ModB0001", "Bee")]
        );
        let other = [module("ModB0001", "Bea", json!([]))];
        let conflicts = run(&base, &same, &other).unwrap_err();
        assert_eq!(conflicts, [conflict("ModB0001", Kind::BothChanged)]);
    }

    #[test]
    fn a_change_against_a_removal_conflicts_either_way_round() {
        let base = [module("ModB0001", "B", json!([]))];
        let changed = [module("ModB0001", "Bee", json!([]))];
        for (ours, theirs) in [(&changed[..], &[][..]), (&[][..], &changed[..])] {
            let conflicts = run(&base, ours, theirs).unwrap_err();
            assert_eq!(conflicts, [conflict("ModB0001", Kind::ChangedRemoved)]);
        }
        // A removal the other side left alone applies.
        assert!(run(&base, &[], &base).unwrap().0.is_empty());
    }

    #[test]
    fn relationships_merge_apart_from_their_module() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let renamed = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "Bee", json!([])),
        ];
        let called = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([call("RelB0001", "ModA0001", 0)])),
        ];
        let (merged, _) = run(&base, &renamed, &called).unwrap();
        assert_eq!(merged[1].name, "Bee");
        assert_eq!(merged[1].outgoing.len(), 1);
        assert_eq!(merged[1].outgoing[0].id, "RelB0001");
    }

    #[test]
    fn a_relationship_whose_module_the_other_side_removed_conflicts() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let added = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([call("RelB0001", "ModA0001", 0)])),
        ];
        let removed = [module("ModA0001", "A", json!([]))];
        let conflicts = run(&base, &added, &removed).unwrap_err();
        assert_eq!(conflicts, [conflict("RelB0001", Kind::ChangedRemoved)]);
    }

    #[test]
    fn a_module_in_conflict_names_its_relationships_once() {
        let rels = json!([call("RelB0001", "ModB0001", 0)]);
        let base = [module("ModB0001", "B", rels.clone())];
        let ours = [module("ModB0001", "Bee", rels.clone())];
        let theirs = [module("ModB0001", "Bea", rels)];
        let conflicts = run(&base, &ours, &theirs).unwrap_err();
        assert_eq!(conflicts, [conflict("ModB0001", Kind::BothChanged)]);
    }

    #[test]
    fn an_unchanged_module_keeps_its_relationship_order() {
        let rels = json!([
            call("RelZ0001", "ModA0001", 0),
            call("RelA0001", "ModA0001", 0)
        ]);
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", rels),
        ];
        let theirs = [
            base[0].clone(),
            base[1].clone(),
            module("ModC0001", "C", json!([])),
        ];
        let (merged, _) = run(&base, &base, &theirs).unwrap();
        assert_eq!(merged[1], base[1]);
    }

    #[test]
    fn collections_merge_as_whole_files() {
        let base = [collection("Coll0001", "Orders")];
        let ours = [
            collection("Coll0001", "Orders"),
            collection("Coll0002", "Refunds"),
        ];
        let theirs = [collection("Coll0001", "Purchases")];
        let (_, merged) = merge(M, (&[], &base), (&[], &ours), (&[], &theirs), &[]).unwrap();
        let names: Vec<&str> = merged.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Purchases", "Refunds"]);
        let other = [collection("Coll0001", "Sales")];
        let conflicts = merge(M, (&[], &base), (&[], &other), (&[], &theirs), &[]).unwrap_err();
        assert_eq!(conflicts, [conflict("Coll0001", Kind::BothChanged)]);
    }

    #[test]
    fn an_id_the_current_state_retired_conflicts() {
        let held = [module("ModB0001", "B", json!([]))];
        let retired = ["ModB0001".to_owned()];
        let conflicts = merge(M, (&[], &[]), (&held, &[]), (&[], &[]), &retired).unwrap_err();
        assert_eq!(conflicts, [conflict("ModB0001", Kind::Retired)]);
    }
}
