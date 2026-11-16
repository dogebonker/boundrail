//! Which language servers run: the committed list names them, and only the built-in allowlist
//! or the user's own config, outside the checkout, says what program runs with what arguments.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::super::code::{Skip, SkipReason, SkipSite};
use super::super::{Code, Refusal};
use super::wire::extension;

/// The repository's server list, relative to the checkout root.
pub const CONFIG: &str = ".boundrail/language-servers.json";
/// The user's server definitions, below the user's config folder.
const USER_CONFIG: &str = "boundrail/language-servers.json";
/// The largest config file read.
const CONFIG_MAX: u64 = 1 << 20;

/// A server a repository may name: run as its own name looked up on `PATH`, with fixed
/// arguments, covering these languages and extensions unless the entry names others.
struct Known {
    name: &'static str,
    /// The language `model language-server` names it by; the first server of a language is its
    /// default.
    language: &'static str,
    args: &'static [&'static str],
    languages: &'static [&'static str],
    extensions: &'static [&'static str],
    /// Project files that show the language; `*.ext` stands for any file of that extension.
    /// `model setup` names a language's default server when one sits at the checkout root.
    project_files: &'static [&'static str],
    /// Whether one of `project_files` must sit beside or above a file the server opens, up to
    /// the checkout root, else it is not started.
    required: bool,
}

impl Known {
    fn server(&self) -> Server {
        let owned = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        Server {
            name: self.name.to_owned(),
            command: self.name.to_owned(),
            args: owned(self.args),
            languages: owned(self.languages),
            extensions: owned(self.extensions),
            initialization_options: None,
        }
    }
}

/// The servers a repository may name without the user defining them. Elixir's server ships as a
/// `language_server.sh` script rather than a program on `PATH`, so it is left out; a user can
/// define it. On Windows a name resolves to a `.exe` only, so servers npm installs as `.cmd`
/// shims (the TypeScript and Pyright ones) run on Unix only, unless the user defines them.
static KNOWN_SERVERS: [Known; 9] = [
    Known {
        name: "typescript-language-server",
        language: "typescript",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        project_files: &["package.json", "tsconfig.json"],
        required: false,
    },
    Known {
        name: "pyright-langserver",
        language: "python",
        args: &["--stdio"],
        languages: &["python"],
        extensions: &["py", "pyi"],
        project_files: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        required: false,
    },
    Known {
        name: "basedpyright-langserver",
        language: "python",
        args: &["--stdio"],
        languages: &["python"],
        extensions: &["py", "pyi"],
        project_files: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        required: false,
    },
    Known {
        name: "rust-analyzer",
        language: "rust",
        args: &[],
        languages: &["rust"],
        extensions: &["rs"],
        project_files: &["Cargo.toml", "rust-project.json"],
        required: true,
    },
    Known {
        name: "gopls",
        language: "go",
        args: &[],
        languages: &["go"],
        extensions: &["go"],
        project_files: &["go.mod", "go.work"],
        required: true,
    },
    Known {
        name: "jdtls",
        language: "java",
        args: &[],
        languages: &["java"],
        extensions: &["java"],
        project_files: &["pom.xml", "build.gradle", "build.gradle.kts"],
        required: true,
    },
    Known {
        name: "clangd",
        language: "c",
        args: &[],
        languages: &["c", "cpp", "objective-c", "objective-cpp"],
        extensions: &["c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "m", "mm"],
        project_files: &[
            "compile_commands.json",
            "build/compile_commands.json",
            "compile_flags.txt",
        ],
        required: true,
    },
    Known {
        name: "csharp-ls",
        language: "csharp",
        args: &[],
        languages: &["csharp"],
        extensions: &["cs"],
        project_files: &["*.sln", "*.slnx", "*.csproj"],
        required: true,
    },
    Known {
        name: "sourcekit-lsp",
        language: "swift",
        args: &[],
        languages: &["swift"],
        extensions: &["swift"],
        project_files: &["Package.swift"],
        required: true,
    },
];

/// Whether a run uses language servers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Enable {
    /// Never, as `--no-language-servers` and `model view` ask.
    #[default]
    Off,
    /// Always, as `--language-servers` asks.
    On,
    /// When the user's config, outside the checkout, trusts the checkout.
    ByUserConfig,
}

