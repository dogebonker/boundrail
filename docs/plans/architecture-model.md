# Authored architecture model

This plan built Boundrail's authored architecture model, which agents write and Boundrail checks
against the code: stable module identities, relationships with code evidence, flows, proposals
drawn against the current architecture, and a history of why the architecture changed. It started
from a tool that read import graphs and checked declared boundaries; T18 removed that tool's
commands once the model no longer needed them.

The scanned graph is evidence for the model: an authored relationship is supported, missing, or
backwards according to what the code shows.

## Decisions

- D1. The model lives in `.boundrail/model/` at the checkout root and is committed with the code.
  T18 removed the scan-era commands and their `.archgraph/` state folder.
- D2. One JSON file per module, owning its outgoing relationships. A manifest (`model.json`)
  names the model and its current state. Vocabulary lives in `config.yaml`.
- D3. Identities are allocated by Boundrail: 8-character base62 local IDs, qualified as
  `<model>#<id>`. Renames keep the ID and record former names. Retired IDs stay reserved.
- D4. Every write names the exact revision it read (a SHA-256 over the model inputs) and an
  operation ID. A stale revision refuses; repeating an operation ID returns the same receipt.
- D5. Writes are CLI file operations under a vault lock. No server is required to read or write.
  The live browser view, `boundrail model view`, runs its own local read-only HTTP server.
- D6. Checkpoints are commits on a separate history branch in the source repository, never on
  the working branch. A private vault outside every checkout (T31) keeps them in a bare
  repository beside it instead.
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
| T10 | Measures: module interface counts (`model inspect`), co-change locality (`model locality`), branch footprint (`model footprint`) | done |
| T11 | Live browser view: model canvas, drill-down, focus and filters, detail panel, warnings, comparison, history, split panes | done |
| T12 | Infrastructure evidence: docker-compose, Kubernetes, Terraform bindings and configured-address hops between modules | done |
| T13 | Language-server evidence: call-level references through configured language servers | done |
| T14 | Walkthroughs: ordered beats over readings, playback in the browser | done |
| T15 | Embedded pictures: render recipes in SVG/PNG, `check embeds`, PNG rasterize | done |
| T16 | Agent integration: `model setup`, skills for authoring/planning/PRs, CI action with PR annotations and footprint comment | done |
| T17 | Reconcile drafts after current changes, export/import bundles, history publication | done |
| T18 | Remove scan-era code that the model does not use | done |
| T19 | Footprint against a proposal: modules the proposal names and the branch changes, changes outside it, named but unchanged, held subjects, large changes, `--fail-outside` exit 1, exit 2 on an unreadable ref | done |
| T20 | Check correctness: infrastructure-unit bindings exempt from `CONTAINER_PARTS_OUTSIDE`; the code check scans every language in the checkout, not one; `check --every-draft` | done |
| T21 | Render quality and options: edges route around boxes when a grid path exists, else take the T9 bend; labels avoid each other and boxes when there is room, else sit at the middle of the middle segment; `--focus`, `--hide-kind`, legend, `--theme auto`; JSON receipt with dimensions, placed IDs, digest and fallback counts; direct PNG with `--scale` and `--region` | done |
| T22 | Model-side inspect: entry points, hidden parts, callers, outgoing, pass-through, container relationships and measures read from the model, `--checkpoint`, and `inspect --group` | done |
| T23 | Compare: interface changes and interface measures between states, `--beside` alternatives, `--against` a checkpoint | done |
| T24 | Schema: relationship traffic, step anchors, authored flow participant order, flow summary required on write (`FLOW_SUMMARY_REQUIRED`) | done |
| T25 | History on every write: each edit, including in proposals, retains a checkpoint with its doing line; publication destination with per-checkpoint status; source-commit availability on checkpoint reads | done |
| T26 | Proposal metadata: summary, linked documents, rename and edit through `set`, select by unique name; walkthroughs authored inside proposals | done |
| T27 | Proposal families: child proposals of a draft, family reconcile, standing, settle conflicts by mine, theirs or edited; `INHERITANCE_DROPPED` and `PART_RECREATED` confirmations | done |
| T28 | Readings: group regions and group filter; beat selection apart from focus, relationship filters, groups, visibility; saved views that need no output file | done |
| T29 | Cross-model references: modules reference another model, state or subject, `read --references`, pinned foreign checkpoints; `read --code` resolves and opens the code target | done |
| T30 | History repair and restore: correct a recorded state with source commits and an audit trail; restore a bundle into an empty vault with identities and history | done |
| T31 | Vaults outside Git: a private history for a vault outside a checkout, used for temporary pull request models | done |
| T32 | Language servers: `model language-server` add, remove and list; defaults for C-family, C# and Swift; on by default through user config outside the checkout; `unready` diagnostic and step timings | done |
| T33 | Browser: multi-select, relationship-kind filter, URL state for proposal, focus and flow with Back and Forward; an agent shows a reading in the user's open pane | done |
| T34 | `model explain`: ask the local agent about a selection, relationship, flow or proposal | done |
| T35 | Walkthrough recording: render a walkthrough to MP4 with local voice and replay it | done |
| T36 | Agent workflow: plan skill slices per ticket with the footprint gate; pull request skill body, state marker, temporary model and split guidance; action reads the proposal and skips held subjects; design guidance for inspect and locality | done |

## Notes

- T5: the code check scans the checkout once with the native engine, every language present
  (T20). The Python collector scans one top-level package under `src/` when it exists, so Python
  outside it shows as `not-scanned`. A failed scan is a warning (`SCAN_FAILED`) that fails the
  check; paths it would have judged are skipped as `scan-failed` and the semantic check still
  reports. `--only semantic` opts out of the scan.
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
  would break `include.path` and per-repository signing setups. The same holds for the clean and
  process filter drivers repository config defines, which run when Git content-checks a working-tree
  entry, for example in `model footprint`; `src/git.rs` states both exceptions. The ref moves by
  compare-and-swap with a reflog entry; a lost race or a held ref lock refuses `HISTORY_MOVED` and a
  retry builds on the new tip. The new commit's parent and the swap's old value are the tip the
  history was read from, even a commit without checkpoint trailers. Checkpoints restore the current
  state; drafts travel with the committed vault or in bundles (T17), and `model history push` and
  `fetch` publish the history ref (T17); once fetched, checkpoint readings resolve. A vault file
  larger than the 4 MiB a checkpoint reader accepts refuses `MODEL_INVALID` before anything is
  written. Retaining an unchanged model refuses `NOTHING_TO_RETAIN` (since T25, only with the
  same `--doing`). Trailers `Source` and `Operation` appear only when there is a value.
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
  links, file and folder names) with one exception: they exempt every retired ID, so a proposal
  holding one loads, and adoption checks the adopted state against the retired list again
  (`MODEL_INVALID`). A planning model's manifest has `current: null`; the key is required, so a
  manifest without it is `MODEL_INVALID`. A planning model with module or collection files is
  `MODEL_INVALID`.
- T8: drawing, editing, adopting and shelving are journaled writes through the edit journal. The
  journal record names its `scope` (current, a proposal, or an adoption covering both); older
  records default to the current state. `model propose`, `adopt` and `shelve` take flags like
  `model retain`: `--writer` defaults to the Git author name, and `--operation-id` is required, as
  for edits. Their fingerprints cover the kind, writer and arguments, not `--doing`. Under D4,
  adoption names both revisions it read (`--expected-revision` for the current state,
  `--expected-proposal-revision` for the proposal) and shelving names the proposal's
  (`--expected-revision`, as `edit --proposal` names it). `model propose` takes no revision: the
  draft records the predecessor it copied, adoption checks that predecessor again, and the
  required proposal revision means whoever adopts has read the draft. Every
  write's paths are checked for linked folders before its record is journaled, so such a write
  refuses `MODEL_INVALID` with the folder in `details.path` without leaving a pending record; a
  linked `proposals` folder refuses the same way before it is listed. A journal record may hold up
  to 256 MiB, since a pending one carries every file its write makes; a larger one refuses
  `MODEL_INVALID` before anything is written. `model recover` lists each finished write with its
  `kind` and `scope`. A proposal's `created` is Unix seconds, as a claim's.
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
  (the current state or the proposal), the retired list, the proposal folder names, and in a
  proposal its `dropped` list; drawing a proposal or adopting one also checks the current state's
  IDs, and adoption the proposal's. A proposal edit reads no other state. IDs two proposals
  allocate are not checked against each other: adopting a state that clashes with the model (a
  retired or duplicate ID) refuses `MODEL_INVALID`. Removing something inside a proposal retires
  nothing; an ID the proposal held before the edit goes on its `dropped` list instead, a copy of
  a draft inherits the list, and adoption retires it with the rest. A `dropped` entry that is not
  8 base62 characters is `MODEL_INVALID`.
  Adoption writes the proposal's files over the current state's (unchanged files are left alone),
  gives the state a fresh ID and the proposal's name, and retires every ID the old state held that
  the new one drops, the old state's ID included. The adopted proposal keeps its folder, reading
  only; its `adopted_as` names the new state. `model adopt` prints the current state's new
  `revision` beside the receipt, whose revisions cover both the state and the proposal.
- T8: `model adopt` refuses `STALE_REVISION` when `--expected-revision` is not the current revision,
  then `PREDECESSOR_MOVED` when the proposal was drawn against another, pointing at `model
  reconcile` (T17). It also
  refuses `STALE_REVISION` when `--expected-proposal-revision` is not the proposal's revision, so
  a proposal edited after the comparison is never adopted unseen. `model shelve` refuses
  `STALE_REVISION` when its `--expected-revision` is not the proposal's. Each refusal names the
  revision found in `details.current`. Edits,
  shelving and adoption of an adopted or shelved proposal refuse `PROPOSAL_CLOSED`. A
  current-state edit or claim in a planning model refuses `NO_CURRENT_STATE`.
- T8: IDs are model-wide, so a claim on a module holds it in the current state and in every
  proposal. Inside a proposal the proposal's qualified ID takes the model ID's place as the root
  level and contains the root modules and collections, so a claim on the proposal holds all of it.
  A claim request's `target.state` may name a proposal, and its subjects then resolve there.
  Claims never overlap across targets: a new claim is checked against the containment of its
  target and of every draft, read by the status in each `proposal.json`, so a claim on a proposal
  and a claim on a module it contains clash either way round, as do claims on two modules one of
  which holds the other in any draft. Adopted and shelved proposals read only and contain nothing
  for this check. A draft that does not load is skipped with a warning on stderr; an edit inside
  that draft refuses anyway. Adoption touches what an edit making the
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
- T10: the measures read only; each prints JSON, or Markdown with `--format markdown`, sorted by
  qualified ID. `model inspect [subject] [--proposal P]` maps scanned files to modules exactly as
  the code check does (the deepest judged binding owns a file) and skips modules with the code
  check's reasons. Outside a module means any file it does not own: a part's files are outside its
  container, and unbound files are outside every module. A module's entry points are those of
  `ENTRY_POINT_UNEXPOSED`: entry files directly in its bound directory and files a package
  publishes below it; a module bound to a file has none. Its exported symbols are the exports of
  its entry points, or of every file it owns when it has none. An export is used when a file
  outside imports it: by resolved target, by name from the exporting file when unresolved, by an
  unresolved wildcard import of that file, or by an import edge into that file from a file with
  no symbol import of it, which counts as a wildcard use. The scan's surface ignores unresolved
  named imports, so inspect errs toward "used" where the two differ. The rest are `unused`, the
  candidates to make internal, withheld as null when symbol coverage is incomplete (not
  `complete` and a limitation naming no scanned file), as the surface withholds its
  could-be-internal list; TypeScript scans report that today, so they show counts without
  candidates. In a scan of several languages the rule applies per language (T20): a module's
  candidates are withheld only when a language of a file it owns is incomplete. Without symbol data
  `symbols` is null with `symbols_skipped` `language-unsupported`; a file a symbol limitation names
  makes it `symbols-incomplete`. Deep imports are the runtime edges `ENTRY_POINT_UNEXPOSED`
  reports. Incoming and outgoing entries count import edges between files of two owners, type-only
  edges included, and list the relationships drawn that way between the two or modules containing
  them.
- T10: `model locality` and `model footprint` map paths by binding alone, without a scan, so
  deleted files and unscanned languages count: the deepest binding in this repository that is not
  external wins (one path lookup per folder), a module bound with a symbol to a path another
  module holds owns nothing, and the vault's own files belong to no module.
  `model locality [--since <rev>] [--limit N]` reads at most N (default 500) non-merge commits of
  HEAD, after `--since` when given, through the co-change log reader without following renames: a
  rename counts at its old and new path, so a move between modules touches both. Sweeps of more
  than 30 files are left out and counted. In a shallow checkout the boundary commits, whose diffs
  show their whole tree as added, are left out and `history.shallow` is true. Per module: commits
  touching it, `local` (touching no other module), `local_share`, and partners by commits together
  with a Jaccard ratio (together over either), to three decimals. A pair is a hidden coupling when
  it changed together in at least 3 commits (the co-change minimum) and no relationship is drawn
  either way between the two or modules containing them, and neither contains the other. Locality
  counts the module owning each path, not its containers.
- T10: `model footprint [--base <rev>] [--head <rev>]` diffs from the merge-base of the base and
  the head to the head, or to the working tree with untracked files when `--head` is absent, through
  a disposable copy of the index with Git's stat refresh on, so a file whose stat alone changed is
  content-checked and left out and the user's index is never written; renames count as a deletion
  and an addition, and the vault's own files are left out. The base is `--base`, else the first of
  origin/HEAD, main or master, else the head itself; `base` is `{commit, source:
  explicit|merge-base|head-fallback, ref}` with the merge-base in `commit`. A shallow checkout
  without a default branch refuses `REQUEST_INVALID` rather than fall back. Each changed file's
  owner lists it under `files`, and every module containing that owner counts it in `in_parts`.
  `outside` lists changed files no binding holds. `crossings` are module pairs, neither containing
  the other, that an import edge with a changed file at either end joins, with the relationships
  covering each (empty when undrawn). They are null with `crossings_skipped` when not checked:
  `not-needed` (nothing to scan), `scan-failed` (with `scan_error`), or `head-not-checked-out`,
  since the scan reads the checkout and serves only a head equal to HEAD; with `--head HEAD` and a
  dirty tree the edges still come from the working tree. `model` is `model compare` of the head's
  current state against the merge-base's, read from that commit's tree as checkpoints are read
  (proposals left out; modes 100644 and 100755, anything else refuses `MODEL_INVALID`), or null when
  the base holds no model, or null with `model_error` naming the commit when its model does not
  load. With `--head` the model is the one committed there. The JSON always has every key, null when
  unset: `schema`, `kind`, `success`, `vault`, `revision`, `base`, `head`, `modules`, `outside`,
  `crossings`, `crossings_skipped`, `model`, `model_error`, `scan_error`, `large` and `proposal`
  (T19).
- T19: `large` is true when more files changed than a sweep holds (`cochange::MAX_FILES`, 30, the
  size past which locality leaves a commit out), files outside every binding included; it is
  information and never changes the exit status. `model footprint --proposal <id>` reads the
  proposal from the checkout's vault, whatever head the footprint reads, and sets `proposal` to
  `{id, name, named_and_changed, changed_not_named, named_not_changed, outside}`, each list of
  `{module, name, holds}` in ID order and `outside` a list of qualified IDs; an unknown proposal
  refuses `SUBJECT_NOT_FOUND`. Named: the modules the draft adds, removes or changes against the
  proposal's base snapshot (the checkout's current state when the draft has no base), and both ends
  of each relationship it adds, removes or changes. Changed: changed files map to modules through
  the proposal's bindings and the head's together, the deeper binding winning and the proposal's
  on an equal path, rolled up through the proposal's containment (the head's for modules the
  proposal removed), so a module the proposal adds or rebinds counts once its code
  changes and one it removes counts where the head still binds it; and the modules the branch
  changes in the model, by the same rule as named. Flows and steps name no module. `holds` (the
  held-subject rule) lists the changed modules inside a changed module when it holds none of the
  changed files itself, so every change it has lies in a part; it is empty for a module that holds
  a changed file, changes in the model, or did not change. `outside` is `changed_not_named` with
  empty `holds`. `--fail-outside` (needs `--proposal`, else clap exits 2) exits 1 when `outside` is
  not empty, after printing the output and naming each such module on stderr; files outside every
  binding never fail it, and without the flag the verdict never changes the exit status. A base or
  head that names no commit, or a base sharing no ancestor with the head, refuses
  `REQUEST_INVALID` (exit 2) as before. Markdown adds `Large change` and an `Against proposal`
  block, and the pull request comment the same after the model counts. A rebound module keeps the
  head's path too, so a file at its old path still counts for it. Claims are not read: they belong
  to one worktree, and a footprint judges a branch.
- T36: the action reads the pull request body through `gh api repos/<repo>/pulls/<n>` with the
  token, on `pull_request` events with a base, rather than the event payload, so an edited body
  counts. The state marker is one line `<!-- boundrail:proposal <id> -->` (surrounding spaces and
  CR allowed) whose ID is a local ID, 8 ASCII letters or digits, matched by a bash regular
  expression and passed as one argument; any other text after `boundrail:proposal`, or a second
  marker line, stops the step with exit 2 and an error that quotes nothing from the body. With a
  marker both footprint calls take `--proposal <id>`, so the comment and the whole summary carry
  `Against proposal`; `gate: outside` (input `gate`, `none` by default, anything else exit 2)
  adds `--fail-outside`, whose exit 1 joins the fail-on verdict. Held subjects need nothing in the
  action: `outside` already leaves them out (T19). Without a token the body is not read: a notice
  under `gate: none`, exit 2 under `gate: outside`. Without a marker every call is as before.
  `tests/action.rs` checks the flags exist in `model footprint --help`, the calls with and
  without the gate, and that hostile markers stop before any footprint runs.
