//! The code check: compares the authored model with the checkout and its scanned import graph.
//! Bindings and anchors must name existing code, code edges between bound modules need a drawn
//! relationship, and relationships whose kind expects code evidence need edges that support them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::iter;
use std::path::Path;

use serde::Serialize;

use super::diagnostic::{Diagnostic, DiagnosticCode};
use super::index::{Index, Line};
use super::infra::{self, Hop, Infra, Joined, Unit};
use super::lsp::{Facts, Reference};
use super::segments;
use super::types::{Binding, EXTERNAL_KIND, Evidence, Module, VaultConfig};
use super::vault::parse_message;
use crate::model::{Document, Edge, EdgeKind, Language};
use crate::modules::dir_of;
use crate::surface::{SymbolExport, is_entry_file};

/// Why a binding, anchor or relationship was not judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    Unbound,
    PathMissing,
    External,
    /// The binding or anchor names another repository.
    OtherRepository,
    /// The relationship's kind expects no evidence the scan collects.
    EvidenceNotCode,
    /// No edge supports the relationship, and part of its kind's evidence is not collected.
    EvidenceUnavailable,
    /// The scan has no symbol data to look a symbol up in.
    LanguageUnsupported,
    /// The symbol was not found, and the scan reports incomplete symbols for a file there.
    SymbolsIncomplete,
    /// The module shares its path with another bound module and stands for a symbol there;
    /// symbol-level references are not collected.
    SymbolBinding,
    /// The path exists but holds no scanned file.
    NotScanned,
    /// The path exists but the scan failed.
    ScanFailed,
    /// No module binds the infrastructure unit.
    InfraUnbound,
    /// Language servers are configured but were not enabled with `--language-servers`.
    LanguageServersNotEnabled,
    /// The language server failed; the check went without it.
    LanguageServerFailed,
    /// The picture could not be drawn or its file read, so its freshness is unknown.
    Unrenderable,
    /// The beat or picture reads a checkpoint, and the history ref is not in this repository.
    HistoryNotLocal,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::PathMissing => "path-missing",
            Self::External => "external",
            Self::OtherRepository => "other-repository",
            Self::EvidenceNotCode => "evidence-not-code",
            Self::EvidenceUnavailable => "evidence-unavailable",
            Self::LanguageUnsupported => "language-unsupported",
            Self::SymbolsIncomplete => "symbols-incomplete",
            Self::SymbolBinding => "symbol-binding",
            Self::NotScanned => "not-scanned",
            Self::ScanFailed => "scan-failed",
            Self::InfraUnbound => "infra-unbound",
            Self::LanguageServersNotEnabled => "language-servers-not-enabled",
            Self::LanguageServerFailed => "language-server-failed",
            Self::Unrenderable => "unrenderable",
            Self::HistoryNotLocal => "history-not-local",
        }
    }
}

/// What a skip concerns: a module's binding, a relationship's anchor, the relationship, a
/// language server (its name, or the config file), a walkthrough's beat, or a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipSite {
    Binding,
    Anchor,
    Relationship,
    LanguageServer,
    Walkthrough,
    Picture,
}

impl SkipSite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binding => "binding",
            Self::Anchor => "anchor",
            Self::Relationship => "relationship",
            Self::LanguageServer => "language-server",
            Self::Walkthrough => "walkthrough",
            Self::Picture => "picture",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Skip {
    /// Qualified ID of the module, relationship, beat or picture, or a language server's name.
    pub subject: String,
    pub site: SkipSite,
    pub reason: SkipReason,
}

#[derive(Debug, Default)]
pub struct CodeReport {
    pub diagnostics: Vec<Diagnostic>,
    pub skips: Vec<Skip>,
    /// Every infrastructure unit found, with the module it maps to.
    pub units: Vec<UnitStatus>,
}

/// An infrastructure unit and the module it maps to.
#[derive(Debug, Serialize)]
pub struct UnitStatus {
    #[serde(flatten)]
    pub unit: Unit,
    /// The module bound to it, else the owner of the directory a compose service builds from.
    pub owner: Option<String>,
    /// `infra-unbound` when no module owns it.
    pub reason: Option<SkipReason>,
}

impl CodeReport {
    fn skip(&mut self, site: SkipSite, subject: &str, reason: SkipReason) {
        self.skips.push(Skip {
            subject: subject.to_owned(),
            site,
            reason,
        });
    }

    fn warn(&mut self, code: DiagnosticCode, subjects: Vec<String>, message: String) {
        self.diagnostics
            .push(Diagnostic::new(code, subjects, message));
    }
}

/// Which evidence is collected for which modules.
struct Collected<'i> {
    /// The modules holding an infrastructure unit, and the modules containing them; empty
    /// unless a module is bound to a unit, which opts the model into infrastructure judging.
    configured: HashSet<&'i str>,
    /// The modules holding a workspace package folder, and the modules containing them.
    build: HashSet<&'i str>,
    /// The modules bound to a unit, which hold no code.
    units: HashSet<&'i str>,
}

impl<'i> Collected<'i> {
    fn new(index: &'i Index<'_>, joined: &Joined<'i, '_>) -> Self {
        let held = |owners: &[&'i str]| -> HashSet<&'i str> {
            (owners.iter())
                .flat_map(|&owner| index.chain(owner))
                .collect()
        };
        let units: HashSet<&str> = (index.by_id.iter())
            .filter(|(_, m)| m.binding.as_ref().is_some_and(infra::is_unit_binding))
            .map(|(id, _)| id.as_str())
            .collect();
        let owners: Vec<&str> = joined.owners.iter().flatten().copied().collect();
        Self {
            configured: if units.is_empty() {
                HashSet::new()
            } else {
                held(&owners)
            },
            build: held(&joined.packages),
            units,
        }
    }

    /// The kinds of `accepted` that apply to `line`: code evidence never applies to a module
    /// bound to a unit.
    fn applicable(&self, accepted: &[Evidence], line: &Line<'_>) -> Vec<Evidence> {
        let unit = self.units.contains(line.from.as_str()) || self.units.contains(line.to);
        (accepted.iter().copied())
            .filter(|e| !(unit && matches!(e, Evidence::Call | Evidence::Import)))
            .collect()
    }

    /// Whether evidence of this kind is collected for a relationship from `from` to `to`.
    /// Import edges stand in for call evidence where language servers do not judge both ends;
    /// configured paths are collected between modules holding units, and build dependencies
    /// between modules holding workspace packages.
    fn holds(&self, evidence: Evidence, from: &str, to: &str) -> bool {
        let both = |set: &HashSet<&str>| set.contains(from) && set.contains(to);
        match evidence {
            Evidence::Import | Evidence::Call => true,
            Evidence::ConfiguredPath => both(&self.configured),
            Evidence::BuildDependency => both(&self.build),
        }
    }
}

/// Why the check runs without a scan.
#[derive(Debug, Clone)]
pub enum NoScan {
    /// No binding or anchor names existing code, so none was needed.
    NotNeeded,
    /// The scan failed with this error.
    Failed(String),
}

/// Whether a binding of a module that is not external, other than to an infrastructure unit,
/// or any anchor, names an existing path of this repository under `root`: without one there is
/// no code to compare, so no scan is needed.
pub fn needs_scan(modules: &[Module], root: &Path) -> bool {
    let bindings = modules
        .iter()
        .filter(|m| m.kind != EXTERNAL_KIND)
        .filter_map(|m| m.binding.as_ref())
        .filter(|b| !infra::is_unit_binding(b))
        .map(|b| (&b.repo, &b.path));
    let anchors = modules
        .iter()
        .flat_map(|m| &m.outgoing)
        .filter_map(|r| r.anchor.as_ref())
        .map(|a| (&a.repo, &a.path));
    bindings
        .chain(anchors)
        .any(|(repo, path)| repo.is_none() && root.join(path).exists())
}

/// Every code diagnostic of model `model` against the checkout at `root`, its scan, its
/// infrastructure and what language servers found, sorted, with what was not judged. Without a
/// scan, existing paths are skipped as `not-scanned`, or as `scan-failed` with a `SCAN_FAILED`
/// warning; infrastructure that failed to read does the same to modules bound to units. The
/// modules must have loaded, so parents and targets exist and containment has no cycle.
pub fn check(
    model: &str,
    modules: &[Module],
    config: &VaultConfig,
    root: &Path,
    scan: Result<&Document, NoScan>,
    infra: &Infra,
    lsp: &Facts,
) -> CodeReport {
    let index = Index::new(model, modules);
    let (scan, failure) = Scan::of(scan);
    let mut report = CodeReport::default();
    report.skips.extend(lsp.skips.iter().cloned());
    let status = bindings(&index, root, &scan, infra, lsp, &mut report);
    anchors(&index, root, &scan, lsp, &mut report);
    let paths = path_owners(&status);
    let joined = joined(&index, &status, infra, &paths);
    let owners = owners(&status, &paths);
    let served = served(&index, &status, lsp);
    let targets = targets(&index, &status, &owners, &served, lsp);
    let mut pairs = Pairs::new(
        &index,
        &scan,
        &owners,
        &joined.hops,
        &lsp.references,
        (served, targets),
    );
    let collected = Collected::new(&index, &joined);
    // Without a unit binding, configured hops only support relationships: they reveal none.
    if collected.units.is_empty() {
        pairs.configured.clear();
    }
    report.units = unit_statuses(infra, &joined.owners);
    let judged = (&status, &pairs, &collected);
    let backwards = relationships(&index, config, judged, &mut report);
    undrawn(&index, &pairs, &backwards, &mut report);
    entry_points(&index, &scan, &status, &mut report);
    let unit = |subject: &str| collected.units.contains(subject);
    if let Some(error) = failure {
        let what = "the scan of the checkout failed";
        scan_failed(what, &error, |s| !unit(s), &mut report);
    }
    if let Some(error) = &infra.error {
        let what = "reading the checkout's infrastructure failed";
        scan_failed(what, error, unit, &mut report);
    }
    report.diagnostics.sort();
    report.skips.sort();
    report
}

/// Where the code check finds each module's code, for measures that read the same mapping.
pub(super) struct Ownership<'d> {
    /// Every module by qualified ID, with why its code is not judged when it is not.
    pub(super) status: BTreeMap<String, Result<(), SkipReason>>,
    /// Each scanned file's owner: the deepest judged binding holding it.
    pub(super) owners: HashMap<String, String>,
    /// The entry points of each module bound to a directory that has any.
    pub(super) entries: BTreeMap<String, BTreeSet<String>>,
    /// Runtime edges from outside such a module into it, past its entry points, by module.
    pub(super) deep: Vec<(String, &'d Edge)>,
    /// Files a symbol limitation names.
    pub(super) limited: HashSet<&'d str>,
    /// Infrastructure hops between two modules, as (source, target, hop).
    pub(super) hops: Vec<(String, String, &'d Hop)>,
}

impl Ownership<'_> {
    /// The scanned files each module owns.
    pub(super) fn owned_files(&self) -> HashMap<&str, BTreeSet<&str>> {
        let mut owned: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for (file, id) in &self.owners {
            owned.entry(id.as_str()).or_default().insert(file.as_str());
        }
        owned
    }

    /// The entry points of module `id`, sorted.
    pub(super) fn entry_points(&self, id: &str) -> impl Iterator<Item = &str> {
        self.entries
            .get(id)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// The exports of module `id`'s interface: those of its entry points, or of every file it
    /// owns (`owned`) when it has none.
    pub(super) fn interface_exports<'s>(
        &self,
        exports: &'s [SymbolExport],
        id: &str,
        owned: &BTreeSet<&str>,
    ) -> BTreeSet<&'s SymbolExport> {
        let entries = self.entries.get(id);
        (exports.iter())
            .filter(|e| match entries {
                Some(entries) => entries.contains(&e.file),
                None => owned.contains(e.file.as_str()),
            })
            .collect()
    }
}

