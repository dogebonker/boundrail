//! A scripted language server for the integration tests, built with `rustc` from the standard
//! library alone. Its one argument, else `$BOUNDRAIL_FAKE_LSP_SCRIPT`, names a script: lines of
//! `method<TAB>needle<TAB>result`. A request gets the result of the first line whose method
//! matches and whose needle appears in the request's JSON, else null; the result `hang` makes the
//! server stop answering. Before anything else it asks the client for its configuration, as
//! real servers do.

use std::io::{self, BufRead, Read, Write};
use std::{env, fs, thread, time::Duration};

fn main() {
    // Started under an allowlisted name the server gets that name's fixed arguments instead.
    let path = (env::args().nth(1).filter(|arg| !arg.starts_with("--")))
        .or_else(|| env::var("BOUNDRAIL_FAKE_LSP_SCRIPT").ok())
        .expect("a script path");
    let script = fs::read_to_string(path).expect("a readable script");
    let rules: Vec<Vec<&str>> = (script.lines())
        .filter(|line| !line.is_empty())
        .map(|line| line.splitn(3, '\t').collect())
        .collect();
    eprintln!("fake server started");
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout();
    let configuration = concat!(
        r#"{"jsonrpc":"2.0","id":"configuration","#,
        r#""method":"workspace/configuration","params":{"items":[{}]}}"#
    );
    send(&mut out, configuration);
    while let Some(body) = read(&mut input) {
        let method = between(&body, "\"method\":\"", "\"");
        if method == "exit" {
            return;
        }
        let Some(id) = numeric_id(&body) else {
            continue;
        };
        let rule = rules.iter().find(|r| r[0] == method && body.contains(r[1]));
        let result = rule.map_or("null", |r| r[2]);
        if result == "hang" {
            eprintln!("fake server hangs on {method}");
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }
        send(
            &mut out,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
        );
    }
}

fn read(input: &mut impl BufRead) -> Option<String> {
    let mut length = 0;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body).ok()?;
    String::from_utf8(body).ok()
}

fn send(out: &mut impl Write, body: &str) {
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    out.flush().unwrap();
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let Some((_, rest)) = text.split_once(start) else {
        return "";
    };
    rest.split_once(end).map_or("", |(found, _)| found)
}

/// The numeric ID of a request; replies to the server's own request carry a string ID.
fn numeric_id(body: &str) -> Option<u64> {
    let (_, rest) = body.split_once("\"id\":")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
