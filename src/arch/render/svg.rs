//! A small SVG writer. Text and attribute values are XML-escaped as they are written, and the
//! writer tracks the bounds of everything it draws so the picture is sized to hold it.

use std::fmt::Write as _;

use super::{INK, MARGIN};

pub(super) const DECIMALS: usize = 1;
pub(super) const FONT_SIZE: f64 = 12.0;
/// The mark on every drawing's root element, by which `model pictures` knows a file it may
/// overwrite.
pub(crate) const GENERATOR: &str = r#"data-generator="boundrail""#;
const FONT_FAMILY: &str = "sans-serif";
pub(super) const BACKGROUND: &str = "#ffffff";
/// Estimated width of one character at the base font size.
/// ponytail: one width for every glyph, so wide or non-Latin text can overflow its box; measure
/// the font when that shows.
const CHAR_W: f64 = 7.0;
/// How far a glyph reaches below its baseline.
pub(super) const DESCENT: f64 = 4.0;
/// The arrowhead's length and half its width.
const HEAD_LENGTH: f64 = 9.0;
const HEAD_HALF: f64 = 4.5;
/// How strongly what a focus leaves out still shows.
const DIMMED_OPACITY: f64 = 0.3;

pub(super) type Point = (f64, f64);

/// How a shape is painted. Unset fields are left out of the element; no fill means none.
#[derive(Default)]
pub(super) struct Style<'a> {
    pub(super) fill: Option<&'a str>,
    pub(super) fill_opacity: Option<f64>,
    pub(super) stroke: Option<&'a str>,
    pub(super) width: Option<f64>,
    pub(super) dash: Option<&'a str>,
    pub(super) opacity: Option<f64>,
}

#[derive(Default, Clone, Copy)]
pub(super) enum Anchor {
    #[default]
    Start,
    Middle,
}

/// How a text is set. Unset fields are left out of the element.
#[derive(Default)]
pub(super) struct Font<'a> {
    pub(super) anchor: Anchor,
    pub(super) bold: bool,
    pub(super) size: Option<f64>,
    pub(super) fill: Option<&'a str>,
    pub(super) struck: bool,
    pub(super) opacity: Option<f64>,
}

/// Estimated width of `text` at the base font size.
pub(super) fn text_width(text: &str) -> f64 {
    text.chars().count() as f64 * CHAR_W
}

/// Appends `text` to `out` with the five XML special characters replaced by entities.
fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

/// An SVG body under construction; [`Svg::finish`] wraps it in the root element.
#[derive(Default)]
pub(super) struct Svg {
    body: String,
    /// The lowest and highest corner of everything drawn.
    bounds: Option<(Point, Point)>,
}

impl Svg {
    fn grow(&mut self, low: Point, high: Point) {
        let (l, h) = self.bounds.unwrap_or((low, high));
        self.bounds = Some((
            (l.0.min(low.0), l.1.min(low.1)),
            (h.0.max(high.0), h.1.max(high.1)),
        ));
    }

    fn attr(&mut self, key: &str, value: &str) {
        let _ = write!(self.body, " {key}=\"");
        escape_into(&mut self.body, value);
        self.body.push('"');
    }

    fn num(&mut self, key: &str, value: f64) {
        let _ = write!(self.body, " {key}=\"{value:.DECIMALS$}\"");
    }

    fn style(&mut self, style: &Style<'_>) {
        self.attr("fill", style.fill.unwrap_or("none"));
        if let Some(stroke) = style.stroke {
            self.attr("stroke", stroke);
        }
        if let Some(dash) = style.dash {
            self.attr("stroke-dasharray", dash);
        }
        let numbers = [
            ("fill-opacity", style.fill_opacity),
            ("stroke-width", style.width),
            ("opacity", style.opacity),
        ];
        for (key, value) in numbers {
            if let Some(value) = value {
                self.num(key, value);
            }
        }
    }

    fn points(&mut self, points: &[Point]) {
        self.body.push_str(" points=\"");
        for (k, &(x, y)) in points.iter().enumerate() {
            let space = if k == 0 { "" } else { " " };
            let _ = write!(self.body, "{space}{x:.DECIMALS$},{y:.DECIMALS$}");
            self.grow((x, y), (x, y));
        }
        self.body.push('"');
    }

    /// Opens a group with the class `class` and, when given, the model ID it draws in `data-id`.
    pub(super) fn group(&mut self, class: &str, id: Option<&str>) {
        self.body.push_str("<g");
        self.attr("class", class);
        if let Some(id) = id {
            self.attr("data-id", id);
        }
        self.body.push_str(">\n");
    }

    /// Opens a group that fades what it holds, for what a focus leaves out; close it with
    /// [`Svg::close_group`].
    pub(super) fn dimmed(&mut self) {
        self.body.push_str("<g");
        self.attr("class", "dimmed");
        self.num("opacity", DIMMED_OPACITY);
        self.body.push_str(">\n");
    }

    pub(super) fn close_group(&mut self) {
        self.body.push_str("</g>\n");
    }

