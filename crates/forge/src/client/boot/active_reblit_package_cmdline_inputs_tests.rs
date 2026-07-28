use std::time::{Duration, Instant};

use super::*;

#[path = "active_reblit_package_cmdline_inputs_tests/bounds_and_deadlines.rs"]
mod bounds_and_deadlines;
#[path = "active_reblit_package_cmdline_inputs_tests/semantics.rs"]
mod semantics;
#[path = "active_reblit_package_cmdline_inputs_tests/source_binding.rs"]
mod source_binding;
#[path = "active_reblit_package_cmdline_inputs_tests/support.rs"]
mod support;

// Generous on purpose. This is a test-side deadline standing in for a
// caller's absolute budget, and the work it bounds is not slow — but under a
// parallel suite, contention alone can exhaust a short window and surface as
// `DeadlineExceeded` far from anything the test is about
// (`plans/future_impl.md` §2.1a). Keep it well above any plausible
// contention stall; these tests never assert on how long the work takes.
fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(600)
}
