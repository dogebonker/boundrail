//! `model render`: SVG pictures of the authored model. The architecture view draws modules and
//! relationships; the sequence and flowchart views draw one flow. Drawn against another state,
//! each view marks what was added, removed and changed. Rendering is pure: loaded models in, an
//! SVG string out, the same bytes for the same input.

mod architecture;
mod flowchart;
mod layout;
mod sequence;
mod svg;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

use super::compare::{self, Changes, Comparison, Entry};
use super::index::Index;
use super::types::{Dash, EXTERNAL_KIND, Emphasis, Flow, Module, Step, VaultConfig, walk};
use super::vault::{self, Model};
use super::{Code, Refusal, ids};
pub(crate) use svg::GENERATOR;
use svg::{Font, Point, Style, Svg, text_width};

/// What to draw.
#[derive(Debug)]
pub enum View<'a> {
    /// Modules and relationships: the parts of `scope`, or the whole model, nested `depth` deep.
    Architecture {
        scope: Option<&'a str>,
        depth: usize,
    },
    /// A flow as a sequence diagram.
    Sequence(&'a str),
    /// A flow as a flowchart.
    Flowchart(&'a str),
}

impl View<'_> {
    pub fn name(&self) -> &'static str {
        match self {
            View::Architecture { .. } => "architecture",
            View::Sequence(_) => "sequence",
            View::Flowchart(_) => "flowchart",
        }
    }
}

/// One state of the model, and how the title names it: `None` for the current state.
pub struct Side<'a> {
    pub model: &'a Model,
    pub name: Option<String>,
}

/// How to draw, beyond what: the modules lit, selected and kept, the node kinds and relationship
/// kinds left out, the group regions, whether a legend names the kinds drawn, and the colors.
#[derive(Default)]
pub struct Options<'a> {
    /// Qualified module IDs drawn as usual, the rest dimmed; none dims nothing. A focus module
    /// the group filter leaves out is not drawn, so every module drawn is dimmed.
    pub focus: &'a [String],
    /// Node kind keys whose modules the architecture view leaves out.
    pub hide: &'a [String],
    /// Qualified module IDs the architecture view outlines as selected.
    pub selection: &'a [String],
    /// Relationship kind keys the architecture view draws; empty draws every kind.
    pub relationships: &'a [String],
    /// Group keys whose members alone the architecture view draws as usual, with the modules
    /// they have relationships with dimmed; without a focus the members light as one would.
    /// Empty draws every module; a group with no module drawn is refused.
    pub groups: &'a [String],
    /// Group keys the architecture view draws as labelled regions behind their members.
    pub regions: &'a [String],
    /// How many relationship hops from the focus the architecture view keeps drawn; `None`, or
    /// no focus, keeps every module.
    pub visibility: Option<usize>,
    /// Name the node and relationship kinds the architecture view draws, below it.
    pub legend: bool,
    pub theme: Theme,
}

/// The colors of a drawing: light, dark, or light unless the viewer prefers dark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Light,
    Dark,
    Auto,
}

/// A finished drawing and what it shows.
#[derive(Debug)]
pub struct Rendered {
    pub svg: String,
    /// The revision digest of the state drawn.
    pub revision: String,
    pub width: f64,
    pub height: f64,
    /// Qualified IDs of the modules drawn: boxes, or a flow's lifelines and participants.
    pub modules: Vec<String>,
    /// Qualified IDs of the relationships the arrows stand for.
    pub relationships: Vec<String>,
    pub fallbacks: Fallbacks,
}

/// Arrows and labels the architecture layout could not place as it prefers; zero for flows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fallbacks {
    /// Routes that found no path around the boxes and took the plain bend.
    pub routes: usize,
    /// Labels that found no free spot and sit at the middle of their route's middle segment.
    pub labels: usize,
}

/// How a title names the current state.
const CURRENT_NAME: &str = "the current state";

/// How deep the architecture view nests containers unless asked otherwise.
pub const DEFAULT_DEPTH: usize = 2;

const MARGIN: f64 = 24.0;
const TITLE_SIZE: f64 = 15.0;
/// Room the title line takes above the picture.
const TITLE_H: f64 = 36.0;
/// How far below the middle of a box a one-line label's baseline sits.
const BASELINE: f64 = 4.0;
const INK: &str = "#1f2933";
const SOFT_INK: &str = "#616e7c";
const LINE: &str = "#3e4c59";
const MUTED_LINE: &str = "#9aa5b1";
const LINE_WIDTH: f64 = 1.3;
const HERO_WIDTH: f64 = 2.8;
const MARKED_WIDTH: f64 = 2.2;
const OUTLINE_WIDTH: f64 = 1.0;
const MUTED_OPACITY: f64 = 0.5;
const REMOVED_OPACITY: f64 = 0.4;
const REMOVED_TEXT_OPACITY: f64 = 0.6;
const ADDED: &str = "#2f9e44";
const REMOVED: &str = "#e03131";
const CHANGED: &str = "#f08c00";
const BLUE: &str = "#dbe9f6";
const LAVENDER: &str = "#ece4f7";
const TEAL: &str = "#ddf1f3";
const PERIWINKLE: &str = "#e3e7fa";
/// Fills for node kinds without a configured color, none reading as an added, removed or changed
/// mark; a kind's fill is picked by a hash of its key, so adding a kind recolors no other.
const PALETTE: [&str; 4] = [BLUE, LAVENDER, TEAL, PERIWINKLE];
const EXTERNAL_FILL: &str = "#f1f3f5";
/// Fill of lifeline heads, frame tags, loop headers and decisions.
const HEAD_FILL: &str = "#edf2f7";
const DASHED: &str = "6 4";
const DOTTED: &str = "2 3";
/// A dashed rule: lifelines and the separators between a frame's sections.
const RULE: Style<'static> = Style {
    fill: None,
    fill_opacity: None,
    stroke: Some(SOFT_INK),
    width: None,
    dash: Some(DASHED),
    opacity: None,
};
const LEGEND_SWATCH: f64 = 14.0;
const LEGEND_STEP: f64 = 96.0;
const LEGEND_GAP: f64 = 6.0;
const LEGEND_LINE: f64 = 28.0;
const LEGEND_SPACE: f64 = 18.0;
const SWATCH_RADIUS: f64 = 2.0;
/// The outline of a selected module.
const SELECTED: &str = "#1c7ed6";
const SELECTED_WIDTH: f64 = 2.6;

