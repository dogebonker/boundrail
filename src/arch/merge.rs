//! The three-way merge `model reconcile` runs: what a proposal changed since its base, against
//! what its predecessor (the current state, or a child's parent draft) changed since the same
//! base, by ID.
//!
//! Granularity: a module's own fields (everything but `outgoing`) merge as one subject, each
//! relationship as a subject of its own with its owning module as a field, so one side adding a
//! relationship to a module the other side renamed merges cleanly. A collection merges as its
//! whole file, flows and steps included, since a flow or step move would need its own subject.
//! A subject one side changed and the other did not takes the changed side; a subject both sides
//! changed the same way takes it; a subject both changed differently is `both-changed`, and one
//! changed by one side and removed by the other is `changed-removed`. A settlement decides a
//! conflicting subject instead: the proposal's side, the predecessor's, or a replacement.

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

/// How a conflicting subject settles.
#[derive(Debug)]
pub(super) enum Choice {
    /// The proposal's side.
    Mine,
    /// The predecessor's side.
    Theirs,
    /// A replacement of the subject's own kind.
    Edited(Box<Item>),
}

/// A replacement module (its own fields), relationship or collection.
#[derive(Debug)]
pub(super) enum Item {
    Module(Module),
    Relationship(Relationship),
    Collection(Collection),
}

/// Choices by qualified subject.
pub(super) type Settlement = BTreeMap<String, Choice>;

/// What a merge gives: the merged modules and collections sorted by ID, the conflicts left,
/// sorted by subject, the subjects a settlement decided, and the subjects it took from the
/// predecessor over the base's version, which the proposal kept. The merge holds when no
/// conflict is left.
#[derive(Debug, Default)]
pub(super) struct Merge {
    pub(super) modules: Vec<Module>,
    pub(super) collections: Vec<Collection>,
    pub(super) conflicts: Vec<Conflict>,
    pub(super) settled: BTreeSet<String>,
    pub(super) taken: BTreeSet<String>,
}

/// Merges `ours` (the proposal) and `theirs` (its predecessor), both changed from `base`, in the
/// model `model`, settling conflicts by `settlement`. The current state's `retired` IDs may not
/// stay in the result, settled or not.
pub(super) fn merge<'a>(
    model: &str,
    base: Side<'a>,
    ours: Side<'a>,
    theirs: Side<'a>,
    retired: &[String],
    settlement: &'a Settlement,
) -> Merge {
    let mut out = Merge::default();
    let (b, o, t) = (modules(base.0), modules(ours.0), modules(theirs.0));
    let settle = |subject: &str, o: Option<&&'a Module>, t: Option<&&'a Module>| {
        picked(settlement.get(subject)?, o, t, |item| match item {
            Item::Module(m) => Some(m),
            _ => None,
        })
    };
    let fields = three(model, [&b, &o, &t], same_fields, settle, &mut out);
    let (b, o, t) = (owned(base.0), owned(ours.0), owned(theirs.0));
    let settle = |subject: &str, o: Option<&Owned<'a>>, t: Option<&Owned<'a>>| {
        let owner = o.or(t).map(|(owner, _)| *owner);
        picked(settlement.get(subject)?, o, t, |item| match item {
            Item::Relationship(r) => Some((owner?, r)),
            _ => None,
        })
    };
    let owned = three(model, [&b, &o, &t], PartialEq::eq, settle, &mut out);
    let (b, o, t) = (
        collections(base.1),
        collections(ours.1),
        collections(theirs.1),
    );
    let settle = |subject: &str, o: Option<&&'a Collection>, t: Option<&&'a Collection>| {
        picked(settlement.get(subject)?, o, t, |item| match item {
            Item::Collection(c) => Some(c),
            _ => None,
        })
    };
    out.collections = three(model, [&b, &o, &t], PartialEq::eq, settle, &mut out)
        .into_values()
        .cloned()
        .collect();
    let rebuilt = Rebuild {
        model,
        sides: [ours.0, theirs.0],
        settlement,
    };
    out.modules = rebuilt.modules(fields, owned, &mut out);
    let left = still_retired(model, &out.modules, &out.collections, retired);
    out.conflicts.extend(left);
    out.conflicts.sort();
    out.conflicts.dedup();
    out
}

/// The version `choice` takes: ours, theirs, or the one `edited` makes of its replacement, `None`
/// when the replacement is of another kind.
fn picked<'a, V: Clone>(
    choice: &'a Choice,
    ours: Option<&V>,
    theirs: Option<&V>,
    edited: impl FnOnce(&'a Item) -> Option<V>,
) -> Option<Option<V>> {
    match choice {
        Choice::Mine => Some(ours.cloned()),
        Choice::Theirs => Some(theirs.cloned()),
        Choice::Edited(item) => edited(item).map(Some),
    }
}

/// Items, borrowed, by local ID.
type Keyed<'a, V> = BTreeMap<&'a str, V>;

/// A relationship with the local ID of the module owning it.
type Owned<'a> = (&'a str, &'a Relationship);