- T36: the skills are the workflow; no command changed. Slices live in the proposal's
  `--description` (set only at `propose` until T26) or as beats of a current-state walkthrough
  reading the proposal against the current state with `focus` on the slice's modules, since a
  proposal edit refuses walkthroughs. A deviation is a proposal edit whose doing line names it.
  The temporary model is a proposal committed on the branch and shelved after the merge; since
  T31 it is drawn in a private vault outside the repository, and the committed proposal is the
  alternative.
- T11: `model view [--port N] [--no-open]` serves the page and a JSON API on 127.0.0.1 only, port 0
  (a free one) by default, and prints the URL with its token as the first line on stdout. Without
  `--no-open` it starts the platform opener (`open`, `xdg-open`, `cmd /c start ""`) with a one-time
  launch URL, `/launch?c=<code>`, as one argument, never through a shell string, so the token stays
  out of every process's arguments. The code is a second OS-random value, never the token. The first
  request naming it gets a 303 to `/?t=<token>` and spends it; an unknown or spent code gets 403.
  Connection threads run with the invoking thread's tool timeout. The page
  (`src/arch/view/index.html`, `app.js`, `style.css`) is embedded with `include_str!`, and its font
  (`geist.woff2`, served at `/font.woff2`) with `include_bytes!`; it uses no framework and loads
  nothing from other origins, so it works offline.
- T11: the API answers with the command outputs through the same functions the CLI prints:
  `/api/model?proposal=&checkpoint=` is `model read` (both at once refuse `REQUEST_INVALID`, 400),
  `/api/render?view=&flow=&scope=&depth=&proposal=&checkpoint=&against=` is `model render` as
  `image/svg+xml`, `/api/check?proposal=` is `model check` with its skips, `/api/proposals`,
  `/api/checkpoints`, `/api/compare?proposal=&against=` and `/api/inspect?subject=&proposal=` are
  their commands. A refusal is its JSON output with 404 for `*_NOT_FOUND` codes and 400 otherwise;
  any other error is a 500. An empty parameter counts as absent. `/api/events` is a Server-Sent
  Events stream that sends `event: changed` with `{"revision": ..., "parts": [...]}` once the
  watched files have been quiet for 200 ms (only a relevant event restarts the wait), and a
  keepalive comment every 15 s so a closed page ends its thread. The revision is the current
  state's, or null when it does not load, computed once by the watcher for every stream. The parts
  say what changed: `model` (the current state's files), `proposals`, `state` (this worktree's `<git
  dir>/boundrail`: claims, journal) and `history` (`refs/boundrail` and `packed-refs` in the common
  Git directory, so checkpoints retained from any worktree show); a stream that slept through
  several changes gets every part. The watcher follows no symbolic links, ignores read access, and
  watches only those folders, deeply, plus their parents shallowly so it sees them appear. The page
  reloads everything for `proposals` or `history`, for a revision other than the last event's (null
  included; the first load's until an event comes) or after a failed load of the current state, only
  claims and checks for `state`, and nothing otherwise; `/api/model` answers are kept per state
  until the next change.
- T11: security. The server binds the loopback interface only. Each run's token is 256 bits from the
  operating system's random source (`/dev/urandom`, or the system RNG on Windows), from
  `lib::token`; a target with neither refuses to start. Every `/api/` request carries it as
  `t` or the `X-Boundrail-Token` header, compared in constant time, or gets 403. The page drops the
  token from the address bar once read and keeps it in the tab's session storage, so a reload works.
  The page and its three assets carry no data and need no token. Every request must name exactly one
  Host, `127.0.0.1:<port>` or `localhost:<port>`, else 403, which stops DNS rebinding. Only GET is
  served (405 with `Allow: GET` otherwise). A request head is capped at 16 KiB (431) and must arrive
  whole within 10 s (408), however slowly it drips. At most 64 connections are served at once
  besides 16 event streams; more get 503, and a failed thread start or accept never stops the
  server. Every response sends `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and `Connection: close`, the font included. The page's
  Content-Security-Policy allows only its own script, style, font (`font-src 'self'`) and
  connections; SVG is parsed with `DOMParser` and imported, never run, and `/api/render` opened on
  its own carries `default-src 'none'; style-src 'unsafe-inline'`.
  The server writes nothing but its state file (T33): every endpoint is a read the CLI already
  does. The hardening exception of T7 covers its reads too: a clean or process filter driver
  repository config defines may run when Git content-checks a working-tree entry.
- T11: limits. ponytail: a thread per connection under the caps, fine for one person's pages; a pool
  if many share a server. The vault watch is set at start: a vault folder deleted and made again is
  not followed. Checks and interfaces scan the checkout on each request, as the commands do, so
  `/api/check` and `/api/inspect` take as long as `model check` and `model inspect`; checkpoints
  have neither. The page no longer inverts the rendered SVG for dark mode: its CSS maps the
  renderer's colours to the theme's tokens, and each kind's tone is the configured node-kind colour
  when it is `#rgb` or `#rrggbb`, else the palette's. `#<qualified module ID>` after the URL selects
  that module when the page opens.
- T12: a module binds an infrastructure unit with the existing binding fields, no schema change:
  `path` names a compose, Kubernetes or Terraform file the collector reads and `symbol` names the
  unit, a compose service, a workload or `TYPE.NAME`. What a binding means is inferred from where
  its file sits (a compose file name, a YAML file below a Kubernetes folder, a `.tf` file); the
  upgrade path is a separate `unit` field on the binding, which would also let one module bind
  code and a unit together. The unit is looked up in the file's folder, which is one compose
  project or one Terraform module, so a binding to `docker-compose.yml` finds a service an
  override file declares. A missing file is `BINDING_PATH_MISSING`, a unit the folder does not
  declare `BINDING_SYMBOL_MISSING`. Such a module owns no scanned file, needs no scan, is never a
  `symbol-binding`, and holds no path for `model locality` and `model footprint`. Until the
  `unit` field exists a module binds either code or one unit: a compose service built from a
  directory maps to that directory's owner without a binding, but Kubernetes workloads and
  Terraform resources need a module bound to them.
- T12: files read (`src/arch/infra/`). A walk at most 8 folders deep, skipping hidden folders,
  `node_modules`, `vendor` and `target`, lists candidates by file name and place; the scan's
  exclusion policy then drops tests, fixtures, examples, stories and gitignored files, but not
  generated folders such as `build/` or `dist/`. No file or env file is read through a symbolic
  link. Compose files (`compose.y(a)ml`, `docker-compose*.y(a)ml`) at the root and up to 2 folders
  below, base names before overrides, an override's `build` replacing the base's: each service's
  `environment` (map or `KEY=value` list), `env_file` (string, list, or entries with `path`, each
  file read once), `links` aliases and `build` or `build.context`; `${NAME:-default}` and
  `${NAME-default}` read as their default. Kubernetes YAML anywhere below a `k8s`, `kubernetes`,
  `deploy` or `manifests` folder and not below `charts`, multi-document, with Deployment,
  StatefulSet, DaemonSet, Job and CronJob pod templates (labels, container and init container
  `env` with `value` or `valueFrom.configMapKeyRef`), Services with their selectors, and ConfigMap
  `data`. Terraform `*.tf` `resource "TYPE" "NAME"` blocks. Workspace manifests: the npm members
  of the workspace inventory (`package_splits::workspaces::discover`) when the root declares a
  workspace (`workspaces` or `pnpm-workspace.yaml`), with dependencies of all four kinds naming
  another member, a name two members share left out; `Cargo.toml` path dependencies in the
  dependency tables, the workspace's and each target's; and `go.mod` `replace` directives to a
  `./` or `../` path. `depends_on`, `image`, `ports` and `expose` are not read: none names an
  address. A file that does not read or parse is left out, and a compose file without `services`
  counts as not read. A unit binding to a file that exists but was not read (a hidden, dependency,
  test or ignored folder, too deep, or past a symbolic link) is skipped as `not-scanned`, and its
  relationships with it. When the read itself fails, as when the exclusion policy refuses, the
  command warns on stderr and goes on without infrastructure: unit-bound modules are skipped as
  `scan-failed` under a `SCAN_FAILED` warning naming them, inspect and footprint set `scan_error`,
  and footprint skips its crossings as `scan-failed`.
- T12: address rules. Parts of a value split at whitespace, commas and semicolons. A part names a
  host as the host of a URL (after `://`, past any user info, before the port and path); as
  `host:port` with a numeric port, only when the value is that one part or holds a URL, so
  `mongodb://a:1,b:2` names both but `billing:3,search:1` none; or, when the variable name ends
  with `HOST`, `HOSTNAME`, `ADDR` or `ADDRESS`, as a host alone. A part with `@` but no scheme,
  such as an email address, names none. A host matches only as a whole name, so `b` never matches
  inside `abc` or `b.example.com`. A compose host matches a service of the same project by name or
  by a link alias. A Kubernetes host `svc` names a Service of the workload's namespace, and
  `svc.ns`, `svc.ns.svc` or `svc.ns.svc.cluster.local` one of `ns`; the hop goes to every workload
  of the Service's namespace whose pod labels hold its non-empty selector. An object whose
  manifest names no namespace is in any namespace of its own folder, and else in `default`.
  ConfigMap keys resolve the same way. A Terraform reference is a `TYPE.NAME` token in another
  resource's body of the same folder, outside comments and string text, inside `${...}` included
  (strings inside it hide their braces); `var`, `local`, `module`, `data` and the other
  non-resource roots are not followed. Each hop records the file and line where the address is
  written (for a ConfigMap key, the ConfigMap's line; a line is the one setting the variable as a
  key, `name:` or `name=` at its start), the variable, attribute or dependency name, and the
  address.
