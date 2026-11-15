//! `boundrail model view`: the live browser view. A small HTTP/1.1 server on the loopback
//! interface answers the page's requests with the same JSON the model commands print, and tells
//! open pages over Server-Sent Events when the vault changes. It reads only.

use super::*;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use boundrail::arch::proposal::CURRENT;
use boundrail::arch::vault;
use boundrail::arch::view::{self as page, Request};
use boundrail::arch::{Code, Refusal};
use clap::ValueEnum as _;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};

use super::model_cli::{self, RenderSpec, RenderView};

/// How long the watcher waits for the watched files to go quiet before it tells the pages.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// How often an idle event stream sends a comment, so a closed page ends its thread.
const KEEPALIVE: Duration = Duration::from_secs(15);
/// How long a write may wait on a page that does not read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once, besides event streams; more are refused with 503.
const MAX_CONN: usize = 64;
/// Event streams open at once; more are refused with 503.
const MAX_STREAMS: usize = 16;
/// The pause after a failed accept, so a persistent error does not spin the loop.
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

#[derive(Args)]
pub(super) struct ViewArgs {
    /// Port on 127.0.0.1; 0 picks a free one
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Print the URL without opening a browser
    #[arg(long)]
    no_open: bool,
}

struct Server {
    root: PathBuf,
    token: String,
    /// The code the opener's URL carries instead of the token.
    launch: page::Launch,
    port: u16,
    connections: AtomicUsize,
    streams: AtomicUsize,
    /// The latest change; event streams wait on `changed`.
    news: Mutex<News>,
    changed: Condvar,
}

/// What the watcher last saw change.
#[derive(Default)]
struct News {
    /// How many changes there have been.
    generation: u64,
    /// The current state's revision after it, or `None` when it does not load.
    revision: Option<String>,
    /// What changed: `model` (the current state's files), `proposals`, `state` (claims and the
    /// edit journal) or `history` (checkpoints).
    parts: BTreeSet<&'static str>,
}

/// Serves until the process is stopped. The URL, with its token, is the first line on stdout.
/// The browser opener gets a one-time launch URL instead, so the token stays out of its
/// arguments.
pub(super) fn run(root: &Path, args: &ViewArgs) -> Result<()> {
    // The tool timeout belongs to the calling thread; connections take it along.
    let timeout = collect::tool_timeout();
    let vault = vault::locate(root)?;
    let listener = TcpListener::bind(("127.0.0.1", args.port))
        .with_context(|| format!("binding 127.0.0.1:{}", args.port))?;
    let port = listener.local_addr()?.port();
    let server = Arc::new(Server {
        root: root.to_path_buf(),
        token: boundrail::token()?,
        launch: page::Launch::new()?,
        port,
        connections: AtomicUsize::new(0),
        streams: AtomicUsize::new(0),
        news: Mutex::default(),
        changed: Condvar::new(),
    });
    watch(&server, &vault)?;
    let url = format!("http://127.0.0.1:{port}/?t={}", server.token);
    println!("{url}");
    std::io::stdout().flush()?;
    eprintln!("Serving a read-only view of the model; stop with Ctrl-C");
    if !args.no_open
        && let Some(code) = server.launch.code()
    {
        open(&format!("http://127.0.0.1:{port}/launch?c={code}"));
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                thread::sleep(ACCEPT_PAUSE);
                continue;
            }
        };
        if !claim(&server.connections, MAX_CONN) {
            refuse_busy(stream);
            continue;
        }
        let worker = Arc::clone(&server);
        let spawned = thread::Builder::new()
            .name("view-connection".into())
            .spawn(move || {
                collect::request_timeout(timeout);
                let connection = Release(&worker.connections);
                if let Some(stream) = worker.serve(stream) {
                    // An event stream counts against its own cap, claimed in `serve`.
                    drop(connection);
                    let _stream = Release(&worker.streams);
                    worker.events(stream);
                }
            });
        if let Err(error) = spawned {
            server.connections.fetch_sub(1, Ordering::SeqCst);
            eprintln!("warning: could not start a connection thread: {error}");
        }
    }
    Ok(())
}

