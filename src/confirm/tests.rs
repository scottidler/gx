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
