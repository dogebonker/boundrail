//! `model footprint`: the modules a branch's changes touch. Changed paths map to modules by
//! binding path, the deepest binding winning as in the code check, and roll up to the modules
//! containing them. Import edges of the changed files show which relationships the change
//! crosses, and the vault's own changes compare the branch's model with its base's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::code::{self, PathOwners};
use super::compare::{self, Comparison};
use super::index::Index;
use super::types::{Collection, Module};
use super::{Code, Refusal, vault};
use crate::git;
use crate::model::Document;

#[derive(Debug, Default, Serialize)]
pub struct FootprintReport {
    /// Modules holding a changed file or containing one that does, by qualified ID.
    pub modules: Vec<Touched>,
    /// Changed files outside every binding.
    pub outside: Vec<String>,
    /// Module pairs an import edge of a changed file joins; `None` when they were not checked.
    pub crossings: Option<Vec<Crossing>>,
    /// Why `crossings` is `None`.
    pub crossings_skipped: Option<CrossingsSkipped>,
    /// What the model changes against the base's; `None` when the base has no model.
    pub model: Option<Comparison>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Touched {
    pub module: String,
    pub name: String,
    /// Changed files the module holds itself, not through a part.
    pub files: Vec<String>,
    /// Changed files its parts hold, at any depth.
    pub in_parts: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Crossing {
    pub from: String,
    pub to: String,
    /// Import edges from `from` to `to` with a changed file at either end.
    pub edges: usize,
    /// Relationships drawn from `from` or a module containing it to `to` or one containing it;
    /// empty when the crossing is undrawn.
    pub relationships: Vec<String>,
}

/// Why a footprint has no crossings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrossingsSkipped {
    /// No binding or anchor names existing code, so nothing was scanned.
    NotNeeded,
    ScanFailed,
    /// The scan reads the checkout, which is not at the head commit.
    HeadNotCheckedOut,
}

impl CrossingsSkipped {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotNeeded => "not-needed",
            Self::ScanFailed => "scan-failed",
            Self::HeadNotCheckedOut => "head-not-checked-out",
        }
    }
}

/// How the base was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseSource {
    /// `--base` named it.
    Explicit,
    /// A default branch named it.
    MergeBase,
    /// No default branch exists, so the base is the head itself.
    HeadFallback,
}

