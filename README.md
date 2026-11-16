# Boundrail

**Architecture your agents can follow.**

Boundrail keeps an authored architecture model next to your code and checks the two against each
other. You and your coding agents describe modules, the code each one binds, their relationships
and flows in `.boundrail/model/`, committed with the code. `boundrail model check` reports where
the code crosses a boundary the model does not draw, and `boundrail model footprint` shows what a
branch changes. Developers, agents and CI read the same model and the same evidence.

[Quickstart](#quickstart) | [The model](#the-model) | [In CI](#in-ci) | [Language coverage](#language-coverage) | [Limits](#limits)

## Quickstart

Install from the checked-out source with stable Rust:

```sh
git clone https://github.com/dogebonker/archgraph.git
cd archgraph
cargo +stable install --path . --locked --bin boundrail
```

Then, from the root of your project's Git checkout, create the model and install the model skills
for your agent:

```sh
boundrail model setup --agent claude
```

Setup creates `.boundrail/model/` when it is absent and writes the `boundrail-model`,
`boundrail-plan` and `boundrail-pr` skills into the agent's project skill folder (`--agent` takes
`claude`, `codex`, `cursor` or `opencode`, and repeats). Running it again changes nothing; files
you edited stay unless `--force` replaces them. It prints the next steps.

The skills carry the agent workflow. One spec gets one proposal (`boundrail-plan`), sliced into
tickets; each ticket's branch passes `model footprint --proposal <id> --fail-outside` before review,
and the last slice's merge adopts the proposal. Each pull request body (`boundrail-pr`) has a
Summary, Evidence (footprint, check, interface changes, picture) and Merge danger section, plus a
state marker line, `<!-- boundrail:proposal <id> -->`, that CI reads. A pull request outside any
plan draws a temporary proposal and shelves it after the merge. `boundrail-model` says when to use
`inspect`, `locality` and `compare --beside` to find and choose refactors.

Boundrail was previously named **Archgraph**. The repository URL keeps the old name.

## The model

The model is the architecture as you and your agents describe it: modules bound to code,
relationships, collections, flows and walkthroughs, proposals for a future state, and checkpoints
of past ones.

```sh
# Read the model, then change it with a batch request (schema: boundrail model schema model-edit).
boundrail model read --format markdown
boundrail model edit --doing "Add the billing service" --input request.json

# Compare the model with the code, then open it in the browser.
boundrail model check --format markdown
boundrail model view
```

Every edit names the revision it read and an operation ID, so a stale edit refuses and a retry is
safe. `model propose`, `compare` and `adopt` draft and adopt a future architecture; `model
inspect` reports each module's interface from the code; `model locality` reads Git history for
modules that change together; `model render` draws the model as SVG. `boundrail model --help`
lists every command. [Design notes](docs/plans/architecture-model.md).

A proposal carries a one-line `--summary` and linked `--document <title> <path-or-https-url>`
entries, shown as text and never fetched; an `edit-proposal` change renames it or sets and clears
them, retaining a checkpoint like any proposal edit. Every command that takes a proposal also takes
its name when no other proposal has it: an exact ID wins, and a name two proposals share refuses
`SUBJECT_AMBIGUOUS` with their IDs. A walkthrough may be written inside a proposal with `model edit
--proposal`; its beats read the proposal unless they name another state, `model compare` lists it,
`model render --proposal <id> --walkthrough <w> --beat <n>` draws a beat, and adoption adds it to
the current state's walkthroughs.

`model propose --parent <draft>` draws a child: a copy of the draft, which is its predecessor, so
it adopts only after its parent. `model proposals` and `model read --proposal` give each draft's
`standing`: `up-to-date`, `behind` (its predecessor moved since it was drawn or reconciled) or
`orphaned` (its parent was shelved, or adopted and the current state moved on). `model reconcile
<draft>` merges what its predecessor changed; `--family` goes on through its descendants, parents
first. A conflict settles with `--settle <subject>=mine|theirs|edited:<file>`; a merge that keeps a
part whose container the predecessor removed refuses `INHERITANCE_DROPPED`, one that keeps a
module the predecessor removed refuses `PART_RECREATED`, and one of a child whose parent was
shelved or is gone, which replaces what the parent changed, refuses `PARENT_CHANGES_DROPPED`,
until `--confirm <CODE>[=<subject>]`.

