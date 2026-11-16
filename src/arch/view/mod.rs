//! The live browser view's pure parts: the page it serves, and the parsing and checks every
//! request to `boundrail model view` passes. The server itself lives with the CLI, which owns
//! the command outputs it answers with.

use std::io::{self, Read};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const INDEX_HTML: &str = include_str!("index.html");
pub const APP_JS: &str = include_str!("app.js");
pub const STYLE_CSS: &str = include_str!("style.css");
/// Geist, the page's one typeface: a variable font under the SIL Open Font License, whose text
/// sits beside it in `geist-OFL.txt`.
pub const FONT_WOFF2: &[u8] = include_bytes!("geist.woff2");

/// The most a request line and its headers may take.
pub const HEAD_MAX: usize = 16 * 1024;
/// How long a whole request head may take to arrive, however slowly it drips in.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// The header an API request may carry its token in, instead of the `t` query parameter.
pub const TOKEN_HEADER: &str = "x-boundrail-token";

/// What the page may load: its own script, style and font, and requests to its own server. SVG
/// from `/api/render` is imported as nodes and styled by presentation attributes, which the
/// policy does not govern.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; \
     style-src 'self'; font-src 'self'; connect-src 'self'; img-src 'self'; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// The policy of an SVG opened on its own: no scripts or loads, only its own styling.
pub const RENDER_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// A request line and headers. Nothing after the head is read: GET requests carry no body, and
/// `POST /api/explain` takes its parameters in the query too.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Decoded query parameters in order.
    pub query: Vec<(String, String)>,
    /// Header names in ASCII lowercase, values trimmed.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The first value of header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        (self.headers.iter())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first value of query parameter `name`, or `None` when it is absent or empty.
    pub fn param(&self, name: &str) -> Option<&str> {
        (self.query.iter())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

/// Why a request was not parsed.
#[derive(Debug, PartialEq, Eq)]
pub enum Bad {
    /// The head is longer than [`HEAD_MAX`]: 431.
    TooLarge,
    /// Not an HTTP/1 request head, or the connection closed first: 400.
    Malformed,
    /// The head did not arrive by its deadline: 408.
    Timeout,
}

impl Bad {
    pub fn status(&self) -> u16 {
        match self {
            Bad::TooLarge => 431,
            Bad::Malformed => 400,
            Bad::Timeout => 408,
        }
    }
}

/// Reads one request head from `stream` by `deadline` and parses it. Before each read, `wait`
/// limits how long that read may block to the time left, so a head that drips in slowly is cut
/// off at the deadline rather than after one timeout per byte.
pub fn read_request<S: Read>(
    stream: &mut S,
    deadline: Instant,
    mut wait: impl FnMut(&mut S, Duration) -> io::Result<()>,
) -> Result<Request, Bad> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Bad::Timeout);
        }
        wait(stream, left).map_err(|_| Bad::Malformed)?;
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Bad::Timeout);
            }
            Err(_) => return Err(Bad::Malformed),
        };
        if n == 0 {
            return Err(Bad::Malformed);
        }
        // Search only where the new bytes could complete the terminator.
        let from = head.len().saturating_sub(3);
        head.extend_from_slice(&chunk[..n]);
        if let Some(end) = find(&head[from..], b"\r\n\r\n") {
            head.truncate(from + end);
            return parse(&head);
        }
        if head.len() > HEAD_MAX {
            return Err(Bad::TooLarge);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parses a request head without its final blank line.
pub fn parse(head: &[u8]) -> Result<Request, Bad> {
    if head.len() > HEAD_MAX {
        return Err(Bad::TooLarge);
    }
    let text = std::str::from_utf8(head).map_err(|_| Bad::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (line.next(), line.next(), line.next(), line.next())
    else {
        return Err(Bad::Malformed);
    };
    if method.is_empty() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(Bad::Malformed);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = (query.split('&').filter(|pair| !pair.is_empty()))
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((decode(name)?, decode(value)?))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Bad::Malformed)?;
    let headers = (lines.filter(|line| !line.is_empty()))
        .map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.to_ascii_lowercase();
            let token = !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic());
            token.then(|| (name, value.trim().to_owned()))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Bad::Malformed)?;
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query,
        headers,
    })
}