/// One language server to run, as the user's config defines it or as [`KNOWN_SERVERS`] holds it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    /// A program name looked up on `PATH`, or, in the user's config only, an absolute path.
    pub command: String,
    pub args: Vec<String>,
    /// LSP language identifiers; a file opens as the standard one of its extension when listed,
    /// else as the first.
    pub languages: Vec<String>,
    /// The file extensions the server covers, without the dot.
    #[serde(deserialize_with = "extensions")]
    pub extensions: Vec<String>,
    /// Sent with `initialize`; only the user's config sets it, since options can name programs
    /// a server runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<Value>,
}

/// The user's config: the checkouts whose runs use language servers without
/// `--language-servers`, and the servers it defines. A bare array of servers reads as the
/// servers, trusting no checkout.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Absolute folders: a checkout at or below one, once links resolve, runs the servers
    /// without the flag.
    #[serde(default)]
    pub trusted: Vec<PathBuf>,
    #[serde(default)]
    pub servers: Vec<Server>,
}

impl UserConfig {
    /// Whether the checkout at `root` lies at or below a trusted folder, once links resolve.
    fn trusts(&self, root: &Path) -> bool {
        let Ok(root) = fs::canonicalize(root) else {
            return false;
        };
        (self.trusted.iter())
            .filter_map(|folder| fs::canonicalize(folder).ok())
            .any(|folder| root.starts_with(folder))
    }
}

/// One entry of the repository's list: a server's name, and optionally other languages and
/// extensions. Anything else, a command above all, is refused.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "some_extensions",
        skip_serializing_if = "Option::is_none"
    )]
    pub extensions: Option<Vec<String>>,
}

/// Extensions as written, with or without the dot, stored without it.
fn extensions<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<String>, D::Error> {
    let written = Vec::<String>::deserialize(input)?;
    Ok((written.into_iter())
        .map(|e| e.trim_start_matches('.').to_owned())
        .collect())
}

fn some_extensions<'de, D: Deserializer<'de>>(input: D) -> Result<Option<Vec<String>>, D::Error> {
    extensions(input).map(Some)
}

impl Server {
    pub(super) fn covers(&self, file: &str) -> bool {
        let extension = extension(file);
        !extension.is_empty() && self.extensions.iter().any(|e| e == extension)
    }

    pub(super) fn language_id(&self, file: &str) -> String {
        let standard = match extension(file) {
            "ts" | "mts" | "cts" => "typescript",
            "tsx" => "typescriptreact",
            "js" | "mjs" | "cjs" => "javascript",
            "jsx" => "javascriptreact",
            "py" | "pyi" => "python",
            "rs" => "rust",
            "go" => "go",
            "java" => "java",
            "c" | "h" => "c",
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
            "m" => "objective-c",
            "mm" => "objective-cpp",
            "cs" => "csharp",
            "swift" => "swift",
            "ex" | "exs" => "elixir",
            other => other,
        };
        (self.languages.iter())
            .find(|l| *l == standard)
            .or(self.languages.first())
            .map_or_else(|| standard.to_owned(), Clone::clone)
    }
}

/// The user's config file: below `$XDG_CONFIG_HOME` when it is absolute, else `~/.config`, or
/// `%APPDATA%` on Windows.
fn user_config() -> Option<PathBuf> {
    let var = |name: &str| {
        let value = std::env::var_os(name).map(PathBuf::from)?;
        value.is_absolute().then_some(value)
    };
    let dir = if cfg!(windows) {
        var("APPDATA")?
    } else {
        var("XDG_CONFIG_HOME").or_else(|| Some(var("HOME")?.join(".config")))?
    };
    Some(dir.join(USER_CONFIG))
}

