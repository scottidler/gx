//! `remote` -- the credential-bound half of gx (Track B0, Phase 3). Depends on
//! `local` for repo/git/file primitives; owns every module that talks to
//! ssh/persona/github or orchestrates a gx command (create/review/checkout/
//! clone/cleanup/undo/rollback/transaction/state/doctor/status/output/cli/mcp).
//! The `gx` bin is a thin shim over this crate.

pub mod app;
pub mod catalog;
pub mod checkout;
pub mod cleanup;
pub mod cli;
pub mod clone;
pub mod confirm;
pub mod crash;
pub mod create;
pub mod doctor;
pub mod git;
pub mod github;
pub mod lock;
pub mod mcp;
pub mod output;
pub mod persona;
pub mod review;
pub mod rollback;
pub mod ssh;
pub mod state;
pub mod status;
pub mod transaction;
pub mod undo;
