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
| T10 | Measures: module interface counts (`model inspect`), co-change locality (`model locality`), branch footprint (`model footprint`) | done |
| T11 | Live browser view: model canvas, drill-down, focus and filters, detail panel, warnings, comparison, history, split panes | done |
| T12 | Infrastructure evidence: docker-compose, Kubernetes, Terraform bindings and configured-address hops between modules | done |
| T13 | Language-server evidence: call-level references through configured language servers | done |
| T14 | Walkthroughs: ordered beats over readings, playback in the browser | done |
| T15 | Embedded pictures: render recipes in SVG/PNG, `check embeds`, PNG rasterize | done |
| T16 | Agent integration: `model setup`, skills for authoring/planning/PRs, CI action with PR annotations and footprint comment | done |
| T17 | Reconcile drafts after current changes, export/import bundles, history publication | done |
| T18 | Remove scan-era code that the model does not use | done |

## Notes

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
  would break `include.path` and per-repository signing setups. The same holds for the clean and
  process filter drivers repository config defines, which run when Git content-checks a working-tree
  entry, for example in `model footprint`; `src/git.rs` states both exceptions. The ref moves by
  compare-and-swap with a reflog entry; a lost race or a held ref lock refuses `HISTORY_MOVED` and a
  retry builds on the new tip. The new commit's parent and the swap's old value are the tip the
  history was read from, even a commit without checkpoint trailers. Checkpoints restore the current
  state; drafts travel with the committed vault or in bundles (T17), and `model history push` and
  `fetch` publish the history ref (T17); once fetched, checkpoint readings resolve. A vault file
  larger than the 4 MiB a checkpoint reader accepts refuses `MODEL_INVALID` before anything is
  written. Retaining an unchanged model refuses `NOTHING_TO_RETAIN`. Trailers `Source` and
  `Operation` appear only when there is a value.
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
  candidates. Without symbol data `symbols` is null with `symbols_skipped`
  `language-unsupported`; a file a symbol limitation names makes it `symbols-incomplete`. Deep
  imports are the runtime edges `ENTRY_POINT_UNEXPOSED` reports. Incoming and outgoing entries
  count import edges between files of two owners, type-only edges included, and list the
  relationships drawn that way between the two or modules containing them.
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
  `crossings`, `crossings_skipped`, `model`, `model_error` and `scan_error`.
- T11: `model view [--port N] [--no-open]` serves the page and a JSON API on 127.0.0.1 only, port 0
  (a free one) by default, and prints the URL with its token as the first line on stdout. Without
  `--no-open` it starts the platform opener (`open`, `xdg-open`, `cmd /c start ""`) with a one-time
  launch URL, `/launch?c=<code>`, as one argument, never through a shell string, so the token stays
  out of every process's arguments. The code is a second OS-random value, never the token. The first
  request naming it gets a 303 to `/?t=<token>` and spends it; an unknown or spent code gets 403.
  Connection threads run with the invoking thread's tool timeout. The page
  (`src/arch/view/index.html`, `app.js`, `style.css`) is embedded with `include_str!`, uses no
  framework and loads nothing from other origins, so it works offline.
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
  The page and its two assets carry no data and need no token. Every request must name exactly one
  Host, `127.0.0.1:<port>` or `localhost:<port>`, else 403, which stops DNS rebinding. Only GET is
  served (405 with `Allow: GET` otherwise). A request head is capped at 16 KiB (431) and must arrive
  whole within 10 s (408), however slowly it drips. At most 64 connections are served at once
  besides 16 event streams; more get 503, and a failed thread start or accept never stops the
  server. Every response sends `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and `Connection: close`. The page's Content-Security-Policy allows
  only its own script, style and connections; SVG is parsed with `DOMParser` and imported, never
  run, and `/api/render` opened on its own carries `default-src 'none'; style-src 'unsafe-inline'`.
  The server writes nothing: every endpoint is a read the CLI already does. The hardening exception
  of T7 covers its reads too: a clean or process filter driver repository config defines may run
  when Git content-checks a working-tree entry.
- T11: limits. ponytail: a thread per connection under the caps, fine for one person's pages; a
  pool if many share a server. The vault watch is set at start: a vault folder deleted and made
  again is not followed. Checks and interfaces scan the checkout on each request, as the commands
  do, so `/api/check` and `/api/inspect` take as long as `model check` and `model inspect`;
  checkpoints have neither. Dark mode inverts the rendered SVG with a CSS filter rather than
  drawing it in other colors. `#<qualified module ID>` after the URL selects that module when the
  page opens.
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
  (the T5 polyglot gap) keeps its `not-scanned` skips, and a compose service built from its folder
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
  not judge (the T5 polyglot gap) stay `not-scanned`. Tried by hand with
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
  walk, and `--doing`, `Writer` and `Operation` values that pass retain's own checks; otherwise it
  refuses `MODEL_INVALID` (`details.object`) and the local ref stays. A local tip that already
  holds the fetched one stays. Diverged histories replay: the local-only first-parent commits,
  checked the same way, are copied in order onto the fetched tip with `commit-tree`, each keeping
  its tree, message, trailers and author and committer dates, signed as retain signs
  (`SIGNING_FAILED` before the ref moves). A local-only ID the fetched history holds in any case
  refuses `HISTORY_DIVERGED` with `details.clashing`. The receipt lists `replayed`. Replaying
  replaces those commits, so stored retain receipts naming them go stale; readings name
  checkpoints by ID and still resolve, and a retain retried with the same operation ID returns the
  checkpoint as the history now holds it. Otherwise the local ref moves by compare-and-swap
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
  now passes the same value: `native::typescript::collect`'s `scan_all` (always false),
  `native::python::collect`'s `package` (always none) and `python_paths` (always empty),
  `native::go::collect`'s `go` (always `go`), `grimp::package`'s `explicit`, `analyze`'s
  `cochange` (always none), the constant `Config` fields `live` fills (`granularity`, `includes`,
  `all_kinds`, `no_default_excludes`, `include_generated`, `explicit_excludes`, `rules`,
  `codeowners`), and `CollectionOptions::engine`, which has one variant.