/// A `REQUEST_INVALID` refusal about the config `file`.
fn invalid(file: &str, message: impl std::fmt::Display) -> anyhow::Error {
    Refusal::new(Code::RequestInvalid, format!("{file}: {message}")).into()
}

/// The JSON in the config file `path`, shown as `shown`; `None` when there is none. A file that
/// is not a regular file under [`CONFIG_MAX`] bytes, or does not parse, refuses.
fn read<T: for<'de> Deserialize<'de>>(path: &Path, shown: &str) -> Result<Option<T>> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() || metadata.len() >= CONFIG_MAX {
        return Err(invalid(
            shown,
            format!("not a regular file under {CONFIG_MAX} bytes"),
        ));
    }
    let mut text = Vec::new();
    let file = fs::File::open(path).map_err(|e| invalid(shown, e))?;
    (file.take(CONFIG_MAX).read_to_end(&mut text)).map_err(|e| invalid(shown, e))?;
    serde_json::from_slice(&text)
        .map(Some)
        .map_err(|e| invalid(shown, e))
}

/// The user's config file, and how messages name it.
struct UserFile {
    path: Option<PathBuf>,
    shown: String,
}

impl UserFile {
    fn locate() -> Self {
        let path = user_config();
        let shown = (path.as_ref()).map_or_else(
            || format!("$XDG_CONFIG_HOME/{USER_CONFIG}"),
            |p| p.display().to_string(),
        );
        Self { path, shown }
    }

    /// The config, empty when there is none. Refuses one inside the checkout at `root`, where
    /// the repository could commit it, one that does not read, and a trusted folder that is not
    /// absolute.
    fn read(&self, root: &Path) -> Result<UserConfig> {
        let Some(path) = &self.path else {
            return Ok(UserConfig::default());
        };
        let at = path.ancestors().find_map(|a| fs::canonicalize(a).ok());
        if let (Ok(root), Some(at)) = (fs::canonicalize(root), at)
            && at.starts_with(root)
        {
            let message = "the user's config lies inside the checkout, where the repository \
                           could commit it; move the user's config folder outside the checkout";
            return Err(invalid(&self.shown, message));
        }
        let Some(value) = read::<Value>(path, &self.shown)? else {
            return Ok(UserConfig::default());
        };
        let config: UserConfig = match value {
            Value::Array(_) => serde_json::from_value(value).map(|servers| UserConfig {
                trusted: Vec::new(),
                servers,
            }),
            _ => serde_json::from_value(value),
        }
        .map_err(|e| invalid(&self.shown, e))?;
        if let Some(folder) = config.trusted.iter().find(|f| !f.is_absolute()) {
            let message = format!("the trusted folder {folder:?} is not an absolute path");
            return Err(invalid(&self.shown, message));
        }
        Ok(config)
    }
}

/// The servers to run. Off, neither config is read, and the repository's list is only noted by a
/// `language-servers-not-enabled` skip when it exists; by the user's config, the user's config is
/// read when the list exists, and the skip stands unless it trusts the checkout at `root`. A user's
/// config that does not read, or lies inside the checkout, refuses `REQUEST_INVALID` when servers
/// are on, and by the user's config is a warning that leaves them off. Once servers are on, a
/// repository's list that does not read, a user command that is empty or a relative path, or an
/// entry naming a server neither allowlisted nor defined by the user refuses `REQUEST_INVALID`.
pub fn configured(root: &Path, enable: Enable) -> Result<(Vec<Server>, Vec<Skip>)> {
    let path = root.join(CONFIG);
    if !path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let not_enabled = || {
        let skip = Skip {
            subject: CONFIG.to_owned(),
            site: SkipSite::LanguageServer,
            reason: SkipReason::LanguageServersNotEnabled,
        };
        Ok((Vec::new(), vec![skip]))
    };
    if enable == Enable::Off {
        return not_enabled();
    }
    let user_file = UserFile::locate();
    let user = match user_file.read(root) {
        Ok(user) => user,
        Err(error) if enable == Enable::On => return Err(error),
        Err(error) => {
            eprintln!("warning: language servers stay off: {error:#}");
            return not_enabled();
        }
    };
    if enable == Enable::ByUserConfig && !user.trusts(root) {
        return not_enabled();
    }
    validate_user_servers(&user.servers, &user_file.shown)?;
    let entries = read::<Vec<Entry>>(&path, CONFIG)?.unwrap_or_default();
    let servers = resolve(&entries, &user.servers).map_err(|message| {
        invalid(
            CONFIG,
            format!("{message}; define it in {}", user_file.shown),
        )
    })?;
    Ok((servers, Vec::new()))
}

