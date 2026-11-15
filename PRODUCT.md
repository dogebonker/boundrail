# Product

<!-- impeccable:product-schema 1 -->

## Platform

CLI, with a local read-only browser view

## Users

Developers and coding agents who describe a repository's architecture, plan changes to it, and
check that the code still matches. They need source evidence and repeatable checks.

## Product Purpose

Boundrail keeps an authored architecture model in `.boundrail/model/`, committed with the code:
modules bound to code, relationships, collections, flows, walkthroughs, proposals and checkpoints.
`boundrail model check` compares the model with the code, and `boundrail model footprint` shows
what a branch changes. The browser view (`boundrail model view`) makes the model explorable.

## Positioning

A model written by people and agents, checked against local source evidence, without an
application SDK or running the application. Code evidence is static and incomplete; the model is
the stated intent.

## Operating Context

Rust CLI with JSON, Markdown and SVG output, a local browser view on the loopback interface, a
GitHub Action that annotates `model check` findings and comments the footprint, and agent skills
that `model setup` installs. Language servers run only on explicit opt-in, on trusted checkouts.

## Capabilities and Constraints

- Built-in collectors read TypeScript and JavaScript, Python, Go, Rust, Java and Elixir; one
  language per checkout.
- Every edit names the revision it read and an operation ID; a stale edit refuses.
- Source locations, skips and limitations stay visible in every output.
- The view reads only, works offline and loads nothing from other origins.

## Brand Commitments

The product is Boundrail and its executable is `boundrail`. Use direct technical copy.
Distinguish the authored model from code evidence. The repository URL, `.archgraphignore` and the
`ARCHGRAPH_*` install variables keep their earlier names for compatibility.

## Product Principles

- Show the source location behind a finding.
- Never let code evidence silently rewrite the authored model.
- Refuse rather than guess: stale revisions, unknown subjects and invalid requests fail loudly.

## Accessibility & Inclusion

The browser view supports keyboard focus, light and dark schemes, narrow screens and
reduced-motion preferences. The sidebar and detail panel list in text what the drawing shows.
