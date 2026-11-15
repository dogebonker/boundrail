//! One language server session over stdio: start, initialize, open files, ask, shut down. A
//! reader thread drains the server's output and a writer thread feeds its input, so a server
//! that stops reading never blocks the client past its timeouts.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::framing;
use super::wire::file_uri;

/// How long one request may take, within the session's budget.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `shutdown` may take, within the session's budget.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the server may take to exit after `exit` before it is killed, and how often that
/// is checked.
const EXIT_GRACE: Duration = Duration::from_secs(1);
const EXIT_POLL: Duration = Duration::from_millis(10);
/// How many messages the reader holds before it stops reading the server's output.
const INCOMING_MAX: usize = 4;
/// The server's last stderr lines kept for an error report, and the longest line kept.
const STDERR_LINES: usize = 5;
const STDERR_LINE_MAX: u64 = 512;

/// A running server. Dropping it kills the server if it is still running.
pub(super) struct Session {
    child: Child,
    outgoing: Sender<Value>,
    incoming: Receiver<Result<Value, String>>,
    next_id: u64,
    /// When the whole session's budget runs out.
    deadline: Instant,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Session {
    /// Starts `program` with `args` in `root`, with `budget` for the whole session. The server
    /// sees only the absolute folders of `PATH`, so a program it looks up there itself, such as
    /// `node`, cannot be one the checkout plants.
    pub(super) fn spawn(
        program: &Path,
        args: &[String],
        root: &Path,
        budget: Duration,
    ) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .env("PATH", crate::collect::absolute_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", program.display()))?;
        let (stdin, stdout, stderr) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (stdin, stdout, stderr) = (
            stdin.context("no stdin")?,
            stdout.context("no stdout")?,
            stderr.context("no stderr")?,
        );
        let (outgoing, to_server) = mpsc::channel::<Value>();
        thread::spawn(move || feed(stdin, &to_server));
        let (from_server, incoming) = mpsc::sync_channel(INCOMING_MAX);
        thread::spawn(move || drain(stdout, &from_server));
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let kept = Arc::clone(&tail);
        thread::spawn(move || keep_tail(stderr, &kept));
        Ok(Self {
            child,
            outgoing,
            incoming,
            next_id: 1,
            deadline: Instant::now() + budget,
            stderr: tail,
        })
    }

    /// `initialize` for the workspace at `root`, asking only for references, definitions,
    /// document and workspace symbols, then `initialized`. Returns the server's capabilities.
    pub(super) fn initialize(
        &mut self,
        root: &Path,
        initialization_options: Option<&Value>,
    ) -> Result<Value> {
        let uri = file_uri(root);
        let name = root
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let capabilities = json!({
            "textDocument": {
                "references": {},
                "definition": {},
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            },
            "workspace": {"symbol": {}},
        });
        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{"uri": uri, "name": name}],
            "capabilities": capabilities,
            "initializationOptions": initialization_options,
        });
        let result = self.request("initialize", params)?;
        self.notify("initialized", json!({}))?;
        Ok(result["capabilities"].clone())
    }

    /// Sends a request and waits for its answer, answering the server's own requests meanwhile.
    pub(super) fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let until = (Instant::now() + REQUEST_TIMEOUT).min(self.deadline);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let message = match self.incoming.recv_timeout(left) {
                Ok(message) => message.map_err(|e| anyhow!(e))?,
                Err(RecvTimeoutError::Timeout) => bail!("{method} got no answer in time"),
                Err(RecvTimeoutError::Disconnected) => bail!("the server stopped"),
            };
            if message.get("method").is_some() {
                if let Some(reply) = answer(&message) {
                    self.send(reply)?;
                }
                continue;
            }
            if message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("{method} failed: {}", error["message"]);
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub(super) fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn send(&self, message: Value) -> Result<()> {
        (self.outgoing.send(message)).map_err(|_| anyhow!("the server closed its input"))
    }

    /// Asks the server to shut down and exit; it is killed when it does not exit in time.
    pub(super) fn shutdown(mut self) {
        self.deadline = self.deadline.min(Instant::now() + SHUTDOWN_TIMEOUT);
        if self.request("shutdown", Value::Null).is_ok() {
            let _ = self.notify("exit", Value::Null);
            let until = Instant::now() + EXIT_GRACE;
            while Instant::now() < until && matches!(self.child.try_wait(), Ok(None)) {
                thread::sleep(EXIT_POLL);
            }
        }
    }

    /// The server's last stderr lines, joined.
    pub(super) fn stderr_tail(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        tail.iter().cloned().collect::<Vec<_>>().join(" | ")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Writes each message `to_server` sends to the server's input, until either side closes.
fn feed(mut stdin: impl Write, to_server: &Receiver<Value>) {
    for message in to_server {
        if framing::write(&mut stdin, &message).is_err() {
            break;
        }
    }
}

/// Sends each message the server writes to `from_server`, blocking while it is full, until the
/// output ends or breaks, which is sent as the last item.
fn drain(stdout: impl Read, from_server: &SyncSender<Result<Value, String>>) {
    let mut stdout = BufReader::new(stdout);
    loop {
        let message = match framing::read(&mut stdout) {
            Ok(Some(message)) => Ok(message),
            Ok(None) => Err("the server closed its output".to_owned()),
            Err(error) => Err(format!("reading the server's output: {error}")),
        };
        let end = message.is_err();
        if from_server.send(message).is_err() || end {
            break;
        }
    }
}

/// Reads `stderr` to its end, keeping its last lines in `tail`.
fn keep_tail(stderr: impl Read, tail: &Mutex<VecDeque<String>>) {
    let mut stderr = BufReader::new(stderr);
    loop {
        let mut line = Vec::new();
        match (stderr.by_ref().take(STDERR_LINE_MAX)).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&line);
        let line: String = (line.trim_end().chars())
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        if line.is_empty() {
            continue;
        }
        let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        if tail.len() == STDERR_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// The reply to a request from the server: `workspace/configuration` gets one null per item,
/// anything else null, so a server waiting on an answer never hangs. Notifications get none.
pub(super) fn answer(message: &Value) -> Option<Value> {
    let id = message.get("id")?;
    let result = match message["method"].as_str() {
        Some("workspace/configuration") => {
            let items = message["params"]["items"].as_array().map_or(0, Vec::len);
            Value::Array(vec![Value::Null; items])
        }
        _ => Value::Null,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_requests_get_null_or_one_null_per_configuration_item() {
        let configuration =
            json!({"id": 7, "method": "workspace/configuration", "params": {"items": [{}, {}]}});
        assert_eq!(
            answer(&configuration),
            Some(json!({"jsonrpc": "2.0", "id": 7, "result": [null, null]}))
        );
        for method in [
            "client/registerCapability",
            "window/workDoneProgress/create",
        ] {
            let request = json!({"id": "a", "method": method, "params": {}});
            assert_eq!(
                answer(&request),
                Some(json!({"jsonrpc": "2.0", "id": "a", "result": null}))
            );
        }
        assert_eq!(answer(&json!({"method": "window/logMessage"})), None);
    }
}
