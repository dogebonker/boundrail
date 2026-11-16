---
name: boundrail-model
description: Author the repository's architecture model in .boundrail/model through the boundrail CLI when reading the architecture, changing modules, relationships, flows, walkthroughs, pictures or saved views, binding them to code, finding refactor candidates, or reacting to a `model check` warning or refusal code.
license: MIT OR Apache-2.0
metadata:
  compatibility: Needs git and the boundrail binary on PATH. Plans and proposals are in boundrail-plan; pull requests in boundrail-pr.
---

# boundrail-model

The model is a set of JSON files in `.boundrail/model/`, committed with the code. Boundrail owns
every file there: change it only with `boundrail model edit`, never by writing the files. Every
command prints JSON; add `--format markdown` for a readable summary. A refusal prints JSON with
`"kind": "refusal"` and `error.code`, and exits 2.

## Steps

1. **Discover.** Run `boundrail model read --format markdown`. On `VAULT_NOT_FOUND`, run
   `boundrail model setup` (it creates the vault and changes nothing else without `--agent`).
   Done when you know the modules, their parts and relationships, and the flows.
2. **Read what you change.** `boundrail model read <subject>` prints one module with its parts and
   relationships; a subject is a qualified ID (`<model>#<id>`), a local ID, or a unique current or
   former name. Use `--flow <flow>`, `--walkthrough <id>`, `--picture <id>` or `--view <id>` for
   those.
3. **Check first.** Run `boundrail model check --format markdown` so you know which warnings were
   there before your change. Exit 1 means at least one warning.
4. **Write the request.** Print the request schema with `boundrail model schema model-edit`; list
   every schema with `boundrail model schema`. Copy the schema, not an example from memory. Fill:
   - `target.model`: `manifest.id` from `model read`.
   - `target.state`: `manifest.current.id`.
   - `expected_revision`: `revision.digest` from the same read. This is the **revision** you
     read; the edit refuses when the model moved since.
   - `operation_id`: a fresh ID (letters, digits, `.`, `_`, `-`) for this change. Reusing it with
     the same request returns the same receipt, so a retry after a timeout is safe.
   - `writer`: your agent name. `claim`: your claim ID, when you hold one.
   - `changes`: one **batch**, applied in order, all or nothing. A later change can name a module
     an earlier change in the batch added.

   A request that adds one module:

   ```json
   {
     "target": {"model": "<manifest.id>", "state": "<manifest.current.id>"},
     "expected_revision": "<revision.digest>",
     "operation_id": "add-billing-1",
     "writer": "agent",
     "changes": [
       {
         "type": "add-module",
         "module": {"name": "Billing", "kind": "service", "order": 0, "binding": {"path": "src/billing"}}
       }
     ]
   }
   ```
5. **Apply.** `boundrail model edit --doing "<why, one line>" --input <request.json>` (or the
   request on standard input). The receipt's `allocated` lists the new IDs by name, and its
   `checkpoint` is the history entry the edit retained with your `--doing` line, so write that
   line for a reader asking later why the model changed.
6. **Verify.** Run `boundrail model check` again. Done when it reports no warning your change
   added, and every warning you leave is named in your reply with the reason.

## Evidence rules

- Bind a module to its code with `binding: {path}` (a folder or file, repository-relative), plus
  `symbol` for one exported name. A container's binding holds its parts' bindings.
- A relationship of kind `call` or `render` needs import or call evidence. Other kinds are
  supported by evidence when the code shows it and skipped otherwise. An `anchor` (`path` and
  `symbol`) pins a relationship to the code site that carries it. `traffic` (`protocol`, `sync`,
  `rate`, `payload`) says what runs over it.
- `boundrail model read <module> --code` prints where the module's code is on this machine
  (`code_target.local_path`); `--open` opens it (on Windows and Linux, its folder). A binding
  with a `repo` resolves only through the user's `boundrail/checkouts.json`
  (`[{"name": "<repo>", "path": "<absolute path>"}]`).
