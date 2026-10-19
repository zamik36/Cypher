//! Helpers for tests that run against the live stack (Redis, NATS).

/// Set to `1` in a job that provides the live stack: a test missing it
/// then fails instead of skipping.
pub const REQUIRE_LIVE: &str = "CYPHER_REQUIRE_LIVE";

/// The value of `var` (e.g. `CYPHER_TEST_REDIS`), which names a part of the
/// live stack a test needs. Unset (or empty) it is `None`, and the test
/// skips; where [`REQUIRE_LIVE`] is `1` it fails the test instead, so a
/// job that should run the live tests cannot pass by running none.
///
/// # Panics
///
/// When [`REQUIRE_LIVE`] is `1` and `var` is not set.
pub fn live_env(var: &str) -> Option<String> {
    let value = std::env::var(var).ok().filter(|v| !v.is_empty());
    let required = std::env::var(REQUIRE_LIVE).is_ok_and(|v| v == "1");
    assert!(
        value.is_some() || !required,
        "{var} must be set where {REQUIRE_LIVE}=1: this test needs the live stack"
    );
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing variable skips the test, unless the live stack is required.
    #[test]
    fn a_missing_variable_skips_unless_the_stack_is_required() {
        let missing = "CYPHER_TEST_NEVER_SET_ANYWHERE";
        let outcome = std::panic::catch_unwind(|| live_env(missing));
        if std::env::var(REQUIRE_LIVE).is_ok_and(|v| v == "1") {
            assert!(outcome.is_err(), "a required stack that is missing fails");
        } else {
            assert_eq!(outcome.ok(), Some(None), "otherwise the test skips");
        }
    }
}
