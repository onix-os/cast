
//! Harness-free proof for descriptor-anchored package-cache cleaning.
//!
//! The production container boundary authenticates an exactly single-tasked
//! supervisor immediately before its fork-like clone. Rust's ordinary libtest
//! worker process cannot satisfy that contract, so this target deliberately
//! opts out of libtest and invokes Mason's narrow feature-gated entry point.

fn main() {
    match std::env::var("CAST_CACHE_CLEAN_TEST_RUNNER") {
        Ok(value) if value == "1" => mason::cache_clean_test_support::run(),
        Err(std::env::VarError::NotPresent) => {
            eprintln!("cache-clean proof is runner-only; use `make cache-clean-test`");
        }
        Ok(value) => panic!("CAST_CACHE_CLEAN_TEST_RUNNER must be exactly `1`, found {value:?}"),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("CAST_CACHE_CLEAN_TEST_RUNNER must be the UTF-8 value `1`")
        }
    }
}
