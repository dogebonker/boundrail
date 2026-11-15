# Design

## Source of truth

Status: Active. Surface: the live model view served by `boundrail model view`
(`src/arch/view/index.html`, `app.js`, `style.css`), and the SVG drawings of `boundrail model
render`, which the view embeds.

## Brand

A precise developer tool. Trust comes from source locations and truthful limits. Use the
Boundrail product name.

## Product goals

Make the authored model easy to read and to compare: find a module, follow a relationship or a
flow, see what a proposal changes, and see where the code disagrees with the model.

## Information architecture

A top bar names the model, its revision and whether the view is live, with the state to show, the
state to compare with, and the view (architecture, sequence, flowchart). A sidebar filters and
lists modules, flows, walkthroughs, pictures, warnings, changes, proposals and history. The drawing
sheet sits in the middle with depth, fit, focus and split controls, and a detail panel presents the
selection.

## Visual language

A survey sheet: cool paper, graphite ink, and one teal accent for what is selected or live. The
drawing sheet is the only textured surface. Colours are tokens on `:root` with a dark-scheme set;
the drawn SVG uses its own accent that reads in both schemes. Headings and prose use the serif
stack, controls the system sans, IDs and paths the monospace stack. No remote fonts or imagery.
Warning, information and bad states use their own tokens and always carry a text label.

## Accessibility

Native buttons, inputs and selects with labels, visible focus outlines in the accent colour, a
polite status region for announcements, and no colour-only distinctions. Transitions stop under
`prefers-reduced-motion`.

## Responsive behavior

Below 1000px wide the drawing comes first and the sidebar and detail panel stack under it; split
panes stack too.

## Implementation constraints

The page is embedded with `include_str!`, uses no framework, and loads nothing from other origins,
so it works offline. Repository text is built into the DOM with `textContent`. The API answers with
the same JSON the CLI prints, and every API request carries the view's token.
