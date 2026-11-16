//! Language-server evidence: references from one module's code to another's interface symbols,
//! and declarations that confirm or refute binding and anchor symbols. The servers run only when
//! the caller opts in, and run code from the checkout they open, so only trusted checkouts should
//! be checked with them. A server that fails is a warning and a skip; the code check then keeps
//! the import approximation.

mod client;
mod config;
mod framing;
mod wire;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::{Value, json};

use super::code::{self, Ownership, Skip, SkipReason, SkipSite};
use super::index::Index;
use super::infra::{self, Infra};
use super::segments;
use super::types::{EXTERNAL_KIND, Module};
use crate::model::Document;
use client::Session;
pub use config::{
    CONFIG, Change, DefinedBy, Edited, Effective, Enable, Entry, Listed, Listing, Server,
    UserConfig, add, configured, detected_servers, listing, remove,
};
use wire::{Range, Roots, Text, file_uri};

/// The most interface symbols asked about per module.
const SYMBOLS_MAX: usize = 64;
/// A session's budget when the tool timeout is off.
const SESSION_BUDGET_DEFAULT: Duration = Duration::from_secs(600);

/// What the language servers found; empty without them.
#[derive(Debug, Default)]
pub struct Facts {
    /// Declarations found, as (file, name).
    pub(super) declared: HashSet<(String, String)>,
    /// Names a file's non-empty `textDocument/documentSymbol` answer lacks, as (file, name),
    /// that are not `default` and not written in the file as a whole word.
    pub(super) unlisted: HashSet<(String, String)>,
    /// References to interface symbols.
    pub(super) references: Vec<Reference>,
    /// The modules whose every interface symbol a server found and was asked about.
    pub(super) queried: HashSet<String>,
    /// The modules with at least one interface symbol a server found.
    pub(super) located: HashSet<String>,
    /// Servers that failed or were not ready, or the config when language servers are not
    /// enabled.
    pub skips: Vec<Skip>,
    /// Each server that had something to answer: how it went and how long each step took.
    pub runs: Vec<Run>,
}

/// How one server's session went.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub server: String,
    pub outcome: Outcome,
    /// Why it failed or was not ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The steps it reached; `None` when it was not started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings_ms: Option<Timings>,
}

/// How a server's session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Answered,
    Failed,
    /// Not started: the project lacks a file the server needs.
    Unready,
}

/// Milliseconds each step of a session took, for the steps it reached.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Timings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialize: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<u64>,
}

/// Runs `step`, recording its milliseconds in `slot`.
fn timed<T>(slot: &mut Option<u64>, step: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let value = step();
    *slot = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    value
}

impl Facts {
    /// Whether `name` is declared in one of `files`: yes when a server found it there, no when
    /// `files` is one file whose symbol list lacks it (see [`Facts::unlisted`]) and no server
    /// found it declared anywhere, else unknown. An empty answer refutes nothing, since a server
    /// may not have indexed the file.
    pub(super) fn symbol(&self, files: &[&str], name: &str) -> Option<bool> {
        let declared = |f: &&str| self.declared.contains(&((*f).to_owned(), name.to_owned()));
        if files.iter().any(declared) {
            return Some(true);
        }
        let unlisted = |file: &str| self.unlisted.contains(&(file.to_owned(), name.to_owned()));
        match files {
            [file] if unlisted(file) && !self.declared.iter().any(|(_, n)| n == name) => {
                Some(false)
            }
            _ => None,
        }
    }
}

/// A use of an interface symbol, found by `textDocument/references`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reference {
    /// Where the use is, a repository path and a 1-based line.
    pub(super) from: String,
    pub(super) line: u64,
    /// The symbol's definition: its file and name.
    pub(super) file: String,
    pub(super) name: String,
}

/// A module's interface symbols, as (definition file, name), and the files it owns.
struct Interface {
    id: String,
    owned: Vec<String>,
    definitions: Vec<(String, String)>,
}

/// A binding or anchor symbol, with the scanned files it may be in.
struct Lookup<'a> {
    name: &'a str,
    files: Vec<&'a str>,
}

