//! `boundrail model view`: the live browser view. A small HTTP/1.1 server on the loopback
//! interface answers the page's requests with the same JSON the model commands print, and tells
//! open pages over Server-Sent Events when the vault changes or an agent asks to show a reading.
//! It writes only its own state file, so `model view --show` and `--panes` can find it.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use boundrail::arch::proposal::CURRENT;
use boundrail::arch::vault;
use boundrail::arch::view::{self as page, Request, Shown};
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
/// The file in this worktree's Boundrail state folder that names the running view.
const STATE_FILE: &str = "view.json";
/// The most a state file may hold.
const STATE_MAX: u64 = 4096;
/// How long `--show` and `--panes` wait to reach the view and for its answer.
const CONTROL_WAIT: Duration = Duration::from_secs(10);
/// Panes a page may report; the page has two at most.
const MAX_PANES: usize = 2;
const RESPONSE_MAX: u64 = 1 << 20;
const STATUS_CODE: std::ops::Range<usize> = 9..12;

#[derive(Args)]
pub(super) struct ViewArgs {
    /// Port on 127.0.0.1; 0 picks a free one
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Print the URL without opening a browser
    #[arg(long)]
    no_open: bool,
    /// Let the page ask the local agent CLI about the model (POST /api/explain), as `model
    /// explain` does. Off by default: a browser request then runs a program on this machine
    #[arg(long)]
    allow_explain: bool,
    /// Show a reading in the page open on the view already running for this checkout, in its
    /// active pane, instead of serving: the current state's architecture, or what the reading
    /// flags below ask for, which `model render` would draw
    #[arg(long, conflicts_with_all = ["port", "no_open", "allow_explain", "panes"])]
    show: bool,
    /// List the pages open on the view already running for this checkout, with what each of
    /// their panes shows, instead of serving
    #[arg(long, conflicts_with_all = ["port", "no_open", "allow_explain"])]
    panes: bool,
    #[command(flatten)]
    reading: ShowArgs,
}

/// What `--show` shows, with the flags `model render` takes for it.
#[derive(Args)]
struct ShowArgs {
    /// The view to show; a flow's sequence diagram when --flow is given, else the architecture
    #[arg(long, value_enum, requires = "show")]
    view: Option<RenderView>,
    /// The flow a sequence or flowchart view shows: its qualified or local ID, or its unique name
    #[arg(long, requires = "show")]
    flow: Option<String>,
    /// Show only this module's parts: its ID or unique current or former name
    #[arg(long, requires = "show")]
    scope: Option<String>,
    /// How many levels of containers the architecture view opens [default: 2]
    #[arg(long, requires = "show")]
    depth: Option<usize>,
    /// Select this module and focus on it, dimming what the selection does not touch: its ID or
    /// unique name; repeat for several
    #[arg(long, requires = "show")]
    focus: Vec<String>,
    /// Show this proposal
    #[arg(long, requires = "show")]
    proposal: Option<String>,
    /// Show the model a checkpoint holds: its ID or a unique commit prefix
    #[arg(long, requires = "show")]
    checkpoint: Option<String>,
    /// Compare with the current state or this proposal
    #[arg(long, requires = "show")]
    against: Option<String>,
    /// Show what this saved view reads: its ID or unique name, with no other reading flag
    #[arg(long = "saved-view", requires = "show")]
    saved_view: Option<String>,
    /// Play this walkthrough: its ID or unique name, with no other reading flag
    #[arg(long, requires = "show", conflicts_with = "saved_view")]
    walkthrough: Option<String>,
    /// The beat of --walkthrough to show: its number, from 1, or its ID [default: 1]
    #[arg(long, requires = "walkthrough")]
    beat: Option<String>,
    /// The page to show it in, as --panes numbers them [default: the page used last]
    #[arg(long, requires = "show")]
    page: Option<u64>,
}

struct Server {
    root: PathBuf,
    token: String,
    /// The code the opener's URL carries instead of the token.
    launch: page::Launch,
    port: u16,
    /// Whether `POST /api/explain` runs the agent.
    allow_explain: bool,
    /// The token `model view --show` and `--panes` send, which the page never sees.
    control: String,
    pages: Mutex<Pages>,
    explains: AtomicUsize,
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
    /// How many readings `--show` has sent.
    shows: u64,
    /// The last of them: the page it is for and its hash.
    show: Option<(u64, String)>,
}

