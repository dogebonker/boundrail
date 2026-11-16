//! Files that change together in git history.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use crate::{git, hotspots};

/// A commit touching more files is a sweep (formatting, renames, vendoring) and says nothing
/// about which files belong together.
pub const MAX_FILES: usize = 30;
/// How many of the most recent commits a history measure reads unless told otherwise.
pub const HISTORY_LIMIT: usize = 500;
/// Fewer shared commits than this is coincidence more often than coupling.
pub(crate) const MIN_COMMITS: usize = 3;

/// Whether an import joins the pair's files either way; `edges` holds (from, to) of the scan's
/// edges, so only direct imports count.
pub(crate) fn linked(edges: &HashSet<(&str, &str)>, [a, b]: &[String; 2]) -> bool {
    edges.contains(&(a.as_str(), b.as_str())) || edges.contains(&(b.as_str(), a.as_str()))
}

/// The paths each non-merge commit of `revs` changed, newest first, at most `limit` commits,
/// relative to `root` and limited to it; sweeps of more than [`MAX_FILES`] files are left out
/// and counted. With `follow`, each path is renamed to its newest name; otherwise a rename gives
/// its old and its new path.
pub(crate) fn history(
    root: &Path,
    revs: &[&str],
    limit: usize,
    follow: bool,
) -> Result<(Vec<Vec<String>>, usize)> {
    // `--relative` and the `.` pathspec limit the log to commits touching `root` and make their
    // paths relative to it; `-z` leaves paths unquoted, so they match the scanned names.
    let limit = format!("-n{limit}");
    let mut args = vec![
        "log",
        "--numstat",
        "-z",
        "-M",
        "--no-merges",
        "--no-ext-diff",
        "--no-textconv",
        "--relative",
        hotspots::LOG_FORMAT,
        &limit,
        "--end-of-options",
    ];
    args.extend(revs);
    args.extend(["--", "."]);
    let log = hotspots::parse_log(&String::from_utf8_lossy(&git::run(root, &args)?))?;
    let (commits, skipped) = if follow {
        hotspots::follow_renames(log, MAX_FILES)
    } else {
        hotspots::drop_sweeps(log, MAX_FILES)
    };
    let paths = (commits.into_iter())
        .map(|c| {
            (c.changes.into_iter())
                .flat_map(|change| change.old.into_iter().chain([change.path]))
                .collect()
        })
        .collect();
    Ok((paths, skipped))
}
