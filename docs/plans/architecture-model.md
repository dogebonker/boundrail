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
| T2 | Batch edits: add/edit/remove module and relationship, rename history, expected revision, operation IDs, `model edit` | done |
| T3 | Claims: `model claim`, `model release`, `model claims`; an edit inside another writer's claim refuses, an edit with a claim must be covered by it | done |
| T4 | Semantic check: container binding, parts outside container, unconnected roots, shared bindings, repeated relationships, relationships on containers, lines to own part, `model check` | done |
| T5 | Code check: binding and anchor paths and symbols, relationships undrawn / unsupported / backwards from the scanned graph, module interface exposure | done |
| T6 | Flows: collections of flows in `collections/<id>.collection.json`, message/decision/loop steps, collection/flow/step/branch edits with flow and step moves, `REFERENCED_BY_FLOW`, collection and flow claims, `model read --flow`, `FLOW_SUMMARY_MISSING` and `MESSAGE_UNDRAWN` in the semantic check | done |
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

## Notes

- T16: scoped agent hooks treat .boundrail/model changes as out of scope; decide whether the vault is always in scope.
- T5: the code check scans the checkout once with the native engine, which picks one language per
  checkout. Bound code in any other language holds no scanned file and is skipped as
  `not-scanned` (polyglot gap). The TypeScript and Python collectors scan only under `src/` when
  it exists, so bindings outside it show as `not-scanned` too. A failed scan is a warning
  (`SCAN_FAILED`) that fails the check; paths it would have judged are skipped as `scan-failed`
  and the semantic check still reports. `--only semantic` opts out of the scan.
- T5: import edges stand in for call evidence until T13. Only kinds whose evidence is entirely
  import or call (`call`, `render` by default) get the strict check: no edge is unsupported, or
  backwards. A kind with other evidence too (HTTP, RPC, event, queue, data, dependency) is
  supported by an edge and otherwise skipped as `evidence-unavailable`.
- T5: a binding or anchor symbol is the exported name, `default` for a default export. A file the
  scan cannot fully index (CommonJS exports, destructured exports, syntax errors) skips a missing
  symbol as `symbols-incomplete`. Elixir files whose definitions `use` macros generate are not
  marked yet, so a symbol such a macro defines reports missing.
- T6: a flow's place in its collection is its `order`, as for modules; a step's place is its index
  in its list, since a sequence has no other order. Collections and flows resolve by ID or unique
  name, so one batch can add a collection and its flows; steps, branches and loops resolve by ID
  only, and `add-flow`, `add-step` and `add-branch` take nested steps so a whole tree lands in one
  change. `model read <subject>` and claims resolve collection and flow IDs, not names, before
  module names. `model read --flow` prints every flow, step and branch ID qualified.
- T6: a claim on a flow covers its steps; a claim on a collection covers its flows. A step's
  module references need no claim. Adding or renaming a collection or flow touches every one that
  already has the name, as for modules. `REFERENCED_BY_FLOW` refuses only references the batch
  broke by removing a module or removing or moving a relationship, in steps the batch did not
  edit; an edited step whose reference does not hold is `MODEL_INVALID`.
