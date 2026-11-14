//! `model locality`: how well module boundaries hold changes, from Git history. Each commit's
//! changed paths map to modules by binding path, the deepest binding winning as in the code
//! check; a commit is local to a module when it touches that module alone. Renames are not
//! followed: a renamed file counts at its old and its new path, so moving a file between modules
//! touches both.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::code::{self, PathOwners};
use super::index::Index;
use super::types::Module;
use super::{Code, Refusal, vault};
use crate::{cochange, git};

/// The commits read: the changed paths of each, newest first.
#[derive(Debug, Serialize)]
pub struct History {
    /// The revision range read, `HEAD` or `<since>..HEAD` with `<since>` resolved.
    pub range: String,
    /// The most commits read.
    pub limit: usize,
    /// Non-merge commits read, sweeps left out.
    pub commits: usize,
    /// Commits left out for touching more than `max_files` files.
    pub sweeps: usize,
    pub max_files: usize,
    /// Whether the checkout is shallow: its oldest commits, whose diffs show their whole tree as
    /// added, are left out, and older history is missing.
    pub shallow: bool,
    #[serde(skip)]
    pub changes: Vec<Vec<String>>,
}

#[derive(Debug, Default, Serialize)]
pub struct LocalityReport {
    /// Modules some commit touched, by qualified ID.
    pub modules: Vec<ModuleLocality>,
    /// Pairs that changed together in at least `cochange::MIN_COMMITS` commits with no
    /// relationship drawn between them either way, most commits first.
    pub hidden_couplings: Vec<Coupling>,
    /// Commits that touched no bound module.
    pub unbound_commits: usize,
}

#[derive(Debug, Serialize)]
pub struct ModuleLocality {
    pub module: String,
    pub name: String,
    /// Commits touching the module.
    pub commits: usize,
    /// Of those, the commits touching no other module.
    pub local: usize,
    /// `local / commits`, rounded to `1 / RATIO_SCALE`.
    pub local_share: f64,
    /// The modules it changed with, most commits first.
    pub partners: Vec<Partner>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Partner {
    pub module: String,
    pub commits: usize,
    /// Commits touching both over commits touching either, rounded to `1 / RATIO_SCALE`.
    pub jaccard: f64,
    /// Whether a relationship is drawn between the two, or one contains the other.
    pub related: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Coupling {
    pub modules: [String; 2],
    pub commits: usize,
    pub jaccard: f64,
}

/// `local / commits` and the Jaccard ratio are rounded to `1 / RATIO_SCALE`.
const RATIO_SCALE: f64 = 1000.0;

/// The changed paths of at most `limit` commits on HEAD in the checkout `root`, after `since`
/// when given. Refuses `REQUEST_INVALID` when `since` names no commit.
pub fn read(root: &Path, since: Option<&str>, limit: usize) -> Result<History> {
    let range = match since {
        Some(since) => {
            let commit = git::resolve(root, since).map_err(|error| {
                let message = format!("--since {since} names no commit: {error:#}");
                Refusal::new(Code::RequestInvalid, message)
            })?;
            format!("{commit}..HEAD")
        }
        None => "HEAD".to_owned(),
    };
    let shallow = git::is_shallow(root)?;
    let boundaries = if shallow {
        boundaries(root)?
    } else {
        Vec::new()
    };
    let mut revs = vec![range.as_str()];
    revs.extend(boundaries.iter().map(String::as_str));
    let (changes, sweeps) = cochange::history(root, &revs, limit, false)?;
    Ok(History {
        range,
        limit,
        commits: changes.len(),
        sweeps,
        max_files: cochange::MAX_FILES,
        shallow,
        changes,
    })
}

/// `^<commit>` for each boundary commit of the shallow checkout `root`.
fn boundaries(root: &Path) -> Result<Vec<String>> {
    let path = git::text(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "shallow",
        ],
    )?;
    let listed = std::fs::read_to_string(path.trim_end_matches(['\n', '\r'])).unwrap_or_default();
    Ok((listed.lines())
        .filter(|commit| git::is_full_hash(commit))
        .map(|commit| format!("^{commit}"))
        .collect())
}

/// Commits per module and per pair of modules, by qualified ID.
#[derive(Default)]
struct Counts<'b> {
    /// Per module: commits touching it, and those touching no other module.
    touched: BTreeMap<&'b str, (usize, usize)>,
    /// Per pair, in ID order: commits touching both.
    together: BTreeMap<(&'b str, &'b str), usize>,
    unbound: usize,
}

fn count<'b>(owners: &PathOwners<'b, '_>, changes: &[Vec<String>]) -> Counts<'b> {
    let mut counts = Counts::default();
    for paths in changes {
        let hit: BTreeSet<&str> = (paths.iter())
            .filter(|p| !vault::in_vault(p))
            .filter_map(|p| owners.owner(p))
            .collect();
        let hit: Vec<&str> = hit.into_iter().collect();
        counts.unbound += usize::from(hit.is_empty());
        for (i, &module) in hit.iter().enumerate() {
            let (commits, local) = counts.touched.entry(module).or_default();
            *commits += 1;
            *local += usize::from(hit.len() == 1);
            for &other in &hit[i + 1..] {
                *counts.together.entry((module, other)).or_default() += 1;
            }
        }
    }
    counts
}

