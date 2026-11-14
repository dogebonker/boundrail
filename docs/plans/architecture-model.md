# Authored architecture model

Boundrail today reads import graphs and checks declared boundaries. This plan adds an
authored architecture model that agents write and that Boundrail checks against the code:
stable module identities, relationships with code evidence, flows, proposals drawn against
the current architecture, and a history of why the architecture changed.

The scanned graph stays. It becomes evidence for the model: an authored relationship is
supported, missing, or backwards according to what the code shows.

## Decisions

- D1. The model lives in `.boundrail/model/` at the checkout root and is committed with the code.
  The existing `.archgraph/` state folder stays for scan-era commands until T18 retires them.
- D2. One JSON file per module, owning its outgoing relationships. A manifest (`model.json`)
  names the model and its current state. Vocabulary lives in `config.yaml`.
- D3. Identities are allocated by Boundrail: 8-character base62 local IDs, qualified as
  `<model>#<id>`. Renames keep the ID and record former names. Retired IDs stay reserved.
- D4. Every write names the exact revision it read (a SHA-256 over the model inputs) and an
  operation ID. A stale revision refuses; repeating an operation ID returns the same receipt.
- D5. Writes are CLI file operations under a vault lock. No server is required to read or write.
  The live browser view reuses the existing `serve` daemon.
- D6. Checkpoints are commits on a separate history branch in the source repository, never on
  the working branch.
- D7. New code lives under `src/arch/`. Existing scan code is reused as an evidence source, and
  code that no longer serves the product is removed in its own task.

## Tasks

Status: `open`, `wip`, `done`. Each task is one or more commits with tests.

| ID | Task | Status |
|----|------|--------|
| T1 | Model core: types, vault read/write, revision digest, `model init`, `model read`, `model schema` | done |
| T2 | Batch edits: add/edit/remove module and relationship, rename history, expected revision, operation IDs, `model edit` | open |
| T3 | Claims: `model claim`, `model release`, `model claims`; edits require a claim covering their subjects | open |
| T4 | Semantic check: container binding, parts outside container, unconnected roots, shared bindings, repeated relationships, relationships on containers, lines to own part, flow summaries | open |
| T5 | Code check: binding and anchor paths and symbols, relationships undrawn / unsupported / backwards from the scanned graph, module interface exposure | open |
| T6 | Flows: collections, flows, message/decision/loop steps, step moves, `model read --flow` | open |
| T7 | Checkpoints: history branch, `model retain`, `model checkpoints`, `model read --checkpoint` | open |
| T8 | Proposals: create/edit drafts against a predecessor, `model compare`, adopt, shelve, planning models without a current state | open |
| T9 | Render: SVG architecture view with automatic layout, sequence and flowchart views of a flow, comparison marks | open |
| T10 | Measures: module interface counts (`model inspect`), co-change locality (`model locality`), branch footprint (`model footprint`) | open |
| T11 | Live browser view: model canvas, drill-down, focus and filters, detail panel, warnings, comparison, history, split panes | open |
| T12 | Infrastructure evidence: docker-compose, Kubernetes, Terraform bindings and configured-address hops between modules | open |
| T13 | Language-server evidence: call-level references through configured language servers | open |
| T14 | Walkthroughs: ordered beats over readings, playback in the browser | open |
| T15 | Embedded pictures: render recipes in SVG/PNG, `check embeds`, PNG rasterize | open |
| T16 | Agent integration: `init`, skills for authoring/planning/PRs, CI action with PR annotations and footprint comment | open |
| T17 | Reconcile drafts after current changes, export/import bundles, history publication | open |
| T18 | Remove scan-era code that the model does not use | open |
