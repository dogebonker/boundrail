//! The sequence view of a flow: a lifeline per module in first-appearance order, messages top to
//! bottom, decisions as alt frames with a section per branch, loops as loop frames, and notes in
//! boxes beside their message.

use std::collections::BTreeMap;

use super::svg::{Anchor, Font, Point, Style, Svg};
use super::{
    BASELINE, Context, DASHED, HEAD_FILL, MARGIN, Mark, RULE, SOFT_INK, TITLE_H, class, label,
    line_style, no_steps, outline, participants, text_width,
};
use crate::arch::types::{Branch, Emphasis, Step};

const COL_MIN: f64 = 160.0;
const COL_GAP: f64 = 40.0;
const HEAD_H: f64 = 34.0;
const HEAD_RADIUS: f64 = 4.0;
const ROW: f64 = 40.0;
/// Baselines of a message's label and line below the top of its row.
const LABEL_LINE: f64 = 15.0;
const ARROW_LINE: f64 = 23.0;
const SELF_W: f64 = 36.0;
const SELF_H: f64 = 18.0;
const SELF_LABEL: f64 = 6.0;
const FRAME_PAD: f64 = 72.0;
const FRAME_INSET: f64 = 10.0;
const FRAME_HEAD: f64 = 28.0;
const BRANCH_HEAD: f64 = 20.0;
const FRAME_FOOT: f64 = 10.0;
const FRAME_GAP: f64 = 8.0;
/// How far right of a frame's edge and below a section's top its labels sit.
const FRAME_TEXT_X: f64 = 8.0;
const FRAME_TEXT_Y: f64 = 14.0;
const TAG_H: f64 = 18.0;
const TAG_CUT: f64 = 6.0;
const TAG_TEXT_X: f64 = 5.0;
const TAG_TEXT_Y: f64 = 13.0;
const NOTE_W: f64 = 240.0;
const NOTE_CHARS: usize = 32;
const NOTE_LINE: f64 = 15.0;
const NOTE_PAD: f64 = 8.0;
const NOTE_RADIUS: f64 = 3.0;
const NOTE_FILL: &str = "#fff9db";
const NOTE_EDGE: &str = "#e0c873";

/// A message step's fields.
struct Message<'a> {
    id: &'a str,
    from: &'a str,
    to: &'a str,
    label: &'a str,
    reply: bool,
    emphasis: Option<Emphasis>,
    repeat: Option<u32>,
    note: Option<&'a str>,
}

/// What a frame holds: a decision's branches, or a loop's steps.
enum Frame<'a> {
    Alt(&'a [Branch]),
    Loop(&'a [Step]),
}

impl Frame<'_> {
    fn tag(&self) -> &'static str {
        match self {
            Frame::Alt(_) => "alt",
            Frame::Loop(_) => "loop",
        }
    }

    /// The title beside the tag: a decision's question, or a loop's label in brackets.
    fn title(&self, label: &str) -> String {
        match self {
            Frame::Alt(_) => label.to_owned(),
            Frame::Loop(_) => format!("[{label}]"),
        }
    }
}

/// The diagram under construction: the lifeline columns and how far down the steps reach.
struct Lanes<'a> {
    ctx: &'a Context<'a>,
    /// Participants in first-appearance order, each with its lifeline's x.
    columns: Vec<(&'a str, f64)>,
    x: BTreeMap<&'a str, f64>,
    col: f64,
    notes_x: f64,
    body: Svg,
    y: f64,
}

pub(super) fn render(ctx: &Context<'_>, steps: &[Step]) -> String {
    let names = participants(steps);
    let col = (names.iter())
        .map(|id| text_width(ctx.name(id)) + COL_GAP)
        .fold(COL_MIN, f64::max);
    let columns: Vec<(&str, f64)> = (names.into_iter().enumerate())
        .map(|(k, id)| (id, MARGIN + col * (k as f64 + 0.5)))
        .collect();
    let mut lanes = Lanes {
        ctx,
        x: columns.iter().copied().collect(),
        notes_x: MARGIN + col * columns.len() as f64,
        columns,
        col,
        body: Svg::default(),
        y: MARGIN + TITLE_H + HEAD_H + FRAME_GAP,
    };
    lanes.steps(steps, 0);
    let mut svg = Svg::default();
    ctx.title(&mut svg);
    if steps.is_empty() {
        no_steps(&mut svg);
    }
    lanes.heads(&mut svg, lanes.y + FRAME_GAP);
    svg.append(lanes.body);
    ctx.legend(&mut svg);
    svg.finish()
}