/// The co-change locality of the modules of model `model` over `changes`, each one commit's
/// changed paths. The vault's own files belong to no module.
pub fn measure(model: &str, modules: &[Module], changes: &[Vec<String>]) -> LocalityReport {
    let index = Index::new(model, modules);
    let bound = code::bound_paths(model, modules);
    let owners = PathOwners::new(bound.iter().map(|(i, p)| (i.as_str(), p.as_str())));
    let counts = count(&owners, changes);
    let pairs = pairs(&index, &counts);
    let mut partners: BTreeMap<&str, Vec<Partner>> = BTreeMap::new();
    for (coupling, related) in &pairs {
        let [a, b] = &coupling.modules;
        for (this, other) in [(a, b), (b, a)] {
            partners.entry(this.as_str()).or_default().push(Partner {
                module: other.clone(),
                commits: coupling.commits,
                jaccard: coupling.jaccard,
                related: *related,
            });
        }
    }
    let modules = (counts.touched.iter())
        .map(|(&module, &(commits, local))| ModuleLocality {
            module: module.to_owned(),
            name: index.name(module).to_owned(),
            commits,
            local,
            local_share: ratio(local, commits),
            partners: partners.remove(module).unwrap_or_default(),
        })
        .collect();
    let hidden_couplings = (pairs.into_iter())
        .filter(|(c, related)| c.commits >= cochange::MIN_COMMITS && !related)
        .map(|(c, _)| c)
        .collect();
    LocalityReport {
        modules,
        hidden_couplings,
        unbound_commits: counts.unbound,
    }
}

/// Every pair of modules some commit touched together, most commits first, with whether a
/// relationship is drawn between them either way or one contains the other.
fn pairs(index: &Index<'_>, counts: &Counts<'_>) -> Vec<(Coupling, bool)> {
    let commits = |m: &str| counts.touched.get(m).map_or(0, |t| t.0);
    let mut pairs: Vec<(Coupling, bool)> = (counts.together.iter())
        .map(|(&(a, b), &n)| {
            let related = index.is_below(a, b)
                || index.is_below(b, a)
                || index
                    .covering(a, b)
                    .chain(index.covering(b, a))
                    .next()
                    .is_some();
            let coupling = Coupling {
                modules: [a.to_owned(), b.to_owned()],
                commits: n,
                jaccard: ratio(n, commits(a) + commits(b) - n),
            };
            (coupling, related)
        })
        .collect();
    pairs.sort_by(|(x, _), (y, _)| {
        y.commits
            .cmp(&x.commits)
            .then_with(|| x.modules.cmp(&y.modules))
    });
    pairs
}

/// `part / whole` rounded to `1 / RATIO_SCALE`, 0 for an empty whole.
fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64 / whole as f64 * RATIO_SCALE).round() / RATIO_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::code::tests::{M, link, module, q, shop};

    fn commit(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    /// Billing changes alone twice; Web and Bank change together three times with nothing drawn
    /// between them; Web and Billing once, with a relationship; one commit touches only docs.
    #[test]
    fn locality_counts_local_commits_partners_and_hidden_couplings() {
        let mut modules = shop().to_vec();
        link(&mut modules[1], "WebBill1", "Billing", "call");
        let together = commit(&["app/web/page.ts", "bank/index.ts"]);
        let changes = vec![
            commit(&["app/billing/charge.ts"]),
            commit(&["app/billing/index.ts", "app/billing/charge.ts"]),
            together.clone(),
            together.clone(),
            together,
            commit(&["app/web/index.ts", "app/billing/index.ts"]),
            commit(&["docs/README.md"]),
        ];
        let report = measure(M, &modules, &changes);
        assert_eq!(report.unbound_commits, 1);
        let summary: Vec<(String, usize, usize, f64)> = (report.modules.iter())
            .map(|m| (m.name.clone(), m.commits, m.local, m.local_share))
            .collect();
        assert_eq!(
            summary,
            [
                ("Bank".to_owned(), 3, 0, 0.0),
                ("Billing".to_owned(), 3, 2, 0.667),
                ("Web".to_owned(), 4, 0, 0.0),
            ]
        );
        let web = &report.modules[2];
        assert_eq!(
            web.partners,
            [
                Partner {
                    module: q("Bank"),
                    commits: 3,
                    jaccard: 0.75,
                    related: false
                },
                Partner {
                    module: q("Billing"),
                    commits: 1,
                    jaccard: 0.167,
                    related: true
                },
            ]
        );
        assert_eq!(
            report.hidden_couplings,
            [Coupling {
                modules: [q("Bank"), q("Web")],
                commits: 3,
                jaccard: 0.75
            }]
        );
    }

    #[test]
    fn a_container_and_its_part_or_a_drawn_pair_is_never_hidden() {
        let mut modules = shop().to_vec();
        link(&mut modules[3], "BankWeb1", "Web", "event");
        let part = commit(&["app/shop/cart.ts", "app/web/page.ts"]);
        let drawn = commit(&["app/web/page.ts", "bank/index.ts"]);
        let changes = [
            part.clone(),
            part.clone(),
            part,
            drawn.clone(),
            drawn.clone(),
            drawn,
        ];
        let report = measure(M, &modules, &changes);
        assert_eq!(report.hidden_couplings, []);
        // The vault's own files belong to no module, even under a binding of the whole checkout.
        let mut everything = modules.clone();
        everything.push(module("Root", None, Some(".")));
        let vault_only = [commit(&[".boundrail/model/model.json"])];
        assert_eq!(measure(M, &everything, &vault_only).unbound_commits, 1);
        let related: Vec<bool> = report.modules[2]
            .partners
            .iter()
            .map(|p| p.related)
            .collect();
        assert_eq!(
            (report.modules[2].name.as_str(), related),
            ("Web", vec![true, true])
        );
    }
}
