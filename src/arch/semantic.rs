//! The semantic check: warnings about the shape of the authored model itself, read from the
//! modules alone before any code is compared with them.

use std::collections::{BTreeMap, BTreeSet};

use super::diagnostic::{Diagnostic, DiagnosticCode};
use super::index::Index;
use super::segments;
use super::types::{Binding, Module};

/// Every semantic diagnostic of the modules of model `model`, sorted. The modules must have
/// loaded, so parents and targets exist and containment has no cycle.
pub fn check(model: &str, modules: &[Module]) -> Vec<Diagnostic> {
    let index = Index::new(model, modules);
    let mut out = Vec::new();
    containers(&index, &mut out);
    parts_outside(&index, &mut out);
    unconnected_roots(&index, &mut out);
    shared_bindings(&index, &mut out);
    repeated_relationships(&index, &mut out);
    relationship_ends(&index, &mut out);
    out.sort();
    out
}

/// Whether `inner` names `outer`'s path or one below it, in the same repository.
fn encloses(outer: &Binding, inner: &Binding) -> bool {
    outer.repo == inner.repo && segments(&inner.path).starts_with(&segments(&outer.path))
}

/// The longest whole-segment path every binding starts with, when they share a repository and
/// it is certainly a directory: some binding goes deeper than it.
fn common_dir(bindings: &[&Binding]) -> Option<String> {
    let (first, rest) = bindings.split_first()?;
    if rest.iter().any(|b| b.repo != first.repo) {
        return None;
    }
    let paths: Vec<Vec<&str>> = bindings.iter().map(|b| segments(&b.path)).collect();
    let (head, tail) = paths.split_first()?;
    let mut prefix = head.as_slice();
    for path in tail {
        let shared = prefix.iter().zip(path).take_while(|(a, b)| a == b).count();
        prefix = &prefix[..shared];
    }
    let deeper = paths.iter().any(|p| p.len() > prefix.len());
    (!prefix.is_empty() && deeper).then(|| prefix.join("/"))
}

/// `CONTAINER_UNBOUND` and `CONTAINER_PASS_THROUGH`.
fn containers(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    for (id, module) in &index.by_id {
        let parts = index.parts_of(id);
        if parts.is_empty() {
            continue;
        }
        let name = &module.name;
        if let [(part, only)] = parts {
            out.push(Diagnostic::new(
                DiagnosticCode::ContainerPassThrough,
                vec![id.clone(), part.clone()],
                format!("{name} has one part, {}; draw the part directly", only.name),
            ));
        }
        if module.binding.is_none() {
            let bound: Vec<&Binding> = index
                .descendants(id)
                .into_iter()
                .filter_map(|(_, m)| m.binding.as_ref())
                .collect();
            let mut unbound = Diagnostic::new(
                DiagnosticCode::ContainerUnbound,
                vec![id.clone()],
                format!("{name} has parts but no binding"),
            );
            unbound.suggested_binding = common_dir(&bound);
            out.push(unbound);
        }
    }
}

/// `CONTAINER_PARTS_OUTSIDE`: a bound module outside its closest bound ancestor.
fn parts_outside(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    for (id, module) in &index.by_id {
        let Some(inner) = &module.binding else {
            continue;
        };
        if let Some((above_id, above, outer)) = index.bound_ancestor(id)
            && !encloses(outer, inner)
        {
            out.push(Diagnostic::new(
                DiagnosticCode::ContainerPartsOutside,
                vec![above_id.to_owned(), id.clone()],
                format!(
                    "{} is bound to {}, outside {} at {}",
                    module.name, inner.path, above.name, outer.path
                ),
            ));
        }
    }
}

/// `ROOT_UNCONNECTED`: with two or more roots, a root that no relationship crosses into or out
/// of, counting its descendants as the root.
fn unconnected_roots(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    if index.roots.len() < 2 {
        return;
    }
    let mut root_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (root, _) in &index.roots {
        root_of.insert(root, root);
        root_of.extend(
            index
                .descendants(root)
                .into_iter()
                .map(|(d, _)| (d, root.as_str())),
        );
    }
    let mut connected: BTreeSet<&str> = BTreeSet::new();
    for line in &index.lines {
        let (from, to) = (root_of.get(line.from.as_str()), root_of.get(line.to));
        if from != to {
            connected.extend(from.into_iter().chain(to).copied());
        }
    }
    for (id, module) in &index.roots {
        if !connected.contains(id.as_str()) {
            out.push(Diagnostic::new(
                DiagnosticCode::RootUnconnected,
                vec![id.clone()],
                format!(
                    "{} and its parts have no relationship to the rest of the model",
                    module.name
                ),
            ));
        }
    }
}