/// Asks each of `servers` about the modules of `model` in the checkout at `root` and its scan
/// `doc`: references to every interface symbol of the modules whose files it covers, and the
/// binding and anchor symbols. A server is started only when it has something to answer.
pub fn gather(
    root: &Path,
    servers: &[Server],
    model: &str,
    modules: &[Module],
    doc: &Document,
    infra: &Infra,
) -> Facts {
    let mut facts = Facts::default();
    if servers.is_empty() {
        return facts;
    }
    let index = Index::new(model, modules);
    let own = code::ownership(&index, root, Ok(doc), infra);
    let interfaces = interfaces(&own, doc);
    let lookups = lookups(&index, doc);
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let roots = Roots {
        canonical: &canonical,
        given: root,
    };
    let mut project_files = config::ProjectFiles::new();
    for server in servers {
        let (served, asks) = plan(server, &interfaces, &lookups);
        if asks.names.is_empty() {
            continue;
        }
        let run = |outcome, reason, timings_ms| Run {
            server: server.name.clone(),
            outcome,
            reason,
            timings_ms,
        };
        if let Some(reason) = config::unready(server, &canonical, &asks.files, &mut project_files) {
            eprintln!(
                "warning: language server {} is not ready: {reason}",
                server.name
            );
            facts.skips.push(Skip {
                subject: server.name.clone(),
                site: SkipSite::LanguageServer,
                reason: SkipReason::LanguageServerUnready,
            });
            facts.runs.push(run(Outcome::Unready, Some(reason), None));
            continue;
        }
        let mut timings = Timings::default();
        match ask(roots, server, &asks, &mut timings) {
            Ok(found) => {
                merge(&mut facts, &served, found);
                let step = |name: &str, ms: Option<u64>| format!("{name} {} ms", ms.unwrap_or(0));
                eprintln!(
                    "boundrail: language server {} answered: {}, {}, {}, {}",
                    server.name,
                    step("start", timings.start),
                    step("initialize", timings.initialize),
                    step("queries", timings.queries),
                    step("shutdown", timings.shutdown)
                );
                facts.runs.push(run(Outcome::Answered, None, Some(timings)));
            }
            Err(error) => {
                eprintln!("warning: language server {} failed: {error:#}", server.name);
                facts.skips.push(Skip {
                    subject: server.name.clone(),
                    site: SkipSite::LanguageServer,
                    reason: SkipReason::LanguageServerFailed,
                });
                let reason = Some(format!("{error:#}"));
                facts.runs.push(run(Outcome::Failed, reason, Some(timings)));
            }
        }
    }
    facts
}

/// The interfaces `server` covers every owned file of, and what to ask it: their definitions,
/// and each lookup it covers a file of. Of a lookup only the first covered file is opened, and
/// the symbol list is asked of a lookup that is that one file.
fn plan<'a>(
    server: &Server,
    interfaces: &'a [Interface],
    lookups: &'a [Lookup<'a>],
) -> (Vec<&'a Interface>, Asks<'a>) {
    let served: Vec<&Interface> = (interfaces.iter())
        .filter(|i| i.owned.iter().all(|f| server.covers(f)))
        .collect();
    let mut asks = Asks::default();
    for (file, name) in served.iter().flat_map(|i| &i.definitions) {
        asks.definitions.insert((file, name));
        asks.names.insert(name);
        asks.files.insert(file);
    }
    for lookup in lookups {
        let Some(&first) = lookup.files.iter().find(|f| server.covers(f)) else {
            continue;
        };
        asks.names.insert(lookup.name);
        asks.files.insert(first);
        if lookup.files.len() == 1 {
            asks.documents.entry(first).or_default().insert(lookup.name);
        }
    }
    (served, asks)
}

/// Adds what one server found to `facts`: its modules count as queried when it found every
/// interface symbol of theirs, and as located when it found any.
fn merge(facts: &mut Facts, served: &[&Interface], found: Found<'_>) {
    let located = |(f, n): &(String, String)| found.located.contains(&(f.as_str(), n.as_str()));
    for i in served {
        if i.definitions.iter().all(located) {
            facts.queried.insert(i.id.clone());
        }
        if i.definitions.iter().any(located) {
            facts.located.insert(i.id.clone());
        }
    }
    facts.declared.extend(found.declared);
    facts.unlisted.extend(found.unlisted);
    facts.references.extend(found.references);
}

/// The interfaces of the judged modules a server could answer for: those with symbol data, no
/// file a symbol limitation names, and at most [`SYMBOLS_MAX`] interface symbols.
fn interfaces(own: &Ownership<'_>, doc: &Document) -> Vec<Interface> {
    let Some(symbols) = &doc.symbols else {
        return Vec::new();
    };
    let owned = own.owned_files();
    let empty = BTreeSet::new();
    (own.status.iter())
        .filter(|(_, status)| status.is_ok())
        .filter_map(|(id, _)| {
            let files = owned.get(id.as_str()).unwrap_or(&empty);
            if files.iter().any(|f| own.limited.contains(f)) {
                return None;
            }
            let definitions: BTreeSet<(String, String)> = (own
                .interface_exports(&symbols.exports, id, files)
                .into_iter())
            .map(|e| (e.target.file.clone(), e.target.name.clone()))
            .collect();
            // ponytail: a module exporting more than SYMBOLS_MAX symbols keeps the import
            // approximation; ask in batches if real modules need more.
            (definitions.len() <= SYMBOLS_MAX).then(|| Interface {
                id: id.clone(),
                owned: files.iter().map(|f| (*f).to_owned()).collect(),
                definitions: definitions.into_iter().collect(),
            })
        })
        .collect()
}

