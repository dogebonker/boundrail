//! What goes over the wire, mapped to and from the checkout: `file:` URIs, symbol and reference
//! results as repository paths, and positions in a file's text.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// The extension of the repository path `file`, without the dot; empty without one.
pub(super) fn extension(file: &str) -> &str {
    let name = file.rsplit('/').next().unwrap_or(file);
    name.rsplit_once('.').map_or("", |(_, extension)| extension)
}

/// The checkout as the server sees it (canonical) and as the caller named it: a URI inside
/// either maps to a repository path.
#[derive(Clone, Copy)]
pub(super) struct Roots<'a> {
    pub(super) canonical: &'a Path,
    pub(super) given: &'a Path,
}

/// A declaration's range: its 0-based start line and UTF-16 column, and its end line.
pub(super) type Range = (u64, u64, u64);

/// A symbol a server declared, with its range.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Declaration {
    pub(super) file: String,
    pub(super) name: String,
    pub(super) range: Range,
}

/// The declarations in a `workspace/symbol` result (`SymbolInformation` or `WorkspaceSymbol`)
/// that lie inside the checkout.
pub(super) fn declarations(result: &Value, roots: Roots<'_>) -> Vec<Declaration> {
    let items = result.as_array().map_or(&[][..], Vec::as_slice);
    (items.iter())
        .filter_map(|item| {
            let location = &item["location"];
            let file = relative(location["uri"].as_str()?, roots)?;
            let (start, end) = (&location["range"]["start"], &location["range"]["end"]);
            let start_line = start["line"].as_u64().unwrap_or(0);
            let range = (
                start_line,
                start["character"].as_u64().unwrap_or(0),
                end["line"].as_u64().unwrap_or(start_line),
            );
            let name = item["name"].as_str()?.to_owned();
            Some(Declaration { file, name, range })
        })
        .collect()
}

/// The names a `textDocument/documentSymbol` result declares at the top of its file: every
/// `DocumentSymbol`, without its children, or every `SymbolInformation` without a container.
pub(super) fn top_level_names(result: &Value) -> Vec<&str> {
    let items = result.as_array().map_or(&[][..], Vec::as_slice);
    (items.iter())
        .filter(|item| {
            item.get("location").is_none()
                || item["containerName"].as_str().is_none_or(str::is_empty)
        })
        .filter_map(|item| item["name"].as_str())
        .collect()
}

/// The locations in a `textDocument/references` result inside the checkout, as repository path
/// and 1-based line.
pub(super) fn locations(result: &Value, roots: Roots<'_>) -> Vec<(String, u64)> {
    let items = result.as_array().map_or(&[][..], Vec::as_slice);
    (items.iter())
        .filter_map(|item| {
            let file = relative(item["uri"].as_str()?, roots)?;
            let line = item["range"]["start"]["line"].as_u64()?;
            Some((file, line.checked_add(1)?))
        })
        .collect()
}

/// A file's text with where each line starts, indexed once.
pub(super) struct Text {
    text: String,
    starts: Vec<usize>,
}

impl Text {
    pub(super) fn new(text: String) -> Self {
        let breaks = text.match_indices('\n').map(|(at, _)| at + 1);
        let starts = std::iter::once(0).chain(breaks).collect();
        Self { text, starts }
    }

    /// The 0-based line `n`, without its line end.
    fn line(&self, n: u64) -> Option<&str> {
        let n = usize::try_from(n).ok()?;
        let start = *self.starts.get(n)?;
        let end = self
            .starts
            .get(n + 1)
            .map_or(self.text.len(), |&next| next - 1);
        Some(self.text[start..end].trim_end_matches('\r'))
    }
}

