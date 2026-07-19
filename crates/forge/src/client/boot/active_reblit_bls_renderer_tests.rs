use super::*;
use crate::client::{
    active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
    active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
};
use crate::{Installation, db, state};
use std::{path::Path, time::Duration};

// Focused renderer contracts are split by behavior below. Keeping this root
// small also lets the existing render-input fixture be included without a
// second filesystem implementation.

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;
#[path = "active_reblit_bls_renderer_tests/topology.rs"]
mod topology;

#[path = "active_reblit_bls_renderer_tests/bounds_and_deadlines.rs"]
mod bounds_and_deadlines;
#[path = "active_reblit_bls_renderer_tests/golden_documents.rs"]
mod golden_documents;
#[path = "active_reblit_bls_renderer_tests/ownership_and_effects.rs"]
mod ownership_and_effects;
#[path = "active_reblit_bls_renderer_tests/payloads_and_collisions.rs"]
mod payloads_and_collisions;
#[path = "active_reblit_bls_renderer_tests/schemas_and_identity.rs"]
mod schemas_and_identity;

macro_rules! with_render_inputs {
    ($fixture:expr, $deadline:expr, |$fixture_name:ident, $inputs:ident| $body:block) => {{
        let $fixture_name = $fixture;
        let stone = $fixture_name.stone();
        let roots = $fixture_name.roots(&stone);
        let prepared = support::prepare_static(&$fixture_name, &stone, &roots);
        let local_policy = $fixture_name.local_policy();
        let root_intent = $fixture_name.root_intent();
        let $inputs = prepared
            .revalidate_until(
                &$fixture_name.state_db,
                &$fixture_name.layout_db,
                &$fixture_name.installation,
                &local_policy,
                &root_intent,
                $deadline,
            )
            .unwrap();
        $body
    }};
}

pub(super) use with_render_inputs;

fn fixture_plan<'asset, 'attempt, 'stone, 'roots>(
    rendered: RenderedActiveReblitBlsRequests<'asset, 'attempt, 'stone, 'roots>,
    topology: &crate::client::active_reblit_mounted_boot_topology::ActiveReblitMountedBootTopology,
) -> (PreparedActiveReblitBootPublicationPlan, SealedSourceCatalog<'asset>) {
    let deadline = rendered.deadline;
    rendered
        .into_fixture_publication_plan(topology.bound(), deadline, Instant::now)
        .unwrap()
}

fn generated_at<'a>(plan: &'a PreparedActiveReblitBootPublicationPlan, path: &str) -> &'a [u8] {
    plan.outputs()
        .iter()
        .find(|output| output.relative_path() == Path::new(path))
        .and_then(|output| output.source().generated_bytes())
        .unwrap()
}
