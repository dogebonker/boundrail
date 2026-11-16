//! `model footprint`: the modules a branch's changes touch. Changed paths map to modules by
//! binding path, the deepest binding winning as in the code check, and roll up to the modules
//! containing them. Import edges of the changed files show which relationships the change
//! crosses, and the vault's own changes compare the branch's model with its base's. Against a
//! proposal, the modules the branch changes are set beside the modules the proposal names.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::code::{self, PathOwners};
use super::compare::{self, Comparison};
use super::index::Index;
use super::infra::{self, Infra};
use super::types::{Collection, EXTERNAL_KIND, Module};
use super::{Code, Refusal, ids, vault};
use crate::collect::TempDir;
use crate::model::{Document, Edge};
use crate::{cochange, git};

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
    /// Whether more files changed than a sweep holds ([`cochange::MAX_FILES`]).
    pub large: bool,
}

/// A proposal a footprint sets its changes beside.
pub struct Proposed<'a> {
    /// Qualified ID.
    pub id: String,
    pub name: &'a str,
    /// The state the proposal was drawn from.
    pub predecessor: State<'a>,
    /// The state the proposal draws.
    pub drawn: State<'a>,
}

/// The modules a branch changes beside the modules a proposal names, by qualified ID.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AgainstProposal {
    pub id: String,
    pub name: String,
    pub named_and_changed: Vec<Subject>,
    pub changed_not_named: Vec<Subject>,
    pub named_not_changed: Vec<Subject>,
    /// The modules of `changed_not_named` whose `holds` is empty: what `--fail-outside` fails on.
    pub outside: Vec<String>,
}

/// The subject lists of an [`AgainstProposal`] by JSON key, with their headings, in the order the
/// reports show them.
pub const AGAINST_LISTS: [(&str, &str); 3] = [
    ("named_and_changed", "Named and changed"),
    ("changed_not_named", "Changed, not named"),
    ("named_not_changed", "Named, not changed"),
];

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Subject {
    pub module: String,
    pub name: String,
    /// The changed modules inside this one, when they hold every changed file it holds; empty
    /// when it holds a changed file itself, changes in the model, or did not change.
    pub holds: Vec<String>,
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
    /// Configured addresses and build dependencies from `from` to `to` written in a changed
    /// file.
    pub hops: usize,
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
/// hashes; renames count as a deletion and an addition. The working tree is compared through a
/// disposable copy of the index with Git's stat refresh on, so a file whose stat changed but
/// whose content did not is content-checked and left out, and the user's index is never written.
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
    let mut files = if head.is_some() {
        split(&git::run(root, &args)?)
    } else {
        let temp = TempDir::new("footprint-index")?;
        let index = temp.path().join("index");
        let actual = ["rev-parse", "--path-format=absolute", "--git-path", "index"];
        let actual = PathBuf::from(git::text(root, &actual)?.trim_end_matches(['\n', '\r']));
        if actual.is_file() {
            copy_index(&actual, &index)?;
        }
        let mut files = split(&git::run_with_index(root, &args, Some(&index))?);
        let untracked = ["ls-files", "--others", "--exclude-standard", "-z"];
        files.extend(split(&git::run(root, &untracked)?));
        files
    };
    files.retain(|f| !vault::in_vault(f));
    files.sort_unstable();
    files.dedup();
    Ok(files)
}

/// Copies the index at `from` to `to` with its modification time. Git content-checks an entry
/// whose file is not older than the index (racily clean), so a copy newer than the original
/// would pass files edited in the same second as unchanged.
fn copy_index(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).context("copying the Git index")?;
    let modified = fs::metadata(from)?.modified()?;
    fs::File::options()
        .write(true)
        .open(to)?
        .set_modified(modified)
        .context("dating the Git index copy")?;
    Ok(())
}

