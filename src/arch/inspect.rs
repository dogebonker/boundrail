//! `model inspect`: the interface of each bound module as the scan shows it. Files map to modules
//! as the code check maps them, the deepest judged binding winning, so the counts and the check
//! agree on where a module's code is. Outside a module means any file it does not own, its parts'
//! files and unbound files included.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use super::code::{self, NoScan, Ownership, SkipReason};
use super::index::{Index, Line};
use super::infra::Infra;
use super::lsp::Facts;
use super::types::{Evidence, Module};
use crate::model::Document;
use crate::surface::{SymbolExport, Symbols as SymbolData};

#[derive(Debug, Default, Serialize)]
pub struct InspectReport {
    /// The modules whose code was found, by qualified ID.
    pub modules: Vec<Interface>,
    /// The modules whose code was not judged, with the code check's reason.
    pub skips: Vec<Skip>,
    /// Language servers that failed, or the config when language servers are not enabled.
    pub language_servers: Vec<code::Skip>,
}

#[derive(Debug, Serialize)]
pub struct Skip {
    pub module: String,
    pub reason: SkipReason,
}

/// One module's interface.
#[derive(Debug, Serialize)]
pub struct Interface {
    pub module: String,
    pub name: String,
    /// Scanned files the module owns.
    pub files: usize,
    /// Entry files directly in its bound directory and files a package publishes below it.
    pub entry_points: Vec<String>,
    /// `None` when the scan has no symbol data for the module's files, or not all of it.
    pub symbols: Option<Symbols>,
    /// Why `symbols` is `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols_skipped: Option<SkipReason>,
    /// Imports from outside the module past its entry points.
    pub deep_imports: Vec<DeepImport>,
    pub incoming: Vec<Neighbour>,
    pub outgoing: Vec<Neighbour>,
    /// Infrastructure hops from other modules into this one.
    pub hops_in: Vec<HopEntry>,
    /// Infrastructure hops from this module to others.
    pub hops_out: Vec<HopEntry>,
}

/// A configured address or build dependency joining a module with one other module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct HopEntry {
    /// The other module.
    pub module: String,
    pub evidence: Evidence,
    pub file: String,
    pub line: usize,
    pub variable: String,
    pub address: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Symbols {
    /// Exports of its entry points, or of every file it owns when it has none.
    pub exported: usize,
    /// Exported symbols a file outside the module imports, or references when `source` is
    /// `language-server`.
    pub used: usize,
    /// Exported symbols no file outside the module imports: candidates to make internal.
    /// `None` when the scan's symbol coverage is incomplete, as the scan's surface withholds
    /// its could-be-internal list.
    pub unused: Option<Vec<SymbolExport>>,
    pub source: Source,
}

/// What `used` and `unused` were judged by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// References a language server found.
    LanguageServer,
    /// The scan's imports.
    Scan,
}

/// A file outside a module importing a file inside it that is not an entry point.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DeepImport {
    pub from: String,
    /// The file as the import names it, before a facade resolves it.
    pub to: String,
}

/// Import edges between a module and one other module, one direction.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Neighbour {
    pub module: String,
    pub edges: usize,
    /// Relationships drawn that way between the two modules or modules containing them.
    pub relationships: Vec<String>,
}

/// The interface of every module of `modules`, or of `subject` alone (a qualified ID), against
/// the checkout at `root`, its scan, its infrastructure and what language servers found.
pub fn inspect(
    model: &str,
    modules: &[Module],
    root: &Path,
    scan: Result<&Document, NoScan>,
    infra: &Infra,
    subject: Option<&str>,
    lsp: &Facts,
) -> InspectReport {
    let index = Index::new(model, modules);
    let doc = scan.as_ref().ok().copied();
    let own = code::ownership(&index, root, scan, infra);
    let grouped = Grouped::new(&own, doc);
    let uses = doc.and_then(|d| Some(Uses::new(d.symbols.as_ref()?, d, lsp)));
    let mut report = InspectReport {
        language_servers: lsp.skips.clone(),
        ..InspectReport::default()
    };
    for (id, status) in &own.status {
        if subject.is_some_and(|s| s != id) {
            continue;
        }
        match status {
            Err(reason) => report.skips.push(Skip {
                module: id.clone(),
                reason: *reason,
            }),
            Ok(()) => {
                let interface = interface(&index, &own, &grouped, uses.as_ref(), id);
                report.modules.push(interface);
            }
        }
    }
    report
}

