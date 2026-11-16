//! The architecture view: modules as boxes labelled with name and kind, containers drawn around
//! their parts, relationships as arrows routed between box sides. Parts deeper than the depth
//! collapse into their container, and their relationships lift to it.

use std::collections::{BTreeMap, BTreeSet};

use super::layout::{self, Boxed, Rect, Scene};
use super::svg::{Anchor, BACKGROUND, DESCENT, FONT_SIZE, Font, Point, Style, Svg};
use super::{
    BASELINE, Context, Fallbacks, MARGIN, MUTED_LINE, Mark, OUTLINE_WIDTH, SELECTED,
    SELECTED_WIDTH, SOFT_INK, TITLE_H, class, label, line_style, outline, text_width,
};
use crate::arch::index::Index;
use crate::arch::types::{EXTERNAL_KIND, Emphasis, Module, Traffic};

const BOX_MIN_W: f64 = 120.0;
const BOX_H: f64 = 50.0;
const TEXT_PAD: f64 = 14.0;
const RADIUS: f64 = 6.0;
const CONTAINER_OPACITY: f64 = 0.4;
const EXTERNAL_DASH: &str = "5 3";
const STRIPE_W: f64 = 5.0;
const LABEL_PAD: f64 = 4.0;
const LABEL_H: f64 = 16.0;
const LABEL_RADIUS: f64 = 3.0;
/// Baselines of a box's name and kind lines below its top.
const NAME_LINE: f64 = 20.0;
const KIND_LINE: f64 = 37.0;
const TITLE_GAP: f64 = 2.0;
/// Space between a group region's edge and the boxes inside it, and between its name and the
/// boxes below, past the straight start of a route into them.
const REGION_PAD: f64 = 12.0;
const REGION_RADIUS: f64 = 10.0;
const REGION_OPACITY: f64 = 0.1;

/// The boxes drawn, in containment pre-order, down to the depth asked.
#[derive(Default)]
struct Visible<'a> {
    ids: Vec<String>,
    modules: Vec<&'a Module>,
    boxes: Vec<Boxed>,
    /// Each box's kind line, with the parts it hides.
    kinds: Vec<String>,
    /// Whether each box has parts drawn inside it.
    containers: Vec<bool>,
    /// The box each drawn or collapsed module shows in, by qualified ID.
    at: BTreeMap<String, usize>,
}

/// One group's region: the band behind its drawn members, and where its name sits.
struct Region<'c> {
    name: &'c str,
    color: &'c str,
    band: Rect,
    label: Rect,
}

/// The relationships of one kind between two boxes, drawn as one arrow.
struct Arrow<'a> {
    from: usize,
    to: usize,
    /// The first relationship's qualified ID.
    id: String,
    ids: Vec<String>,
    kind: &'a str,
    label: Option<&'a str>,
    traffic: Option<&'a Traffic>,
    count: usize,
    hero: bool,
    muted: bool,
    mark: Option<Mark>,
}