/// The pages open on the server, by the number their event stream gave them.
#[derive(Default)]
struct Pages {
    next: u64,
    /// How many reports the pages have made, which orders them by when they were used.
    reports: u64,
    open: BTreeMap<u64, Page>,
}

/// What a page last reported: each pane's hash, the active one, and its report's number.
#[derive(Default)]
struct Page {
    panes: Vec<String>,
    active: usize,
    reported: u64,
}

/// What `model view` leaves in the state folder while it serves, readable by the user alone.
#[derive(serde::Serialize, serde::Deserialize)]
struct Running {
    port: u16,
    control: String,
}

/// Serves until the process is stopped. The URL, with its token, is the first line on stdout.
/// The browser opener gets a one-time launch URL instead, so the token stays out of its
/// arguments. With `--show` or `--panes` it asks the running view instead.
pub(super) fn run(root: &Path, markdown: bool, args: &ViewArgs) -> Result<()> {
    if args.show {
        return show(root, markdown, &args.reading);
    }
    if args.panes {
        return list_panes(root, markdown);
    }
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
        allow_explain: args.allow_explain,
        control: boundrail::token()?,
        pages: Mutex::default(),
        explains: AtomicUsize::new(0),
        connections: AtomicUsize::new(0),
        streams: AtomicUsize::new(0),
        news: Mutex::default(),
        changed: Condvar::new(),
    });
    watch(&server, &vault)?;
    let _published = publish(&vault, port, &server.control);
    let url = format!("http://127.0.0.1:{port}/?t={}", server.token);
    println!("{url}");
    std::io::stdout().flush()?;
    eprintln!("Serving a read-only view of the model; stop with Ctrl-C");
    if args.allow_explain {
        eprintln!("The page may ask the local agent CLI about the model (--allow-explain)");
    }
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
    let opened = if cfg!(target_os = "macos") {
        spawn_detached("open", [url])
    } else if cfg!(windows) {
        // `start` takes its first quoted argument as a window title.
        spawn_detached("cmd", ["/c", "start", "", url])
    } else {
        spawn_detached("xdg-open", [url])
    };
    if let Err(error) = opened {
        eprintln!("warning: could not open a browser ({error}); open the URL above");
    }
}

/// `view.json` in this worktree's Boundrail state folder, which Git never tracks.
fn state_file(vault: &Path) -> Result<PathBuf> {
    Ok(boundrail::arch::state_dir(vault)?.join(STATE_FILE))
}

/// Removes the state file when dropped, if it still names this view: a second view in the
/// checkout may have taken it over.
struct Published {
    path: PathBuf,
    control: String,
}

