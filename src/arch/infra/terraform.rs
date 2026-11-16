//! Terraform resources and the references between them.

use std::collections::HashMap;
use std::ops::Range;

use super::{End, Infra, UnitKind, Var};
use crate::arch::types::Evidence;
use crate::modules::dir_of;

/// Reference roots that name no resource.
const ROOTS: [&str; 9] = [
    "var",
    "local",
    "module",
    "data",
    "each",
    "count",
    "path",
    "terraform",
    "self",
];

/// A Terraform resource: `TYPE.NAME`, and each `TYPE.NAME` reference in its body as a variable
/// named after the attribute holding it.
#[derive(Debug)]
pub(super) struct Resource {
    name: String,
    refs: Vec<Var>,
}

/// The resources of the Terraform file `file`.
// ponytail: a hand reader of `resource "TYPE" "NAME" { ... }` blocks over masked text, not an
// HCL parser: heredoc interpolation is ignored, and modules, variables and locals are not
// followed beyond literal references. Use an HCL parser if real modules need more.
pub(super) fn parse(file: &str, text: &str) -> Vec<Resource> {
    let masked = mask(text);
    let mut resources = Vec::new();
    let (mut depth, mut header, mut open) = (0usize, 0usize, None);
    let mut lines = LineCounter::default();
    for (at, byte) in masked.iter().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    open = resource_header(&masked[header..at], &text[header..at]).map(|n| (n, at));
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some((name, start)) = open.take() {
                        let refs = references(file, text, &masked, start..at, &mut lines);
                        resources.push(Resource { name, refs });
                    }
                    header = at + 1;
                }
            }
            b'\n' if depth == 0 => header = at + 1,
            _ => {}
        }
    }
    resources
}

/// The 1-based line of increasing offsets, counted from where the last one was.
#[derive(Default)]
struct LineCounter {
    at: usize,
    line: usize,
}

impl LineCounter {
    fn line(&mut self, bytes: &[u8], at: usize) -> usize {
        self.line += bytes[self.at..at].iter().filter(|&&b| b == b'\n').count();
        self.at = at;
        self.line + 1
    }
}

/// `TYPE.NAME` of a `resource "TYPE" "NAME"` block header, read from the original text between
/// the quotes the masked header keeps.
fn resource_header(masked: &[u8], text: &str) -> Option<String> {
    let rest = masked.trim_ascii_start().strip_prefix(b"resource")?;
    if !rest.first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let quotes: Vec<usize> = (masked.iter().enumerate())
        .filter(|(_, b)| **b == b'"')
        .map(|(at, _)| at)
        .collect();
    let [a, b, c, d] = quotes[..] else {
        return None;
    };
    Some(format!("{}.{}", &text[a + 1..b], &text[c + 1..d]))
}

/// Each `TYPE.NAME` reference in `range` of the masked text, as a variable named after the
/// attribute assigned on its line, or after itself. Tokens start and end at ASCII bytes, so
/// every slice falls on a character boundary.
fn references(
    file: &str,
    text: &str,
    masked: &[u8],
    range: Range<usize>,
    lines: &mut LineCounter,
) -> Vec<Var> {
    let ident = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.');
    let mut refs = Vec::new();
    let mut at = range.start;
    while at < range.end {
        if !ident(masked[at]) {
            at += 1;
            continue;
        }
        let end = (at..range.end)
            .find(|&i| !ident(masked[i]))
            .unwrap_or(range.end);
        let parts: Vec<&str> = text[at..end].split('.').collect();
        let named = parts.len() >= 2 && !parts[1].is_empty();
        if named && masked[at].is_ascii_alphabetic() && !ROOTS.contains(&parts[0]) {
            let reference = format!("{}.{}", parts[0], parts[1]);
            refs.push(Var {
                name: attribute(text, masked, at).unwrap_or(&reference).to_owned(),
                value: reference,
                file: file.to_owned(),
                line: lines.line(masked, at),
            });
        }
        at = end;
    }
    refs
}