- T12: judging. Evidence is collected per relationship. Configured-path evidence counts only
  when both ends hold a unit (the unit's owner or a module containing it), and build-dependency
  evidence only when both ends hold a workspace package folder; otherwise the relationship keeps
  the T5 `evidence-unavailable` skip, so an unbound database or a stray `.tf` file changes
  nothing. Binding any module to a unit opts the model into infrastructure judging, build-context
  mapping included: without one, configured-path evidence is never collected and configured hops
  report no `RELATIONSHIP_UNDRAWN`, though they still support relationships and are still listed
  in `units`, inspect hops and footprint hops; build-dependency evidence needs no opt-in. A custom
  kind accepting only build-dependency evidence therefore becomes strict between modules that
  hold workspace packages. Code evidence (call, import) never applies to a relationship with an
  end bound to a unit: a `call` between two workloads is `evidence-not-code`, and a `queue` from
  code to a unit-bound broker is judged on configured paths alone. Units map to modules by
  binding, else by `build.context`; package folders map through the deepest binding as files do.
  A hop of an
  evidence kind that applies, from the source or a part of it to the target or a part of it,
  supports the relationship. With every applicable kind collected, a hop only the other way is
  `RELATIONSHIP_BACKWARDS` (`only Api names the address of Rabbit`, or `depends on`), and none is
  `RELATIONSHIP_UNSUPPORTED`, naming what is missing. A configured hop between two modules,
  neither containing the other, that no relationship covers is `RELATIONSHIP_UNDRAWN`
  (`worker.env:2 API_URL names api`), once per pair and not when an import edge already reports
  it. Build-dependency hops only support. `model check` lists `units` beside `skips` when the code
  check runs: each unit with `kind`, `name`, `file`, `build`, `owner` and `reason`
  (`infra-unbound` when `owner` is null); Markdown counts `unit infra-unbound`. `model inspect`
  lists each module's `hops_in` and `hops_out`; `model footprint` crossings gain `hops`, the hops
  written in a changed file, and without a scan the crossings are the hops alone when a module is
  bound to a unit. The new keys are always present, null or empty when unset, as footprint keeps
  its keys, rather than skipped. The infrastructure is read from the checkout, only when a
  footprint has changed files and its head is checked out.
- T12: limits. ponytail: a hand reader of Terraform blocks over masked text, not an HCL parser:
  heredoc interpolation is ignored and modules and variables are not followed; YAML merge keys
  (`<<`) are not applied; `envFrom`, Helm templates and Kustomize overlays are not read; compose
  files of one folder are one project, so two projects need two folders; Kubernetes Services and
  ConfigMaps are indexed by name and filtered by namespace, since an object may name none. Env
  files are read even when gitignored, as compose reads them. A module whose code is not scanned
  (Python outside its package) keeps its `not-scanned` skips, and a compose service built from its folder
  shows as `infra-unbound`. The scan-era service inference reads code facts, not infrastructure,
  so it is not reused.
- T13: config. Two files, each read only when it is a regular file under 1 MiB (else
  `REQUEST_INVALID`). The committed `.boundrail/language-servers.json` at the checkout root is an
  array of `{server, languages?, extensions?}`, unknown keys refused, so it can name a server but
  never a command, arguments or options. `server` names an entry of the user's config, else one
  of the allowlist `KNOWN_SERVERS` in `src/arch/lsp/config.rs`, each run as its own name with
  fixed arguments and default languages and extensions: `typescript-language-server --stdio`
  (TypeScript and JavaScript extensions), `basedpyright-langserver --stdio` and
  `pyright-langserver --stdio` (`py`, `pyi`), `rust-analyzer` (`rs`), `gopls` (`go`) and `jdtls`
  (`java`). Elixir's server ships as a `language_server.sh` script, not a program on `PATH`, so it
  is left out. On Windows a name resolves to a `.exe` only; `.cmd` shims are not run, since `cmd`
  re-parses their arguments, so the npm-installed TypeScript and Pyright servers work on Unix
  only unless the user defines them. The user's config,
  `$XDG_CONFIG_HOME/boundrail/language-servers.json` (an absolute `XDG_CONFIG_HOME` only), else
  `~/.config/boundrail/language-servers.json`, or `%APPDATA%\boundrail\language-servers.json` on
  Windows, is an array of `{name, command, args, languages, extensions,
  initialization_options?}`; it defines custom servers or redefines an allowlisted name, and its
  definition wins. Its `command` is a program name or an absolute path. An entry's `languages`
  and `extensions` replace the server's. `extensions` (with or without the dot, stored without)
  decide which files a server covers; `languages` are LSP language identifiers, and a file opens
  as the standard one of its extension when the list holds it, else as the first.
  `initialization_options` goes to `initialize` as given, and only the user's config sets it,
  since options can name programs a server runs (rust-analyzer's `check.overrideCommand`,
  typescript-language-server's `tsserver.path`).
- T13: security. `--language-servers` runs code from the checkout: the TypeScript server runs the
  workspace's `tsserver.js` under `node_modules` and its `tsconfig.json` plugins, rust-analyzer
  runs build scripts and proc macros and reads a committed `rust-analyzer.toml`, and jdtls runs
  Gradle and Maven imports. It is for trusted checkouts only and must never be used on untrusted
  pull request heads in CI; the flag's help says so. The servers run only with the flag on `model
  check` or `model inspect`, and `model view` never runs them. Without it neither config is read,
  and a skip `{subject: ".boundrail/language-servers.json", site: "language-server", reason:
  "language-servers-not-enabled"}` is listed only when the repository's file exists. With it, a
  file that does not parse, an entry with any other key (`command`, `args`,
  `initialization_options`), an entry naming a server neither allowlisted nor defined by the
  user (the refusal names the allowlist and the user's config path), or a user command that is
  empty or a relative path refuses `REQUEST_INVALID` before any server starts. A program name is
  looked up in the absolute folders of `PATH` only (`collect::resolve_program`) and must be an
  executable file that, once links are resolved, lies outside the checkout; the server's own
  `PATH` holds only the absolute folders, so neither Boundrail nor the server (an `env
  node` launcher, say) finds a program a relative entry would reach in the checkout. A server
  installed inside the checkout, such as `node_modules/.bin/typescript-language-server`, is
  therefore no longer found by name; an absolute `command` in the user's config is the
  workaround. The server's `PATH` still keeps absolute folders inside the checkout, unlike the
  rasterizer's. `boundrail:
  starting language server <name>: <program> <args>` goes to stderr before it starts, with the
  resolved program path and shell-quoted arguments. It runs in the checkout root. This is the
  third stated exception in `src/git.rs`'s hardening header, gated by the flag. The servers may
  leave build output and project metadata in the checkout (rust-analyzer `target/`, jdtls
  `.project`, `.classpath` and `.settings`), while Boundrail itself writes nothing there; the
  flag's help says so too.
- T13: client (`src/arch/lsp/`: `config.rs`, `wire.rs` for URIs and result mapping, `framing.rs`,
  `client.rs`, and `mod.rs` for the evidence). JSON-RPC 2.0 over stdio with `Content-Length`
  framing; a body over 16 MiB or a header line over 1 KiB is an error, and the reader holds at
  most 4 messages before it stops reading. `initialize` names the canonical checkout as `rootUri`
  and the one workspace folder, with client capabilities limited to references, definition,
  document symbols and workspace symbols; then `initialized`, `textDocument/didOpen` of each
  definition file and the first covered file of each binding or anchor symbol, `workspace/symbol`
  once per name, `textDocument/documentSymbol` of single-file lookups when the server offers it,
  `textDocument/references` (declaration excluded) per interface symbol, and `shutdown` (2 s)
  then `exit`, the server killed when it has not exited a second later. A reader thread drains
  stdout and a writer thread feeds stdin, so a server that stops reading cannot block past a
  timeout. Server notifications are ignored; server requests are answered,
  `workspace/configuration` with one null per item and anything else
  (`client/registerCapability`, `window/workDoneProgress/create`) with null. Stderr is read on a
  thread and dropped, keeping the last 5 lines (512 bytes each, control characters replaced) for
  the failure warning. Each request may take 60 s, within a session budget of the tool timeout
  (`--tool-timeout`, 600 s by default), or 600 s when it is 0; a request past either fails the
  session, and dropping the session kills the server.
- T13: evidence. Files map to modules exactly as the code check maps scanned files. A module's
  interface symbols are those of `model inspect` (exports of its entry points, or of every file it
  owns), asked about at their definition (the export's target): `workspace/symbol` finds the
  declaration of that name in that file, and references are asked at the name's first whole-word
  occurrence from the declaration's start, since a range may begin at a keyword. A reference from a
  scanned file of one module into another, as credited below, is a call hop. A module is served when
  every file it owns is covered by one server that answered, no file it owns is named by a symbol
  limitation, it has at most 64 interface symbols, and the server found every one of them; a
  container is served only when every judged part below it is. A served module counts as a call
  target only when it, or a judged part below it, has an interface symbol a server located, and
  every judged module in that subtree owning a file, itself included, has one; into any other module
  imports stand in. A reference belongs to the module of the file its use site imports the symbol
  through (the written target of an import edge from the use-site file to the definition file, or to
  a file exporting the symbol), and to the defining module only without such an edge, as import
  edges are credited. Between two served modules, call evidence is the references, replacing the
  import approximation: a hop from the source or a part of it into the target or a part of it
  supports a kind accepting `call`; under the T5 strictness rules a hop only the other way is
  `RELATIONSHIP_BACKWARDS` (`only Web calls Bank`) and none is `RELATIONSHIP_UNSUPPORTED` (`never
  calls`). Elsewhere the import approximation stays. Import evidence (`dependency`) is still import
  edges. `RELATIONSHIP_UNDRAWN` gains call hops between modules no relationship covers
  (`src/web/index.ts:2 calls pay`), once per pair and not when an import edge already reports it.
  For binding and anchor symbols, where the scan cannot tell (no symbol data, or a file a limitation
  names), the servers decide: a declaration of that name in one of the files, from
  `workspace/symbol` or a symbol list, confirms it; a binding or anchor of one file whose
  `textDocument/documentSymbol` list is not empty and lacks the name refutes it,
  `BINDING_SYMBOL_MISSING`/`ANCHOR_SYMBOL_MISSING` (`no file there declares refund`), but only when
  the name is not `default` (a default export lists under its own name), the file's text does not
  hold it as a whole word, and no server found it declared anywhere. An empty answer refutes
  nothing, since a server may not have indexed the file, so such a symbol, and any symbol of a
  folder the servers do not find, keeps its `symbols-incomplete` or `language-unsupported` skip. A
  complete scan's verdict stands. `model inspect` counts `used` from references for served modules,
  lists `unused` even when the scan's coverage is incomplete, and adds `source: "language-server" |
  "scan"` to `symbols`; its `language_servers` key lists the language-server skips, empty when none.
- T13: failures. A missing program, a crash, a protocol error or a timeout prints `warning:
  language server <name> failed: <reason>; stderr: <last lines>` and adds the skip `{subject:
  <name>, site: "language-server", reason: "language-server-failed"}`; the check never fails for
  it and uses none of that server's answers, falling back to imports. A server with nothing to
  answer is not started.
- T13: limits. ponytail: one session per server per command, and every question in it, so a large
  repository can run past the budget and fall back to imports; a module with more than 64
  interface symbols keeps the import approximation (ask in batches when real modules need more);
  a mixed-language module is served only by a server covering all its files; only the direct
  child is killed on timeout, so a server's own helper processes may outlive it, as for the
  collectors. Languages are whatever servers the config names; the evidence needs the scan's
  symbol data, so it reaches the languages the native engine indexes, and modules the scan does
  not judge (Python outside its package) stay `not-scanned`. Tried by hand with
  `typescript-language-server --stdio` on a three-module TypeScript checkout: references
  supported the drawn calls, a line without one reported `never calls`, and inspect counted uses.
- T13: tests. No real language server runs in CI. `tests/fake_lsp/server.rs` is a standard-library
  program the integration tests build once per test process with the toolchain's `rustc` (beside
  `$CARGO`, or `$RUSTC`) into a folder of `CARGO_TARGET_TMPDIR` named for the process, since
  `cargo test --test arch_model` builds no example or extra test target, copy it as
  `typescript-language-server` too (both with the platform's executable suffix), and put that
  folder first on `PATH`. Each test points `XDG_CONFIG_HOME` at a temporary folder whose user
  config defines `fake` with the script as its argument, and the repository names it. The script
  is `method<TAB>needle<TAB>result` lines: a request gets the first matching result, else null,
  and `hang` stops it answering. Run as the allowlisted name it gets `--stdio` and reads its
  script from `$BOUNDRAIL_FAKE_LSP_SCRIPT`. It asks the client for its configuration first, as
  real servers do. A Unix test starts it through `/usr/bin/env` with a planted program of its name
  on a relative `PATH` entry, which never runs.
- T14: a walkthrough is `walkthroughs/<id>.walkthrough.json` (schema `model-walkthrough`):
  `{schema_version, id, name, order, description?, beats}`, each beat `{id, title, narration,
  reading}` and each reading `{view, state?, against?, scope?, depth?, flow?, focus?}`. A state is
  `current` (the default, left out of the file), `{"proposal": <qualified ID>}` or
  `{"checkpoint": <ID>}`; module and flow references are qualified. Walkthroughs belong to the
  current state: its revision, `file_digests`, the journal's write allow-list, checkpoints and
  committed reads cover them, and proposal loads never read a `walkthroughs` folder; a proposal
  edit with a walkthrough change refuses `REQUEST_INVALID`. Walkthrough and beat IDs come from the
  model-wide allocator and are retired on removal. Limits: 200 beats (`BEATS_MAX`), a title of 60
  characters on one line, a narration of 1 to 2000 characters with line breaks
  (`NARRATION_MAX`), depth 1 to 9 (`READING_DEPTH_MAX`).
- T14: validation. A reading keeps the `model render` rules: sequence and flowchart need a flow,
  the architecture view takes none, scope and depth are for the architecture view, `against` is
  the current state or a proposal, never the drawn state and never newer than it (`state` is
  always the state drawn), and focus lists a module once.
  Reading the current state, scope, focus and flow must exist; reading a proposal or checkpoint,
  they and the proposal must only be qualified IDs of this model (a checkpoint 8 base62
  characters), since those states change on their own. Adoption checks the adopted state with the
  current walkthroughs, so a beat reading a module or flow the proposal drops refuses
  `MODEL_INVALID` until it is edited. A proposal's predecessor digest leaves out the
  `walkthroughs/` files, so editing a walkthrough does not move it; `expected_revision` still
  names the full revision. It also hashes `model.json` with `retired_ids` emptied, since removing
  a walkthrough or picture grows that list. A draft recorded before that change holds the digest
  of `model.json` as it was on disk, which adoption still accepts.
- T14: edits. `add-walkthrough` (with beats), `edit-walkthrough` (set name, order, description;
  clear description), `remove-walkthrough`, `add-beat` (walkthrough, position), `edit-beat` (set
  title, narration and any reading field; clear state, against, scope, depth, flow, focus),
  `remove-beat`, and `move-beat` (to a position of the same or another walkthrough). A
  walkthrough resolves by ID or unique name, a beat by ID. Current-state references resolve like
  step references (module names, flow names); others are qualified as given. `edit-beat`
  resolves only the fields it sets, so a stored reference the batch broke is judged by
  validation. Focus is deduplicated in order. Walkthroughs have no former names. Removing a
  module or flow (or the collection holding it) that a current-state beat the batch did not edit
  reads refuses `REFERENCED_BY_WALKTHROUGH` with the beats in `details.beats`, after
  `REFERENCED_BY_FLOW`; an edited beat is `MODEL_INVALID` instead. The receipt lists walkthroughs
  as subjects, not beats. A claim on a walkthrough covers its beats; adding or removing one
  touches the model ID, and moving a beat touches both walkthroughs.
- T14: `model check` adds `WALKTHROUGH_EMPTY` (information) and `WALKTHROUGH_TARGET_MISSING`
  (warning) for a beat whose proposal or checkpoint does not load, or whose scope, focus or flow
  the state it draws lacks; each such state loads once per check, through `load_proposal` and
  `checkpoint::load`, and only when a beat names it. `model read --walkthrough` (or `model read
  <walkthrough ID>`) prints it with qualified IDs, or as a script in Markdown: numbered beats with
  the narration indented and a `Shows:` line; `model read` lists walkthroughs with their beat
  counts. `model render --walkthrough W --beat N|ID` draws the beat's reading through the same
  path as the flags, which it refuses beside it (`REQUEST_INVALID`); a beat number counts from 1.
  A focus highlights the focused modules and everything inside them, keeps the containers around
  them, and wraps the rest in `<g class="dimmed" opacity="0.3">`; an arrow or message stays when
  either end is focused. A focus the architecture drawing does not show dims nothing.
- T14: the browser lists the current state's walkthroughs in the sidebar. Playing one shows a
  panel above the canvas with the beat's title and narration, Previous and Next, one marker per
  beat, and Left and Right arrow keys (which pan the canvas otherwise). Each beat sets the active
  pane's state, comparison, view, flow, scope and depth, draws through
  `/api/render?walkthrough=&beat=` so the server applies the focus, and fits the picture. Any
  change made by hand drops the beat from the pane. `#walkthrough=<id>&beat=<n>` opens a beat
  and follows the playback; a live update reads the walkthrough again and stays on the same beat
  number. `/api/walkthrough?id=` is `model read --walkthrough`.
- T15: a picture is `pictures/<id>.picture.json` (schema `model-picture`): `{schema_version, id,
  name, order, description?, output, reading}`, the reading a walkthrough beat's, validated the same
  way (render rules, newer state in `state`, current-state references must exist). `output` is a
  repository path (`is_repo_path`, no empty or `.` segment) ending in `.svg` or `.png` (lowercase),
  not below `.boundrail` and with no `.git` segment, compared as Windows resolves names (any case,
  trailing dots and spaces dropped, `git~1` naming `.git`); no two pictures share an output,
  compared without case. Pictures belong to the current state as walkthroughs do: its revision,
  `file_digests`, the journal's write allow-list, checkpoints and committed reads cover them,
  proposal loads never read a `pictures` folder, a proposal edit with a picture change refuses
  `REQUEST_INVALID`, and a proposal's predecessor digest leaves `pictures/` out. Picture IDs come
  from the model-wide allocator and are retired on removal. Edits: `add-picture`, `edit-picture`
  (set name, order, description, output and any reading field; clear description and the reading
  fields as `edit-beat` does), `remove-picture`, which leaves the drawn file in place. A picture
  resolves by ID or unique name. Removing a module or flow a current-state picture the batch did not
  edit reads refuses `REFERENCED_BY_PICTURE` with `details.pictures`, after
  `REFERENCED_BY_WALKTHROUGH`. A claim on a picture holds it; adding or removing one touches the
  model ID. Adoption checks the adopted state with the current pictures, as with walkthroughs.
- T15: `model pictures [--write] [--only <id|name>]` lists each picture in order with `id`,
  `name`, `output`, `format` (`svg`|`png`), `reading`, `status` (`fresh`, `stale`, `missing`,
  `unrenderable`), `reason` and `message` when unrenderable, and `written`. Reasons:
  `render-failed` (the reading's proposal or checkpoint does not load, or the render refuses),
  `output-blocked` (a symbolic link or non-folder on the way, an output that is not a regular
  file or fails the output rules, a file Boundrail did not draw, or a failed write), and with
  `--write` only `no-rasterizer` (naming the tools tried) and `rasterizer-failed`. `--write`
  draws every stale or missing picture and leaves fresh ones untouched; one picture's failure
  never stops the others, and the command exits 0 once it prints the statuses, an unrenderable
  picture among them; a refusal still exits 2. It writes only the declared output
  files, never the vault, so it takes no revision, operation ID or journal: each file is replaced
  through a temporary file and a rename, missing folders are created one at a time inside the
  checkout, and no symbolic link is followed or written through. Boundrail overwrites only a file
  it drew: an SVG whose root element carries `data-generator="boundrail"`, which every render
  now has, or a PNG with its source chunk. Any other existing file blocks the picture (`exists and
  was not drawn by Boundrail; delete it to draw it here`); a missing file is always written. `model
  render --picture <id>` draws a picture's reading with its focus through the beat path, refusing
  any other flag that says what to draw; `model read` lists pictures, and `model read --picture
  <id>` (or `model read <picture ID>`) prints one with qualified IDs, or in Markdown its file and a
  `Shows:` line.
- T15: freshness. An SVG is fresh when the file's bytes equal the deterministic render, or do once
  CRLF line endings become LF, as a checkout may store text. A PNG carries a `tEXt` chunk
  `boundrail-source` right after `IHDR` whose text is the SHA-256 (lowercase hex) of the SVG it
  was rasterized from; Boundrail inserts it after rasterizing, replacing any it held and copying
  the other chunks byte for byte, with chunk parsing and CRC-32 written by hand
  (`src/arch/picture.rs`). A check reads the first 4096 bytes of a PNG and walks its chunks from
  `IHDR` to the source chunk, stopping at `IDAT`. A PNG is fresh when that chunk names the digest
  of the SVG the picture draws now, and stale when it names another; a PNG without it, or whose
  chunk fails its CRC, was not drawn by Boundrail. Checking never rasterizes, so rasterizer output
  may differ between machines and versions without making a picture stale.
- T15: rasterizing. Boundrail picks the rasterizer, never the repository: the first of
  `RASTERIZERS` in `src/arch/picture.rs`, `resvg` (`in.svg out.png`), `rsvg-convert` (`-o out.png
  in.svg`) and `magick` (`in.svg out.png`), found by bare name as an executable file in the
  absolute folders of `PATH` and lying outside the checkout once links are resolved, the
  language servers' lookup (`collect::resolve_program`). It runs with `PATH` holding only the
  absolute folders that lie outside the checkout by the same test (`collect::path_outside`), in a
  private temporary folder (mode 0700) holding only the SVG, under the tool
  timeout. The SVG is Boundrail's own render; the repository supplies only the model text it
  draws.
- T15: `model check` gains an embeds part, `--only embeds`, run only when the current state has
  pictures, so models without pictures see no change; with `--proposal` it refuses
  `REQUEST_INVALID`, since pictures belong to the current state. `PICTURE_TARGET_MISSING`
  (warning) comes from the walkthrough target check, fed the picture readings: the proposal or
  checkpoint does not load, or lacks the scope, focus or flow; a render failing for another
  reason reports it too. `PICTURE_STALE`, `PICTURE_MISSING` and `PICTURE_UNRENDERABLE` (an
  `output-blocked` file) are warnings. A picture whose freshness is unknown adds a skip
  `{subject: <picture>, site: "picture", reason: "unrenderable"}`. When the history ref is not in
  the repository, as in a clone that did not fetch it, every beat or picture reading a checkpoint
  adds a skip `{subject: <beat or picture>, site: "walkthrough" or "picture", reason:
  "history-not-local"}` instead of a warning (CI can run `model history fetch` before `model
  check` to judge them), and such a picture skips its freshness check too; a
  ref that lacks the checkpoint, or a checkpoint lacking the scope, focus or flow, still warns. So
  `skips` appears when the code check ran, a picture could not be checked, or a reading was
  skipped. A missing rasterizer never reaches the check.
  Each proposal or checkpoint a beat or picture reads loads once per run (`semantic::States`),
  shared by the target check and every picture drawn, and the current state loads once.
- T15: the browser lists the recipes in the sidebar from `/api/pictures`, which without `status=1`
  lists them as `model read` does and draws nothing; with it, it is `model pictures`. The Pictures
  list asks for per-file statuses only through the section's `Check files` button, so reloads never
  ask for them, and drops them on each reload. The Warnings panel's `/api/check` runs the embeds
  part as `model check` does, so picture warnings show without it. Clicking a picture applies its
  reading to the active pane and draws it through `/api/render?picture=`, the beat path, so the
  server applies the focus; after a reload a pane showing a picture follows its recipe, or keeps
  what it shows once the picture is gone.
- T15: limits. ponytail: every check and listing renders each picture once; cache renders per
  reading if checks over many pictures get slow. Output folders are checked, then used, not
  opened relative to a folder handle, so a link swapped in between is followed; open each
  segment without following links if outputs land where others write. No test runs a real
  rasterizer (none was installed here); a fake `resvg` script stands in.
- T17: base snapshot. A new draft keeps `proposals/<id>/base/` with `modules/` and `collections/`
  as its predecessor held them, and `proposal.json` says so with `"base": true` (left out when
  false), since an empty predecessor leaves no base files to show it. The base is part of the
  proposal's revision, `file_digests` and the journal's write allow-list, so an edit, adoption or
  recovery covers it, but loads never parse it: reads, compare, render and checks see only the
  draft. Only `model reconcile` reads it (`vault::load_base`, parsing without validation). Edits
  never write it. `propose --from <draft>` copies the source's base (none when the source has
  none); a draft from the current state, or from an adopted proposal that still is it, takes the
  current state's modules and collections. A draft drawn before T17 has no base. Base files are
  parsed and must be named by a local ID matching their item (`MODEL_INVALID`), so their writes
  never leave the folder. ponytail: each draft stores a second full copy of the model it was
  drawn from, and every proposal load reads and hashes it for the revision; hash the base lazily
  (only for writes and reconcile) if drafts of large models make reads slow.
- T17: `model reconcile <proposal> --expected-revision --expected-proposal-revision --operation-id
  --doing [--writer] [--claim]` merges base, draft and current state (`src/arch/merge.rs`).
  Granularity: a module's own fields (all but `outgoing`) are one subject and each relationship
  another, with its owning module as a field, so a relationship added on one side to a module
  renamed on the other merges, and a moved relationship is one change. A collection is its whole
  file, flows and steps included: flows and steps move between lists and collections, and a finer
  merge would need those moves as subjects of their own. A subject one side changed takes that side;
  both sides making the same change merge. Different changes are `both-changed`, a change against a
  removal `changed-removed`, a relationship whose owner the other side removed `changed-removed`
  (unless the owner is in conflict already), and an ID the current state retired that the merged
  draft holds `retired`. Any of them refuses `RECONCILE_CONFLICT` with `details.conflicts`
  (`subject`, `kind`), writing nothing; the user edits the draft to match one side and reconciles
  again. A merged module equal to one side's (relationship order aside) is that side's file, so
  unchanged files are not rewritten. Refusals in order: `PROPOSAL_CLOSED`, `STALE_REVISION` for
  either revision, `RECONCILE_NO_BASE`, `RECONCILE_CONFLICT`, claims, `MODEL_INVALID` for a merged
  draft that does not load, saying the problems come from merging the draft with the current state
  (`details.cause: "merge"`) and that the draft needs editing. The write is one journaled operation
  of scope `reconcile` (the current state and the proposal, like adoption, writing only the
  proposal): the merged files, a base equal to the current state, the predecessor set to the current
  state and its normalized predecessor digest, and the dropped list grown by the IDs the merge
  removed, less those the current state retired. Claims cover what the merge changes in the draft,
  as an edit would, plus the proposal.
- T17: bundles. `model export [--proposal P | --checkpoint C] [--output FILE]` prints one JSON
  bundle (`model schema model-bundle`): `{schema: "boundrail/bundle/v1", model: {id, name},
  source, files, digests}`. `source` is `{kind: "current", state, revision}`, `{kind:
  "proposal", proposal, name, revision}` or `{kind: "checkpoint", checkpoint, commit,
  revision}`. `files` maps paths to text: the vault files of the current state (manifest, config,
  modules, collections, walkthroughs, pictures; claims and the journal live under `.git` and never
  travel) or of a checkpoint's tree, or a proposal's own files (`proposal.json`, `modules/`,
  `collections/`, `base/`) proposal-relative. `digests` holds each file's SHA-256, the digest the
  load read. A file that changed since the load refuses `STALE_REVISION`, and a bundle larger than
  the `INPUT_MAX` bytes import reads refuses `BUNDLE_INVALID` on export. `--output` uses render's
  output check, which now also refuses the Git folders: the resolved folder must exist outside the
  vault, the worktree's Git folder and the repository's common Git folder, and the file is
  replaced atomically, never written through a link; JSON then prints `{kind: "export", output,
  source, files}`, and Markdown without `--output` refuses `REQUEST_INVALID`.
- T17: `model import <FILE|-> --expected-revision --operation-id --doing [--name] [--writer]`
  draws a new draft from a bundle, leaving the current state alone. It refuses a request over
  `INPUT_MAX` or one that does not parse with `REQUEST_INVALID` (as every request), another
  model's bundle `MODEL_MISMATCH` (the header or the bundled manifest), and `BUNDLE_INVALID` for
  another schema, a path its source never holds, two paths differing only in case, a missing
  `model.json`, `config.yaml` or `proposal.json`, a digest that does not match, or a proposal base
  whose files are not named by their local IDs or fail this vault's rules (`details.path`, with
  the base's `problems`). The bundle's own text (schema, paths, model ID) goes into the details,
  never into a refusal's message. The files then load through a temporary vault, so every loader
  check applies (`MODEL_INVALID`): a current or checkpoint bundle with its own manifest and
  config, a proposal bundle with this vault's. The draft holds the bundle's modules and
  collections, named `--name`, the proposal's name or the state's. A current-state or checkpoint
  bundle's draft has the current state as predecessor and base. A proposal bundle's draft keeps
  that proposal's predecessor, base (none when it had none) and dropped IDs, so when this clone's
  current state differs from the one the proposal was drawn against, adoption refuses
  `PREDECESSOR_MOVED` and `model reconcile` merges the two. Walkthroughs and pictures in a
  current-state bundle are validated and not imported, since they belong to the current state. The
  fingerprint covers the writer, name, revision and the bundle's SHA-256. Claims: the model root.
- T17: publication. `model history push [--remote origin]` pushes the object the local history ref
  holds (`<object>:refs/boundrail/history`, with `--no-follow-tags`), without `+` and with
  `--force-with-lease=refs/boundrail/history:<remote tip or empty>`, so it moves the remote only
  from the tip it checked: that tip must be absent or an ancestor of the local one, and every
  first-parent commit the push adds must pass the checks a fetch applies (`MODEL_INVALID`
  otherwise, before anything is published). When the remote holds what the local history lacks, or
  the push is refused because the remote moved meanwhile, it fetches the remote ref onto a per-run
  ref and refuses `HISTORY_DIVERGED`, telling a remote that is only ahead (run fetch) from
  diverged histories, whose local-only checkpoint IDs go into `details.local_only` with the
  recovery: fetch replays them, then push again. Histories that share no commit refuse
  `HISTORY_DIVERGED` with `details.related: false` on push and fetch alike, since replaying would
  join unrelated histories. `model history fetch [--remote origin]` fetches the remote ref into
  the scratch ref `refs/boundrail/fetching/<pid>-<nanos>`, a per-run name that never collides with
  another fetch or a leftover ref, deleted on every exit path (a failed deletion after a good
  fetch only warns on stderr). A fetch killed by a signal leaves its ref behind; `git update-ref
  -d refs/boundrail/fetching/<run>` removes it, and nothing cleans it up automatically. The fetch
  passes an empty `--refmap=`, so a configured `remote.<name>.fetch` such as
  `+refs/boundrail/*:refs/boundrail/*` cannot move `refs/boundrail/history` while it runs. The
  fetched ref must peel to a commit, and every first-parent commit from it that the local history
  lacks must be one retain could have written: at most one parent (a merge refuses), checkpoint
  trailers, a Checkpoint ID that is a well-formed local ID unique in any case across the fetched
  walk, `--doing`, `Writer` and `Operation` values that pass retain's own checks, a `Revision` of
  exactly 64 lowercase hex characters, and a `Source`, when present, that is a full 40- or 64-hex
  commit hash; otherwise it refuses `MODEL_INVALID` (`details.object`) and the local ref stays. A local tip that already
  holds the fetched one stays. Diverged histories replay: the local-only first-parent commits,
  checked the same way, are copied in order onto the fetched tip with `commit-tree`, each keeping
  its tree, message, trailers and author and committer dates, signed as retain signs
  (`SIGNING_FAILED` before the ref moves). A local-only ID the fetched history holds in any case
  refuses `HISTORY_DIVERGED` with `details.clashing`: the remote holds earlier copies of those
  checkpoints, as when they were replayed from another remote. The history has one authoritative
  remote; the publication commands assume it, and their help says so. Publishing one history to
  two remotes that each take replays never converges, so the clash message names the recovery
  that does: make the non-authoritative remote's ref match the authoritative history (a forced
  update or a deletion outside Boundrail), or stop publishing to it. T7 lets retain build on a
  history tip that is not a checkpoint, but such a commit cannot be pushed or replayed: the push
  and the replay refuse `MODEL_INVALID` ("is not a checkpoint"), and the manual recovery, which
  the message names, is to rewrite the local history without that commit. The receipt lists
  `replayed`. Replaying replaces those commits, so stored retain receipts naming them go stale;
  readings name checkpoints by ID and still resolve, and a retain retried with the same operation
  ID returns the checkpoint as the history now holds it. Otherwise the local ref moves by compare-and-swap
  (`update-ref <new> <old or absent>`); a swap lost to a concurrent retain re-reads the tip and
  judges again, up to three times. Ancestry is `git merge-base --is-ancestor`, asked only when the
  other side's ref exists. `--remote` must be a name `git remote` lists, never a URL or anything
  starting with `-`, and reaches Git after `--end-of-options`; fetch passes `--no-tags`,
  `--no-recurse-submodules` and `--no-write-fetch-head`. Both print `{kind: "history-push" |
  "history-fetch", remote, ref, before, after, updated, checkpoints, replayed}` with the IDs
  gained; a missing local history or remote ref is no change. Once fetched, checkpoint readings
  resolve, so CI can run `model history fetch` before `model check`.

- T16: setup is `boundrail model setup`. `model setup [--name N] [--agent A]... [--language-servers]
  [--force]` creates the vault through `vault::init` when absent (named after the checkout folder
  unless `--name`), installs the `boundrail-model`, `boundrail-plan` and `boundrail-pr` skills,
  embedded with `include_str!` from `skills/`, into each agent's project skill folder
  (`.claude/skills`, `.agents/skills` for Codex as the project skill already uses, `.cursor/skills`,
  `.opencode/skills`), and with `--language-servers` writes `.boundrail/language-servers.json`
  naming the allowlisted server of each language a project file at the checkout root shows
  (`package.json` or `tsconfig.json`, `pyproject.toml`, `setup.py`, `setup.cfg` or
  `requirements.txt` for `pyright-langserver`, `Cargo.toml`, `go.mod`, `pom.xml` or Gradle files),
  nothing when none does. A file equal to what setup writes is `unchanged`, a missing one `written`,
  and a different one `kept` unless `--force` replaces it; no symbolic link on the way is followed.
  JSON lists `created`, `written`, `unchanged`, `kept`, `language_servers` (null without the flag),
  `notes` (what setup left alone and why, such as a `--name` unused because the vault exists) and
  `next_steps`. ponytail: languages come from root project files only; a polyglot monorepo with
  nested manifests gets fewer servers than it could use.
- T16: `model check` diagnostics gain `file` and `line` (absent when unknown): the binding or anchor
  path for `BINDING_*` and `ANCHOR_*` warnings, and the file and line of the evidence for
  `RELATIONSHIP_UNDRAWN` (the import, call reference or configured address) and
  `ENTRY_POINT_UNEXPOSED`. Semantic diagnostics carry none. `--format annotations` (model check
  only) prints one GitHub workflow command per diagnostic, `::warning` for a warning and `::notice`
  for information, with `file`, `line` and the code as `title`; data escapes `%`, CR and LF, and
  properties also `:` and `,`. Either format on another command refuses `REQUEST_INVALID`.
- T16: `--format pr-comment` (model footprint only) prints a Markdown comment whose first line is
  the marker `<!-- boundrail:footprint -->`: the touched modules, files outside every binding, the
  crossings with the relationships drawn for them or `undrawn`, the `crossings_skipped` reason with
  `scan_error`, and the model change counts per section, or why they were not compared. Module
  names, paths, relationship names and the base reference are code spans, fenced longer than any
  backtick run in them, so Markdown, HTML, mentions, issue references and links stay literal; `|`
  is escaped in table cells. Control characters, zero-width and direction marks and bidirectional
  embeddings, overrides and isolates (U+200B to U+200F, U+202A to U+202E, U+2066 to U+2069) become
  spaces, and a name is cut at 120 characters. Messages are escaped as Markdown with `@` as an
  entity. Each list shows at most 50 rows, and a comment over 60,000 bytes is cut at a line and
  ends saying so and pointing at the job summary. It renders from the command's JSON output
  (`src/arch/github.rs`), as the annotations do.
- T16: the action is one step that runs `ci/model-action.sh` (T18 removed its scan mode and the
  `mode` input). It keeps the token in a script variable, unsets `INPUT_TOKEN`, and gives it to `gh`
  as `GH_TOKEN` only while posting. It installs the release binary, or uses `binary`, runs `model
  check --format annotations`, replays only its `::warning` and `::notice` lines with workflow
  commands on (every other output runs under `::stop-commands::`), and adds the counts to the job
  summary, since GitHub shows at most 10 annotations of each kind. With a base (`base-ref`, else the
  pull request's base, refused when it starts with `-`) it unshallows a shallow checkout, writes
  `model footprint --base <base> --format pr-comment` to the job summary (plus the whole `--format
  markdown` footprint in a fenced block when the comment was cut), and on `pull_request` with a
  `token` replaces the comment that starts with the marker, taken from the comment's first line, and
  that the token's account wrote: the login `gh api user` names, else `github-actions[bot]` for
  `GITHUB_TOKEN`. Posting is best effort: when the token cannot write (a fork's pull request, a
  Dependabot run, a read-only token) the step leaves a notice and goes on. `fail-on` takes
  `warnings` (the default), `information` or `never`; an empty one means `never`. It never passes
  `--language-servers`: language servers run code from the checkout, and a pull request head is
  untrusted. `tests/action.rs` checks that, that its commands, flags and formats exist, and the
  best-effort post with stub `boundrail` and `gh` programs.
- T18: the binary has one command, `model`, and one global flag, `--tool-timeout`. The scan-era
  commands, the daemon, MCP, agent hooks and the edit gate, the reference collectors and their
  helpers, the action's scan mode and their docs are gone. `model check`, `inspect` and
  `footprint` read code from `scan::live`, which runs the native collector with the default
  excludes and no layout rules, then `lib::analyze`. The model reads only the document's files,
  edges, package entries, language and symbols; `live` leaves the provenance fields (`git`,
  `generated_at`, `scan_root`) and CODEOWNERS out, and no longer prints the scan-era warnings for
  unresolved collector references and CODEOWNERS problems; the collector's own limitations still
  go to stderr. Kept: `.archgraphignore`, the `archgraph-` temporary-file prefix and the
  `ARCHGRAPH_*` install variables.
- T18 follow-up (A7, not done): a lean scan. `lib::analyze` still runs the scan-era pipeline
  (layout rules, findings, recipes, targets, exposure, grab-bags, cycles and metrics), and the
  `Document` types keep modules such as these alive: `rules`, `findings`, `recipes`, `target`,
  `known`, `exposure`, `grabbag`, `quality`, `graph`, `resolution`, `simulate`, `codeowners`,
  `bisect`, `early_smells` and `diff`. A collector-to-model path that builds only files, edges,
  package entries and symbols could delete them. It could also drop the parameters every caller
  now passes the same value: `native::typescript::collect`'s `scan_all` (always true since T20),
  `native::python::collect`'s `package` (always none) and `python_paths` (always empty),
  `native::go::collect`'s `go` (always `go`), `grimp::package`'s `explicit`, `analyze`'s
  `cochange` (always none), the constant `Config` fields `live` fills (`granularity`, `includes`,
  `all_kinds`, `no_default_excludes`, `include_generated`, `explicit_excludes`, `rules`,
  `codeowners`), and `CollectionOptions::engine`, which has one variant. Also on the list:
  `rules::load`, which has no callers; `package_splits`, which is partly live through the infra
  workspaces and `simulate`, so only its scan-era parts go; and the validators that can raise
  `SCAN_FAILED`, which a lean path must keep or replace. Before switching, compare the lean path's
  output with the current one on the same checkouts.
- T20: a binding to an infrastructure unit names a deployment file, not code, so
  `CONTAINER_PARTS_OUTSIDE` never judges it and `Index::bound_ancestor` passes over it to the
  closest ancestor bound to code.
- T20: `scan::live` runs the T18 pipeline once per language `collect::detect_languages` finds,
  most files first, and merges files, edges, package entries and symbols (`complete` only when
  every language's is); each file keeps its language in `File::language`, which the entry-point
  test reads, and the document's `source.language` is the first. Detection counts TypeScript
  anywhere, since the collector now runs with `scan_all`, and Python under `source_dir` only: its
  collector resolves one top-level package (`grimp::package`) from one import root, so lifting
  that needs a scan per package root. Intended change: a TypeScript checkout with `src/` now
  scans outside it too (source root `.`), so files beside `src/` are judged rather than
  `not-scanned`. Since TypeScript counts anywhere, the TypeScript policy, which detection also
  walks with, leaves out the build output of each language whose manifest sits at the root:
  `target/` with a `Cargo.toml`, `target/` and `out/` with a `pom.xml` or Gradle build, `_build/`
  and `deps/` with a `mix.exs`. Otherwise `cargo doc`'s JavaScript under `target/doc` would make
  a TypeScript scan. Without that manifest the folders stay, so TypeScript in `src/out` counts.
  Rust needs a `Cargo.toml` and Go a `go.mod` or `go.work` anywhere in the checkout, as their
  collectors find nested manifests. Each language is collected under its own policy, built once.
  A language whose collector fails while another succeeds warns on stderr and lands in
  `Document::scan_failures` with its detected files that its own policy keeps (so Rust test
  files stay `not-scanned`); a binding or anchor whose path holds none of the scanned files but
  some of those is `scan-failed`, and one `SCAN_FAILED` names those subjects with each language's
  error (`go: ...; python: ...`). A path holding scanned files beside such files is judged on
  the scanned ones and named by that `SCAN_FAILED` too, so the check fails rather than passing
  on part of the code. Only when every language fails does the whole scan fail. `inspect` sees
  the merged scan, with each language's `complete` and limitations naming none of its files kept
  in `Document::symbol_languages` (empty for one language), and sets `scan_error` to the failed
  languages' errors when a module it reports is one `SCAN_FAILED` would name; `footprint` skips
  its crossings as `scan-failed`, with each failed language's error in `scan_error`, when a
  changed file is one of a failed language's.
- T20: `model check --every-draft` (not with `--proposal`) runs `run_check` on the current
  state, then on every proposal whose `proposal.json` reads as a draft or does not read, and
  prints `{"kind": "check-every-draft", "passed", "states"}` with each state's check output, or
  its refusal output with `proposal.id`; Markdown prints each state in turn. Exit 1 when any
  state has a warning or refuses. `--format annotations` and `--only embeds` refuse
  `REQUEST_INVALID`. The states share one scan of the checkout, made the first time a state needs
  it; a draft's error other than a refusal stops the command with `checking draft <id>`.
- T21: routing replaces the T9 lanes and its ponytail about edges crossing boxes. Ports stay
  where T9 put them (facing sides, spread by the other end). Each route leaves its port straight
  for 10 units, then runs A* over a grid of lines: every box's outline kept 10 away, each container
  title, the picture title, the route ends, two rings around everything, and filler lines at most
  12 apart. A route may not enter any box except the containers around its ends, nor their titles
  or the picture title; it pays 40 per bend, 30 per crossing and 4 times the length again for
  each route already on a segment, and shorter edges route first. Without a path it retries
  ignoring titles, then falls back to the T9 bend. The searches share one scratch grid, refilled
  per edge, and skip heap entries a cheaper path has overtaken. Container headers grew from 44 to
  56 so a route can pass between a title and the parts below it. Labels go on the route, middle
  segment first, in steps of 8: first where they meet no other label, box, container title band
  or border, fixed area or other route; then ignoring routes; then just beside a segment; then on
  rings around the route; with nothing free, at the middle of the middle segment. A label may sit
  inside a container below its header but never across its border. Every route and arrowhead is
  drawn first and every label after, each label in its own `relationship label` group with the
  arrow's `data-id` and an opaque plate, so a label that had to sit on another route covers it
  rather than being crossed. Same model, same bytes. `assert_clean_layout` in
  `tests/arch_model.rs` parses a render and asserts no segment enters a box but its ends and their
  containers, every label group comes after every route, no two labels meet, and no label meets a
  box, a box's text or a container border; it fails on the T9 layout of the eleven-module fixture.
  The new layout changes every architecture picture's bytes, so pictures drawn before T21 report
  stale until `model pictures --write`. Known ceiling (ponytail): the grid has one point per
  crossing of its lines, and every edge searches it anew, so cost grows with the number of lines
  squared times the number of edges. Fine for tens of boxes; for pictures of hundreds, replace
  the grid with a sparse visibility graph over box corners.
- T21: `model render` flags. `--focus <subject>` (repeatable) lights the subject, its parts and the
  other end of every relationship from or to them, through the beat focus, and dims the rest.
  `--hide-kind <key>` (repeatable) leaves out modules of a configured node kind; their parts move
  to the nearest drawn ancestor; an unknown kind, or a `--scope` of a hidden kind, refuses
  `REQUEST_INVALID`. `--legend` draws a row of node kinds (fill, dashed when external) and a row
  of relationship kinds (dash) under the picture, in configured names, only those drawn.
  `--hide-kind` and `--legend` with a flow view refuse `REQUEST_INVALID`. `--theme light` (the
  default) writes nothing new; `dark` adds a `<style>` that recolors by attribute selectors on the
  light colors, built from the same constants the drawing writes, and drops box fills to 0.3
  opacity so configured colors stay readable; `auto` wraps it in `@media (prefers-color-scheme:
  dark)`. In a PNG the dark and auto themes depend on the rasterizer's CSS support. All four
  refuse with `--walkthrough` and `--picture`, whose readings say what to draw.
- T21: `--format json` adds `revision` (the drawn state's digest), `width` and `height` (the
  viewBox), `modules` (boxes drawn, or a flow's participants), `relationships` (every relationship
  an arrow stands for, empty for flows), `digest` (SHA-256 of the SVG, computed only for the
  receipt) and `fallbacks: {routes, labels}`, the arrows that took the T9 bend and the labels set at
  the middle of the middle segment (zero for flows); when either is non-zero a note goes to stderr,
  and the SVG is unchanged. `--png <file>` checks its folder as `--output` does, refuses the same
  file as `--output`, rasterizes with the T15 discovery, writes the PNG with its source chunk after
  the output file (before printing, when there is none), and prints nothing unless `--output` or
  `--format json` asks (the receipt adds `png`). `--scale` (default 1, at most 8) and `--region
  x,y,w,h` (exactly four finite numbers, picture units from the top-left corner, inside the picture)
  rewrite the root's size and viewBox before rasterizing, so every rasterizer honors them. A missing
  or failing rasterizer refuses `REQUEST_INVALID` with `details.reason` `no-rasterizer` or
  `rasterizer-failed`.
- T35: `model record <walkthrough> --output <file> [--voice <name>] [--rate <wpm>] [--width N
  --height N] [--fps N]` draws each beat's reading through the beat path, rasterizes it with the T15
  discovery, scaled to fit the frame (default 1920 by 1080, even sides from 16 to 7680, 30 fps, at
  most 60), speaks the narration with the first of `SPEAKERS` in `src/arch/record.rs`: `say` (`-o
  out.aiff -f in.txt [-v voice] [-r rate]`, AIFF), `espeak-ng` (`-w out.wav -f in.txt [-v voice] [-s
  rate]`, WAV) and `piper` (`--model <voice> --output_file out.wav [--length_scale 175/rate]`, the
  narration on stdin; it needs `--voice` naming its model file, made absolute against the current
  folder before piper runs in its temporary folder). The narration goes in a file or on stdin, never
  as an argument; `--voice` may not be empty or start with `-`, and `--rate` is 80 to 450. A beat
  lasts its narration plus 0.5 s (`PAD`), the narration's length read from the WAV or AIFF header
  (AIFF-C too, its 80-bit sample rate decoded by hand). One `ffmpeg` run encodes every beat: per
  beat `-protocol_whitelist file -f image2 -pattern_type none -loop 1 -framerate F -t S -i
  beat-NNN.png -protocol_whitelist file -f aiff|wav -i beat-NNN.<audio>`, each input's format forced
  and only the `file` protocol allowed, a filter that fits each image into the frame on white
  (`scale` with `force_original_aspect_ratio=decrease`, `pad`), resamples the audio to 44.1 kHz mono
  and pads and trims it to S, then `concat`, written as `-f mp4 out.mp4` with `yuv420p` and AAC; the
  video codec is ffmpeg's default for MP4. Speakers and ffmpeg are found like the rasterizers (bare
  name, absolute `PATH` folder outside the checkout), run with that `PATH`, without a shell, in a
  private temporary folder (mode 0700) holding their inputs, under the tool timeout. No title card;
  no network.
- T35: order. Refusals come before any file is written: the output's folder (`render --output`
  rules: it must exist, outside the vault and the Git folder), then ffmpeg; for a recording, then an
  output ending in `.talk`, a talk folder that is not one Boundrail wrote, the flags, the
  walkthrough (`SUBJECT_NOT_FOUND`; one without beats `REQUEST_INVALID`), the rasterizer and the
  speaker. An `--output` that is a symbolic link or a folder refuses with the output checks. A
  missing tool refuses `REQUEST_INVALID` with `details.reason` `no-ffmpeg`, `no-rasterizer` or
  `no-text-to-speech` (naming the programs tried). Every beat is drawn before any is rasterized and
  every PNG made before any narration is spoken. A failing tool refuses with `rasterizer-failed`,
  `text-to-speech-failed` or `ffmpeg-failed`; a recorded beat whose length `talk_problem` refuses
  (an AIFF rate near 0 makes it infinite) is `text-to-speech-failed`. The rasterizer is resolved
  once per recording. Only after ffmpeg succeeds are the talk folder and the MP4 written.
- T35: the talk folder is the output's path with the extension `.talk` (`out/tour.mp4` →
  `out/tour.talk/`): `beat-NNN.png`, `beat-NNN.aiff` or `.wav`, and `talk.json` `{schema_version: 1,
  walkthrough, name, revision, width, height, fps, speaker, voice?, rate?, beats: [{id, title,
  narration, image, audio, narration_seconds, seconds}]}`. It is written whole into a new folder
  beside it, which then takes the old one's place, so a beat left from a longer recording goes. An
  existing path there that is not a folder holding a `talk.json` that parses as a talk, a symbolic
  link included, refuses, checked again just before the old folder is replaced. The MP4 is replaced
  through a temporary file and a rename. `--replay <talk folder> --output <file>` reads `talk.json`
  (a regular file, not a link, pipe or device, at most 1 MiB), takes `--width`, `--height` and
  `--fps` in place of its own, copies each beat's files (each a regular file of at most 64 MiB whose
  bytes are a PNG, or a WAV or AIFF header Boundrail reads) into a private temporary folder and
  encodes them again; it needs only ffmpeg, renders and speaks nothing, writes only the MP4 and
  refuses `REQUEST_INVALID` a manifest that does not read, has another schema version, no beats, a
  beat of 0 or over 3600 seconds, or a file name that is not plain (letters, digits, `.`, `_`, `-`,
  starting with a letter or digit, so ffmpeg reads no folder, protocol, option or image-sequence
  pattern), an image not ending in `.png` or an audio not ending in `.wav` or `.aiff` (a `.m3u8`
  would let ffmpeg's HLS demuxer read absolute paths outside the folder). `--voice` and `--rate`
  refuse beside `--replay`.
- T35: tests. A fake `say` logs its arguments and the narration file and writes a one-second AIFF; a
  fake `ffmpeg` logs its arguments, fails unless every `-i` file exists in its folder, and writes
  its last argument; the fake `resvg` stands in for the rasterizer. Missing tools are tested with a
  `PATH` of links to the chosen fakes, Git and `cat`. A real run (macOS `say`, ffmpeg 9, a fake
  rasterizer) made a 1280 by 720 H.264 and AAC MP4 of the summed length, and `--replay` the same;
  with real ffmpeg a `.png` or `.aiff` holding an HLS playlist of an outside file is refused before
  ffmpeg runs (forced `image2` alone stops the outside read, but `-loop 1` then spins on the
  undecodable file until the tool timeout). Known ceiling (ponytail): one ffmpeg run takes every
  beat as two inputs, so a 200-beat walkthrough opens 400 inputs; encode in batches joined with the
  concat demuxer if that strains ffmpeg.
- Open after T21 and the view redesign:
  - The view's CSS keys on the renderer's hex color literals and baked opacity values. The
    renderer should emit classes or CSS variables for the note, the kind line, containers,
    emphasis and dimmed groups, so the view styles roles rather than colors.
  - `MAX_SCALE` caps the `--scale` factor, not the PNG's pixels, so a large picture at scale 8
    can still produce a huge PNG. It should cap the output's pixel count.
  - HOLD: the pane outline's painting order. Check it in Chrome, Firefox and Safari before
    calling the redesign done.
- T22: `model inspect` adds `model_interfaces` beside the scan's `modules`: each module's
  interface as the model draws it, from relationships alone, every relationship kind counted and
  flow steps not, one per module sorted by qualified ID, or the subject's alone. Inside a module
  means the module and every module below it; outside means neither inside nor a module holding
  it. A relationship with one end inside reaches the direct part holding that end, or the module
  itself when the end is the module. A relationship between the module, or a module inside it,
  and a container above the module joins a container and its own part, which
  `CONTAINER_LINE_TO_OWN_PART` flags, and counts in no list or measure. `entry_points`
  are the direct parts an outside relationship reaches, each with those relationships; a module
  without parts is its own entry point when one reaches it. `hidden_parts` are the direct parts
  none reaches. `callers` are the outside modules with relationships into the module, `outgoing`
  the outside modules its relationships go to, each with those relationships; a relationship
  from or to a part of an outside container names that part. `pass_through` are entry points
  without parts of their own that send a relationship outside and whose every relationship has
  its other end outside the module, so no sibling, no part and not the container: such a part
  only forwards from outside to outside. It is not the `CONTAINER_PASS_THROUGH` diagnostic, which
  flags a container with exactly one part; the list names part-less entry parts and judges
  relationships, the diagnostic names containers and counts parts.
  `on_container` are the relationships between the module itself and the outside while it has
  parts, the ones `RELATIONSHIP_ON_CONTAINER` flags; they count among `callers` and `outgoing`
  but reach no entry point. `measures` counts entry points, hidden parts, callers and outgoing
  modules, and `depth`, the levels of containment below the module (0 without parts, 1 when no
  part has parts), chosen over a ratio because the four counts already give the hidden share
  (`hidden_parts / (entry_points + hidden_parts)` for a container) while depth adds what they do
  not show. Markdown prints the drawn interface only when a subject is given, since one
  paragraph per module would bury the scan; JSON carries every module's.
- T22: `model inspect --checkpoint <id>` (not with `--proposal` or `--language-servers`) loads
  the checkpoint as `read --checkpoint` does and reads its interfaces from the model alone. The
  scan is skipped, since the checkout's code is not that state's code: `modules`, `skips` and
  `language_servers` are empty and `code_skipped` says why; Markdown prints the checkpoint line
  and `Code not scanned: ...`. `model inspect --group <key>` (not with a subject or
  `--language-servers`) reads one group of `config.yaml` by its key from the model alone, in the
  current state, a proposal or a checkpoint, and prints `group` instead of the interfaces:
  `members` (modules naming the group), `internal` (relationships between two different
  members), `boundary` (relationships with exactly one end inside, each with `direction` `in` or
  `out`, the `member` holding the inside end and the `neighbour` at the outside end) and
  `neighbours` (those outside ends). A relationship reaching a member's part reaches that
  member, the innermost one when members nest, so one within a single member is neither internal
  nor boundary. A key the config lacks refuses `SUBJECT_NOT_FOUND`. `/api/inspect` serializes
  the same output, so it carries `model_interfaces`; it takes no checkpoint or group yet, and
  the page still shows only the scanned interface.
- T23: `model compare` adds `interface_changes` after the subject sections, whose keys and order
  hold: one entry per module present in both states whose drawn interface differs, read with
  T22's `drawn()` on each state and sorted by qualified ID. Each names the module and its name
  after the change, then `entry_points_added`/`_removed`, `callers_gained`/`_lost`,
  `outgoing_gained`/`_lost`, `hidden_parts_added`/`_removed` and `pass_through_added`/`_removed`
  as qualified module IDs, every list present even when empty, and `interface_measures` with
  `before` and `after`. Lists compare modules, not the relationships behind them, so a module
  reaching the same neighbours through other relationships shows no change. A module whose lists
  hold but whose measures differ (a deeper part added) still shows. Added and removed modules
  have no entry; their effect shows on the modules around them.
- T23: the checkpoint base is `--against-checkpoint <id>`, not a prefix inside `--against`: every
  other command names a checkpoint with its own flag taking an ID or unique commit prefix
  (`read`, `inspect` and `render --checkpoint`), and a separate flag keeps `--against` an ID or
  `current`. It conflicts with an explicit `--against` and loads as `read --checkpoint` does;
  `against` is then `{checkpoint, revision}`, the checkpoint as `read --checkpoint` prints it, and
  Markdown names it `checkpoint <id>`. An unknown one refuses `CHECKPOINT_NOT_FOUND`.
- T23: `--beside <proposal>`, repeatable, compares more proposals against the same base. The
  top-level sections and `interface_changes` stay those of the positional proposal; after them
  `alternatives` lists the positional proposal, then each `--beside` in order, each with
  `proposal` (as the top-level key names it), `subjects` (added, removed and changed counts over
  all five sections) and `interface_changes` (how many modules' interfaces changed).
  `interface_measures` has one row per module any state draws, sorted by qualified ID: `module`,
  `name` (from the first state holding it, the base first), `against` and `alternatives`, the
  measures in the base and in each alternative in `alternatives` order, `null` where the state
  lacks the module. JSON carries every module; Markdown prints a counts table and a measures
  table of the modules whose counts are not the same in every state, each cell
  `entry points/hidden parts/callers/outgoing/depth` or `-`. No ranking is computed: which counts
  matter depends on the design question. An unknown `--beside` refuses as an unknown proposal
  does. `/api/compare` passes neither flag.
- T24: every new field is optional and left out of a file when absent, so a vault written before
  T24 loads, reads and digests as it did, and an edit rewrites only the files it changes.
  Relationship `traffic` is `{protocol?, sync?, rate?, payload?}`: `sync` is `sync` or `async`,
  the others one line of 1 to 60 characters, and at least one field is given (`MODEL_INVALID`
  otherwise). `edit-relationship` sets it whole and clears it as `traffic`; `model compare` reports
  it as one field. The architecture view appends `(protocol, sync, rate, payload)`, the fields
  given in that order, to the label of an arrow standing for one relationship; a merged
  `Kind xN` arrow shows none. Labels are placed on the full text, so the placement rules hold.
- T24: a message step's `anchor` reuses the relationship anchor type, `{repo?, path, symbol}` with
  `symbol` required, so both are validated and judged by the same code: the code check reports
  `ANCHOR_PATH_MISSING` and `ANCHOR_SYMBOL_MISSING` and the `anchor` skips with the step's
  qualified ID as subject, worded `From -> To is anchored at` as for a relationship. A step anchor
  on an existing path makes the check scan, as a relationship anchor does.
- T24: a flow's `participants` are qualified module IDs, each once. Writes resolve them like
  module subjects (`SUBJECT_NOT_FOUND` for an unknown one); `edit-flow` sets the list whole and
  clears it as `participants`. Removing a listed module refuses `REFERENCED_BY_FLOW` with the
  flow in `details.flows` beside `details.steps`, unless the batch edits that flow, which
  validation then judges. The sequence view draws the listed modules first, in order, a listed
  module no step uses included, then the other message ends in first-use order; the render
  receipt's modules follow the same order. The flowchart ignores the list.
- T24: `add-flow` without a summary, or with a blank one, refuses `FLOW_SUMMARY_REQUIRED`, as do
  `edit-flow` clearing it and setting it blank. The check runs after the collection or flow resolves, so a missing subject refuses `SUBJECT_NOT_FOUND` first. Other
  writes to a flow without a summary still apply, and the semantic check keeps reporting it as
  `FLOW_SUMMARY_MISSING`.
- T25: every journaled write (`edit` of the current state or a proposal, `propose`, `adopt`,
  `shelve`, `reconcile`, `import`) retains one checkpoint through the retain machinery, carrying
  the write's `--doing`, writer and operation ID, and its receipt gains `checkpoint`. The
  checkpoint is part of the journaled operation: the record stays `pending` until it lands.
  Before the record is written, `checkpoint::draft_write` builds the checkpoint's tree from the
  planned files (the write's contents and the files it leaves, read once from the vault and
  checked against their planned digests) without touching the vault, and the checkpoint commit is
  made, signed when `commit.gpgSign` is set, on the history tip. So a Git identity `git var`
  cannot find, a file larger than the 4 MiB a checkpoint reader accepts (`MODEL_INVALID`) or a
  signer that fails (`SIGNING_FAILED`, as with a locked key) refuses with nothing written and no
  record. After the vault writes reach their planned digests, `checkpoint::retain_write` looks
  for a checkpoint of the same operation, writer, state, revision and `--doing` on the history
  and returns it, else moves the ref to the commit made before the write by compare-and-swap; on
  a tip that moved it commits, and signs, again, up to three times; then the record is `done`.
  So a replay returns the stored receipt, its checkpoint as the history holds it now (a fetch
  may have replayed it onto another commit) with statuses read now, and retains nothing; a write
  interrupted after its vault writes, before or after the ref moved, is finished by `model
  recover` (or a retry of the operation) with exactly one checkpoint, drafted again from the
  record. A checkpoint that fails after the vault was written (a signing failure when the tip
  moved, a ref that kept moving) refuses `RECOVERY_REQUIRED` with the cause in `details.cause`
  (`code`, `message`, `details`) and leaves the record pending. Every write now needs a Git
  identity; test fixtures set one and `commit.gpgsign=false`.
- T25: a proposal write or reconcile retains the proposal: the tree holds `model.json`,
  `config.yaml` and `proposals/<id>/` (its `proposal.json`, modules, collections and base), the
  files its load reads, and a `Proposal: <id>` trailer names it. `Revision` is the proposal's
  revision, the one `read --proposal` shows. An adoption retains the current state it makes.
  `read --checkpoint` (and every reader of `checkpoint::load`) loads a proposal checkpoint as that
  proposal; its tree may hold nothing else (`MODEL_INVALID`), and must hold `proposal.json`.
  `export --checkpoint` of one writes a proposal bundle (`source.kind: "proposal"`, the
  checkpoint's revision, proposal-relative files, and `checkpoint` and `commit` naming where it
  came from), so `import` restores it as a draft.
- T25: explicit `model retain` stays, for retaining the current state with a reason of its own
  (a release, a review). Since every write retains, it refuses `NOTHING_TO_RETAIN` only when the
  latest current-state checkpoint (proposal checkpoints are skipped) holds the same revision with
  the same `--doing`. Its cross-worktree replay refuses `OPERATION_REUSED` for a proposal
  checkpoint with that operation ID.
- T25: `Uncommitted: true` follows `Source` when `git status --porcelain` lists any change,
  tracked or untracked, outside `.boundrail` (`:/` and `:(top,exclude).boundrail`), so model
  files never count. Readings add `uncommitted` and `source_status`: `unavailable` when there is
  no `Source` or this repository lacks that commit (one `git rev-list --no-walk
  --ignore-missing` over the full-hash sources of a listing), `uncommitted` when it holds the
  commit and the trailer is set, `available` otherwise. Checkpoints retained before T25 have no
  such trailer and read `available` when their commit is here.
- T25: the publication destination is `boundrail.historyRemote` in the repository's Git config,
  which every worktree shares, written by `model history configure --remote <name>` (a name `git
  remote` lists whose refs below are well-formed, `REQUEST_INVALID` otherwise; JSON `{kind:
  "history-configure", remote, before}` with the destination it replaced). A configured name
  that is no longer such a remote reads as `origin` with a warning on stderr. It is Git config,
  not `config.yaml`: that file is part of every revision, no journaled write may change it, and
  remote names belong to a clone. `history push` and `fetch` default to it, else `origin`. The
  remote name is one ref component below `refs/boundrail/pushing/` and
  `refs/boundrail/published/`, with `%` and `/` percent-encoded so remotes `a` and `a/b` never
  clash. A push writes the pushing ref with the object it sends before `git push` runs and, once
  the push succeeds, sets the published ref to it and deletes the pushing ref; a push that fails
  deletes the pushing ref once the remote can be asked what it holds (recording it as published
  when it holds the object), and leaves it pending only when it cannot; a push that finds the
  remote already at the local object records it as published too. A fetch that succeeds sets
  the published ref to the fetched object, and a remote without the history ref deletes it.
  `model checkpoints` (and checkpoint readings) judge publication against the destination from
  refs only: `published` when the published ref reaches the commit, `pending` when the pushing
  ref reaches it and the published ref does not, `saved-locally` otherwise; `model checkpoints`
  prints the `destination` (`remote`, `configured`). Replayed commits are new commits, so they
  read `saved-locally` until pushed. New trailers are optional, so older histories read and
  publish as before; a `Proposal` that is not a local ID or an `Uncommitted` other than `true`
  after a `Source` is not a checkpoint, which fetch and push refuse as for other forged trailers.
- T32: commands. `model language-server list` prints both files (`repository` and `user`, each
  `{file, content, problem?}`), and `effective`: each server a run with servers on would start,
  once per language of the files it covers (`language`, `server`, `command`, `args`,
  `extensions`, `defined_by: allowlist | user`), or a top-level `problem` when the list names an
  unknown server. A language is a key of the allowlist: `c` (C, C++, Objective-C), `csharp`, `go`,
  `java`, `python`, `rust`, `swift`, `typescript`, each with its allowlisted servers, the first
  its default (`pyright-langserver` for Python, as setup writes). `add <language>` writes the
  repository's list: the default's name, or `--command` when it is another allowlisted server of
  that language; any other command, or an `--arg`, refuses `REQUEST_INVALID` pointing to
  `--user`. `add <language> --user` defines the default's name in the user's config with
  `--command` and its `--arg`s, else with the allowlisted program and arguments, so a list naming
  that server runs the user's program; a `--command` naming an allowlisted server of the
  language, without `--arg`, keeps that server's arguments (TypeScript's `--stdio`). The command
  is checked as in T13. The user's config is written as a bare array of servers unless it trusts
  a folder, so binaries older than trusted folders still read it. Both replace the
  entries naming an allowlisted server of the language; `remove <language> [--user]` drops them
  and deletes the repository's list when it is left empty. Entries naming a server only the user
  defines are edited by hand. A file that does not read refuses, unchanged files are not
  rewritten, and writes go through `files::write_atomic` (the repository's path through
  `files::safe_path`). The output adds `edited: {file, change: written | deleted | unchanged}`.
- T32: defaults. The allowlist adds `clangd` (`c h cc cpp cxx hh hpp hxx m mm`, languages `c`,
  `cpp`, `objective-c`, `objective-cpp`), `csharp-ls` (`cs`; picked over OmniSharp, since `dotnet tool install --global
  csharp-ls` puts a program of that name on `PATH` that speaks LSP with no arguments) and `sourcekit-lsp` (`swift`), each run
  as its own name with no arguments under the T13 rules. clangd reads a committed `.clangd` and
  `compile_commands.json` and passes their flags to its clang frontend, and its background index
  writes `.cache/clangd` in the checkout; sourcekit-lsp 6 and later background-indexes, building
  the package into `.build/` and running its macros and build-tool plugins; csharp-ls runs MSBuild
  design-time targets and NuGet restore, which reaches the network, runs the `.targets` of
  packages and writes `obj/`. All of it is code from the checkout. Each allowlisted server holds
  its `project_files`, and `required` when it starts only with one of them (below); `model setup
  --language-servers` names the default server of each language one of whose project files sits
  at the root (`lsp::detected_servers`), so setup names no server that would be unready there.
- T32: on by default in trusted folders. The user's config may be `{"trusted": [<absolute
  folder>...], "servers": [...]}`, unknown keys refused; a bare array still reads as the servers,
  trusting nothing. A global switch would have turned servers on for every checkout, freshly
  cloned untrusted ones included, so trust is per folder. `--language-servers` turns servers on,
  `--no-language-servers` off, and without either they run when the checkout root lies at or
  below a trusted folder, both once links resolve (`lsp::Enable`: `On`, `Off`, `ByUserConfig`). A trusted folder that is not absolute makes the config not read. The user's
  config is read only when the repository's list exists, its server commands are checked only
  once servers are on, and a user's config at or below the checkout root (after links resolve)
  does not read, as the repository could commit it. A user's config that does not read refuses
  `REQUEST_INVALID` with `--language-servers`; without it, it prints `warning: language servers
  stay off: <reason>` and the run keeps the `language-servers-not-enabled` skip, as when servers
  are off. The committed list is never read without servers on, so it cannot turn them on. `model
  view` passes `Off`. The GitHub Action passes `--no-language-servers`; any other machine that
  checks untrusted pull requests must not list their folder as trusted.
- T32: unready and timings. Before an allowlisted server starts (by name, so a user's definition
  of the name too), one of its project files must sit in the folder of a file it would open or a
  folder above it, up to the checkout root: `compile_commands.json`, `build/compile_commands.json`
  or `compile_flags.txt` for clangd; `Cargo.toml` or `rust-project.json`; `go.mod` or `go.work`;
  `pom.xml`, `build.gradle` or `build.gradle.kts`; a `*.sln`, `*.slnx` or `*.csproj`;
  `Package.swift` for sourcekit-lsp. The TypeScript and Pyright servers need none (their project
  files, `package.json` or `tsconfig.json` and `pyproject.toml`, `setup.py`, `setup.cfg` or
  `requirements.txt`, only drive setup). Each folder holding a file a server would open, and each
  folder above it, is looked in once per project file name for the whole run. Otherwise it is not started: `warning: language server <name> is not ready: no <files>
  beside or above <first file> in the checkout` on stderr and the skip `{subject: <name>, site:
  "language-server", reason: "language-server-unready"}`. `language_server_runs`, beside the skips
  in `model check` JSON and beside `language_servers` in `model inspect` JSON (kept as the skip
  list, so a new key rather than a changed one), lists each server that had something to answer:
  `{server, outcome: answered | failed | unready, reason?, timings_ms?: {start, initialize,
  queries, shutdown}}`, with the milliseconds of the steps it reached; it is left out when empty.
  An answered server also prints `boundrail: language server <name> answered: start N ms,
  initialize N ms, queries N ms, shutdown N ms` to stderr in every format, as the starting line
  is.
- T28: a reading (beat, picture or saved view) gains four optional fields, each left out of the
  file when absent, so a vault written before T28 loads and digests as it did and a reading that
  uses none of them draws the same bytes. `selection` lists qualified modules, each once, outlined
  in `#1c7ed6` at 2.6 with the group class `module selected`; it dims nothing, and on a compared
  drawing its outline replaces the mark's while the class and label keep the mark. `filters` is
  `{relationships?, groups?}`: the relationship kinds drawn (the others leave the drawing before
  layout and count for no hop) and the groups whose members alone are drawn as usual, with the
  other end of each of their relationships, or a member's part's, kept and dimmed and everything
  else left out. `groups` names the group regions, `config.yaml` keys or `"all"`.
  `visibility` (0 to 9) keeps the focus, its parts and what lies within that many relationship
  hops of them, counted both ways; the rest is left out, not dimmed. Every kept module keeps the
  containers around it. A group filter and visibility together keep what both keep. A group
  filter without a focus lights the members as a focus would; with one, the focus decides what
  dims. The task row asked for `filters.relationships` to name the kinds shown, so it does, and
  an empty list or `{}` is a problem rather than "show nothing".