/// How a comparison marks an element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Added,
    Removed,
    Changed,
}

impl Mark {
    const ALL: [Mark; 3] = [Mark::Added, Mark::Removed, Mark::Changed];

    fn class(self) -> &'static str {
        match self {
            Mark::Added => "added",
            Mark::Removed => "removed",
            Mark::Changed => "changed",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Mark::Added => ADDED,
            Mark::Removed => REMOVED,
            Mark::Changed => CHANGED,
        }
    }

    /// One mark for several elements drawn as one: theirs when they agree, else changed.
    fn merge(a: Option<Mark>, b: Option<Mark>) -> Option<Mark> {
        if a == b { a } else { Some(Mark::Changed) }
    }
}

/// What one render draws with: the vocabulary, module names, comparison marks, the focus and the
/// title.
struct Context<'a> {
    model: &'a str,
    config: &'a VaultConfig,
    names: BTreeMap<String, &'a str>,
    /// `None` when nothing is compared; marks by qualified ID otherwise.
    marks: Option<BTreeMap<String, Mark>>,
    /// `None` when nothing is focused.
    focus: Option<Focus>,
    /// Qualified IDs of the modules outlined as selected.
    selection: BTreeSet<String>,
    title: String,
}

/// The modules a focus highlights, by qualified ID: the focused ones and everything inside them.
/// The containers around them are context and stay undimmed; everything else is dimmed.
struct Focus {
    lit: BTreeSet<String>,
    around: BTreeSet<String>,
}

impl Focus {
    /// The focus on the modules `focus` among `modules`, `None` for an empty list.
    fn new(model: &str, modules: &[Module], focus: &[String]) -> Option<Self> {
        if focus.is_empty() {
            return None;
        }
        let index = Index::new(model, modules);
        let (mut lit, mut around) = (BTreeSet::new(), BTreeSet::new());
        for id in focus {
            lit.insert(id.clone());
            lit.extend(index.descendants(id).into_iter().map(|(d, _)| d.to_owned()));
            around.extend(index.chain(id).into_iter().skip(1).map(str::to_owned));
        }
        Some(Self { lit, around })
    }
}

impl Context<'_> {
    fn qualify(&self, local: &str) -> String {
        ids::qualify(self.model, local)
    }

    fn name<'s>(&'s self, id: &'s str) -> &'s str {
        self.names.get(id).copied().unwrap_or(id)
    }

    /// The mark of the element with qualified ID `id`.
    fn mark(&self, id: &str) -> Option<Mark> {
        self.marks.as_ref()?.get(id).copied()
    }

    /// Whether the module `id` is drawn dimmed: a focus neither holds it nor lies inside it.
    fn dims(&self, id: &str) -> bool {
        (self.focus.as_ref()).is_some_and(|f| !f.lit.contains(id) && !f.around.contains(id))
    }

    /// Whether a message between `from` and `to` is drawn dimmed: a focus holds neither end.
    fn dims_message(&self, from: &str, to: &str) -> bool {
        (self.focus.as_ref()).is_some_and(|f| !f.lit.contains(from) && !f.lit.contains(to))
    }

    fn kind_name<'s>(&'s self, kind: &'s str) -> &'s str {
        self.config.node_kinds.get(kind).map_or(kind, |k| &k.name)
    }

    fn relationship_name<'s>(&'s self, kind: &'s str) -> &'s str {
        (self.config.relationship_kinds.get(kind)).map_or(kind, |k| &k.name)
    }

    /// The node kind's configured color when it is `#rgb` or `#rrggbb`; else gray for external
    /// modules and a palette color for the rest.
    fn fill(&self, kind: &str) -> &str {
        let configured = (self.config.node_kinds.get(kind))
            .and_then(|k| k.color.as_deref())
            .filter(|c| is_color(c));
        configured.unwrap_or(match kind {
            EXTERNAL_KIND => EXTERNAL_FILL,
            _ => PALETTE[(stable_hash(kind) % PALETTE.len() as u64) as usize],
        })
    }

    /// The first valid configured color of the groups `groups`.
    fn group_color(&self, groups: &[String]) -> Option<&str> {
        (groups.iter())
            .filter_map(|g| self.config.groups.get(g)?.color.as_deref())
            .find(|c| is_color(c))
    }

    fn dash(&self, kind: &str) -> Option<&'static str> {
        match self.config.relationship_kinds.get(kind).map(|k| k.dash) {
            Some(Dash::Dashed) => Some(DASHED),
            Some(Dash::Dotted) => Some(DOTTED),
            Some(Dash::Solid) | None => None,
        }
    }

    fn title(&self, svg: &mut Svg) {
        let font = Font {
            size: Some(TITLE_SIZE),
            bold: true,
            fill: Some(INK),
            ..Font::default()
        };
        svg.text((MARGIN, MARGIN + TITLE_SIZE), &self.title, &font);
    }

    fn title_area(&self) -> layout::Rect {
        layout::Rect {
            x: MARGIN,
            y: MARGIN,
            w: text_width(&self.title) * TITLE_SIZE / svg::FONT_SIZE,
            h: TITLE_SIZE + svg::DESCENT,
        }
    }

    fn kinds(&self, svg: &mut Svg, nodes: &BTreeSet<&str>, lines: &BTreeSet<&str>) {
        let mut y = svg.bottom() + MARGIN;
        svg.group("legend kinds", None);
        let mut x = MARGIN;
        for &kind in nodes {
            let mut style = outline(None, self.fill(kind));
            if kind == EXTERNAL_KIND {
                style.dash = Some(DASHED);
            }
            svg.rect(
                (x, y),
                (LEGEND_SWATCH, LEGEND_SWATCH),
                SWATCH_RADIUS,
                &style,
            );
            x = legend_name(svg, (x + LEGEND_SWATCH, y), self.kind_name(kind));
        }
        if !nodes.is_empty() {
            y += LEGEND_SWATCH + LEGEND_GAP;
        }
        x = MARGIN;
        for &kind in lines {
            let mid = y + LEGEND_SWATCH / 2.0;
            let style = line_style(None, None, self.dash(kind));
            svg.polyline(&[(x, mid), (x + LEGEND_LINE, mid)], &style);
            x = legend_name(svg, (x + LEGEND_LINE, y), self.relationship_name(kind));
        }
        svg.close_group();
    }

    /// Below everything drawn so far, the legend when marks are on.
    fn legend(&self, svg: &mut Svg) {
        if self.marks.is_none() {
            return;
        }
        let y = svg.bottom() + MARGIN;
        svg.group("legend", None);
        for (k, mark) in Mark::ALL.into_iter().enumerate() {
            let x = MARGIN + LEGEND_STEP * k as f64;
            let style = Style {
                fill: Some(mark.color()),
                ..Style::default()
            };
            let swatch = (LEGEND_SWATCH, LEGEND_SWATCH);
            svg.rect((x, y), swatch, SWATCH_RADIUS, &style);
            let at = (x + LEGEND_SWATCH + LEGEND_GAP, y + LEGEND_SWATCH - BASELINE);
            svg.text(at, mark.class(), &label(None));
        }
        svg.close_group();
    }
}

