//! The live native scan the model commands read code evidence from.
use super::*;
use anyhow::bail;
use boundrail::collect::excludes::{self, Policy};
use boundrail::model::{CollectionOptions, Config, LanguageSymbols, ScanFailure};

/// Live native evidence for `model check`, `inspect` and `footprint`: files, edges, package
/// entries, each file's language and symbols of the checkout at `root`, under the default
/// excludes. Every language present is scanned on its own and the results merged; the first,
/// the one with the most files, names the document's language. A language whose collector fails
/// is warned about and listed in `scan_failures` with the files its own policy selects; only
/// when every one fails does the scan fail. With several languages, each one's symbol coverage
/// is kept in `symbol_languages`, as the merged `complete` holds only when every one's does.
pub(super) fn live(root: &Path) -> Result<Document> {
    let source_dir = collect::source_dir(root);
    let found = collect::detect_languages(root, source_dir, excludes::Options::default())?;
    let (mut merged, mut failures, mut coverage) = (None::<Document>, Vec::new(), Vec::new());
    for (language, files) in found {
        let policy = Policy::new(root, language, excludes::Options::default())?;
        match scan(root, source_dir, language, &policy) {
            Ok(doc) => {
                coverage.extend(symbol_coverage(&doc));
                match &mut merged {
                    None => merged = Some(doc),
                    Some(into) => merge(into, doc),
                }
            }
            Err(error) => failures.push(ScanFailure {
                language,
                error: format!("{error:#}"),
                files: policy.select(&files)?,
            }),
        }
    }
    let Some(mut doc) = merged else {
        let errors: Vec<_> = failures.into_iter().map(|f| f.error).collect();
        bail!("{}", errors.join("; "));
    };
    for failure in &failures {
        let name = failure.language.as_str();
        eprintln!("warning: the {name} scan failed: {}", failure.error);
    }
    doc.files.sort_by(|a, b| a.path.cmp(&b.path));
    doc.scan_failures = failures;
    if coverage.len() > 1 {
        doc.symbol_languages = coverage;
    }
    Ok(doc)
}

/// The symbol coverage of `doc`, a scan of one language: whether it is complete and the
/// limitations naming none of its files.
fn symbol_coverage(doc: &Document) -> Option<LanguageSymbols> {
    let symbols = doc.symbols.as_ref()?;
    let scanned: std::collections::HashSet<_> = doc.files.iter().map(|f| f.path.as_str()).collect();
    let limitations = (symbols.limitations.iter())
        .filter(|l| {
            l.split_once(':')
                .is_none_or(|(path, _)| !scanned.contains(path))
        })
        .cloned()
        .collect();
    Some(LanguageSymbols {
        language: doc.source.language,
        complete: symbols.complete,
        limitations,
    })
}

/// Adds the files, edges, package entries and symbols of `doc`, a scan of another language, to
/// `into`.
fn merge(into: &mut Document, doc: Document) {
    into.files.extend(doc.files);
    into.edges.extend(doc.edges);
    into.package_entries.extend(doc.package_entries);
    match (&mut into.symbols, doc.symbols) {
        (Some(symbols), Some(more)) => {
            symbols.complete &= more.complete;
            symbols.exports.extend(more.exports);
            symbols.imports.extend(more.imports);
            symbols.limitations.extend(more.limitations);
        }
        (symbols, more) => *symbols = symbols.take().or(more),
    }
}

/// The scan of the files of one language under its exclude policy.
fn scan(root: &Path, source_dir: &str, language: Language, policy: &Policy) -> Result<Document> {
    let (output, python_package) = collect_native(root, source_dir, language, policy)?;
    let (mut raw, symbols) = (output.raw, output.symbols);
    let excluded_files = keep_selected(&mut raw, policy)?;
    let mut config = config(language, &mut raw, output.source_root, python_package);
    config.excludes = policy.patterns().to_vec();
    config.excluded_files = excluded_files;
    // The model reads none of the provenance fields.
    let source = Source {
        language,
        collector: raw.collector,
        git: None,
        generated_at: String::new(),
        scan_root: None,
    };
    let entries = match language {
        Language::Typescript => collect::depcruise::package_entries(root, &raw.files),
        _ => Vec::new(),
    };
    let mut doc = boundrail::analyze(source, config, raw.files, raw.edges, entries, None)?;
    attach_symbols(&mut doc, symbols)?;
    Ok(doc)
}