- T28: validation, beside the T14 rules: the four fields apply to the architecture view only;
  `filters` names a kind or group, `groups` lists one; a kind or group listed twice, or one
  `config.yaml` lacks, is a problem; `visibility` needs a focus; `selection` references follow
  `focus` (existing modules in the current state, qualified IDs elsewhere), and
  `WALKTHROUGH_TARGET_MISSING`, `PICTURE_TARGET_MISSING` and `VIEW_TARGET_MISSING` judge it too.
  Keys are checked against the vault's config, so removing a group a reading names makes the
  vault invalid until the reading is edited, as a module naming it already did. A drawn state
  whose own config lacks a key (an old checkpoint) refuses `REQUEST_INVALID` on render. Edits set
  `selection`, `filters`, `groups` and `visibility` whole and clear them by name, deduplicating
  in order; `edit-beat`, `edit-picture` and `edit-view` take them alike.
- T28: group regions. After layout and before routing, each group named, in the order given
  (`"all"` in key order), with a drawn box naming it, gets a band: its members' boxes' bounds grown
  by 12, joined with its name's plate grown by 4, `rx` 10, filled and stroked with the group's
  color (else `#9aa5b1`) at fill opacity 0.1, drawn in `<g class="region">` (no `data-id`, so the
  browser never takes it for a module) after the title and before every box. The name, bold
  `SOFT_INK`, sits 12 above the members' top left corner, or on the first spot of the label rings
  that keeps 12 off every box (inside a container only below its header) and meets no fixed area
  (`layout::free_spot`), 12 being a route's straight start and a gap. Each placed name joins the
  scene's fixed areas, so later names, routes and labels keep off it. A band may cover boxes
  that are not members; it is behind them. `assert_regions_clear` in `tests/arch_model.rs`
  checks names against boxes, titles, labels, routes and each other on the platform fixture, and
  `assert_clean_layout` still holds. Known ceiling (ponytail): a route that finds no path while
  fixed areas block it retries ignoring them, as for the picture title, and may then cross a
  region name; the fixture does not.
