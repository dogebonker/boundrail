---
name: boundrail-plan
description: Plan an architecture change in a Boundrail proposal before the code changes. Use when designing a future architecture, comparing options, drawing what a plan adds, removes and changes, slicing a plan into tickets, gating a ticket's branch against its plan, adopting a plan as the current model, or shelving it, and on a PREDECESSOR_MOVED refusal.
license: MIT OR Apache-2.0
metadata:
  compatibility: Needs git and the boundrail binary on PATH. Request rules, claims and refusal codes are in boundrail-model.
---

# boundrail-plan

A proposal is a draft of the model in `.boundrail/model/proposals/<proposal>/`. It starts as a copy of
the current state (its **predecessor**) and changes only through `model edit --proposal`. The
current state stays as it is until you adopt the proposal. Follow boundrail-model for requests,
claims and refusals.

## Steps

1. **Draw.** One spec gets one proposal. `boundrail model propose --name "<plan>" --doing "<why>" --operation-id <operation-id>`. Add
   `--from <proposal>` to copy another draft instead of the current state, `--parent <draft>` to
   draw a child that builds on a draft (its predecessor is the draft, and it adopts only after
   it), `--summary` for one
   line saying what it changes, `--description` for the intent, and `--document "<title>"
   <path-or-https-url>` (repeatable) to link the spec. The receipt's `allocated[0].id` is the
   proposal's qualified ID; its local ID is the part after `#`. Every command that takes a
   proposal also takes its name when no other proposal has it; on `SUBJECT_AMBIGUOUS` use an ID
   from `details.matches`.
2. **Edit.** Read it with `boundrail model read --proposal <proposal>`. Write a `model-edit` request with
   `target.state` set to the proposal's local ID and `expected_revision` set to that read's
   `revision.digest`, then run
   `boundrail model edit --proposal <proposal> --doing "<why>" --input <request.json>`.
   An `edit-proposal` change renames it or sets and clears its description, summary and
   documents: `{"type": "edit-proposal", "set": {"summary": "<line>"}}`. Walkthroughs may live in
   the proposal; pictures and saved views belong to the current state and a proposal edit
   refuses them.
3. **Check.** `boundrail model check --proposal <proposal> --format markdown`. Done when the plan adds no
   warning you cannot explain.
4. **Compare.** `boundrail model compare <proposal> --format markdown` lists what the plan adds, removes
   and changes against the current state; `--against <other>` compares two drafts.
5. **Draw the difference.** `boundrail model render --proposal <proposal> --output plan.svg` draws the
   plan with comparison marks: added green, changed amber, removed red. `--against <other>` draws
   it against another draft, `--flow <flow>` draws one flow, `--scope <module>` and `--depth <n>`
   narrow the architecture view. Show the picture to the person deciding.
6. **Decide.** A person decides whether the plan is adopted. Ask before adopting.

## Slice into tickets

A spec too big for one pull request lands as **slices**, one per ticket. A slice is a small set
of the modules and relationships the proposal names; every subject `model compare` lists belongs
to exactly one slice. Commit the proposal folder before the first ticket, so every ticket's
branch holds it.

1. **Record the slices.** Write each slice's ticket and subjects in the proposal's description
   (`--description`, or later `edit-proposal`), or as a walkthrough inside the proposal:
   `add-walkthrough` in a `model edit --proposal` request, one beat per slice, `against`
   `"current"`, `focus` the slice's modules, title naming the ticket. A beat there reads the
   proposal unless its `state` names another. Done when every subject `model compare` lists sits
   in one slice.
2. **Gate each ticket.** On the ticket's branch run
   `boundrail model footprint --proposal <proposal> --fail-outside --format markdown`. Exit 1 names the
   modules the branch changes outside the proposal. For each, decide:
   - the change may belong to the plan: ask the person before any edit. The gate compares the
     branch with the proposal, so editing the proposal until it passes makes the gate agree with
     the code and checks nothing. With the person's yes, edit the proposal to name it, with the
     deviation in the doing line:
     `boundrail model edit --proposal <proposal> --doing "deviation <ticket>: <why>" --input <request.json>`.
     List each deviation under Merge danger in the pull request body (boundrail-pr);
   - it does not: move it to its own branch (boundrail-pr, Split).

   Done when the gate exits 0. Put the proposal's marker in the pull request body
   (boundrail-pr) so CI runs the same gate.
3. **Adopt with the last slice.** When the last slice's pull request merges, adopt the proposal
   on the default branch (Adopt, after a person agrees). Slices that edited the current state to
   keep `model check` clean make adopt refuse `PREDECESSOR_MOVED`: reconcile first.

