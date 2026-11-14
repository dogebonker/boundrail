//! The flowchart view of a flow: steps top to bottom. A message is a rounded box
//! "From -> To: label", a decision a diamond whose labelled branches run side by side and rejoin,
//! and a loop a header box with a back edge from the end of its steps.

use std::collections::BTreeMap;

use super::svg::{Anchor, Font, Point, Style, Svg};
use super::{
    BASELINE, BLUE, Context, DASHED, HEAD_FILL, HERO_WIDTH, LAVENDER, LINE_WIDTH, MARGIN,
    MUTED_OPACITY, SOFT_INK, TITLE_H, class, label, no_steps, outline, text_width,
};
use crate::arch::types::{Branch, Emphasis, Step};

const NODE_H: f64 = 36.0;
const NODE_PAD: f64 = 14.0;
const MESSAGE_RADIUS: f64 = 12.0;
const LOOP_RADIUS: f64 = 4.0;
const GAP: f64 = 32.0;
const DIAMOND_H: f64 = 56.0;
const DIAMOND_EXTRA: f64 = 64.0;
const BRANCH_GAP: f64 = 32.0;
const BRANCH_MIN: f64 = 96.0;
/// Where a branch label sits: right of the branch's center and below the fork line.
const BRANCH_LABEL_X: f64 = 5.0;
const BRANCH_LABEL_Y: f64 = 13.0;
/// Room on each side of a loop's steps for its back edge.
const BACK: f64 = 28.0;
const BACK_LABEL_Y: f64 = 4.0;
const JOIN_R: f64 = 4.0;

struct Chart<'c, 's> {
    ctx: &'c Context<'c>,
    svg: Svg,
    /// The width of each step and branch by local ID, from one measuring pass.
    widths: BTreeMap<&'s str, f64>,
    /// The box text of each message by local ID.
    texts: BTreeMap<&'s str, String>,
}

pub(super) fn render(ctx: &Context<'_>, steps: &[Step]) -> String {
    let mut chart = Chart {
        ctx,
        svg: Svg::default(),
        widths: BTreeMap::new(),
        texts: BTreeMap::new(),
    };
    let width = chart.measure_list(steps);
    chart.list(steps, MARGIN + width / 2.0, MARGIN + TITLE_H);
    let mut svg = Svg::default();
    ctx.title(&mut svg);
    if steps.is_empty() {
        no_steps(&mut svg);
    }
    svg.append(chart.svg);
    ctx.legend(&mut svg);
    svg.finish()
}

fn loop_text(label: &str) -> String {
    format!("loop: {label}")
}

