//! The semantic check: warnings about the shape of the authored model itself, read from the
//! modules, flows and walkthroughs alone before any code is compared with them.

use std::collections::{BTreeMap, BTreeSet};
use std::iter;

use super::checkpoint;
use super::diagnostic::{Diagnostic, DiagnosticCode};
use super::index::Index;
use super::types::{BeatReading, Binding, Collection, Module, ReadingState, Step, walk};
use super::vault::{self, Model};
use super::{ids, segments};

/// Every semantic diagnostic of the modules and collections of model `model`, sorted. They must
/// have loaded, so parents, targets and step references exist and containment has no cycle.
pub fn check(model: &str, modules: &[Module], collections: &[Collection]) -> Vec<Diagnostic> {
    let index = Index::new(model, modules);
    let mut out = Vec::new();
    containers(&index, &mut out);
    parts_outside(&index, &mut out);
    unconnected_roots(&index, &mut out);
    shared_bindings(&index, &mut out);
    repeated_relationships(&index, &mut out);
    relationship_ends(&index, &mut out);
    flows(model, &index, collections, &mut out);
    out.sort();
    out
}

/// `FLOW_SUMMARY_MISSING`, and `MESSAGE_UNDRAWN` for a message between two modules, neither
/// containing the other, that no relationship between the modules or their ancestors connects
/// in either direction. Ancestors the two modules share do not count: a line from one module's
/// container to a shared container joins nothing between them.
fn flows(model: &str, index: &Index<'_>, collections: &[Collection], out: &mut Vec<Diagnostic>) {
    if collections.is_empty() {
        return;
    }
    let drawn: BTreeSet<(&str, &str)> = index
        .lines
        .iter()
        .map(|l| (l.from.as_str(), l.to))
        .collect();
    let connected = |a: &str, b: &str| {
        let (mut above_a, mut above_b) = (index.chain(a), index.chain(b));
        while above_a.last().is_some() && above_a.last() == above_b.last() {
            above_a.pop();
            above_b.pop();
        }
        above_a.iter().any(|x| {
            above_b
                .iter()
                .any(|y| drawn.contains(&(*x, *y)) || drawn.contains(&(*y, *x)))
        })
    };
    for flow in collections.iter().flat_map(|c| &c.flows) {
        if flow.summary.is_none() {
            out.push(Diagnostic::new(
                DiagnosticCode::FlowSummaryMissing,
                vec![ids::qualify(model, &flow.id)],
                format!("{} has no summary", flow.name),
            ));
        }
        for (step, _) in walk(&flow.steps) {
            let Step::Message { id, from, to, .. } = step else {
                continue;
            };
            if from == to || index.is_below(from, to) || index.is_below(to, from) {
                continue;
            }
            if !connected(from, to) {
                let (sender, receiver) = (index.name(from), index.name(to));
                out.push(Diagnostic::new(
                    DiagnosticCode::MessageUndrawn,
                    vec![ids::qualify(model, id), from.clone(), to.clone()],
                    format!(
                        "{}: {sender} -> {receiver} has no relationship between them",
                        flow.name
                    ),
                ));
            }
        }
    }
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

/// The current state and every proposal or checkpoint a reading names, each loaded once per run
/// through the loaders `model read` uses, and only when a reading names it.
pub struct States<'m> {
    current: &'m Model,
    /// Each model with how a title names it: `proposal <name>` or `checkpoint <ID>`.
    loaded: BTreeMap<ReadingState, anyhow::Result<(Model, String)>>,
}

impl<'m> States<'m> {
    pub fn new(current: &'m Model) -> Self {
        Self {
            current,
            loaded: BTreeMap::new(),
        }
    }

    pub fn current(&self) -> &'m Model {
        self.current
    }

    /// Loads `state` unless it is the current state or already loaded.
    pub fn load(&mut self, state: &ReadingState) {
        if state.is_current() || self.loaded.contains_key(state) {
            return;
        }
        let dir = &self.current.dir;
        let loaded = match state {
            ReadingState::Current => unreachable!("the current state is never loaded again"),
            ReadingState::Proposal(p) => vault::load_proposal(dir.clone(), p).map(|m| {
                let name = m.proposal.as_ref().map_or(p.as_str(), |p| &p.name);
                let name = format!("proposal {name}");
                (m, name)
            }),
            ReadingState::Checkpoint(c) => {
                checkpoint::load(dir, c).map(|(c, m)| (m, format!("checkpoint {}", c.id)))
            }
        };
        self.loaded.insert(state.clone(), loaded);
    }

    /// The model `state` holds, with how a title names it (`None` for the current state), or why
    /// it does not load. `load` must have been called for it.
    pub fn get(&self, state: &ReadingState) -> Result<(&Model, Option<&str>), &anyhow::Error> {
        if state.is_current() {
            return Ok((self.current, None));
        }
        match self.loaded.get(state).expect("`load` comes before `get`") {
            Ok((model, name)) => Ok((model, Some(name))),
            Err(error) => Err(error),
        }
    }
}

/// One reading a check judges: the code it raises, its subjects, and how a message names it.
pub struct Judged<'a> {
    pub code: DiagnosticCode,
    pub subjects: Vec<String>,
    pub label: String,
    pub reading: &'a BeatReading,
}

/// `code` for each reading whose proposal or checkpoint does not load, or whose scope, focus or
/// flow the state it draws lacks.
pub fn targets<'a>(
    states: &mut States<'_>,
    readings: impl IntoIterator<Item = Judged<'a>>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for judged in readings {
        let reading = judged.reading;
        let named =
            iter::once((&reading.state, true)).chain(reading.against.iter().map(|a| (a, false)));
        for (state, drawn) in named.filter(|(s, _)| !s.is_current()) {
            states.load(state);
            let missing = match states.get(state) {
                Err(error) => vec![format!("{} does not load: {error:#}", state.text())],
                Ok((target, _)) if drawn => lacks(target, reading, state),
                Ok(_) => Vec::new(),
            };
            out.extend(missing.into_iter().map(|why| {
                let message = format!("{}: {why}", judged.label);
                Diagnostic::new(judged.code, judged.subjects.clone(), message)
            }));
        }
    }
    out
}