/// Keeps what the exclude policy selects, and returns the files it left out.
fn keep_selected(raw: &mut collect::RawGraph, policy: &Policy) -> Result<Vec<String>> {
    let kept: std::collections::HashSet<_> = policy.select(&raw.files)?.into_iter().collect();
    let excluded = (raw.files.iter())
        .filter(|p| !kept.contains(*p))
        .cloned()
        .collect();
    raw.files.retain(|p| kept.contains(p));
    raw.edges
        .retain(|e| kept.contains(&e.from) && kept.contains(&e.to));
    if let Some(imports) = &mut raw.imports {
        imports.retain(|i| kept.contains(&i.from));
    }
    if let Some(globals) = &mut raw.globals {
        globals.retain(|g| kept.contains(&g.from));
    }
    Ok(excluded)
}

/// The scan configuration of a native collection, taking the collector's metadata, import
/// records and globals from `raw`; excludes are the caller's to fill.
fn config(
    language: Language,
    raw: &mut collect::RawGraph,
    source_root: String,
    python_package: Option<String>,
) -> Config {
    // Java keeps package directories intact: its source root is the scan root, so depth 2 would
    // put a Maven tree in `src/main`.
    let package_modules = language == Language::Java;
    Config {
        depth: if package_modules { 0 } else { 2 },
        granularity: Granularity::default(),
        package_modules,
        source_root,
        excludes: Vec::new(),
        include_tests: raw.files.iter().any(|p| excludes::is_test(p)),
        include_generated: false,
        includes: Vec::new(),
        excluded_files: Vec::new(),
        explicit_excludes: Some(Vec::new()),
        all_kinds: false,
        no_default_excludes: false,
        rules: None,
        codeowners: None,
        collection: Some(CollectionOptions {
            engine: collect::Engine::Native,
            python_package,
            python_paths: Vec::new(),
        }),
        file_metadata: std::mem::take(&mut raw.metadata),
        legacy_module_classes: false,
        import_records: raw.imports.take(),
        globals: raw.globals.take(),
    }
}

/// Attaches the symbols whose files the scan kept.
fn attach_symbols(doc: &mut Document, mut symbols: boundrail::surface::Symbols) -> Result<()> {
    let kept: std::collections::BTreeSet<_> = doc.files.iter().map(|f| f.path.as_str()).collect();
    symbols.exports.retain(|export| {
        kept.contains(export.file.as_str()) && kept.contains(export.target.file.as_str())
    });
    symbols.imports.retain(|import| {
        kept.contains(import.from.as_str())
            && kept.contains(import.to.as_str())
            && (import.target.as_ref()).is_none_or(|target| kept.contains(target.file.as_str()))
    });
    boundrail::surface::attach(doc, symbols, boundrail::surface::DEFAULT_MAX_SYMBOLS)
}

/// The native collector's output for `language`, its limitations warned and recorded, and the
/// Python package it scanned.
fn collect_native(
    root: &Path,
    source_dir: &str,
    language: Language,
    policy: &Policy,
) -> Result<(collect::native::Output, Option<String>)> {
    let mut output = match language {
        // The whole checkout, not only `source_dir`: in a checkout of several languages the
        // TypeScript often sits beside `src/`.
        Language::Typescript => {
            collect::native::typescript::collect(root, source_dir, policy, true)?
        }
        Language::Python => collect::native::python::collect(root, source_dir, None, &[], policy)?,
        Language::Go => collect::native::go::collect(root, "go", policy)?,
        Language::Rust => collect::native::rust::collect(root, policy)?,
        Language::Java => collect::native::java::collect(root, policy)?,
        Language::Elixir => collect::native::elixir::collect(root, policy)?,
    };
    let python_package = (language == Language::Python).then(|| {
        (output.source_root)
            .strip_prefix(&format!("{source_dir}/"))
            .unwrap_or(&output.source_root)
            .replace('/', ".")
    });
    let imports = output.raw.imports.as_ref();
    if imports.is_some_and(|imports| imports.iter().any(|import| import.unresolved.is_some())) {
        output.symbols.complete = false;
        output.symbols.limitations.push("Unresolved requests can hide local symbol consumers; unused-export suggestions are withheld.".into());
    }
    output.symbols.limitations.sort();
    output.symbols.limitations.dedup();
    (output.raw.collector.limitations).clone_from(&output.symbols.limitations);
    for limit in output.raw.collector.limitations.iter().take(10) {
        eprintln!("warning: {limit}");
    }
    Ok((output, python_package))
}
