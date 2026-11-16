//! Symbol-directed forwarding resolution shared by source collectors.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::RawGraph;
use crate::model::{Confidence, Edge, EdgeKind, NodeMetadata};
use crate::surface::{SymbolExport, SymbolImport, SymbolTarget};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarrelModule {
    pub file: String,
    pub pure: bool,
    pub declared: Vec<String>,
    pub exports: Vec<ExportRoute>,
    pub imports: Vec<ImportRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRoute {
    pub exported: String,
    pub imported: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRoute {
    pub to: String,
    pub names: Vec<String>,
    pub wildcard: bool,
    pub kind: EdgeKind,
    pub specifier: Option<String>,
    pub line: usize,
    pub side_effect: bool,
}

pub fn resolve(graph: &mut RawGraph, modules: &[BarrelModule]) -> Vec<String> {
    let modules: BTreeMap<_, _> = modules
        .iter()
        .map(|module| (module.file.as_str(), module))
        .collect();
    for module in modules.values() {
        graph.metadata.insert(
            module.file.clone(),
            NodeMetadata {
                barrel: module.pure,
                ..Default::default()
            },
        );
    }
    let mut warnings = BTreeSet::new();
    let mut output = Vec::new();
    for edge in &graph.edges {
        let routes: Vec<_> = modules
            .get(edge.from.as_str())
            .into_iter()
            .flat_map(|module| &module.imports)
            .filter(|route| route.to == edge.to)
            .collect();
        if routes.is_empty() {
            output.push(edge.clone());
            continue;
        }
        for route in routes {
            let mut updated = edge.clone();
            updated.kind = if matches!(edge.kind, EdgeKind::Type | EdgeKind::TypeReexport)
                && matches!(route.kind, EdgeKind::Static | EdgeKind::Reexport)
            {
                if route.kind == EdgeKind::Reexport {
                    EdgeKind::TypeReexport
                } else {
                    EdgeKind::Type
                }
            } else {
                route.kind
            };
            updated.weight = None;
            updated.is_reexport = matches!(route.kind, EdgeKind::Reexport | EdgeKind::TypeReexport)
                || modules
                    .get(edge.from.as_str())
                    .is_some_and(|module| module.pure)
                    && !route.side_effect;
            let source_is_barrel = modules
                .get(edge.from.as_str())
                .is_some_and(|module| module.pure);
            if source_is_barrel && updated.is_reexport && updated.kind == EdgeKind::Static {
                updated.kind = EdgeKind::Reexport;
            } else if !source_is_barrel && updated.kind == EdgeKind::Reexport {
                // Executable modules still load the modules they re-export.
                updated.kind = EdgeKind::Static;
            }
            updated.specifier = route.specifier.clone();
            updated.line = Some(route.line);
            updated.names = route.names.clone();
            let target_is_barrel = modules
                .get(route.to.as_str())
                .is_some_and(|module| module.pure);
            if !target_is_barrel || route.side_effect {
                if route.side_effect {
                    updated.kind = EdgeKind::Static;
                    updated.is_reexport = false;
                }
                output.push(updated);
                continue;
            }
            let mut targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut failure = None;
            if route.wildcard || route.names.is_empty() {
                failure = Some("namespace or star import cannot be narrowed".to_owned());
            } else {
                for name in &route.names {
                    match target(&modules, &route.to, name, 0, &mut BTreeSet::new()) {
                        Ok(file) => {
                            targets.entry(file).or_default().push(name.clone());
                        }
                        Err(reason) => {
                            failure = Some(reason);
                            break;
                        }
                    }
                }
            }
            if let Some(reason) = failure {
                updated.kind = EdgeKind::Reexport;
                updated.confidence = Some(Confidence::Low);
                updated.resolution_reason = Some(reason.clone());
                warnings.insert(format!(
                    "{}:{} → {}: {reason}",
                    updated.from, route.line, route.to
                ));
                output.push(updated);
            } else {
                for (file, names) in targets {
                    let mut resolved = updated.clone();
                    resolved
                        .original_to
                        .get_or_insert_with(|| resolved.to.clone());
                    resolved.to = file;
                    resolved.names = names;
                    resolved.confidence = Some(Confidence::High);
                    output.push(resolved);
                }
            }
        }
    }
    // Keep the strongest occurrence of a pair; a real runtime occurrence beats structural wiring.
    let mut unique: BTreeMap<(String, String, String), Edge> = BTreeMap::new();
    for edge in output {
        let key = (
            edge.from.clone(),
            edge.to.clone(),
            edge.original_to.clone().unwrap_or_else(|| edge.to.clone()),
        );
        match unique.get_mut(&key) {
            Some(existing) if edge.kind > existing.kind => *existing = edge,
            Some(existing)
                if edge.kind == existing.kind
                    && existing.confidence == Some(Confidence::Low)
                    && edge.confidence != Some(Confidence::Low) =>
            {
                *existing = edge
            }
            None => {
                unique.insert(key, edge);
            }
            _ => {}
        }
    }
    graph.edges = unique.into_values().collect();
    warnings.into_iter().collect()
}

/// Forward symbol exports through re-export routes until no route adds an export.
pub fn forward_exports(
    exports: &mut Vec<SymbolExport>,
    modules: &[BarrelModule],
    star_skips_default: bool,
) {
    for _ in 0..10 {
        let before = exports.len();
        let mut by_file: BTreeMap<&str, Vec<&SymbolExport>> = BTreeMap::new();
        for export in exports.iter() {
            by_file.entry(&export.file).or_default().push(export);
        }
        let mut forwarded = Vec::new();
        for module in modules {
            for route in &module.exports {
                for export in by_file.get(route.to.as_str()).into_iter().flatten() {
                    if route.imported == export.name
                        || route.imported == "*"
                            && !(star_skips_default && export.name == "default")
                    {
                        forwarded.push(SymbolExport {
                            file: module.file.clone(),
                            name: if route.exported == "*" {
                                export.name.clone()
                            } else {
                                route.exported.clone()
                            },
                            target: export.target.clone(),
                        });
                    }
                }
            }
        }
        exports.extend(forwarded);
        exports.sort();
        exports.dedup();
        if exports.len() == before {
            break;
        }
    }
}

/// Point each symbol import at the definition behind its export when that export is unique.
pub fn import_targets(imports: &mut [SymbolImport], exports: &[SymbolExport]) {
    let mut targets: BTreeMap<(&str, &str), Vec<&SymbolTarget>> = BTreeMap::new();
    for export in exports {
        targets
            .entry((&export.file, &export.name))
            .or_default()
            .push(&export.target);
    }
    for import in imports {
        if let Some([target]) = targets
            .get(&(import.to.as_str(), import.name.as_str()))
            .map(Vec::as_slice)
        {
            import.target = Some((*target).clone());
        }
    }
}

/// Point import records at the files their edges reach after forwarding resolution.
pub fn retarget_records(graph: &mut RawGraph, with_kind: bool) {
    let mut reached: BTreeMap<(&str, Option<usize>, &str), Vec<&Edge>> = BTreeMap::new();
    for edge in &graph.edges {
        if let Some(specifier) = &edge.specifier {
            reached
                .entry((&edge.from, edge.line, specifier))
                .or_default()
                .push(edge);
        }
    }
    for record in graph.imports.iter_mut().flatten() {
        let Some(edges) =
            reached.get(&(record.from.as_str(), record.line, record.specifier.as_str()))
        else {
            continue;
        };
        if with_kind {
            record.kind = edges.iter().map(|e| e.kind).max().unwrap_or(record.kind);
        }
        record.targets = edges
            .iter()
            .map(|e| e.to.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
}

fn target<'a>(
    modules: &BTreeMap<&str, &'a BarrelModule>,
    file: &'a str,
    name: &str,
    depth: usize,
    visiting: &mut BTreeSet<(String, String)>,
) -> std::result::Result<String, String> {
    let Some(module) = modules.get(file) else {
        return Err(format!("missing export metadata for {file}"));
    };
    if !module.pure {
        return Ok(file.to_owned());
    }
    if depth >= 10 {
        return Err("re-export chain exceeds depth 10".into());
    }
    let key = (file.to_owned(), name.to_owned());
    if !visiting.insert(key.clone()) {
        return Err(format!("re-export chain cycle at {file}:{name}"));
    }
    let explicit: Vec<_> = module
        .exports
        .iter()
        .filter(|route| route.exported == name)
        .collect();
    let candidates: Vec<_> = if explicit.is_empty() {
        module
            .exports
            .iter()
            .filter(|route| route.exported == "*" && name != "default")
            .collect()
    } else {
        explicit
    };
    let mut resolved = BTreeSet::new();
    let mut failures = Vec::new();
    for route in candidates {
        if route.imported == "*" && route.exported != "*" {
            failures.push("namespace re-export cannot be narrowed".into());
            continue;
        }
        let imported = if route.imported == "*" {
            name
        } else {
            route.imported.as_str()
        };
        // Star exports require proof that the target exposes this particular symbol.
        if route.exported == "*" && !exposes(modules, &route.to, imported, &mut BTreeSet::new(), 0)
        {
            continue;
        }
        match target(modules, &route.to, imported, depth + 1, visiting) {
            Ok(file) => {
                resolved.insert(file);
            }
            Err(reason) => failures.push(reason),
        }
    }
    visiting.remove(&key);
    if resolved.len() > 1 {
        return Err(format!("ambiguous star export {name} from {file}"));
    }
    if !failures.is_empty() {
        return Err(failures.remove(0));
    }
    resolved.into_iter().next().ok_or_else(|| {
        format!("named export {name} is unproven by {file}; export metadata may be incomplete")
    })
}

fn exposes(
    modules: &BTreeMap<&str, &BarrelModule>,
    file: &str,
    name: &str,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 10 || !seen.insert(file.to_owned()) {
        return false;
    }
    let Some(module) = modules.get(file) else {
        return false;
    };
    if module.declared.iter().any(|declared| declared == name)
        || module.exports.iter().any(|route| route.exported == name)
    {
        return true;
    }
    module
        .exports
        .iter()
        .filter(|route| route.exported == "*")
        .any(|route| exposes(modules, &route.to, name, &mut seen.clone(), depth + 1))
}