    pub(super) fn text(&mut self, at: Point, text: &str, font: &Font<'_>) {
        let size = font.size.unwrap_or(FONT_SIZE);
        let width = text_width(text) * size / FONT_SIZE;
        let left = match font.anchor {
            Anchor::Start => at.0,
            Anchor::Middle => at.0 - width / 2.0,
        };
        self.grow((left, at.1 - size), (left + width, at.1 + DESCENT));
        self.body.push_str("<text");
        self.num("x", at.0);
        self.num("y", at.1);
        if let Anchor::Middle = font.anchor {
            self.attr("text-anchor", "middle");
        }
        if let Some(size) = font.size {
            self.num("font-size", size);
        }
        if font.bold {
            self.attr("font-weight", "bold");
        }
        self.attr("fill", font.fill.unwrap_or("currentColor"));
        if font.struck {
            self.attr("text-decoration", "line-through");
        }
        if let Some(opacity) = font.opacity {
            self.num("opacity", opacity);
        }
        self.body.push('>');
        escape_into(&mut self.body, text);
        self.body.push_str("</text>\n");
    }

    pub(super) fn rect(&mut self, at: Point, size: Point, radius: f64, style: &Style<'_>) {
        self.grow(at, (at.0 + size.0, at.1 + size.1));
        self.body.push_str("<rect");
        self.num("x", at.0);
        self.num("y", at.1);
        self.num("width", size.0);
        self.num("height", size.1);
        if radius > 0.0 {
            self.num("rx", radius);
        }
        self.style(style);
        self.body.push_str("/>\n");
    }

    pub(super) fn polyline(&mut self, points: &[Point], style: &Style<'_>) {
        self.body.push_str("<polyline");
        self.points(points);
        self.style(style);
        self.body.push_str("/>\n");
    }

    pub(super) fn polygon(&mut self, points: &[Point], style: &Style<'_>) {
        self.body.push_str("<polygon");
        self.points(points);
        self.style(style);
        self.body.push_str("/>\n");
    }

    pub(super) fn circle(&mut self, center: Point, radius: f64, fill: &str) {
        let (x, y) = center;
        self.grow((x - radius, y - radius), (x + radius, y + radius));
        self.body.push_str("<circle");
        self.num("cx", x);
        self.num("cy", y);
        self.num("r", radius);
        self.attr("fill", fill);
        self.body.push_str("/>\n");
    }

    /// A filled arrowhead at `to`, pointing along the segment from `from`.
    pub(super) fn arrowhead(&mut self, from: Point, to: Point, color: &str) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy);
        if length == 0.0 {
            return;
        }
        let (ux, uy) = (dx / length, dy / length);
        let base = (to.0 - ux * HEAD_LENGTH, to.1 - uy * HEAD_LENGTH);
        let left = (base.0 - uy * HEAD_HALF, base.1 + ux * HEAD_HALF);
        let right = (base.0 + uy * HEAD_HALF, base.1 - ux * HEAD_HALF);
        let style = Style {
            fill: Some(color),
            ..Style::default()
        };
        self.polygon(&[to, left, right], &style);
    }

    pub(super) fn append(&mut self, other: Svg) {
        self.body.push_str(&other.body);
        if let Some((low, high)) = other.bounds {
            self.grow(low, high);
        }
    }

    /// The lowest point drawn so far.
    pub(super) fn bottom(&self) -> f64 {
        self.bounds.map_or(0.0, |(_, high)| high.1)
    }

    pub(super) fn frame(&self) -> (f64, f64, f64, f64) {
        let ((x0, y0), (x1, y1)) = self.bounds.unwrap_or(((0.0, 0.0), (0.0, 0.0)));
        let (left, top) = (
            (x0 - MARGIN).min(0.0).floor(),
            (y0 - MARGIN).min(0.0).floor(),
        );
        (
            left,
            top,
            (x1 + MARGIN - left).ceil(),
            (y1 + MARGIN - top).ceil(),
        )
    }

    pub(super) fn finish(self) -> String {
        let (left, top, w, h) = self.frame();
        let mut out = String::with_capacity(self.body.len() + 256);
        let _ = writeln!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" \
             viewBox=\"{left:.0} {top:.0} {w:.0} {h:.0}\" font-family=\"{FONT_FAMILY}\" \
             font-size=\"{FONT_SIZE:.DECIMALS$}\" color=\"{INK}\" {GENERATOR}>"
        );
        let _ = writeln!(
            out,
            "<rect x=\"{left:.0}\" y=\"{top:.0}\" width=\"{w:.0}\" height=\"{h:.0}\" fill=\"{BACKGROUND}\"/>"
        );
        out.push_str(&self.body);
        out.push_str("</svg>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text_and_attributes() {
        let mut svg = Svg::default();
        svg.group("a\"<&", None);
        svg.text((1.0, 20.0), "<&\"'>", &Font::default());
        svg.close_group();
        let out = svg.finish();
        assert!(out.contains(r#"class="a&quot;&lt;&amp;""#), "{out}");
        assert!(out.contains(">&lt;&amp;&quot;&apos;&gt;</text>"), "{out}");
        assert!(!out.contains("<&"));
    }

    #[test]
    fn sizes_the_picture_to_what_was_drawn() {
        let mut svg = Svg::default();
        svg.rect((30.0, 30.0), (100.0, 40.0), 0.0, &Style::default());
        let out = svg.finish();
        assert!(
            out.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" width="154" height="94" viewBox="0 0 154 94""#),
            "{out}"
        );
        // A centered text reaching left of the origin moves the viewBox left.
        let mut svg = Svg::default();
        let font = Font {
            anchor: Anchor::Middle,
            ..Font::default()
        };
        svg.text((10.0, 50.0), "twenty characters...", &font);
        assert!(svg.finish().contains(r#"viewBox="-84 0 "#));
    }
}