fn interface(
    index: &Index<'_>,
    own: &Ownership<'_>,
    grouped: &Grouped<'_>,
    uses: Option<&Uses<'_>>,
    id: &str,
) -> Interface {
    let empty = BTreeSet::new();
    let owned = grouped.owned.get(id).unwrap_or(&empty);
    let entry_points: Vec<String> = own.entry_points(id).map(str::to_owned).collect();
    let (symbols, symbols_skipped) = match symbols(own, uses, owned, id) {
        Ok(symbols) => (Some(symbols), None),
        Err(reason) => (None, Some(reason)),
    };
    let neighbours = |pairs: &HashMap<&str, BTreeMap<&str, usize>>, outgoing: bool| {
        (pairs.get(id).into_iter().flatten())
            .map(|(&other, &edges)| {
                let (from, to) = if outgoing { (id, other) } else { (other, id) };
                Neighbour {
                    module: other.to_owned(),
                    edges,
                    relationships: index.covering(from, to).map(|l| l.id.clone()).collect(),
                }
            })
            .collect()
    };
    Interface {
        module: id.to_owned(),
        name: index.name(id).to_owned(),
        files: owned.len(),
        entry_points,
        symbols,
        symbols_skipped,
        deep_imports: grouped
            .deep
            .get(id)
            .into_iter()
            .flatten()
            .map(Clone::clone)
            .collect(),
        incoming: neighbours(&grouped.incoming, false),
        outgoing: neighbours(&grouped.outgoing, true),
        hops_in: hops(own, id, false),
        hops_out: hops(own, id, true),
    }
}

/// One module's interface as the model draws it, read from relationships alone with no scan.
/// Inside the module means the module and every module below it; a relationship with one end
/// inside reaches the direct part holding that end, or the module itself when the end is the
/// module. Outside means neither inside nor a module holding this one: a relationship between
/// the module and a container above it joins a container and its own part
/// (`CONTAINER_LINE_TO_OWN_PART`) and counts nowhere here.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Drawn {
    pub module: String,
    pub name: String,
    /// Direct parts an outside relationship reaches, or the module itself when it has no parts
    /// and one reaches it, each with those relationships.
    pub entry_points: Vec<Reach>,
    /// Direct parts no outside relationship reaches.
    pub hidden_parts: Vec<String>,
    /// Outside modules with relationships into the module, each with those relationships.
    pub callers: Vec<Reach>,
    /// Outside modules the module's relationships go to, each with those relationships.
    pub outgoing: Vec<Reach>,
    /// Entry points without parts of their own that send a relationship outside and whose every
    /// relationship has its other end outside the module: they only forward from outside to
    /// outside. Not the `CONTAINER_PASS_THROUGH` diagnostic, which flags a container with
    /// exactly one part.
    pub pass_through: Vec<String>,
    /// Relationships between the module itself and the outside while it has parts: they name
    /// the container rather than the part that does the work.
    pub on_container: Vec<String>,
    pub measures: Measures,
}

/// A module and the relationships joining it with the inspected one or group.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Reach {
    pub module: String,
    pub relationships: Vec<String>,
}

/// The counts of a [`Drawn`] interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Measures {
    pub entry_points: usize,
    pub hidden_parts: usize,
    pub callers: usize,
    pub outgoing: usize,
    /// Levels of containment below the module: 0 without parts, 1 with parts that have none.
    pub depth: usize,
}

