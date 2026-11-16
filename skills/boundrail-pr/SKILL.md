---
name: boundrail-pr
description: Prepare and describe a pull request against the repository's Boundrail architecture model. Use when opening or reviewing a pull request, writing its body, checking which modules and relationships a branch touches, gating it against its proposal, splitting a branch that changes too much, or retaining a model checkpoint after a merge.
license: MIT OR Apache-2.0
metadata:
  compatibility: Needs git and the boundrail binary on PATH. Model edits are in boundrail-model; proposals and slices in boundrail-plan.
---

# boundrail-pr

A branch changes code, the model, or both. The **footprint** says which modules the branch
touches, which relationships its changed imports cross, and what it changes in the model. Every
pull request names a **proposal**: the plan's, or a temporary model drawn for it, by default in a
private vault outside the repository.

## Steps

1. **Measure.** Run
   `boundrail model footprint --base <base branch> --proposal <proposal> --fail-outside --format markdown`.
   Without `--base` it starts from the merge-base with `origin/HEAD`, `main` or `master`. The JSON
   form (no `--format`) has every key; `--format pr-comment` prints the comment CI posts. With no
   proposal in the repository, run it without `--proposal` and `--fail-outside`, then draw the
   temporary model (Temporary model): the gate does not apply to it.
2. **Read the crossings.** Each crossing joins two modules through a changed file. A crossing
   with no relationship is **undrawn**: draw the relationship (boundrail-model) or remove the
   import. `crossings_skipped` says why crossings were not checked: `not-needed` (nothing bound
   to code), `scan-failed` (see `scan_error`) or `head-not-checked-out`.
3. **Gate.** Exit 1 names the modules changed outside the proposal; `Against proposal` lists
   them under `Changed, not named`. A module whose changes all lie in changed parts shows
   `held by` those parts and never fails. Name each outside module in the proposal (boundrail-plan,
   Gate each ticket, after asking the person) or split it off (Split). Done when the gate exits 0.
4. **Check.** Run `boundrail model check`. Done when it exits 0, or every warning left is named in
   the body with the reason.
5. **Compare.** Run `boundrail model compare <proposal> --format markdown` for the interface
   changes, and `boundrail model render --proposal <proposal> --output <file.svg>` for the picture.
6. **Write the body** from the template below. State only what the footprint, check and compare
   printed.
7. **After the merge.** On the default branch, retain the merged model as a checkpoint:
   `boundrail model retain --doing "<what changed and why>" --operation-id <operation-id> --expected-revision <revision.digest>`.
   It records the model on `refs/boundrail/history`, never on the working branch, beside the
   checkpoints every edit already retained. On `NOTHING_TO_RETAIN` the latest checkpoint already
   holds this model with the same reason: skip it. On
   `HISTORY_MOVED` another checkpoint landed first: retry. Then publish it with
   `boundrail model history push`. On `HISTORY_DIVERGED` with `details.related: false` or
   `details.clashing`, stop and report the message: no fetch converges those. On any other
   `HISTORY_DIVERGED` run `boundrail model history fetch`, then push again. List checkpoints with
   `boundrail model checkpoints`. Shelve a temporary model committed on the branch now.

## Body

The body holds one **state marker** line naming the proposal's local ID (the 8 letters or digits
after `#`) in exactly the form below. The CI action reads it to run the footprint against that
proposal, and with `gate: outside` fails the pull request on a module outside it. A marker it
cannot read stops CI, so replace the placeholder. A temporary model in a private vault is not in
the repository, so leave the line out for it.

```markdown
<!-- boundrail:proposal <proposal local ID> -->

## Summary

<What the branch does and why, in two or three lines. The ticket, and the slice of the proposal
it lands, or "temporary model".>

## Evidence

- Footprint: <modules touched; named and changed, changed but not named (with holds), named but
  not changed; each undrawn crossing and the decision; "large" when it is>
- Check: <exit status; each warning left and why>
- Interface changes: <per module from `model compare`: entry points, callers and outgoing
  modules gained or lost; or "none">
- Picture: <link to the rendered SVG>

## Merge danger

<What a reviewer must judge before merging: modules changed outside the proposal, undrawn
crossings kept, warnings left, a large change, callers gained by a module, a proposal whose
current state moved (`PREDECESSOR_MOVED`), each deviation edit to the proposal, a temporary
model drawn after the code. Or "none found".>
```

## Temporary model

A pull request outside any plan still gets a picture of what it changes. Draw it in a private
vault outside the repository, so nothing is committed and the repository's model and history
stay as they are.

1. Ask the person before drawing it: the model is drawn after the code, from the footprint, so
   it explains the change and judges nothing.
2. Start a private vault from the repository's current state, keeping its module IDs:

   ```bash
   evidence=$(mktemp -d)
   boundrail model export --output "$evidence/current.json"
   boundrail model init --private "$evidence/model" --from "$evidence/current.json"
   ```

   `init --private` refuses a folder inside a Git checkout, so keep it under `mktemp -d`. Every
   command below takes `--root "$evidence/model"`. Each write retains a checkpoint in its private
   history, `$evidence/model/.boundrail/history.git`, which needs a Git identity: when Boundrail
   reports none, set `user.name` and `user.email` with
   `git --git-dir "$evidence/model/.boundrail/history.git" config`.
3. Draw a proposal there and edit it (boundrail-plan, Edit) to draw what the branch does: the
   modules the footprint touches and the relationships its crossings need. `propose` takes
   `--root "$evidence/model"`, a name such as `"PR: <title>"`, `--doing` and `--operation-id`.
4. Compare and picture it against the repository's state:
   `boundrail model --root "$evidence/model" compare <proposal> --format markdown`,
   `boundrail model --root "$evidence/model" render --proposal <proposal> --output "$evidence/after.svg"`,
   and `boundrail model --root "$evidence/model" check --code .` to check it against the
   branch's code. `footprint` and `locality` refuse `CHECKOUT_REQUIRED` there: run them in the
   repository.
5. Upload the picture to the body, write `temporary model` in Summary and no state marker, and
   state under Merge danger that the model was drawn after the code.
6. Delete the folder: `rm -rf "$evidence"`. The repository holds nothing of it.

Alternative: when CI must gate the pull request against the temporary model, commit it instead.
Draw it with `boundrail model propose --name "PR: <title>" --description "Temporary model for one
pull request" --doing "<why>" --operation-id <operation-id>` in the repository, edit, compare and
render it (Steps 5), commit its folder on the branch and put its marker in the body. After the
merge, shelve it on the default branch (boundrail-plan, Shelve): the current state now carries
the change.

## Split

A branch splits when the gate names a module the plan does not want, or the footprint says
`large`.

1. Group the changed files by module from the footprint JSON (`modules[].files`).
2. Create a new branch from the same base holding the files of each outside module, or each
   group past the first, and open a pull request for it with its own proposal or temporary
   model. On this branch revert those files in a new commit. Never force-push a shared branch.
3. Measure every branch again. Done when each gate exits 0 and no branch is `large` without a
   reason in its Merge danger.