A relationship may describe its `traffic` (`protocol`, `sync` or `async`, `rate`, `payload`),
which reads, compares and draws after its label. A message step may name the code that sends it
in an `anchor` (`path` and `symbol`), which `model check` verifies like a relationship's. A flow
needs a one-line `summary` when it is added, and may list `participants`, the modules its
sequence diagram draws first, in order.

A module may list `references` to another model, state or subject: a model in another checkout
(a path inside this one, or `<alias>:` for a checkout named in the user's
`boundrail/checkouts.json`), a proposal or checkpoint, a module or flow there, and a `pin` to one
of its checkpoints. References are followed only when read, and only read: the other repository is
never written or fetched.

```sh
# Each reference followed: the other model's name, the subject's name and kind, the state read.
boundrail model read Billing --references
# What reference 0 points at, as `model read` prints it there.
boundrail model read Billing --reference 0
# Where Billing's code is on this machine; --open opens it.
boundrail model read Billing --code --open
```

### Rendering

`model render` draws the architecture, or a flow as a sequence diagram or flowchart. Arrows route
around the boxes they do not connect when a path exists, and otherwise take a plain bend that may
cross a box. Labels avoid each other and the boxes when there is room, and otherwise sit at the
middle of their arrow's middle segment. Labels are drawn over the arrows. The JSON receipt counts
both fallbacks, and a note on stderr names them. The same model always draws the same bytes.

```sh
# Billing and what it talks to, the rest dimmed, with a legend of the kinds drawn.
boundrail model render --focus Billing --legend --output billing.svg

# Leave out jobs; follow the viewer's light or dark color scheme.
boundrail model render --hide-kind job --theme auto --output model.svg

# A PNG at twice the size, cut to one region (x,y,w,h in picture units).
boundrail model render --png model.png --scale 2 --region 0,0,600,400

# A receipt: revision, width, height, the module and relationship IDs drawn, the SVG's digest,
# and how many arrows and labels fell back.
boundrail model render --format json

# Only the members of a configured group, and dimmed, the modules they talk to.
boundrail model render --group payments

# A saved view: a named reading kept in the model, drawn without any file to keep.
boundrail model render --saved-view "Payments path"
```

A reading, in a walkthrough beat, a picture or a saved view (`add-view`, `edit-view`,
`remove-view`; `model read --view <id>`), can also outline `selection` modules without dimming
anything, show only some relationship kinds (`filters.relationships`), keep only some groups'
members and their neighbours (`filters.groups`), draw configured groups as labelled regions behind
their members (`groups`: keys or `"all"`), and keep only what lies within `visibility`
relationship hops of its focus. These apply to the architecture view. The browser lists saved
views in the sidebar.

`--png` needs `resvg`, `rsvg-convert` or `magick` on PATH outside the checkout, and refuses
without one. A dark or auto theme in a PNG is only as dark as the rasterizer's CSS support makes
it.

### Recording a walkthrough

`model record` turns a walkthrough into an MP4: each beat's picture stays on screen while a local
voice reads its narration, then half a second more.

```sh
# Record Tour at 1280x720; writes tour.mp4 and the talk folder tour.talk/.
boundrail model record Tour --output tour.mp4 --voice Samantha --rate 180 --width 1280 --height 720

# Encode the MP4 again from the talk folder, without drawing or speaking.
boundrail model record --replay tour.talk --output tour-24fps.mp4 --fps 24
```

It needs a rasterizer (as `--png` does), a voice (`say` on macOS, `espeak-ng` or `piper`
elsewhere; `piper` takes its voice model file as `--voice`) and `ffmpeg`, all on PATH outside the
checkout. A missing tool refuses before anything is written, naming it in `details.reason`. The
talk folder holds each beat's PNG and audio and `talk.json` with each beat's title, narration and
length. Nothing goes over the network.

## Footprint

`boundrail model footprint` lists the modules a branch changes, the relationships its changed
imports cross and what it changes in the model, and marks a change of more than 30 files as
`large`. With `--proposal <id>` it sets those changes beside what the proposal names: modules
named and changed, changed but not named, and named but not changed. A changed module whose
changes all lie in changed parts lists those parts in `holds`. `--fail-outside` is the merge gate:

```sh
boundrail model footprint --base main --head ticket --proposal <id> --fail-outside
```

It exits 1 when a module the proposal does not name changed and holds nothing, naming each on
standard error, and 0 otherwise. Changed files that no module binds are listed but never fail
the gate. A ref or proposal it cannot read refuses with exit 2.

## Inspect