/// Takes one of `max` places counted by `counter`, or returns false when all are taken.
fn claim(counter: &AtomicUsize, max: usize) -> bool {
    if counter.fetch_add(1, Ordering::SeqCst) < max {
        return true;
    }
    counter.fetch_sub(1, Ordering::SeqCst);
    false
}

/// Gives a place taken with [`claim`] back when dropped.
struct Release<'a>(&'a AtomicUsize);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Answers 503 without waiting on the client, then closes.
fn refuse_busy(mut stream: TcpStream) {
    let _ = stream.set_nonblocking(true);
    let _ = stream.write_all(&text(503, "Too many connections"));
}

/// Opens `url` in the default browser through the platform opener, as one argument.
fn open(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        // `start` takes its first quoted argument as a window title.
        command.args(["/c", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    let opened = (command.arg(url))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(error) = opened {
        eprintln!("warning: could not open a browser ({error}); open the URL above");
    }
}

/// The paths the view follows, canonical, since watchers report canonical paths.
struct Watched {
    vault: PathBuf,
    proposals: PathBuf,
    /// `<git dir>/boundrail`: claims and the edit journal of this worktree.
    state: Option<PathBuf>,
    /// `<common git dir>/refs/boundrail`: the checkpoint history every worktree shares.
    history: Option<PathBuf>,
    /// `<common git dir>/packed-refs`, which can hold the history ref too.
    packed: Option<PathBuf>,
}

impl Watched {
    fn find(vault: &Path) -> Self {
        let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let vault = canonical(vault);
        let git = boundrail::git::git_dir(&vault).ok().map(|g| canonical(&g));
        let state = (git.as_ref().zip(boundrail::arch::state_dir(&vault).ok()))
            .and_then(|(git, state)| Some(git.join(state.file_name()?)));
        // Relative to the folder Git ran in, the vault.
        let common = boundrail::git::text(&vault, &["rev-parse", "--git-common-dir"])
            .ok()
            .map(|dir| canonical(&vault.join(dir.trim_end_matches(['\n', '\r']))));
        Watched {
            proposals: vault.join("proposals"),
            vault,
            state,
            history: common.as_ref().map(|c| c.join("refs").join("boundrail")),
            packed: common.map(|c| c.join("packed-refs")),
        }
    }

    /// The part of the model a changed path belongs to, or `None` for a path the view ignores.
    fn part(&self, path: &Path) -> Option<&'static str> {
        let under = |dir: &Option<PathBuf>| dir.as_ref().is_some_and(|d| path.starts_with(d));
        if path.starts_with(&self.proposals) {
            Some("proposals")
        } else if path.starts_with(&self.vault) {
            Some("model")
        } else if under(&self.state) {
            Some("state")
        } else if under(&self.history) || self.packed.as_deref() == Some(path) {
            Some("history")
        } else {
            None
        }
    }

    /// The parts of the model an event touches; every part for an error, which may have hidden
    /// any change, and none for reads.
    fn parts(&self, event: &notify::Result<notify::Event>) -> BTreeSet<&'static str> {
        match event {
            Err(_) => ["model", "proposals", "state", "history"].into(),
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => BTreeSet::new(),
            Ok(event) => event.paths.iter().filter_map(|p| self.part(p)).collect(),
        }
    }

    /// Starts watching `dir` recursively once it exists. Its parent is watched shallowly from
    /// the start, so its creation is seen.
    fn follow(watcher: &mut RecommendedWatcher, dir: &Option<PathBuf>, followed: &mut bool) {
        if let Some(dir) = dir
            && !*followed
            && dir.is_dir()
        {
            *followed = watcher.watch(dir, RecursiveMode::Recursive).is_ok();
        }
    }
}