/// Refuses a user command that is empty or a relative path, which would resolve against the
/// checkout the server runs in.
fn validate_user_servers(user: &[Server], shown: &str) -> Result<()> {
    for server in user {
        let bare = !server.command.contains(['/', '\\']);
        if server.command.is_empty() || !(bare || Path::new(&server.command).is_absolute()) {
            let message = format!(
                "language server {} names the command {:?}; name a program on PATH or an \
                 absolute path",
                server.name, server.command
            );
            return Err(invalid(shown, message));
        }
    }
    Ok(())
}

/// The servers the repository's `entries` name: the user's definition of that name, else the
/// allowlisted one, with the entry's languages and extensions when it gives them. An unknown
/// name is an error naming the allowlist.
pub(super) fn resolve(entries: &[Entry], user: &[Server]) -> Result<Vec<Server>, String> {
    (entries.iter())
        .map(|entry| {
            let known = || {
                (KNOWN_SERVERS.iter())
                    .find(|k| k.name == entry.server)
                    .map(Known::server)
            };
            let defined = user.iter().find(|s| s.name == entry.server).cloned();
            let Some(mut server) = defined.or_else(known) else {
                let names: Vec<&str> = KNOWN_SERVERS.iter().map(|k| k.name).collect();
                return Err(format!(
                    "the language server {:?} is not one of {}, nor defined by the user",
                    entry.server,
                    names.join(", ")
                ));
            };
            if let Some(languages) = &entry.languages {
                server.languages.clone_from(languages);
            }
            if let Some(extensions) = &entry.extensions {
                server.extensions.clone_from(extensions);
            }
            Ok(server)
        })
        .collect()
}

/// Whether each folder below the checkout root holds each project file, as far as asked.
pub(super) type ProjectFiles<'a> = HashMap<(&'a Path, &'static str), bool>;

/// Why the allowlisted `server` is not ready to answer about `files`, repository paths below
/// `root`: none of its project files sits beside or above any of them, up to `root`. `None` when
/// it is ready or needs none. Each folder is looked in once for each name, remembered in `found`
/// across servers.
pub(super) fn unready<'a>(
    server: &Server,
    root: &Path,
    files: &BTreeSet<&'a str>,
    found: &mut ProjectFiles<'a>,
) -> Option<String> {
    let known = KNOWN_SERVERS.iter().find(|k| k.name == server.name)?;
    if !known.required {
        return None;
    }
    let folders: BTreeSet<&Path> = (files.iter())
        .flat_map(|file| Path::new(*file).ancestors().skip(1))
        .collect();
    let mut holds = |dir: &'a Path, name: &'static str| {
        *(found.entry((dir, name))).or_insert_with(|| has_project_file(&root.join(dir), name))
    };
    let ready =
        (folders.into_iter()).any(|dir| known.project_files.iter().any(|name| holds(dir, name)));
    if ready {
        return None;
    }
    let first = files.first()?;
    Some(format!(
        "no {} beside or above {first} in the checkout",
        known.project_files.join(" or ")
    ))
}