/// `text` broken into lines of at most `NOTE_CHARS` characters at spaces, longer words split.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let chunks = text.split_whitespace().flat_map(|word| {
        let chars: Vec<char> = word.chars().collect();
        let pieces: Vec<String> = chars.chunks(NOTE_CHARS).map(String::from_iter).collect();
        pieces
    });
    for chunk in chunks {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + chunk.chars().count() <= NOTE_CHARS => {
                line.push(' ');
                line.push_str(&chunk);
            }
            _ => lines.push(chunk),
        }
    }
    lines
}

impl Lanes<'_> {
    fn x(&self, id: &str) -> f64 {
        self.x.get(id).copied().unwrap_or(MARGIN + self.col / 2.0)
    }

    /// A head box per participant and its lifeline down to `end`.
    fn heads(&self, svg: &mut Svg, end: f64) {
        let top = MARGIN + TITLE_H;
        let w = self.col - COL_GAP;
        for &(id, x) in &self.columns {
            let mark = self.ctx.mark(id);
            svg.group(&class("lifeline", mark), Some(id));
            svg.polyline(&[(x, top + HEAD_H), (x, end)], &RULE);
            let head = outline(mark, HEAD_FILL);
            svg.rect((x - w / 2.0, top), (w, HEAD_H), HEAD_RADIUS, &head);
            let font = Font {
                anchor: Anchor::Middle,
                bold: true,
                ..label(mark)
            };
            svg.text((x, top + HEAD_H / 2.0 + BASELINE), self.ctx.name(id), &font);
            svg.close_group();
        }
    }

    fn steps(&mut self, steps: &[Step], depth: usize) {
        for step in steps {
            match step {
                Step::Message {
                    id,
                    from,
                    to,
                    label,
                    relationship: _,
                    reply,
                    emphasis,
                    repeat,
                    note,
                } => self.message(&Message {
                    id,
                    from,
                    to,
                    label,
                    reply: *reply,
                    emphasis: *emphasis,
                    repeat: *repeat,
                    note: note.as_deref(),
                }),
                Step::Decision {
                    id,
                    label,
                    branches,
                } => self.frame(id, label, &Frame::Alt(branches), depth),
                Step::Loop { id, label, steps } => {
                    self.frame(id, label, &Frame::Loop(steps), depth);
                }
            }
        }
    }

    fn message(&mut self, message: &Message<'_>) {
        let id = self.ctx.qualify(message.id);
        let mark = self.ctx.mark(&id);
        let (a, b) = (self.x(message.from), self.x(message.to));
        let style = line_style(message.emphasis, mark, message.reply.then_some(DASHED));
        let text = match message.repeat {
            Some(n) => format!("{} x{n}", message.label),
            None => message.label.to_owned(),
        };
        self.body.group(&class("message", mark), Some(&id));
        let (points, at, anchor, height) = self.arrow(a, b);
        self.body.polyline(&points, &style);
        if let [.., before, last] = points.as_slice() {
            self.body
                .arrowhead(*before, *last, style.stroke.unwrap_or(SOFT_INK));
        }
        let font = Font {
            anchor,
            ..label(mark)
        };
        self.body.text(at, &text, &font);
        let note = message.note.map_or(0.0, |n| self.note(n));
        self.body.close_group();
        self.y += height.max(note);
    }

    /// The line of a message from `a` to `b` in the current row, a loop back for a message to
    /// itself, with where its label goes, the label's anchor and the row's height.
    fn arrow(&self, a: f64, b: f64) -> (Vec<Point>, Point, Anchor, f64) {
        let line = self.y + ARROW_LINE;
        let at_label = self.y + LABEL_LINE;
        if a == b {
            let loop_back = vec![
                (a, line),
                (a + SELF_W, line),
                (a + SELF_W, line + SELF_H),
                (a, line + SELF_H),
            ];
            return (
                loop_back,
                (a + SELF_LABEL, at_label),
                Anchor::Start,
                ROW + SELF_H,
            );
        }
        let across = vec![(a, line), (b, line)];
        (across, ((a + b) / 2.0, at_label), Anchor::Middle, ROW)
    }

    /// A note box beside the current row; returns the height it takes.
    fn note(&mut self, text: &str) -> f64 {
        let lines = wrap(text);
        let h = lines.len() as f64 * NOTE_LINE + NOTE_PAD;
        let style = Style {
            fill: Some(NOTE_FILL),
            stroke: Some(NOTE_EDGE),
            ..Style::default()
        };
        let top = self.y + NOTE_PAD / 2.0;
        self.body
            .rect((self.notes_x, top), (NOTE_W, h), NOTE_RADIUS, &style);
        for (k, line) in lines.iter().enumerate() {
            let at = (self.notes_x + NOTE_PAD, top + NOTE_LINE * (k as f64 + 1.0));
            self.body.text(at, line, &label(None));
        }
        h + NOTE_PAD
    }

    /// The left and right edges of `frame`: wide enough for the lifelines its messages touch,
    /// narrower the deeper it nests.
    fn span(&self, frame: &Frame<'_>, depth: usize) -> Point {
        let xs: Vec<f64> = match frame {
            Frame::Alt(branches) => (branches.iter())
                .flat_map(|b| participants(&b.steps))
                .map(|id| self.x(id))
                .collect(),
            Frame::Loop(steps) => participants(steps)
                .into_iter()
                .map(|id| self.x(id))
                .collect(),
        };
        let pad = (FRAME_PAD - FRAME_INSET * depth as f64).max(FRAME_INSET);
        let low = xs.iter().copied().reduce(f64::min).unwrap_or(self.x(""));
        let high = xs.iter().copied().reduce(f64::max).unwrap_or(low);
        (low - pad, high + pad)
    }

    /// An alt frame with a section per branch, or a loop frame with one section.
    fn frame(&mut self, id: &str, title: &str, frame: &Frame<'_>, depth: usize) {
        let id = self.ctx.qualify(id);
        let mark = self.ctx.mark(&id);
        let span = self.span(frame, depth);
        let top = self.y;
        self.body
            .group(&class(&format!("frame {}", frame.tag()), mark), Some(&id));
        self.y += FRAME_HEAD;
        match frame {
            Frame::Alt(branches) => {
                for (k, branch) in branches.iter().enumerate() {
                    if k > 0 {
                        self.body
                            .polyline(&[(span.0, self.y), (span.1, self.y)], &RULE);
                    }
                    let font = Font {
                        fill: Some(SOFT_INK),
                        ..label(None)
                    };
                    let at = (span.0 + FRAME_TEXT_X, self.y + FRAME_TEXT_Y);
                    self.body.text(at, &format!("[{}]", branch.label), &font);
                    self.y += BRANCH_HEAD;
                    self.section(&branch.steps, depth);
                }
            }
            Frame::Loop(steps) => self.section(steps, depth),
        }
        self.y += FRAME_FOOT;
        let size = (span.1 - span.0, self.y - top);
        self.body
            .rect((span.0, top), size, 0.0, &outline(mark, "none"));
        self.tag(frame, title, (span.0, top), mark);
        self.body.close_group();
        self.y += FRAME_GAP;
    }

    fn section(&mut self, steps: &[Step], depth: usize) {
        self.steps(steps, depth + 1);
        if steps.is_empty() {
            self.y += ROW / 2.0;
        }
    }

    /// The frame's tag in its top-left corner, with its title after it.
    fn tag(&mut self, frame: &Frame<'_>, title: &str, corner: Point, mark: Option<Mark>) {
        let (x, y) = corner;
        let tag = frame.tag();
        let w = text_width(tag) + 2.0 * TAG_CUT + TAG_TEXT_X;
        let shape = [
            (x, y),
            (x + w, y),
            (x + w, y + TAG_H - TAG_CUT),
            (x + w - TAG_CUT, y + TAG_H),
            (x, y + TAG_H),
        ];
        self.body.polygon(&shape, &outline(mark, HEAD_FILL));
        let bold = Font {
            bold: true,
            ..label(mark)
        };
        self.body.text((x + TAG_TEXT_X, y + TAG_TEXT_Y), tag, &bold);
        let at = (x + w + FRAME_TEXT_X, y + TAG_TEXT_Y);
        self.body.text(at, &frame.title(title), &label(mark));
    }
}

#[cfg(test)]
mod tests {
    use super::{NOTE_CHARS, wrap};

    #[test]
    fn notes_wrap_at_spaces_and_split_long_words() {
        let text = "one two three four five six seven eight nine ten eleven";
        let lines = wrap(text);
        assert!(lines.iter().all(|l| l.chars().count() <= NOTE_CHARS));
        assert_eq!(lines.join(" "), text);
        let long = "y".repeat(NOTE_CHARS * 2 + 3);
        let lines = wrap(&format!("a {long}"));
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= NOTE_CHARS));
    }
}
