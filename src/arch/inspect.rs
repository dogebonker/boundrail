//! `model inspect`: the interface of each bound module as the scan shows it. Files map to modules
//! as the code check maps them, the deepest judged binding winning, so the counts and the check
//! agree on where a module's code is. Outside a module means any file it does not own, its parts'
//! files and unbound files included.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use super::code::{self, NoScan, Ownership, SkipReason};
use super::index::Index;
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