/// The commit a footprint starts from: the merge-base of the head with the reference named.
#[derive(Debug, Serialize)]
pub struct Base {
    pub commit: String,
    pub source: BaseSource,
    /// The reference as given or found; `None` for the head fallback.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

/// What a footprint compares.
#[derive(Debug)]
pub struct Range {
    pub base: Base,
    /// The head commit; `None` for HEAD with the working tree.
    pub head: Option<String>,
    /// Whether the checkout is at the head, so its scan shows the head's edges.
    pub checked_out: bool,
}

/// The range a footprint of the checkout `root` covers: from the merge-base of `base` (else the
/// first of origin/HEAD, main or master that exists) and the head, to `head` or the working tree.
/// Refuses `REQUEST_INVALID` for a revision that names no commit, for revisions with no common
/// ancestor, and when a shallow checkout has no default branch to start from.
pub fn range(root: &Path, base: Option<&str>, head: Option<&str>) -> Result<Range> {
    let commit = |flag: &str, rev: &str| {
        git::resolve(root, rev).map_err(|error| {
            let message = format!("--{flag} {rev} names no commit: {error:#}");
            anyhow::Error::from(Refusal::new(Code::RequestInvalid, message))
        })
    };
    let head = head.map(|rev| commit("head", rev)).transpose()?;
    let current = git::resolve(root, "HEAD")?;
    let tip = head.clone().unwrap_or_else(|| current.clone());
    let (reference, source) = match base {
        Some(rev) => (Some(rev), BaseSource::Explicit),
        None => match (git::DEFAULT_BRANCHES.iter()).find(|r| git::resolve(root, r).is_ok()) {
            Some(found) => (Some(*found), BaseSource::MergeBase),
            None if git::is_shallow(root)? => {
                let message = "the checkout is shallow and has no origin/HEAD, main or master: \
                               pass --base and fetch enough history to reach it";
                return Err(Refusal::new(Code::RequestInvalid, message).into());
            }
            None => (None, BaseSource::HeadFallback),
        },
    };
    let from = reference.map_or(Ok(tip.clone()), |rev| commit("base", rev))?;
    let merge = git::text(root, &["merge-base", &from, &tip]).map_err(|error| {
        let message = format!("the base and the head have no common ancestor: {error:#}");
        Refusal::new(Code::RequestInvalid, message)
    })?;
    Ok(Range {
        base: Base {
            commit: merge.trim().to_owned(),
            source,
            reference: reference.map(str::to_owned),
        },
        checked_out: head.as_ref().is_none_or(|h| *h == current),
        head,
    })
}

/// The paths changed from `base` to `head`, or to the working tree with its untracked files
/// when `head` is `None`, in the checkout `root`, less the vault's own files. Both are commit
/// hashes; renames count as a deletion and an addition.
pub fn changed(root: &Path, base: &str, head: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec![
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        base,
    ];
    args.extend(head);
    args.push("--");
    let mut files = split(&git::run(root, &args)?);
    if head.is_none() {
        let untracked = ["ls-files", "--others", "--exclude-standard", "-z"];
        files.extend(split(&git::run(root, &untracked)?));
    }
    files.retain(|f| !vault::in_vault(f));
    files.sort_unstable();
    files.dedup();
    Ok(files)
}

fn split(out: &[u8]) -> Vec<String> {
    (out.split(|b| *b == 0))
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// The modules and collections of one state.
pub type State<'a> = (&'a [Module], &'a [Collection]);

/// The footprint of `files` on the state `head` of model `model`, with the edges of `scan` and
/// what `head` changes against `base`. Without changed files there are no crossings to check.
pub fn measure(
    model: &str,
    head: State<'_>,
    base: Option<State<'_>>,
    files: &[String],
    scan: Result<&Document, CrossingsSkipped>,
) -> FootprintReport {
    let index = Index::new(model, head.0);
    let bound = code::bound_paths(model, head.0);
    let owners = PathOwners::new(bound.iter().map(|(i, p)| (i.as_str(), p.as_str())));
    let mut report = FootprintReport::default();
    let mut touched: BTreeMap<&str, Touched> = BTreeMap::new();
    for file in files {
        let Some(id) = owners.owner(file) else {
            report.outside.push(file.clone());
            continue;
        };
        for (depth, above) in index.chain(id).into_iter().enumerate() {
            let entry = touched.entry(above).or_insert_with(|| Touched {
                module: above.to_owned(),
                name: index.name(above).to_owned(),
                files: Vec::new(),
                in_parts: 0,
            });
            match depth {
                0 => entry.files.push(file.clone()),
                _ => entry.in_parts += 1,
            }
        }
    }
    report.modules = touched.into_values().collect();
    match scan {
        _ if files.is_empty() => report.crossings = Some(Vec::new()),
        Ok(doc) => report.crossings = Some(crossings(&index, &owners, files, doc)),
        Err(skipped) => report.crossings_skipped = Some(skipped),
    }
    report.model = base.map(|base| compare::compare(model, base, head));
    report
}

/// The module pairs, neither containing the other, that an import edge with a changed file at
/// either end joins, with the relationships drawn that way between them or modules containing
/// them.
fn crossings(
    index: &Index<'_>,
    owners: &PathOwners<'_, '_>,
    files: &[String],
    doc: &Document,
) -> Vec<Crossing> {
    let changed: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let touching = (doc.edges.iter())
        .filter(|e| changed.contains(e.from.as_str()) || changed.contains(code::written(e)));
    let mut pairs: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (from, to, _) in code::owner_pairs(touching, |f| owners.owner(f)) {
        if !index.is_below(from, to) && !index.is_below(to, from) {
            *pairs.entry((from, to)).or_default() += 1;
        }
    }
    (pairs.into_iter())
        .map(|((from, to), edges)| Crossing {
            from: from.to_owned(),
            to: to.to_owned(),
            edges,
            relationships: index.covering(from, to).map(|l| l.id.clone()).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::code::tests::{M, checkout, link, module, q, shop};
    use crate::model::EdgeKind;

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn changed_files_roll_up_to_containers_and_their_edges_cross_relationships() {
        let (_dir, mut doc) = checkout();
        // A container importing its own part crosses no boundary of the model.
        let inward = code::tests::edge("app/shop/cart.ts", "app/web/index.ts", EdgeKind::Static, 1);
        doc.edges.push(inward);
        let mut modules = shop().to_vec();
        link(&mut modules[0], "AppBank1", "Bank", "call");
        let changed = files(&[
            "app/billing/charge.ts",
            "app/shop/cart.ts",
            "app/web/page.ts",
            "docs/README.md",
        ]);
        let report = measure(M, (&modules, &[]), None, &changed, Ok(&doc));
        let touched: Vec<(&str, usize, usize)> = (report.modules.iter())
            .map(|t| (t.name.as_str(), t.files.len(), t.in_parts))
            .collect();
        assert_eq!(touched, [("App", 1, 2), ("Billing", 1, 0), ("Web", 1, 0)]);
        assert_eq!(report.outside, ["docs/README.md"]);
        let crossing = |from: &str, to: &str, edges, relationships: &[&str]| Crossing {
            from: q(from),
            to: q(to),
            edges,
            relationships: relationships.iter().map(|r| q(r)).collect(),
        };
        // Billing's charge calls Bank under App's line; Web's page reaches into Billing, undrawn,
        // and imports a Bank type. The cart's edge into the charge goes through `shared`, unbound.
        assert_eq!(
            report.crossings.unwrap(),
            [
                crossing("Billing", "Bank", 1, &["AppBank1"]),
                crossing("Web", "Bank", 1, &["AppBank1"]),
                crossing("Web", "Billing", 1, &[]),
            ]
        );
        assert!(report.model.is_none());
    }

    #[test]
    fn model_changes_compare_with_the_base_state() {
        let head = shop();
        let base = [module("App", None, Some("app")), module("Gone", None, None)];
        let skipped = Err(CrossingsSkipped::NotNeeded);
        let report = measure(M, (&head, &[]), Some((&base, &[])), &[], skipped);
        let model = report.model.unwrap();
        let ids = |entries: &[compare::Entry]| -> Vec<String> {
            entries.iter().map(|e| e.id.clone()).collect()
        };
        assert_eq!(
            ids(&model.modules.added),
            [q("Bank"), q("Billing"), q("Web")]
        );
        assert_eq!(ids(&model.modules.removed), [q("Gone")]);
        assert_eq!(report.crossings, Some(Vec::new()));
        assert!(report.modules.is_empty());
        let changed = files(&["app/web/page.ts"]);
        let report = measure(M, (&head, &[]), None, &changed, skipped);
        assert_eq!(report.crossings, None);
        assert_eq!(report.crossings_skipped, Some(CrossingsSkipped::NotNeeded));
    }
}