- T28: `model render --group <key>` (repeatable) is the group filter on the command line; it
  refuses beside `--walkthrough`, `--picture` and `--saved-view`, with a flow view, and for a key
  `config.yaml` lacks (`REQUEST_INVALID`, as `--hide-kind`). No flag sets selection, regions or
  visibility: a saved view does without a file.
- T28: a saved view is `views/<id>.view.json` (schema `model-view`): `{schema_version, id, name,
  order, description?, reading}`, a picture without `output`. Views belong to the current state
  as walkthroughs and pictures do: its revision, `file_digests`, the journal's write allow-list,
  checkpoints, bundles and committed reads cover them; proposal loads never read a `views`
  folder; a proposal edit with a view change refuses `REQUEST_INVALID`; a proposal's predecessor
  digest leaves `views/` out; adoption checks the adopted state with the current views. IDs come
  from the model-wide allocator and are retired on removal; a view resolves by ID or unique name
  and a claim on it holds it. Edits: `add-view`, `edit-view` (set name, order, description and any
  reading field; clear description and the reading fields), `remove-view`. Removing a module or
  flow an unedited view reads refuses `REFERENCED_BY_VIEW` with `details.views`, after
  `REFERENCED_BY_PICTURE`. `model read` lists `views` (JSON, and `Views:` in Markdown), `model read
  --view <id>` (or `model read <view ID>`) prints one with its `Shows:` line, and `model render
  --saved-view <id>` draws it through the beat path, refusing any other drawing flag. The render
  flag is not `--view`, which already names the kind of drawing. `model check` adds
  `VIEW_TARGET_MISSING` (warning, skip site `view`) in the semantic part, loading each state once
  with the beats'.