impl<'s> Chart<'_, 's> {
    /// Records the width of `step` and of the branches and steps inside it, and each message's
    /// text; returns the step's width.
    fn measure(&mut self, step: &'s Step) -> f64 {
        let width = match step {
            Step::Message {
                id,
                from,
                to,
                label,
                repeat,
                ..
            } => {
                let (from, to) = (self.ctx.name(from), self.ctx.name(to));
                let repeat = repeat.map(|n| format!(" x{n}")).unwrap_or_default();
                let text = format!("{from} -> {to}: {label}{repeat}");
                let width = text_width(&text) + 2.0 * NODE_PAD;
                self.texts.insert(id, text);
                width
            }
            Step::Decision {
                label, branches, ..
            } => {
                let gaps = BRANCH_GAP * branches.len().saturating_sub(1) as f64;
                let branches: f64 = branches.iter().map(|b| self.measure_branch(b)).sum();
                (text_width(label) + DIAMOND_EXTRA).max(branches + gaps)
            }
            Step::Loop { label, steps, .. } => {
                let header = text_width(&loop_text(label)) + 2.0 * NODE_PAD;
                header.max(self.measure_list(steps)) + 2.0 * BACK
            }
        };
        self.widths.insert(step.id(), width);
        width
    }

    fn measure_list(&mut self, steps: &'s [Step]) -> f64 {
        (steps.iter()).fold(0.0, |widest, step| self.measure(step).max(widest))
    }

    /// A branch is wide enough for its steps and for its label on either side of its center.
    fn measure_branch(&mut self, branch: &'s Branch) -> f64 {
        let label = 2.0 * (text_width(&branch.label) + BRANCH_LABEL_X) + NODE_PAD;
        let width = self.measure_list(&branch.steps).max(label).max(BRANCH_MIN);
        self.widths.insert(&branch.id, width);
        width
    }

    /// A plain line, with an arrowhead at its end when `head`.
    fn line(&mut self, points: &[Point], head: bool) {
        let style = Style {
            stroke: Some(SOFT_INK),
            width: Some(LINE_WIDTH),
            ..Style::default()
        };
        self.svg.polyline(points, &style);
        if let (true, [.., from, to]) = (head, points) {
            self.svg.arrowhead(*from, *to, SOFT_INK);
        }
    }

    /// Draws `steps` down from `y` centered on `cx`, each joined to the next; returns the first
    /// step's entry and the last step's exit, `None` for no steps.
    fn list(&mut self, steps: &[Step], cx: f64, y: f64) -> Option<(Point, Point)> {
        let mut ends: Option<(Point, Point)> = None;
        let mut y = y;
        for step in steps {
            let (entry, exit) = match step {
                Step::Message {
                    id,
                    reply,
                    emphasis,
                    ..
                } => self.message(id, *reply, *emphasis, (cx, y)),
                Step::Decision {
                    id,
                    label,
                    branches,
                } => self.decision(id, label, branches, (cx, y)),
                Step::Loop { id, label, steps } => self.repeat(id, label, steps, (cx, y)),
            };
            if let Some((_, previous)) = ends {
                self.line(&[previous, entry], true);
            }
            ends = Some((ends.map_or(entry, |(first, _)| first), exit));
            y = exit.1 + GAP;
        }
        ends
    }

    fn message(
        &mut self,
        id: &str,
        reply: bool,
        emphasis: Option<Emphasis>,
        (cx, y): Point,
    ) -> (Point, Point) {
        let text = &self.texts[id];
        let w = self.widths[id];
        let qualified = self.ctx.qualify(id);
        let mark = self.ctx.mark(&qualified);
        let mut style = outline(mark, BLUE);
        if reply {
            style.dash = Some(DASHED);
        }
        match emphasis {
            Some(Emphasis::Hero) => style.width = Some(HERO_WIDTH),
            Some(Emphasis::Muted) => style.opacity = style.opacity.or(Some(MUTED_OPACITY)),
            Some(Emphasis::Normal) | None => {}
        }
        self.svg
            .group(&class("step message", mark), Some(&qualified));
        self.svg
            .rect((cx - w / 2.0, y), (w, NODE_H), MESSAGE_RADIUS, &style);
        let font = Font {
            anchor: Anchor::Middle,
            ..label(mark)
        };
        self.svg
            .text((cx, y + NODE_H / 2.0 + BASELINE), text, &font);
        self.svg.close_group();
        ((cx, y), (cx, y + NODE_H))
    }

    /// A diamond, its branches side by side below it, each entered by a labelled edge, and a
    /// join point where they meet again.
    fn decision(
        &mut self,
        id: &str,
        question: &str,
        branches: &[Branch],
        (cx, y): Point,
    ) -> (Point, Point) {
        let qualified = self.ctx.qualify(id);
        let mark = self.ctx.mark(&qualified);
        let half = (text_width(question) + DIAMOND_EXTRA) / 2.0;
        let mid = y + DIAMOND_H / 2.0;
        self.svg
            .group(&class("step decision", mark), Some(&qualified));
        let diamond = [
            (cx, y),
            (cx + half, mid),
            (cx, y + DIAMOND_H),
            (cx - half, mid),
        ];
        self.svg.polygon(&diamond, &outline(mark, LAVENDER));
        let font = Font {
            anchor: Anchor::Middle,
            ..label(mark)
        };
        self.svg.text((cx, mid + BASELINE), question, &font);
        let widths: Vec<f64> = branches
            .iter()
            .map(|b| self.widths[b.id.as_str()])
            .collect();
        let total = widths.iter().sum::<f64>() + BRANCH_GAP * widths.len().saturating_sub(1) as f64;
        let fork = y + DIAMOND_H + GAP / 2.0;
        let mut x = cx - total / 2.0;
        let mut exits = Vec::new();
        for (branch, w) in branches.iter().zip(widths) {
            exits.push(self.branch(branch, (cx, y + DIAMOND_H), x + w / 2.0, fork));
            x += w + BRANCH_GAP;
        }
        let join = exits.iter().map(|e| e.1).fold(fork, f64::max) + GAP / 2.0;
        for exit in exits {
            self.line(&[exit, (exit.0, join), (cx, join)], false);
        }
        self.svg.circle((cx, join), JOIN_R, SOFT_INK);
        self.svg.close_group();
        ((cx, y), (cx, join))
    }

    /// One branch at `bx`: the edge from the diamond's tip, the label, and the steps; returns
    /// where it leaves for the join.
    fn branch(&mut self, branch: &Branch, tip: Point, bx: f64, fork: f64) -> Point {
        let font = Font {
            fill: Some(SOFT_INK),
            ..label(None)
        };
        let at = (bx + BRANCH_LABEL_X, fork + BRANCH_LABEL_Y);
        self.svg.text(at, &branch.label, &font);
        let fork_edge = [tip, (tip.0, fork), (bx, fork)];
        match self.list(&branch.steps, bx, fork + GAP) {
            Some((entry, exit)) => {
                self.line(&[tip, (tip.0, fork), (bx, fork), entry], true);
                exit
            }
            None => {
                self.line(&fork_edge, false);
                (bx, fork)
            }
        }
    }

    /// A loop header, its steps below it, and a back edge from their end to the header.
    fn repeat(&mut self, id: &str, title: &str, steps: &[Step], (cx, y): Point) -> (Point, Point) {
        let qualified = self.ctx.qualify(id);
        let mark = self.ctx.mark(&qualified);
        let text = loop_text(title);
        let hw = text_width(&text) + 2.0 * NODE_PAD;
        self.svg.group(&class("step loop", mark), Some(&qualified));
        let mut style = outline(mark, HEAD_FILL);
        style.dash = Some(DASHED);
        self.svg
            .rect((cx - hw / 2.0, y), (hw, NODE_H), LOOP_RADIUS, &style);
        let font = Font {
            anchor: Anchor::Middle,
            bold: true,
            ..label(mark)
        };
        self.svg
            .text((cx, y + NODE_H / 2.0 + BASELINE), &text, &font);
        let header_end = y + NODE_H;
        let body_end = match self.list(steps, cx, header_end + GAP) {
            Some((entry, exit)) => {
                self.line(&[(cx, header_end), entry], true);
                exit.1
            }
            None => header_end,
        };
        let exit = body_end + GAP / 2.0;
        self.line(&[(cx, body_end), (cx, exit)], false);
        let left = cx - self.widths[id] / 2.0 + BACK / 2.0;
        let header_mid = y + NODE_H / 2.0;
        let back = [
            (cx, exit),
            (left, exit),
            (left, header_mid),
            (cx - hw / 2.0, header_mid),
        ];
        self.line(&back, true);
        let font = Font {
            anchor: Anchor::Middle,
            fill: Some(SOFT_INK),
            ..label(mark)
        };
        self.svg
            .text(((cx + left) / 2.0, exit - BACK_LABEL_Y), "repeat", &font);
        self.svg.close_group();
        ((cx, y), (cx, exit))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::View;
    use super::super::tests::{draw, flow, model, q};

    #[test]
    fn a_decision_rejoins_and_a_loop_points_back() {
        let message = |id: &str, label: &str| json!({"kind": "message", "id": id, "from": q("WebApp01"), "to": q("ApiSrv01"), "label": label});
        let collections = flow(json!([
            message("Step0001", "ask"),
            {"kind": "decision", "id": "Deci0001", "label": "ok?", "branches": [
                {"id": "Bran0001", "label": "yes", "steps": [message("Step0002", "pay")]},
                {"id": "Bran0002", "label": "no", "steps": []},
            ]},
            {"kind": "loop", "id": "Loop0001", "label": "each", "steps": [message("Step0003", "poll")]},
        ]));
        let shop = model(Vec::new(), collections);
        let out = draw(&shop, &View::Flowchart("Checkout")).unwrap();
        assert!(
            out.contains("Model001#WebApp01 -&gt; Model001#ApiSrv01: ask"),
            "{out}"
        );
        assert_eq!(out.matches("<circle").count(), 1);
        assert!(out.contains(">loop: each</text>") && out.contains(">repeat</text>"));
        assert!(out.contains(">yes</text>") && out.contains(">no</text>"));
    }

    #[test]
    fn a_long_branch_label_clears_the_next_branch() {
        let long = "a branch label of some length";
        let collections = flow(json!([
            {"kind": "decision", "id": "Deci0001", "label": "ok?", "branches": [
                {"id": "Bran0001", "label": long, "steps": []},
                {"id": "Bran0002", "label": "no", "steps": []},
            ]},
        ]));
        let out = draw(
            &model(Vec::new(), collections),
            &View::Flowchart("Checkout"),
        )
        .unwrap();
        let x_of = |text: &str| -> f64 {
            let end = out.find(&format!(">{text}</text>")).unwrap();
            let tag = &out[out[..end].rfind("<text").unwrap()..end];
            let start = tag.find("x=\"").unwrap() + 3;
            tag[start..start + tag[start..].find('"').unwrap()]
                .parse()
                .unwrap()
        };
        let reach = x_of(long) + super::text_width(long);
        assert!(reach < x_of("no"), "{out}");
    }
}