/// The merge of one kind of subject, base, ours and theirs, `same` saying when two versions of
/// it are equal and `settle` what a conflicting subject's settlement takes. Only the version
/// picked is copied.
fn three<'a, V: Clone>(
    model: &str,
    [base, ours, theirs]: [&Keyed<'a, V>; 3],
    same: fn(&V, &V) -> bool,
    settle: impl Fn(&str, Option<&V>, Option<&V>) -> Option<Option<V>>,
    out: &mut Merge,
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
            o.cloned()
        } else if eq(o, b) {
            out.taken.insert(ids::qualify(model, key));
            t.cloned()
        } else {
            let subject = ids::qualify(model, key);
            let Some(pick) = settle(&subject, o, t) else {
                let kind = if o.is_some() && t.is_some() {
                    Kind::BothChanged
                } else {
                    Kind::ChangedRemoved
                };
                out.conflicts.push(Conflict { subject, kind });
                continue;
            };
            out.settled.insert(subject);
            pick
        };
        if let Some(value) = pick {
            merged.insert(*key, value);
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
fn owned(modules: &[Module]) -> Keyed<'_, Owned<'_>> {
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

/// What puts merged modules and relationships back together: the model, the proposal's and
/// the predecessor's modules, and the settlement.
struct Rebuild<'a, 's> {
    model: &'s str,
    sides: [&'a [Module]; 2],
    settlement: &'a Settlement,
}

impl<'a> Rebuild<'a, '_> {
    /// The merged modules with their merged relationships. A relationship whose owner the merge
    /// removed is `changed-removed`, unless that owner is in conflict itself. Settled as mine or
    /// theirs, it stays with its owner as that side holds them when that side holds it, and goes
    /// otherwise; settled as a replacement, the replacement stays with its owner as the proposal,
    /// else the predecessor, holds it. A module equal to one side's, relationship order aside, is
    /// that side's as it stands, so an unchanged file is not rewritten.
    fn modules(
        &self,
        mut fields: Keyed<'a, &'a Module>,
        mut owned: Keyed<'a, Owned<'a>>,
        out: &mut Merge,
    ) -> Vec<Module> {
        let sides = self.sides.map(modules);
        let qualify = |id: &str| ids::qualify(self.model, id);
        let conflicted = |out: &Merge, id: &str| {
            let subject = qualify(id);
            out.conflicts.iter().any(|c| c.subject == subject)
        };
        let (mut kept, mut dropped, mut replaced) = (Vec::new(), BTreeSet::new(), Vec::new());
        for (&id, &(owner, _)) in &owned {
            if fields.contains_key(owner) || conflicted(out, owner) {
                continue;
            }
            let subject = qualify(id);
            let side = match self.settlement.get(&subject) {
                Some(Choice::Mine) => Some(&sides[0]),
                Some(Choice::Theirs) => Some(&sides[1]),
                Some(Choice::Edited(item)) => match &**item {
                    Item::Relationship(r) => {
                        replaced.push((id, (owner, r)));
                        None
                    }
                    _ => continue,
                },
                None => continue,
            };
            let holder = match side {
                Some(side) => (side.get(owner)).filter(|m| m.outgoing.iter().any(|r| r.id == id)),
                None => sides.iter().find_map(|side| side.get(owner)),
            };
            match holder {
                Some(module) => kept.push((owner, *module)),
                None if side.is_some() => {
                    dropped.insert(id);
                }
                None => continue,
            }
            out.settled.insert(subject);
        }
        fields.extend(kept);
        owned.extend(replaced);
        let mut outgoing: BTreeMap<&str, Vec<&Relationship>> = BTreeMap::new();
        for (id, (owner, relationship)) in owned {
            if dropped.contains(id) {
                continue;
            }
            if fields.contains_key(owner) {
                outgoing.entry(owner).or_default().push(relationship);
                continue;
            }
            let subject = qualify(id);
            if let Some(Choice::Theirs) = self.settlement.get(&subject) {
                out.settled.insert(subject);
            } else if !conflicted(out, owner) {
                out.conflicts.push(Conflict {
                    subject,
                    kind: Kind::ChangedRemoved,
                });
            }
        }
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

    /// The merged modules and collections, or the conflicts left.
    fn held(merge: Merge) -> Result<(Vec<Module>, Vec<Collection>), Vec<Conflict>> {
        if merge.conflicts.is_empty() {
            return Ok((merge.modules, merge.collections));
        }
        Err(merge.conflicts)
    }

    fn run(
        base: &[Module],
        ours: &[Module],
        theirs: &[Module],
    ) -> Result<(Vec<Module>, Vec<Collection>), Vec<Conflict>> {
        let settlement = Settlement::new();
        held(merge(
            M,
            (base, &[]),
            (ours, &[]),
            (theirs, &[]),
            &[],
            &settlement,
        ))
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
        let none = Settlement::new();
        let (_, merged) = held(merge(
            M,
            (&[], &base),
            (&[], &ours),
            (&[], &theirs),
            &[],
            &none,
        ))
        .unwrap();
        let names: Vec<&str> = merged.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Purchases", "Refunds"]);
        let other = [collection("Coll0001", "Sales")];
        let conflicts = held(merge(
            M,
            (&[], &base),
            (&[], &other),
            (&[], &theirs),
            &[],
            &none,
        ))
        .unwrap_err();
        assert_eq!(conflicts, [conflict("Coll0001", Kind::BothChanged)]);
    }

    #[test]
    fn a_settlement_takes_mine_theirs_or_a_replacement() {
        let base = [module("ModB0001", "B", json!([]))];
        let ours = [module("ModB0001", "Bee", json!([]))];
        let theirs = [module("ModB0001", "Bea", json!([]))];
        let edited = module("ModB0001", "Bey", json!([]));
        for (choice, name) in [
            (Choice::Mine, "Bee"),
            (Choice::Theirs, "Bea"),
            (Choice::Edited(Box::new(Item::Module(edited))), "Bey"),
        ] {
            let settlement = Settlement::from([(q("ModB0001"), choice)]);
            let merged = merge(
                M,
                (&base, &[]),
                (&ours, &[]),
                (&theirs, &[]),
                &[],
                &settlement,
            );
            assert_eq!(merged.settled, BTreeSet::from([q("ModB0001")]));
            assert_eq!(names(&held(merged).unwrap().0), [("ModB0001", name)]);
        }
    }

    #[test]
    fn a_relationship_settled_as_mine_keeps_the_owner_the_other_side_removed() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let ours = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([call("RelB0001", "ModA0001", 0)])),
        ];
        let theirs = [module("ModA0001", "A", json!([]))];
        let settle = |choice| Settlement::from([(q("RelB0001"), choice)]);
        let mine = settle(Choice::Mine);
        let merged = merge(M, (&base, &[]), (&ours, &[]), (&theirs, &[]), &[], &mine);
        let (modules, _) = held(merged).unwrap();
        assert_eq!(modules[1], ours[1]);
        let gone = settle(Choice::Theirs);
        let merged = merge(M, (&base, &[]), (&ours, &[]), (&theirs, &[]), &[], &gone);
        assert_eq!(names(&held(merged).unwrap().0), [("ModA0001", "A")]);
    }

    #[test]
    fn a_replacement_for_a_relationship_whose_owner_the_other_side_removed_is_written() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let ours = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([call("RelB0001", "ModA0001", 0)])),
        ];
        let theirs = [module("ModA0001", "A", json!([]))];
        let replacement = serde_json::from_value(call("RelB0001", "ModA0001", 7)).unwrap();
        let edited = Choice::Edited(Box::new(Item::Relationship(replacement)));
        let settlement = Settlement::from([(q("RelB0001"), edited)]);
        let merged = merge(
            M,
            (&base, &[]),
            (&ours, &[]),
            (&theirs, &[]),
            &[],
            &settlement,
        );
        assert_eq!(merged.settled, BTreeSet::from([q("RelB0001")]));
        let (modules, _) = held(merged).unwrap();
        assert_eq!(names(&modules), [("ModA0001", "A"), ("ModB0001", "B")]);
        assert_eq!(modules[1].outgoing.len(), 1);
        assert_eq!(modules[1].outgoing[0].order, 7);
    }

    #[test]
    fn a_relationship_settled_to_the_side_without_it_goes_with_its_owner_left_removed() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let ours = [module("ModA0001", "A", json!([]))];
        let theirs = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([call("RelB0002", "ModA0001", 0)])),
        ];
        let settle = |choice| Settlement::from([(q("RelB0002"), choice)]);
        let mine = settle(Choice::Mine);
        let merged = merge(M, (&base, &[]), (&ours, &[]), (&theirs, &[]), &[], &mine);
        assert_eq!(merged.settled, BTreeSet::from([q("RelB0002")]));
        assert_eq!(names(&held(merged).unwrap().0), [("ModA0001", "A")]);
        let kept = settle(Choice::Theirs);
        let merged = merge(M, (&base, &[]), (&ours, &[]), (&theirs, &[]), &[], &kept);
        let (modules, _) = held(merged).unwrap();
        assert_eq!(modules[1], theirs[1]);
    }

    #[test]
    fn the_merge_names_what_it_took_from_the_predecessor_over_the_base() {
        let base = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let ours = [
            module("ModA0001", "Ay", json!([])),
            module("ModB0001", "B", json!([])),
        ];
        let theirs = [
            module("ModA0001", "A", json!([])),
            module("ModB0001", "Bee", json!([])),
        ];
        let none = Settlement::new();
        let merged = merge(M, (&base, &[]), (&ours, &[]), (&theirs, &[]), &[], &none);
        assert_eq!(merged.taken, BTreeSet::from([q("ModB0001")]));
    }

    #[test]
    fn an_id_the_current_state_retired_conflicts() {
        let kept = [module("ModB0001", "B", json!([]))];
        let retired = ["ModB0001".to_owned()];
        let none = Settlement::new();
        let conflicts = held(merge(
            M,
            (&[], &[]),
            (&kept, &[]),
            (&[], &[]),
            &retired,
            &none,
        ))
        .unwrap_err();
        assert_eq!(conflicts, [conflict("ModB0001", Kind::Retired)]);
    }
}