/// Each binding and anchor symbol in this repository, with the scanned files it may be in.
fn lookups<'a>(index: &Index<'a>, doc: &'a Document) -> Vec<Lookup<'a>> {
    let bindings = (index.by_id.values())
        .filter(|m| m.kind != EXTERNAL_KIND)
        .filter_map(|m| m.binding.as_ref())
        .filter(|b| !infra::is_unit_binding(b))
        .filter_map(|b| Some((b.repo.as_ref(), b.path.as_str(), b.symbol.as_deref()?)));
    let anchors = (index.lines.iter())
        .filter_map(|l| l.anchor)
        .map(|a| (a.repo.as_ref(), a.path.as_str(), a.symbol.as_str()));
    (bindings.chain(anchors))
        .filter(|(repo, _, _)| repo.is_none())
        .map(|(_, path, name)| {
            let path = segments(path).join("/");
            let files = (doc.files.iter())
                .map(|f| f.path.as_str())
                .filter(|f| {
                    path.is_empty()
                        || *f == path
                        || f.strip_prefix(path.as_str())
                            .is_some_and(|r| r.starts_with('/'))
                })
                .collect();
            Lookup { name, files }
        })
        .collect()
}

/// What to ask one server.
#[derive(Default)]
struct Asks<'a> {
    /// Interface symbols to find references to, as (definition file, name).
    definitions: BTreeSet<(&'a str, &'a str)>,
    /// Names to look up with `workspace/symbol`.
    names: BTreeSet<&'a str>,
    /// Files to open first.
    files: BTreeSet<&'a str>,
    /// Opened files to ask for their symbol list, with the names looked up in each.
    documents: BTreeMap<&'a str, BTreeSet<&'a str>>,
}

/// What one server answered.
#[derive(Default)]
struct Found<'a> {
    declared: HashSet<(String, String)>,
    unlisted: HashSet<(String, String)>,
    /// The definitions whose declaration the server found.
    located: HashSet<(&'a str, &'a str)>,
    references: Vec<Reference>,
}

/// Runs one session of `server` in the checkout, within the tool timeout (else
/// [`SESSION_BUDGET_DEFAULT`]), printing the command line first and recording each step's time
/// in `timings`.
fn ask<'a>(
    roots: Roots<'_>,
    server: &Server,
    asks: &Asks<'a>,
    timings: &mut Timings,
) -> Result<Found<'a>> {
    let program = if server.command.contains(['/', '\\']) {
        PathBuf::from(&server.command)
    } else {
        crate::collect::resolve_program(&server.command, roots.canonical)?
    };
    let line = (std::iter::once(program.display().to_string()))
        .chain(server.args.iter().map(|a| quoted(a)))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "boundrail: starting language server {}: {line}",
        server.name
    );
    let budget = crate::collect::tool_timeout().unwrap_or(SESSION_BUDGET_DEFAULT);
    let mut session = timed(&mut timings.start, || {
        Session::spawn(&program, &server.args, roots.canonical, budget)
    })?;
    let options = server.initialization_options.as_ref();
    let found = timed(&mut timings.initialize, || {
        session.initialize(roots.canonical, options)
    })
    .and_then(|capabilities| {
        timed(&mut timings.queries, || {
            questions(&mut session, roots, server, asks, &capabilities)
        })
    });
    match found {
        Ok(found) => {
            timed(&mut timings.shutdown, || session.shutdown());
            Ok(found)
        }
        Err(error) => match session.stderr_tail() {
            tail if tail.is_empty() => Err(error),
            tail => Err(anyhow!("{error:#}; stderr: {tail}")),
        },
    }
}

