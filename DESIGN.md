# Design

## Source of truth

Status: Active. Surface: the live model view served by `boundrail model view`
(`src/arch/view/index.html`, `app.js`, `style.css`, `geist.woff2`), and the SVG drawings of
`boundrail model render`, which the view embeds.

## Brand

A precise developer tool. Trust comes from source locations and truthful limits. Use the
Boundrail product name.

## Product goals

Make the authored model easy to read and to compare: find a module, follow a relationship or a
flow, see what a proposal changes, and see where the code disagrees with the model.

## Information architecture

A top bar names the model, its revision and whether the view is live, with the state to show and
the state to compare with. The canvas toolbar groups the view switch (architecture, sequence,
flowchart), a depth stepper, and fit, focus and split, each showing its state. A sidebar filters
modules by text and by kind, hides relationships by kind (chips drawn with the kind's dash; a
pressed chip is struck through and dashed, since it switches its kind off), and lists modules,
flows, walkthroughs, pictures, warnings, changes, proposals and history. The detail panel presents
the selection, with relationships as rows, or a model summary when nothing is selected; with
several modules selected (Shift-, Command- or Control-click) it lists the members, the
relationships between them and their outside neighbours. A walkthrough plays in a floating bar
that the diagram fits above.

The address after `#` holds what the active pane shows (state, comparison, view, flow, scope,
depth, saved view, picture or beat, selection and focus), and every change adds a history entry,
so Back and Forward step through what the user looked at. A reading an agent sends with `model
view --show` replaces the active pane's reading the same way, with a toast and a 2px accent ring
around the pane that fades in 600 ms.

## Visual language

Quiet neutral chrome and one green accent (`--accent`) for what is selected, focused or live. The
canvas is the only textured surface: a dot grid (`--grid`). Type is Geist, a variable font
embedded and served from `/font.woff2`, for everything (`--sans`, with the system sans stack as
fallback), and the system monospace stack (`--mono`: `ui-monospace`, SF Mono, Menlo, Consolas) for
revisions, IDs, paths and diagnostic codes. Sizes come from `--fs-11` to `--fs-20`, radii from
`--radius-s`, `--radius` and `--radius-l`, controls are 28px high with 24px minimum targets.

Colours are tokens on `:root`: surfaces (`--chrome`, `--panel`, `--canvas`, `--raised`), dividers,
three text levels, the accent, states (`--warn`, `--info`, `--bad`, `--live`), comparison marks
(`--add`, `--remove`, `--change`), canvas roles (`--node`, `--container`, `--edge`,
`--edge-label`, `--head`, `--note`) and one tone per node kind (`--k-ui` to `--k-external`). The
light theme is the default and a dark set replaces every token under `prefers-color-scheme: dark`;
the page follows the system setting. The page never inverts the drawn SVG: its CSS maps the
renderer's colours to the canvas tokens. Each kind's tone is its configured node-kind colour when
that is `#rgb` or `#rrggbb`, else the palette's, and the same tone marks the module in the list,
on the canvas and in the kind filters. Warning, information and bad states always carry a text
label. No remote fonts or imagery.

## Accessibility

Native buttons, inputs and selects with labels, visible focus outlines in the accent colour, a
polite status region for announcements, and no colour-only distinctions. Transitions stop under
`prefers-reduced-motion`, and the arrival ring is not drawn at all.

## Responsive behavior

From 601px to 1000px wide the page scrolls as one column: the canvas comes first, at least 440px
and up to 70% of the viewport high, and the sidebar and detail panel stack under it. Split panes
stack one above the other.

At 600px and below the canvas fills the screen under a 48px top bar. The state and comparison
selects move into a menu sheet opened from the top bar, and the toolbar scrolls sideways in one
row. The detail panel and the sidebar live in a bottom drawer that shows a 64px handle when closed
and opens to 78% of the viewport height; the walkthrough bar sits above the closed drawer.

## Implementation constraints

The page is embedded with `include_str!` and the font with `include_bytes!`; the page uses no
framework and loads nothing from other origins, so it works offline. The Content-Security-Policy
allows only its own script, style, font and connections, and every response, the font included,
is sent with `Cache-Control: no-store`. Repository text is built into the DOM with `textContent`,
and only a validated hex colour from the config reaches CSS. The API answers with the same JSON
the CLI prints, and every API request carries the view's token.

## Open release checks

- HOLD: the pane outline's painting order. Check it in Chrome, Firefox and Safari before calling
  the redesign done.
