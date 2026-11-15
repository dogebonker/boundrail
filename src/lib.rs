// Library half of the CLI, shared with integration tests.

pub mod arch;
pub mod bisect;
pub mod cochange;
pub mod codeowners;
pub mod collect;
pub mod diff;
pub mod early_smells;
pub mod exposure;
pub mod files;
pub mod findings;
pub mod git;
pub mod grabbag;
pub mod graph;
pub mod hotspots;
pub mod known;
pub mod model;
pub mod modules;
pub mod package_splits;
pub mod quality;
pub mod recipes;
pub mod resolution;
pub mod rules;
pub mod simulate;
pub mod surface;
pub mod target;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use model::{Cochange, Config, Document, Edge, File, SCHEMA, Source, Summary, Tool};
use rules::Compiled;

/// 64 hex digits (256 bits) from the operating system's random source: `/dev/urandom` on Unix,
/// the system RNG on Windows. Fails on a target with neither, rather than return a guessable
/// value.
#[cfg(any(unix, windows))]
pub fn token() -> Result<String> {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read as _;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(h: *mut std::ffi::c_void, p: *mut u8, n: u32, flags: u32) -> i32;
        } // System RNG, with no algorithm handle.
        if unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), 32, 2) } < 0 {
            bail!("the system random number generator failed")
        };
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Always fails: this target has no operating system random source.
#[cfg(not(any(unix, windows)))]
pub fn token() -> Result<String> {
    bail!("no operating system random source on this target")
}

/// The scan pipeline the model's live scan runs: module assignment, barrels, verdicts under
/// `config.rules`, metrics, cycles and findings from collected files and edges (and git
/// co-changes, when read). Every edge must join two of `files`.
pub fn analyze(
    source: Source,
    config: Config,
    files: Vec<String>,
    edges: Vec<Edge>,
    package_entries: Vec<String>,
    cochange: Option<Cochange>,
) -> Result<Document> {
    let rules = rules::compile(&config)?;
    resolution::require_policy_accounting(&config)?;
    let owners = config
        .codeowners
        .as_ref()
        .map(codeowners::compile)
        .transpose()?;
    let mut doc = build(
        source,
        config,
        files,
        edges,
        package_entries,
        rules.as_ref(),
        owners.as_ref(),
    );
    if doc.modules.iter().any(|m| {
        ![
            m.weighted_ca,
            m.weighted_ce,
            m.weighted_internal_edges,
            m.weighted_outgoing_edges,
            m.weighted_ca + m.weighted_ce,
            m.weighted_internal_edges + m.weighted_outgoing_edges,
        ]
        .into_iter()
        .all(f64::is_finite)
    }) {
        bail!("edge_weights overflow the weighted metrics; use smaller finite weights");
    }
    resolution::validate(doc.imports.as_deref(), doc.globals.as_deref(), &doc.files)?;
    exposure::validate(&doc)?;
    findings::place_registrars(&mut doc, rules.as_ref(), owners.as_ref());
    doc.cochange = cochange;
    doc.findings = findings::find(&doc, rules.as_ref(), owners.as_ref());
    doc.target = rules
        .as_ref()
        .and_then(|rules| target::analyze(&doc, rules));
    resolution::attach(&mut doc);
    recipes::attach(&mut doc)?;
    Ok(doc)
}

