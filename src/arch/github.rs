//! GitHub output for CI: workflow annotations from `model check` and a pull request comment
//! from `model footprint`. Both read the commands' JSON output, so they render exactly what the
//! JSON says.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

/// The hidden first line of the footprint comment, which CI looks for to update its comment.
pub const COMMENT_MARKER: &str = "<!-- boundrail:footprint -->";

/// The most rows one list of the comment shows; the rest are counted.
const ROWS_MAX: usize = 50;

/// Escapes `text` as workflow command data: `%`, carriage returns and line feeds.
fn command_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escapes `text` as a workflow command property value: the data escapes plus `:` and `,`.
fn command_property(text: &str) -> String {
    command_data(text).replace(':', "%3A").replace(',', "%2C")
}

/// One workflow command line per diagnostic of a `model check` JSON output: `warning` for a
/// warning, `notice` for information, with `file` and `line` when the diagnostic has them, and
/// the code as the title.
pub fn annotations(check: &Value) -> String {
    let mut out = String::new();
    for d in check["diagnostics"].as_array().into_iter().flatten() {
        let command = match d["severity"].as_str() {
            Some("warning") => "warning",
            _ => "notice",
        };
        let mut properties = Vec::new();
        if let Some(file) = d["file"].as_str() {
            properties.push(format!("file={}", command_property(file)));
            if let Some(line) = d["line"].as_u64() {
                properties.push(format!("line={line}"));
            }
        }
        let code = d["code"].as_str().unwrap_or_default();
        properties.push(format!("title={}", command_property(code)));
        let message = d["message"].as_str().unwrap_or_default();
        let _ = writeln!(
            out,
            "::{command} {}::{}",
            properties.join(","),
            command_data(message)
        );
    }
    out
}

/// The longest name or path the comment shows; a longer one ends in `…`.
const NAME_MAX: usize = 120;

/// The longest comment in bytes, below GitHub's limit of 65,536 characters.
const COMMENT_MAX: usize = 60_000;

/// The last line of a comment cut at [`COMMENT_MAX`].
const TRUNCATED: &str =
    "\n_Truncated at 60,000 characters; the job summary has the whole footprint._\n";

/// Whether `c` hides or reorders text: a control character, a zero-width or direction mark, or
/// a bidirectional embedding, override or isolate.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// Repository text on one line: hidden characters become spaces, and text longer than
/// [`NAME_MAX`] characters is cut, ending in `…`.
fn one_line(text: &str) -> String {
    let mut out: String = (text.chars().take(NAME_MAX))
        .map(|c| if hidden(c) { ' ' } else { c })
        .collect();
    if text.chars().count() > NAME_MAX {
        out.pop();
        out.push('…');
    }
    out
}