/// The attribute assigned on the line of offset `at`: the name before the last `=` of the
/// masked line before it, so an `=` inside a string never counts.
fn attribute<'t>(text: &'t str, masked: &[u8], at: usize) -> Option<&'t str> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let line = masked[..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |n| n + 1);
    let equals = line + masked[line..at].iter().rposition(|&b| b == b'=')?;
    let end = (line..equals)
        .rev()
        .find(|&i| !masked[i].is_ascii_whitespace())?
        + 1;
    let start = (line..end)
        .rev()
        .find(|&i| !word(masked[i]))
        .map_or(line, |i| i + 1);
    (start < end).then(|| &text[start..end])
}

#[derive(Clone, Copy)]
enum Mask {
    Code,
    String,
    /// Inside `${...}` of a string, at this brace depth.
    Interpolation(usize),
    /// A string inside an interpolation, at its depth.
    Inner(usize),
    Line,
    Block,
}

/// `text` with comments, heredoc bodies and string contents outside `${...}` replaced by
/// spaces, byte for byte, so offsets and line numbers match the original. Quotes stay; strings
/// inside an interpolation are blanked whole.
fn mask(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let (mut state, mut at) = (Mask::Code, 0);
    while at < bytes.len() {
        let (byte, next) = (bytes[at], bytes.get(at + 1).copied());
        (state, at) = match state {
            Mask::Code => match (byte, next) {
                (b'#', _) | (b'/', Some(b'/')) => (Mask::Line, blank(&mut out, at, 1)),
                (b'/', Some(b'*')) => (Mask::Block, blank(&mut out, at, 2)),
                (b'"', _) => (Mask::String, at),
                (b'<', Some(b'<')) => (Mask::Code, heredoc(bytes, at, &mut out)),
                _ => (Mask::Code, at),
            },
            Mask::Line if byte == b'\n' => (Mask::Code, at),
            Mask::Block if (byte, next) == (b'*', Some(b'/')) => {
                (Mask::Code, blank(&mut out, at, 2))
            }
            Mask::Line | Mask::Block => (state, blank(&mut out, at, 1)),
            Mask::String => match (byte, next) {
                (b'\\', _) => (Mask::String, blank(&mut out, at, 2)),
                (b'"', _) => (Mask::Code, at),
                (b'$', Some(b'{')) => (Mask::Interpolation(1), at + 1),
                _ => (Mask::String, blank(&mut out, at, 1)),
            },
            Mask::Interpolation(depth) => match byte {
                b'{' => (Mask::Interpolation(depth + 1), at),
                b'}' if depth == 1 => (Mask::String, at),
                b'}' => (Mask::Interpolation(depth - 1), at),
                b'"' => (Mask::Inner(depth), blank(&mut out, at, 1)),
                _ => (state, at),
            },
            Mask::Inner(depth) => match byte {
                b'\\' => (state, blank(&mut out, at, 2)),
                b'"' => (Mask::Interpolation(depth), blank(&mut out, at, 1)),
                _ => (state, blank(&mut out, at, 1)),
            },
        };
        at += 1;
    }
    out
}