pub(super) fn render(
    ctx: &Context<'_>,
    modules: &[Module],
    scope: Option<&str>,
    depth: usize,
    (regions, legend): (&[String], bool),
) -> (Svg, Vec<String>, Vec<String>, Fallbacks) {
    let index = Index::new(ctx.model, modules);
    let roots = match scope {
        Some(scope) if index.is_container(scope) => index.parts_of(scope).to_vec(),
        Some(scope) => (index.by_id.get_key_value(scope).into_iter())
            .map(|(id, m)| (id.clone(), *m))
            .collect(),
        None => index.roots.clone(),
    };
    let visible = visible(ctx, &index, roots, depth);
    let arrows = arrows(ctx, modules, &visible);
    let pairs: Vec<(usize, usize)> = arrows.iter().map(|a| (a.from, a.to)).collect();
    let (rects, _) = layout::nest(&visible.boxes, &pairs);
    let rects: Vec<Rect> = (rects.into_iter())
        .map(|r| Rect {
            x: r.x + MARGIN,
            y: r.y + MARGIN + TITLE_H,
            ..r
        })
        .collect();
    let mut scene = Scene {
        parents: visible.boxes.iter().map(|b| b.parent).collect(),
        titles: (0..rects.len())
            .map(|i| visible.containers[i].then(|| title_area(&rects[i], &visible, i)))
            .collect(),
        fixed: vec![ctx.title_area()],
        rects: &rects,
    };
    let regions = place_regions(ctx, &visible, &mut scene, regions);
    let (routes, bent) = layout::routes(&scene, &pairs);
    let texts: Vec<String> = arrows.iter().map(|a| arrow_text(ctx, a)).collect();
    let sizes: Vec<(f64, f64)> = (texts.iter())
        .map(|t| (text_width(t) + 2.0 * LABEL_PAD, LABEL_H))
        .collect();
    let (centers, stuck) = layout::labels(&scene, &routes, &sizes);
    let mut svg = Svg::default();
    ctx.title(&mut svg);
    for region in &regions {
        draw_region(&mut svg, region);
    }
    let lit = lit_boxes(ctx, &visible);
    let dims = |i: usize| lit.as_ref().is_some_and(|l| !l.contains(&i));
    for (i, rect) in rects.iter().enumerate() {
        let dim = dims(i) && ctx.dims(&visible.ids[i]);
        if dim {
            svg.dimmed();
        }
        draw_box(&mut svg, ctx, rect, &visible, i);
        if dim {
            svg.close_group();
        }
    }
    let each_arrow = |svg: &mut Svg, draw: &dyn Fn(&mut Svg, usize, &Arrow<'_>)| {
        for (k, arrow) in arrows.iter().enumerate() {
            let dim = dims(arrow.from) && dims(arrow.to);
            if dim {
                svg.dimmed();
            }
            draw(svg, k, arrow);
            if dim {
                svg.close_group();
            }
        }
    };
    each_arrow(&mut svg, &|svg, k, arrow| {
        draw_route(svg, ctx, arrow, &routes[k]);
    });
    each_arrow(&mut svg, &|svg, k, arrow| {
        draw_label(svg, arrow, &texts[k], centers[k]);
    });
    ctx.legend(&mut svg);
    if legend {
        let nodes: BTreeSet<&str> = visible.modules.iter().map(|m| m.kind.as_str()).collect();
        let lines: BTreeSet<&str> = arrows.iter().map(|a| a.kind).collect();
        ctx.kinds(&mut svg, &nodes, &lines);
    }
    let relationships = (arrows.into_iter()).flat_map(|a| a.ids).collect();
    let fallbacks = Fallbacks {
        routes: bent,
        labels: stuck,
    };
    (svg, visible.ids, relationships, fallbacks)
}

/// The regions of the groups `keys` with a drawn member, in that order. Each name sits just
/// above its members' top left corner, or on the nearest spot that meets no box, title or name
/// placed before, and joins the scene's fixed areas so routes and labels keep off it.
fn place_regions<'c>(
    ctx: &'c Context<'_>,
    visible: &Visible<'_>,
    scene: &mut Scene<'_>,
    keys: &[String],
) -> Vec<Region<'c>> {
    let mut out = Vec::new();
    for key in keys {
        let Some(group) = ctx.config.groups.get(key) else {
            continue;
        };
        let members = (0..visible.ids.len())
            .filter(|&i| visible.modules[i].groups.contains(key))
            .map(|i| scene.rects[i].clone());
        let Some(bounds) = members.reduce(|a, b| union(&a, &b)) else {
            continue;
        };
        let w = text_width(&group.name) + 2.0 * LABEL_PAD;
        let above = (bounds.x + w / 2.0, bounds.y - REGION_PAD - LABEL_H / 2.0);
        let label = layout::free_spot(scene, (w, LABEL_H), above);
        scene.fixed.push(label.clone());
        out.push(Region {
            name: &group.name,
            color: ctx
                .group_color(std::slice::from_ref(key))
                .unwrap_or(MUTED_LINE),
            band: union(&grow(&bounds, REGION_PAD), &grow(&label, LABEL_PAD)),
            label,
        });
    }
    out
}