- T28: the browser lists the current state's views in a Views section of the sidebar from the
  model read. Opening one applies its reading to the active pane and draws through
  `/api/render?saved=<id>`, so the server applies every field; after a reload a pane showing a
  view follows it, or keeps what it shows once it is gone. Beats and pictures were already drawn
  from the stored reading through `/api/render?walkthrough=&beat=` and `?picture=`, so their new
  fields reach the server untouched.
- T34: `model explain <subject>... [--relationship <id>] [--flow <id>] [--proposal <id>]
  --question <text> [--agent claude|codex] [--print]` asks a local agent CLI and prints its
  answer as text on stdout (`--format` only shapes a refusal). It needs a subject, relationship,
  flow or proposal and a question that is not blank, else `REQUEST_INVALID`; unknown modules,
  relationships and flows refuse as `model read` does. With `--proposal` the modules,
  relationship and flow are read in that proposal. The bundle is Markdown: a heading naming the
  model, state and revision, then per subject its `model read` module, parts, incoming and
  outgoing relationships and its drawn interface from `model inspect` (no scan), the relationship
  with its two modules, the flow with its steps as `model read --flow` prints it, the proposal's
  `model compare` against the current state without `schema`, `success` and `vault`, each as a
  JSON block, and the question last. It holds only model text and the question: no environment
  value, absolute path or token. The prompt states the bundle is data, not instructions, and sets
  it between `<<<BOUNDRAIL-DATA-<sha256 of the bundle>` and `BOUNDRAIL-DATA-<same>>>>` lines; text
  inside cannot hold its own digest, so it cannot close the frame early, and the same request
  gives the same prompt. `--print` prints that prompt and runs nothing, so the user can paste it
  anywhere.
