use super::*;

#[test]
fn test_already_confirmed_defaults_to_already_confirmed() {
    let guard = crate::test_utils::env_lock();
    let prior = std::env::var("GX_TEST_CONFIRM_TOKEN").ok();
    unsafe { std::env::remove_var("GX_TEST_CONFIRM_TOKEN") };

    assert_eq!(already_confirmed(), Confirmation::AlreadyConfirmed);

    if let Some(v) = prior {
        unsafe { std::env::set_var("GX_TEST_CONFIRM_TOKEN", v) };
    }
    drop(guard);
}

#[test]
fn test_already_confirmed_honors_test_token_hook() {
    // GX_TEST_CONFIRM_TOKEN is inert unless set (matching GX_CRASH_POINT /
    // GX_TEST_LOCK_DELAY_MS): a real test can prove a Token threads through a
    // wrapper into its core unchanged by setting it and inspecting what the
    // core received.
    let guard = crate::test_utils::env_lock();
    let prior = std::env::var("GX_TEST_CONFIRM_TOKEN").ok();
    unsafe { std::env::set_var("GX_TEST_CONFIRM_TOKEN", "deadbeef") };

    assert_eq!(
        already_confirmed(),
        Confirmation::Token("deadbeef".to_string())
    );

    match prior {
        Some(v) => unsafe { std::env::set_var("GX_TEST_CONFIRM_TOKEN", v) },
        None => unsafe { std::env::remove_var("GX_TEST_CONFIRM_TOKEN") },
    }
    drop(guard);
}

#[test]
fn test_confirmation_variants_are_constructible_and_comparable() {
    // Both variants must be constructible from any core's call site (CLI
    // wrappers pass `AlreadyConfirmed` today; a future MCP caller passes
    // `Token`). Equality/Debug are load-bearing for the tests each split core
    // adds (they assert on the exact confirmation a core received).
    let already = Confirmation::AlreadyConfirmed;
    let token_a = Confirmation::Token("deadbeef".to_string());
    let token_b = Confirmation::Token("deadbeef".to_string());
    let token_c = Confirmation::Token("other".to_string());

    assert_eq!(token_a, token_b);
    assert_ne!(token_a, token_c);
    assert_ne!(already, token_a);
    assert_eq!(format!("{already:?}"), "AlreadyConfirmed");
}

/// Break-the-guard bite (Phase 3 success criterion): every finish-line op on
/// non-interactive stdin (which is exactly what `cargo test` runs under)
/// without `--yes` must FAIL CLOSED with a loud error naming `--yes`. If the
/// guard is removed (e.g. the `is_terminal()` branch is dropped or made to
/// return `Ok(false)`/`Ok(true)`), this test fails.
#[test]
fn test_confirm_destructive_fails_closed_naming_yes_for_each_op() {
    for op in [
        DestructiveOp::ReviewApprove,
        DestructiveOp::ReviewDelete,
        DestructiveOp::Cleanup,
    ] {
        let err = confirm_destructive(op, 7, false)
            .expect_err("non-interactive stdin without --yes must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("--yes"),
            "fail-closed error for {op:?} must name --yes: {msg}"
        );
        // The blast radius (the count) rides the message so a scripted operator
        // sees exactly what was refused.
        assert!(
            msg.contains('7'),
            "fail-closed error for {op:?} must name the count: {msg}"
        );
    }
}

/// `--yes` bypasses the prompt entirely (no stdin read, no TTY needed) and
/// proceeds. This is the documented non-interactive path.
#[test]
fn test_confirm_destructive_yes_proceeds_without_prompt() {
    for op in [
        DestructiveOp::ReviewApprove,
        DestructiveOp::ReviewDelete,
        DestructiveOp::Cleanup,
    ] {
        assert!(
            confirm_destructive(op, 42, true).expect("--yes must not error"),
            "--yes must proceed for {op:?}"
        );
    }
}

/// `review delete` abandons UNMERGED work; its consent/fail-closed message must
/// state that truthfully so consent is informed (design doc Phase 4 wording,
/// staged in the Phase 3 prompt).
#[test]
fn test_confirm_destructive_delete_message_states_unmerged() {
    let err = confirm_destructive(DestructiveOp::ReviewDelete, 3, false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("UNMERGED"),
        "delete fail-closed message must state the unmerged destruction: {msg}"
    );
}
