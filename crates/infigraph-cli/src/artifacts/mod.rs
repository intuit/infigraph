//! Data-driven integration artifacts: bundled config fragments, hook scripts,
//! and docs for every supported agent/editor, applied via one of five
//! strategies. See docs/superpowers/specs/2026-08-09-agent-target-templates-design.md.
//!
//! Built bottom-up (Tasks 1-12 of the implementation plan): most items here
//! are only exercised by this module's own unit tests until discovery
//! (Task 9) and dispatch (Task 11) wire them together, and until
//! `cmd_install`/`cmd_uninstall` (Tasks 18-19) call into the module from
//! `install.rs`. Suppressed here rather than per-item; remove once real
//! non-test callers make it unnecessary.
#![allow(dead_code, unused_imports)]

mod convention;
mod discovery;
mod manifest;
mod resolver;
mod step;
mod strategy;
mod template;

pub(crate) use discovery::{discover_artifacts, ResolvedArtifact};
pub(crate) use step::InstallStep;
pub(crate) use strategy::{ApplyOutcome, Strategy};

include!(concat!(env!("OUT_DIR"), "/bundled_integrations.rs"));