/// `WALKTHROUGH_EMPTY` for a walkthrough without beats, and `WALKTHROUGH_TARGET_MISSING` for a
/// beat whose target the current state's `states` cannot draw, as `targets` judges it.
pub fn walkthroughs(states: &mut States<'_>) -> Vec<Diagnostic> {
    let model = states.current();
    let mut out = Vec::new();
    let mut beats = Vec::new();
    for walkthrough in &model.walkthroughs {
        let id = model.qualify(&walkthrough.id);
        let name = &walkthrough.name;
        if walkthrough.beats.is_empty() {
            let message = format!("{name} has no beats");
            out.push(Diagnostic::new(
                DiagnosticCode::WalkthroughEmpty,
                vec![id.clone()],
                message,
            ));
        }
        beats.extend((1..).zip(&walkthrough.beats).map(|(n, beat)| Judged {
            code: DiagnosticCode::WalkthroughTargetMissing,
            subjects: vec![id.clone(), model.qualify(&beat.id)],
            label: format!("{name}, beat {n} ({})", beat.title),
            reading: &beat.reading,
        }));
    }
    out.extend(targets(states, beats));
    out
}

/// The scope, focus and flow of `reading` that `target`, the state named `state`, does not hold.
fn lacks(target: &Model, reading: &BeatReading, state: &ReadingState) -> Vec<String> {
    let place = state.text();
    let (modules, flows) =
        vault::reading_ids(&target.manifest.id, &target.modules, &target.collections);
    let mut out = Vec::new();
    let named = (reading.scope.iter().map(|s| ("scope", s)))
        .chain(reading.focus.iter().map(|f| ("focus", f)));
    for (field, module) in named.filter(|(_, m)| !modules.contains(*m)) {
        out.push(format!("{field} {module} is not a module of {place}"));
    }
    if let Some(flow) = reading.flow.as_ref().filter(|f| !flows.contains(*f)) {
        out.push(format!("flow {flow} is not a flow of {place}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::DiagnosticCode::*;
    use super::*;
    use crate::arch::diagnostic::Severity;
    use crate::arch::ids;
    use crate::arch::types::{Collection, Relationship, SCHEMA_VERSION};

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
        check(M, modules, &[])
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
        assert_eq!(check(M, &modules, &[]), []);
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
    fn flows_without_a_summary_and_messages_without_a_relationship_are_information() {
        let mut container = module("Box", None, None);
        link(&mut container, "R1", "Db", "call");
        let modules = [
            container,
            module("Part", Some("Box"), None),
            module("Other", Some("Box"), None),
            module("Db", None, None),
            module("Web", None, None),
        ];
        let message = |id: &str, from: &str, to: &str| serde_json::json!({"kind": "message", "id": id, "from": q(from), "to": q(to), "label": "x"});
        let collection: Collection = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0, "flows": [
                {"id": "Flow0001", "name": "Told", "order": 0, "summary": "Has one", "steps": [
                    // Drawn from the parts' box, either way round; within the box needs none.
                    message("S1", "Part", "Db"),
                    message("S2", "Db", "Other"),
                    message("S3", "Box", "Other"),
                    {"kind": "loop", "id": "L1", "label": "again", "steps": [message("S4", "Web", "Db")]},
                ]},
                {"id": "Flow0002", "name": "Untold", "order": 1},
            ],
        }))
        .unwrap();
        let mut collection = collection;
        // A in P in Q and B in Q: the line P -> Q joins nothing between A and B.
        let mut p = module("P", Some("Q"), None);
        link(&mut p, "R2", "Q", "call");
        let modules: Vec<Module> = modules
            .into_iter()
            .chain([
                module("Q", None, None),
                p,
                module("A", Some("P"), None),
                module("B", Some("Q"), None),
            ])
            .collect();
        collection.flows[0]
            .steps
            .push(serde_json::from_value(message("S5", "A", "B")).unwrap());
        let found: Vec<_> = check(M, &modules, &[collection])
            .into_iter()
            .filter(|d| matches!(d.code, FlowSummaryMissing | MessageUndrawn))
            .map(|d| (d.code, d.severity, d.subjects, d.message))
            .collect();
        assert_eq!(
            found,
            [
                (
                    FlowSummaryMissing,
                    Severity::Information,
                    vec![q("Flow0002")],
                    "Untold has no summary".to_owned()
                ),
                (
                    MessageUndrawn,
                    Severity::Information,
                    vec![q("S4"), q("Web"), q("Db")],
                    "Told: Web -> Db has no relationship between them".to_owned()
                ),
                (
                    MessageUndrawn,
                    Severity::Information,
                    vec![q("S5"), q("A"), q("B")],
                    "Told: A -> B has no relationship between them".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn diagnostics_serialize_with_screaming_codes_and_omit_absent_suggestions() {
        let modules = [module("Box", None, None), module("Part", Some("Box"), None)];
        let value = serde_json::to_value(check(M, &modules, &[])).unwrap();
        assert_eq!(value[0]["code"], "CONTAINER_UNBOUND");
        assert_eq!(value[0]["severity"], "warning");
        assert!(value[0].get("suggested_binding").is_none());
        assert_eq!(value[1]["code"], "CONTAINER_PASS_THROUGH");
        assert_eq!(value[1]["severity"], "information");
    }
}
