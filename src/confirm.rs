//! The confirmation seam every mutating core function takes instead of doing
//! its own TTY prompt (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, API Design > Core
//! signatures). A CLI wrapper prompts (or honors `--yes`) BEFORE calling a
//! core function and always passes [`Confirmation::AlreadyConfirmed`]; an MCP
//! caller instead passes back the token from a prior plan/propose call.
//!
//! Phase 3 introduces the type and threads it through every split core
//! (`create::core::execute_create`, `undo::core::execute_undo`,
//! `rollback::core::execute_recovery`) without giving `Token` real teeth: none
//! of those three flows persists a hashable plan/manifest yet, so a CLI
//! wrapper that already confirmed always passes `AlreadyConfirmed`. `Token`
//! exists now so every core signature is stable across the phases that DO add
//! a persisted, hashable plan (propose/apply in Phase 4/5, MCP tools in Phase
//! 9) - at that point the core gains a real check that the token's hash
//! matches the plan the caller was shown.

/// Proof that a mutating operation has been confirmed to proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// A short hash binding the caller to a specific persisted plan/proposal
    /// manifest (the one the caller was shown before confirming). Not yet
    /// verified by any core in this phase - no plan/proposal manifest exists
    /// to hash against until Phase 4+ (propose/apply) and Phase 9 (MCP).
    Token(String),
    /// The caller already confirmed by another means (a CLI TTY prompt, or
    /// `--yes`) before calling this core function.
    AlreadyConfirmed,
}

/// What every CLI wrapper passes a core once its own gate (TTY prompt or
/// `--yes`) has already been satisfied. Honors `GX_TEST_CONFIRM_TOKEN` (inert
/// unless set - a test-only hook, matching `GX_CRASH_POINT` /
/// `GX_TEST_LOCK_DELAY_MS`) so a test can prove a `Token` actually threads
/// through a wrapper into its core unchanged, without any core doing its own
/// env lookup.
pub fn already_confirmed() -> Confirmation {
    match std::env::var("GX_TEST_CONFIRM_TOKEN") {
        Ok(hash) if !hash.is_empty() => Confirmation::Token(hash),
        _ => Confirmation::AlreadyConfirmed,
    }
}

#[cfg(test)]
mod tests;