/// The drawn interface of every module of `modules`, or of `subject` alone (a qualified ID),
/// sorted by qualified ID.
pub fn drawn(model: &str, modules: &[Module], subject: Option<&str>) -> Vec<Drawn> {
    let index = Index::new(model, modules);
    // Each relationship under every module holding one of its ends, so each module reads only
    // the relationships with an end inside it.
    let mut touching: HashMap<&str, Vec<&Line<'_>>> = HashMap::new();
    for line in &index.lines {
        let mut holders = index.chain(&line.from);
        holders.extend(index.chain(line.to));
        holders.sort_unstable();
        holders.dedup();
        for holder in holders {
            touching.entry(holder).or_default().push(line);
        }
    }
    let read = |(id, module): (&String, &&Module)| {
        let lines = touching.get(id.as_str()).map_or(&[][..], Vec::as_slice);
        drawn_interface(&index, lines, id, &module.name)
    };
    match subject {
        Some(subject) => index
            .by_id
            .get_key_value(subject)
            .map(read)
            .into_iter()
            .collect(),
        None => index.by_id.iter().map(read).collect(),
    }
}

/// The drawn interface of module `id` from `lines`, the relationships with an end inside it.
fn drawn_interface(index: &Index<'_>, lines: &[&Line<'_>], id: &str, name: &str) -> Drawn {
    let parts = index.parts_of(id);
    let place = places(index, id);
    let container = !parts.is_empty();
    let mut reached: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut callers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let (mut sends, mut joined) = (HashSet::new(), HashSet::new());
    let mut on_container = Vec::new();
    for line in lines {
        let (from, to, rel) = (line.from.as_str(), line.to, line.id.as_str());
        match (place.get(from), place.get(to)) {
            // The outside end holds this module: a container and its own part.
            (None, Some(_)) if index.is_below(id, from) => {}
            (Some(_), None) if index.is_below(id, to) => {}
            (None, Some(&inside)) => {
                callers.entry(from).or_default().push(rel);
                if container && inside == id {
                    on_container.push(rel);
                } else {
                    reached.entry(inside).or_default().push(rel);
                }
            }
            (Some(&inside), None) => {
                outgoing.entry(to).or_default().push(rel);
                if container && inside == id {
                    on_container.push(rel);
                } else {
                    sends.insert(inside);
                }
            }
            (Some(&from), Some(&to)) => joined.extend([from, to]),
            (None, None) => {}
        }
    }
    let reach = |found: BTreeMap<&str, Vec<&str>>| -> Vec<Reach> {
        (found.into_iter())
            .map(|(module, mut relationships)| {
                relationships.sort_unstable();
                Reach {
                    module: module.to_owned(),
                    relationships: relationships.into_iter().map(str::to_owned).collect(),
                }
            })
            .collect()
    };
    let mut hidden_parts: Vec<String> = (parts.iter())
        .filter(|(part, _)| !reached.contains_key(part.as_str()))
        .map(|(part, _)| part.clone())
        .collect();
    hidden_parts.sort();
    let pass_through: Vec<String> = (reached.keys())
        .filter(|p| **p != id && !index.is_container(p))
        .filter(|p| sends.contains(*p) && !joined.contains(*p))
        .map(|p| (*p).to_owned())
        .collect();
    on_container.sort_unstable();
    let (entry_points, callers, outgoing) = (reach(reached), reach(callers), reach(outgoing));
    Drawn {
        module: id.to_owned(),
        name: name.to_owned(),
        measures: Measures {
            entry_points: entry_points.len(),
            hidden_parts: hidden_parts.len(),
            callers: callers.len(),
            outgoing: outgoing.len(),
            depth: depth(index, id),
        },
        entry_points,
        hidden_parts,
        callers,
        outgoing,
        pass_through,
        on_container: on_container.into_iter().map(str::to_owned).collect(),
    }
}