## Adopt

Adopt only the revisions you compared. Read both revisions, then run:

```sh
boundrail model adopt <proposal> \
    --expected-revision <current state revision.digest> \
    --expected-proposal-revision <proposal revision.digest> \
    --doing "<why>" --operation-id <operation-id>
```

The proposal's files replace the current state's and its walkthroughs join the current state's;
the output's `revision` is the current state's new revision. The adopted proposal stays, reading
only. On the default branch, retain and
publish the adopted model as boundrail-pr says under After the merge.

## Shelve

Set a plan aside with
`boundrail model shelve <proposal> --expected-revision <proposal revision.digest> --doing "<why>" --operation-id <operation-id>`.
It stays, reading only. List drafts and their status with `boundrail model proposals`.

## Families

`boundrail model proposals` lists each draft's `parent`, `children` and `standing`:
`up-to-date`, `behind` (its predecessor moved: reconcile it) or `orphaned` (its parent was shelved,
or adopted and the current state moved on: reconcile it onto the current state, which takes it out
of the family). After changing a parent, reconcile it and its descendants in one command:

```sh
boundrail model reconcile <parent> --family \
    --expected-revision <current revision.digest> \
    --expected-proposal-revision <parent revision.digest> \
    --doing "<why>" --operation-id <operation-id>
```

Each descendant is its own write with operation ID `<operation-id>.<its local ID>`, so the
operation ID needs room for a dot and an 8-character local ID within the operation ID limit;
`REQUEST_INVALID` says how long it may be. Only `--confirm <CODE>=<subject>` reaches the
descendants; `--confirm <CODE>` covers the named draft alone. A refusal stops the walk and `details.family` lists the drafts done
before it; settle that draft on its own, then run `--family` from it. Adopt a parent before its
children.

## Settle conflicts

`RECONCILE_CONFLICT` lists `details.conflicts`. Read both sides (`model read --proposal`, and
the parent or the current state), ask the person which side each subject keeps, then reconcile
again with one `--settle` per listed subject: `<subject>=mine` keeps the draft's side,
`<subject>=theirs` takes the predecessor's, and `<subject>=edited:<file>` takes the module file
(with `outgoing` empty: relationships settle on their own), relationship or collection JSON in
the file. A `retired` conflict never settles: edit the draft to drop that ID.

## Move a plan between clones

`boundrail model export --proposal <proposal> --output plan.bundle.json` writes the draft as one
file. In the other clone,
`boundrail model import plan.bundle.json --expected-revision <current revision.digest> --doing "<why>" --operation-id <operation-id>`
draws it as a new draft there.

## Refusals

Other codes react as boundrail-model says.

| Code | Reaction |
|---|---|
| `PREDECESSOR_MOVED` | The proposal's predecessor changed since it was drawn. Run `boundrail model reconcile <proposal> --expected-revision <current revision.digest> --expected-proposal-revision <proposal revision.digest> --doing "<why>" --operation-id <operation-id>`, then compare and adopt again. On `RECONCILE_CONFLICT`, settle each listed subject (Settle conflicts) or edit the draft to match one side, and reconcile again. |
| `PARENT_NOT_ADOPTED` | The proposal is a child of a draft. Adopt `details.parent` first, then this one. |
| `INHERITANCE_DROPPED` | The merge keeps parts (`details.modules`) whose container the predecessor removed. With the person's yes, reconcile again with `--confirm INHERITANCE_DROPPED` (or `=<module>` for one): each moves to `lifted_to`, the nearest container left, or the root. Otherwise edit the draft. |
| `PARENT_CHANGES_DROPPED` | The draft's parent was shelved or is gone, and merging it onto the current state replaces the parent's version of `details.subjects`. With the person's yes, reconcile again with `--confirm PARENT_CHANGES_DROPPED` (or `=<subject>` for one); otherwise edit the draft to keep what it needs. |
| `PART_RECREATED` | The merge keeps modules (`details.modules`) the predecessor removed, from a settlement as `mine` or `edited`. With the person's yes, reconcile again with `--confirm PART_RECREATED` (or `=<module>`); otherwise settle them as `theirs`. |
| `PROPOSAL_CLOSED` | The proposal is adopted or shelved. Draw a new one. |
| `STALE_REVISION` on adopt | Either revision moved. Read both again and compare again before adopting. |
| `MODEL_INVALID` on adopt | A beat or picture reads a module or flow the plan drops. Edit it first. |
| `HISTORY_DIVERGED` on `history push` | With `details.related: false` or `details.clashing`, stop and report the message: no fetch converges those. Otherwise run `boundrail model history fetch`, then push again. |
