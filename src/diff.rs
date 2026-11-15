//! Trend between two scans: module metrics, cycles and forbidden imports.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    Cycle, Diff, Document, Edge, Link, Module, ModuleDelta, SCHEMA, Snapshot, Verdict,
};

pub fn diff(old: &Document, new: &Document) -> Diff {
    let snapshot = |d: &Document| Snapshot {
        commit: d.source.git.as_ref().map(|g| g.commit.clone()),
        generated_at: d.source.generated_at.clone(),
    };
    fn by_id(d: &Document) -> BTreeMap<&str, &Module> {
        d.modules.iter().map(|m| (m.id.as_str(), m)).collect()
    }
    let (was, is) = (by_id(old), by_id(new));
    let missing = |a: &BTreeMap<&str, &Module>, b: &BTreeMap<&str, &Module>| {
        a.keys()
            .filter(|id| !b.contains_key(*id))
            .map(|id| id.to_string())
            .collect()
    };
    let modules = was
        .iter()
        .filter_map(|(id, o)| {
            let n = is.get(id)?;
            let delta = ModuleDelta {
                cohesion: [o.cohesion, n.cohesion],
                ca: [o.ca, n.ca],
                ce: [o.ce, n.ce],
                weighted_ca: [o.weighted_ca, n.weighted_ca],
                weighted_ce: [o.weighted_ce, n.weighted_ce],
                instability: [o.instability, n.instability],
            };
            Some((id.to_string(), delta))
        })
        .collect();
    let cycles_only_in = |a: &Document, b: &Document| -> Vec<Cycle> {
        a.cycles
            .iter()
            .filter(|c| {
                !b.cycles
                    .iter()
                    .any(|d| d.level == c.level && d.members == c.members)
            })
            .cloned()
            .collect()
    };
    // Imports with `verdict` in `a` whose pair has another verdict, or is missing, in `b`.
    let only_in = |a: &Document, b: &Document, verdict: Verdict| -> Vec<Edge> {
        let with = |e: &&Edge| e.verdict == Some(verdict);
        let theirs: BTreeSet<(&str, &str, crate::model::EdgeKind)> = b
            .edges
            .iter()
            .chain(&b.external_edges)
            .filter(with)
            .map(|e| (e.from.as_str(), e.to.as_str(), e.kind))
            .collect();
        a.edges
            .iter()
            .chain(&a.external_edges)
            .filter(with)
            .filter(|e| !theirs.contains(&(e.from.as_str(), e.to.as_str(), e.kind)))
            .cloned()
            .collect()
    };
    Diff {
        schema: SCHEMA,
        kind: "diff",
        old: snapshot(old),
        new: snapshot(new),
        modules,
        added_modules: missing(&is, &was),
        removed_modules: missing(&was, &is),
        new_cycles: cycles_only_in(new, old),
        new_cyclic_imports: crate::graph::new_cyclic_import_pairs(old, new)
            .into_iter()
            .map(|(from, to)| Link { from, to })
            .collect(),
        resolved_cycles: cycles_only_in(old, new),
        new_forbidden: only_in(new, old, Verdict::Forbidden),
        resolved_forbidden: only_in(old, new, Verdict::Forbidden),
        new_unplanned: only_in(new, old, Verdict::Unplanned),
        stale_known: new.stale_known.clone(),
        target: crate::target::delta(old, new),
    }
}

/// `n` and `noun`, plural unless `n` is 1.
pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}