/// Where `name` is written in `text` within `range`: its first whole-word occurrence from the
/// range's start, as 0-based line and UTF-16 column; else the range's start. A declaration's
/// range may begin at a keyword, and references are asked at the name.
pub(super) fn name_position(text: &Text, name: &str, (line, column, end): Range) -> (u64, u64) {
    if name.is_empty() {
        return (line, column);
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    for at in line..=end {
        let Some(text) = text.line(at) else {
            break;
        };
        let mut search = if at == line { byte_at(text, column) } else { 0 };
        while let Some(found) = text[search..].find(name) {
            let start = search + found;
            let end = start + name.len();
            let before = text[..start].chars().next_back().is_none_or(|c| !word(c));
            let after = text[end..].chars().next().is_none_or(|c| !word(c));
            if before && after {
                let units = text[..start].encode_utf16().count();
                return (at, u64::try_from(units).unwrap_or(u64::MAX));
            }
            search = end;
        }
    }
    (line, column)
}

/// The byte offset of the UTF-16 `column` in `line`, clamped to its end.
fn byte_at(line: &str, column: u64) -> usize {
    let mut units = 0;
    for (at, c) in line.char_indices() {
        if units >= column {
            return at;
        }
        units += c.len_utf16() as u64;
    }
    line.len()
}

/// The repository path of a `file:` URI inside the checkout; `None` outside it or when a
/// component is anything but a plain name, such as `..`.
pub(super) fn relative(uri: &str, roots: Roots<'_>) -> Option<String> {
    let path = uri_path(uri)?;
    let inside = (path.strip_prefix(roots.canonical))
        .or_else(|_| path.strip_prefix(roots.given))
        .ok()?;
    let parts = (inside.components())
        .map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<Vec<String>>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The `file:` URI of an absolute path, every byte but unreserved ones and `/` percent-encoded.
pub(super) fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The path of a `file:` URI, percent-decoded.
fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex = [input.next()?, input.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    // `file:///C:/x` names `C:/x` on Windows.
    let windows = cfg!(windows) && text.as_bytes().get(2) == Some(&b':');
    Some(PathBuf::from(if windows { &text[1..] } else { &text }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo() -> Roots<'static> {
        Roots {
            canonical: Path::new("/private/repo"),
            given: Path::new("/repo"),
        }
    }

    #[test]
    fn uris_round_trip_and_map_to_repository_paths() {
        let path = Path::new("/repo/my dir/src/ä.ts");
        let uri = file_uri(path);
        assert_eq!(uri, "file:///repo/my%20dir/src/%C3%A4.ts");
        assert_eq!(uri_path(&uri).unwrap(), path);
        assert_eq!(relative(&uri, repo()).as_deref(), Some("my dir/src/ä.ts"));
        assert_eq!(
            relative("file:///private/repo/a.ts", repo()).as_deref(),
            Some("a.ts")
        );
        assert_eq!(relative("file:///elsewhere/a.ts", repo()), None);
        assert_eq!(relative("file:///repo", repo()), None);
        assert_eq!(relative("file:///repo/src/../../etc/x", repo()), None);
        assert_eq!(relative("https://repo/a.ts", repo()), None);
        assert_eq!(uri_path("file:///a%2"), None);
    }

    #[test]
    fn symbol_and_reference_results_map_to_files_and_lines() {
        let range = |line: u64, character: u64, end: u64| json!({"start": {"line": line, "character": character}, "end": {"line": end, "character": 0}});
        let symbols = json!([
            {"name": "charge", "kind": 12, "location": {"uri": "file:///repo/src/a.ts", "range": range(3, 0, 5)}},
            {"name": "pay", "location": {"uri": "file:///repo/b.ts"}},
            {"name": "out", "location": {"uri": "file:///other/c.ts", "range": range(0, 0, 0)}},
        ]);
        let declaration = |file: &str, name: &str, range| Declaration {
            file: file.into(),
            name: name.into(),
            range,
        };
        assert_eq!(
            declarations(&symbols, repo()),
            [
                declaration("src/a.ts", "charge", (3, 0, 5)),
                declaration("b.ts", "pay", (0, 0, 0)),
            ]
        );
        assert_eq!(declarations(&Value::Null, repo()), []);
        let references = json!([
            {"uri": "file:///repo/src/web.ts", "range": range(1, 4, 1)},
            {"uri": "file:///repo/src/x.ts"},
            {"uri": "file:///other/y.ts", "range": range(0, 0, 0)},
        ]);
        assert_eq!(
            locations(&references, repo()),
            [("src/web.ts".to_owned(), 2)]
        );

        let hierarchical =
            json!([{"name": "charge", "children": [{"name": "inner"}]}, {"name": "pay"}]);
        assert_eq!(top_level_names(&hierarchical), ["charge", "pay"]);
        let flat = json!([
            {"name": "charge", "location": {}},
            {"name": "inner", "containerName": "charge", "location": {}},
            {"name": "pay", "containerName": "", "location": {}},
        ]);
        assert_eq!(top_level_names(&flat), ["charge", "pay"]);
    }

    #[test]
    fn references_are_asked_at_the_declared_name() {
        let text = Text::new(
            "// charge\r\nexport const recharge = 1, charge = (n) => n;\nconst é = charge;\n"
                .into(),
        );
        assert_eq!(name_position(&text, "charge", (1, 0, 1)), (1, 27));
        // A column past a multibyte character counts UTF-16 units.
        assert_eq!(name_position(&text, "charge", (2, 2, 2)), (2, 10));
        assert_eq!(name_position(&text, "missing", (1, 3, 2)), (1, 3));
        assert_eq!(name_position(&text, "", (1, 3, 2)), (1, 3));
        assert_eq!(
            name_position(&Text::new(String::new()), "x", (9, 1, 9)),
            (9, 1)
        );
        assert_eq!(text.line(0), Some("// charge"));
        assert_eq!(text.line(4), None);
    }
}