/// The direct part of `id` holding each module inside it, and `id` itself for itself.
fn places<'s>(index: &'s Index<'_>, id: &'s str) -> HashMap<&'s str, &'s str> {
    let mut place = HashMap::from([(id, id)]);
    for (part, _) in index.parts_of(id) {
        place.insert(part, part);
        place.extend((index.descendants(part).into_iter()).map(|(d, _)| (d, part.as_str())));
    }
    place
}

/// Levels of containment below `id`: 0 without parts, 1 when no part has parts.
fn depth(index: &Index<'_>, id: &str) -> usize {
    (index.parts_of(id).iter())
        .map(|(part, _)| 1 + depth(index, part))
        .max()
        .unwrap_or(0)
}

/// A configured group as the model draws it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct GroupReport {
    /// The group's key in the config.
    pub group: String,
    pub name: String,
    /// Modules naming the group, by qualified ID.
    pub members: Vec<String>,
    /// Relationships between two different members.
    pub internal: Vec<String>,
    /// Relationships with one end inside a member and the other outside every member.
    pub boundary: Vec<Crossing>,
    /// The outside modules at the other end of a boundary relationship.
    pub neighbours: Vec<String>,
}

/// A relationship crossing a group's boundary.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Crossing {
    pub relationship: String,
    pub direction: Direction,
    /// The innermost member holding the inside end.
    pub member: String,
    /// The outside end.
    pub neighbour: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// Into the group.
    In,
    /// Out of the group.
    Out,
}

/// Group `key` (named `name`) of the model's `modules`. A relationship reaching a member's part
/// reaches that member, the innermost one when members nest; one within a single member joins
/// no two members and crosses no boundary.
pub fn group(model: &str, modules: &[Module], key: &str, name: &str) -> GroupReport {
    let index = Index::new(model, modules);
    let members: BTreeSet<&str> = (index.by_id.iter())
        .filter(|(_, m)| m.groups.iter().any(|g| g == key))
        .map(|(id, _)| id.as_str())
        .collect();
    let member =
        |module: &str| (index.chain(module).into_iter()).find_map(|m| members.get(m).copied());
    let (mut internal, mut boundary, mut neighbours) = (Vec::new(), Vec::new(), BTreeSet::new());
    for line in &index.lines {
        let (from, to) = (line.from.as_str(), line.to);
        let crossing = match (member(from), member(to)) {
            (Some(a), Some(b)) if a != b => {
                internal.push(line.id.clone());
                continue;
            }
            (Some(member), None) => (Direction::Out, member, to),
            (None, Some(member)) => (Direction::In, member, from),
            _ => continue,
        };
        let (direction, member, neighbour) = crossing;
        neighbours.insert(neighbour.to_owned());
        boundary.push(Crossing {
            relationship: line.id.clone(),
            direction,
            member: member.to_owned(),
            neighbour: neighbour.to_owned(),
        });
    }
    internal.sort();
    boundary.sort();
    GroupReport {
        group: key.to_owned(),
        name: name.to_owned(),
        members: members.into_iter().map(str::to_owned).collect(),
        internal,
        boundary,
        neighbours: neighbours.into_iter().collect(),
    }
}

/// The hops from (`outgoing`) or into module `id`, sorted.
fn hops(own: &Ownership<'_>, id: &str, outgoing: bool) -> Vec<HopEntry> {
    let mut found: Vec<HopEntry> = (own.hops.iter())
        .filter_map(|(from, to, hop)| {
            let (here, other) = if outgoing { (from, to) } else { (to, from) };
            (here == id).then(|| HopEntry {
                module: other.clone(),
                evidence: hop.evidence,
                file: hop.file.clone(),
                line: hop.line,
                variable: hop.variable.clone(),
                address: hop.address.clone(),
            })
        })
        .collect();
    found.sort();
    found
}

