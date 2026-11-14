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
| T8 | Proposals: create/edit drafts against a predecessor, `model compare`, adopt, shelve, planning models without a current state | done |
| T9 | Render: SVG architecture view with automatic layout, sequence and flowchart views of a flow, comparison marks | done |
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
- T8: a proposal is `proposals/<id>/` in the vault: `proposal.json` (schema `model-proposal`) beside
  its own `modules/` and `collections/` in the current state's formats. Each load is one target:
  the current state, or one proposal with the vault's manifest and config. The current state's
  revision covers what it did before, so existing digests hold; a proposal's covers its
  `proposal.json` and folders. A proposal edit names the proposal's ID as `target.state`, its
  revision as `expected_revision`, and `--proposal <id>` on the command line; the request names
  the proposal, so its operation fingerprint differs from a current-state edit. Proposal IDs are
  local or qualified, never names.
- T8: proposal loads validate like current-state loads (limits, references inside the proposal,
  links, file and folder names) with one exception: an ID the current state retired after the
  proposal was drawn does not stop it loading, since such a proposal refuses adoption anyway
  (`PREDECESSOR_MOVED`). A planning model's manifest has `current: null`; a planning model with
  module or collection files is `MODEL_INVALID`.
- T8: drawing, editing, adopting and shelving are journaled writes through the edit journal. The
  journal record names its `scope` (current, a proposal, or an adoption covering both); older
  records default to the current state. `model propose`, `adopt` and `shelve` take flags like
  `model retain`: `--writer` defaults to the Git author name, and without `--operation-id` a fresh
  one is generated. Their fingerprints cover the kind, writer and arguments, not `--doing`. Every
  write's paths are checked for linked folders before its record is journaled, so such a write
  refuses without leaving a pending record. A proposal's `created` is Unix seconds, as a claim's.
- T8: a draft drawn from the current state has the current state and revision as predecessor; a
  copy of another proposal (`--from <id>`) inherits that proposal's predecessor, since adopting
  the copy would otherwise revert whatever the current state gained since its source was drawn.
  A copy of an adopted proposal is drawn only while that proposal still is the current state,
  unchanged (its `adopted_as` is the current state and their modules and collections are equal);
  otherwise it refuses `PREDECESSOR_MOVED`.
- T8: `model proposals` reads only each `proposal.json` and lists its ID, name, status and
  predecessor, or the folder's ID with an `error` when it does not read, so one broken proposal
  hides no other. `model read --proposal` shows a proposal's revision.
- T8: new IDs are random 8-character base62, collision-checked against the state being edited
  (the current state or the proposal), the retired list, and the proposal folder names; drawing a
  proposal or adopting one also checks the current state's IDs, and adoption the proposal's. IDs
  two proposals allocate are not checked against each other: adopting a state that clashes with
  the model (a retired or duplicate ID) refuses `MODEL_INVALID`. Removing something inside a
  proposal retires nothing.
  Adoption writes the proposal's files over the current state's (unchanged files are left alone),
  gives the state a fresh ID and the proposal's name, and retires every ID the old state held that
  the new one drops, the old state's ID included. The adopted proposal keeps its folder, reading
  only; its `adopted_as` names the new state. `model adopt` prints the current state's new
  `revision` beside the receipt, whose revisions cover both the state and the proposal.
- T8: `model adopt` refuses `STALE_REVISION` when `--expected-revision` is not the current revision,
  then `PREDECESSOR_MOVED` when the proposal was drawn against another; reconcile is T17. With
  `--expected-proposal-revision` it also refuses `STALE_REVISION` when the proposal is at another
  revision; agents should pass the proposal revision they compared, so a proposal edited after the
  comparison is never adopted unseen. The flag is part of the fingerprint only when given. Edits,
  shelving and adoption of an adopted or shelved proposal refuse `PROPOSAL_CLOSED`. A
  current-state edit or claim in a planning model refuses `NO_CURRENT_STATE`.
