---
name: boundrail-pr
description: Prepare and describe a pull request against the repository's Boundrail architecture model. Use when opening or reviewing a pull request, writing its description, checking which modules and relationships a branch touches, or retaining a model checkpoint after a merge.
license: MIT OR Apache-2.0
metadata:
  compatibility: Needs git and the boundrail binary on PATH. Model edits are in boundrail-model.
---

# boundrail-pr

A branch changes code, the model, or both. The **footprint** says which modules the branch
touches, which relationships its changed imports cross, and what it changes in the model.

## Steps

1. **Measure.** Run `boundrail model footprint --base <base branch> --format markdown`. Without
   `--base` it starts from the merge-base with `origin/HEAD`, `main` or `master`. The JSON form
   (no `--format`) has every key; `--format pr-comment` prints the comment CI posts.
2. **Read the crossings.** Each crossing joins two modules through a changed file. A crossing
   with no relationship is **undrawn**: draw the relationship (boundrail-model) or remove the
   import. `crossings_skipped` says why crossings were not checked: `not-needed` (nothing bound
   to code), `scan-failed` (see `scan_error`) or `head-not-checked-out`.
3. **Check.** Run `boundrail model check`. Done when it exits 0, or every warning left is named in
   the description with the reason.
4. **Describe.** Write an "Architecture" section in the pull request description:
   - the modules touched, from the footprint;
   - each relationship added, removed or changed, and why;
   - each undrawn crossing and what you decided about it;
   - the model changes (`model` in the footprint), or "no model change";
   - for a planned change, the proposal it implements and the picture from
     `boundrail model render --proposal <proposal>` (boundrail-plan).
   State only what the footprint and the check printed.
5. **After the merge.** On the default branch, retain the merged model as a checkpoint:
   `boundrail model retain --doing "<what changed and why>" --operation-id <operation-id> --expected-revision <revision.digest>`.
   It records the model on `refs/boundrail/history`, never on the working branch. On
   `NOTHING_TO_RETAIN` the model did not change since the last checkpoint: skip it. On
   `HISTORY_MOVED` another checkpoint landed first: retry. Then publish it with
   `boundrail model history push`. On `HISTORY_DIVERGED` run `boundrail model history fetch`
   first, then push again. List checkpoints with `boundrail model checkpoints`.