/// The scan's facts grouped by owning module, each built in one pass.
struct Grouped<'o> {
    owned: HashMap<&'o str, BTreeSet<&'o str>>,
    deep: HashMap<&'o str, BTreeSet<DeepImport>>,
    /// Import edges to each other module, by source module and by target module.
    outgoing: HashMap<&'o str, BTreeMap<&'o str, usize>>,
    incoming: HashMap<&'o str, BTreeMap<&'o str, usize>>,
}

impl<'o> Grouped<'o> {
    fn new(own: &'o Ownership<'_>, doc: Option<&Document>) -> Self {
        let mut grouped = Self {
            owned: own.owned_files(),
            deep: HashMap::new(),
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
        };
        for (id, edge) in &own.deep {
            grouped
                .deep
                .entry(id.as_str())
                .or_default()
                .insert(DeepImport {
                    from: edge.from.clone(),
                    to: code::written(edge).to_owned(),
                });
        }
        let edges = doc.map_or(&[][..], |d| d.edges.as_slice());
        let owner = |file: &str| own.owners.get(file).map(String::as_str);
        for (from, to, _) in code::owner_pairs(edges, owner) {
            *grouped
                .outgoing
                .entry(from)
                .or_default()
                .entry(to)
                .or_default() += 1;
            *grouped
                .incoming
                .entry(to)
                .or_default()
                .entry(from)
                .or_default() += 1;
        }
        grouped
    }
}

/// Who imports what, built once from the scan's symbol data and edges.
struct Uses<'d> {
    data: &'d SymbolData,
    /// Importers of each resolved target, by its file and name.
    by_target: HashMap<(&'d str, &'d str), Vec<&'d str>>,
    /// Importers of an unresolved name from a file.
    by_name: HashMap<(&'d str, &'d str), Vec<&'d str>>,
    /// Importers of every export of a file: unresolved wildcard imports, and edges from a file
    /// with no symbol import of that file.
    by_file: HashMap<&'d str, Vec<&'d str>>,
    /// Whether a limitation names no scanned file, so coverage is incomplete everywhere.
    incomplete: bool,
    /// Scanned files referring to each definition, by its file and name, as language servers
    /// found them.
    referrers: HashMap<(&'d str, &'d str), Vec<&'d str>>,
    /// The modules whose interface the language servers were asked about in full.
    served: &'d HashSet<String>,
}

impl<'d> Uses<'d> {
    fn new(data: &'d SymbolData, doc: &'d Document, lsp: &'d Facts) -> Self {
        let mut uses = Self {
            data,
            by_target: HashMap::new(),
            by_name: HashMap::new(),
            by_file: HashMap::new(),
            incomplete: false,
            referrers: HashMap::new(),
            served: &lsp.queried,
        };
        let mut named: HashSet<(&str, &str)> = HashSet::new();
        for import in &data.imports {
            let from = import.from.as_str();
            named.insert((from, import.to.as_str()));
            match &import.target {
                Some(target) => {
                    let key = (target.file.as_str(), target.name.as_str());
                    uses.by_target.entry(key).or_default().push(from);
                }
                None if import.wildcard => uses.by_file.entry(&import.to).or_default().push(from),
                None => (uses.by_name.entry((&import.to, &import.name)).or_default()).push(from),
            }
        }
        for edge in &doc.edges {
            let to = code::written(edge);
            if !named.contains(&(edge.from.as_str(), to)) {
                uses.by_file.entry(to).or_default().push(&edge.from);
            }
        }
        let scanned: HashSet<&str> = doc.files.iter().map(|f| f.path.as_str()).collect();
        for r in lsp
            .references
            .iter()
            .filter(|r| scanned.contains(r.from.as_str()))
        {
            let key = (r.file.as_str(), r.name.as_str());
            uses.referrers.entry(key).or_default().push(&r.from);
        }
        uses.incomplete = !data.complete
            && (data.limitations.iter()).any(|l| {
                l.split_once(':')
                    .is_none_or(|(path, _)| !scanned.contains(path))
            });
        uses
    }

