//! Local CODEOWNERS ownership matching for a document that carries its rules; no GitHub
//! permission checks.

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Codeowners {
    pub path: String,
    pub prefix: String,
    pub rules: Vec<OwnerRule>,
    pub warnings: Vec<OwnerWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerRule {
    pub pattern: String,
    pub owners: Vec<String>,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerWarning {
    pub line: usize,
    pub message: String,
}

fn relative(path: &str, dot: bool) -> Result<()> {
    if !crate::collect::is_relative_path(path, dot) {
        bail!("expected a repository-relative path without traversal: {path:?}");
    }
    Ok(())
}

fn validate_pattern(pattern: &str) -> Result<()> {
    if pattern.starts_with('!') || pattern.contains(['[', ']', '{', '}', '#']) {
        bail!(
            "unsupported CODEOWNERS pattern {pattern:?}: negation, ranges and braces are not supported"
        );
    }
    if pattern.contains("//") {
        bail!("empty path component in CODEOWNERS pattern");
    }
    relative(
        pattern
            .strip_prefix('/')
            .unwrap_or(pattern)
            .trim_end_matches('/'),
        false,
    )?;
    if pattern.contains(['\n', '\r', '\t']) {
        bail!("invalid whitespace in CODEOWNERS pattern");
    }
    Ok(())
}

fn validate_owner(owner: &str) -> Result<()> {
    let valid = if let Some(name) = owner.strip_prefix('@') {
        let parts: Vec<&str> = name.split('/').collect();
        parts.len() <= 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            })
    } else {
        owner.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
                && !owner
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == '/')
        })
    };
    if !valid {
        bail!("invalid CODEOWNERS owner {owner:?}");
    }
    Ok(())
}

pub struct Compiled {
    prefix: String,
    rules: Vec<(Vec<GlobMatcher>, Vec<String>)>,
}

/// Revalidates embedded data as well as parsed files before compiling matchers.
pub fn compile(data: &Codeowners) -> Result<Compiled> {
    relative(&data.path, false).context("CODEOWNERS location")?;
    relative(&data.prefix, true).context("CODEOWNERS prefix")?;
    let mut rules = Vec::new();
    for rule in &data.rules {
        let build = || -> Result<_> {
            validate_pattern(&rule.pattern)?;
            for owner in &rule.owners {
                validate_owner(owner)?;
            }
            let directory = rule.pattern.ends_with('/');
            let stripped = rule
                .pattern
                .strip_prefix('/')
                .unwrap_or(&rule.pattern)
                .trim_end_matches('/');
            let anywhere = !rule.pattern.starts_with('/') && !stripped.contains('/');
            let base = if anywhere {
                format!("**/{stripped}")
            } else {
                stripped.to_owned()
            };
            let mut patterns = if directory {
                Vec::new()
            } else {
                vec![base.clone()]
            };
            // A literal final segment can name a directory; docs/* stays direct.
            if directory
                || stripped
                    .rsplit('/')
                    .next()
                    .is_some_and(|last| !last.contains(['*', '?']))
            {
                patterns.push(format!("{base}/**"));
            }
            let matchers = patterns
                .into_iter()
                .map(|pattern| {
                    GlobBuilder::new(&pattern)
                        .literal_separator(true)
                        .backslash_escape(false)
                        .build()
                        .with_context(|| format!("glob {pattern:?}"))
                        .map(|g| g.compile_matcher())
                })
                .collect::<Result<Vec<_>>>()?;
            let mut owners = rule.owners.clone();
            owners.sort();
            owners.dedup();
            Ok((matchers, owners))
        };
        rules.push(build().with_context(|| format!("{}:{}", data.path, rule.line))?);
    }
    Ok(Compiled {
        prefix: data.prefix.clone(),
        rules,
    })
}

impl Compiled {
    /// Paths are scan-root-relative; malformed or escaping paths have no owners.
    pub fn owners(&self, path: &str) -> Vec<String> {
        if relative(path, false).is_err() {
            return Vec::new();
        }
        let path = if self.prefix == "." {
            path.to_owned()
        } else {
            format!("{}/{path}", self.prefix)
        };
        self.rules
            .iter()
            .rev()
            .find(|(patterns, _)| patterns.iter().any(|p| p.is_match(&path)))
            .map(|(_, owners)| owners.clone())
            .unwrap_or_default()
    }
}