fn legend_name(svg: &mut Svg, at: Point, name: &str) -> f64 {
    let x = at.0 + LEGEND_GAP;
    svg.text((x, at.1 + LEGEND_SWATCH - BASELINE), name, &label(None));
    x + text_width(name) + LEGEND_SPACE
}

/// The finished `svg` in `theme`: the light drawing as it is, or with a style sheet that recolors
/// it dark, always or when the viewer prefers dark. The sheet matches the light colors the
/// drawing writes as attributes, which a style rule overrides, and lets fills show through so
/// configured node colors stay readable under light text.
fn themed(svg: String, theme: Theme) -> String {
    let sheet = match theme {
        Theme::Light => return svg,
        Theme::Dark => dark(),
        Theme::Auto => format!("@media (prefers-color-scheme: dark){{{}}}", dark()),
    };
    let at = svg.find('\n').map_or(svg.len(), |end| end + 1);
    format!("{}<style>{sheet}</style>\n{}", &svg[..at], &svg[at..])
}

fn dark() -> String {
    let swaps = [
        ("fill", svg::BACKGROUND, "#16191d"),
        ("fill", INK, "#e4e7eb"),
        ("fill", SOFT_INK, "#9aa5b1"),
        ("stroke", SOFT_INK, "#9aa5b1"),
        ("fill", LINE, "#c3cbd3"),
        ("stroke", LINE, "#c3cbd3"),
        ("fill", MUTED_LINE, "#616e7c"),
        ("stroke", MUTED_LINE, "#616e7c"),
        ("fill", HEAD_FILL, "#2a3038"),
        ("fill", sequence::NOTE_FILL, "#3d3720"),
    ];
    let mut sheet = String::from("svg{color:#e4e7eb}");
    for (key, light, dark) in swaps {
        let _ = write!(sheet, "[{key}=\"{light}\"]{{{key}:{dark}}}");
    }
    sheet.push_str(".module rect,.step rect,.step polygon,.legend rect{fill-opacity:0.3}");
    sheet
}

/// Whether `text` is a color as `#rgb` or `#rrggbb`.
fn is_color(text: &str) -> bool {
    text.strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// FNV-1a over `text`: the same number on every run and platform.
fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The stroke of a line with `emphasis` and `mark`.
fn line_style(
    emphasis: Option<Emphasis>,
    mark: Option<Mark>,
    dash: Option<&'static str>,
) -> Style<'static> {
    let hero = emphasis == Some(Emphasis::Hero);
    let muted = emphasis == Some(Emphasis::Muted);
    let width = match (hero, mark) {
        (true, _) => HERO_WIDTH,
        (false, Some(_)) => MARKED_WIDTH,
        (false, None) => LINE_WIDTH,
    };
    let stroke = match (mark, muted) {
        (Some(mark), _) => mark.color(),
        (None, true) => MUTED_LINE,
        (None, false) => LINE,
    };
    let opacity = match (mark, muted) {
        (Some(Mark::Removed), _) => Some(REMOVED_OPACITY),
        (_, true) => Some(MUTED_OPACITY),
        _ => None,
    };
    Style {
        stroke: Some(stroke),
        width: Some(width),
        dash,
        opacity,
        ..Style::default()
    }
}

/// A shape's outline for `mark`: its color and a heavier stroke, faded when removed.
fn outline<'a>(mark: Option<Mark>, fill: &'a str) -> Style<'a> {
    Style {
        fill: Some(fill),
        stroke: Some(mark.map_or(SOFT_INK, Mark::color)),
        width: Some(if mark.is_some() {
            MARKED_WIDTH
        } else {
            OUTLINE_WIDTH
        }),
        opacity: (mark == Some(Mark::Removed)).then_some(REMOVED_OPACITY),
        ..Style::default()
    }
}

/// The font of a label on an element with `mark`: removed labels are faded and struck through.
fn label(mark: Option<Mark>) -> Font<'static> {
    let removed = mark == Some(Mark::Removed);
    Font {
        fill: Some(INK),
        struck: removed,
        opacity: removed.then_some(REMOVED_TEXT_OPACITY),
        ..Font::default()
    }
}

/// A group class: `base` and the mark's class.
fn class(base: &str, mark: Option<Mark>) -> Cow<'_, str> {
    match mark {
        Some(mark) => Cow::Owned(format!("{base} {}", mark.class())),
        None => Cow::Borrowed(base),
    }
}