/// A query component with `+` as a space and `%XX` escapes decoded, or `None` when an escape is
/// broken or the bytes are not UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Whether the request names this server by its loopback address or `localhost` with its port,
/// in exactly one Host header. Another name means a page on some other site resolved its own
/// name to this machine (DNS rebinding).
pub fn host_allowed(request: &Request, port: u16) -> bool {
    let mut hosts = request.headers.iter().filter(|(n, _)| n == "host");
    let (Some((_, host)), None) = (hosts.next(), hosts.next()) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// Whether the request carries `token` in the [`TOKEN_HEADER`], or, for a GET, in the `t` query
/// parameter. A POST must use the header, which a cross-site form cannot set and a cross-site
/// script cannot send without a preflight.
pub fn token_matches(request: &Request, token: &str) -> bool {
    let query = (request.method == "GET")
        .then(|| request.param("t"))
        .flatten();
    let given = query.or_else(|| request.header(TOKEN_HEADER));
    given.is_some_and(|given| same(given.as_bytes(), token.as_bytes()))
}

/// Whether the request has no Origin header, or exactly one naming this server by its loopback
/// address or `localhost` with its port.
pub fn origin_allowed(request: &Request, port: u16) -> bool {
    let mut origins = request.headers.iter().filter(|(n, _)| n == "origin");
    match (origins.next(), origins.next()) {
        (None, _) => true,
        (Some((_, origin)), None) => {
            let origin = origin.to_ascii_lowercase();
            origin == format!("http://127.0.0.1:{port}")
                || origin == format!("http://localhost:{port}")
        }
        (Some(_), Some(_)) => false,
    }
}

/// Compares in time independent of where the inputs differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// A one-time code the browser opener gets in place of the token, so the token never appears in
/// a process's arguments. The first request naming it redeems it; after that it is spent. The
/// code is still visible to other local users while the opener runs, so a spent code presented
/// again means someone else may have redeemed it first.
pub struct Launch(Mutex<(String, bool)>);

/// What presenting a launch code found.
#[derive(Debug, PartialEq, Eq)]
pub enum Redeem {
    /// The unspent code, now spent.
    Fresh,
    /// The code, already spent by an earlier request.
    Spent,
    /// Not the code.
    Unknown,
}

impl Launch {
    /// A new code from the operating system's random source.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(Mutex::new((crate::token()?, false))))
    }

    /// The code while it is unspent.
    pub fn code(&self) -> Option<String> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (!state.1).then(|| state.0.clone())
    }

    /// Spends `given` when it is the unspent code.
    pub fn redeem(&self, given: Option<&str>) -> Redeem {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !given.is_some_and(|given| same(state.0.as_bytes(), given.as_bytes())) {
            Redeem::Unknown
        } else if state.1 {
            Redeem::Spent
        } else {
            state.1 = true;
            Redeem::Fresh
        }
    }
}