impl Drop for Published {
    fn drop(&mut self) {
        if let Ok(Some(running)) = read_running(&self.path)
            && running.control == self.control
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Writes the state file, readable and writable by the user alone, so `--show` and `--panes`
/// find this view: whole, by renaming a new file over it. A view that cannot write it still
/// serves, with a warning.
fn publish(vault: &Path, port: u16, control: &str) -> Option<Published> {
    let written = (|| -> Result<PathBuf> {
        let path = state_file(vault)?;
        let dir = path.parent().context("the state file has a folder")?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let fresh = dir.join(format!("{STATE_FILE}.{}", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let running = Running {
            port,
            control: control.to_owned(),
        };
        let mut file = options
            .open(&fresh)
            .with_context(|| format!("creating {}", fresh.display()))?;
        file.write_all(&serde_json::to_vec(&running)?)?;
        drop(file);
        fs::rename(&fresh, &path).inspect_err(|_| {
            let _ = fs::remove_file(&fresh);
        })?;
        Ok(path)
    })();
    match written {
        Ok(path) => Some(Published {
            path,
            control: control.to_owned(),
        }),
        Err(error) => {
            eprintln!("warning: model view --show cannot find this view: {error:#}");
            None
        }
    }
}

fn read_running(path: &Path) -> Result<Option<Running>> {
    let mut text = String::new();
    match fs::File::open(path) {
        Ok(file) => file.take(STATE_MAX).read_to_string(&mut text).map(drop)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("reading {}", path.display()))
}

/// Refuses as no running view for this checkout, with `why`.
fn no_view(why: &str) -> anyhow::Error {
    let message = format!("{why}; start one with `boundrail model view`");
    Refusal::new(Code::RequestInvalid, message)
        .with_details(json!({"reason": "no-view"}))
        .into()
}

/// Sends `method target` to the view running for this checkout with its control token, and
/// returns its JSON answer. A refusal comes back as a refusal.
fn ask_running(root: &Path, method: &str, target: &str) -> Result<Value> {
    let path = state_file(&vault::locate(root)?)?;
    let Some(running) = read_running(&path)? else {
        return Err(no_view("no view is running for this checkout"));
    };
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, running.port));
    let gone = || no_view("the view that wrote its state file is no longer running");
    let mut stream = TcpStream::connect_timeout(&address, CONTROL_WAIT).map_err(|_| gone())?;
    stream.set_read_timeout(Some(CONTROL_WAIT))?;
    stream.set_write_timeout(Some(CONTROL_WAIT))?;
    write!(
        stream,
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{}: {}\r\nContent-Length: 0\r\n\r\n",
        running.port,
        page::CONTROL_HEADER,
        running.control
    )
    .map_err(|_| gone())?;
    let mut response = String::new();
    stream
        .take(RESPONSE_MAX)
        .read_to_string(&mut response)
        .map_err(|_| gone())?;
    let status = response
        .get(STATUS_CODE)
        .and_then(|s| s.parse::<u16>().ok());
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body);
    let (Some(status), Some(body)) = (status, body) else {
        return Err(gone());
    };
    if status == 403 {
        return Err(gone());
    }
    let answer: Value = serde_json::from_str(body).map_err(|_| gone())?;
    if status == 200 {
        return Ok(answer);
    }
    let error = &answer["error"];
    let code = match error["code"].as_str() {
        Some("SUBJECT_NOT_FOUND") => Code::SubjectNotFound,
        _ => Code::RequestInvalid,
    };
    let message = error["message"].as_str().unwrap_or("the view refused");
    let mut refusal = Refusal::new(code, message);
    if !error["details"].is_null() {
        refusal = refusal.with_details(error["details"].clone());
    }
    Err(refusal.into())
}

/// `model view --show`: sends the reading to the running view, which shows it in a page's
/// active pane.
fn show(root: &Path, markdown: bool, args: &ShowArgs) -> Result<()> {
    let spec = RenderSpec {
        walkthrough: args.walkthrough.clone(),
        beat: args
            .walkthrough
            .as_ref()
            .map(|_| args.beat.clone().unwrap_or_else(|| "1".into())),
        saved_view: args.saved_view.clone(),
        proposal: args.proposal.clone(),
        checkpoint: args.checkpoint.clone(),
        view: args.view,
        flow: args.flow.clone(),
        scope: args.scope.clone(),
        depth: args.depth,
        against: args.against.clone(),
        focus: args.focus.clone(),
        ..RenderSpec::default()
    };
    let shown = model_cli::shown_reading(root, &spec)?;
    let hash = shown.to_hash();
    let mut target = format!("/api/show?reading={}", page::encode(&hash));
    if let Some(page) = args.page {
        target.push_str(&format!("&page={page}"));
    }
    let answer = ask_running(root, "POST", &target)?;
    if markdown {
        println!("Shown in page {}: #{hash}", answer["page"]);
        return Ok(());
    }
    print_json(&json!({
        "schema": model_cli::MODEL_SCHEMA,
        "kind": "view-show",
        "success": true,
        "page": answer["page"],
        "hash": hash,
        "reading": shown,
    }))
}

/// `model view --panes`: the pages open on the running view and what their panes show.
fn list_panes(root: &Path, markdown: bool) -> Result<()> {
    let answer = ask_running(root, "GET", "/api/panes")?;
    let pages = answer["pages"].as_array().cloned().unwrap_or_default();
    if !markdown {
        return print_json(&json!({
            "schema": model_cli::MODEL_SCHEMA,
            "kind": "view-panes",
            "success": true,
            "pages": pages,
        }));
    }
    if pages.is_empty() {
        println!("No page is open on the view");
    }
    for page in &pages {
        let used = if page["last_used"] == true {
            ", used last"
        } else {
            ""
        };
        println!("Page {}{used}:", page["page"]);
        for pane in page["panes"].as_array().into_iter().flatten() {
            let active = if pane["active"] == true {
                " (active)"
            } else {
                ""
            };
            let hash = vault::escape_controls(pane["hash"].as_str().unwrap_or_default());
            println!("- pane {}{active}: #{hash}", pane["pane"]);
        }
    }
    Ok(())
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
        let own = path.parent() == self.state.as_deref()
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(STATE_FILE));
        if own {
            None
        } else if path.starts_with(&self.proposals) {
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
        let explain = request.path == "/api/explain";
        let control = matches!(request.path.as_str(), "/api/show" | "/api/panes");
        let allowed = match request.path.as_str() {
            "/api/explain" => "GET, POST",
            "/api/show" | "/api/pane-state" => "POST",
            _ => "GET",
        };
        if !allowed.split(", ").any(|m| m == request.method) {
            let allow = [("Allow", allowed)];
            let body = format!("Only {allowed}");
            return Some(page::response(
                405,
                "text/plain; charset=utf-8",
                body.as_bytes(),
                &allow,
            ));
        }
        if (request.method == "POST" || control) && !page::origin_allowed(request, self.port) {
            return Some(text(403, "Unknown Origin"));
        }
        if control && !page::control_matches(request, &self.control) {
            let refusal = Refusal::new(Code::RequestInvalid, "missing or wrong control token");
            return Some(json_response(403, &model_cli::refusal_output(&refusal)));
        }
        let api = request.path.starts_with("/api/");
        if api && !control && !page::token_matches(request, &self.token) {
            let refusal = Refusal::new(Code::RequestInvalid, "missing or wrong view token");
            return Some(json_response(403, &model_cli::refusal_output(&refusal)));
        }
        if explain && request.method == "POST" && !self.allow_explain {
            let message = "this view was started without --allow-explain; run `boundrail model \
                           explain` in a terminal instead";
            let refusal = Refusal::new(Code::RequestInvalid, message)
                .with_details(json!({"reason": "explain-disabled"}));
            return Some(json_response(403, &model_cli::refusal_output(&refusal)));
        }
        None
    }