/// `BINDING_SHARED`: one diagnostic per binding that two or more modules hold.
fn shared_bindings(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    type Key<'k> = (Option<&'k str>, Vec<&'k str>, Option<&'k str>);
    let mut holders: BTreeMap<Key<'_>, (&str, Vec<String>)> = BTreeMap::new();
    for (id, module) in &index.by_id {
        if let Some(b) = &module.binding {
            let key = (b.repo.as_deref(), segments(&b.path), b.symbol.as_deref());
            let (_, ids) = holders.entry(key).or_insert((&b.path, Vec::new()));
            ids.push(id.clone());
        }
    }
    for ((.., symbol), (path, subjects)) in holders {
        if subjects.len() < 2 {
            continue;
        }
        let names: Vec<&str> = subjects.iter().map(|s| index.name(s)).collect();
        let symbol = symbol.map(|s| format!(" {s}")).unwrap_or_default();
        let message = format!("{} share the binding {path}{symbol}", names.join(", "));
        out.push(Diagnostic::new(
            DiagnosticCode::BindingShared,
            subjects,
            message,
        ));
    }
}

/// `RELATIONSHIP_REPEATED`: one diagnostic per source, target and kind drawn more than once,
/// with the relationships as subjects.
fn repeated_relationships(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    let mut drawn: BTreeMap<(&str, &str, &str), Vec<String>> = BTreeMap::new();
    for line in &index.lines {
        drawn
            .entry((&line.from, line.to, line.kind))
            .or_default()
            .push(line.id.clone());
    }
    for ((from, to, kind), mut subjects) in drawn {
        if subjects.len() < 2 {
            continue;
        }
        subjects.sort();
        let message = format!(
            "{} -> {} [{kind}] is drawn {} times: {}",
            index.name(from),
            index.name(to),
            subjects.len(),
            subjects.join(", ")
        );
        out.push(Diagnostic::new(
            DiagnosticCode::RelationshipRepeated,
            subjects,
            message,
        ));
    }
}