/// The code check's mapping of the modules of `index` to the code under `root`, without judging
/// the relationships. Diagnostics are dropped: `model check` reports them.
pub(super) fn ownership<'d>(
    index: &Index<'_>,
    root: &Path,
    scan: Result<&'d Document, NoScan>,
    infra: &'d Infra,
) -> Ownership<'d> {
    let (scan, _) = Scan::of(scan);
    let none = Facts::default();
    let status = bindings(index, root, &scan, infra, &none, &mut CodeReport::default());
    let paths = path_owners(&status);
    let hops = (joined(index, &status, infra, &paths).hops.into_iter())
        .map(|(from, to, hop)| (from.to_owned(), to.to_owned(), hop))
        .collect();
    let owners = owners(&status, &paths)
        .into_iter()
        .map(|(file, id)| (file.to_owned(), id.to_owned()))
        .collect();
    let dirs = entry_dirs(&scan, &status);
    let deep = deep_imports(&scan, &dirs)
        .into_iter()
        .map(|(dir, edge)| (dirs[dir].0.to_owned(), edge))
        .collect();
    let entries = dirs
        .into_values()
        .map(|(id, entries)| {
            (
                id.to_owned(),
                entries.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
    let status = (status.into_iter())
        .map(|(id, found)| (id.to_owned(), found.map(|_| ())))
        .collect();
    Ownership {
        status,
        owners,
        entries,
        deep,
        limited: scan.limited.clone(),
        hops,
    }
}

/// The modules whose binding alone maps paths to them, without a scan, by qualified ID with
/// their normalized paths: those bound in this repository that are not external, less those
/// bound to an infrastructure unit and those [`symbol_bound`] leaves out.
pub(super) fn bound_paths(model: &str, modules: &[Module]) -> BTreeMap<String, String> {
    let bound: Vec<(String, String, bool)> = modules
        .iter()
        .filter(|m| m.kind != EXTERNAL_KIND)
        .filter_map(|m| Some((m, m.binding.as_ref()?)))
        .filter(|(_, b)| b.repo.is_none() && !infra::is_unit_binding(b))
        .map(|(m, b)| {
            let id = super::ids::qualify(model, &m.id);
            (id, segments(&b.path).join("/"), b.symbol.is_some())
        })
        .collect();
    let refs: Vec<(&str, &str, bool)> = (bound.iter())
        .map(|(id, path, symbol)| (id.as_str(), path.as_str(), *symbol))
        .collect();
    let left_out: HashSet<String> = symbol_bound(&refs).into_iter().map(str::to_owned).collect();
    (bound.into_iter())
        .filter(|(id, _, _)| !left_out.contains(id))
        .map(|(id, path, _)| (id, path))
        .collect()
}

/// The modules of `bound`, as (qualified ID, normalized path, bound with a symbol), that stand
/// for a symbol in a path another of them holds: they own no file of it.
fn symbol_bound<'i>(bound: &[(&'i str, &str, bool)]) -> HashSet<&'i str> {
    let mut holders: HashMap<&str, usize> = HashMap::new();
    for (_, path, _) in bound {
        *holders.entry(path).or_default() += 1;
    }
    (bound.iter())
        .filter(|(_, path, symbol)| *symbol && holders[path] > 1)
        .map(|(id, _, _)| *id)
        .collect()
}

/// Paths to the module whose binding holds them: the deepest binding, the first by ID among
/// bindings of one path. A binding of the whole repository has the empty path.
pub(super) struct PathOwners<'i, 'p> {
    by_path: HashMap<&'p str, &'i str>,
}

impl<'i, 'p> PathOwners<'i, 'p> {
    /// From qualified IDs with normalized binding paths, in ID order.
    pub(super) fn new(bound: impl IntoIterator<Item = (&'i str, &'p str)>) -> Self {
        let mut by_path = HashMap::new();
        for (id, path) in bound {
            by_path.entry(path).or_insert(id);
        }
        Self { by_path }
    }

    /// The owner of the repository path `file`: the binding of the file itself, else of its
    /// closest folder.
    pub(super) fn owner(&self, file: &str) -> Option<&'i str> {
        (iter::once(file)
            .chain(ancestors(file))
            .chain(iter::once("")))
        .find_map(|path| self.by_path.get(path).copied())
    }
}

/// Each edge joining files of two different owners, through its written target, with the
/// source's and the target's owner.
pub(super) fn owner_pairs<'i, 'e>(
    edges: impl IntoIterator<Item = &'e Edge>,
    owner: impl Fn(&str) -> Option<&'i str>,
) -> impl Iterator<Item = (&'i str, &'i str, &'e Edge)> {
    edges.into_iter().filter_map(move |edge| {
        let (from, to) = (owner(&edge.from)?, owner(written(edge))?);
        (from != to).then_some((from, to, edge))
    })
}

/// The written target of `edge`, before a facade resolves it.
pub(super) fn written(edge: &Edge) -> &str {
    edge.original_to.as_deref().unwrap_or(&edge.to)
}

/// `SCAN_FAILED`: a read the check needed failed, as `what` says. Its subjects are the
/// `scan-failed` skips `left` accepts; with none, every path it was needed for turned out
/// missing and already warns.
fn scan_failed(what: &str, error: &str, left: impl Fn(&str) -> bool, report: &mut CodeReport) {
    let subjects: BTreeSet<&str> = report
        .skips
        .iter()
        .filter(|s| s.reason == SkipReason::ScanFailed && left(&s.subject))
        .map(|s| s.subject.as_str())
        .collect();
    if subjects.is_empty() {
        return;
    }
    let subjects = subjects.into_iter().map(str::to_owned).collect();
    let message = format!("{what}: {}", parse_message(&error));
    report.warn(DiagnosticCode::ScanFailed, subjects, message);
}