    /// The answer to an allowed request.
    fn answer(&self, request: &Request) -> Vec<u8> {
        let _running = if request.path == "/api/explain" && request.method == "POST" {
            if !claim(&self.explains, 1) {
                let message = "the agent is still answering another question; ask again when it \
                               is done";
                let refusal = Refusal::new(Code::RequestInvalid, message)
                    .with_details(json!({"reason": "explain-busy"}));
                return json_response(409, &model_cli::refusal_output(&refusal));
            }
            Some(Release(&self.explains))
        } else {
            None
        };
        let asset = |content_type: &str, body: &str| {
            let csp = [("Content-Security-Policy", page::CONTENT_SECURITY_POLICY)];
            page::response(200, content_type, body.as_bytes(), &csp)
        };
        match request.path.as_str() {
            "/" => asset("text/html; charset=utf-8", page::INDEX_HTML),
            "/app.js" => asset("text/javascript; charset=utf-8", page::APP_JS),
            "/style.css" => asset("text/css; charset=utf-8", page::STYLE_CSS),
            "/font.woff2" => page::response(200, "font/woff2", page::FONT_WOFF2, &[]),
            "/launch" => match self.launch.redeem(request.param("c")) {
                page::Redeem::Fresh => {
                    let location = format!("/?t={}", self.token);
                    page::response(
                        303,
                        "text/plain; charset=utf-8",
                        b"",
                        &[("Location", &location)],
                    )
                }
                page::Redeem::Spent => {
                    eprintln!(
                        "warning: the launch link was opened twice; another local process may \
                         have opened this view first. Stop it and run `model view --no-open`"
                    );
                    text(
                        403,
                        "This launch link was already used, possibly by another local process. \
                         Stop the view and open the URL it printed with --no-open.",
                    )
                }
                page::Redeem::Unknown => text(403, "Unknown launch code"),
            },
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
        let reads = matches!(path, "/api/model" | "/api/references" | "/api/reference");
        if reads && param("checkpoint").is_some() && param("proposal").is_some() {
            return Err(invalid("read a checkpoint or a proposal, not both"));
        }
        Ok(Some(match path {
            "/api/model" => model_cli::read_output(root, param("checkpoint"), param("proposal"))?,
            "/api/check" => model_cli::run_check(
                root,
                None,
                param("proposal"),
                boundrail::arch::lsp::Enable::Off,
                &Default::default(),
            )?
            .output()?,
            "/api/proposals" => model_cli::proposals_output(root)?,
            "/api/checkpoints" => model_cli::checkpoints_output(root, None)?,
            "/api/compare" => {
                let proposal =
                    param("proposal").ok_or_else(|| invalid("compare needs proposal"))?;
                let against = param("against").unwrap_or(CURRENT);
                let asked = model_cli::Compare {
                    proposal,
                    against,
                    against_checkpoint: None,
                    beside: &[],
                };
                model_cli::compare_output(root, &asked)?
            }
            "/api/inspect" => {
                let asked = model_cli::Inspect {
                    subject: param("subject"),
                    proposal: param("proposal"),
                    ..Default::default()
                };
                model_cli::inspect_output(root, &asked)?
            }
            "/api/walkthrough" => {
                let id = param("id").ok_or_else(|| invalid("walkthrough needs id"))?;
                model_cli::walkthrough_output(root, id)?
            }
            "/api/pictures" => model_cli::pictures_output(root, param("status").is_some())?,
            "/api/show" => self.send_show(request)?,
            "/api/pane-state" => self.pane_state(request)?,
            "/api/panes" => self.panes(),
            "/api/explain" if request.method == "GET" => {
                json!({"kind": "explain-status", "allowed": self.allow_explain})
            }
            "/api/explain" => {
                let subjects = (request.query.iter())
                    .filter(|(name, value)| name == "subject" && !value.is_empty())
                    .map(|(_, value)| value.as_str())
                    .collect();
                let asked = model_cli::Asked {
                    subjects,
                    relationship: param("relationship"),
                    flow: param("flow"),
                    proposal: param("proposal"),
                    question: param("question").unwrap_or_default(),
                };
                model_cli::explain_output(root, &asked)?
            }
            "/api/references" => {
                let subject = param("subject").ok_or_else(|| invalid("references need subject"))?;
                model_cli::references_output(root, subject, param("checkpoint"), param("proposal"))?
            }
            "/api/reference" => {
                let subject = param("subject").ok_or_else(|| invalid("reference needs subject"))?;
                let index = (param("index"))
                    .and_then(|i| i.parse().ok())
                    .ok_or_else(|| invalid("reference needs index, a whole number"))?;
                let (checkpoint, proposal) = (param("checkpoint"), param("proposal"));
                model_cli::reference_output(root, subject, index, checkpoint, proposal)?
            }
            _ => return Ok(None),
        }))
    }

    /// Sends the reading `reading` (a hash, possibly empty) to page `page`, else to the page used
    /// last, over its event stream.
    fn send_show(&self, request: &Request) -> Result<Value> {
        let reading = (request.query.iter())
            .find(|(name, _)| name == "reading")
            .map(|(_, value)| value.clone())
            .ok_or_else(|| invalid("show needs reading"))?;
        let asked = (request.param("page"))
            .map(|p| {
                p.parse::<u64>()
                    .map_err(|_| invalid("page must be a whole number"))
            })
            .transpose()?;
        let pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        let page = match asked {
            Some(page) if pages.open.contains_key(&page) => page,
            Some(page) => {
                let message = format!("no page {page} is open; `model view --panes` lists them");
                return Err(Refusal::new(Code::SubjectNotFound, message).into());
            }
            None => pages.last_used().ok_or_else(|| {
                let message = "no page is open on this view; open its URL in a browser";
                anyhow::Error::from(
                    Refusal::new(Code::RequestInvalid, message)
                        .with_details(json!({"reason": "no-page"})),
                )
            })?,
        };
        drop(pages);
        let mut news = self.news.lock().unwrap_or_else(|e| e.into_inner());
        news.shows += 1;
        news.show = Some((page, reading.clone()));
        drop(news);
        self.changed.notify_all();
        Ok(json!({"kind": "view-show", "page": page, "reading": reading}))
    }

    /// Keeps what an open page says its panes show: `pane` once per pane, as hashes, and the
    /// `active` one's index.
    fn pane_state(&self, request: &Request) -> Result<Value> {
        let page = (request.param("page"))
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| invalid("pane-state needs page, a whole number"))?;
        let panes: Vec<String> = (request.query.iter())
            .filter(|(name, _)| name == "pane")
            .map(|(_, value)| value.strip_prefix('#').unwrap_or(value).to_owned())
            .collect();
        let active = request
            .param("active")
            .and_then(|a| a.parse().ok())
            .unwrap_or(0);
        if panes.is_empty() || panes.len() > MAX_PANES || active >= panes.len() {
            let message = format!("pane-state needs one to {MAX_PANES} panes and an active one");
            return Err(invalid(&message));
        }
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.reports += 1;
        let reported = pages.reports;
        let Some(open) = pages.open.get_mut(&page) else {
            return Err(Refusal::new(Code::SubjectNotFound, "no such page is open").into());
        };
        *open = Page {
            panes,
            active,
            reported,
        };
        Ok(json!({"kind": "pane-state", "page": page}))
    }