/// Draws `view` of the state `drawn`. With `base`, marks what `drawn` adds, removes and changes
/// against it; removed modules, relationships and steps are drawn too, faded and struck. With a
/// focus of qualified module IDs, those modules and what touches them are drawn as usual and
/// the rest dimmed. Hidden node kinds and the kinds legend apply to the architecture view only;
/// hidden kinds must be configured.
pub fn render(
    drawn: &Side<'_>,
    base: Option<&Side<'_>>,
    view: &View<'_>,
    options: &Options<'_>,
) -> Result<Rendered, Refusal> {
    let model = drawn.model;
    let id = model.manifest.id.as_str();
    let architecture = matches!(view, View::Architecture { .. });
    if !architecture && (!options.hide.is_empty() || options.legend) {
        let message = "--hide-kind and --legend apply to the architecture view only";
        return Err(Refusal::new(Code::RequestInvalid, message));
    }
    let arranged = !options.selection.is_empty()
        || !options.relationships.is_empty()
        || !options.groups.is_empty()
        || !options.regions.is_empty()
        || options.visibility.is_some();
    if !architecture && arranged {
        let message = "--group, selection, filters, groups and visibility apply to the architecture view only";
        return Err(Refusal::new(Code::RequestInvalid, message));
    }
    let config = &model.config;
    let groups = keys(&config.groups);
    let asked = [
        (
            "--hide-kind",
            "node kind",
            options.hide,
            keys(&config.node_kinds),
        ),
        (
            "relationship filter",
            "relationship kind",
            options.relationships,
            keys(&config.relationship_kinds),
        ),
        ("--group", "group", options.groups, groups.clone()),
        ("group region", "group", options.regions, groups),
    ];
    for (flag, what, wanted, known) in asked {
        if let Some(key) = wanted.iter().find(|k| !known.contains(&k.as_str())) {
            let message = format!(
                "{flag} `{key}` is not a {what} in config.yaml; known: {}",
                known.join(", ")
            );
            return Err(Refusal::new(Code::RequestInvalid, message));
        }
    }
    let comparison = base.map(|b| {
        compare::compare(
            id,
            (&b.model.modules, &b.model.collections),
            (&model.modules, &model.collections),
        )
    });
    let compared = base.map(|b| b.model).zip(comparison.as_ref());
    let modules = match compared {
        Some((before, c)) => with_removed(id, &model.modules, &before.modules, c),
        None => Cow::Borrowed(model.modules.as_slice()),
    };
    let modules = without_kinds(id, modules, options.hide);
    let (modules, members) = kept(id, modules, options)?;
    let focus = match options.focus {
        [] => members.as_slice(),
        focus => focus,
    };
    let mut context = Context {
        model: id,
        config: &model.config,
        names: (modules.iter())
            .map(|m| (ids::qualify(id, &m.id), m.name.as_str()))
            .collect(),
        marks: comparison.as_ref().map(marks),
        focus: Focus::new(id, &modules, focus),
        selection: options.selection.iter().cloned().collect(),
        title: title(drawn, base),
    };
    let (svg, drawn_modules, relationships, fallbacks) = match *view {
        View::Architecture { scope, depth } => {
            draw_architecture(&mut context, model, &modules, (scope, depth), options)?
        }
        View::Sequence(subject) | View::Flowchart(subject) => {
            let (_, flow) = model.flow(subject)?;
            let _ = write!(context.title, ", flow {}", flow.name);
            let steps = match compared {
                Some((before, c)) => with_removed_steps(id, flow, before, &c.steps),
                None => Cow::Borrowed(flow.steps.as_slice()),
            };
            let (svg, shown) = match view {
                View::Sequence(_) => {
                    let names = lineup(&flow.participants, &steps);
                    (sequence::render(&context, &names, &steps), names)
                }
                _ => (flowchart::render(&context, &steps), participants(&steps)),
            };
            let shown = shown.into_iter().map(str::to_owned).collect();
            (svg, shown, Vec::new(), Fallbacks::default())
        }
    };
    let (_, _, width, height) = svg.frame();
    let svg = themed(svg.finish(), options.theme);
    Ok(Rendered {
        svg,
        revision: model.revision.digest.clone(),
        width,
        height,
        modules: drawn_modules,
        relationships,
        fallbacks,
    })
}

/// `modules` cut to what `options` keep: relationships of the kinds asked; the members of the
/// groups asked and the other ends of their relationships; what lies within `visibility` hops of
/// the focus; each with the containers around it. Returns the members too, by qualified ID.
/// Refuses a group asked that has no member among `modules`, which would draw none of it.
fn kept<'a>(
    model: &str,
    mut modules: Cow<'a, [Module]>,
    options: &Options<'_>,
) -> Result<(Cow<'a, [Module]>, Vec<String>), Refusal> {
    let kinds = options.relationships;
    let unasked = |m: &Module| m.outgoing.iter().any(|r| !kinds.contains(&r.kind));
    if !kinds.is_empty() && modules.iter().any(unasked) {
        for module in modules.to_mut() {
            module.outgoing.retain(|r| kinds.contains(&r.kind));
        }
    }
    let member = |m: &Module, group: &String| m.groups.contains(group);
    if let Some(group) = (options.groups.iter()).find(|g| !modules.iter().any(|m| member(m, g))) {
        let message = format!("group `{group}` has no module drawn; name a group with members");
        return Err(Refusal::new(Code::RequestInvalid, message));
    }
    let members: Vec<String> = (modules.iter())
        .filter(|m| options.groups.iter().any(|g| member(m, g)))
        .map(|m| ids::qualify(model, &m.id))
        .collect();
    let keep = {
        let index = Index::new(model, &modules);
        let grouped = (!options.groups.is_empty()).then(|| reach(&index, &members, 1));
        let near = (options.visibility)
            .filter(|_| !options.focus.is_empty())
            .map(|hops| reach(&index, options.focus, hops));
        let keep = match (grouped, near) {
            (Some(a), Some(b)) => Some(a.intersection(&b).cloned().collect()),
            (a, b) => a.or(b),
        };
        keep.map(|keep: BTreeSet<String>| {
            let around = keep.iter().flat_map(|id| index.chain(id));
            around.map(str::to_owned).collect::<BTreeSet<String>>()
        })
    };
    let Some(keep) = keep else {
        return Ok((modules, members));
    };
    let kept = |m: &Module| keep.contains(&ids::qualify(model, &m.id));
    let modules = match modules {
        Cow::Owned(mut owned) => {
            owned.retain(kept);
            owned
        }
        Cow::Borrowed(all) => all.iter().filter(|m| kept(m)).cloned().collect(),
    };
    Ok((Cow::Owned(modules), members))
}