/// The parts of a scan the check reads, empty without one.
struct Scan<'d> {
    /// Sorted.
    files: Vec<&'d str>,
    edges: &'d [Edge],
    language: Option<Language>,
    /// `None` without symbol data; otherwise the files exporting each name.
    exporters: Option<HashMap<&'d str, Vec<&'d str>>>,
    exports_of: HashMap<&'d str, Vec<&'d SymbolExport>>,
    /// Files a symbol limitation names.
    limited: HashSet<&'d str>,
    package_entries: &'d [String],
    /// Why an existing path without scanned files is skipped.
    absent: SkipReason,
}

impl<'d> Scan<'d> {
    /// The scan the check reads, and the error of a failed scan.
    fn of(scan: Result<&'d Document, NoScan>) -> (Self, Option<String>) {
        match scan {
            Ok(doc) => (Self::new(Ok(doc)), None),
            Err(NoScan::NotNeeded) => (Self::new(Err(SkipReason::NotScanned)), None),
            Err(NoScan::Failed(error)) => (Self::new(Err(SkipReason::ScanFailed)), Some(error)),
        }
    }

    fn new(scan: Result<&'d Document, SkipReason>) -> Self {
        let mut this = Self {
            files: Vec::new(),
            edges: &[],
            language: None,
            exporters: None,
            exports_of: HashMap::new(),
            limited: HashSet::new(),
            package_entries: &[],
            absent: SkipReason::NotScanned,
        };
        let doc = match scan {
            Ok(doc) => doc,
            Err(reason) => {
                this.absent = reason;
                return this;
            }
        };
        this.files = doc.files.iter().map(|f| f.path.as_str()).collect();
        this.files.sort_unstable();
        this.edges = &doc.edges;
        this.language = Some(doc.source.language);
        this.package_entries = &doc.package_entries;
        if let Some(symbols) = &doc.symbols {
            let mut exporters: HashMap<_, Vec<_>> = HashMap::new();
            for export in &symbols.exports {
                exporters
                    .entry(export.name.as_str())
                    .or_default()
                    .push(export.file.as_str());
                this.exports_of
                    .entry(export.file.as_str())
                    .or_default()
                    .push(export);
            }
            this.exporters = Some(exporters);
            // Limitations about one file start with its path: `src/a.go: ...`.
            this.limited = symbols
                .limitations
                .iter()
                .filter_map(|l| Some(l.split_once(':')?.0))
                .collect();
        }
        this
    }

    /// The scanned files at the normalized `path`: the file itself, or every file below it.
    fn files_under(&self, path: &str) -> &[&'d str] {
        if path.is_empty() {
            return &self.files;
        }
        if let Ok(at) = self.files.binary_search(&path) {
            return &self.files[at..=at];
        }
        // Every path from `<path>/` up to, not including, `<path>0` starts with `<path>/`.
        let (low, high) = (format!("{path}/"), format!("{path}0"));
        let start = self.files.partition_point(|f| *f < low.as_str());
        let end = self.files.partition_point(|f| *f < high.as_str());
        &self.files[start..end]
    }

    /// Whether one of the sorted `files` exports `name`, or `None` without symbol data.
    fn exports(&self, files: &[&str], name: &str) -> Option<bool> {
        let exporters = self.exporters.as_ref()?;
        Some(
            exporters
                .get(name)
                .is_some_and(|found| found.iter().any(|f| files.binary_search(f).is_ok())),
        )
    }

    fn is_entry(&self, file: &str) -> bool {
        let exports = self.exports_of.get(file).map_or(&[][..], Vec::as_slice);
        self.language
            .is_some_and(|language| is_entry_file(file, language, exports))
    }
}

/// Whether `edge` exists at run time: type-only edges do not.
fn runtime(edge: &Edge) -> bool {
    !matches!(edge.kind, EdgeKind::Type | EdgeKind::TypeReexport)
}

fn location(edge: &Edge) -> String {
    match edge.line {
        Some(line) => format!("{}:{line}", edge.from),
        None => edge.from.clone(),
    }
}

/// Whether every segment of the normalized `path` names an entry under `root` spelled exactly
/// so: a case-insensitive file system finds the path spelled with other cases too.
fn spelled_exactly(root: &Path, path: &str) -> bool {
    let mut dir = root.to_path_buf();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let listed = fs::read_dir(&dir)
            .is_ok_and(|mut entries| entries.any(|e| e.is_ok_and(|e| e.file_name() == segment)));
        if !listed {
            return false;
        }
        dir.push(segment);
    }
    true
}

/// The scanned files a binding or anchor names, with its normalized path.
struct Found<'s, 'd> {
    files: &'s [&'d str],
    dir: bool,
    path: String,
}

/// One binding or anchor to judge.
struct Site<'a> {
    site: SkipSite,
    subject: &'a str,
    /// The bound module, or the anchored relationship's source.
    from: &'a str,
    /// The anchored relationship's target.
    to: Option<&'a str>,
    repo: Option<&'a str>,
    path: &'a str,
    symbol: Option<&'a str>,
    path_code: DiagnosticCode,
    symbol_code: DiagnosticCode,
}

impl Site<'_> {
    /// The sentence start naming the site, as in `Billing is bound to`.
    fn label(&self) -> String {
        match self.to {
            None => format!("{} is bound to", self.from),
            Some(to) => format!("{} -> {to} is anchored at", self.from),
        }
    }

    /// The site's scanned files, or why it was not judged. Records its diagnostics and skips.
    fn judge<'s, 'd>(
        &self,
        root: &Path,
        scan: &'s Scan<'d>,
        lsp: &Facts,
        report: &mut CodeReport,
    ) -> Result<Found<'s, 'd>, SkipReason> {
        let found = self.locate(root, scan);
        let (subject, path) = (self.subject, self.path);
        match (&found, self.symbol) {
            (Err(reason), _) => {
                if *reason == SkipReason::PathMissing {
                    let message = format!("{} {path}, which does not exist", self.label());
                    report.warn(self.path_code, vec![subject.to_owned()], message);
                }
                report.skip(self.site, subject, *reason);
            }
            (Ok(found), Some(symbol)) => match symbol_missing(found.files, symbol, scan, lsp) {
                Ok(None) => {}
                Ok(Some(verb)) => {
                    let label = self.label();
                    let message =
                        format!("{label} {path} {symbol}, but no file there {verb} {symbol}");
                    report.warn(self.symbol_code, vec![subject.to_owned()], message);
                }
                Err(reason) => report.skip(self.site, subject, reason),
            },
            (Ok(_), None) => {}
        }
        found
    }

    fn locate<'s, 'd>(&self, root: &Path, scan: &'s Scan<'d>) -> Result<Found<'s, 'd>, SkipReason> {
        if self.repo.is_some() {
            return Err(SkipReason::OtherRepository);
        }
        let Ok(metadata) = fs::metadata(root.join(self.path)) else {
            return Err(SkipReason::PathMissing);
        };
        let path = segments(self.path).join("/");
        let files = scan.files_under(&path);
        if !files.is_empty() {
            Ok(Found {
                files,
                dir: metadata.is_dir(),
                path,
            })
        } else if spelled_exactly(root, &path) {
            Err(scan.absent)
        } else {
            Err(SkipReason::PathMissing)
        }
    }
}

/// Whether `symbol` is missing from `files`, as the verb of the sentence saying so, or why that
/// is not known. Where the scan cannot tell, the language servers decide: a declaration they
/// found confirms it, and only a non-empty symbol list of a single file refutes it.
fn symbol_missing(
    files: &[&str],
    symbol: &str,
    scan: &Scan<'_>,
    lsp: &Facts,
) -> Result<Option<&'static str>, SkipReason> {
    let limited = files.iter().any(|f| scan.limited.contains(f));
    let indexed = scan.exports(files, symbol);
    match (indexed, lsp.symbol(files, symbol)) {
        (Some(true), _) => Ok(None),
        (Some(false), _) if !limited => Ok(Some("exports")),
        (_, Some(declared)) => Ok((!declared).then_some("declares")),
        (None, None) => Err(SkipReason::LanguageUnsupported),
        (Some(false), None) => Err(SkipReason::SymbolsIncomplete),
    }
}

type Status<'i, 's, 'd> = BTreeMap<&'i str, Result<Found<'s, 'd>, SkipReason>>;