/// The default servers of the languages whose project files sit at the checkout `root`.
pub fn detected_servers(root: &Path) -> Vec<&'static str> {
    let mut languages = BTreeSet::new();
    (KNOWN_SERVERS.iter())
        .filter(|k| languages.insert(k.language))
        .filter(|k| k.project_files.iter().any(|f| has_project_file(root, f)))
        .map(|k| k.name)
        .collect()
}

/// Whether the folder `dir` holds the project file `name`, or for `*.ext` any file of that
/// extension.
fn has_project_file(dir: &Path, name: &str) -> bool {
    match name.strip_prefix("*.") {
        Some(wanted) => fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .any(|path| path.extension().is_some_and(|e| e == wanted) && path.is_file())
        }),
        None => dir.join(name).is_file(),
    }
}

/// Where a server a run would start is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DefinedBy {
    Allowlist,
    User,
}

/// A server a run would start, for one language of the files it covers.
#[derive(Debug, Serialize)]
pub struct Effective {
    pub language: String,
    pub server: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
    pub defined_by: DefinedBy,
}

/// One config file: where it is, what it holds, and why it does not read.
#[derive(Debug, Serialize)]
pub struct Listed<T> {
    pub file: Option<PathBuf>,
    pub content: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// Both config files and the servers a run would start.
#[derive(Debug, Serialize)]
pub struct Listing {
    pub repository: Listed<Vec<Entry>>,
    pub user: Listed<UserConfig>,
    /// By language; empty when a file does not read or the list names an unknown server.
    pub effective: Vec<Effective>,
    /// Why a run would refuse although both files read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// The value of `result`, else the default and the error's message.
fn or_problem<T: Default>(result: Result<Option<T>>) -> (T, Option<String>) {
    match result {
        Ok(value) => (value.unwrap_or_default(), None),
        Err(error) => (T::default(), Some(format!("{error:#}"))),
    }
}

/// The repository's list at `root`, the user's config, and the servers a run with language
/// servers on would start, each for every language of the files it covers.
pub fn listing(root: &Path) -> Listing {
    let user_file = UserFile::locate();
    let (entries, repository_problem) = or_problem(read::<Vec<Entry>>(&root.join(CONFIG), CONFIG));
    let user = user_file.read(root).and_then(|config| {
        validate_user_servers(&config.servers, &user_file.shown)?;
        Ok(Some(config))
    });
    let (user, user_problem) = or_problem(user);
    let (mut effective, mut problem) = (Vec::new(), None);
    if repository_problem.is_none() && user_problem.is_none() {
        match resolve(&entries, &user.servers) {
            Ok(servers) => effective = effective_servers(servers, &user.servers),
            Err(message) => problem = Some(message),
        }
    }
    Listing {
        repository: Listed {
            file: Some(root.join(CONFIG)),
            content: entries,
            problem: repository_problem,
        },
        user: Listed {
            file: user_file.path,
            content: user,
            problem: user_problem,
        },
        effective,
        problem,
    }
}

/// `servers` once per language of the files each covers, sorted by language.
fn effective_servers(servers: Vec<Server>, user: &[Server]) -> Vec<Effective> {
    let mut effective = Vec::new();
    for server in servers {
        let defined_by = if user.iter().any(|s| s.name == server.name) {
            DefinedBy::User
        } else {
            DefinedBy::Allowlist
        };
        let languages: BTreeSet<String> = (server.extensions.iter())
            .map(|e| {
                let id = server.language_id(&format!("file.{e}"));
                (KNOWN_SERVERS.iter())
                    .find(|k| k.languages.contains(&id.as_str()))
                    .map_or(id, |k| k.language.to_owned())
            })
            .collect();
        for language in languages {
            effective.push(Effective {
                language,
                server: server.name.clone(),
                command: server.command.clone(),
                args: server.args.clone(),
                extensions: server.extensions.clone(),
                defined_by,
            });
        }
    }
    effective.sort_by(|a, b| a.language.cmp(&b.language));
    effective
}

/// What an edit did to a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Written,
    Deleted,
    Unchanged,
}

/// The config file an edit read, and what it did.
#[derive(Debug, Serialize)]
pub struct Edited {
    pub file: PathBuf,
    pub change: Change,
}

/// The names of `servers`.
fn names(servers: &[&Known]) -> Vec<&'static str> {
    servers.iter().map(|k| k.name).collect()
}