    /// Whether a file outside `owned` imports `export`.
    fn used(&self, export: &SymbolExport, owned: &BTreeSet<&str>) -> bool {
        let importers = [
            (self.by_target).get(&(export.target.file.as_str(), export.target.name.as_str())),
            self.by_name
                .get(&(export.file.as_str(), export.name.as_str())),
            self.by_file.get(export.file.as_str()),
        ];
        (importers.into_iter().flatten().flatten()).any(|from| !owned.contains(from))
    }

    /// Whether a scanned file outside `owned` refers to `export`'s definition.
    fn referred(&self, export: &SymbolExport, owned: &BTreeSet<&str>) -> bool {
        let key = (export.target.file.as_str(), export.target.name.as_str());
        (self.referrers.get(&key).into_iter().flatten()).any(|from| !owned.contains(from))
    }
}

/// The exports of a module's interface files and who outside it uses them, or why not known.
/// When language servers were asked about the whole interface of module `id`, their references
/// decide what is used.
fn symbols(
    own: &Ownership<'_>,
    uses: Option<&Uses<'_>>,
    owned: &BTreeSet<&str>,
    id: &str,
) -> Result<Symbols, SkipReason> {
    let uses = uses.ok_or(SkipReason::LanguageUnsupported)?;
    if owned.iter().any(|f| own.limited.contains(f)) {
        return Err(SkipReason::SymbolsIncomplete);
    }
    let exported = own.interface_exports(&uses.data.exports, id, owned);
    let served = uses.served.contains(id);
    let used = |e: &SymbolExport| {
        if served {
            uses.referred(e, owned)
        } else {
            uses.used(e, owned)
        }
    };
    let unused: Vec<SymbolExport> = (exported.iter())
        .filter(|e| !used(e))
        .map(|&e| e.clone())
        .collect();
    Ok(Symbols {
        exported: exported.len(),
        used: exported.len() - unused.len(),
        unused: (served || !uses.incomplete).then_some(unused),
        source: if served {
            Source::LanguageServer
        } else {
            Source::Scan
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::code::tests::{M, checkout, link, module, q, shop};
    use crate::surface::{SymbolImport, SymbolTarget};

    fn export(file: &str, name: &str, target: &str) -> SymbolExport {
        SymbolExport {
            file: file.into(),
            name: name.into(),
            target: SymbolTarget {
                file: target.into(),
                name: name.into(),
            },
        }
    }

    fn import(from: &str, to: &str, name: &str, target: &str) -> SymbolImport {
        SymbolImport {
            from: from.into(),
            to: to.into(),
            name: name.into(),
            target: Some(SymbolTarget {
                file: target.into(),
                name: name.into(),
            }),
            wildcard: false,
        }
    }

    fn of<'r>(report: &'r InspectReport, local: &str) -> &'r Interface {
        let id = q(local);
        report.modules.iter().find(|m| m.module == id).unwrap()
    }

    /// Billing's index re-exports `charge` and exports `refund`; Web imports `charge` through
    /// the index, Billing's charge imports Bank's `pay`.
    #[test]
    fn interfaces_count_entry_points_symbols_deep_imports_and_neighbours() {
        let (dir, mut doc) = checkout();
        let symbols = doc.symbols.as_mut().unwrap();
        symbols.exports.extend([
            export("app/billing/index.ts", "charge", "app/billing/charge.ts"),
            export("app/billing/index.ts", "refund", "app/billing/index.ts"),
        ]);
        symbols.imports = vec![
            import(
                "app/web/index.ts",
                "app/billing/index.ts",
                "charge",
                "app/billing/charge.ts",
            ),
            import(
                "app/billing/charge.ts",
                "bank/index.ts",
                "pay",
                "bank/index.ts",
            ),
        ];
        let mut modules = shop().to_vec();
        link(&mut modules[1], "WebBill1", "Billing", "call");
        modules.push(module("Loose", None, None));
        let none = Facts::default();
        let report = inspect(
            M,
            &modules,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            None,
            &none,
        );

        let billing = of(&report, "Billing");
        assert_eq!(billing.files, 2);
        assert_eq!(billing.entry_points, ["app/billing/index.ts"]);
        let refund = export("app/billing/index.ts", "refund", "app/billing/index.ts");
        let expected = Symbols {
            exported: 2,
            used: 1,
            unused: Some(vec![refund]),
            source: Source::Scan,
        };
        assert_eq!(billing.symbols.as_ref(), Some(&expected));
        let deep: Vec<(&str, &str)> = (billing.deep_imports.iter())
            .map(|d| (d.from.as_str(), d.to.as_str()))
            .collect();
        assert_eq!(
            deep,
            [
                ("app/web/page.ts", "app/billing/charge.ts"),
                ("shared/index.ts", "app/billing/charge.ts"),
            ]
        );
        let neighbour = |module: &str, edges, relationships: &[&str]| Neighbour {
            module: q(module),
            edges,
            relationships: relationships.iter().map(|r| q(r)).collect(),
        };
        assert_eq!(billing.incoming, [neighbour("Web", 2, &["WebBill1"])]);
        assert_eq!(billing.outgoing, [neighbour("Bank", 1, &[])]);
        // App owns only the cart; its parts own the rest.
        assert_eq!(of(&report, "App").files, 1);
        let bank = of(&report, "Bank").symbols.as_ref().unwrap();
        assert_eq!(
            (bank.exported, bank.used, &bank.unused),
            (1, 1, &Some(Vec::new()))
        );
        let skips: Vec<(&str, SkipReason)> = (report.skips.iter())
            .map(|s| (s.module.as_str(), s.reason))
            .collect();
        assert_eq!(skips, [(q("Loose").as_str(), SkipReason::Unbound)]);
    }

    #[test]
    fn symbols_are_skipped_without_data_or_with_limited_files_and_one_subject_filters() {
        let (dir, mut doc) = checkout();
        let modules = shop();
        let only = q("Bank");
        let report = inspect(
            M,
            &modules,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            Some(&only),
            &Facts::default(),
        );
        assert_eq!(report.modules.len(), 1);
        // Billing's charge imports Bank's index with no symbol import: a use of all it exports.
        let expected = Symbols {
            exported: 1,
            used: 1,
            unused: Some(Vec::new()),
            source: Source::Scan,
        };
        assert_eq!(report.modules[0].symbols.as_ref(), Some(&expected));

        // A limitation naming no file makes coverage incomplete: candidates are withheld.
        let symbols = doc.symbols.as_mut().unwrap();
        symbols.limitations = vec!["CommonJS export surfaces are not indexed.".into()];
        let report = inspect(
            M,
            &modules,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            Some(&only),
            &Facts::default(),
        );
        let symbols = report.modules[0].symbols.as_ref().unwrap();
        assert_eq!(
            (symbols.exported, symbols.used, &symbols.unused),
            (1, 1, &None)
        );

        doc.symbols.as_mut().unwrap().limitations = vec!["bank/index.ts: CommonJS".into()];
        let report = inspect(
            M,
            &modules,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            Some(&only),
            &Facts::default(),
        );
        assert_eq!(
            report.modules[0].symbols_skipped,
            Some(SkipReason::SymbolsIncomplete)
        );

        doc.symbols = None;
        let report = inspect(
            M,
            &modules,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            Some(&only),
            &Facts::default(),
        );
        assert_eq!(report.modules[0].symbols, None);
        assert_eq!(
            report.modules[0].symbols_skipped,
            Some(SkipReason::LanguageUnsupported)
        );

        let report = inspect(
            M,
            &modules,
            dir.path(),
            Err(NoScan::NotNeeded),
            &Infra::default(),
            None,
            &Facts::default(),
        );
        assert!(report.modules.is_empty());
        assert!(
            report
                .skips
                .iter()
                .all(|s| s.reason == SkipReason::NotScanned)
        );
    }
}