fn grow(r: &Rect, by: f64) -> Rect {
    Rect {
        x: r.x - by,
        y: r.y - by,
        w: r.w + 2.0 * by,
        h: r.h + 2.0 * by,
    }
}

fn union(a: &Rect, b: &Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

fn draw_region(svg: &mut Svg, region: &Region<'_>) {
    svg.group("region", None);
    let style = Style {
        fill: Some(region.color),
        fill_opacity: Some(REGION_OPACITY),
        stroke: Some(region.color),
        width: Some(OUTLINE_WIDTH),
        ..Style::default()
    };
    let band = &region.band;
    svg.rect((band.x, band.y), (band.w, band.h), REGION_RADIUS, &style);
    let font = Font {
        bold: true,
        fill: Some(SOFT_INK),
        ..Font::default()
    };
    let at = (
        region.label.x + LABEL_PAD,
        region.label.y + LABEL_H / 2.0 + BASELINE,
    );
    svg.text(at, region.name, &font);
    svg.close_group();
}

fn title_area(rect: &Rect, visible: &Visible<'_>, i: usize) -> Rect {
    let width = text_width(&visible.modules[i].name).max(text_width(&visible.kinds[i]));
    Rect {
        x: rect.x + TEXT_PAD - TITLE_GAP,
        y: rect.y + NAME_LINE - FONT_SIZE - TITLE_GAP,
        w: width + 2.0 * TITLE_GAP,
        h: KIND_LINE - NAME_LINE + FONT_SIZE + DESCENT + 2.0 * TITLE_GAP,
    }
}

/// The boxes a focus lights: those of the focused modules and of the modules inside them, drawn
/// or collapsed. `None` when nothing is focused or no focused module is drawn, so nothing dims.
fn lit_boxes(ctx: &Context<'_>, visible: &Visible<'_>) -> Option<BTreeSet<usize>> {
    let focus = ctx.focus.as_ref()?;
    let lit: BTreeSet<usize> = (focus.lit.iter())
        .filter_map(|id| visible.at.get(id).copied())
        .collect();
    (!lit.is_empty()).then_some(lit)
}

/// Modules sorted as a reading lists them: by order, then name, then ID.
fn sorted(mut list: Vec<(String, &Module)>) -> Vec<(String, &Module)> {
    list.sort_by(|(a_id, a), (b_id, b)| (a.order, &a.name, a_id).cmp(&(b.order, &b.name, b_id)));
    list
}

/// The boxes for `roots` and their parts down to level `depth` (roots are level 1). A box at
/// the last level stands for everything below it.
fn visible<'a>(
    ctx: &Context<'_>,
    index: &Index<'a>,
    roots: Vec<(String, &'a Module)>,
    depth: usize,
) -> Visible<'a> {
    let mut v = Visible::default();
    let mut stack: Vec<_> = (sorted(roots).into_iter().rev())
        .map(|(id, m)| (id, m, None, 1))
        .collect();
    while let Some((id, module, parent, level)) = stack.pop() {
        let at = v.ids.len();
        let hidden = if level < depth {
            let parts = sorted(index.parts_of(&id).to_vec());
            stack.extend(
                parts
                    .into_iter()
                    .rev()
                    .map(|(p, m)| (p, m, Some(at), level + 1)),
            );
            0
        } else {
            let below = index.descendants(&id);
            v.at.extend(below.iter().map(|(d, _)| ((*d).to_owned(), at)));
            below.len()
        };
        if let Some(parent) = parent {
            v.containers[parent] = true;
        }
        let kind = kind_line(ctx, module, hidden);
        let label = text_width(&module.name).max(text_width(&kind));
        v.boxes.push(Boxed {
            parent,
            w: (label + 2.0 * TEXT_PAD).max(BOX_MIN_W),
            h: BOX_H,
        });
        v.kinds.push(kind);
        v.containers.push(false);
        v.at.insert(id.clone(), at);
        v.ids.push(id);
        v.modules.push(module);
    }
    v
}

