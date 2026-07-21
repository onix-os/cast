use std::time::{Duration, Instant};

use super::*;
use crate::{
    Installation, db, state,
    client::{
        active_reblit_bls_renderer::{
            RenderedActiveReblitBlsRequests, arm_bound_plan_collision_drift,
        },
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
    },
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod render_support;
#[path = "active_reblit_boot_publication_preflight_tests/support.rs"]
mod support;

#[path = "active_reblit_boot_publication_preflight_tests/global_merge.rs"]
mod global_merge;
#[path = "active_reblit_boot_publication_preflight_tests/integration.rs"]
mod integration;