/// `BINDING_PATH_MISSING` and `BINDING_SYMBOL_MISSING`. Every module's scanned files, or why
/// its code is not judged.
fn bindings<'i, 's, 'd>(
    index: &'i Index<'_>,
    root: &Path,
    scan: &'s Scan<'d>,
    infra: &Infra,
    lsp: &Facts,
    report: &mut CodeReport,
) -> Status<'i, 's, 'd> {
    let mut status = BTreeMap::new();
    for (id, module) in &index.by_id {
        let id: &str = id;
        let found = match &module.binding {
            _ if module.kind == EXTERNAL_KIND => Err(SkipReason::External),
            None => Err(SkipReason::Unbound),
            Some(binding) if infra::is_unit_binding(binding) => {
                unit_binding(id, &module.name, binding, root, infra, report)
            }
            Some(binding) => Site {
                site: SkipSite::Binding,
                subject: id,
                from: &module.name,
                to: None,
                repo: binding.repo.as_deref(),
                path: &binding.path,
                symbol: binding.symbol.as_deref(),
                path_code: DiagnosticCode::BindingPathMissing,
                symbol_code: DiagnosticCode::BindingSymbolMissing,
            }
            .judge(root, scan, lsp, report),
        };
        if let Err(reason @ (SkipReason::External | SkipReason::Unbound)) = found {
            report.skip(SkipSite::Binding, id, reason);
        }
        status.insert(id, found);
    }
    symbol_bindings(index, &mut status);
    status
}

/// `BINDING_PATH_MISSING` and `BINDING_SYMBOL_MISSING` of a module bound to an infrastructure
/// unit: the file must exist and its folder declare the unit. A file that exists but was not
/// read is `not-scanned`, and every file is `scan-failed` when reading failed. The module owns
/// no scanned file.
fn unit_binding<'s, 'd>(
    subject: &str,
    name: &str,
    binding: &Binding,
    root: &Path,
    infra: &Infra,
    report: &mut CodeReport,
) -> Result<Found<'s, 'd>, SkipReason> {
    let path = &binding.path;
    if !root.join(path).is_file() {
        let message = format!("{name} is bound to {path}, which does not exist");
        report.warn(
            DiagnosticCode::BindingPathMissing,
            vec![subject.to_owned()],
            message,
        );
        report.skip(SkipSite::Binding, subject, SkipReason::PathMissing);
        return Err(SkipReason::PathMissing);
    }
    let normalized = segments(path).join("/");
    let unread = if infra.error.is_some() {
        Some(SkipReason::ScanFailed)
    } else {
        (!infra.files.contains(&normalized)).then_some(SkipReason::NotScanned)
    };
    if let Some(reason) = unread {
        report.skip(SkipSite::Binding, subject, reason);
        return Err(reason);
    }
    if infra.bound(binding).is_none() {
        let symbol = binding.symbol.as_deref().unwrap_or_default();
        let message =
            format!("{name} is bound to {path} {symbol}, but no unit there is named {symbol}");
        report.warn(
            DiagnosticCode::BindingSymbolMissing,
            vec![subject.to_owned()],
            message,
        );
    }
    Ok(Found {
        files: &[],
        dir: false,
        path: normalized,
    })
}

/// Marks `symbol-binding` the modules bound with a symbol to a path another bound module holds:
/// they stand for part of that code, so they own no file of it. Units are not code: modules
/// bound to units of one file stay judged.
fn symbol_bindings(index: &Index<'_>, status: &mut Status<'_, '_, '_>) {
    let bound: Vec<(&str, &str, bool)> = (status.iter())
        .filter_map(|(&id, found)| {
            let binding = index.by_id.get(id).and_then(|m| m.binding.as_ref());
            if binding.is_some_and(infra::is_unit_binding) {
                return None;
            }
            let symbol = binding.is_some_and(|b| b.symbol.is_some());
            Some((id, found.as_ref().ok()?.path.as_str(), symbol))
        })
        .collect();
    let tied: Vec<String> = symbol_bound(&bound)
        .into_iter()
        .map(str::to_owned)
        .collect();
    for id in tied {
        if let Some(found) = status.get_mut(id.as_str()) {
            *found = Err(SkipReason::SymbolBinding);
        }
    }
}

/// `ANCHOR_PATH_MISSING` and `ANCHOR_SYMBOL_MISSING`.
fn anchors(index: &Index<'_>, root: &Path, scan: &Scan<'_>, lsp: &Facts, report: &mut CodeReport) {
    for line in &index.lines {
        let Some(anchor) = line.anchor else {
            continue;
        };
        let site = Site {
            site: SkipSite::Anchor,
            subject: &line.id,
            from: index.name(&line.from),
            to: Some(index.name(line.to)),
            repo: anchor.repo.as_deref(),
            path: &anchor.path,
            symbol: Some(&anchor.symbol),
            path_code: DiagnosticCode::AnchorPathMissing,
            symbol_code: DiagnosticCode::AnchorSymbolMissing,
        };
        // The skips and diagnostics are recorded; an anchor's files serve nothing else.
        let _ = site.judge(root, scan, lsp, report);
    }
}

/// The judged modules as owners of the paths their bindings hold.
fn path_owners<'i, 'p>(status: &'p Status<'i, '_, '_>) -> PathOwners<'i, 'p> {
    PathOwners::new(
        (status.iter()).filter_map(|(&id, found)| Some((id, found.as_ref().ok()?.path.as_str()))),
    )
}

/// Each scanned file's owner: the deepest judged module whose binding encloses it. Modules
/// bound to one path without a symbol share its files with the first by ID; the semantic check
/// reports them as `BINDING_SHARED`.
fn owners<'i, 'd>(
    status: &Status<'i, '_, 'd>,
    paths: &PathOwners<'i, '_>,
) -> HashMap<&'d str, &'i str> {
    (status.values().flatten())
        .flat_map(|found| found.files.iter().copied())
        .filter_map(|file| Some((file, paths.owner(file)?)))
        .collect()
}

/// The infrastructure as judged modules: units belong to the modules bound to them, else to
/// the owner of the directory a compose service builds from; folders to their owners.
fn joined<'i, 'x>(
    index: &Index<'_>,
    status: &Status<'i, '_, '_>,
    infra: &'x Infra,
    paths: &PathOwners<'i, '_>,
) -> Joined<'i, 'x> {
    let bound = (status.iter())
        .filter(|(_, found)| found.is_ok())
        .filter_map(|(&id, _)| Some((id, index.by_id.get(id)?.binding.as_ref()?)));
    infra.join(bound, |dir| paths.owner(dir))
}

/// Every unit with its owner, or `infra-unbound`.
fn unit_statuses(infra: &Infra, owners: &[Option<&str>]) -> Vec<UnitStatus> {
    (infra.units.iter().zip(owners))
        .map(|(unit, owner)| UnitStatus {
            unit: unit.clone(),
            owner: owner.map(str::to_owned),
            reason: owner.is_none().then_some(SkipReason::InfraUnbound),
        })
        .collect()
}

/// The judged modules language servers judge calls of: those whose every interface symbol a
/// server was asked about, with every judged part below them.
fn served<'i>(index: &'i Index<'_>, status: &Status<'i, '_, '_>, lsp: &Facts) -> HashSet<&'i str> {
    let judged = status
        .iter()
        .filter(|(_, found)| found.is_ok())
        .map(|(&id, _)| id);
    let mut served: HashSet<&str> = judged
        .clone()
        .filter(|id| lsp.queried.contains(*id))
        .collect();
    for id in judged.filter(|id| !lsp.queried.contains(*id)) {
        for above in index.chain(id) {
            served.remove(above);
        }
    }
    served
}

/// The `served` modules language servers judge calls into: those with a judged module in their
/// subtree, themselves included, that has an interface symbol a server located, while every
/// judged module there owning a file has one. Into the others imports stand in, so a part with
/// no located symbol never leaves its container's calls unsupported.
fn targets<'i>(
    index: &'i Index<'_>,
    status: &Status<'i, '_, '_>,
    owners: &HashMap<&str, &'i str>,
    served: &HashSet<&'i str>,
    lsp: &Facts,
) -> HashSet<&'i str> {
    let owning: HashSet<&str> = owners.values().copied().collect();
    let judged = |id: &&str| status.get(*id).is_some_and(Result::is_ok);
    (served.iter().copied())
        .filter(|&id| {
            let parts = index.descendants(id).into_iter().map(|(part, _)| part);
            let (located, unlocated): (Vec<&str>, Vec<&str>) = (iter::once(id).chain(parts))
                .filter(judged)
                .partition(|m| lsp.located.contains(*m));
            !located.is_empty() && unlocated.iter().all(|m| !owning.contains(m))
        })
        .collect()
}

/// The file through which the use site of `r` reaches its definition: the written target of an
/// import edge of `imports` from the use site to the definition file or to a file exporting the
/// symbol, else the definition file itself.
fn through<'a>(
    r: &'a Reference,
    imports: &HashMap<&str, Vec<&'a Edge>>,
    scan: &Scan<'a>,
) -> &'a str {
    let exports = |file: &str| {
        (scan.exports_of.get(file).into_iter().flatten())
            .any(|e| e.name == r.name || (e.target.file == r.file && e.target.name == r.name))
    };
    (imports.get(r.from.as_str()).into_iter().flatten())
        .find(|e| e.to == r.file || written(e) == r.file || exports(written(e)))
        .map_or(r.file.as_str(), |e| written(e))
}