/// `start`, the modules inside it, and every module within `hops` relationships of them, by
/// qualified ID.
fn reach(index: &Index<'_>, start: &[String], hops: usize) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for id in start.iter().filter(|id| index.by_id.contains_key(*id)) {
        found.insert(id.clone());
        found.extend(index.descendants(id).into_iter().map(|(d, _)| d.to_owned()));
    }
    for _ in 0..hops {
        let mut next = found.clone();
        for line in &index.lines {
            if found.contains(&line.from) {
                next.insert(line.to.to_owned());
            }
            if found.contains(line.to) {
                next.insert(line.from.clone());
            }
        }
        if next.len() == found.len() {
            break;
        }
        found = next;
    }
    found
}

fn keys<V>(map: &BTreeMap<String, V>) -> Vec<&str> {
    map.keys().map(String::as_str).collect()
}

fn without_kinds<'a>(
    model: &str,
    modules: Cow<'a, [Module]>,
    hide: &[String],
) -> Cow<'a, [Module]> {
    if !modules.iter().any(|m| hide.contains(&m.kind)) {
        return modules;
    }
    let parents: BTreeMap<String, (Option<String>, bool)> = (modules.iter())
        .map(|m| {
            (
                ids::qualify(model, &m.id),
                (m.parent.clone(), hide.contains(&m.kind)),
            )
        })
        .collect();
    let shown_parent = |mut parent: Option<String>| {
        while let Some((above, true)) = parent.as_ref().and_then(|p| parents.get(p)) {
            parent.clone_from(above);
        }
        parent
    };
    let kept = (modules.iter())
        .filter(|m| !hide.contains(&m.kind))
        .map(|m| Module {
            parent: shown_parent(m.parent.clone()),
            ..m.clone()
        });
    Cow::Owned(kept.collect())
}

/// The modules `subjects` name in `model` and their direct neighbours: the other end of every
/// relationship from or to a subject or a part inside it, by qualified ID. A focus on these
/// lights each subject with what it talks to.
pub fn neighbourhood(model: &Model, subjects: &[String]) -> Result<Vec<String>, Refusal> {
    let id = model.manifest.id.as_str();
    let index = Index::new(id, &model.modules);
    let mut lit = BTreeSet::new();
    let mut inside = BTreeSet::new();
    for subject in subjects {
        let found = ids::qualify(id, &vault::find(id, &model.modules, subject)?.id);
        inside.extend(
            index
                .descendants(&found)
                .into_iter()
                .map(|(d, _)| d.to_owned()),
        );
        inside.insert(found.clone());
        lit.insert(found);
    }
    for module in &model.modules {
        let from = ids::qualify(id, &module.id);
        for r in &module.outgoing {
            if inside.contains(&from) {
                lit.insert(r.to.clone());
            }
            if inside.contains(&r.to) {
                lit.insert(from.clone());
            }
        }
    }
    Ok(lit.into_iter().collect())
}

/// The architecture view of `modules`, the parts of `scope` when given, `depth` levels deep.
fn draw_architecture(
    context: &mut Context<'_>,
    model: &Model,
    modules: &[Module],
    (scope, depth): (Option<&str>, usize),
    options: &Options<'_>,
) -> Result<(Svg, Vec<String>, Vec<String>, Fallbacks), Refusal> {
    if depth == 0 {
        return Err(Refusal::new(
            Code::RequestInvalid,
            "--depth must be at least 1",
        ));
    }
    let scope = (scope.map(|s| vault::find(&model.manifest.id, &model.modules, s))).transpose()?;
    if let Some(scope) = scope {
        if options.hide.contains(&scope.kind) {
            let message = format!("--scope {} is of a kind --hide-kind leaves out", scope.name);
            return Err(Refusal::new(Code::RequestInvalid, message));
        }
        let _ = write!(context.title, ", parts of {}", scope.name);
    }
    let scope = scope.map(|m| context.qualify(&m.id));
    Ok(architecture::render(
        context,
        modules,
        scope.as_deref(),
        depth,
        (options.regions, options.legend),
    ))
}

/// The model's name, the drawn state's unless it is the current state drawn alone, and the
/// state it is compared with.
fn title(drawn: &Side<'_>, base: Option<&Side<'_>>) -> String {
    let name = |side: &Side<'_>| side.name.as_deref().unwrap_or(CURRENT_NAME).to_owned();
    let mut title = drawn.model.manifest.name.clone();
    if drawn.name.is_some() || base.is_some() {
        let _ = write!(title, ", {}", name(drawn));
    }
    if let Some(base) = base {
        let _ = write!(title, ", compared with {}", name(base));
    }
    title
}

/// The marks of a comparison by qualified ID.
fn marks(comparison: &Comparison) -> BTreeMap<String, Mark> {
    let mut marks = BTreeMap::new();
    for (_, changes) in comparison.sections() {
        let lists = [
            (Mark::Added, &changes.added),
            (Mark::Removed, &changes.removed),
            (Mark::Changed, &changes.changed),
        ];
        for (mark, entries) in lists {
            marks.extend(entries.iter().map(|e| (e.id.clone(), mark)));
        }
    }
    marks
}

fn locals<'a>(model: &str, entries: &'a [Entry]) -> BTreeSet<&'a str> {
    entries.iter().map(|e| ids::local(model, &e.id)).collect()
}

