//! `model pictures`: each picture recipe's file against what the model draws now, and drawing the
//! stale and missing ones; and the embeds part of `model check`.

use super::*;
use boundrail::arch::diagnostic::DiagnosticCode;
use boundrail::arch::picture::{self, Drawing, Format};
use boundrail::arch::semantic::Judged;
use std::collections::BTreeSet;

/// One picture recipe and the state of its file.
#[derive(serde::Serialize)]
pub(super) struct Entry<'a> {
    id: String,
    name: &'a str,
    output: &'a str,
    format: Format,
    reading: &'a BeatReading,
    status: Status,
    /// Why an unrenderable picture is: `render-failed`, `output-blocked`, `no-rasterizer` or
    /// `rasterizer-failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Whether `--write` wrote the file in this run.
    written: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Fresh,
    Stale,
    Missing,
    Unrenderable,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Unrenderable => "unrenderable",
        }
    }
}

const RENDER_FAILED: &str = "render-failed";
const OUTPUT_BLOCKED: &str = "output-blocked";

impl Entry<'_> {
    fn unrenderable(mut self, reason: &'static str, message: String) -> Self {
        self.status = Status::Unrenderable;
        self.reason = Some(reason);
        self.message = Some(message);
        self
    }
}

/// `model pictures`: exits 1 when `write` leaves any picture unrenderable.
pub(super) fn list(
    root: &Path,
    markdown: bool,
    only: Option<&str>,
    write: bool,
) -> Result<ExitCode> {
    let (model, _, pending) = load_for_read(root, None)?;
    let entries = survey(root, &mut States::new(&model), only, write)?;
    let stuck = entries.iter().any(|e| e.status == Status::Unrenderable);
    let code = ExitCode::from(u8::from(write && stuck));
    if !markdown {
        super::write(None, &output(&model, pending, json!(entries)))?;
        return Ok(code);
    }
    if entries.is_empty() {
        println!("No pictures");
    }
    for e in &entries {
        let why = (e.reason.zip(e.message.as_ref()))
            .map(|(reason, message)| format!(" ({reason}: {message})"))
            .unwrap_or_default();
        let written = if e.written { ", written" } else { "" };
        let status = e.status.as_str();
        println!("- {} {}: {} {status}{why}{written}", e.name, e.id, e.output);
    }
    Ok(code)
}

/// The JSON output of `model pictures` without writing. Without `status` it lists the recipes
/// alone and draws nothing.
pub(crate) fn pictures_output(root: &Path, status: bool) -> Result<Value> {
    let (model, _, pending) = load_for_read(root, None)?;
    let pictures = if status {
        json!(survey(root, &mut States::new(&model), None, false)?)
    } else {
        json!(model.picture_entries())
    };
    Ok(output(&model, pending, pictures))
}

fn output(model: &Model, pending: Option<Value>, pictures: Value) -> Value {
    let mut output = json!({
        "schema": MODEL_SCHEMA,
        "kind": "pictures",
        "success": true,
        "vault": model.dir,
        "revision": model.revision,
        "pictures": pictures,
    });
    if let Some(pending) = pending {
        output["pending_operation"] = pending;
    }
    output
}

/// The pictures of the current state in `states`, in order, or the one `only` names, each drawn
/// and compared with its file; with `write`, stale and missing files are drawn.
fn survey<'m>(
    root: &Path,
    states: &mut States<'m>,
    only: Option<&str>,
    write: bool,
) -> Result<Vec<Entry<'m>>> {
    let model = states.current();
    let checkout = checkpoint::checkout(&model.dir)?;
    let pictures = match only {
        Some(subject) => vec![model.picture(subject)?],
        None => model.pictures_in_order(),
    };
    let entries = pictures.into_iter().map(|p| {
        let entry = Entry {
            id: model.qualify(&p.id),
            name: &p.name,
            output: &p.output,
            format: Format::of(&p.output),
            reading: &p.reading,
            status: Status::Fresh,
            reason: None,
            message: None,
            written: false,
        };
        judge(root, &checkout, states, entry, write)
    });
    Ok(entries.collect())
}