`boundrail model inspect <module>` reads a module's interface two ways. From the model alone it
lists the parts that outside relationships reach (`entry_points`), the parts none reaches
(`hidden_parts`), the outside modules relating to it (`callers`, `outgoing`), entry parts
without parts of their own that send outside and relate to nothing inside, so they only forward
(`pass_through`, not the `CONTAINER_PASS_THROUGH` diagnostic, which flags a container with
exactly one part), relationships drawn on the container instead of a part (`on_container`), and
five counts in `measures`. Relationships with a container above the module count nowhere. From
a scan of the checkout it counts entry files, exported and used symbols, imports past the entry
points and import edges per neighbour. Markdown prints the drawn interface only for a subject,
JSON carries every module's.

```sh
boundrail model inspect Billing --format markdown
boundrail model inspect Billing --checkpoint <id>   # a retained state; the code is not scanned
boundrail model inspect --group payments            # members, internal and boundary relationships, neighbours
```

## Compare

`boundrail model compare <proposal>` lists what a proposal adds, removes and changes against the
current state, another proposal (`--against <id>`) or a retained checkpoint
(`--against-checkpoint <id>`). `interface_changes` follows: for each module present in both
states whose drawn interface changed, the entry points added and removed, callers and outgoing
modules gained and lost, hidden and pass-through parts added and removed, and the five measures
before and after. `--beside <proposal>`, repeatable, compares alternatives against the same base:
`alternatives` gives each one's subject counts and number of interface changes, and
`interface_measures` sets every module's measures side by side, `null` where a state lacks it.
Markdown prints both as tables, the measures only for modules whose counts differ.

```sh
boundrail model compare <id> --format markdown
boundrail model compare <id> --against-checkpoint <checkpoint>   # against a retained state
boundrail model compare <a> --beside <b> --beside <c>            # rank alternatives
```

## History

Every write (an edit of the current state or a proposal, `propose`, `adopt`, `shelve`,
`reconcile`, `import`) retains a checkpoint on `refs/boundrail/history` with its `--doing` line,
writer and operation ID, so the history says why the architecture changed. A proposal's
checkpoints name it and read back as that proposal. `model retain --doing <why>` gives the
current state a checkpoint with a reason of its own, such as a release.

Each checkpoint is a Git commit, so every write runs a handful of Git commands and needs a Git
identity. With `commit.gpgSign` on, every write also invokes your signer, before it changes the
vault: a locked key refuses the write with nothing written. To stop signing checkpoints in one
repository, run `git config commit.gpgSign false` there; it stops signing your own commits there
too.

```sh
boundrail model checkpoints --format markdown       # newest first
boundrail model read --checkpoint <id>               # the model a checkpoint holds
boundrail model history configure --remote origin   # the remote the history is published to
boundrail model history push                        # fast-forward that remote's history
boundrail model history fetch                       # take its history, replaying local checkpoints
```

Each checkpoint reports `source_status`: `available` when this repository holds the commit it was
drawn against, `uncommitted` when the checkout also had changes outside `.boundrail` that the
commit lacks, and `unavailable` when no commit was recorded or this repository lacks it. Its
`publication` is `published` when the remote's history held it at the last push or fetch,
`pending` while a push holding it is unconfirmed, and `saved-locally` otherwise. Listing reads
local refs only, never the network. A write interrupted before its checkpoint landed refuses
`RECOVERY_REQUIRED`; `boundrail model recover` retains it once.

A checkpoint that recorded the wrong source commit is corrected, never rewritten: `model history
correct <checkpoint> --source <full commit hash> --doing <why>` appends a checkpoint naming it in
a `Corrects` trailer. Readings then show the corrected source, and the checkpoint's `corrections`
list who corrected it, when, why and the source before. To move a whole model, proposals and
history included, to a checkout without one, export it with its history and restore it there:

```sh
boundrail model export --with-history --output shop.bundle.json
boundrail model import --restore shop.bundle.json     # in a checkout with no vault and no history
```

### Private vaults

A vault can live outside every Git checkout, for a model the repository should not keep, such as
a pull request's temporary model. `model init --private <folder>` creates the vault in
`<folder>/.boundrail/model` and its history beside it in the bare repository
`<folder>/.boundrail/history.git`, so every write still retains a checkpoint, signed as
`commit.gpgSign` says. `--from <bundle>` starts it from a current-state or checkpoint bundle with
that model's IDs, so its proposals compare and render against the repository's state:

```sh
evidence=$(mktemp -d)
boundrail model export --output "$evidence/current.json"
boundrail model init --private "$evidence/model" --from "$evidence/current.json"
boundrail model --root "$evidence/model" propose --name "PR: queue" --doing "draw the PR" --operation-id pr-1
boundrail model --root "$evidence/model" compare <proposal> --format markdown
boundrail model --root "$evidence/model" render --proposal <proposal> --output "$evidence/after.svg"
boundrail model --root "$evidence/model" check --code .   # the code check reads this checkout
rm -rf "$evidence"
```

Every command that takes `--root` works there. `model check` and `model inspect` skip the code
(`no-checkout`) unless `--code <checkout>` names the code to read; `footprint` and `locality`
refuse `CHECKOUT_REQUIRED`, as do `history push` and `fetch` until a remote is added to the
private history with `git --git-dir <folder>/.boundrail/history.git remote add`. `init --private`
refuses a folder inside a checkout, where the checkout's vault applies, and a symbolic link; a
plain `model init` below a private folder refuses `CHECKOUT_NOT_FOUND`. Boundrail uses a private
history only where no folder above holds `.git`, so one committed into a repository is never
read, and only when the current user owns `.boundrail`, the history and the vault. A `.git`
created above the folder later absorbs the vault into that checkout: commands then warn and use
the checkout's vault, so move the folder out.

## Explain

`boundrail model explain` asks a local agent CLI about the model and prints its answer. Boundrail
writes the modules (with parts, relationships and drawn interface), the relationship, the flow's
steps and the proposal's comparison you name into a Markdown bundle, frames it as data rather than
instructions, and hands it with your question to `claude -p` (no tools, no MCP servers) or `codex
exec` (read-only sandbox) on stdin, run in an empty temporary folder. Prefer `claude`: the
read-only sandbox still lets `codex` read any file on disk and put it in its answer. The answer
goes only to you, on stdout or in the page. Pick the agent with `--agent`, or with `{"agent":
"codex"}` in `~/.config/boundrail/explain.json`; otherwise the first found on `PATH` runs. The
repository never chooses it. `--print` prints the bundle instead, to paste into any agent.

```sh
boundrail model explain Billing --question "Why does Billing call the bank directly?"
boundrail model explain --relationship <id> --flow Checkout --question "Is this step drawn right?"
boundrail model explain --proposal <id> --question "What does this proposal risk?" --print
```

`boundrail model view --allow-explain` lets the browser's `Ask the agent` field run the same
command; without the flag the field copies the command line for you to run.

An agent can show you a reading in the page you have open. `model view --show` takes the reading
flags of `model render` and sends them to the view running for this checkout, which switches the
active pane of the page you used last; `--panes` lists what every open page shows. They find the
view through a state file in `.git/boundrail`, readable by you alone, and send it a control token
the page never sees.

```sh
boundrail model view --show --flow Checkout --focus Billing
boundrail model view --show --walkthrough Tour --beat 2
boundrail model view --panes
```

In the page, Shift-click adds modules to the selection, the relationship chips hide a kind of
relationship, and the address keeps what the pane shows, so Back, Forward and a pasted link work.

## In CI

The GitHub Action runs `model check`, annotates its findings, and on a pull request comments the
branch's footprint:

```yaml
on:
  pull_request:
    types: [opened, synchronize, reopened, edited]
permissions:
  contents: read
  pull-requests: write # the footprint comment
jobs:
  boundrail:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: dogebonker/archgraph@main
        with:
          token: ${{ github.token }}
          # Fail when the branch changes a module outside the proposal its body's marker names,
          # and when the body has no marker.
          # gate: outside
          # A release that includes `boundrail model` (default: the action's Cargo.toml version),
          # or an executable you built:
          # version: <release>
          # binary: path/to/boundrail
```

The action needs a `boundrail` that includes `boundrail model`: set `version` to a release that
does, or `binary` to an executable you built. `fail-on` is `warnings` (the default),
`information` or `never`. The action never passes `--language-servers`, and passes
`--no-language-servers` so a user's config cannot turn them on, since language servers run code
from the checkout. Commenting is best effort: a token that cannot write leaves a notice
instead.

When the pull request body holds one line `<!-- boundrail:proposal <id> -->`, naming by its
8-character local ID a proposal committed on the branch, the footprint runs with `--proposal <id>`
and the comment adds the `Against proposal` lists. The token reads the body, which is untrusted:
any other ID, or a second marker, stops the step with exit 2. `gate` is `none` (the default) or
`outside`, which adds `--fail-outside`: the step fails when the branch changes a module the
proposal does not name, unless changed parts hold all of that module's changes. Under `outside`
the step fails closed (exit 2) on an unreadable body, a body with no marker, and any event but
`pull_request` (for example `pull_request_target` or `push`), so a gate that did not run never
reads as a pass. Under `none` a body without a marker runs as before. A marker line inside a
fenced code block does not count. Keep `edited` in the `pull_request` types so a marker added
later reruns the step.

