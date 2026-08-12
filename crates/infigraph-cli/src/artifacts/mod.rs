//! Data-driven integration artifacts: bundled config fragments, hook scripts,
//! and docs for every supported agent/editor, applied via one of five
//! strategies. See docs/superpowers/specs/2026-08-09-agent-target-templates-design.md.

mod convention;
mod discovery;
mod manifest;
mod resolver;
mod step;
mod strategy;
mod template;
