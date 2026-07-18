//! `local`: credential-free gx logic (repo/config/subprocess/hash/utils/bare/diff/user_org).
//!
//! This crate MUST NOT depend on `ssh`/`persona`/`github` or any remote-git
//! function. That boundary is what makes the intel-catalog cross-org non-goal
//! compiler-structural rather than conventional (Track B1, gx-intel-catalog
//! design doc). See `docs/design/2026-07-17-gx-lib-decomposition.md`.

pub mod bare;
pub mod config;
pub mod diff;
pub mod hash;
pub mod repo;
pub mod subprocess;
pub mod user_org;
pub mod utils;

// test_utils is used by ~30 test sites across STAYING gx modules' tests, not
// just this crate's own tests, so `#[cfg(test)]` alone (which does not cross
// a crate boundary) is insufficient. Gated behind the `testutil` feature so
// gx's [dev-dependencies] can enable it; also covered by `cfg(test)` so
// `local`'s own test target builds it without needing the feature.
#[cfg(any(test, feature = "testutil"))]
pub mod test_utils;