    /// The open pages, each pane with its hash and what it reads.
    fn panes(&self) -> Value {
        let pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        let last = pages.last_used();
        let list: Vec<Value> = (pages.open.iter())
            .map(|(number, page)| {
                let panes: Vec<Value> = (page.panes.iter().enumerate())
                    .map(|(i, hash)| {
                        json!({
                            "pane": i,
                            "active": i == page.active,
                            "hash": hash,
                            "reading": Shown::parse(hash),
                        })
                    })
                    .collect();
                json!({"page": number, "last_used": Some(*number) == last, "panes": panes})
            })
            .collect();
        json!({"kind": "view-panes", "pages": list})
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
            saved_view: owned("saved"),
            proposal: owned("proposal"),
            checkpoint: owned("checkpoint"),
            view,
            flow: owned("flow"),
            scope: owned("scope"),
            depth,
            against: owned("against"),
            ..RenderSpec::default()
        };
        model_cli::render_svg(&self.root, &spec, None).map(|(rendered, _)| rendered.svg)
    }

    /// Streams a `hello` event with the number the page goes by, then `changed` events with the
    /// current state's revision (null when it does not load) and the parts that changed, and
    /// `show` events with a reading `--show` sent this page, until the page goes away.
    fn events(&self, mut stream: TcpStream) {
        let number = {
            let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
            pages.next += 1;
            let number = pages.next;
            pages.open.insert(number, Page::default());
            number
        };
        let _open = Closing(self, number);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\
             X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\r\n\
             retry: 2000\n\nevent: hello\ndata: {}\n\n",
            json!({ "page": number })
        );
        let lock = || self.news.lock().unwrap_or_else(|e| e.into_inner());
        // Read before the head goes out, so a change the page causes once connected is sent.
        let (mut seen, mut shown) = {
            let news = lock();
            (news.generation, news.shows)
        };
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        loop {
            let (news, _) = (self.changed)
                .wait_timeout_while(lock(), KEEPALIVE, |n| {
                    n.generation == seen && n.shows == shown
                })
                .unwrap_or_else(|e| e.into_inner());
            let mut message = String::new();
            if news.shows != shown {
                shown = news.shows;
                if let Some((page, reading)) = &news.show
                    && *page == number
                {
                    let data = json!({ "reading": reading });
                    message.push_str(&format!("event: show\ndata: {data}\n\n"));
                }
            }
            if news.generation == seen {
                if message.is_empty() {
                    message.push_str(": keepalive\n\n");
                }
            } else {
                // A stream that slept through several changes cannot tell what they touched.
                let parts: Vec<&str> = if news.generation == seen + 1 {
                    news.parts.iter().copied().collect()
                } else {
                    vec!["model", "proposals", "state", "history"]
                };
                seen = news.generation;
                let data = json!({ "revision": news.revision, "parts": parts });
                message.push_str(&format!("event: changed\ndata: {data}\n\n"));
            }
            drop(news);
            if stream.write_all(message.as_bytes()).is_err() {
                return;
            }
        }
    }
}

impl Pages {
    /// The open page that reported last, else the newest.
    fn last_used(&self) -> Option<u64> {
        (self.open.iter())
            .max_by_key(|(number, page)| (page.reported, **number))
            .map(|(number, _)| *number)
    }
}

/// Forgets page `.1` when its event stream ends.
struct Closing<'a>(&'a Server, u64);

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        let mut pages = self.0.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.open.remove(&self.1);
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
            allow_explain: false,
            control: boundrail::token().unwrap(),
            pages: Mutex::default(),
            explains: AtomicUsize::new(0),
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
        let reused = launch(&code);
        assert!(
            reused.starts_with("HTTP/1.1 403 "),
            "a used code is refused"
        );
        assert!(reused.contains("already used"), "{reused}");
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