/// Repository text as a code span, so Markdown, HTML, mentions, references and links stay
/// literal. The fence is longer than any run of backticks in the text. `cell` escapes `|`, which
/// ends a table cell even inside a code span.
fn code(text: &str, cell: bool) -> String {
    let mut text = one_line(text);
    if text.is_empty() {
        return text;
    }
    if cell {
        text = text.replace('|', "\\|");
    }
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// Escapes a message for Markdown: no formatting, no HTML, no mentions, one line.
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '@' => out.push_str("&#64;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '#' | '~' => {
                out.push('\\');
                out.push(c);
            }
            c if hidden(c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// The name `names` gives a qualified ID, else the ID itself, as a table cell.
fn named(names: &BTreeMap<String, String>, id: &str) -> String {
    code(names.get(id).map_or(id, String::as_str), true)
}

/// A commit as the comment shows it: its first 12 characters.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// The array under `key`, empty when absent.
fn items<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    value[key].as_array().map_or(&[][..], Vec::as_slice)
}

/// "and N more" when `items` holds more than [`ROWS_MAX`] entries.
fn more(out: &mut String, items: &[Value], noun: &str) {
    if items.len() > ROWS_MAX {
        let _ = writeln!(out, "\nand {} more {noun}", items.len() - ROWS_MAX);
    }
}

/// A Markdown pull request comment from a `model footprint` JSON output: the modules touched,
/// the files outside every binding, the crossings with the relationships drawn for them or
/// "undrawn", why crossings were not checked, and what the branch changes in the model. `names`
/// maps qualified module and relationship IDs to how the comment shows them. The first line is
/// [`COMMENT_MARKER`]; a comment longer than [`COMMENT_MAX`] bytes is cut at a line and ends with
/// [`TRUNCATED`].
pub fn pr_comment(footprint: &Value, names: &BTreeMap<String, String>) -> String {
    let mut out = format!("{COMMENT_MARKER}\n### Architecture footprint\n\n");
    range_section(&mut out, footprint);
    modules_section(&mut out, items(footprint, "modules"));
    outside_section(&mut out, items(footprint, "outside"));
    crossings_section(&mut out, footprint, names);
    model_section(&mut out, footprint);
    if out.len() > COMMENT_MAX {
        let mut cut = COMMENT_MAX - TRUNCATED.len();
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(out[..cut].rfind('\n').map_or(cut, |line| line + 1));
        out.push_str(TRUNCATED);
    }
    out
}

/// The commits the footprint spans.
fn range_section(out: &mut String, footprint: &Value) {
    let base = &footprint["base"];
    let head = footprint["head"].as_str().map_or_else(
        || "the working tree".to_owned(),
        |h| format!("`{}`", short(h)),
    );
    let reference = base["ref"]
        .as_str()
        .map(|r| format!(" ({})", code(r, false)))
        .unwrap_or_default();
    let commit = base["commit"].as_str().unwrap_or_default();
    let _ = writeln!(out, "From `{}`{reference} to {head}.\n", short(commit));
}

/// The modules touched, as a table.
fn modules_section(out: &mut String, modules: &[Value]) {
    if modules.is_empty() {
        out.push_str("No bound module changed.\n\n");
        return;
    }
    out.push_str("| Module | Files | In parts |\n|---|---|---|\n");
    for m in modules.iter().take(ROWS_MAX) {
        let files = m["files"].as_array().map_or(0, Vec::len);
        let name = code(m["name"].as_str().unwrap_or_default(), true);
        let _ = writeln!(out, "| {name} | {files} | {} |", m["in_parts"]);
    }
    more(out, modules, "modules");
    out.push('\n');
}

/// The changed files no binding holds.
fn outside_section(out: &mut String, outside: &[Value]) {
    if outside.is_empty() {
        return;
    }
    let mut shown: Vec<String> = (outside.iter().take(ROWS_MAX))
        .map(|v| code(v.as_str().unwrap_or_default(), false))
        .collect();
    if outside.len() > ROWS_MAX {
        shown.push(format!("and {} more", outside.len() - ROWS_MAX));
    }
    let _ = writeln!(out, "Outside every binding: {}\n", shown.join(", "));
}

/// The crossings as a table, or why they were not checked.
fn crossings_section(out: &mut String, footprint: &Value, names: &BTreeMap<String, String>) {
    let Some(crossings) = footprint["crossings"].as_array() else {
        let reason = footprint["crossings_skipped"].as_str().unwrap_or("unknown");
        let error = footprint["scan_error"]
            .as_str()
            .map(|e| format!(": {}", md(e)))
            .unwrap_or_default();
        let _ = writeln!(out, "Crossings not checked: {reason}{error}\n");
        return;
    };
    if crossings.is_empty() {
        out.push_str("No crossings.\n\n");
        return;
    }
    out.push_str("| From | To | Edges | Hops | Drawn as |\n|---|---|---|---|---|\n");
    for c in crossings.iter().take(ROWS_MAX) {
        let lines = items(c, "relationships");
        let drawn = if lines.is_empty() {
            "undrawn".to_owned()
        } else {
            (lines.iter())
                .map(|r| named(names, r.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let from = named(names, c["from"].as_str().unwrap_or_default());
        let to = named(names, c["to"].as_str().unwrap_or_default());
        let (edges, hops) = (&c["edges"], &c["hops"]);
        let _ = writeln!(out, "| {from} | {to} | {edges} | {hops} | {drawn} |");
    }
    more(out, crossings, "crossings");
    out.push('\n');
}

/// What the branch changes in the model, per section, or why it was not compared.
fn model_section(out: &mut String, footprint: &Value) {
    if let Some(error) = footprint["model_error"].as_str() {
        let _ = writeln!(out, "Model changes not compared: {}", md(error));
        return;
    }
    if footprint["model"].is_null() {
        out.push_str("The base holds no model.\n");
        return;
    }
    let mut changed = false;
    for section in ["modules", "relationships", "collections", "flows", "steps"] {
        let changes = &footprint["model"][section];
        let count = |kind: &str| items(changes, kind).len();
        let (added, removed, edited) = (count("added"), count("removed"), count("changed"));
        if added + removed + edited > 0 {
            changed = true;
            let _ = writeln!(
                out,
                "- Model {section}: {added} added, {removed} removed, {edited} changed"
            );
        }
    }
    if !changed {
        out.push_str("No model changes.\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workflow_command_text_and_properties_are_escaped() {
        assert_eq!(command_data("100%\r\nnext"), "100%25%0D%0Anext");
        assert_eq!(command_property("a:b,c%\n"), "a%3Ab%2Cc%25%0A");
    }

    #[test]
    fn annotations_carry_file_and_line_when_the_diagnostic_has_them() {
        let check = json!({"diagnostics": [
            {"code": "RELATIONSHIP_UNDRAWN", "severity": "warning", "subjects": [],
             "message": "Billing uses Ledger but no relationship is drawn: src/a,b.ts:1 imports x\nmore",
             "file": "src/a,b.ts", "line": 1},
            {"code": "BINDING_PATH_MISSING", "severity": "warning", "subjects": [],
             "message": "Ghost is bound to src/ghost, which does not exist", "file": "src/ghost"},
            {"code": "ROOT_UNCONNECTED", "severity": "warning", "subjects": [],
             "message": "Bank: 100% alone"},
            {"code": "ENTRY_POINT_UNEXPOSED", "severity": "information", "subjects": [],
             "message": "deep import"},
        ]});
        assert_eq!(
            annotations(&check),
            "::warning file=src/a%2Cb.ts,line=1,title=RELATIONSHIP_UNDRAWN::Billing uses Ledger \
             but no relationship is drawn: src/a,b.ts:1 imports x%0Amore\n\
             ::warning file=src/ghost,title=BINDING_PATH_MISSING::Ghost is bound to src/ghost, \
             which does not exist\n\
             ::warning title=ROOT_UNCONNECTED::Bank: 100%25 alone\n\
             ::notice title=ENTRY_POINT_UNEXPOSED::deep import\n"
        );
        assert_eq!(annotations(&json!({"diagnostics": []})), "");
    }

    fn footprint(crossings: Value, skipped: Value, model: Value) -> Value {
        json!({
            "base": {"commit": "0123456789abcdef", "source": "explicit", "ref": "main"},
            "head": null,
            "modules": [
                {"module": "M#Billing1", "name": "Billing", "files": [], "in_parts": 1},
                {"module": "M#Fees0001", "name": "Fees_v2", "files": ["src/billing/fee.ts"], "in_parts": 0},
            ],
            "outside": ["NOTES.md"],
            "crossings": crossings,
            "crossings_skipped": skipped,
            "model": model,
            "model_error": null,
            "scan_error": null,
        })
    }

    fn names() -> BTreeMap<String, String> {
        [
            ("M#WebApp01", "Web"),
            ("M#Fees0001", "Fees_v2"),
            ("M#RelWeb01", "Web -> Billing [http]"),
        ]
        .into_iter()
        .map(|(id, name)| (id.to_owned(), name.to_owned()))
        .collect()
    }

    #[test]
    fn the_comment_lists_modules_crossings_and_model_changes() {
        let crossings = json!([
            {"from": "M#WebApp01", "to": "M#Fees0001", "edges": 1, "hops": 0, "relationships": ["M#RelWeb01"]},
            {"from": "M#Fees0001", "to": "M#Other001", "edges": 2, "hops": 1, "relationships": []},
        ]);
        let model = json!({
            "modules": {"added": [{"id": "M#Fees0001", "name": "Fees"}], "removed": [], "changed": []},
            "relationships": {"added": [], "removed": [], "changed": [{"id": "M#RelWeb01", "fields": ["label"]}]},
            "collections": {"added": [], "removed": [], "changed": []},
            "flows": {"added": [], "removed": [], "changed": []},
            "steps": {"added": [], "removed": [], "changed": []},
        });
        let comment = pr_comment(&footprint(crossings, Value::Null, model), &names());
        assert_eq!(
            comment,
            "<!-- boundrail:footprint -->\n### Architecture footprint\n\n\
             From `0123456789ab` (`main`) to the working tree.\n\n\
             | Module | Files | In parts |\n|---|---|---|\n\
             | `Billing` | 0 | 1 |\n\
             | `Fees_v2` | 1 | 0 |\n\n\
             Outside every binding: `NOTES.md`\n\n\
             | From | To | Edges | Hops | Drawn as |\n|---|---|---|---|---|\n\
             | `Web` | `Fees_v2` | 1 | 0 | `Web -> Billing [http]` |\n\
             | `Fees_v2` | `M#Other001` | 2 | 1 | undrawn |\n\n\
             - Model modules: 1 added, 0 removed, 0 changed\n\
             - Model relationships: 0 added, 0 removed, 1 changed\n"
        );
    }

    #[test]
    fn unchecked_crossings_name_their_reason_and_an_empty_footprint_says_so() {
        let mut skipped = footprint(Value::Null, json!("scan-failed"), Value::Null);
        skipped["scan_error"] = json!("collector <crashed>");
        let comment = pr_comment(&skipped, &BTreeMap::new());
        assert!(
            comment.contains("Crossings not checked: scan-failed: collector &lt;crashed&gt;\n"),
            "{comment}"
        );
        assert!(comment.ends_with("The base holds no model.\n"), "{comment}");

        let empty = json!({
            "base": {"commit": "abc", "source": "head-fallback", "ref": null},
            "head": "fedcba9876543210",
            "modules": [], "outside": [], "crossings": null,
            "crossings_skipped": "not-needed",
            "model": {"modules": {"added": [], "removed": [], "changed": []}},
            "model_error": null, "scan_error": null,
        });
        assert_eq!(
            pr_comment(&empty, &BTreeMap::new()),
            "<!-- boundrail:footprint -->\n### Architecture footprint\n\n\
             From `abc` to `fedcba987654`.\n\n\
             No bound module changed.\n\n\
             Crossings not checked: not-needed\n\n\
             No model changes.\n"
        );
    }

    #[test]
    fn messages_cannot_format_or_mention() {
        assert_eq!(md("@team <b>|x`\n\u{202E}"), "&#64;team &lt;b&gt;\\|x\\`  ");
    }

    /// A one-module footprint named `name`, with no crossings checked.
    fn named_module(name: &str) -> Value {
        let mut value = footprint(Value::Null, json!("not-needed"), Value::Null);
        value["modules"] =
            json!([{"module": "M#A0000001", "name": name, "files": ["a"], "in_parts": 0}]);
        value
    }

    /// The module row of the comment for a module named `name`.
    fn module_row(name: &str) -> String {
        let comment = pr_comment(&named_module(name), &BTreeMap::new());
        let row = comment.lines().find(|l| l.ends_with("| 1 | 0 |")).unwrap();
        row.to_owned()
    }

    #[test]
    fn names_render_as_code_spans_that_mention_reference_and_link_nothing() {
        assert_eq!(module_row("@team"), "| `@team` | 1 | 0 |");
        assert_eq!(module_row("org/repo#1"), "| `org/repo#1` | 1 | 0 |");
        assert_eq!(
            module_row("https://example.com/x"),
            "| `https://example.com/x` | 1 | 0 |"
        );
        assert_eq!(module_row("a|b"), "| `a\\|b` | 1 | 0 |");
        // The fence is longer than any backtick run, padded when the text starts with one.
        assert_eq!(module_row("x `y` ``z"), "| ```x `y` ``z``` | 1 | 0 |");
        assert_eq!(module_row("`edge"), "| `` `edge `` | 1 | 0 |");
    }

    #[test]
    fn hidden_characters_become_spaces_and_long_names_are_cut() {
        let bidi = "admin\u{202E}txt.exe\u{2066}\u{200B}";
        assert_eq!(module_row(bidi), "| `admin txt.exe  ` | 1 | 0 |");
        let long = "n".repeat(200);
        let row = module_row(&long);
        assert_eq!(row, format!("| `{}…` | 1 | 0 |", "n".repeat(119)));
    }

    #[test]
    fn an_overlong_comment_is_cut_at_a_line_and_says_so() {
        let names: BTreeMap<String, String> = (0..50)
            .map(|i| (format!("M#R{i:07}"), "r".repeat(120)))
            .collect();
        let crossings: Vec<Value> = (0..50)
            .map(|i| {
                let lines: Vec<String> = names.keys().cloned().collect();
                json!({"from": format!("M#F{i:07}"), "to": "M#T0000001", "edges": 1, "hops": 0, "relationships": lines})
            })
            .collect();
        let value = footprint(json!(crossings), Value::Null, Value::Null);
        let comment = pr_comment(&value, &names);
        assert!(comment.len() <= COMMENT_MAX, "{}", comment.len());
        assert!(comment.starts_with(COMMENT_MARKER));
        assert!(
            comment.ends_with(TRUNCATED),
            "{}",
            &comment[comment.len() - 200..]
        );
        let body = &comment[..comment.len() - TRUNCATED.len()];
        assert!(body.ends_with("|\n"), "cut inside a row");
    }
}