/// `arg` as a shell would need it written: plain when it holds only safe characters.
fn quoted(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:@,+%".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Opens the files, looks every name up, lists the symbols of single-file lookups when the
/// server can, then asks for the references of each definition found.
fn questions<'a>(
    session: &mut Session,
    roots: Roots<'_>,
    server: &Server,
    asks: &Asks<'a>,
    capabilities: &Value,
) -> Result<Found<'a>> {
    let texts = open_files(session, roots.canonical, server, &asks.files)?;
    let mut found = Found::default();
    let at = find_declarations(session, roots, &asks.names, &mut found)?;
    let lists = &capabilities["documentSymbolProvider"];
    if !(lists.is_null() || *lists == Value::Bool(false)) {
        list_symbols(
            session,
            roots.canonical,
            &asks.documents,
            &texts,
            &mut found,
        )?;
    }
    for &(file, name) in &asks.definitions {
        let Some(&range) = at.get(&(file.to_owned(), name.to_owned())) else {
            continue;
        };
        found.located.insert((file, name));
        let text = texts.get(file);
        let references = find_references(session, roots, file, name, range, text)?;
        found.references.extend(references);
    }
    Ok(found)
}

/// Opens each of `files` that reads, returning their texts.
fn open_files<'a>(
    session: &Session,
    root: &Path,
    server: &Server,
    files: &BTreeSet<&'a str>,
) -> Result<HashMap<&'a str, Text>> {
    let mut texts = HashMap::new();
    for &file in files {
        let Ok(text) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        let document = json!({
            "uri": file_uri(&root.join(file)),
            "languageId": server.language_id(file),
            "version": 1,
            "text": text,
        });
        session.notify("textDocument/didOpen", json!({ "textDocument": document }))?;
        texts.insert(file, Text::new(text));
    }
    Ok(texts)
}

/// Looks each of `names` up with `workspace/symbol`, recording exact declarations in `found`.
/// Returns each declaration's range by (file, name).
fn find_declarations(
    session: &mut Session,
    roots: Roots<'_>,
    names: &BTreeSet<&str>,
    found: &mut Found<'_>,
) -> Result<HashMap<(String, String), Range>> {
    let mut at = HashMap::new();
    for &name in names {
        let result = session.request("workspace/symbol", json!({ "query": name }))?;
        for declaration in wire::declarations(&result, roots) {
            if declaration.name != name {
                continue;
            }
            let key = (declaration.file, declaration.name);
            at.entry(key.clone()).or_insert(declaration.range);
            found.declared.insert(key);
        }
    }
    Ok(at)
}

/// Asks for the symbol list of each of `documents`, recording the names declared at the top of
/// each, and, when the list is not empty, the names looked up that it lacks: those other than
/// `default` (a default export lists under its own name) that the file's text in `texts` does
/// not hold as a whole word.
fn list_symbols(
    session: &mut Session,
    root: &Path,
    documents: &BTreeMap<&str, BTreeSet<&str>>,
    texts: &HashMap<&str, Text>,
    found: &mut Found<'_>,
) -> Result<()> {
    for (&file, looked_up) in documents {
        let params = json!({ "textDocument": {"uri": file_uri(&root.join(file))} });
        let result = session.request("textDocument/documentSymbol", params)?;
        let names = wire::top_level_names(&result);
        if !names.is_empty()
            && let Some(text) = texts.get(file)
        {
            let lacks = (looked_up.iter())
                .filter(|&&n| n != "default" && !names.contains(&n) && !text.has_word(n))
                .map(|n| (file.to_owned(), (*n).to_owned()));
            found.unlisted.extend(lacks);
        }
        let declared = names.into_iter().map(|n| (file.to_owned(), n.to_owned()));
        found.declared.extend(declared);
    }
    Ok(())
}

/// The references to `name`, defined in `file` within `range`, asked at the name.
fn find_references(
    session: &mut Session,
    roots: Roots<'_>,
    file: &str,
    name: &str,
    range: Range,
    text: Option<&Text>,
) -> Result<Vec<Reference>> {
    let (line, character) =
        text.map_or((range.0, range.1), |t| wire::name_position(t, name, range));
    let params = json!({
        "textDocument": {"uri": file_uri(&roots.canonical.join(file))},
        "position": {"line": line, "character": character},
        "context": {"includeDeclaration": false},
    });
    let result = session.request("textDocument/references", params)?;
    let references = (wire::locations(&result, roots).into_iter()).map(|(from, line)| Reference {
        from,
        line,
        file: file.to_owned(),
        name: name.to_owned(),
    });
    Ok(references.collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_quote_what_a_shell_would_split() {
        assert_eq!(quoted("--stdio"), "--stdio");
        assert_eq!(quoted("a b"), "'a b'");
        assert_eq!(quoted("it's"), r"'it'\''s'");
        assert_eq!(quoted(""), "''");
    }
}