The gate checks scope against the proposal the author chose: the marker and the proposal both
come from the pull request branch. It enforces a plan. It does not protect against a hostile
author, who can name a proposal that covers their change.

Checkpoint readings resolve only against a fetched history. To judge them, run `boundrail model
history fetch` in a step before the action, so `model check` sees `refs/boundrail/history`. The
history has one authoritative remote: push to and fetch from only that one.

## Language coverage

`model check`, `inspect` and `footprint` read the code with built-in tree-sitter collectors for
TypeScript and JavaScript, Python, Go, Rust, Java and Elixir. Go collection runs the Go
toolchain's `go list` for read-only package metadata. With `--language-servers`, `model check`
and `model inspect` add call-level references from allowlisted language servers named in
`.boundrail/language-servers.json` (`model setup --language-servers` writes a starter list).
The allowlist covers TypeScript, Python, Rust, Go, Java, C, C++ and Objective-C (`clangd`), C#
(`csharp-ls`) and Swift (`sourcekit-lsp`). `model language-server list`, `add <language>` and
`remove <language>` edit the repository's list, which only names allowlisted servers, and with
`--user` your own config (`~/.config/boundrail/language-servers.json`), the only place a program
or arguments can come from. Listing absolute folders in that config, as `{"trusted":
["/home/me/work"], "servers": [...]}`, turns the servers on without the flag for checkouts at or
below them, once links resolve; `--no-language-servers` turns them off for one run. Use them on
trusted checkouts only: the servers run code from the repository (clangd passes the flags of a
committed `.clangd` and `compile_commands.json` to its compiler, sourcekit-lsp builds the package
and runs its macros and plugins, csharp-ls runs MSBuild targets and NuGet restore), so never
list a folder you clone untrusted code into. A server whose project file is
missing (clangd without `compile_commands.json`, rust-analyzer without `Cargo.toml`) is skipped
as `language-server-unready`, and `language_server_runs` in the JSON output times each server's
start, initialize, queries and shutdown.

## Limits

The code check reads every language in the checkout and merges what each collector finds. Rust
needs a `Cargo.toml` and Go a `go.mod` or `go.work` somewhere in the checkout. The Python
collector reads one top-level package, under `src/` when it exists and else under the root, so
Python code outside it is skipped as `not-scanned`. When one language's collector fails, code of
that language alone is skipped as `scan-failed` under a `SCAN_FAILED` warning; a module whose
code mixes languages is judged on the languages that were read, and the warning names it too.

The scan leaves out, by default:

- tests: `tests/`, `test/`, `__tests__/`, `*.test.*`, `*.spec.*`, `*_test.go`, `*_test.exs`,
  `test_helper.exs`, `test_*.py`, `*_test.py`, `conftest.py`, `*Test.java`, `*Tests.java`, and
  in Rust `tests.rs`, `*_tests.rs`, `*_test.rs`, `test_support.rs` and `test_support/`;
- generated, vendored and build output: `generated/`, `__generated__/`, `*.generated.*`,
  `*.gen.*`, `*_pb.*`, `*_pb2.py`, `*_pb2_grpc.py`, `*.pb.go`, `*.pb.gw.go`, `zz_generated*.go`,
  `*.d.ts`, `dist/`, `build/`, `node_modules/`, `vendor/`, `.venv/`, and `target/` (Rust,
  Java), `out/` (Java), `_build/` and `deps/` (Elixir);
- support files: `testdata/`, `fixtures/`, `fixture/`, `__fixtures__/`, `stories/`, `story/`,
  `*.stories.*`, `*.story.*`, `examples/` and `example/`;
- paths the checkout's `.gitignore` files ignore, and those a `.archgraphignore` at the root
  ignores, in the same syntax.

Each pattern matches at any depth. Code bound under an excluded path is skipped as
`not-scanned`. The code evidence is **at least these relationships** within the
collected scope. Rust relationships describe source coupling, and Elixir macros and dynamic
references can hide dependencies. A finding is evidence to review, not proof: semantic changes
still need the project's build and tests.

[Changelog](CHANGELOG.md) | [Third-party notices](THIRD_PARTY_NOTICES.md).

## License

MIT or Apache-2.0, at your option.