/// `CONTAINER_LINE_TO_OWN_PART`, else `RELATIONSHIP_ON_CONTAINER`, per relationship.
fn relationship_ends(index: &Index<'_>, out: &mut Vec<Diagnostic>) {
    for line in &index.lines {
        let (from, to) = (index.name(&line.from), index.name(line.to));
        if index.is_below(line.to, &line.from) || index.is_below(&line.from, line.to) {
            out.push(Diagnostic::new(
                DiagnosticCode::ContainerLineToOwnPart,
                vec![line.id.clone()],
                format!("{from} -> {to} joins a container and its own part"),
            ));
        } else if index.is_container(&line.from) || index.is_container(line.to) {
            out.push(Diagnostic::new(
                DiagnosticCode::RelationshipOnContainer,
                vec![line.id.clone()],
                format!("{from} -> {to} ends on a container; draw it between parts"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiagnosticCode::*;
    use super::*;
    use crate::arch::diagnostic::Severity;
    use crate::arch::ids;
    use crate::arch::types::{Relationship, SCHEMA_VERSION};

    const M: &str = "Model001";

    fn q(local: &str) -> String {
        ids::qualify(M, local)
    }

    fn module(id: &str, parent: Option<&str>, path: Option<&str>) -> Module {
        Module {
            schema_version: SCHEMA_VERSION,
            id: id.to_owned(),
            name: id.to_owned(),
            kind: "module".to_owned(),
            order: 0,
            parent: parent.map(q),
            responsibility: None,
            description: None,
            groups: Vec::new(),
            binding: path.map(|p| Binding {
                repo: None,
                path: p.to_owned(),
                symbol: None,
            }),
            former_names: Vec::new(),
            outgoing: Vec::new(),
        }
    }

    fn link(from: &mut Module, id: &str, to: &str, kind: &str) {
        from.outgoing.push(Relationship {
            id: id.to_owned(),
            to: q(to),
            kind: kind.to_owned(),
            order: 0,
            label: None,
            description: None,
            emphasis: None,
            anchor: None,
        });
    }

    fn of(code: DiagnosticCode, modules: &[Module]) -> Vec<Diagnostic> {
        check(M, modules)
            .into_iter()
            .filter(|d| d.code == code)
            .collect()
    }

    fn subjects(code: DiagnosticCode, modules: &[Module]) -> Vec<Vec<String>> {
        of(code, modules).into_iter().map(|d| d.subjects).collect()
    }

    #[test]
    fn a_bound_model_drawn_between_parts_is_clean() {
        let mut api = module("Api", Some("App"), Some("app/api"));
        link(&mut api, "R1", "Db", "call");
        let modules = [
            module("App", None, Some("app")),
            api,
            module("Web", Some("App"), Some("app/web")),
            module("Db", None, None),
        ];
        assert_eq!(check(M, &modules), []);
    }

    #[test]
    fn an_unbound_container_suggests_the_directory_its_bound_parts_share() {
        let modules = [
            module("App", None, None),
            module("Api", Some("App"), Some("app/api/v1")),
            module("Web", Some("App"), None),
            module("Page", Some("Web"), Some("./app/web/")),
            module("Leaf", Some("Web"), Some("app/apix")),
        ];
        let found = of(ContainerUnbound, &modules);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].subjects, [q("App")]);
        assert_eq!(found[0].suggested_binding.as_deref(), Some("app"));
        assert_eq!(found[1].subjects, [q("Web")]);
        assert_eq!(found[1].severity, Severity::Warning);
    }

    #[test]
    fn no_binding_is_suggested_without_a_shared_directory_or_repository() {
        let mut other = module("Other", Some("App"), Some("app/b"));
        other.binding.as_mut().unwrap().repo = Some("billing".to_owned());
        let modules = [
            module("App", None, None),
            module("Mine", Some("App"), Some("app/a")),
            other,
            module("Empty", None, None),
            module("Part", Some("Empty"), None),
            module("Part2", Some("Empty"), Some("lib")),
            module("Part3", Some("Empty"), Some("src")),
            module("Single", None, None),
            module("Solo", Some("Single"), Some("lib/one")),
            module("Twice", None, None),
            module("First", Some("Twice"), Some("pkg")),
            module("Second", Some("Twice"), Some("pkg/")),
        ];
        let found = of(ContainerUnbound, &modules);
        assert_eq!(found.len(), 4);
        assert!(found.iter().all(|d| d.suggested_binding.is_none()));
    }

    #[test]
    fn bound_modules_outside_their_closest_bound_ancestor_are_judged_by_segments_and_repository() {
        let mut elsewhere = module("Remote", Some("Api"), Some("app/api/v2"));
        elsewhere.binding.as_mut().unwrap().repo = Some("other".to_owned());
        let modules = [
            module("Api", None, Some("app/api")),
            module("Same", Some("Api"), Some("app/api/")),
            module("Inside", Some("Api"), Some("app/api/v1")),
            module("Prefix", Some("Api"), Some("app/apix")),
            module("Unbound", Some("Api"), None),
            elsewhere,
            module("App", None, Some("app")),
            module("Mid", Some("App"), None),
            module("Leaf", Some("Mid"), Some("lib/x")),
            module("Kept", Some("Mid"), Some("app/y")),
        ];
        assert_eq!(
            subjects(ContainerPartsOutside, &modules),
            [
                vec![q("Api"), q("Prefix")],
                vec![q("Api"), q("Remote")],
                vec![q("App"), q("Leaf")],
            ]
        );
        let leaf = &of(ContainerPartsOutside, &modules)[2];
        assert_eq!(leaf.message, "Leaf is bound to lib/x, outside App at app");
    }

    #[test]
    fn a_container_with_one_part_is_information() {
        let modules = [
            module("One", None, Some("a")),
            module("Only", Some("One"), Some("a/b")),
            module("Two", None, Some("c")),
            module("Left", Some("Two"), Some("c/l")),
            module("Right", Some("Two"), Some("c/r")),
        ];
        let found = of(ContainerPassThrough, &modules);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subjects, [q("One"), q("Only")]);
        assert_eq!(found[0].severity, Severity::Information);
    }

    #[test]
    fn a_root_no_relationship_crosses_is_unconnected_counting_its_parts() {
        let mut part = module("Part", Some("Linked"), None);
        link(&mut part, "R1", "Target", "call");
        let mut inner = module("Inner", Some("Alone"), None);
        link(&mut inner, "R2", "Inner2", "call");
        let modules = [
            module("Linked", None, None),
            part,
            module("Target", None, None),
            module("Alone", None, None),
            inner,
            module("Inner2", Some("Alone"), None),
        ];
        assert_eq!(subjects(RootUnconnected, &modules), [vec![q("Alone")]]);
    }

    #[test]
    fn a_single_root_is_never_unconnected() {
        let modules = [
            module("Solo", None, None),
            module("Kid", Some("Solo"), None),
        ];
        assert_eq!(
            subjects(RootUnconnected, &modules),
            Vec::<Vec<String>>::new()
        );
    }

    #[test]
    fn modules_holding_the_same_binding_share_one_diagnostic() {
        let mut symbol = module("Sym", None, Some("app/api"));
        symbol.binding.as_mut().unwrap().symbol = Some("Handler".to_owned());
        let modules = [
            module("A", None, Some("./app/api")),
            module("B", None, Some("app/api/")),
            module("C", None, Some("app/api")),
            symbol,
            module("D", None, Some("app/web")),
        ];
        let found = of(BindingShared, &modules);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subjects, [q("A"), q("B"), q("C")]);
        assert_eq!(found[0].message, "A, B, C share the binding ./app/api");
    }

    #[test]
    fn a_relationship_drawn_twice_with_one_kind_repeats() {
        let mut from = module("From", None, None);
        link(&mut from, "R2", "To", "call");
        link(&mut from, "R1", "To", "call");
        link(&mut from, "R3", "To", "http");
        let modules = [from, module("To", None, None)];
        let found = of(RelationshipRepeated, &modules);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subjects, [q("R1"), q("R2")]);
        assert!(found[0].message.contains(&q("R1")), "{}", found[0].message);
    }

    #[test]
    fn a_line_between_a_container_and_its_own_part_is_reported_once() {
        let mut outer = module("Outer", None, Some("o"));
        link(&mut outer, "Down", "Deep", "call");
        let mut deep = module("Deep", Some("Mid"), Some("o/m/d"));
        link(&mut deep, "Up", "Outer", "call");
        let modules = [
            outer,
            module("Mid", Some("Outer"), Some("o/m")),
            module("Side", Some("Outer"), Some("o/s")),
            deep,
            module("Leaf", Some("Mid"), Some("o/m/l")),
        ];
        assert_eq!(
            subjects(ContainerLineToOwnPart, &modules),
            [vec![q("Down")], vec![q("Up")]]
        );
        assert_eq!(
            subjects(RelationshipOnContainer, &modules),
            Vec::<Vec<String>>::new()
        );
    }

    #[test]
    fn a_relationship_ending_on_a_container_is_information() {
        let mut client = module("Client", None, None);
        link(&mut client, "ToBox", "Box", "call");
        link(&mut client, "ToPart", "Left", "call");
        let modules = [
            client,
            module("Box", None, Some("b")),
            module("Left", Some("Box"), Some("b/l")),
            module("Right", Some("Box"), Some("b/r")),
        ];
        let found = of(RelationshipOnContainer, &modules);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subjects, [q("ToBox")]);
        assert_eq!(found[0].severity, Severity::Information);
    }

    #[test]
    fn diagnostics_serialize_with_screaming_codes_and_omit_absent_suggestions() {
        let modules = [module("Box", None, None), module("Part", Some("Box"), None)];
        let value = serde_json::to_value(check(M, &modules)).unwrap();
        assert_eq!(value[0]["code"], "CONTAINER_UNBOUND");
        assert_eq!(value[0]["severity"], "warning");
        assert!(value[0].get("suggested_binding").is_none());
        assert_eq!(value[1]["code"], "CONTAINER_PASS_THROUGH");
        assert_eq!(value[1]["severity"], "information");
    }
}