/// The allowlisted servers of `language`, its default first; any other language refuses
/// `REQUEST_INVALID`.
fn language_servers(language: &str) -> Result<Vec<&'static Known>> {
    let found: Vec<&Known> = (KNOWN_SERVERS.iter())
        .filter(|k| k.language == language)
        .collect();
    if found.is_empty() {
        let mut languages: Vec<&str> = KNOWN_SERVERS.iter().map(|k| k.language).collect();
        languages.sort_unstable();
        languages.dedup();
        let message = format!(
            "{language:?} is not a language with an allowlisted server; name one of {}",
            languages.join(", ")
        );
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    }
    Ok(found)
}

/// Names `language`'s server, replacing the entries that name one of its allowlisted servers.
/// In the repository's list at `root` it is an allowlisted name: `command` when it is one, else
/// the language's default. With `user`, the user's config defines the default's name as
/// `command` with `args`, else as the allowlisted program and arguments; a `command` naming an
/// allowlisted server of the language, without `args`, keeps that server's arguments. A command
/// that is no
/// allowlisted name of the language, or arguments, for the repository's list, an unknown
/// language, or a file that does not read refuses `REQUEST_INVALID`.
pub fn add(
    root: &Path,
    language: &str,
    command: Option<&str>,
    args: &[String],
    user: bool,
) -> Result<Edited> {
    let known = language_servers(language)?;
    let names = names(&known);
    let default = known[0];
    if user {
        let mut server = default.server();
        if let Some(command) = command {
            let named = known.iter().find(|k| k.name == command);
            server.command = command.to_owned();
            server.args = (named.filter(|_| args.is_empty()))
                .map_or_else(|| args.to_vec(), |k| k.server().args);
        }
        return edit_user(root, |config, shown| {
            validate_user_servers(std::slice::from_ref(&server), shown)?;
            config.servers.retain(|s| !names.contains(&s.name.as_str()));
            config.servers.push(server);
            Ok(())
        });
    }
    let name = match command {
        None if args.is_empty() => default.name,
        Some(command) if args.is_empty() && names.contains(&command) => command,
        _ => {
            let message = format!(
                "the repository's list names {language}'s server only as one of {}, run with \
                 fixed arguments; a command or arguments go in the user's config: pass --user",
                names.join(", ")
            );
            return Err(Refusal::new(Code::RequestInvalid, message).into());
        }
    };
    edit_repository(root, |entries| {
        entries.retain(|e| !names.contains(&e.server.as_str()));
        entries.push(Entry {
            server: name.to_owned(),
            languages: None,
            extensions: None,
        });
    })
}

/// Removes the entries naming an allowlisted server of `language` from the repository's list at
/// `root`, or with `user` from the user's config. The list is deleted when left empty.
pub fn remove(root: &Path, language: &str, user: bool) -> Result<Edited> {
    let names = names(&language_servers(language)?);
    if user {
        return edit_user(root, |config, _| {
            config.servers.retain(|s| !names.contains(&s.name.as_str()));
            Ok(())
        });
    }
    edit_repository(root, |entries| {
        entries.retain(|e| !names.contains(&e.server.as_str()));
    })
}

/// Applies `change` to the repository's list at `root`, refusing a symbolic link on the way.
fn edit_repository(root: &Path, change: impl FnOnce(&mut Vec<Entry>)) -> Result<Edited> {
    let path = crate::files::safe_path(root, CONFIG)?;
    let mut entries = read::<Vec<Entry>>(&path, CONFIG)?.unwrap_or_default();
    let before = entries.clone();
    change(&mut entries);
    let text = (!entries.is_empty())
        .then(|| pretty(&entries))
        .transpose()?;
    store(path, entries != before, text)
}