/// The modules code edges, language-server references and infrastructure hops join, through
/// the written target of each edge.
struct Pairs<'i, 'd, 'x> {
    /// Ordered pairs of different owners joined by a runtime edge, with the first one.
    runtime: BTreeMap<(&'i str, &'i str), &'d Edge>,
    /// Ordered pairs of different owners joined by a reference, with the first one.
    calls: BTreeMap<(&'i str, &'i str), &'x Reference>,
    /// Ordered pairs of different owners joined by a configured hop, with the first one.
    configured: BTreeMap<(&'i str, &'i str), &'x Hop>,
    /// Pairs of those owners or their ancestors with the evidence joining them: `import` for
    /// any edge, `call` for a runtime edge, and each hop's own evidence.
    reach: HashSet<(Evidence, &'i str, &'i str)>,
    /// Pairs of those owners or their ancestors that a reference joins.
    called: HashSet<(&'i str, &'i str)>,
    /// The modules whose calls out are judged by the references rather than runtime edges.
    served: HashSet<&'i str>,
    /// The served modules whose calls in are judged by the references.
    targets: HashSet<&'i str>,
}

impl<'i, 'd, 'x> Pairs<'i, 'd, 'x> {
    fn new(
        index: &'i Index<'_>,
        scan: &Scan<'d>,
        owners: &HashMap<&'d str, &'i str>,
        hops: &[(&'i str, &'i str, &'x Hop)],
        references: &'x [Reference],
        (served, targets): (HashSet<&'i str>, HashSet<&'i str>),
    ) -> Self {
        let mut joined = HashSet::new();
        let mut pairs = Self {
            runtime: BTreeMap::new(),
            calls: BTreeMap::new(),
            configured: BTreeMap::new(),
            reach: HashSet::new(),
            called: HashSet::new(),
            served,
            targets,
        };
        let owner = |file: &str| owners.get(file).copied();
        let mut imports: HashMap<&str, Vec<&Edge>> = HashMap::new();
        for edge in scan.edges {
            imports.entry(edge.from.as_str()).or_default().push(edge);
        }
        // A reference belongs to the module its use site imports the symbol through, as an
        // import edge does.
        for r in references {
            if let (Some(from), Some(to)) = (owner(&r.from), owner(through(r, &imports, scan)))
                && from != to
            {
                pairs.calls.entry((from, to)).or_insert(r);
            }
        }
        let lift = |from: &'i str, to: &'i str| {
            let targets = index.chain(to);
            (index.chain(from).into_iter())
                .flat_map(move |above| targets.clone().into_iter().map(move |below| (above, below)))
        };
        (pairs.called).extend(pairs.calls.keys().flat_map(|&(from, to)| lift(from, to)));
        for (from, to, edge) in owner_pairs(scan.edges, owner) {
            joined.insert((Evidence::Import, from, to));
            if runtime(edge) {
                joined.insert((Evidence::Call, from, to));
                pairs.runtime.entry((from, to)).or_insert(edge);
            }
        }
        for &(from, to, hop) in hops {
            joined.insert((hop.evidence, from, to));
            if hop.evidence == Evidence::ConfiguredPath {
                pairs.configured.entry((from, to)).or_insert(hop);
            }
        }
        for (evidence, from, to) in joined {
            (pairs.reach).extend(lift(from, to).map(|(above, below)| (evidence, above, below)));
        }
        pairs
    }

    /// Whether evidence of a kind in `accepted` joins `from` or a part of it to `to` or a part
    /// of it. Call evidence between two modules language servers serve is their references.
    fn holds(&self, accepted: &[Evidence], from: &str, to: &str) -> bool {
        let served = self.served(from, to);
        (accepted.iter()).any(|&evidence| match evidence {
            Evidence::Call if served => self.called.contains(&(from, to)),
            _ => self.reach.contains(&(evidence, from, to)),
        })
    }

    /// Whether language servers judge the calls from `from` to `to`.
    fn served(&self, from: &str, to: &str) -> bool {
        self.served.contains(from) && self.targets.contains(to)
    }

    /// Each pair's evidence as it reads, the first kind found: a runtime edge, else a
    /// reference, else a configured hop.
    fn evidence(&self) -> BTreeMap<(&'i str, &'i str), String> {
        let mut found = BTreeMap::new();
        for (&pair, edge) in &self.runtime {
            found.insert(
                pair,
                format!("{} imports {}", location(edge), written(edge)),
            );
        }
        for (&pair, r) in &self.calls {
            let calls = || format!("{}:{} calls {}", r.from, r.line, r.name);
            found.entry(pair).or_insert_with(calls);
        }
        for (&pair, hop) in &self.configured {
            let (file, line, variable) = (&hop.file, hop.line, &hop.variable);
            let names = || format!("{file}:{line} {variable} names {}", hop.address);
            found.entry(pair).or_insert_with(names);
        }
        found
    }

    /// How call evidence from `from` to `to` reads: references or imports.
    fn call_verb(&self, from: &str, to: &str) -> &'static str {
        if self.served(from, to) {
            "calls"
        } else {
            "imports"
        }
    }
}

/// `RELATIONSHIP_UNSUPPORTED`, or `RELATIONSHIP_BACKWARDS` when the evidence only runs the
/// other way, for relationships whose kind's evidence is all collected. Returns the backwards
/// lines.
fn relationships<'i>(
    index: &'i Index<'_>,
    config: &VaultConfig,
    judged: Judged<'_, '_>,
    report: &mut CodeReport,
) -> HashSet<(&'i str, &'i str)> {
    let (_, pairs, collected) = judged;
    let mut backwards = HashSet::new();
    for line in &index.lines {
        let evidence = collected.applicable(accepted(config, line.kind), line);
        let code = match verdict(line, &evidence, judged) {
            Ok(Some(code)) => code,
            Ok(None) => continue,
            Err(reason) => {
                report.skip(SkipSite::Relationship, &line.id, reason);
                continue;
            }
        };
        if code == DiagnosticCode::RelationshipBackwards {
            backwards.insert((line.from.as_str(), line.to));
        }
        let message = unsupported(index, line, &evidence, code, pairs);
        report.warn(code, vec![line.id.clone()], message);
    }
    backwards
}

/// What relationships are judged against: each module's status, the pairs evidence joins and
/// where evidence is collected.
type Judged<'s, 'i> = (
    &'s Status<'i, 's, 's>,
    &'s Pairs<'i, 's, 's>,
    &'s Collected<'i>,
);

/// The evidence kinds that support relationships of `kind`.
fn accepted<'c>(config: &'c VaultConfig, kind: &str) -> &'c [Evidence] {
    (config.relationship_kinds.get(kind)).map_or(&[], |k| k.evidence.as_slice())
}

/// What a relationship the evidence does not support lacks, or the evidence that runs against
/// it.
fn unsupported(
    index: &Index<'_>,
    line: &Line<'_>,
    accepted: &[Evidence],
    code: DiagnosticCode,
    pairs: &Pairs<'_, '_, '_>,
) -> String {
    let (from, to, kind) = (index.name(&line.from), index.name(line.to), line.kind);
    if code == DiagnosticCode::RelationshipBackwards {
        let against = (accepted.iter()).find(|&&e| pairs.holds(&[e], line.to, &line.from));
        let verb = match against {
            Some(Evidence::ConfiguredPath) => "names the address of",
            Some(Evidence::BuildDependency) => "depends on",
            Some(Evidence::Call) => pairs.call_verb(line.to, &line.from),
            _ => "imports",
        };
        return format!("{from} -> {to} [{kind}] runs backwards: only {to} {verb} {from}");
    }
    let mut lacks = Vec::new();
    if accepted.contains(&Evidence::Import) {
        lacks.push(format!("never imports {to}"));
    } else if accepted.contains(&Evidence::Call) {
        lacks.push(format!(
            "never {} {to}",
            pairs.call_verb(&line.from, line.to)
        ));
    }
    if accepted.contains(&Evidence::ConfiguredPath) {
        lacks.push(format!("names no address of {to}"));
    }
    if accepted.contains(&Evidence::BuildDependency) {
        lacks.push(format!("declares no build dependency on {to}"));
    }
    let code_only = (accepted.iter()).all(|e| matches!(e, Evidence::Call | Evidence::Import));
    let what = if code_only {
        "code evidence"
    } else {
        "evidence"
    };
    format!(
        "{from} -> {to} [{kind}] has no {what}: {from} {}",
        lacks.join(" and ")
    )
}

