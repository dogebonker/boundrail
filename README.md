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

The code check reads one language per checkout, the one detected at its root. Bound code in
another language is skipped as `not-scanned`, and the TypeScript and Python collectors read only
under `src/` when it exists.

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
