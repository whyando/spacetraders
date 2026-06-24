# Introduction

Knowledge base for the SpaceTraders trading agent (this repository).

It captures the *why* and the *how* behind the agent's subsystems — design,
decisions, and gotchas that aren't obvious from the code alone. Code-level
reference (types, function signatures) lives in `cargo doc`; this book covers the
narrative that ties it together.

## Viewing

This is an [mdBook](https://rust-lang.github.io/mdBook/). To read it as a
searchable site with live reload:

```sh
cargo install mdbook          # one-time
mdbook serve docs --open      # serves at http://localhost:3000
```

Or just read the Markdown sources under `docs/src/` directly.

## Contents

- [Architecture](architecture.md) — lifecycle/eras, fleet, ship scripts, the
  logistics planner, and the web/API surface.
- [T5 Trading](t5-trading.md) — dispatching refining freighters to trade the
  highest-value systems, including the P(T5) probability model.
