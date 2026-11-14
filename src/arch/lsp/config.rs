//! Which language servers run: the committed list names them, and only the built-in allowlist
//! or the user's own config, outside the checkout, says what program runs with what arguments.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Deserializer};
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
    args: &'static [&'static str],
    languages: &'static [&'static str],
    extensions: &'static [&'static str],
}

/// The servers a repository may name without the user defining them. Elixir's server ships as a
/// `language_server.sh` script rather than a program on `PATH`, so it is left out; a user can
/// define it. On Windows a name resolves to a `.exe` only, so servers npm installs as `.cmd`
/// shims (the TypeScript and Pyright ones) run on Unix only, unless the user defines them.
const KNOWN_SERVERS: [Known; 6] = [
    Known {
        name: "typescript-language-server",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
    },
    Known {
        name: "basedpyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        extensions: &["py", "pyi"],
    },
    Known {
        name: "pyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        extensions: &["py", "pyi"],
    },
    Known {
        name: "rust-analyzer",
        args: &[],
        languages: &["rust"],
        extensions: &["rs"],
    },
    Known {
        name: "gopls",
        args: &[],
        languages: &["go"],
        extensions: &["go"],
    },
    Known {
        name: "jdtls",
        args: &[],
        languages: &["java"],
        extensions: &["java"],
    },
];

/// One language server to run, as the user's config defines it or as [`KNOWN_SERVERS`] holds it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    #[serde(default)]
    pub initialization_options: Option<Value>,
}

/// One entry of the repository's list: a server's name, and optionally other languages and
/// extensions. Anything else, a command above all, is refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    server: String,
    languages: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some_extensions")]
    extensions: Option<Vec<String>>,
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

/// The JSON array in the config file `path`, shown as `shown`; `None` when there is none. A file
/// that is not a regular file under [`CONFIG_MAX`] bytes, or does not parse, refuses.
fn read<T: for<'de> Deserialize<'de>>(path: &Path, shown: &str) -> Result<Option<Vec<T>>> {
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

/// The servers to run. Without `enabled` neither config is read, and the repository's list is
/// only noted by a `language-servers-not-enabled` skip when it exists. With it, a config that is
/// not a small regular JSON file, a user command that is a relative path, or an entry naming a
/// server neither allowlisted nor defined by the user, refuses `REQUEST_INVALID`.
pub fn configured(root: &Path, enabled: bool) -> Result<(Vec<Server>, Vec<Skip>)> {
    let path = root.join(CONFIG);
    if !enabled {
        let skips = (path.exists().then(|| Skip {
            subject: CONFIG.to_owned(),
            site: SkipSite::LanguageServer,
            reason: SkipReason::LanguageServersNotEnabled,
        }))
        .into_iter()
        .collect();
        return Ok((Vec::new(), skips));
    }
    let Some(entries) = read::<Entry>(&path, CONFIG)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let user_path = user_config();
    let shown = (user_path.as_ref()).map_or_else(
        || format!("$XDG_CONFIG_HOME/{USER_CONFIG}"),
        |p| p.display().to_string(),
    );
    let user = match &user_path {
        Some(path) => read::<Server>(path, &shown)?.unwrap_or_default(),
        None => Vec::new(),
    };
    validate_user_servers(&user, &shown)?;
    let servers = resolve(entries, &user)
        .map_err(|message| invalid(CONFIG, format!("{message}; define it in {shown}")))?;
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
pub(super) fn resolve(entries: Vec<Entry>, user: &[Server]) -> Result<Vec<Server>, String> {
    let owned = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
    (entries.into_iter())
        .map(|entry| {
            let known = KNOWN_SERVERS.iter().find(|k| k.name == entry.server);
            let mut server = match (user.iter().find(|s| s.name == entry.server), known) {
                (Some(defined), _) => defined.clone(),
                (None, Some(known)) => Server {
                    name: known.name.to_owned(),
                    command: known.name.to_owned(),
                    args: owned(known.args),
                    languages: owned(known.languages),
                    extensions: owned(known.extensions),
                    initialization_options: None,
                },
                (None, None) => {
                    let names: Vec<&str> = KNOWN_SERVERS.iter().map(|k| k.name).collect();
                    return Err(format!(
                        "the language server {:?} is not one of {}, nor defined by the user",
                        entry.server,
                        names.join(", ")
                    ));
                }
            };
            server.languages = entry.languages.unwrap_or(server.languages);
            server.extensions = entry.extensions.unwrap_or(server.extensions);
            Ok(server)
        })
        .collect()
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
            entries(json!([{"server": "typescript-language-server"}])),
            &[],
        );
        let tsls = &found.unwrap()[0];
        assert_eq!(
            (tsls.command.as_str(), &tsls.args[..]),
            ("typescript-language-server", &["--stdio".to_owned()][..])
        );
        assert!(tsls.covers("a.tsx") && tsls.covers("b.cjs") && !tsls.covers("c.py"));
        assert_eq!(tsls.initialization_options, None);

        // An entry may narrow the files; the user's definition of a name wins over the list.
        let mut own = server(&["ts"], &["typescript"]);
        own.name = "rust-analyzer".into();
        own.initialization_options = Some(json!({"a": 1}));
        let found = resolve(
            entries(
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

        let error = resolve(entries(json!([{"server": "evil"}])), &[]).unwrap_err();
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
        assert!(read::<Entry>(&path, "x").unwrap().is_none());
        fs::write(&path, r#"[{"server": "gopls"}]"#).unwrap();
        assert_eq!(read::<Entry>(&path, "x").unwrap().unwrap().len(), 1);
        fs::write(&path, vec![b' '; CONFIG_MAX as usize]).unwrap();
        let error = read::<Entry>(&path, "x").unwrap_err();
        assert!(
            error.to_string().contains("not a regular file under"),
            "{error}"
        );
        let error = read::<Entry>(dir.path(), "x").unwrap_err();
        assert!(
            error.to_string().contains("not a regular file under"),
            "{error}"
        );
    }
}