/// Blanks up to `count` bytes from `at`, newlines kept, and returns the offset of the last.
fn blank(out: &mut [u8], at: usize, count: usize) -> usize {
    let end = (at + count).min(out.len());
    for byte in &mut out[at..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
    end - 1
}

/// Blanks the heredoc opened by `<<` at `at`, through the line holding its marker alone, and
/// returns the offset of that line's end; not a heredoc leaves everything and returns `at`.
fn heredoc(bytes: &[u8], at: usize, out: &mut [u8]) -> usize {
    let mut start = at + 2;
    if bytes.get(start) == Some(&b'-') {
        start += 1;
    }
    let marker_end = (start..bytes.len())
        .find(|&i| !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_'))
        .unwrap_or(bytes.len());
    if marker_end == start {
        return at;
    }
    let marker = &bytes[start..marker_end];
    let mut line = (marker_end..bytes.len())
        .find(|&i| bytes[i] == b'\n')
        .unwrap_or(bytes.len());
    while line < bytes.len() {
        let end = (line + 1..bytes.len())
            .find(|&i| bytes[i] == b'\n')
            .unwrap_or(bytes.len());
        for byte in &mut out[line + 1..end] {
            *byte = b' ';
        }
        if bytes[line + 1..end].trim_ascii() == marker {
            return end.saturating_sub(1).max(at);
        }
        line = end;
    }
    bytes.len() - 1
}

/// Every resource becomes a unit, one per Terraform module (folder) and `TYPE.NAME`; a
/// reference to another resource of the module is a hop.
pub(super) fn link(infra: &mut Infra, resources: &[(String, Resource)]) {
    let mut at: HashMap<(&str, &str), usize> = HashMap::new();
    for (file, resource) in resources {
        let key = (dir_of(file), resource.name.as_str());
        (at.entry(key))
            .or_insert_with(|| infra.unit(UnitKind::TerraformResource, &resource.name, file, None));
    }
    for (file, resource) in resources {
        let from = at[&(dir_of(file), resource.name.as_str())];
        for var in &resource.refs {
            if let Some(&to) = at.get(&(dir_of(file), var.value.as_str()))
                && to != from
            {
                let ends = (End::Unit(from), End::Unit(to));
                infra.hop(ends, Evidence::ConfiguredPath, var, &var.value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hop_names, named};
    use super::*;

    #[test]
    fn resources_and_references_past_comments_strings_and_heredocs() {
        let text = r#"# resource "fake" "one" {
resource "aws_sqs_queue" "jobs" {
  name = "jobs-{x}" // aws_lambda_function.fake
}

/* resource "fake" "two" { aws_sqs_queue.jobs } */
resource "aws_lambda_function" "worker" {
  policy = <<EOF
{ "aws_sqs_queue.jobs": "}" }
EOF
  environment {
    variables = {
      QUEUE_URL = aws_sqs_queue.jobs.url
      NOTE      = "a=b {aws_sqs_queue.jobs} ${aws_sqs_queue.jobs.arn}"
      REGION    = var.region
    }
  }
  depends_on = [aws_sqs_queue.jobs, data.aws_iam_policy.x]
}
"#;
        let resources = parse("infra/main.tf", text);
        let names: Vec<&str> = resources.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["aws_sqs_queue.jobs", "aws_lambda_function.worker"]);
        let refs: Vec<(&str, &str, usize)> = (resources[1].refs.iter())
            .map(|r| (r.name.as_str(), r.value.as_str(), r.line))
            .collect();
        assert_eq!(
            refs,
            [
                ("QUEUE_URL", "aws_sqs_queue.jobs", 13),
                ("NOTE", "aws_sqs_queue.jobs", 14),
                ("depends_on", "aws_sqs_queue.jobs", 18),
            ]
        );
        assert!(resources[0].refs.is_empty());

        // Files of one folder are one module.
        let mut infra = Infra::default();
        let mut parsed = resources.into_iter();
        let (queue, worker) = (parsed.next().unwrap(), parsed.next().unwrap());
        let resources = [
            ("infra/main.tf".to_owned(), worker),
            ("infra/queue.tf".to_owned(), queue),
        ];
        link(&mut infra, &resources);
        assert_eq!(infra.units[1].file, "infra/queue.tf");
        let hops = hop_names(&infra);
        let worker = "aws_lambda_function.worker";
        let queue = "aws_sqs_queue.jobs";
        let main = "infra/main.tf";
        assert_eq!(hops[0], named(worker, queue, main, 13, "QUEUE_URL", queue));
        assert_eq!(hops.len(), 3);
    }

    #[test]
    fn strings_inside_interpolations_hide_their_braces_and_quotes() {
        let text = r#"resource "aws_s3_bucket" "a" {
  bucket = "${replace(var.name, "}", "\"{")}-${ "é" }"
}

resource "aws_lambda_function" "c" {
  BUCKET = aws_s3_bucket.b.id
}

resource "aws_s3_bucket" "b" {}
"#;
        let resources = parse("infra/main.tf", text);
        let names: Vec<&str> = resources.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "aws_s3_bucket.a",
                "aws_lambda_function.c",
                "aws_s3_bucket.b"
            ]
        );
        let refs: Vec<(&str, usize)> = (resources[1].refs.iter())
            .map(|r| (r.value.as_str(), r.line))
            .collect();
        assert_eq!(refs, [("aws_s3_bucket.b", 6)]);
    }
}
