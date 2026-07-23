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

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