/// The diagnostic a relationship earns, if any, or why it is not judged, by the `evidence` that
/// applies to it. Type-only edges count only for kinds that accept import evidence. A
/// relationship is judged strictly only where all of it is collected.
fn verdict(
    line: &Line<'_>,
    evidence: &[Evidence],
    (status, pairs, collected): Judged<'_, '_>,
) -> Result<Option<DiagnosticCode>, SkipReason> {
    for end in [line.from.as_str(), line.to] {
        if let Some(Err(reason)) = status.get(end) {
            return Err(*reason);
        }
    }
    if evidence.is_empty() {
        return Err(SkipReason::EvidenceNotCode);
    }
    let (from, to) = (line.from.as_str(), line.to);
    if pairs.holds(evidence, from, to) {
        return Ok(None);
    }
    if !evidence.iter().all(|&e| collected.holds(e, from, to)) {
        return Err(SkipReason::EvidenceUnavailable);
    }
    Ok(Some(if pairs.holds(evidence, to, from) {
        DiagnosticCode::RelationshipBackwards
    } else {
        DiagnosticCode::RelationshipUnsupported
    }))
}

/// `RELATIONSHIP_UNDRAWN`: runtime edges, or else references, or else configured hops, between
/// two modules, neither containing the other, that no relationship from the source or an
/// ancestor to the target or an ancestor covers. A backwards relationship already reports the
/// evidence against it.
fn undrawn(
    index: &Index<'_>,
    pairs: &Pairs<'_, '_, '_>,
    backwards: &HashSet<(&str, &str)>,
    report: &mut CodeReport,
) {
    let drawn: HashSet<(&str, &str)> = index
        .lines
        .iter()
        .map(|l| (l.from.as_str(), l.to))
        .collect();
    let covers = |set: &HashSet<(&str, &str)>, from: &[&str], to: &[&str]| {
        from.iter()
            .any(|a| to.iter().any(|b| set.contains(&(*a, *b))))
    };
    for ((from, to), evidence) in pairs.evidence() {
        if index.is_below(from, to) || index.is_below(to, from) {
            continue;
        }
        let (above_from, above_to) = (index.chain(from), index.chain(to));
        if covers(&drawn, &above_from, &above_to) || covers(backwards, &above_to, &above_from) {
            continue;
        }
        let message = format!(
            "{} uses {} but no relationship is drawn: {evidence}",
            index.name(from),
            index.name(to),
        );
        report.warn(
            DiagnosticCode::RelationshipUndrawn,
            vec![from.to_owned(), to.to_owned()],
            message,
        );
    }
}

/// The directories above `path`, innermost first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    iter::successors(path.rsplit_once('/'), |(dir, _)| dir.rsplit_once('/')).map(|(dir, _)| dir)
}

/// Whether `path` lies below the non-empty directory `dir`.
fn inside(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// The modules bound to a directory with entry points, by directory: entry files directly in it
/// and the files a package publishes below it.
fn entry_dirs<'i, 's>(
    scan: &Scan<'s>,
    status: &'s Status<'i, '_, 's>,
) -> HashMap<&'s str, (&'i str, BTreeSet<&'s str>)> {
    let mut dirs = HashMap::new();
    for (&id, found) in status {
        if let Ok(found) = found
            && found.dir
            && !found.path.is_empty()
        {
            let entries: BTreeSet<&str> = found
                .files
                .iter()
                .copied()
                .filter(|f| dir_of(f) == found.path && scan.is_entry(f))
                .collect();
            dirs.insert(found.path.as_str(), (id, entries));
        }
    }
    for entry in scan.package_entries {
        for dir in ancestors(entry) {
            if let Some((_, entries)) = dirs.get_mut(dir) {
                entries.insert(entry.as_str());
            }
        }
    }
    dirs.retain(|_, (_, entries)| !entries.is_empty());
    dirs
}

/// Each runtime edge from outside a directory in `dirs` to a file inside it that is not one of
/// its entry points, with that directory.
fn deep_imports<'d>(
    scan: &Scan<'d>,
    dirs: &HashMap<&str, (&str, BTreeSet<&str>)>,
) -> Vec<(&'d str, &'d Edge)> {
    let mut deep = Vec::new();
    for edge in scan.edges.iter().filter(|e| runtime(e)) {
        let target = written(edge);
        for dir in ancestors(target) {
            if let Some((_, entries)) = dirs.get(dir)
                && !inside(&edge.from, dir)
                && !entries.contains(target)
            {
                deep.push((dir, edge));
            }
        }
    }
    deep
}