- T8: IDs are model-wide, so a claim on a module holds it in the current state and in every
  proposal. Inside a proposal the proposal's qualified ID takes the model ID's place as the root
  level and contains the root modules and collections, so a claim on the proposal holds all of it.
  A claim request's `target.state` may name a proposal, and its subjects then resolve there.
  Claims never overlap across targets: a new claim is checked against the containment of its
  target and of every proposal it or another active claim holds, so a claim on a proposal and a
  claim on a module it contains clash either way round. Adoption touches what an edit making the
  same change would (the modules, relationships, collections and flows it adds, changes or
  removes, with their moves), plus the model ID and the proposal; shelving touches the proposal.
- T8: `model compare` compares modules without their relationships, collections without their
  flows, flows without their steps and steps without the steps inside them; a relationship's
  `from`, a flow's `collection` and a step's `flow` and `parent` count as fields, so a move shows
  as a change. A step's index is not compared.
- T9: `model render` draws the loaded model as SVG, purely and deterministically (sorted maps,
  numbers to one decimal). The view is `architecture`, or `sequence` by default when `--flow` is
  given, or `flowchart`. It prints the SVG on stdout; `--format json` wraps it as
  `{kind: "render", view, svg}`, `--format markdown` refuses `REQUEST_INVALID`, and `--output`
  writes the same bytes to a file. The global `--format` has no default any more: absent means JSON
  for every other command and SVG for `render`. A refusal still prints as JSON on stdout, so
  `render > a.svg` can capture a refusal instead of a picture; scripts should use `--output`, which
  writes nothing when the command refuses. `REQUEST_INVALID` refuses a sequence or flowchart
  without `--flow`, `--flow` with the architecture view, `--scope` or `--depth` with a flow,
  `--depth 0`, `--proposal` with `--checkpoint`, `--against` naming the state being drawn, and an
  `--output` whose folder is missing or inside the vault. The picture and its viewBox are sized to
  the bounds of everything drawn, text included at its estimated width.
- T9: the architecture view lays out the parts of each container, and the top level, as layers over
  the relationships lifted to them: ranks by longest path after dropping the back edges of a
  depth-first search in module order, four barycenter sweeps down and up, rows centered, box widths
  from 7 units per character. Containers size to their parts plus padding and a label band, from
  the inside out. Edges leave and enter by facing sides, spread along a side by where the other end
  is; a vertical edge turns in its own lane of the gap beside its source, the shortest run nearest,
  so labels sit in free space. `--depth N` (default 2) collapses deeper parts into their box at
  level N, which shows `+N parts`; relationships lift to the drawn ancestor, one arrow per box pair
  and kind, labelled `Kind xN` when it stands for several. Relationships inside one box or with an
  end outside `--scope` are not drawn, except that a relationship of a drawn module to itself loops
  back to its box. External modules have a dashed border and, without a configured color, a gray
  fill. A kind's configured color fills its boxes when it is `#rgb` or `#rrggbb`; otherwise a
  palette color picked by a hash of the kind's key, so adding a kind recolors no other, and no
  palette color reads as an added, removed or changed mark. A module's first valid group color is a
  stripe on its left edge.
- T9: ponytail: no dummy nodes and no crossing count, so an edge that skips ranks can cross the
  boxes between its ends, and a relationship from a container to its own part loops out of the
  right side; add dummy nodes and crossing reduction when real models show it. Text width is a
  fixed estimate, so wide glyphs can overflow a box; measure the font if that shows.
- T9: a render compares two states when `--against current|<proposal>` names one or `--proposal`
  is given alone (then against the current state). The newer state is drawn and the older is the
  base, in the order checkpoint, current state, proposal; of two proposals, `--proposal` is drawn
  and `--against` is the base. So `--against P` draws P against the current state, and
  `--checkpoint C --against current` draws the current state against C. The title names the drawn
  state and its base. Marks come from `model compare`: added green, changed amber, removed red,
  faded and struck through, with a legend. Removed modules and relationships are drawn from the
  base; a removed step goes back at its old index in its old list when that list still exists,
  else it is not drawn. The flow resolves in the drawn state, so a flow it lacks
  refuses `SUBJECT_NOT_FOUND`. Notes appear beside their message in the sequence view only, and an
  empty flow draws `no steps`.