/// `after` with the modules and relationships the comparison removed put back from `before`.
fn with_removed<'a>(
    model: &str,
    after: &'a [Module],
    before: &[Module],
    c: &Comparison,
) -> Cow<'a, [Module]> {
    if c.modules.removed.is_empty() && c.relationships.removed.is_empty() {
        return Cow::Borrowed(after);
    }
    let removed_modules = locals(model, &c.modules.removed);
    let removed_lines = locals(model, &c.relationships.removed);
    let mut merged = after.to_vec();
    merged.extend(
        (before.iter())
            .filter(|m| removed_modules.contains(m.id.as_str()))
            .map(|m| Module {
                outgoing: Vec::new(),
                ..m.clone()
            }),
    );
    let owners: BTreeMap<String, usize> = (merged.iter().enumerate())
        .map(|(i, m)| (m.id.clone(), i))
        .collect();
    for module in before {
        if let Some(&owner) = owners.get(&module.id) {
            let lines = module
                .outgoing
                .iter()
                .filter(|r| removed_lines.contains(r.id.as_str()));
            merged[owner].outgoing.extend(lines.cloned());
        }
    }
    Cow::Owned(merged)
}

/// The steps of `flow` with the steps the comparison removed put back where they stood in
/// `before`, when the list that held them is still there.
fn with_removed_steps<'a>(
    model: &str,
    flow: &'a Flow,
    before: &Model,
    steps: &Changes,
) -> Cow<'a, [Step]> {
    let Ok((_, was)) = before.flow(&flow.id) else {
        return Cow::Borrowed(&flow.steps);
    };
    if steps.removed.is_empty() {
        return Cow::Borrowed(&flow.steps);
    }
    let removed = locals(model, &steps.removed);
    let mut merged = flow.steps.clone();
    restore(&mut merged, &flow.id, &flow.id, &was.steps, &removed);
    Cow::Owned(merged)
}

/// Puts each removed step of `old`, the list `list` of the old flow, back into the same list of
/// `merged` at its old index; looks inside steps that stayed for removed steps of their own.
fn restore(merged: &mut Vec<Step>, flow: &str, list: &str, old: &[Step], removed: &BTreeSet<&str>) {
    for (index, step) in old.iter().enumerate() {
        if removed.contains(step.id()) {
            let target = if list == flow {
                Some(&mut *merged)
            } else {
                list_mut(merged, list)
            };
            if let Some(target) = target {
                target.insert(index.min(target.len()), pruned(step, removed));
            }
            continue;
        }
        for (inner, steps) in step.lists() {
            restore(merged, flow, inner, steps, removed);
        }
    }
}

/// `step` holding only the steps inside it that were removed too; the others are drawn where
/// they stand now.
fn pruned(step: &Step, removed: &BTreeSet<&str>) -> Step {
    let mut step = step.clone();
    for (_, list) in step.lists_mut() {
        list.retain(|s| removed.contains(s.id()));
        for inner in list.iter_mut() {
            *inner = pruned(inner, removed);
        }
    }
    step
}