fn kind_line(ctx: &Context<'_>, module: &Module, hidden: usize) -> String {
    let kind = ctx.kind_name(&module.kind);
    match hidden {
        0 => format!("[{kind}]"),
        1 => format!("[{kind}] +1 part"),
        n => format!("[{kind}] +{n} parts"),
    }
}

/// One arrow per source box, target box and relationship kind, counting the relationships it
/// stands for. A relationship of a drawn module to itself loops back to its box; other
/// relationships inside one box, and those with an end outside the drawing, are left out.
fn arrows<'a>(ctx: &Context<'_>, modules: &'a [Module], visible: &Visible<'_>) -> Vec<Arrow<'a>> {
    let mut merged: BTreeMap<(usize, usize, &str), Arrow<'a>> = BTreeMap::new();
    for module in modules {
        let own = ctx.qualify(&module.id);
        let Some(&from) = visible.at.get(&own) else {
            continue;
        };
        for r in &module.outgoing {
            let Some(&to) = visible.at.get(&r.to) else {
                continue;
            };
            let drawn_self = r.to == own && visible.ids[from] == own;
            if to == from && !drawn_self {
                continue;
            }
            let id = ctx.qualify(&r.id);
            let mark = ctx.mark(&id);
            let hero = r.emphasis == Some(Emphasis::Hero);
            let muted = r.emphasis == Some(Emphasis::Muted);
            (merged.entry((from, to, r.kind.as_str())))
                .and_modify(|arrow| {
                    arrow.count += 1;
                    arrow.ids.push(id.clone());
                    arrow.hero |= hero;
                    arrow.muted &= muted;
                    arrow.mark = Mark::merge(arrow.mark, mark);
                })
                .or_insert(Arrow {
                    from,
                    to,
                    ids: vec![id.clone()],
                    id,
                    kind: &r.kind,
                    label: r.label.as_deref(),
                    traffic: r.traffic.as_ref(),
                    count: 1,
                    hero,
                    muted,
                    mark,
                });
        }
    }
    merged.into_values().collect()
}

/// Box `i` of `visible`: a container's label sits at its top left, a leaf's in its middle.
fn draw_box(svg: &mut Svg, ctx: &Context<'_>, rect: &Rect, visible: &Visible<'_>, i: usize) {
    let (id, module, container) = (&visible.ids[i], visible.modules[i], visible.containers[i]);
    let mark = ctx.mark(id);
    let selected = ctx.selection.contains(id);
    let base = if selected {
        "module selected"
    } else {
        "module"
    };
    svg.group(&class(base, mark), Some(id));
    let mut style = outline(mark, ctx.fill(&module.kind));
    if selected {
        style.stroke = Some(SELECTED);
        style.width = Some(SELECTED_WIDTH);
    }
    if container {
        style.fill_opacity = Some(CONTAINER_OPACITY);
    }
    if module.kind == EXTERNAL_KIND {
        style.dash = Some(EXTERNAL_DASH);
    }
    svg.rect((rect.x, rect.y), (rect.w, rect.h), RADIUS, &style);
    if let Some(color) = ctx.group_color(&module.groups) {
        let stripe = Style {
            fill: Some(color),
            ..Style::default()
        };
        let height = (rect.h - 2.0 * RADIUS).max(0.0);
        svg.rect((rect.x, rect.y + RADIUS), (STRIPE_W, height), 0.0, &stripe);
    }
    let (x, anchor) = if container {
        (rect.x + TEXT_PAD, Anchor::Start)
    } else {
        (rect.x + rect.w / 2.0, Anchor::Middle)
    };
    let name = Font {
        anchor,
        bold: true,
        ..label(mark)
    };
    svg.text((x, rect.y + NAME_LINE), &module.name, &name);
    let kind = Font {
        anchor,
        fill: Some(SOFT_INK),
        ..label(mark)
    };
    svg.text((x, rect.y + KIND_LINE), &visible.kinds[i], &kind);
    svg.close_group();
}