/// `analyze` without findings, with the rules already compiled from `config.rules`.
fn build(
    source: Source,
    config: Config,
    files: Vec<String>,
    mut edges: Vec<Edge>,
    mut package_entries: Vec<String>,
    rules: Option<&Compiled>,
    owners: Option<&codeowners::Compiled>,
) -> Document {
    let weights = config
        .rules
        .as_ref()
        .and_then(|r| r.doc.edge_weights)
        .unwrap_or_default();
    for edge in &mut edges {
        if edge.kind == model::EdgeKind::TypeReexport {
            edge.is_reexport = true;
            edge.kind = model::EdgeKind::Type;
        }
        edge.weight = Some(weights.weight(edge.kind));
    }
    package_entries.sort();
    package_entries.dedup();
    edges.sort_by(|a, b| {
        (&a.from, &a.to, &a.original_to, a.kind).cmp(&(&b.from, &b.to, &b.original_to, b.kind))
    });
    let tests: BTreeSet<_> = files
        .iter()
        .filter(|path| modules::is_test(path, &config))
        .cloned()
        .collect();
    let module_of: BTreeMap<String, String> = files
        .into_iter()
        .map(|f| {
            let mut module = modules::assigned(&f, &config, source.language, rules);
            if !config.legacy_module_classes && tests.contains(&f) {
                module = format!("tests::{module}");
            }
            (f, module)
        })
        .collect();
    let (external_nodes, mut external_edges) = resolution::external_graph(&source, &config, rules);
    let internal_count = edges.len();
    edges.append(&mut external_edges);
    match rules {
        Some(rules) => {
            let mut policy_modules: BTreeMap<_, _> = module_of
                .keys()
                .map(|path| {
                    (
                        path.clone(),
                        rules
                            .module_of(path)
                            .unwrap_or_else(|| modules::folder_of(path, &config, source.language)),
                    )
                })
                .collect();
            for path in edges.iter().filter_map(|e| e.original_to.as_ref()) {
                policy_modules.entry(path.clone()).or_insert_with(|| {
                    rules
                        .module_of(path)
                        .unwrap_or_else(|| modules::folder_of(path, &config, source.language))
                });
            }
            rules.decide_with_externals(
                &mut edges,
                &policy_modules,
                &resolution::policy_names(&external_nodes),
            );
        }
        None => {
            for e in &mut edges {
                (e.verdict, e.rule, e.reach) = (None, None, None);
            }
        }
    }
    let external_edges = edges.split_off(internal_count);
    let inferred = modules::barrels(&edges);
    let barrels = module_of
        .keys()
        .filter(|path| {
            config
                .file_metadata
                .get(*path)
                .map_or_else(|| inferred.contains(*path), |m| m.barrel)
        })
        .cloned()
        .collect();
    let pairs = modules::weighted(&edges, &barrels);
    let entries = modules::cycle_entries(module_of.keys(), &config.source_root, &package_entries);
    let module_edges = graph::module_edges(&module_of, &edges);
    let (cycles, type_only_cycles) = quality::cycles(
        &module_of,
        edges
            .iter()
            .filter(|e| !tests.contains(&e.from) && !tests.contains(&e.to)),
        &entries,
        &barrels,
        source.language == model::Language::Go,
        config.all_kinds,
    );
    let mut metrics = graph::modules(&module_of, pairs.keys());
    quality::metrics(&mut metrics, &module_of, &pairs);
    for module in &mut metrics {
        if !config.legacy_module_classes
            && module_of
                .iter()
                .any(|(path, id)| id == &module.id && tests.contains(path))
        {
            module.class = Some(model::ModuleClass::Tests);
        }
    }
    let test_coverage = (config.include_tests || !tests.is_empty()).then(|| {
        let production: BTreeSet<_> = module_of
            .iter()
            .filter(|(path, _)| !tests.contains(*path))
            .map(|(_, id)| id.clone())
            .collect();
        let covered: BTreeSet<_> = edges
            .iter()
            .filter(|e| tests.contains(&e.from) && !tests.contains(&e.to))
            .map(|e| module_of[&e.to].clone())
            .collect();
        model::TestCoverage {
            modules: production.len(),
            covered: covered.len(),
            without_importing_tests: production.difference(&covered).cloned().collect(),
        }
    });
    let mut doc = Document {
        schema: SCHEMA,
        kind: "scan",
        tool: Tool {
            // Persisted report identity stays stable across the Boundrail product rename.
            name: "archgraph",
            version: env!("CARGO_PKG_VERSION"),
        },
        modules: metrics,
        cycles,
        type_only_cycles,
        test_coverage,
        imports: config.import_records.clone(),
        globals: config.globals.clone(),
        import_resolution: None,
        unresolved: Vec::new(),
        external_nodes,
        external_edges,
        external_exposure: None,
        grabbags: Vec::new(),
        files: module_of
            .iter()
            .map(|(path, module)| File {
                path: path.clone(),
                module: module.clone(),
                barrel: barrels.contains(path),
                class: (!config.legacy_module_classes && tests.contains(path))
                    .then_some(model::ModuleClass::Tests),
                owners: owners.map(|o| o.owners(path)).unwrap_or_default(),
            })
            .collect(),
        source,
        config,
        edges,
        package_entries,
        module_edges,
        summary: Summary::default(),
        findings: Vec::new(),
        diagnostics: Vec::new(),
        cochange: None,
        hotspots: None,
        symbols: None,
        surface: None,
        package_splits: None,
        target: None,
        stale_known: Vec::new(),
    };
    known::mark(&mut doc);
    if owners.is_some() {
        for module in &mut doc.modules {
            let mut ownership = model::ModuleOwnership {
                owned_files: 0,
                owners: BTreeMap::new(),
                primary: Vec::new(),
            };
            for file in doc.files.iter().filter(|f| f.module == module.id) {
                ownership.owned_files += usize::from(!file.owners.is_empty());
                for owner in &file.owners {
                    *ownership.owners.entry(owner.clone()).or_default() += 1;
                }
            }
            ownership.primary = ownership
                .owners
                .iter()
                .filter(|(_, n)| **n > module.files / 2)
                .map(|(owner, _)| owner.clone())
                .collect();
            module.ownership = Some(ownership);
        }
    }
    resolution::attach(&mut doc);
    grabbag::classify(&mut doc);
    doc.summary = simulate::summary(&doc);
    doc
}