- A module's `references` point at another model, state or subject: each is `{model?, state?,
  subject?, pin?}`, where `model` is a path inside the checkout or `<alias>:` with an optional
  path, `state` is `current`, `{"proposal": <id>}` or `{"checkpoint": <id>}`, `subject` a
  qualified ID there, and `pin` the full commit of a checkpoint there. Set the whole list with
  `edit-module` `set.references`, remove it with `clear: ["references"]`. `model read <module>
  --references` follows each and says why one is skipped; `--reference N` reads one. A reference
  to a vault that is not there yet is allowed.
- Draw what the code does. When the code and the model disagree, decide which one is wrong and
  say so; never draw a relationship only to silence a warning.
- `model check --language-servers` runs language servers from the checkout. Use it only on a
  checkout you trust; pass `--no-language-servers` on any other, since the user's config turns
  them on for every checkout below a folder it lists in `trusted`. Never add a folder to
  `trusted` yourself.
- `model language-server list` shows the repository's list, the user's config and the server
  each language would run. `model language-server add <language>` names the language's
  allowlisted server in `.boundrail/language-servers.json` (`c`, `csharp`, `go`, `java`,
  `python`, `rust`, `swift`, `typescript`); a program or arguments go only in the user's config,
  with `--user`. Tell the user when you add one: the servers run code from the checkout.
- `language-server-unready` means the project lacks a file the server needs, named in the
  reason: `compile_commands.json` for clangd (CMake writes it with
  `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`), `Cargo.toml`, `go.mod`, a Maven or Gradle build file, a
  `.sln` or `.csproj`, or `Package.swift`.

| Warning | What to do |
|---|---|
| `RELATIONSHIP_UNDRAWN` | The code uses a module no relationship covers. Draw the relationship, or report the import as unwanted. |
| `RELATIONSHIP_UNSUPPORTED` | No code evidence. Remove the relationship, fix its kind, or bind the modules. |
| `RELATIONSHIP_BACKWARDS` | The evidence runs the other way. Reverse it, or fix the code. |
| `BINDING_PATH_MISSING`, `ANCHOR_PATH_MISSING` | The path does not exist. Fix the path. |
| `BINDING_SYMBOL_MISSING`, `ANCHOR_SYMBOL_MISSING` | No file there declares the symbol. Fix the symbol. |
| `CONTAINER_UNBOUND` | Bind the container; `suggested_binding` names the shared folder. |
| `ROOT_UNCONNECTED` | Draw how the module relates to the rest, or remove it. |
| `SCAN_FAILED` | Read the message, fix the scan, and check again. |

Other codes explain themselves in their message. `skips` list what the check could not judge.

## Flows, walkthroughs, pictures and views

- A flow lives in a collection: `add-collection`, then `add-flow` with its steps. A message step
  names two modules; `MESSAGE_UNDRAWN` means no relationship covers it. Every `add-flow` needs a
  one-line `summary` saying what the flow does. Put the sending code in a message's `anchor`, and
  list a flow's `participants` to fix the order of its sequence diagram.
- A walkthrough is ordered beats, each with a narration and a reading (what to draw). Preview a
  beat with `boundrail model render --walkthrough <id> --beat <n> --output <file.svg>`. A
  walkthrough may also live in a proposal (`model edit --proposal`); its beats read the proposal
  unless `state` names another, preview with `--proposal <proposal>` beside `--walkthrough`, and
  adoption adds it to the current state's walkthroughs.
- Record a walkthrough with a local voice: `boundrail model record <id> --output <file.mp4>`. It
  needs a rasterizer, `say`, `espeak-ng` or `piper`, and `ffmpeg` on PATH; `details.reason` names
  a missing one. `--replay <file>.talk --output <file.mp4>` encodes it again without the model.
- A picture is a reading with an `output` path (`.svg` or `.png`) outside `.boundrail`. Draw stale
  pictures with `boundrail model pictures --write`; `model check --only embeds` lists stale ones.
- A saved view is a named reading with no file (`add-view`, `edit-view`, `remove-view`). Draw it
  with `boundrail model render --saved-view <id>`; the browser lists it in the sidebar.
- In the architecture view a reading may also set `selection` (modules outlined, nothing
  dimmed), `filters.relationships` (only these relationship kinds), `filters.groups` (only these
  groups' members, their neighbours dimmed), `groups` (group keys or `"all"`, drawn as labelled
  regions) and `visibility` (keep only what lies within that many hops of the focus; needs a
  focus). Kinds and groups are `config.yaml` keys.
- See the model in a browser with `boundrail model view`. While the user has it open, show them
  what you are talking about with `boundrail model view --show` and the `model render` reading
  flags (`--flow`, `--focus`, `--scope`, `--proposal`, `--saved-view`, `--walkthrough --beat`);
  `--panes` lists what their panes show. `details.reason` `no-view` means no view runs for this
  checkout; then point the user at `boundrail model view` or draw with `model render`.
- When the user wants another agent's reading of part of the model, `boundrail model explain
  <module>... [--relationship <id>] [--flow <id>] [--proposal <id>] --question "<text>"` asks
  their local agent CLI; `--print` prints the bundle instead. It writes nothing.

## History

Every write retains a checkpoint; you do not retain after an edit. To mark the current model for
a reason of its own (a release, a review), run `boundrail model retain --doing "<why>"`.
`boundrail model checkpoints` lists them newest first; `boundrail model read --checkpoint <id>`
reads one. `source_status` says whether the code it was drawn against is here (`available`),
was mixed with uncommitted changes (`uncommitted`), or is missing (`unavailable`). `publication`
says whether the history remote holds it. Publish only when the user asks: `boundrail model
history push` (the remote is `boundrail model history configure --remote <name>`, else `origin`).

## Design

To improve the architecture, find candidates in the measures before you draw a plan. A **deep**
module hides much behind few entry points; a **shallow** one lets callers reach its parts.

- `boundrail model inspect <module> --format markdown` reads the interface the model draws:
  - many `entry_points` beside few `hidden_parts`: shallow. Route callers through one part and
    hide the rest.
  - a `pass_through` part only forwards from outside to outside. Join its two ends directly, or
    move real work into it.
  - `on_container` relationships belong on the part that does the work.
  - from the scan: `unused` exports are candidates to make internal; `deep_imports` reach past
    the entry points and want an entry file.
- `boundrail model locality --format markdown` reads co-change from Git history. A hidden coupling
  (modules changed together in 3 or more commits with no relationship drawn) is either a
  relationship the code shows and the model lacks (draw it) or one concern split across modules
  (merge them, or move the code). A low `local_share` means most changes to the module touch
  others too.
- Draw each candidate design as its own proposal (boundrail-plan; `--from` copies another draft),
  then `boundrail model compare <a> --beside <b> --format markdown`. It sets each alternative's
  counts and per-module measures side by side and ranks nothing: pick by the counts the design
  question names, and show the person the table and each proposal's picture.

## Claims

When several writers share the model, reserve what you change first:
`boundrail model claim --doing "<why>" --input <claim.json>` (schema `model-claim`), then name the
claim ID in every edit. List claims with `boundrail model claims`. Release yours with
`boundrail model release --input <release.json>` (schema `model-release`) when you finish.

## Refusals

| Code | Reaction |
|---|---|
| `STALE_REVISION` | Read the model again, rebuild the batch on what you read, and send it with a new `operation_id`. `details.current` is the revision now. |
| `MODEL_INVALID` | The batch would break the model. `details.problems` names each file, subject and change; fix the batch. |
| `REQUEST_INVALID` | The request does not match the schema or the flags. Read the message and the schema. |
| `SUBJECT_NOT_FOUND`, `SUBJECT_AMBIGUOUS` | Name the subject by its ID. |
| `TARGET_MISMATCH` | `target` names another model or state. Copy it from `model read`. |
| `CHILDREN_ORPHANED` | Remove or reparent the parts in the same batch. |
| `REFERENCED_BY_FLOW`, `REFERENCED_BY_WALKTHROUGH`, `REFERENCED_BY_PICTURE`, `REFERENCED_BY_VIEW` | Edit or remove the steps, flows, beats, pictures or views in `details` in the same batch. |
| `FLOW_SUMMARY_REQUIRED` | Give the flow a one-line `summary` that says what it does; `edit-flow` cannot clear it. |
| `OPERATION_REUSED` | That `operation_id` applied a different request. Use a new one. |
| `CLAIM_CONFLICT` | Another writer holds a subject. Wait for the release, or ask a person; revoke only when a person says so. |
| `CLAIM_INCOMPLETE` | Extend your claim to every subject the batch touches. |
| `CLAIM_NOT_HELD`, `CLAIM_REVOKED` | Release it and claim again. |
| `VAULT_LOCKED` | Another write is running. Retry when it finishes. |
| `DOING_INVALID` | Give `--doing` one line of 1 to 140 characters. |
| `NO_CURRENT_STATE` | A planning model: work in a proposal (boundrail-plan). |

## Recovery

- `RECOVERY_REQUIRED`: a write was interrupted. Run `boundrail model recover`, which finishes it,
  then read the model again. When `details.cause` is set, the write landed but its checkpoint did
  not: fix the cause it names (for `SIGNING_FAILED`, ask the user to unlock the signing key),
  then recover.
- `RECOVERY_CONFLICT`: vault files changed after the write was planned. Restore them from Git, or
  drop the write with `boundrail model recover --abandon <operation_id>` and plan it again.
- `OPERATION_ABANDONED`: that operation was dropped. Plan the change again with a new ID.
