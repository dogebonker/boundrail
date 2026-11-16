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

A relationship may describe its `traffic` (`protocol`, `sync` or `async`, `rate`, `payload`),
which reads, compares and draws after its label. A message step may name the code that sends it
in an `anchor` (`path` and `symbol`), which `model check` verifies like a relationship's. A flow
needs a one-line `summary` when it is added, and may list `participants`, the modules its
sequence diagram draws first, in order.

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
```

`--png` needs `resvg`, `rsvg-convert` or `magick` on PATH outside the checkout, and refuses
without one. A dark or auto theme in a PNG is only as dark as the rasterizer's CSS support makes
it.

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

## In CI

The GitHub Action runs `model check`, annotates its findings, and on a pull request comments the
branch's footprint:

```yaml
- uses: actions/checkout@v7
  with:
    fetch-depth: 0
- uses: dogebonker/archgraph@main
  with:
    token: ${{ github.token }}
    # A release that includes `boundrail model` (default: the action's Cargo.toml version),
    # or an executable you built:
    # version: <release>
    # binary: path/to/boundrail
```

The action needs a `boundrail` that includes `boundrail model`: set `version` to a release that
does, or `binary` to an executable you built. `fail-on` is `warnings` (the default),
`information` or `never`. The action never passes `--language-servers`, since language servers
run code from the checkout. Commenting is best effort: a token that cannot write leaves a notice
instead.

Checkpoint readings resolve only against a fetched history. To judge them, run `boundrail model
history fetch` in a step before the action, so `model check` sees `refs/boundrail/history`. The
history has one authoritative remote: push to and fetch from only that one.

## Language coverage

`model check`, `inspect` and `footprint` read the code with built-in tree-sitter collectors for
TypeScript and JavaScript, Python, Go, Rust, Java and Elixir. Go collection runs the Go
toolchain's `go list` for read-only package metadata. With `--language-servers`, `model check`
and `model inspect` add call-level references from allowlisted language servers named in
`.boundrail/language-servers.json` (`model setup --language-servers` writes a starter list). Use
it on trusted checkouts only: the servers run code from the repository.

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