/// Watches the vault, this worktree's Boundrail state and the shared history ref, publishing a
/// change once the relevant files have been quiet for [`DEBOUNCE`]. Symbolic links are not
/// followed. The watcher lives as long as its thread.
fn watch(server: &Arc<Server>, vault: &Path) -> Result<()> {
    let watched = Watched::find(vault);
    let (tx, rx) = mpsc::channel();
    let config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = RecommendedWatcher::new(tx, config)?;
    watcher.watch(&watched.vault, RecursiveMode::Recursive)?;
    // Shallow watches of the parents see the state folder, the history folder and packed-refs
    // appear; the folders themselves are watched deeply once they exist.
    let parents = [&watched.state, &watched.history, &watched.packed];
    let parents: BTreeSet<&Path> = (parents.into_iter().flatten())
        .filter_map(|p| p.parent())
        .collect();
    for parent in parents {
        let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
    }
    let (mut state_followed, mut history_followed) = (false, false);
    Watched::follow(&mut watcher, &watched.state, &mut state_followed);
    Watched::follow(&mut watcher, &watched.history, &mut history_followed);
    let server = Arc::clone(server);
    thread::Builder::new()
        .name("view-watcher".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                let mut parts = watched.parts(&event);
                if parts.is_empty() {
                    continue;
                }
                // Only relevant events restart the quiet period.
                let mut quiet = Instant::now() + DEBOUNCE;
                loop {
                    let wait = quiet.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(event) => {
                            let more = watched.parts(&event);
                            if !more.is_empty() {
                                parts.extend(more);
                                quiet = Instant::now() + DEBOUNCE;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                Watched::follow(&mut watcher, &watched.state, &mut state_followed);
                Watched::follow(&mut watcher, &watched.history, &mut history_followed);
                let revision = vault::load_dir(watched.vault.clone())
                    .ok()
                    .map(|m| m.revision.digest);
                let mut news = server.news.lock().unwrap_or_else(|e| e.into_inner());
                news.generation += 1;
                news.revision = revision;
                news.parts = parts;
                drop(news);
                server.changed.notify_all();
            }
        })?;
    Ok(())
}

impl Server {
    /// Answers one request and closes, or hands back the connection of an event stream, whose
    /// place among [`MAX_STREAMS`] it has claimed.
    fn serve(&self, mut stream: TcpStream) -> Option<TcpStream> {
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let deadline = Instant::now() + page::HEAD_DEADLINE;
        let read = page::read_request(&mut stream, deadline, |s, wait| {
            s.set_read_timeout(Some(wait))
        });
        let reply = match read {
            Err(bad) => text(bad.status(), "Bad request"),
            Ok(request) => match self.refusal(&request) {
                Some(refused) => refused,
                None if request.path == "/api/events" => {
                    if claim(&self.streams, MAX_STREAMS) {
                        return Some(stream);
                    }
                    text(503, "Too many event streams")
                }
                None => self.answer(&request),
            },
        };
        let _ = stream.write_all(&reply);
        None
    }

    /// The refusal of a request that may not be served, or `None`.
    fn refusal(&self, request: &Request) -> Option<Vec<u8>> {
        if !page::host_allowed(request, self.port) {
            return Some(text(403, "Unknown Host"));
        }
        if request.method != "GET" {
            let allow = [("Allow", "GET")];
            let body = b"Only GET";
            return Some(page::response(
                405,
                "text/plain; charset=utf-8",
                body,
                &allow,
            ));
        }
        let api = request.path.starts_with("/api/");
        if api && !page::token_matches(request, &self.token) {
            let refusal = Refusal::new(Code::RequestInvalid, "missing or wrong view token");
            return Some(json_response(403, &model_cli::refusal_output(&refusal)));
        }
        None
    }

    /// The answer to an allowed request.
    fn answer(&self, request: &Request) -> Vec<u8> {
        let asset = |content_type: &str, body: &str| {
            let csp = [("Content-Security-Policy", page::CONTENT_SECURITY_POLICY)];
            page::response(200, content_type, body.as_bytes(), &csp)
        };
        match request.path.as_str() {
            "/" => asset("text/html; charset=utf-8", page::INDEX_HTML),
            "/app.js" => asset("text/javascript; charset=utf-8", page::APP_JS),
            "/style.css" => asset("text/css; charset=utf-8", page::STYLE_CSS),
            "/launch" if self.launch.redeem(request.param("c")) => {
                let location = format!("/?t={}", self.token);
                page::response(
                    303,
                    "text/plain; charset=utf-8",
                    b"",
                    &[("Location", &location)],
                )
            }
            "/launch" => text(403, "Unknown or used launch code"),
            "/api/render" => match self.render(request) {
                Ok(svg) => {
                    let csp = [("Content-Security-Policy", page::RENDER_CSP)];
                    page::response(200, "image/svg+xml", svg.as_bytes(), &csp)
                }
                Err(error) => error_response(&error),
            },
            path => match self.api(path, request) {
                Ok(Some(output)) => json_response(200, &output),
                Ok(None) => text(404, "Not found"),
                Err(error) => error_response(&error),
            },
        }
    }

    /// The JSON answer of an API path, or `None` for an unknown path.
    fn api(&self, path: &str, request: &Request) -> Result<Option<Value>> {
        let root = &self.root;
        let param = |name| request.param(name);
        Ok(Some(match path {
            "/api/model" => {
                if param("checkpoint").is_some() && param("proposal").is_some() {
                    return Err(invalid("read a checkpoint or a proposal, not both"));
                }
                model_cli::read_output(root, param("checkpoint"), param("proposal"))?
            }
            "/api/check" => model_cli::run_check(root, None, param("proposal"), false)?.output()?,
            "/api/proposals" => model_cli::proposals_output(root)?,
            "/api/checkpoints" => model_cli::checkpoints_output(root, None)?,
            "/api/compare" => {
                let proposal =
                    param("proposal").ok_or_else(|| invalid("compare needs proposal"))?;
                let against = param("against").unwrap_or(CURRENT);
                model_cli::compare_output(root, proposal, against)?
            }
            "/api/inspect" => {
                model_cli::inspect_output(root, param("subject"), param("proposal"), false)?
            }
            "/api/walkthrough" => {
                let id = param("id").ok_or_else(|| invalid("walkthrough needs id"))?;
                model_cli::walkthrough_output(root, id)?
            }
            "/api/pictures" => model_cli::pictures_output(root, param("status").is_some())?,
            _ => return Ok(None),
        }))
    }

    fn render(&self, request: &Request) -> Result<String> {
        let owned = |name| request.param(name).map(str::to_owned);
        let view = (request.param("view"))
            .map(|v| RenderView::from_str(v, true).map_err(|_| invalid("unknown view")))
            .transpose()?;
        let depth = (request.param("depth"))
            .map(|d| {
                d.parse()
                    .map_err(|_| invalid("depth must be a whole number"))
            })
            .transpose()?;
        let spec = RenderSpec {
            walkthrough: owned("walkthrough"),
            beat: owned("beat"),
            picture: owned("picture"),
            proposal: owned("proposal"),
            checkpoint: owned("checkpoint"),
            view,
            flow: owned("flow"),
            scope: owned("scope"),
            depth,
            against: owned("against"),
        };
        model_cli::render_svg(&self.root, &spec, None).map(|(svg, _)| svg)
    }

    /// Streams `changed` events with the current state's revision (null when it does not load)
    /// and the parts that changed, until the page goes away.
    fn events(&self, mut stream: TcpStream) {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                    Cache-Control: no-store\r\nConnection: close\r\n\
                    X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\r\n\
                    retry: 2000\n\n";
        let lock = || self.news.lock().unwrap_or_else(|e| e.into_inner());
        // Read before the head goes out, so a change the page causes once connected is sent.
        let mut seen = lock().generation;
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        loop {
            let (news, _) = (self.changed)
                .wait_timeout_while(lock(), KEEPALIVE, |n| n.generation == seen)
                .unwrap_or_else(|e| e.into_inner());
            let message = if news.generation == seen {
                ": keepalive\n\n".to_owned()
            } else {
                // A stream that slept through several changes cannot tell what they touched.
                let parts: Vec<&str> = if news.generation == seen + 1 {
                    news.parts.iter().copied().collect()
                } else {
                    vec!["model", "proposals", "state", "history"]
                };
                seen = news.generation;
                let data = json!({ "revision": news.revision, "parts": parts });
                format!("event: changed\ndata: {data}\n\n")
            };
            drop(news);
            if stream.write_all(message.as_bytes()).is_err() {
                return;
            }
        }
    }
}

fn invalid(message: &str) -> anyhow::Error {
    Refusal::new(Code::RequestInvalid, message).into()
}

fn text(status: u16, body: &str) -> Vec<u8> {
    page::response(status, "text/plain; charset=utf-8", body.as_bytes(), &[])
}

fn json_response(status: u16, value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    page::response(status, "application/json", &body, &[])
}

/// A refusal as its JSON output: 404 when something named is not there, 400 otherwise. Any
/// other error is a 500.
fn error_response(error: &anyhow::Error) -> Vec<u8> {
    let Some(refusal) = error.downcast_ref::<Refusal>() else {
        let output = json!({"success": false, "error": {"message": format!("{error:#}")}});
        return json_response(500, &output);
    };
    let status = match refusal.code {
        Code::VaultNotFound
        | Code::CheckoutNotFound
        | Code::SubjectNotFound
        | Code::SchemaNotFound
        | Code::CheckpointNotFound => 404,
        _ => 400,
    };
    json_response(status, &model_cli::refusal_output(refusal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_are_capped_and_given_back() {
        let counter = AtomicUsize::new(0);
        assert!(claim(&counter, 2) && claim(&counter, 2));
        assert!(!claim(&counter, 2), "a third place is refused");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        drop(Release(&counter));
        assert!(claim(&counter, 2), "a released place is free again");
    }

    #[test]
    fn the_launch_url_redirects_to_the_token_once() {
        let server = Server {
            root: PathBuf::new(),
            token: boundrail::token().unwrap(),
            launch: page::Launch::new().unwrap(),
            port: 4000,
            connections: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
            news: Mutex::default(),
            changed: Condvar::new(),
        };
        let code = server.launch.code().unwrap();
        assert_ne!(code, server.token);
        let launch = |code: &str| {
            let head = format!("GET /launch?c={code} HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n");
            let request = page::parse(head.as_bytes()).unwrap();
            assert!(server.refusal(&request).is_none());
            String::from_utf8(server.answer(&request)).unwrap()
        };
        assert!(launch("wrong").starts_with("HTTP/1.1 403 "));
        let redirect = launch(&code);
        assert!(
            redirect.starts_with("HTTP/1.1 303 See Other\r\n"),
            "{redirect}"
        );
        let location = format!("\r\nLocation: /?t={}\r\n", server.token);
        assert!(redirect.contains(&location), "{redirect}");
        assert!(
            launch(&code).starts_with("HTTP/1.1 403 "),
            "a used code is refused"
        );
    }

    #[test]
    fn changed_paths_are_sorted_into_parts_by_whole_components() {
        let watched = Watched {
            vault: "/r/.boundrail/model".into(),
            proposals: "/r/.boundrail/model/proposals".into(),
            state: Some("/r/.git/boundrail".into()),
            history: Some("/r/.git/refs/boundrail".into()),
            packed: Some("/r/.git/packed-refs".into()),
        };
        let part = |path: &str| watched.part(Path::new(path));
        assert_eq!(
            part("/r/.boundrail/model/modules/A.module.json"),
            Some("model")
        );
        assert_eq!(
            part("/r/.boundrail/model/proposals/P/proposal.json"),
            Some("proposals")
        );
        assert_eq!(part("/r/.git/boundrail/claims.json"), Some("state"));
        assert_eq!(part("/r/.git/refs/boundrail/history"), Some("history"));
        assert_eq!(part("/r/.git/packed-refs"), Some("history"));
        for ignored in [
            "/r/.git/index",
            "/r/.git/refs/heads/main",
            "/r/.git/refs/boundrail-other/x",
            "/r/.git/packed-refs.lock",
            "/r/.boundrail/model-copy/model.json",
        ] {
            assert_eq!(part(ignored), None, "{ignored}");
        }
    }
}