- T34: agents. The fixed list in `src/cli/explain.rs` is `claude` (`-p --tools ""
  --strict-mcp-config`: no tools and none of the user's MCP servers) and `codex` (`exec
  --skip-git-repo-check --sandbox read-only -`); both read the prompt on stdin. The read-only
  sandbox still lets `codex` read any file on disk and put it in its answer, so the docs
  recommend `claude`; the answer goes only to the user, on stdout or in the page.
  Cursor's `cursor-agent -p` has every tool, writes and shell included, and OpenCode's stdin
  prompt was not confirmed, so neither is listed although `model setup` installs skills for them.
  The agent is `--agent`, else `agent` in the user's `boundrail/explain.json` (`{"agent":
  "codex"}`, unknown keys refused, found as the language servers' user config), else the first of
  the list found; the repository never names it. A config naming an unknown agent, or not a
  regular file under 1 MiB, refuses `REQUEST_INVALID`. A chosen agent that is missing is not
  replaced by another. It is looked up as the rasterizers are (`collect::resolve_program`: its own
  name in an absolute `PATH` folder, outside the checkout once links are resolved), runs without a
  shell with `PATH` cut to the absolute folders outside the checkout, in a private temporary
  folder, so it loads no `AGENTS.md` or `CLAUDE.md` from the checkout, and under the tool timeout.
  It keeps the rest of the user's environment, which it needs to reach its provider. `boundrail:
  asking agent <name>: <program> <args>` goes to stderr first. None found refuses
  `REQUEST_INVALID` with `details` `{reason: "no-agent", tried}` and a message pointing at
  `--print`. A failed exit is an error naming the agent, its status and its last stderr line
  (`collect::failure`, shared with the rasterizers, as is `collect::first_program`). At most
  1 MiB of its stdout and of its stderr is kept (`collect::run_capped` drains the rest); more
  refuses `REQUEST_INVALID` with `details.reason` `explain-failed` and a message saying the
  output is too large.
  Explaining writes nothing and takes no claim.
- T34: view. `model view --allow-explain` (off by default) lets the page ask: `GET /api/explain`
  answers `{kind: "explain-status", allowed}`, and `POST /api/explain?subject=...&relationship=
  &flow=&proposal=&question=` (parameters in the query, `subject` repeatable, no body read) runs
  `model explain` with the user's configured or first agent and answers `{kind: "explain", agent,
  answer}`. The token and Host checks apply to both; `/api/explain` alone accepts POST (405 with
  `Allow: GET, POST` otherwise), and a POST to a view started without the flag gets 403 with
  `details.reason` `explain-disabled`. A POST takes the token only from the `X-Boundrail-Token`
  header, never `?t=`, so a cross-site form cannot send it and a cross-site script needs a
  preflight, which the server answers 405; a POST whose `Origin` is present and not
  `http://127.0.0.1:<port>` or `http://localhost:<port>` gets 403. One agent runs at a time: a
  POST while one runs gets 409 with `details.reason` `explain-busy`. The detail panel's `Ask
  the agent` section asks about the selected module, or from a relationship line's `Ask` about
  that relationship, adding the active pane's open flow and its proposal; with no selection it
  appears when a flow or proposal is shown, and never for a checkpoint, which `model explain`
  does not read. The answer renders with `textContent`. When the server does not allow it, `Ask`
  copies the `boundrail model explain` command line, shell-quoted, and shows it. ponytail: one
  blocking request per question with no progress or cancel; a queue with progress events if
  answers get long.
- T34: security. The agent is a program the user chose, run only on the explicit command or, for
  the view, on the explicit `--allow-explain` opt-in, since a browser request then starts a local
  program; the view's header token, Origin and Host checks and loopback binding still gate it. It
  receives the model's text as data, framed as above; the framing lowers but cannot remove the
  risk that an agent follows text written into the model, which is why `claude` gets no tools or
  MCP servers and `codex` a read-only sandbox in an empty folder. That sandbox does not stop
  `codex` reading files on disk into its answer, which reaches only the user. `src/git.rs`
  states this exception beside the rasterizers. Tests run fake `claude` and `codex` scripts from a
  folder that is the whole `PATH` (with Git linked in), asserting the stdin bundle and its
  framing, the arguments, `--print`, the refusals, an output over the cap, the config choice, the
  endpoint off by default and on with `--allow-explain` and the header token, a POST with the
  token only in the query or a foreign Origin refused, and a second POST during a run refused.
  Tried by hand with the real `claude` and `codex` CLIs on a two-module model.
- T33: selection. Shift-, Command- or Control-click on a module in the list or on the canvas
  adds it to the selection or takes it out; a plain click selects it alone and Escape clears.
  With one module the detail panel is the module's; with several it is the selection's: its
  members (each removable), the relationships between them (both ends inside a member or its
  parts) and the outside modules they reach, with counts, plus `Ask the agent` about every
  member. Focus dims what the whole set does not touch, and the canvas outlines every member.
- T33: relationship-kind filter. The sidebar shows a chip per relationship kind the shown state
  uses, named from `config.yaml` with a line drawn in its dash; pressing one hides that kind's
  relationships on the canvas (client side, `display: none`, as the kind filter dims) and in the
  module and selection lists, which say how many are hidden. It never reaches `/api/render`, so
  readings keep drawing every kind they name.
- T33: the address. After `#` the page keeps what its active pane shows, as `key=value` pairs:
  `proposal` or `checkpoint`, `against`, `view` (absent for the architecture), `flow`, `scope`,
  `depth` (absent at 2), `saved`, `picture`, `walkthrough` and `beat` (from 1), then `select` per
  selected module and `focus=1`. A pane drawing a saved view, picture or beat writes only that.
  A lone selected module stays `#<qualified module ID>`, and `#walkthrough=<id>&beat=<n>` reads
  as before. Every change (selection, focus, state, comparison, view, flow, drill, depth, beat,
  view or picture opened, split or active pane) adds a history entry; Back and Forward restore
  the entry's reading in the active pane, replacing rather than adding entries while they do,
  and a pasted address opens it. The token is never in the hash. `Shown` in
  `src/arch/view/mod.rs` reads and writes the same form, so `--show` and `--panes` share it with
  the page; unknown keys and values that do not parse are left out. The relationship filter,
  the text and kind filters and the second pane's reading stay out of the address.
- T33: an agent shows a reading. `model view` writes `view.json` (`{port, control}`) to this
  worktree's `<git dir>/boundrail`, created as a new file with mode 0600 and renamed into place. On
  return it removes the file only if the file still holds its own control token, so a second view's
  file is never removed by the first; there is no signal handler and no `unsafe`. The control token
  is a second 256-bit value the page never sees. The watcher ignores the file. `model view --show`
  takes the reading flags of `model render` (`--view`, `--flow`, `--scope`, `--depth`, `--focus`,
  `--proposal`, `--checkpoint`, `--against`, `--saved-view`, `--walkthrough` with `--beat`, default
  1) plus `--page`, validates them with the checks and name lookups `model render` makes before it
  lays anything out (so it refuses the same way, without drawing; a saved view's or beat's own
  targets are left to `model check` and the page), resolves names to the qualified IDs the page
  reads (a proposal's name to its ID), and sends `POST /api/show?reading=<hash>[&page=N]`;
  `--focus` selects its modules with focus on. The page plays the current state's walkthroughs, so
  `--walkthrough` with `--proposal` (T26) refuses `REQUEST_INVALID`. `--panes` sends `GET /api/panes`. Both carry the control token in `X-Boundrail-Control`
  only, never the query, and neither accepts the page token; the page's token never passes them, and
  the Host and Origin rules apply to both, GET included. `/api/show` and `/api/pane-state` take POST
  only. Each event stream starts with `event: hello` giving the page a number; the page reports its
  panes' hashes and active pane with `POST /api/pane-state?page=&active=&pane=...` (the page token
  in its header), on every change and when its window gains focus. A reading goes to `--page`, else
  to the open page that reported last, as `event: show` on its stream; the page adds a history
  entry, shows it in its active pane and marks the pane with a ring that fades in 600 ms. No view
  refuses `REQUEST_INVALID` with `details.reason` `no-view` (no state file, a server that does not
  answer at its port, or one that refuses the file's control token with 403); no open page refuses
  `no-page` (409); an unknown `--page` refuses `SUBJECT_NOT_FOUND`. Everything stays on 127.0.0.1.
  ponytail: one state file per worktree, so a second view in the same worktree takes it over; a view
  stopped by a signal or killed leaves a stale 0600 file, which readers treat as no view; a closed
  page stays listed until its next keepalive fails (up to 15 s).
- T33: tests. `Shown` round-trips every field and reads the older forms and what `URLSearchParams`
  writes (unit); the control token passes in its header only (unit). Against a running view: the
  state file's port, control token and mode 0600; `--show` and `--panes` with no view, a stale file,
  an unknown page, a reading render refuses and flags without `--show`; two pages, the one that
  reported last receiving the reading with names resolved; the control endpoints refusing no token,
  the page token, the control token in the query or as the page header, a foreign Origin or Host,
  and GET on `/api/show`; two views in one checkout, the second reachable after the first stops; a
  stale file and one whose control token the server refuses reading as no view; page-reported hashes
  printed with control characters escaped; the file's writes raising no change event. A lone
  selection's short hash form is tested on its own (unit).
- T29: a module's `references` is an optional ordered list, left out of the file when empty, so
  a vault written before T29 loads, reads and digests as it did. Each is `{model?, state?,
  subject?, pin?}`: `model` is a repository path relative to the checkout root (the `binding`
  path rules, so no `..` and nothing absolute) or `<alias>:` with an optional such path, where the
  alias names an entry of the user's `boundrail/checkouts.json` (the language-server config's
  folder, a JSON array of `{name, path}` with absolute paths); absent, the reference is to this
  model. `state` reuses `ReadingState` and defaults to `current`; `subject` is a qualified ID;
  `pin` is a full commit hash. A write refuses `MODEL_INVALID` for a reference that names nothing,
  a malformed model, proposal, checkpoint, subject or pin, and a pin beside a non-current state;
  nothing a reference names is read on write, so a missing vault never refuses one.
  `edit-module` sets the list whole and clears it as `references`; `model compare` reports it as
  one module field and reconcile compares it like the other fields.
- T29: following a reference resolves its model to a folder, refusing a path that symbolic links
  lead out of the checkout or the alias's folder, then loads the vault of the Git checkout around
  that folder: the pinned checkpoint from that repository's history ref when `pin` is given, else
  the state. Reads go through `vault::load` and `checkpoint::load`, so the other repository is
  read with the hardened Git runner and never written or fetched. `model read <module>
  --references` lists every reference with `resolved` (`checkout`, `vault`, the other `model`'s
  ID and name, the `state` read with a proposal qualified and a checkpoint by ID, the
  `checkpoint`, and the `subject` with `id`, `name`, `type` and a module's `kind`) or `skipped`
  (the refusal's `code` and `message`), so one missing vault never fails the listing.
  `--reference N` (from 0) prints what one points at as `model read` prints it there, the whole
  model without a subject, plus `reference` (`index`, `module`, `checkout`, `state`); one that
  cannot be followed refuses with its reason. Both read the module from `--checkpoint` or
  `--proposal` when given.
- T29: `model read <module> --code` prints `code_target`: `repo`, `path`, `symbol`, `commit` (the
  checkpoint's source with `--checkpoint`, for a binding without `repo` only), and `local_path`,
  the canonical path in this checkout, or for a `repo` in the checkout `boundrail/checkouts.json`
  names so; otherwise `unavailable` says why. A module without a binding refuses
  `REQUEST_INVALID`. `--open` refuses a target without a local path or that does not exist, then
  starts the platform opener with one path argument, never through a shell, and prints the
  command line to stderr and as `opened`. No opener is handed a file it could run: macOS opens a
  file with `open -t` (the default text editor) and reveals a folder with `open -R`; Windows and
  Linux open the folder holding a file, or the folder itself, in Explorer or with `xdg-open`.
  Explorer gets the folder without the `\\?\` prefix, and a folder whose path holds a comma is
  refused, since Explorer reads commas as option separators.
- T29: the browser lists a module's references in the detail panel through `/api/references`
  and shows a followed one read-only in a reference pane through `/api/reference?subject=&index=`
  (both take `proposal` or `checkpoint`, not both). A reference that cannot be followed is a plain
  label with the reason and no link. Double-clicking a module without parts but with references
  selects it. ponytail: the pane shows the reading as text, not a drawing; render the other model
  when one is needed. The CSP is unchanged and the page makes no request off its own server.
- T26: `proposal.json` gains `summary` (one line, 1 to 200 characters, `SUMMARY_MAX` as a flow's)
  and `documents`, a list of `{title, path?, url?}`: a title of 1 to 120 characters on one line
  and exactly one of a repository path (`is_repo_path`, no control character) or an `https://`
  URL of at most 2000 characters with no space or control character (`MODEL_INVALID`
  otherwise). Both are left out of the file when absent, so proposals written before T26 load
  and digest as they did, and `propose` without them keeps its old fingerprint. Documents are
  text: nothing reads, fetches or links them; the browser shows the summary and documents as
  the proposal item's title text. `model propose --summary` and `--document <title>
  <path-or-url>` (repeatable; a target naming `://` is a URL, else a path) set them, and
  `model proposals` lists them when present. Importing a proposal bundle carries them; a
  description is not imported.
- T26: `edit-proposal` (`set` name, description, summary, documents; `clear` description,
  summary, documents) is an edit change, so it rides the journaled edit of `model edit
  --proposal` and retains a proposal checkpoint like every proposal write (T25). In a
  current-state edit it refuses `REQUEST_INVALID`. The batch holds a copy of `proposal.json`
  that the change edits, once per batch; validation is the load's (`MODEL_INVALID` with the
  change index), the name is trimmed and checked as `propose` does (`NAME_INVALID`), and the
  receipt lists the proposal as changed and
  claims cover it (the proposal's root level).
- T26: one resolver, `vault::load_proposal`, takes a proposal's local or qualified ID, else its
  name compared with case among every `proposal.json` that reads. An exact ID wins, so a
  proposal named like another's ID never hides it; a name two proposals share refuses
  `SUBJECT_AMBIGUOUS` with the local IDs in `details.matches`; none refuses `SUBJECT_NOT_FOUND`.
  Every command loads proposals through it (`--proposal` of read, check, inspect, render,
  explain, footprint, edit and export, `compare` and its `--against` and `--beside`, `adopt`,
  `shelve`, `reconcile`, `propose --from`). An edit request's `target.state` stays the ID.
  Stored readings name proposals by qualified ID and reach the name only when that ID names no
  proposal, which validation keeps to an ID-shaped name.
- T26: walkthroughs inside proposals. A proposal holds `walkthroughs/` beside its modules and
  collections: its revision, `file_digests`, the journal's write allow-list, checkpoints and
  bundles cover them; the base never holds one. Pictures and saved views stay current-only and
  a proposal edit with one still refuses `REQUEST_INVALID`. A beat reads the state the batch
  edits when `state` is absent from the request (`NewReading.state` is optional), stored as
  `{"proposal": <qualified ID>}` in a proposal, so the file never depends on where it lies and
  adoption needs no rewrite; `"current"` given explicitly reads the current state, and clearing
  `state` reads the edited state again. References into the loaded state (the current state, or
  the proposal for its own walkthroughs) must exist and resolve by name in the batch, others are
  qualified as given, and `REFERENCED_BY_WALKTHROUGH` judges the loaded state's readings.
  Walkthrough and beat IDs come from the proposal's allocator; removing one puts its IDs on the
  `dropped` list, and a claim on the proposal holds its walkthroughs. Before the request is
  fingerprinted, an absent `state` is filled with the edited state (`"current"`, or the target
  proposal), so a request written before T26 replays its receipt.
- T26: adoption. The current state's walkthroughs become its own with the proposal's added,
  and the adopted state is validated with them, so an ID both hold refuses `MODEL_INVALID`. A
  beat reading the adopted proposal keeps reading it, read only. `propose --from` copies no
  walkthrough, since its beats read the source. Import of a proposal bundle carries its
  walkthroughs, a beat reading or compared with the bundled proposal then naming the new draft;
  a current-state bundle's are not imported. Reconcile keeps the draft's walkthroughs as they are and validates
  them over the merge (`MODEL_INVALID`, `cause: "merge"`).
- T26: compare. `walkthroughs` joins the comparison, each walkthrough compared as its whole file
  (`beats` a field), and is left out of the JSON when nothing differs, so earlier outputs hold.
  Against the current state or a checkpoint the proposal's side is what adoption leaves (the
  other side's walkthroughs with the proposal's added), so nothing reads as
  removed; against another proposal each side is its own. Render marks and footprints compare
  no walkthroughs. `model render --proposal <id> --walkthrough <w> --beat <n>` finds the
  walkthrough in the proposal; `--proposal` then names where it lies, not what to draw.
  `model check --proposal` judges its beats (`WALKTHROUGH_TARGET_MISSING`); a beat in a
  proposal that reads the current state is judged only by the load's shape rules, since the
  check loads no current state there. ponytail: the browser lists and plays the current state's
  walkthroughs only, and `model record` records them only; add `proposal` to `/api/walkthrough`
  and `record` when a proposal's slices need playing.
- T26: the plan skill records slices in the proposal's description or summary (editable with
  `edit-proposal`) or as a walkthrough in the proposal, so slicing no longer edits the current
  state and no longer makes adoption refuse `PREDECESSOR_MOVED`.
- T27: a child is `model propose --parent <draft>` (ID or unique name; not with `--from`): a copy
  of the draft's modules and collections, no walkthroughs, whose base is the draft as it is and
  whose `dropped` list is the draft's. `proposal.json` gains `parent`, the parent's local ID,
  left out when absent, so proposals written before T27 load and digest as they did, and
  `propose` without it keeps its fingerprint. A child's `predecessor` is `{state: <parent ID>,
  revision: <content digest>}`, the content digest hashing the draft's `modules/` and
  `collections/` files at their proposal-relative paths (`Model::content_digest`), base and
  walkthroughs aside, so editing the parent's walkthroughs or metadata does not move it. An
  adopted or shelved parent refuses `PROPOSAL_CLOSED`. A copy of a child (`--from`) inherits its
  parent; an imported proposal bundle keeps its parent, which another clone may lack.
- T27: standing, of drafts only: the predecessor is the current state for a draft without a
  parent, the parent while the parent is a draft, or the parent adopted while it still is the
  current state (its `adopted_as` is the current state and their modules and collections are
  equal, the rule `--from` already used). `up-to-date` when the recorded revision equals that
  predecessor's (the current state's predecessor digest, or its pre-T14 form, or the parent's
  content digest), `behind` otherwise, and `orphaned` when the parent is shelved, adopted and
  since replaced, or gone. `model proposals` adds `parent`, `children` (local IDs in ID order)
  and `standing` to each entry, leaving `standing` out for closed proposals and when the current
  state or a parent does not load; Markdown nests children under their parent and appends the
  standing. `model read --proposal` adds `standing` (JSON) and a `Standing:` line (Markdown);
  checkpoint readings have none.
- T27: adoption of a child whose parent is a draft refuses `PARENT_NOT_ADOPTED` with
  `details.parent`, after `STALE_REVISION`; once the parent is adopted and still the current
  state, the child adopts when its recorded revision is the parent's content digest, and
  otherwise refuses `PREDECESSOR_MOVED`, as does an orphaned child. Adopting a parent changes no
  child. "Adopt the family in order" is adopting each in turn; there is no family adopt.
- T27: reconcile merges against the parent while it is a draft (keeping `parent`, recording its
  content digest), and otherwise against the current state, clearing `parent`, so an orphaned
  child leaves the family. A child of a shelved or missing parent therefore loses what the parent
  changed and it did not, and keeps what both changed as conflicts: it refuses
  `PARENT_CHANGES_DROPPED` with `details.subjects`, the subjects the child holds as its base (the
  parent) does and whose version the merge takes from the current state, until `--confirm
  PARENT_CHANGES_DROPPED` (or `=<subject>` for each). A child of a parent adopted and since
  replaced needs no confirmation: the parent's changes are in the current state's history. The
  base becomes the predecessor as it is. The current state's revision is still required and its retired IDs still apply.
  `MODEL_INVALID` with `cause: "merge"` now says "its predecessor".
- T27: settling. `--settle <subject>=mine|theirs|edited:<file>` (repeatable; qualified or local
  ID) decides a conflicting subject inside the merge: `mine` takes the draft's version (or its
  removal), `theirs` the predecessor's, `edited` the file's JSON, parsed as what the base, draft
  or predecessor holds under that ID: a module (its own fields; `outgoing` must be empty, since
  each relationship merges and settles as its own subject), a relationship (kept in the owner the
  draft or predecessor gives it) or a collection; one that does not parse, holds another ID, or
  names nothing refuses `REQUEST_INVALID`, and one that does not load refuses `MODEL_INVALID`
  (`cause: "merge"`). A relationship whose owner the merge removed settles too: `mine` and
  `theirs` keep it, with its owner as that side holds them, only when that side holds it, and
  drop it otherwise, leaving the owner removed; `edited` writes the replacement into its owner as
  the draft, else the predecessor, holds it. A settlement naming a subject not
  in conflict, or a subject twice, refuses `REQUEST_INVALID` (`details.subjects`) before
  `RECONCILE_CONFLICT`, which lists only what no settlement decided, with the same `details` as
  before. A `retired` conflict never settles: retired IDs stay reserved (D3). Settlements and
  confirmations join the request, and so the fingerprint, only when given, so a retry with other
  choices is another operation and old fingerprints hold.
- T27: confirmations, judged after the conflicts on the merged draft against the base and the
  predecessor now. `INHERITANCE_DROPPED`: a merged module's `parent` names a module the base held
  and the predecessor no longer does, and the merge does not keep it (a draft's new part in a
  container the predecessor removed, before T27 `MODEL_INVALID`; or a part settled `mine`).
  `details.modules` lists `{module, parent, lifted_to}`, `lifted_to` being the nearest container
  above the removed one, by the base's containment, that the merge keeps, else null for the
  root; confirmed, the part moves there. `PART_RECREATED`: the merge keeps a module, by ID, that
  the base held and the predecessor removed, which only a settlement as `mine` or `edited`
  makes; `details.modules` lists them, and confirmed they stay. Against the current state such
  an ID is retired and refuses `RECONCILE_CONFLICT` (`retired`) first, so `PART_RECREATED` comes
  from children of drafts. `--confirm <CODE>` covers every module, `<CODE>=<module>` one; a
  confirmation nothing needed is ignored. Refusals in order: `PROPOSAL_CLOSED`,
  `STALE_REVISION`, `RECONCILE_NO_BASE`, `REQUEST_INVALID` (settlements), `RECONCILE_CONFLICT`,
  `PARENT_CHANGES_DROPPED`, `INHERITANCE_DROPPED`, `PART_RECREATED`, claims, `MODEL_INVALID`. Adoption takes no
  `--confirm`: it needs an unmoved predecessor, so its base equals the current state and neither
  case can arise; it merges nothing.
- T27: `--family` reconciles the named draft, then every draft descending from it through drafts,
  depth first, parents before children and siblings in ID order, each against its parent as just
  written. Each is its own journaled write retaining its own checkpoint (T25), since a journal
  record's scope covers one proposal: the descendants take operation ID `<operation-id>.<local
  ID>`, the current revision the caller named, their own revision as their write reads it (their
  request names none), the confirmations naming one subject (a confirmation of every subject
  covers the named draft only) and no settlements. When the draft has descendants, an operation
  ID too long to leave room for `.<local ID>` within the operation ID limit refuses
  `REQUEST_INVALID` before any write. A descendant already `up-to-date` is skipped
  (`reconciled: false`); a retry with the same operation ID replays each receipt written and
  goes on from there. A refusal stops the walk with `details.family` listing the
  drafts finished before it (`{proposal, reconciled}`), whose writes stay; settle the refused
  draft alone, then run `--family` from it. JSON is the named draft's receipt with `family`, each
  descendant `{proposal, operation_id, reconciled, receipt?}`; Markdown prints a receipt per
  reconciled draft and `<id> is up to date` for the others.
- T30: correction. `model history correct <checkpoint> --source <commit> --doing [--writer]
  [--operation-id]` appends a checkpoint holding the named one's tree, `Revision` and `Proposal`,
  with `Source: <commit>` and `Corrects: <its ID>`, on the tip it read: the ref moves by
  compare-and-swap (`HISTORY_MOVED`), the commit is signed per `commit.gpgSign`
  (`SIGNING_FAILED`), and nothing is rewritten. `Source` is the only correctable trailer; a
  proposal's checkpoint, shelved or not, corrects the same way. The source must be a full 40- or
  64-hex hash of a commit this repository holds (`REQUEST_INVALID` otherwise), so a correction is
  never `uncommitted`. Refusals: `CHECKPOINT_NOT_FOUND` or `SUBJECT_AMBIGUOUS` for the
  checkpoint, `REQUEST_INVALID` for a checkpoint that is itself a correction (`details.corrects`
  names the one to correct), `NOTHING_TO_RETAIN` when it already records that source, and
  `OPERATION_REUSED` for an operation ID that made another checkpoint; the same operation with
  the same arguments returns the correction it made. Corrections chain one level only, so no
  cycle can form. `Corrects` needs a local ID, a `Source` and no `Uncommitted`, else the commit
  is not a checkpoint. Every reading (`checkpoints`, `read --checkpoint`, source availability,
  `read --code`) applies the corrections when it walks the history, oldest first, to the earlier
  non-correction checkpoint each names: its `source` is the latest correction's, `uncommitted`
  false, and `corrections` lists each as `{checkpoint, commit, writer, time, doing, previous,
  source}`; the correction itself lists with `corrects`. A correction naming no earlier
  checkpoint, or a correction, or one whose revision, proposal or tree (`<commit>^{tree}`)
  differs from that checkpoint's, is not applied. `since()` (push, fetch, replay, export and
  restore) refuses `MODEL_INVALID` for such a correction and for a `Restored-From` that is not a
  full commit hash. `read --code` says when its commit comes from a correction, naming the
  correcting checkpoint and its writer, in markdown and as `code_target.correction`. Corrections
  are attributed by their `Writer` trailer, not verified: no read checks a signature, since that
  would run the signing program on read paths. Retain's `NOTHING_TO_RETAIN` skips corrections, its cross-worktree replay
  refuses `OPERATION_REUSED` for one, and a write never takes one for its own checkpoint. Histories without the new trailers read as
  before.
- T30: restore. `model export --with-history` (current state only) adds every proposal's files at
  their vault-relative paths, each proposal loading first, and `history`: `{checkpoints, blobs}`,
  the checkpoints oldest first as `{commit, author_date, committer_date, message, files}` (dates
  as Git records them, `<seconds> <zone>`; `files` maps vault-relative paths to SHA-256 keys of
  `blobs`, which holds each text once). Every first-parent commit must pass `since()` and every
  tree a checkpoint read (`MODEL_INVALID`). A plain export is unchanged. `model import --restore
  <bundle>` takes no revision, operation ID or doing line: it refuses `VAULT_EXISTS` when the
  checkout has a vault and `HISTORY_DIVERGED` when `refs/boundrail/history` exists, then checks
  the bundle as import does (`BUNDLE_INVALID` also for a proposal or checkpoint bundle), loads its
  current state through a temporary vault at the header's model ID and source revision, and each
  proposal with its base (`MODEL_INVALID`, `BUNDLE_INVALID`). The history is rebuilt before
  anything is written: the texts are checked against their digests, paths against the vault
  files a checkpoint may hold, commits and dates against what Git writes, each message with its
  `Restored-From` as `since()` checks it and each correction against its target's revision,
  proposal and tree (`BUNDLE_INVALID`), all before anything is signed; then each checkpoint is
  committed on the one before with its tree, message and dates and a
  `Restored-From: <original commit>` trailer replacing any earlier one, signed per
  `commit.gpgSign`; the result passes `since()` and every tree a checkpoint read, the same checks
  a fetch applies. Then the vault folder is created (as `init` claims it) with the bundle's files
  byte for byte, so every ID and the revision stay, and the ref moves from absent by
  compare-and-swap; a failure there removes the vault it made. The output is `{kind: "restore",
  vault, model, revision, proposals, checkpoints}`. Claims, the journal, receipts and the history
  remote are not carried. A retry refuses `VAULT_EXISTS`. ponytail: the whole history travels in
  one bundle under the 4 MiB `INPUT_MAX`; page it into several files if real histories outgrow
  it.
- T31: a private vault lives in a folder outside every Git checkout: `model init --private
  <folder>` (made when missing, one level) writes `.boundrail/model` as in a checkout and creates
  `.boundrail/history.git` beside it, a bare repository made with `git init --bare
  --template=`, so it holds no hooks or other template files. Every command then takes `--root`
  at or below the folder. The vault folder's checkout is the private folder, and every Git
  command Boundrail runs at or below it (`src/git.rs`) adds `--git-dir <history>`, so retain,
  every write's checkpoint (T25), claims and the journal (`<git dir>/boundrail`), checkpoint
  readings, export of a checkpoint and the view's watchers use the private history unchanged,
  with no working-tree Git operation. Signing follows `commit.gpgSign` from the user's config or
  the private history's own, and the identity comes from the same; tests set both on the history.
  The history's HEAD is unborn, so checkpoints carry no `Source` and read `source_status:
  unavailable`.
- T31: the history is found by the filesystem, not by Git: the nearest folder at or above the
  path holding `.boundrail/history.git`, with `.boundrail`, the history and `.boundrail/model`
  real directories (no symbolic link) that on Unix the effective user owns, and only when no
  folder at or above the path holds `.git`. The effective user is the owner of a folder the
  process creates in the system temporary folder, since std has no `geteuid`. The answer is
  memoized per canonical path for the process and cleared when `init --private` creates a
  history. So a bare repository committed into a checkout or planted by another user, or a
  private folder later moved into one, is never used; inside a checkout the checkout's vault and
  history apply as before, and a command whose path has a private history absorbed by a `.git`
  above its folder warns once on stderr. A plain `model init` never adopts a private folder: below
  one it refuses `CHECKOUT_NOT_FOUND`. `init --private` refuses `REQUEST_INVALID` for a folder
  that is a symbolic link or lies inside a checkout (Git's toplevel or any `.git` above), saying
  the checkout would absorb the vault, removing a folder it made, and `VAULT_EXISTS` when
  `.boundrail` exists there; a failure after that removes `.boundrail` again. Its JSON adds
  `history`. The history's config is trusted as a checkout's `.git/config` is: running Boundrail
  in a folder trusts its Boundrail folder, as running Git trusts `.git`.
- T31: `init --private <folder> --from <bundle>` starts the vault from a current-state or
  checkpoint bundle (`model export`), validated as `import` validates one and loaded through the
  loader, so it holds that model with its IDs, current state and revision; a proposal bundle
  refuses `BUNDLE_INVALID` and another model's manifest `MODEL_MISMATCH`. Since the IDs match,
  a proposal drawn there compares and renders against the repository's state with the existing
  `compare` and `render --proposal`, and `export --proposal` from it imports into the repository.
  `import` alone could not do this: it refuses another model's bundle, and a fresh private vault
  is another model. Seeding is not a journaled write and retains no checkpoint; the first edit
  does. A bundle exported `--with-history` (T30) seeds its current state only: its proposals and
  history are left out, as `import` leaves them. `import --restore` needs a checkout: in a private
  vault it refuses `VAULT_EXISTS` and outside every checkout `CHECKOUT_NOT_FOUND`, while a private
  vault's `export --with-history` restores into a checkout like any other. `history correct`
  works on a private history; its source must be a commit that history holds, so fetch the
  code's commit into it first (`git --git-dir <history> fetch <repository> <commit>`).
- T31: `model check` on a private vault skips the code check with one skip `{subject: <model>,
  site: "code", reason: "no-checkout"}` and runs the semantic and embeds checks. `--code
  <checkout>` names a Git checkout (its toplevel; `CHECKOUT_NOT_FOUND` otherwise) whose code the
  code check scans instead of the vault's own, for any vault; it only reads it, though language
  servers enabled for it run as they do there. `footprint` and `locality` refuse
  `CHECKOUT_REQUIRED` on a private vault, since they read a checkout's commits. `history push`,
  `fetch` and `configure` work against remotes added to the private history (`git --git-dir
  <history> remote add`); a remote it lacks refuses `CHECKOUT_REQUIRED` naming that command.
  `model inspect` skips the scan the same way (`code_skipped` starts `no-checkout`) and takes
  `--code <checkout>` like check. ponytail: `setup`, `pictures --write`, `record` and `explain`
  treat the private folder as the checkout. Moving the folder moves
  the history with it; deleting the folder discards both.
- T31: temporary pull request models. The PR skill exports the repository's current state,
  starts a private vault from it in `mktemp -d`, draws a proposal there, compares and renders it,
  checks it with `--code <repository>`, and deletes the folder, so nothing is committed. The
  proposal committed on the branch (T36) stays the alternative when CI should gate the pull
  request against it, since the action reads proposals from the checkout's vault.