fn split(out: &[u8]) -> Vec<String> {
    (out.split(|b| *b == 0))
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// The modules and collections of one state.
pub type State<'a> = (&'a [Module], &'a [Collection]);

/// The footprint of `files` on the state `head` of model `model`, with the edges of `scan`, the
/// hops of `infra` and what `head` changes against `base`. Without changed files there are no
/// crossings to check; without a scan the crossings are the hops alone when a module of `head`
/// is bound to a unit.
pub fn measure(
    model: &str,
    head: State<'_>,
    base: Option<State<'_>>,
    files: &[String],
    scan: Result<&Document, CrossingsSkipped>,
    infra: &Infra,
) -> FootprintReport {
    let index = Index::new(model, head.0);
    let bound = code::bound_paths(model, head.0);
    let owners = PathOwners::new(bound.iter().map(|(i, p)| (i.as_str(), p.as_str())));
    let mut report = FootprintReport::default();
    (report.modules, report.outside) = touched(&index, &owners, files);
    report.large = files.len() > cochange::MAX_FILES;
    // Without a scan, only modules bound to units hold anything to cross.
    let units = (head.0.iter())
        .any(|m| m.kind != EXTERNAL_KIND && m.binding.as_ref().is_some_and(infra::is_unit_binding));
    match scan {
        _ if files.is_empty() => report.crossings = Some(Vec::new()),
        Ok(doc) => report.crossings = Some(crossings(&index, &owners, files, &doc.edges, infra)),
        Err(CrossingsSkipped::NotNeeded) if units => {
            report.crossings = Some(crossings(&index, &owners, files, &[], infra));
        }
        Err(skipped) => report.crossings_skipped = Some(skipped),
    }
    report.model = base.map(|base| compare::compare(model, base, head));
    report
}

/// The modules holding a changed file or containing one that does, and the changed files no
/// module holds.
fn touched(
    index: &Index<'_>,
    owners: &PathOwners<'_, '_>,
    files: &[String],
) -> (Vec<Touched>, Vec<String>) {
    let mut outside = Vec::new();
    let mut touched: BTreeMap<&str, Touched> = BTreeMap::new();
    for file in files {
        let Some(id) = owners.owner(file) else {
            outside.push(file.clone());
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
    (touched.into_values().collect(), outside)
}

/// The modules the branch changes, `files` on the state `head` of model `model`, beside those
/// `proposal` names. A changed file counts for its owner in the proposal or in the head's model,
/// the deeper binding winning, so a module the proposal adds or rebinds counts once its code
/// changes, and one it removes counts where the head still binds it. The branch's model changes,
/// `branch` against its base, count too.
pub fn against(
    model: &str,
    proposal: &Proposed<'_>,
    files: &[String],
    head: State<'_>,
    branch: Option<(State<'_>, &Comparison)>,
) -> AgainstProposal {
    let drawn = proposal.drawn;
    let kept: BTreeSet<&str> = drawn.0.iter().map(|m| m.id.as_str()).collect();
    let head_only = (head.0.iter()).filter(|m| !kept.contains(m.id.as_str()));
    let modules: Vec<Module> = drawn.0.iter().chain(head_only).cloned().collect();
    let index = Index::new(model, &modules);
    let (in_drawn, in_head) = (
        code::bound_paths(model, drawn.0),
        code::bound_paths(model, head.0),
    );
    let bound = in_drawn.iter().chain(&in_head);
    let owners = PathOwners::new(bound.map(|(i, p)| (i.as_str(), p.as_str())));
    let (touches, _) = touched(&index, &owners, files);
    // Each changed module with the changed parts holding all its changes.
    let mut changed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in &touches {
        let holds = if t.files.is_empty() {
            (touches.iter())
                .filter(|o| !o.files.is_empty())
                .filter(|o| index.is_below(&o.module, &t.module))
                .map(|o| o.module.clone())
                .collect()
        } else {
            Vec::new()
        };
        changed.insert(t.module.clone(), holds);
    }
    if let Some((base, comparison)) = branch {
        for id in subjects(model, comparison, [base, head]) {
            changed.insert(id, Vec::new());
        }
    }
    let comparison = compare::compare(model, proposal.predecessor, drawn);
    let named = subjects(model, &comparison, [proposal.predecessor, drawn]);
    let before = Index::new(model, proposal.predecessor.0);
    let subject = |id: &str, holds: Vec<String>| Subject {
        module: id.to_owned(),
        name: match index.by_id.get(id) {
            Some(m) => m.name.clone(),
            None => before.name(id).to_owned(),
        },
        holds,
    };
    let mut report = AgainstProposal {
        id: proposal.id.clone(),
        name: proposal.name.to_owned(),
        named_and_changed: Vec::new(),
        changed_not_named: Vec::new(),
        named_not_changed: (named.iter())
            .filter(|id| !changed.contains_key(*id))
            .map(|id| subject(id, Vec::new()))
            .collect(),
        outside: Vec::new(),
    };
    for (id, holds) in changed {
        if named.contains(&id) {
            report.named_and_changed.push(subject(&id, holds));
        } else {
            if holds.is_empty() {
                report.outside.push(id.clone());
            }
            report.changed_not_named.push(subject(&id, holds));
        }
    }
    report
}

/// The modules `comparison` names: those it adds, removes or changes, and both ends of each
/// relationship it adds, removes or changes, as either of `states` draws it.
fn subjects(model: &str, comparison: &Comparison, states: [State<'_>; 2]) -> BTreeSet<String> {
    let all = |c: &compare::Changes| -> Vec<String> {
        let entries = c.added.iter().chain(&c.removed).chain(&c.changed);
        entries.map(|e| e.id.clone()).collect()
    };
    let mut found: BTreeSet<String> = all(&comparison.modules).into_iter().collect();
    let lines: BTreeSet<String> = all(&comparison.relationships).into_iter().collect();
    for module in states.iter().flat_map(|(modules, _)| modules.iter()) {
        for r in &module.outgoing {
            if lines.contains(&ids::qualify(model, &r.id)) {
                found.insert(ids::qualify(model, &module.id));
                found.insert(r.to.clone());
            }
        }
    }
    found
}

/// The module pairs, neither containing the other, that an import edge with a changed file at
/// either end joins, or a hop written in a changed file, with the relationships drawn that way
/// between them or modules containing them. Units map to the modules bound to them, else to the
/// owner of the directory a compose service builds from.
fn crossings(
    index: &Index<'_>,
    owners: &PathOwners<'_, '_>,
    files: &[String],
    edges: &[Edge],
    infra: &Infra,
) -> Vec<Crossing> {
    let changed: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let apart = |from, to| !index.is_below(from, to) && !index.is_below(to, from);
    let touching = (edges.iter())
        .filter(|e| changed.contains(e.from.as_str()) || changed.contains(code::written(e)));
    let mut pairs: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for (from, to, _) in code::owner_pairs(touching, |f| owners.owner(f)) {
        if apart(from, to) {
            pairs.entry((from, to)).or_default().0 += 1;
        }
    }
    let bound = (index.by_id.iter())
        .filter(|(_, m)| m.kind != EXTERNAL_KIND)
        .filter_map(|(id, m)| Some((id.as_str(), m.binding.as_ref()?)));
    for (from, to, hop) in infra.join(bound, |dir| owners.owner(dir)).hops {
        if changed.contains(hop.file.as_str()) && apart(from, to) {
            pairs.entry((from, to)).or_default().1 += 1;
        }
    }
    (pairs.into_iter())
        .map(|((from, to), (edges, hops))| Crossing {
            from: from.to_owned(),
            to: to.to_owned(),
            edges,
            hops,
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
        let report = measure(
            M,
            (&modules, &[]),
            None,
            &changed,
            Ok(&doc),
            &Infra::default(),
        );
        let touched: Vec<(&str, usize, usize)> = (report.modules.iter())
            .map(|t| (t.name.as_str(), t.files.len(), t.in_parts))
            .collect();
        assert_eq!(touched, [("App", 1, 2), ("Billing", 1, 0), ("Web", 1, 0)]);
        assert_eq!(report.outside, ["docs/README.md"]);
        let crossing = |from: &str, to: &str, edges, relationships: &[&str]| Crossing {
            from: q(from),
            to: q(to),
            edges,
            hops: 0,
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
    fn hops_written_in_changed_files_cross_without_a_scan() {
        use crate::arch::infra::{End, Hop, Unit, UnitKind};
        use crate::arch::types::Evidence;
        let mut queue = module("Queue", None, Some("docker-compose.yml"));
        queue.binding.as_mut().unwrap().symbol = Some("rabbit".to_owned());
        let modules = [module("Api", None, Some("api")), queue];
        let unit = |name: &str, build: Option<&str>| Unit {
            kind: UnitKind::ComposeService,
            name: name.to_owned(),
            file: "docker-compose.yml".to_owned(),
            build: build.map(str::to_owned),
        };
        let hop = Hop {
            from: End::Unit(0),
            to: End::Unit(1),
            evidence: Evidence::ConfiguredPath,
            file: "docker-compose.yml".to_owned(),
            line: 4,
            variable: "QUEUE_URL".to_owned(),
            address: "rabbit".to_owned(),
        };
        let units = vec![unit("api", Some("api")), unit("rabbit", None)];
        let infra = Infra::new(units, vec![hop], Vec::new());
        let skipped = Err(CrossingsSkipped::NotNeeded);
        let changed = files(&["docker-compose.yml"]);
        let report = measure(M, (&modules, &[]), None, &changed, skipped, &infra);
        // The compose file belongs to no module: a unit binding holds no path.
        assert_eq!(report.outside, ["docker-compose.yml"]);
        let crossing = Crossing {
            from: q("Api"),
            to: q("Queue"),
            edges: 0,
            hops: 1,
            relationships: Vec::new(),
        };
        assert_eq!(report.crossings, Some(vec![crossing]));
        let changed = files(&["api/index.ts"]);
        let report = measure(M, (&modules, &[]), None, &changed, skipped, &infra);
        assert_eq!(report.crossings, Some(Vec::new()));

        // Without a module bound to a unit there is nothing to cross without a scan.
        let code_only = [module("Api", None, Some("api"))];
        let report = measure(M, (&code_only, &[]), None, &changed, skipped, &infra);
        assert_eq!(report.crossings, None);
        assert_eq!(report.crossings_skipped, Some(CrossingsSkipped::NotNeeded));
    }

    #[test]
    fn an_index_copy_keeps_the_original_modification_time() {
        let temp = TempDir::new("index-copy-test").unwrap();
        let (from, to) = (temp.path().join("index"), temp.path().join("copy"));
        fs::write(&from, b"DIRC").unwrap();
        let old =
            std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1_000_000_000_123_456_789);
        (fs::File::options().write(true).open(&from).unwrap())
            .set_modified(old)
            .unwrap();
        copy_index(&from, &to).unwrap();
        let modified = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(modified(&to), modified(&from));
        assert_eq!(fs::read(&to).unwrap(), b"DIRC");
    }

    #[test]
    fn model_changes_compare_with_the_base_state() {
        let head = shop();
        let base = [module("App", None, Some("app")), module("Gone", None, None)];
        let skipped = Err(CrossingsSkipped::NotNeeded);
        let report = measure(
            M,
            (&head, &[]),
            Some((&base, &[])),
            &[],
            skipped,
            &Infra::default(),
        );
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
        let report = measure(M, (&head, &[]), None, &changed, skipped, &Infra::default());
        assert_eq!(report.crossings, None);
        assert_eq!(report.crossings_skipped, Some(CrossingsSkipped::NotNeeded));
    }
}