/// `ENTRY_POINT_UNEXPOSED`: code outside a module bound to a directory with entry points
/// imports a file inside it that is not one of them. One diagnostic per module and file.
fn entry_points(
    index: &Index<'_>,
    scan: &Scan<'_>,
    status: &Status<'_, '_, '_>,
    report: &mut CodeReport,
) {
    let dirs = entry_dirs(scan, status);
    let mut reported = BTreeSet::new();
    for (dir, edge) in deep_imports(scan, &dirs) {
        let (id, entries) = &dirs[dir];
        let target = written(edge);
        if !reported.insert((*id, target)) {
            continue;
        }
        let message = format!(
            "{} imports {target} inside {}, past its entry points: {}",
            location(edge),
            index.name(id),
            entries.iter().copied().collect::<Vec<_>>().join(", ")
        );
        report.warn(
            DiagnosticCode::EntryPointUnexposed,
            vec![(*id).to_owned()],
            message,
        );
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::DiagnosticCode::*;
    use super::*;
    use crate::arch::diagnostic::Severity;
    use crate::arch::ids;
    use crate::arch::types::{
        Anchor, Binding, Dash, Relationship, RelationshipKind, SCHEMA_VERSION,
    };
    use crate::collect::TempDir;
    use crate::model::{Config, Source};
    use crate::surface::{SymbolTarget, Symbols};
    use serde_json::json;

    pub(in crate::arch) const M: &str = "Model001";

    pub(in crate::arch) fn q(local: &str) -> String {
        ids::qualify(M, local)
    }

    pub(in crate::arch) fn module(id: &str, parent: Option<&str>, path: Option<&str>) -> Module {
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

    fn symbol(mut module: Module, symbol: &str) -> Module {
        module.binding.as_mut().unwrap().symbol = Some(symbol.to_owned());
        module
    }

    pub(in crate::arch) fn link<'m>(
        from: &'m mut Module,
        id: &str,
        to: &str,
        kind: &str,
    ) -> &'m mut Relationship {
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
        from.outgoing.last_mut().unwrap()
    }

    /// App holds Web and Billing; Bank stands beside it. Web's page reaches past Billing's index
    /// into its charge file, Billing calls Bank, and Web uses a Bank type only. The shop's cart
    /// imports Billing's charge through the shared barrel. The README is not scanned.
    const FILES: [&str; 8] = [
        "app/web/index.ts",
        "app/web/page.ts",
        "app/billing/index.ts",
        "app/billing/charge.ts",
        "app/shop/cart.ts",
        "bank/index.ts",
        "shared/index.ts",
        "docs/README.md",
    ];

    pub(in crate::arch) fn edge(from: &str, to: &str, kind: EdgeKind, line: usize) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            kind,
            line: Some(line),
            ..Edge::default()
        }
    }

    /// The checkout on disk, and its scan of every file but the README.
    pub(in crate::arch) fn checkout() -> (TempDir, Document) {
        let dir = TempDir::new("code-check").unwrap();
        for file in FILES {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let config: Config = serde_json::from_value(json!({"depth": 1, "source_root": ".", "excludes": [], "include_tests": false, "rules": null})).unwrap();
        let source: Source = serde_json::from_value(json!({"language": "typescript", "collector": {"name": "fixture", "cruised": 0}, "generated_at": "test"})).unwrap();
        let edges = vec![
            edge("app/web/index.ts", "app/web/page.ts", EdgeKind::Static, 1),
            edge(
                "app/web/index.ts",
                "app/billing/index.ts",
                EdgeKind::Static,
                2,
            ),
            edge(
                "app/web/page.ts",
                "app/billing/charge.ts",
                EdgeKind::Static,
                3,
            ),
            edge("app/web/page.ts", "bank/index.ts", EdgeKind::Type, 4),
            edge(
                "app/billing/index.ts",
                "app/billing/charge.ts",
                EdgeKind::Reexport,
                1,
            ),
            edge(
                "app/billing/charge.ts",
                "bank/index.ts",
                EdgeKind::Static,
                1,
            ),
            edge(
                "shared/index.ts",
                "app/billing/charge.ts",
                EdgeKind::Reexport,
                1,
            ),
        ];
        let files = FILES[..7].iter().map(|f| (*f).to_owned()).collect();
        let mut doc = crate::analyze(source, config, files, edges, Vec::new(), None).unwrap();
        let mut cart = edge(
            "app/shop/cart.ts",
            "app/billing/charge.ts",
            EdgeKind::Static,
            2,
        );
        cart.original_to = Some("shared/index.ts".into());
        doc.edges.push(cart);
        let export = |file: &str, name: &str| SymbolExport {
            file: file.into(),
            name: name.into(),
            target: SymbolTarget {
                file: file.into(),
                name: name.into(),
            },
        };
        doc.symbols = Some(Symbols {
            exports: vec![
                export("app/billing/charge.ts", "charge"),
                export("bank/index.ts", "pay"),
            ],
            ..Symbols::default()
        });
        (dir, doc)
    }

    pub(in crate::arch) fn shop() -> [Module; 4] {
        [
            module("App", None, Some("app")),
            module("Web", Some("App"), Some("app/web")),
            module("Billing", Some("App"), Some("app/billing/")),
            module("Bank", None, Some("bank")),
        ]
    }

    fn run(modules: &[Module], doc: &Document, root: &Path) -> CodeReport {
        check(
            M,
            modules,
            &VaultConfig::default(),
            root,
            Ok(doc),
            &Infra::default(),
            &Facts::default(),
        )
    }

    fn of(report: &CodeReport, code: DiagnosticCode) -> Vec<&Diagnostic> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == code)
            .collect()
    }

    fn subjects(report: &CodeReport, code: DiagnosticCode) -> Vec<Vec<String>> {
        of(report, code)
            .into_iter()
            .map(|d| d.subjects.clone())
            .collect()
    }

    fn skips(report: &CodeReport) -> Vec<(String, &'static str, &'static str)> {
        report
            .skips
            .iter()
            .map(|s| (s.subject.clone(), s.site.as_str(), s.reason.as_str()))
            .collect()
    }

    fn found_codes(report: &CodeReport) -> Vec<DiagnosticCode> {
        report.diagnostics.iter().map(|d| d.code).collect()
    }

    const NONE: [Vec<String>; 0] = [];

    #[test]
    fn code_edges_between_modules_need_a_relationship_from_them_or_an_ancestor() {
        let (dir, doc) = checkout();
        let report = run(&shop(), &doc, dir.path());
        // Web -> Bank is type-only and App -> Web is containment: neither needs a line.
        assert_eq!(
            subjects(&report, RelationshipUndrawn),
            [vec![q("Billing"), q("Bank")], vec![q("Web"), q("Billing")]]
        );
        let web = of(&report, RelationshipUndrawn)[1];
        assert_eq!(
            web.message,
            "Web uses Billing but no relationship is drawn: app/web/index.ts:2 imports app/billing/index.ts"
        );
        assert_eq!(web.severity, Severity::Warning);

        let [mut app, mut web, billing, bank] = shop();
        link(&mut web, "R1", "Billing", "call");
        link(&mut app, "R2", "Bank", "dependency");
        let report = run(&[app, web, billing, bank], &doc, dir.path());
        assert_eq!(subjects(&report, RelationshipUndrawn), NONE);
    }

    #[test]
    fn a_backwards_relationship_alone_reports_the_edges_against_it() {
        let (dir, doc) = checkout();
        let [app, web, billing, mut bank] = shop();
        link(&mut bank, "Backward", "Billing", "call");
        let report = run(&[app, web, billing, bank], &doc, dir.path());
        assert_eq!(
            subjects(&report, RelationshipBackwards),
            [vec![q("Backward")]]
        );
        assert_eq!(
            of(&report, RelationshipBackwards)[0].message,
            "Bank -> Billing [call] runs backwards: only Billing imports Bank"
        );
        assert_eq!(
            subjects(&report, RelationshipUndrawn),
            [vec![q("Web"), q("Billing")]]
        );
    }

    #[test]
    fn relationships_need_edges_when_their_kinds_evidence_is_all_collected() {
        let (dir, doc) = checkout();
        let mut config = VaultConfig::default();
        config.relationship_kinds.insert(
            "types".to_owned(),
            RelationshipKind {
                name: "Types".to_owned(),
                dash: Dash::Dotted,
                evidence: vec![Evidence::Import],
            },
        );
        let [mut app, mut web, billing, mut bank] = shop();
        link(&mut web, "Supportd", "Billing", "call");
        link(&mut app, "ViaPart", "Bank", "dependency");
        link(&mut web, "TypeOnly", "Bank", "render");
        link(&mut web, "TypeKind", "Bank", "types");
        link(&mut bank, "Unproven", "Web", "dependency");
        let modules = [app, web, billing, bank];
        let report = check(
            M,
            &modules,
            &config,
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            &Facts::default(),
        );
        assert_eq!(
            subjects(&report, RelationshipUnsupported),
            [vec![q("TypeOnly")]]
        );
        assert_eq!(
            of(&report, RelationshipUnsupported)[0].message,
            "Web -> Bank [render] has no code evidence: Web never imports Bank"
        );
        assert_eq!(subjects(&report, RelationshipBackwards), NONE);
        assert_eq!(
            skips(&report)
                .into_iter()
                .filter(|(_, site, _)| *site == "relationship")
                .collect::<Vec<_>>(),
            [(q("Unproven"), "relationship", "evidence-unavailable")]
        );
    }

    #[test]
    fn a_container_with_a_file_owning_part_no_server_located_keeps_import_evidence() {
        let (dir, mut doc) = checkout();
        // Bank imports only Web, a part of App whose file has no symbol a server located.
        doc.edges.push(edge(
            "bank/index.ts",
            "app/web/index.ts",
            EdgeKind::Static,
            1,
        ));
        let [app, web, billing, mut bank] = shop();
        link(&mut bank, "ToApp", "App", "call");
        let modules = [app, web, billing, bank];
        let all = ["App", "Web", "Billing", "Bank"].map(q);
        let facts = Facts {
            queried: all.iter().cloned().collect(),
            located: [q("App"), q("Billing")].into_iter().collect(),
            ..Facts::default()
        };
        let report = check(
            M,
            &modules,
            &VaultConfig::default(),
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            &facts,
        );
        assert_eq!(subjects(&report, RelationshipUnsupported), NONE);
        assert_eq!(subjects(&report, RelationshipBackwards), NONE);
        // Once Web has a located symbol too, the references judge calls into App.
        let facts = Facts {
            located: all.iter().cloned().collect(),
            ..facts
        };
        let report = check(
            M,
            &modules,
            &VaultConfig::default(),
            dir.path(),
            Ok(&doc),
            &Infra::default(),
            &facts,
        );
        assert_eq!(
            subjects(&report, RelationshipUnsupported),
            [vec![q("ToApp")]]
        );
    }

    #[test]
    fn edges_through_a_barrel_belong_to_the_module_written() {
        let (dir, doc) = checkout();
        let mut cart = module("Shop", None, Some("app/shop"));
        link(&mut cart, "Uses", "Shared", "call");
        let mut shared = module("Shared", None, Some("shared"));
        link(&mut shared, "Exports", "Billing", "call");
        let modules = [cart, shared, module("Billing", None, Some("app/billing"))];
        let report = run(&modules, &doc, dir.path());
        assert_eq!(subjects(&report, RelationshipUndrawn), NONE);
        assert_eq!(subjects(&report, RelationshipUnsupported), NONE);
    }

    #[test]
    fn relationships_that_cannot_be_judged_are_skipped_with_their_reason() {
        let (dir, doc) = checkout();
        let [app, mut web, billing, bank] = shop();
        let mut vendor = module("Vendor", None, None);
        vendor.kind = EXTERNAL_KIND.to_owned();
        link(&mut web, "Loose", "Billing", "other");
        link(&mut web, "ToVendor", "Vendor", "call");
        link(&mut web, "ToIdea", "Idea", "call");
        link(&mut web, "ToGhost", "Ghost", "call");
        link(&mut web, "ToDocs", "Docs", "call");
        link(&mut web, "ToCharge", "Charge", "call");
        let modules = [
            app,
            web,
            billing,
            bank,
            vendor,
            module("Idea", None, None),
            module("Ghost", None, Some("ghost")),
            module("Docs", None, Some("docs")),
            symbol(module("Charge", None, Some("app/billing")), "charge"),
        ];
        let report = run(&modules, &doc, dir.path());
        let skipped = skips(&report);
        for expected in [
            (q("Loose"), "relationship", "evidence-not-code"),
            (q("ToVendor"), "relationship", "external"),
            (q("ToIdea"), "relationship", "unbound"),
            (q("ToGhost"), "relationship", "path-missing"),
            (q("ToDocs"), "relationship", "not-scanned"),
            (q("ToCharge"), "relationship", "symbol-binding"),
            (q("Vendor"), "binding", "external"),
            (q("Idea"), "binding", "unbound"),
            (q("Docs"), "binding", "not-scanned"),
        ] {
            assert!(skipped.contains(&expected), "{expected:?} in {skipped:?}");
        }
        assert_eq!(subjects(&report, RelationshipUnsupported), NONE);
        // Billing keeps its files beside the symbol bound to the same directory.
        assert!(subjects(&report, RelationshipUndrawn).contains(&vec![q("Billing"), q("Bank")]));
    }

    #[test]
    fn bindings_must_name_existing_paths_spelled_exactly_and_exported_symbols() {
        let (dir, doc) = checkout();
        let mut remote = module("Remote", None, Some("nowhere"));
        remote.binding.as_mut().unwrap().repo = Some("other".to_owned());
        let modules = [
            symbol(
                module("Charge", None, Some("app/billing/charge.ts")),
                "charge",
            ),
            symbol(module("Wrong", None, Some("app/billing")), "refund"),
            remote,
            module("Ghost", None, Some("ghost/x.ts")),
            module("Shouted", None, Some("DOCS")),
        ];
        let report = run(&modules, &doc, dir.path());
        assert_eq!(
            subjects(&report, BindingPathMissing),
            [vec![q("Ghost")], vec![q("Shouted")]]
        );
        assert_eq!(
            of(&report, BindingPathMissing)[0].message,
            "Ghost is bound to ghost/x.ts, which does not exist"
        );
        assert_eq!(subjects(&report, BindingSymbolMissing), [vec![q("Wrong")]]);
        assert_eq!(
            of(&report, BindingSymbolMissing)[0].message,
            "Wrong is bound to app/billing refund, but no file there exports refund"
        );
        assert_eq!(
            skips(&report),
            [
                (q("Ghost"), "binding", "path-missing"),
                (q("Remote"), "binding", "other-repository"),
                (q("Shouted"), "binding", "path-missing"),
            ]
        );
    }

    #[test]
    fn symbols_are_skipped_without_complete_symbol_data_or_a_scan() {
        let (dir, mut doc) = checkout();
        let modules = [symbol(
            module("Billing", None, Some("app/billing")),
            "refund",
        )];
        doc.symbols.as_mut().unwrap().limitations =
            vec!["app/billing/charge.ts: incomplete syntax".to_owned()];
        let report = run(&modules, &doc, dir.path());
        assert_eq!(subjects(&report, BindingSymbolMissing), NONE);
        assert_eq!(
            skips(&report),
            [(q("Billing"), "binding", "symbols-incomplete")]
        );

        doc.symbols = None;
        let report = run(&modules, &doc, dir.path());
        assert_eq!(subjects(&report, BindingSymbolMissing), NONE);
        assert_eq!(
            skips(&report),
            [(q("Billing"), "binding", "language-unsupported")]
        );

        let none = Infra::default();
        let without = |scan| {
            check(
                M,
                &modules,
                &VaultConfig::default(),
                dir.path(),
                Err(scan),
                &none,
                &Facts::default(),
            )
        };
        let report = without(NoScan::NotNeeded);
        assert_eq!(report.diagnostics, []);
        assert_eq!(skips(&report), [(q("Billing"), "binding", "not-scanned")]);

        // A failed scan fails the check, naming what it left unjudged.
        let report = without(NoScan::Failed(
            "no source\nsecond line \u{1b}[31m".to_owned(),
        ));
        assert_eq!(skips(&report), [(q("Billing"), "binding", "scan-failed")]);
        assert_eq!(subjects(&report, ScanFailed), [vec![q("Billing")]]);
        assert_eq!(
            of(&report, ScanFailed)[0].message,
            "the scan of the checkout failed: no source"
        );
        let report = without(NoScan::Failed("bad \u{1b}[31m".to_owned()));
        assert_eq!(
            of(&report, ScanFailed)[0].message,
            "the scan of the checkout failed: bad \\u{1b}[31m"
        );
        assert_eq!(of(&report, ScanFailed)[0].severity, Severity::Warning);

        // Every path turned out missing: nothing went unjudged, so no SCAN_FAILED.
        let ghost = [module("Ghost", None, Some("DOCS"))];
        let failed = NoScan::Failed("no source".to_owned());
        let report = check(
            M,
            &ghost,
            &VaultConfig::default(),
            dir.path(),
            Err(failed),
            &none,
            &Facts::default(),
        );
        assert_eq!(found_codes(&report), [BindingPathMissing]);
    }

    #[test]
    fn anchors_must_name_existing_paths_and_exported_symbols() {
        let (dir, doc) = checkout();
        let [app, mut web, billing, bank] = shop();
        let anchor = |path: &str, symbol: &str| {
            Some(Anchor {
                repo: None,
                path: path.to_owned(),
                symbol: symbol.to_owned(),
            })
        };
        link(&mut web, "Good", "Billing", "call").anchor =
            anchor("app/billing/charge.ts", "charge");
        link(&mut web, "NoSymbol", "Billing", "call").anchor = anchor("app/billing", "refund");
        link(&mut web, "NoPath", "Billing", "call").anchor = anchor("app/gone.ts", "charge");
        let report = run(&[app, web, billing, bank], &doc, dir.path());
        assert_eq!(subjects(&report, AnchorPathMissing), [vec![q("NoPath")]]);
        assert_eq!(
            of(&report, AnchorPathMissing)[0].message,
            "Web -> Billing is anchored at app/gone.ts, which does not exist"
        );
        assert_eq!(
            subjects(&report, AnchorSymbolMissing),
            [vec![q("NoSymbol")]]
        );
        assert!(skips(&report).contains(&(q("NoPath"), "anchor", "path-missing")));
    }

    #[test]
    fn imports_past_a_bound_directorys_entry_points_are_information() {
        let (dir, mut doc) = checkout();
        let report = run(&shop(), &doc, dir.path());
        let found = of(&report, EntryPointUnexposed);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subjects, [q("Billing")]);
        assert_eq!(found[0].severity, Severity::Information);
        assert_eq!(
            found[0].message,
            "app/web/page.ts:3 imports app/billing/charge.ts inside Billing, past its entry points: app/billing/index.ts"
        );

        // A file the package publishes is an entry point too.
        doc.package_entries = vec!["app/billing/charge.ts".to_owned()];
        let report = run(&shop(), &doc, dir.path());
        assert_eq!(of(&report, EntryPointUnexposed), Vec::<&Diagnostic>::new());
    }

    #[test]
    fn a_scan_is_needed_only_for_existing_paths_of_this_repository() {
        let dir = TempDir::new("code-check").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        let mut remote = module("Remote", None, Some("src"));
        remote.binding.as_mut().unwrap().repo = Some("other".to_owned());
        let mut vendor = module("Vendor", None, Some("src"));
        vendor.kind = EXTERNAL_KIND.to_owned();
        let mut missing = module("Missing", None, Some("gone"));
        let modules = [remote.clone(), vendor, missing.clone()];
        assert!(!needs_scan(&modules, dir.path()));
        link(&mut missing, "R1", "Remote", "call").anchor = Some(Anchor {
            repo: None,
            path: "src".to_owned(),
            symbol: "x".to_owned(),
        });
        assert!(needs_scan(&[remote, missing], dir.path()));

        // An anchor counts whatever kind of module owns it.
        let mut vendor = module("Vendor", None, None);
        vendor.kind = EXTERNAL_KIND.to_owned();
        link(&mut vendor, "R2", "Vendor", "call").anchor = Some(Anchor {
            repo: None,
            path: "src".to_owned(),
            symbol: "x".to_owned(),
        });
        assert!(needs_scan(&[vendor], dir.path()));
    }

    #[test]
    fn paths_map_to_the_deepest_binding_without_a_scan() {
        let mut modules = shop().to_vec();
        modules.push(module("Root", None, Some(".")));
        modules.push(symbol(
            module("Charge", Some("Billing"), Some("app/billing")),
            "charge",
        ));
        modules.push(module("Gone", None, Some("app/gone")));
        let mut other = module("Other", None, Some("app"));
        other.binding.as_mut().unwrap().repo = Some("elsewhere".into());
        modules.push(other);
        let bound = bound_paths(M, &modules);
        let owners = PathOwners::new(bound.iter().map(|(i, p)| (i.as_str(), p.as_str())));
        let owner = |file| owners.owner(file);
        // Charge stands for a symbol in Billing's path, so it owns nothing.
        assert!(!bound.contains_key(&q("Charge")) && !bound.contains_key(&q("Other")));
        assert_eq!(owner("app/billing/charge.ts"), Some(q("Billing").as_str()));
        assert_eq!(owner("app/gone/old.ts"), Some(q("Gone").as_str()));
        assert_eq!(owner("app/webby.ts"), Some(q("App").as_str()));
        assert_eq!(owner("README.md"), Some(q("Root").as_str()));
    }

    #[test]
    fn skips_serialize_in_kebab_case() {
        let skip = Skip {
            subject: q("Web"),
            site: SkipSite::Relationship,
            reason: SkipReason::EvidenceNotCode,
        };
        assert_eq!(
            serde_json::to_value(skip).unwrap(),
            json!({"subject": q("Web"), "site": "relationship", "reason": "evidence-not-code"})
        );
    }
}
