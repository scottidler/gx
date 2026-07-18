//! `catalog`: the read-only intel catalog (design doc
//! `docs/design/2026-07-17-gx-intel-catalog.md`, Track B1).
//!
//! This crate depends on `local` ONLY -- never `remote` -- so the cross-org
//! intel/operations boundary is compiler-structural: `catalog` cannot compile
//! a call to `persona`/`github`/`ssh`/remote-git because it has no path to
//! `remote` at all. A CI guard (`otto ci`'s `catalog-boundary` task, added
//! alongside the tools in a later phase) asserts this dependency never
//! reappears.

pub mod db;
pub mod walk;
