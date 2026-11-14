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
| T7 | Checkpoints: history branch, `model retain`, `model checkpoints`, `model read --checkpoint` | done |
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
  in its list, since a sequence has no other order. Collections and flows resolve by ID, else by
  unique current or former name, so one batch can add a collection and its flows; steps, branches
  and loops resolve by ID only, and `add-flow`, `add-step` and `add-branch` take nested steps so a
  whole tree lands in one change. `model read <subject>` and claims resolve collection and flow IDs,
  not names, before module names. The JSON output of `model read --flow` qualifies every flow, step
  and branch ID; its Markdown is a readable sequence.
- T6: a claim on a flow covers its steps; a claim on a collection covers its flows. A step's module
  references need no claim. A rename records the former name, as for modules. Adding or renaming a
  collection or flow touches every one that has or had the name. `REFERENCED_BY_FLOW` refuses only
  references the batch broke by removing a module or removing or moving a relationship, in steps the
  batch did not edit; an edited step whose reference does not hold is `MODEL_INVALID`.
- T7: the history is the ref `refs/boundrail/history`, one per repository, so checkpoints from any
  worktree land on it. A checkpoint's tree holds the recognized vault files at their vault-relative
  paths (`model.json` at the root), not at `.boundrail/model/`. Commits are built with
  `hash-object`, a disposable index and `commit-tree`, so HEAD, the index and the working tree never
  change. With `commit.gpgSign` set, `commit-tree -S` signs with the user's signing program and key,
  the one program repository config may name for us; a failed signature refuses `SIGNING_FAILED`
  before the ref moves, whatever the signer prints. A signing program named by repository config is
  accepted scope: writing `.git/config` already allows running code, and reading only global config
  would break `include.path` and per-repository signing setups. The ref moves by compare-and-swap
  with a reflog entry; a lost race or a held ref lock refuses `HISTORY_MOVED` and a retry builds on
  the new tip. The new commit's parent and the swap's old value are the tip the history was read
  from, even a commit without checkpoint trailers. A vault file larger than the 4 MiB a checkpoint
  reader accepts refuses `MODEL_INVALID` before anything is written. Retaining an unchanged model
  refuses `NOTHING_TO_RETAIN`. Trailers `Source` and `Operation` appear only when there is a value.
- T7: `model retain` takes flags, not a request body. `--writer` defaults to the Git author name.
  `--expected-revision` refuses `STALE_REVISION` when the model is at another revision; agents
  should pass the revision they reviewed. Its operation fingerprint covers the writer, `--doing` and
  `--expected-revision` when given, since `--doing` is the checkpoint's content; an edit's
  fingerprint leaves `--doing` out. The journal record is written `pending` before the ref moves and
  `done` after; a pending record whose commit is on the ref counts as done. The journal is per
  worktree, so a retry from another worktree is answered from the `Operation` trailers on the shared
  ref. Such a retry cannot tell whether the first attempt passed `--expected-revision`, so it
  accepts the flag absent or equal to the retained revision.
- T7: `model checkpoints` and `model read --checkpoint` need no vault in the checkout, so a
  deleted vault can be restored from its history. `--checkpoint` takes a checkpoint ID (any
  case), else a unique commit prefix of at least 4 hex digits; an ambiguous prefix refuses
  `SUBJECT_AMBIGUOUS` and an unknown one `CHECKPOINT_NOT_FOUND`. A checkpoint tree that holds
  anything `model retain` never writes refuses `MODEL_INVALID`. Files are read with one
  `git show` of their blobs, which applies no attributes or filters. A checkpoint reading shows
  no claims or pending edit, which belong to the checkout. Listing without `--limit` walks the
  whole ref.
