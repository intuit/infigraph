# Contributing to Infigraph

Thank you for your interest in contributing!

## AI-assisted contributions

If you're using Claude Code, see [`CLAUDE.md`](CLAUDE.md) — architecture, build/test commands, and cross-cutting invariants, plus deeper guides under `.claude/skills/` (`code-indexing-pipeline`, `analysis-subsystems`, `review-pr-against-issue`). Cursor users get the same content via `.cursor/rules/*.mdc`, auto-loaded per file.

## Prerequisites

- Rust stable (via [rustup](https://rustup.rs/))
- `cmake` — required by the graph database
  - macOS: `brew install cmake`
  - Linux: `sudo apt install cmake`

## Building

```bash
cargo build --release -p infigraph-cli -p infigraph-mcp
```

## Running Tests

```bash
cargo test --all
```

## Code Style

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

Both are enforced in CI. Please run them before pushing.

## Adding a Language

**Tree-sitter path** — add a `tree-sitter-<lang>` crate dependency and write two query files:
- `crates/infigraph-languages/languages/<lang>/entities.scm` — symbols
- `crates/infigraph-languages/languages/<lang>/relations.scm` — call edges, imports

**Grammar plugin path** — for languages without a tree-sitter grammar, write ANTLR `.g4` grammars and a Java extractor. See `GRAMMAR_PLUGINS.md` for a full walkthrough.

## Submitting a PR

1. Fork the repo and create a branch from `main`
2. Write tests for your change
3. Ensure `cargo test --all`, `cargo fmt --all`, and `cargo clippy` all pass
4. Submit a pull request — the template will guide you through the checklist

## Reporting Bugs

Open an issue using the [bug report template](https://github.com/intuit/infigraph/issues/new?template=bug_report.md).

## License

By contributing, you agree that your contributions will be licensed under the [Apache 2.0 license](LICENSE).
