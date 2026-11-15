use std::collections::BTreeMap;

use boundrail::collect::RawGraph;
use boundrail::collect::barrels::{BarrelModule, ExportRoute, ImportRoute, resolve};
use boundrail::model::{Confidence, Edge, EdgeKind};

fn module(
    file: &str,
    pure: bool,
    declared: &[&str],
    exports: &[(&str, &str, &str)],
) -> BarrelModule {
    BarrelModule {
        file: file.into(),
        pure,
        declared: declared.iter().map(|s| (*s).into()).collect(),
        exports: exports
            .iter()
            .map(|(exported, imported, to)| ExportRoute {
                exported: (*exported).into(),
                imported: (*imported).into(),
                to: (*to).into(),
            })
            .collect(),
        imports: exports
            .iter()
            .map(|(_, imported, to)| {
                route(
                    to,
                    &[*imported],
                    *imported == "*",
                    EdgeKind::Reexport,
                    false,
                )
            })
            .collect(),
    }
}
fn route(
    to: &str,
    names: &[&str],
    wildcard: bool,
    kind: EdgeKind,
    side_effect: bool,
) -> ImportRoute {
    ImportRoute {
        to: to.into(),
        names: names.iter().map(|s| (*s).into()).collect(),
        wildcard,
        kind,
        specifier: Some(format!("./{to}")),
        line: 2,
        side_effect,
    }
}
fn graph(modules: &[BarrelModule]) -> RawGraph {
    RawGraph {
        imports: None,
        globals: None,
        files: modules.iter().map(|m| m.file.clone()).collect(),
        edges: modules
            .iter()
            .flat_map(|m| {
                m.imports.iter().map(|i| Edge {
                    from: m.file.clone(),
                    to: i.to.clone(),
                    kind: i.kind,
                    ..Edge::default()
                })
            })
            .collect(),
        collector: serde_json::from_value(
            serde_json::json!({"name":"test", "version":null, "cruised":modules.len()}),
        )
        .unwrap(),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn named_and_default_aliases_resolve_through_multiple_barrels() {
    let mut user = module("user.ts", false, &[], &[]);
    user.imports = vec![route(
        "entry.ts",
        &["publicName", "default"],
        false,
        EdgeKind::Static,
        false,
    )];
    let modules = vec![
        user,
        module(
            "entry.ts",
            true,
            &[],
            &[
                ("publicName", "middle", "bridge.ts"),
                ("default", "default", "bridge.ts"),
            ],
        ),
        module(
            "bridge.ts",
            true,
            &[],
            &[
                ("middle", "original", "leaf.ts"),
                ("default", "default", "leaf.ts"),
            ],
        ),
        module("leaf.ts", false, &["original", "default"], &[]),
    ];
    let mut graph = graph(&modules);
    assert!(resolve(&mut graph, &modules).is_empty());
    let user = graph.edges.iter().find(|e| e.from == "user.ts").unwrap();
    assert_eq!(user.to, "leaf.ts");
    assert_eq!(user.original_to.as_deref(), Some("entry.ts"));
    assert_eq!(user.kind, EdgeKind::Static);
    assert_eq!(user.confidence, Some(Confidence::High));
    assert!(graph.metadata["entry.ts"].barrel);
    assert!(
        graph
            .edges
            .iter()
            .filter(|e| e.from == "entry.ts")
            .all(|e| e.to == "leaf.ts" && e.is_reexport)
    );
}

#[test]
fn star_exports_require_unique_symbol_proof_and_side_effects_keep_runtime_loading() {
    let mut user = module("user.ts", false, &[], &[]);
    user.imports = vec![route("entry.ts", &["Foo"], false, EdgeKind::Static, false)];
    let mut modules = vec![
        user,
        module(
            "entry.ts",
            true,
            &[],
            &[("*", "*", "left.ts"), ("*", "*", "right.ts")],
        ),
        module("left.ts", false, &["Foo"], &[]),
        module("right.ts", false, &["Bar"], &[]),
    ];
    let mut resolved = graph(&modules);
    resolve(&mut resolved, &modules);
    assert_eq!(
        resolved
            .edges
            .iter()
            .find(|e| e.from == "user.ts")
            .unwrap()
            .to,
        "left.ts"
    );
    modules[3].declared.push("Foo".into());
    let mut ambiguous = graph(&modules);
    assert!(
        resolve(&mut ambiguous, &modules)
            .iter()
            .any(|w| w.contains("ambiguous"))
    );
    let user = ambiguous
        .edges
        .iter()
        .find(|e| e.from == "user.ts")
        .unwrap();
    assert_eq!(user.to, "entry.ts");
    assert_eq!(user.kind, EdgeKind::Reexport);
    assert_eq!(user.confidence, Some(Confidence::Low));
    modules[0].imports[0].wildcard = true;
    let mut namespace = graph(&modules);
    resolve(&mut namespace, &modules);
    assert_eq!(
        namespace
            .edges
            .iter()
            .find(|e| e.from == "user.ts")
            .unwrap()
            .to,
        "entry.ts"
    );
    modules[0].imports[0].side_effect = true;
    let mut side_effect = graph(&modules);
    resolve(&mut side_effect, &modules);
    let user = side_effect
        .edges
        .iter()
        .find(|e| e.from == "user.ts")
        .unwrap();
    assert_eq!(user.to, "entry.ts");
    assert_eq!(user.kind, EdgeKind::Static);
    assert_eq!(user.confidence, None);
}

#[test]
fn loops_and_depth_limits_keep_a_low_confidence_written_target() {
    let mut user = module("user.ts", false, &[], &[]);
    user.imports = vec![route("a.ts", &["Foo"], false, EdgeKind::Type, false)];
    let modules = vec![
        user,
        module("a.ts", true, &[], &[("Foo", "Foo", "b.ts")]),
        module("b.ts", true, &[], &[("Foo", "Foo", "a.ts")]),
    ];
    let mut cyclic = graph(&modules);
    assert!(
        resolve(&mut cyclic, &modules)
            .iter()
            .any(|w| w.contains("cycle"))
    );
    let user = cyclic.edges.iter().find(|e| e.from == "user.ts").unwrap();
    assert_eq!(user.to, "a.ts");
    assert_eq!(user.kind, EdgeKind::Reexport);
    assert_eq!(user.confidence, Some(Confidence::Low));
    for length in [10, 11] {
        let mut user = module("user.ts", false, &[], &[]);
        user.imports = vec![route("b0.ts", &["Foo"], false, EdgeKind::Static, false)];
        let mut modules = vec![user];
        for i in 0..length {
            modules.push(module(
                &format!("b{i}.ts"),
                true,
                &[],
                &[("Foo", "Foo", &format!("b{}.ts", i + 1))],
            ));
        }
        modules.push(module(&format!("b{length}.ts"), false, &["Foo"], &[]));
        let mut limited = graph(&modules);
        let warnings = resolve(&mut limited, &modules);
        let user = limited.edges.iter().find(|e| e.from == "user.ts").unwrap();
        if length == 10 {
            assert_eq!(user.to, "b10.ts");
        } else {
            assert_eq!(user.to, "b0.ts");
            assert!(warnings.iter().any(|w| w.contains("depth 10")));
        }
    }
}

#[test]
fn distinct_written_boundaries_survive_resolving_to_the_same_pair() {
    let mut user = module("user.ts", false, &[], &[]);
    user.imports = vec![
        route("entry.ts", &["Foo"], false, EdgeKind::Type, false),
        route("leaf.ts", &["Foo"], false, EdgeKind::Static, false),
    ];
    let modules = vec![
        user,
        module("entry.ts", true, &[], &[("Foo", "Foo", "leaf.ts")]),
        module("leaf.ts", false, &["Foo"], &[]),
    ];
    let mut graph = graph(&modules);
    resolve(&mut graph, &modules);
    let user: Vec<_> = graph.edges.iter().filter(|e| e.from == "user.ts").collect();
    assert_eq!(user.len(), 2);
    assert!(user.iter().all(|e| e.to == "leaf.ts"));
    assert!(
        user.iter()
            .any(|e| e.original_to.as_deref() == Some("entry.ts") && e.kind == EdgeKind::Type)
    );
    assert!(
        user.iter()
            .any(|e| e.original_to.is_none() && e.kind == EdgeKind::Static)
    );
}

#[test]
fn a_star_does_not_forward_default_exports_and_mixed_modules_are_not_bypassed() {
    let mut user = module("user.ts", false, &[], &[]);
    user.imports = vec![route(
        "entry.ts",
        &["default"],
        false,
        EdgeKind::Static,
        false,
    )];
    let mut modules = vec![
        user,
        module("entry.ts", true, &[], &[("*", "*", "leaf.ts")]),
        module("leaf.ts", false, &["default"], &[]),
    ];
    let mut star = graph(&modules);
    resolve(&mut star, &modules);
    assert_eq!(
        star.edges.iter().find(|e| e.from == "user.ts").unwrap().to,
        "entry.ts"
    );
    modules[1].pure = false;
    let mut mixed = graph(&modules);
    resolve(&mut mixed, &modules);
    let user = mixed.edges.iter().find(|e| e.from == "user.ts").unwrap();
    assert_eq!(user.to, "entry.ts");
    assert_eq!(user.kind, EdgeKind::Static);
    assert_eq!(user.confidence, None);
}

#[test]
fn executable_reexport_modules_retain_runtime_cycles() {
    use std::collections::BTreeSet;
    let mixed = module(
        "mixed.ts",
        false,
        &["Initialized"],
        &[("Foo", "Foo", "leaf.ts")],
    );
    let mut leaf = module("leaf.ts", false, &["Foo"], &[]);
    leaf.imports.push(route(
        "mixed.ts",
        &["Initialized"],
        false,
        EdgeKind::Static,
        false,
    ));
    let modules = vec![mixed, leaf];
    let mut graph = graph(&modules);
    resolve(&mut graph, &modules);
    let reexport = graph.edges.iter().find(|e| e.from == "mixed.ts").unwrap();
    assert_eq!(reexport.kind, EdgeKind::Static);
    assert!(reexport.is_reexport);
    let module_of = graph
        .files
        .iter()
        .map(|file| (file.clone(), file.clone()))
        .collect();
    let cycles = boundrail::graph::cycles(
        &module_of,
        &graph.edges,
        &BTreeSet::new(),
        &BTreeSet::new(),
        false,
    );
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].members, ["leaf.ts", "mixed.ts"]);
}