/// `entry` with the status of its file, drawn first when `write` is set and it is stale or
/// missing.
fn judge<'m>(
    root: &Path,
    checkout: &Path,
    states: &mut States<'_>,
    entry: Entry<'m>,
    write: bool,
) -> Entry<'m> {
    // A reading of a proposal or checkpoint that no longer loads draws nothing.
    let drawing = match draw_reading(root, entry.reading, None, states) {
        Ok((svg, _)) => Drawing::new(svg),
        Err(error) => return entry.unrenderable(RENDER_FAILED, format!("{error:#}")),
    };
    let status = match picture::compare(checkout, entry.output, &drawing) {
        picture::Found::Fresh => Status::Fresh,
        picture::Found::Stale => Status::Stale,
        picture::Found::Missing => Status::Missing,
        picture::Found::Blocked(why) => return entry.unrenderable(OUTPUT_BLOCKED, why),
    };
    if !write || status == Status::Fresh {
        return Entry { status, ..entry };
    }
    let bytes = match entry.format {
        Format::Svg => drawing.svg().as_bytes().to_vec(),
        Format::Png => match picture::rasterize(&drawing, checkout) {
            Ok(png) => png,
            Err(failed) => return entry.unrenderable(failed.reason, failed.message),
        },
    };
    match picture::write(checkout, entry.output, &bytes) {
        Ok(()) => Entry {
            written: true,
            ..entry
        },
        Err(why) => entry.unrenderable(OUTPUT_BLOCKED, why),
    }
}

/// The embeds part of `model check` on the current state in `states`: `PICTURE_TARGET_MISSING`
/// for a picture whose proposal or checkpoint is gone or lacks what it reads, `PICTURE_STALE` and
/// `PICTURE_MISSING` for its file, and `PICTURE_UNRENDERABLE` when the output is blocked, with a
/// skip for each picture whose freshness is unknown. A PNG is checked by its source digest, so no
/// rasterizer is needed.
pub(super) fn check(root: &Path, states: &mut States<'_>) -> Result<(Vec<Diagnostic>, Vec<Skip>)> {
    let model = states.current();
    let readings = model.pictures_in_order().into_iter().map(|p| Judged {
        code: DiagnosticCode::PictureTargetMissing,
        subjects: vec![model.qualify(&p.id)],
        label: p.name.clone(),
        reading: &p.reading,
    });
    let mut found = semantic::targets(states, readings);
    let missing: BTreeSet<String> = (found.iter()).flat_map(|d| d.subjects.clone()).collect();
    let mut skips = Vec::new();
    for e in survey(root, states, None, false)? {
        let (code, message) = match e.status {
            Status::Fresh => continue,
            Status::Stale => (
                DiagnosticCode::PictureStale,
                format!(
                    "{}: {} does not match the model; `model pictures --write` redraws it",
                    e.name, e.output
                ),
            ),
            Status::Missing => (
                DiagnosticCode::PictureMissing,
                format!(
                    "{}: {} is missing; `model pictures --write` draws it",
                    e.name, e.output
                ),
            ),
            Status::Unrenderable => {
                skips.push(Skip {
                    subject: e.id.clone(),
                    site: code::SkipSite::Picture,
                    reason: SkipReason::Unrenderable,
                });
                let why = e.message.unwrap_or_default();
                match e.reason {
                    // Already reported, more precisely, as a missing target.
                    Some(RENDER_FAILED) if missing.contains(&e.id) => continue,
                    Some(RENDER_FAILED) => (
                        DiagnosticCode::PictureTargetMissing,
                        format!("{}: {why}", e.name),
                    ),
                    _ => (
                        DiagnosticCode::PictureUnrenderable,
                        format!("{}: {} cannot be checked: {why}", e.name, e.output),
                    ),
                }
            }
        };
        found.push(Diagnostic::new(code, vec![e.id], message));
    }
    Ok((found, skips))
}
