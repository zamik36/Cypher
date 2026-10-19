//! Helpers for tests that run against the live stack (Redis, NATS).

/// The value of `var` (e.g. `CYPHER_TEST_REDIS`), which names a part of the
/// live stack a test needs. Unset (or empty) it is `None` locally, and the
/// test skips; in CI, where `CI` is set, it fails the test instead, so a
/// job without the stack cannot pass by running nothing.
///
/// # Panics
///
/// When `CI` is set and `var` is not.
pub fn live_env(var: &str) -> Option<String> {
    let value = std::env::var(var).ok().filter(|v| !v.is_empty());
    assert!(
        value.is_some() || std::env::var_os("CI").is_none(),
        "{var} must be set in CI: this test needs the live stack"
    );
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing variable skips the test locally and fails it in CI.
    #[test]
    fn a_missing_variable_skips_locally_and_fails_in_ci() {
        let missing = "CYPHER_TEST_NEVER_SET_ANYWHERE";
        let outcome = std::panic::catch_unwind(|| live_env(missing));
        if std::env::var_os("CI").is_some() {
            assert!(outcome.is_err(), "CI without the stack fails");
        } else {
            assert_eq!(outcome.ok(), Some(None), "a local run skips");
        }
    }
}