/// The branch or loop list with ID `id` among `steps` at any depth.
fn list_mut<'a>(steps: &'a mut [Step], id: &str) -> Option<&'a mut Vec<Step>> {
    for step in steps.iter_mut() {
        for (list_id, list) in step.lists_mut() {
            if list_id == id {
                return Some(list);
            }
            if let Some(found) = list_mut(list, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Every message's sender and receiver in first-appearance order.
fn participants(steps: &[Step]) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for (step, _) in walk(steps) {
        if let Step::Message { from, to, .. } = step {
            for end in [from.as_str(), to.as_str()] {
                if seen.insert(end) {
                    order.push(end);
                }
            }
        }
    }
    order
}

/// The modules `listed` in order, then every other message end of `steps` in first-appearance
/// order.
pub(super) fn lineup<'a>(listed: &'a [String], steps: &'a [Step]) -> Vec<&'a str> {
    let mut order: Vec<&str> = listed.iter().map(String::as_str).collect();
    let mut seen: HashSet<&str> = order.iter().copied().collect();
    for end in participants(steps) {
        if seen.insert(end) {
            order.push(end);
        }
    }
    order
}

/// The line an empty flow draws instead of steps.
fn no_steps(svg: &mut Svg) {
    let font = Font {
        fill: Some(SOFT_INK),
        ..Font::default()
    };
    svg.text((MARGIN, MARGIN + TITLE_H + TITLE_SIZE), "no steps", &font);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::revision::Revision;
    use crate::arch::types::{Collection, Manifest, State};
    use serde_json::{Value, json};

    const M: &str = "Model001";

    pub(super) fn q(local: &str) -> String {
        format!("{M}#{local}")
    }

    pub(super) fn module(value: Value) -> Module {
        let mut base = json!({"schema_version": 1, "kind": "service", "order": 0});
        base.as_object_mut()
            .unwrap()
            .extend(value.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    pub(super) fn model(modules: Vec<Module>, collections: Vec<Collection>) -> Model {
        Model {
            dir: "/vault".into(),
            manifest: Manifest {
                schema_version: 1,
                id: M.into(),
                name: "Shop".into(),
                current: Some(State {
                    id: "State001".into(),
                    name: "Current".into(),
                }),
                retired_ids: Vec::new(),
            },
            config: VaultConfig::default(),
            modules,
            collections,
            walkthroughs: Vec::new(),
            pictures: Vec::new(),
            views: Vec::new(),
            revision: Revision {
                digest: "x".into(),
                files: Vec::new(),
            },
            proposal: None,
        }
    }

    /// One flow named Checkout holding `steps`.
    pub(super) fn flow(steps: Value) -> Vec<Collection> {
        serde_json::from_value(json!([{
            "schema_version": 1, "id": "Coll0001", "name": "Orders", "order": 0,
            "flows": [{"id": "Flow0001", "name": "Checkout", "order": 0, "steps": steps}],
        }]))
        .unwrap()
    }

    /// `view` of the current state `model`, compared with nothing.
    pub(super) fn draw(model: &Model, view: &View<'_>) -> Result<String, Refusal> {
        let drawn = Side { model, name: None };
        render(&drawn, None, view, &Options::default()).map(|r| r.svg)
    }

    const WHOLE: View<'static> = View::Architecture {
        scope: None,
        depth: DEFAULT_DEPTH,
    };

    fn shop() -> Model {
        model(
            vec![
                module(json!({"id": "Platfrm1", "name": "Plat<&\"form", "kind": "system"})),
                module(
                    json!({"id": "WebApp01", "name": "Web", "parent": q("Platfrm1"),
                    "outgoing": [{"id": "RelWeb01", "to": q("Billing1"), "kind": "http", "order": 0}]}),
                ),
                module(
                    json!({"id": "Billing1", "name": "Billing", "parent": q("Platfrm1"),
                    "outgoing": [{"id": "RelBil01", "to": q("BankApi1"), "kind": "call", "order": 0}]}),
                ),
                module(json!({"id": "BankApi1", "name": "Bank", "kind": "external"})),
            ],
            Vec::new(),
        )
    }

    /// Asserts every text of `svg`, at its estimated width, lies inside the viewBox.
    fn assert_texts_inside(svg: &str) {
        let attr = |tag: &str, key: &str| -> Option<String> {
            let start = tag.find(&format!(" {key}=\""))? + key.len() + 3;
            Some(tag[start..start + tag[start..].find('"')?].to_owned())
        };
        let view: Vec<f64> = (attr(svg, "viewBox").unwrap().split(' '))
            .map(|n| n.parse().unwrap())
            .collect();
        for (at, _) in svg.match_indices("<text") {
            let tag = &svg[at..at + svg[at..].find('>').unwrap()];
            let content = &svg[at + tag.len() + 1..at + svg[at..].find("</text>").unwrap()];
            let text = content.replace("&gt;", ">").replace("&lt;", "<");
            let size: f64 = attr(tag, "font-size").map_or(svg::FONT_SIZE, |s| s.parse().unwrap());
            let width = text_width(&text) * size / svg::FONT_SIZE;
            let x: f64 = attr(tag, "x").unwrap().parse().unwrap();
            let left = if tag.contains("text-anchor=\"middle\"") {
                x - width / 2.0
            } else {
                x
            };
            assert!(
                left >= view[0] && left + width <= view[0] + view[2],
                "{content} outside"
            );
        }
    }

    #[test]
    fn names_are_escaped() {
        let out = draw(&shop(), &WHOLE).unwrap();
        assert!(out.contains("Plat&lt;&amp;&quot;form"), "{out}");
        assert!(!out.contains("Plat<&"));
    }

    #[test]
    fn rendering_is_deterministic() {
        let first = draw(&shop(), &WHOLE).unwrap();
        assert_eq!(first, draw(&shop(), &WHOLE).unwrap());
    }

    #[test]
    fn depth_zero_is_refused() {
        let zero = View::Architecture {
            scope: None,
            depth: 0,
        };
        assert_eq!(draw(&shop(), &zero).unwrap_err().code, Code::RequestInvalid);
    }

    #[test]
    fn long_labels_stay_inside_the_picture() {
        let long = "x".repeat(60);
        let mut shop = shop();
        shop.modules[1].outgoing[0].label = Some(long.clone());
        shop.collections = flow(json!([
            {"kind": "message", "id": "Step0001", "from": q("WebApp01"), "to": q("BankApi1"), "label": long},
            {"kind": "message", "id": "Step0002", "from": q("BankApi1"), "to": q("BankApi1"), "label": long},
        ]));
        for view in [
            WHOLE,
            View::Sequence("Checkout"),
            View::Flowchart("Checkout"),
        ] {
            let out = draw(&shop, &view).unwrap();
            assert!(out.contains(&long), "{out}");
            assert_texts_inside(&out);
        }
    }

    fn fill_of(config: &VaultConfig, kind: &str) -> String {
        let context = Context {
            model: M,
            config,
            names: BTreeMap::new(),
            marks: None,
            focus: None,
            selection: BTreeSet::new(),
            title: String::new(),
        };
        context.fill(kind).to_owned()
    }

    #[test]
    fn fills_stay_clear_of_the_mark_colors() {
        let config = VaultConfig::default();
        assert_eq!(fill_of(&config, EXTERNAL_KIND), EXTERNAL_FILL);
        for kind in config.node_kinds.keys().filter(|k| *k != EXTERNAL_KIND) {
            assert!(PALETTE.contains(&fill_of(&config, kind).as_str()), "{kind}");
        }
        // A kind's fill depends on its own key only.
        let mut more = config.clone();
        more.node_kinds
            .insert("aaa".into(), more.node_kinds["app"].clone());
        assert_eq!(fill_of(&more, "service"), fill_of(&config, "service"));
        assert!(is_color("#a1B") && is_color("#00ff00"));
        assert!(!is_color("red") && !is_color("#12345") && !is_color("#12\"3"));
    }

    #[test]
    fn an_invalid_configured_color_falls_back_to_the_palette() {
        let mut shop = shop();
        let service = shop.config.node_kinds.get_mut("service").unwrap();
        service.color = Some("url(#x)".into());
        let out = draw(&shop, &WHOLE).unwrap();
        assert!(!out.contains("url(#x)"));
    }

    #[test]
    fn a_focus_dims_what_it_leaves_out() {
        let mut shop = shop();
        shop.collections = flow(json!([
            {"kind": "message", "id": "Step0001", "from": q("WebApp01"), "to": q("Billing1"), "label": "pay"},
            {"kind": "message", "id": "Step0002", "from": q("BankApi1"), "to": q("BankApi1"), "label": "settle"},
        ]));
        let drawn = Side {
            model: &shop,
            name: None,
        };
        let focus = [q("Billing1")];
        let focused = |view: &View<'_>, focus: &[String]| {
            let options = Options {
                focus,
                ..Options::default()
            };
            render(&drawn, None, view, &options).unwrap().svg
        };
        let dimmed = |out: &str, class: &str, id: &str| {
            out.contains(&format!(
                "<g class=\"dimmed\" opacity=\"0.3\">\n<g class=\"{class}\" data-id=\"{id}\">"
            ))
        };
        // Web and Bank dim; the arrows into and out of Billing stay, as Platform around it does.
        let out = focused(&WHOLE, &focus);
        assert_eq!(out.matches("class=\"dimmed\"").count(), 2, "{out}");
        assert!(dimmed(&out, "module", &q("WebApp01")) && dimmed(&out, "module", &q("BankApi1")));
        let sequence = focused(&View::Sequence("Checkout"), &focus);
        assert!(dimmed(&sequence, "lifeline", &q("BankApi1")), "{sequence}");
        assert!(dimmed(&sequence, "message", &q("Step0002")));
        assert!(!dimmed(&sequence, "message", &q("Step0001")));
        let flowchart = focused(&View::Flowchart("Checkout"), &focus);
        assert!(
            dimmed(&flowchart, "step message", &q("Step0002")),
            "{flowchart}"
        );
        assert!(!dimmed(&flowchart, "step message", &q("Step0001")));
        assert!(!draw(&shop, &WHOLE).unwrap().contains("dimmed"));
        // A focus the drawing does not show dims nothing.
        let platform = View::Architecture {
            scope: Some("Platfrm1"),
            depth: DEFAULT_DEPTH,
        };
        let bank = [q("BankApi1")];
        let inside = focused(&platform, &bank);
        assert!(!inside.contains("dimmed"), "{inside}");
    }

    #[test]
    fn readings_select_filter_and_keep_what_is_near() {
        let mut shop = shop();
        let pay = serde_json::from_value(json!({"name": "Payments"})).unwrap();
        shop.config.groups.insert("pay".to_owned(), pay);
        let quiet = serde_json::from_value(json!({"name": "Quiet"})).unwrap();
        shop.config.groups.insert("idle".to_owned(), quiet);
        shop.modules[2].groups = vec!["pay".to_owned()];
        shop.modules
            .push(module(json!({"id": "AuditLg1", "name": "Audit"})));
        let drawn = Side {
            model: &shop,
            name: None,
        };
        let draw = |options: &Options<'_>| render(&drawn, None, &WHOLE, options);
        let (web, billing, bank) = (q("WebApp01"), q("Billing1"), q("BankApi1"));
        let box_of = |out: &str, id: &str| out.contains(&format!("data-id=\"{id}\""));

        let selection = [web.clone()];
        let out = draw(&Options {
            selection: &selection,
            ..Options::default()
        })
        .unwrap()
        .svg;
        let group = format!("<g class=\"module selected\" data-id=\"{web}\">\n<rect");
        let at = out.find(&group).unwrap_or_else(|| panic!("{out}"));
        assert!(out[at..].starts_with(&format!("{group} x=")));
        let stroke = format!("stroke=\"{SELECTED}\"");
        assert!(out[at..out[at..].find("/>").unwrap() + at].contains(&stroke));
        assert!(!out.contains("dimmed"));

        let calls = ["call".to_owned()];
        let out = draw(&Options {
            relationships: &calls,
            ..Options::default()
        })
        .unwrap()
        .svg;
        assert!(
            !box_of(&out, &q("RelWeb01")) && box_of(&out, &q("RelBil01")),
            "{out}"
        );

        // Billing's group keeps Billing lit, Web and Bank dimmed beside it, and Audit out.
        let pay = ["pay".to_owned()];
        let out = draw(&Options {
            groups: &pay,
            ..Options::default()
        })
        .unwrap();
        assert!(!box_of(&out.svg, &q("AuditLg1")), "{}", out.svg);
        assert_eq!(
            out.modules,
            [bank.clone(), q("Platfrm1"), billing.clone(), web.clone()]
        );
        assert_eq!(
            out.svg.matches("class=\"dimmed\"").count(),
            2,
            "{}",
            out.svg
        );

        let focus = [web.clone()];
        let near = |hops| {
            draw(&Options {
                focus: &focus,
                visibility: Some(hops),
                ..Options::default()
            })
            .unwrap()
            .modules
        };
        assert_eq!(near(0), [q("Platfrm1"), web.clone()]);
        assert_eq!(near(1), [q("Platfrm1"), billing.clone(), web.clone()]);
        assert_eq!(near(2), [bank, q("Platfrm1"), billing, web]);

        let regions = Options {
            regions: &pay,
            ..Options::default()
        };
        let out = draw(&regions).unwrap().svg;
        let region = out
            .find("<g class=\"region\">")
            .unwrap_or_else(|| panic!("{out}"));
        assert!(region < out.find("<g class=\"module").unwrap());
        assert!(out[region..].contains(">Payments</text>"));
        assert_eq!(out.matches("<g class=\"region\">").count(), 1);

        let refused = |options: &Options<'_>, view: &View<'_>| {
            render(&drawn, None, view, options).unwrap_err().code
        };
        let edge = ["edge".to_owned()];
        let unknown = Options {
            groups: &edge,
            ..Options::default()
        };
        assert_eq!(refused(&unknown, &WHOLE), Code::RequestInvalid);
        let idle = ["idle".to_owned()];
        let empty = Options {
            groups: &idle,
            ..Options::default()
        };
        let error = render(&drawn, None, &WHOLE, &empty).unwrap_err();
        assert_eq!(error.code, Code::RequestInvalid);
        assert!(error.message.contains("`idle`"), "{}", error.message);
        let sequence = View::Sequence("Checkout");
        assert_eq!(refused(&regions, &sequence), Code::RequestInvalid);
    }

    #[test]
    fn an_empty_flow_says_so() {
        let mut shop = shop();
        shop.collections = flow(json!([]));
        for view in [View::Sequence("Checkout"), View::Flowchart("Checkout")] {
            assert!(draw(&shop, &view).unwrap().contains(">no steps</text>"));
        }
    }
}
