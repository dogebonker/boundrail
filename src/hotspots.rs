//! Explainable architecture hotspots from the scanned commit's Git history.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::{CycleLevel, Document, Evidence};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspots {
    pub since: String,
    pub since_timestamp: i64,
    pub as_of: String,
    pub max_files: usize,
    pub authors_enabled: bool,
    pub shallow: bool,
    pub commits: usize,
    pub skipped: usize,
    pub limitations: Vec<String>,
    pub files: Vec<Hotspot>,
    pub validation: Validation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspot {
    pub path: String,
    pub module: String,
    pub commits: usize,
    pub lines_added: usize,
    pub lines_deleted: usize,
    pub authors: usize,
    pub bugfix_commits: usize,
    pub churn: f64,
    pub coupling: f64,
    pub cycle: f64,
    pub author_factor: f64,
    pub score: f64,
    pub cycle_size: usize,
    pub cut_candidate: bool,
    pub finding_indices: Vec<usize>,
}

/// Unique fix-message commits touching scanned files, rather than summed file touches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Validation {
    pub files: usize,
    pub top_files: usize,
    pub bugfix_commits: usize,
    pub top_bugfix_commits: usize,
    pub share: Option<f64>,
    /// Naive file-share baseline; use random_lift to account for multi-file commits.
    pub lift: Option<f64>,
    #[serde(default)]
    pub expected_random_share: Option<f64>,
    #[serde(default)]
    pub random_lift: Option<f64>,
}

pub(crate) struct Change {
    pub(crate) path: String,
    /// The path before a rename.
    pub(crate) old: Option<String>,
}

#[derive(Default)]
pub(crate) struct Commit {
    pub(crate) changes: Vec<Change>,
}

/// The `git log --numstat -z` header that `parse_log` reads.
pub(crate) const LOG_FORMAT: &str = "--format=%x00ARCHGRAPH%x00%H%x00%aE%x00%B%x00";

/// One NUL-delimited numstat pass; rename records carry separate old/new path tokens.
pub(crate) fn parse_log(text: &str) -> Result<Vec<Commit>> {
    let fields: Vec<_> = text.split('\0').collect();
    let mut commits = Vec::new();
    let mut at = 0;
    while at < fields.len() {
        if fields[at] == "ARCHGRAPH" {
            if at + 3 >= fields.len() {
                bail!("truncated Git hotspot commit header");
            }
            commits.push(Commit::default());
            at += 4;
        } else if fields[at].is_empty() || fields[at] == "\n" {
            at += 1;
        } else {
            let mut stat = fields[at].trim_start_matches('\n').splitn(3, '\t');
            stat.next().context("missing Git numstat additions")?;
            stat.next().context("missing Git numstat deletions")?;
            let path = stat.next().context("missing Git numstat path")?;
            let (old, path) = if path.is_empty() {
                if at + 2 >= fields.len() {
                    bail!("truncated Git hotspot rename");
                }
                at += 2;
                (Some(fields[at - 1].to_owned()), fields[at])
            } else {
                (None, path)
            };
            commits
                .last_mut()
                .context("Git numstat without a commit")?
                .changes
                .push(Change {
                    path: path.into(),
                    old,
                });
            at += 1;
        }
    }
    Ok(commits)
}

/// Renames each path to its newest name, then drops sweeps: commits touching more than
/// `max_files` files, which say nothing about which files belong together. Returns the kept
/// commits and the number of sweeps. Commits must be newest first.
pub(crate) fn follow_renames(mut commits: Vec<Commit>, max_files: usize) -> (Vec<Commit>, usize) {
    let mut renamed: HashMap<String, String> = HashMap::new();
    for change in commits.iter_mut().flat_map(|c| &mut c.changes) {
        if let Some(current) = renamed.get(&change.path) {
            change.path = current.clone();
        }
        if let Some(old) = change.old.take() {
            renamed.insert(old, change.path.clone());
        }
    }
    drop_sweeps(commits, max_files)
}

/// Drops commits touching more than `max_files` files; returns the rest and how many it dropped.
pub(crate) fn drop_sweeps(mut commits: Vec<Commit>, max_files: usize) -> (Vec<Commit>, usize) {
    let total = commits.len();
    commits.retain(|c| c.changes.len() <= max_files);
    let sweeps = total - commits.len();
    (commits, sweeps)
}

/// Refresh after findings are recomputed or re-ranked: indices refer to the current document.
pub fn attach_findings(report: &mut Hotspots, graph: &Document) {
    for file in &mut report.files {
        file.finding_indices = graph
            .findings
            .iter()
            .enumerate()
            .filter_map(|(i, finding)| {
                let attached = finding.subject == file.path
                    || match &finding.evidence {
                        Evidence::Cycle(c) => {
                            if c.level == CycleLevel::File {
                                c.members.contains(&file.path)
                            } else {
                                c.members.contains(&file.module)
                            }
                        }
                        Evidence::ForbiddenEdge { edge } => {
                            edge.from == file.path || edge.to == file.path
                        }
                        Evidence::DeepImport { from, to, .. } => {
                            from == &file.path || to == &file.path
                        }
                        Evidence::HiddenCoupling { files, .. } => files.contains(&file.path),
                        Evidence::ReexportCycle { members, .. } => members.contains(&file.path),
                        Evidence::Grabbag(e) => e.members.contains(&file.module),
                        Evidence::InternalsReach { edges, .. } => edges
                            .iter()
                            .any(|e| e.from == file.path || e.to == file.path),
                        Evidence::SinkSplit { components, .. } => {
                            components.iter().any(|c| c.files.contains(&file.path))
                        }
                        Evidence::Seam { clusters, .. } => {
                            clusters.iter().any(|c| c.files.contains(&file.path))
                        }
                        Evidence::UnstableDependency { from, to, .. } => {
                            file.module == *from || file.module == *to
                        }
                        Evidence::MisplacedFile { .. } | Evidence::SmallRepo { .. } => false,
                    };
                attached.then_some(i)
            })
            .collect();
    }
}

/// Maximum attached hotspot per finding, preserving the existing order when priorities tie.
pub fn finding_priorities(report: &Hotspots, count: usize) -> Vec<f64> {
    let mut scores = vec![0.0_f64; count];
    for file in &report.files {
        for &i in &file.finding_indices {
            if let Some(score) = scores.get_mut(i) {
                *score = score.max(file.score);
            }
        }
    }
    scores
}