/// A complete response that closes the connection. Every response forbids caching and sniffing.
pub fn response(status: u16, content_type: &str, body: &[u8], extra: &[(&str, &str)]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n",
        reason(status),
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        403 => "Forbidden",
        408 => "Request Timeout",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(head: &str) -> Request {
        parse(head.as_bytes()).unwrap()
    }

    #[test]
    fn parses_the_request_line_query_and_headers() {
        let r = request(
            "GET /api/render?view=sequence&flow=Check%20out&scope=&t=ab+c HTTP/1.1\r\nHost: 127.0.0.1:80\r\nX-Boundrail-Token:  tok \r\n",
        );
        assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/api/render"));
        assert_eq!(r.param("flow"), Some("Check out"));
        assert_eq!(r.param("t"), Some("ab c"));
        assert_eq!(r.param("scope"), None, "an empty value counts as absent");
        assert_eq!(r.param("depth"), None);
        assert_eq!(r.header("host"), Some("127.0.0.1:80"));
        assert_eq!(r.header(TOKEN_HEADER), Some("tok"));
    }

    #[test]
    fn refuses_malformed_heads() {
        for head in [
            "",
            "GET /\r\n",
            "GET / HTTP/1.1 extra\r\n",
            "GET relative HTTP/1.1\r\n",
            "GET / SPDY/3\r\n",
            "GET /?q=%zz HTTP/1.1\r\n",
            "GET /?q=%+1 HTTP/1.1\r\n",
            "GET /?q=%c3 HTTP/1.1\r\n",
            "GET / HTTP/1.1\r\nno colon\r\n",
            "GET / HTTP/1.1\r\nbad name: x\r\n",
        ] {
            assert_eq!(
                parse(head.as_bytes()).unwrap_err(),
                Bad::Malformed,
                "{head:?}"
            );
        }
    }

    fn read(stream: &mut impl Read) -> Result<Request, Bad> {
        read_request(stream, Instant::now() + HEAD_DEADLINE, |_, _| Ok(()))
    }

    #[test]
    fn reads_a_head_up_to_the_blank_line_and_caps_its_size() {
        let mut stream = &b"GET /a HTTP/1.1\r\nHost: x\r\n\r\nignored body"[..];
        assert_eq!(read(&mut stream).unwrap().path, "/a");
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(HEAD_MAX));
        let mut stream = long.as_bytes();
        assert_eq!(read(&mut stream).unwrap_err(), Bad::TooLarge);
        let mut closed = &b"GET / HTTP/1.1\r\n"[..];
        assert_eq!(read(&mut closed).unwrap_err(), Bad::Malformed);
    }

    /// Hands out one byte per read, each after a pause.
    struct Drip(&'static [u8]);

    impl Read for Drip {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buf[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn a_head_that_drips_in_is_cut_off_at_the_deadline() {
        let start = Instant::now();
        let mut waits = Vec::new();
        let mut drip = Drip(b"GET /a-long-path-that-takes-a-while HTTP/1.1\r\nHost: x\r\n\r\n");
        let deadline = start + Duration::from_millis(150);
        let out = read_request(&mut drip, deadline, |_, left| {
            waits.push(left);
            Ok(())
        });
        assert_eq!(out.unwrap_err(), Bad::Timeout);
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "{:?}",
            start.elapsed()
        );
        // Each read may wait only for what is left of the whole head's time.
        assert!(waits.windows(2).all(|w| w[1] < w[0]), "{waits:?}");
        assert!(waits.iter().all(|w| *w <= Duration::from_millis(150)));
        let mut fast = Drip(b"GET / HTTP/1.1\r\n\r\n");
        assert_eq!(read(&mut fast).unwrap().path, "/");
    }

    #[test]
    fn only_the_loopback_host_and_localhost_on_this_port_pass() {
        let with = |hosts: &[&str]| {
            let lines: String = hosts.iter().map(|h| format!("Host: {h}\r\n")).collect();
            request(&format!("GET / HTTP/1.1\r\n{lines}"))
        };
        assert!(host_allowed(&with(&["127.0.0.1:4000"]), 4000));
        assert!(host_allowed(&with(&["LocalHost:4000"]), 4000));
        for hosts in [
            &["127.0.0.1:4001"][..],
            &["127.0.0.1"],
            &["evil.example:4000"],
            &["localhost.evil.example:4000"],
            &[],
            &["127.0.0.1:4000", "evil.example:4000"],
        ] {
            assert!(!host_allowed(&with(hosts), 4000), "{hosts:?}");
        }
    }

    #[test]
    fn only_no_origin_or_this_servers_own_origin_passes() {
        let with = |origins: &[&str]| {
            let lines: String = origins.iter().map(|o| format!("Origin: {o}\r\n")).collect();
            request(&format!("POST /api/explain HTTP/1.1\r\n{lines}"))
        };
        for origins in [
            &[][..],
            &["http://127.0.0.1:4000"],
            &["http://LocalHost:4000"],
        ] {
            assert!(origin_allowed(&with(origins), 4000), "{origins:?}");
        }
        for origins in [
            &["https://evil.example"][..],
            &["http://127.0.0.1:4001"],
            &["https://127.0.0.1:4000"],
            &["null"],
            &["http://127.0.0.1:4000", "http://127.0.0.1:4000"],
        ] {
            assert!(!origin_allowed(&with(origins), 4000), "{origins:?}");
        }
    }

    #[test]
    fn a_launch_code_redeems_once_and_is_never_the_token() {
        let token = crate::token().unwrap();
        let launch = Launch::new().unwrap();
        let code = launch.code().unwrap();
        assert_eq!(code.len(), 64);
        assert_ne!(code, token);
        assert_eq!(launch.redeem(None), Redeem::Unknown);
        assert_eq!(launch.redeem(Some(&code[1..])), Redeem::Unknown);
        assert_eq!(launch.redeem(Some(&code)), Redeem::Fresh);
        assert_eq!(
            launch.redeem(Some(&code)),
            Redeem::Spent,
            "a spent code does not redeem again and is told apart from a wrong one"
        );
        assert_eq!(launch.redeem(Some("wrong")), Redeem::Unknown);
        assert_eq!(launch.code(), None);
    }

    #[test]
    fn the_token_passes_in_the_query_or_the_header_only_when_equal() {
        let token = crate::token().unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(token, crate::token().unwrap());
        let query = request(&format!("GET /api/model?t={token} HTTP/1.1\r\n"));
        assert!(token_matches(&query, &token));
        let header = request(&format!(
            "GET /api/model HTTP/1.1\r\n{TOKEN_HEADER}: {token}\r\n"
        ));
        assert!(token_matches(&header, &token));
        let posted = request(&format!("POST /api/explain?t={token} HTTP/1.1\r\n"));
        assert!(
            !token_matches(&posted, &token),
            "a POST takes the header only"
        );
        let posted = request(&format!(
            "POST /api/explain HTTP/1.1\r\n{TOKEN_HEADER}: {token}\r\n"
        ));
        assert!(token_matches(&posted, &token));
        for head in [
            "GET /api/model HTTP/1.1\r\n".to_owned(),
            "GET /api/model?t= HTTP/1.1\r\n".to_owned(),
            format!("GET /api/model?t={} HTTP/1.1\r\n", &token[1..]),
            format!("GET /api/model?t={}x HTTP/1.1\r\n", &token[1..]),
        ] {
            assert!(!token_matches(&request(&head), &token), "{head}");
        }
    }

    #[test]
    fn responses_close_and_forbid_caching() {
        let out =
            String::from_utf8(response(405, "text/plain", b"no", &[("Allow", "GET")])).unwrap();
        assert!(
            out.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
            "{out}"
        );
        for line in [
            "Content-Length: 2",
            "Connection: close",
            "Cache-Control: no-store",
            "Allow: GET",
        ] {
            assert!(out.contains(&format!("{line}\r\n")), "{out}");
        }
        assert!(out.ends_with("\r\n\r\nno"));
    }
}