/// Applies `change` to the user's config, which must lie outside the checkout at `root`. It is
/// written as a bare array of servers, which binaries older than trusted folders read, unless it
/// trusts a folder.
fn edit_user(
    root: &Path,
    change: impl FnOnce(&mut UserConfig, &str) -> Result<()>,
) -> Result<Edited> {
    let file = UserFile::locate();
    let mut config = file.read(root)?;
    let Some(path) = file.path else {
        let message = "no folder for the user's config: set XDG_CONFIG_HOME or HOME";
        return Err(Refusal::new(Code::RequestInvalid, message).into());
    };
    let before = config.clone();
    change(&mut config, &file.shown)?;
    let text = if config.trusted.is_empty() {
        pretty(&config.servers)?
    } else {
        pretty(&config)?
    };
    store(path, config != before, Some(text))
}

/// Pretty JSON with a trailing newline.
fn pretty(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

/// Writes `text` to `path` atomically, creating its folder, or deletes the file for `None`, when
/// `changed`.
fn store(path: PathBuf, changed: bool, text: Option<String>) -> Result<Edited> {
    let change = match text {
        _ if !changed => Change::Unchanged,
        Some(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            crate::files::write_atomic(&path, text.as_bytes())?;
            Change::Written
        }
        None => {
            fs::remove_file(&path)?;
            Change::Deleted
        }
    };
    Ok(Edited { file: path, change })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::collect::TempDir;
    use serde_json::json;

    pub(in crate::arch::lsp) fn server(extensions: &[&str], languages: &[&str]) -> Server {
        let owned = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        Server {
            name: "ts".into(),
            command: "tsls".into(),
            args: Vec::new(),
            languages: owned(languages),
            extensions: owned(extensions),
            initialization_options: None,
        }
    }

    fn entries(json: Value) -> Vec<Entry> {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn entries_name_allowlisted_servers_with_fixed_programs_or_the_users_own() {
        let found = resolve(
            &entries(json!([{"server": "typescript-language-server"}])),
            &[],
        );
        let tsls = &found.unwrap()[0];
        assert_eq!(
            (tsls.command.as_str(), &tsls.args[..]),
            ("typescript-language-server", &["--stdio".to_owned()][..])
        );
        assert!(tsls.covers("a.tsx") && tsls.covers("b.cjs") && !tsls.covers("c.py"));
        assert_eq!(tsls.initialization_options, None);

        let mut own = server(&["ts"], &["typescript"]);
        own.name = "rust-analyzer".into();
        own.initialization_options = Some(json!({"a": 1}));
        let found = resolve(
            &entries(
                json!([{"server": "rust-analyzer", "extensions": [".rs"]}, {"server": "gopls"}]),
            ),
            &[own.clone()],
        )
        .unwrap();
        assert_eq!(found[0].command, "tsls");
        assert_eq!(found[0].extensions, ["rs"]);
        assert_eq!(found[0].initialization_options, own.initialization_options);
        assert_eq!(
            (found[1].command.as_str(), found[1].args.len()),
            ("gopls", 0)
        );

        let error = resolve(&entries(json!([{"server": "evil"}])), &[]).unwrap_err();
        assert!(
            error.contains("\"evil\" is not one of typescript-language-server, "),
            "{error}"
        );
        for refused in [
            json!([{"server": "gopls", "command": "/tmp/x"}]),
            json!([{"server": "gopls", "args": ["-rpc.trace"]}]),
            json!([{"server": "gopls", "initialization_options": {}}]),
            json!([{"name": "gopls"}]),
        ] {
            assert!(
                serde_json::from_value::<Vec<Entry>>(refused.clone()).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn c_family_csharp_and_swift_servers_are_allowlisted_with_fixed_arguments() {
        let found = resolve(
            &entries(json!([
                {"server": "clangd"}, {"server": "csharp-ls"}, {"server": "sourcekit-lsp"}
            ])),
            &[],
        )
        .unwrap();
        let commands: Vec<(&str, usize)> = (found.iter())
            .map(|s| (s.command.as_str(), s.args.len()))
            .collect();
        assert_eq!(
            commands,
            [("clangd", 0), ("csharp-ls", 0), ("sourcekit-lsp", 0)]
        );
        let [clangd, csharp, swift] = &found[..] else {
            panic!("{found:?}");
        };
        assert!(clangd.covers("a.cpp") && clangd.covers("b.h") && clangd.covers("c.mm"));
        assert_eq!(clangd.language_id("a.hpp"), "cpp");
        assert_eq!(clangd.language_id("a.m"), "objective-c");
        assert!(csharp.covers("Program.cs") && !csharp.covers("a.swift"));
        assert_eq!(swift.language_id("App.swift"), "swift");
    }

    #[test]
    fn a_server_is_unready_without_a_project_file_beside_or_above_its_files() {
        let dir = TempDir::new("lsp-unready").unwrap();
        let root = dir.path();
        let owned = |name: &str| Server {
            name: name.to_owned(),
            ..server(&[], &[])
        };
        let unready = |name: &str, files: &BTreeSet<&str>| {
            unready(&owned(name), root, files, &mut ProjectFiles::new())
        };
        let files = BTreeSet::from(["src/app/a.c", "src/b.c"]);
        let reason = unready("clangd", &files).unwrap();
        assert_eq!(
            reason,
            "no compile_commands.json or build/compile_commands.json or compile_flags.txt beside \
             or above src/app/a.c in the checkout"
        );
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/compile_commands.json"), "[]").unwrap();
        assert_eq!(unready("clangd", &files), None);

        let files = BTreeSet::from(["src/Shop/Cart.cs"]);
        assert!(unready("csharp-ls", &files).is_some());
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/Shop.csproj"), "").unwrap();
        assert_eq!(unready("csharp-ls", &files), None);
        assert_eq!(unready("gopls_custom", &files), None);
        assert_eq!(unready("typescript-language-server", &files), None);
    }

    #[test]
    fn servers_cover_files_by_extension_and_open_them_with_a_language() {
        let ts: Server = serde_json::from_value(json!({
            "name": "ts", "command": "tsls", "args": [],
            "languages": ["typescript", "javascript"], "extensions": [".ts", "js"],
        }))
        .unwrap();
        assert_eq!(ts.extensions, ["ts", "js"]);
        assert!(ts.covers("src/a.ts") && ts.covers("b.js") && !ts.covers("src/a.tsx"));
        assert!(!ts.covers("Makefile") && !ts.covers("src.ts/x"));
        assert_eq!(ts.language_id("a.js"), "javascript");
        assert_eq!(ts.language_id("a.mts"), "typescript");
        assert_eq!(server(&["tsx"], &["tsx"]).language_id("a.tsx"), "tsx");
    }

    #[test]
    fn configs_must_be_small_regular_files() {
        let dir = TempDir::new("lsp-config").unwrap();
        let path = dir.path().join("servers.json");
        assert!(read::<Vec<Entry>>(&path, "x").unwrap().is_none());
        fs::write(&path, r#"[{"server": "gopls"}]"#).unwrap();
        assert_eq!(read::<Vec<Entry>>(&path, "x").unwrap().unwrap().len(), 1);
        fs::write(&path, vec![b' '; CONFIG_MAX as usize]).unwrap();
        let error = read::<Vec<Entry>>(&path, "x").unwrap_err();
        assert!(
            error.to_string().contains("not a regular file under"),
            "{error}"
        );
        let error = read::<Vec<Entry>>(dir.path(), "x").unwrap_err();
        assert!(
            error.to_string().contains("not a regular file under"),
            "{error}"
        );
    }
}