/// The relationship's label, else its kind name, with its traffic in brackets; `Kind xN` for an
/// arrow standing for several.
fn arrow_text(ctx: &Context<'_>, arrow: &Arrow<'_>) -> String {
    let kind = ctx.relationship_name(arrow.kind);
    let text = match arrow.count {
        1 => arrow.label.unwrap_or(kind),
        n => return format!("{kind} x{n}"),
    };
    match arrow.traffic {
        Some(traffic) => format!("{text} ({})", traffic.text()),
        None => text.to_owned(),
    }
}

fn draw_route(svg: &mut Svg, ctx: &Context<'_>, arrow: &Arrow<'_>, route: &[Point]) {
    svg.group(&class("relationship", arrow.mark), Some(&arrow.id));
    let emphasis = match (arrow.hero, arrow.muted) {
        (true, _) => Some(Emphasis::Hero),
        (false, true) => Some(Emphasis::Muted),
        _ => None,
    };
    let style = line_style(emphasis, arrow.mark, ctx.dash(arrow.kind));
    svg.polyline(route, &style);
    if let [.., from, to] = route {
        svg.arrowhead(*from, *to, style.stroke.unwrap_or(SOFT_INK));
    }
    svg.close_group();
}

fn draw_label(svg: &mut Svg, arrow: &Arrow<'_>, text: &str, (mx, my): Point) {
    svg.group(&class("relationship label", arrow.mark), Some(&arrow.id));
    let w = text_width(text) + 2.0 * LABEL_PAD;
    let background = Style {
        fill: Some(BACKGROUND),
        ..Style::default()
    };
    let corner = (mx - w / 2.0, my - LABEL_H / 2.0);
    svg.rect(corner, (w, LABEL_H), LABEL_RADIUS, &background);
    let font = Font {
        anchor: Anchor::Middle,
        fill: Some(SOFT_INK),
        ..label(arrow.mark)
    };
    svg.text((mx, my + super::BASELINE), text, &font);
    svg.close_group();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::View;
    use super::super::tests::{draw, model, module, q};

    #[test]
    fn collapsed_relationships_are_counted() {
        let call = |id: &str| json!([{"id": id, "to": q("Banking1"), "kind": "call", "order": 0}]);
        let modules = vec![
            module(json!({"id": "Platfrm1", "name": "Platform", "kind": "system"})),
            module(
                json!({"id": "PartOne1", "name": "One", "parent": q("Platfrm1"), "outgoing": call("RelOne01")}),
            ),
            module(
                json!({"id": "PartTwo1", "name": "Two", "parent": q("Platfrm1"), "outgoing": call("RelTwo01")}),
            ),
            module(json!({"id": "Banking1", "name": "Bank", "kind": "external"})),
        ];
        let shop = model(modules, Vec::new());
        let collapsed = View::Architecture {
            scope: None,
            depth: 1,
        };
        let out = draw(&shop, &collapsed).unwrap();
        assert_eq!(out.matches("class=\"relationship\"").count(), 1, "{out}");
        assert!(out.contains(">Call x2</text>"), "{out}");
        assert!(out.contains("[System] +2 parts"), "{out}");
        assert!(!out.contains(">One</text>"));
        let open = View::Architecture {
            scope: None,
            depth: 2,
        };
        let out = draw(&shop, &open).unwrap();
        assert_eq!(out.matches("class=\"relationship\"").count(), 2, "{out}");
    }

    #[test]
    fn a_relationship_to_itself_loops_unless_collapsed() {
        let selfish = json!([{"id": "RelSelf1", "to": q("PartOne1"), "kind": "call", "order": 0, "label": "retry"}]);
        let modules = vec![
            module(json!({"id": "Platfrm1", "name": "Platform", "kind": "system"})),
            module(
                json!({"id": "PartOne1", "name": "One", "parent": q("Platfrm1"), "outgoing": selfish}),
            ),
        ];
        let shop = model(modules, Vec::new());
        let open = View::Architecture {
            scope: None,
            depth: 2,
        };
        let out = draw(&shop, &open).unwrap();
        assert!(out.contains(">retry</text>"), "{out}");
        let collapsed = View::Architecture {
            scope: None,
            depth: 1,
        };
        assert!(!draw(&shop, &collapsed).unwrap().contains("retry"));
    }
}
