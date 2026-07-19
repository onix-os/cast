use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    Installation,
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
        active_reblit_publication_plan::{
            ACTIVE_REBLIT_BOOT_OUTPUT_MODE, ActiveReblitBootPublicationRequest, PreparedActiveReblitBootPublicationPlan,
        },
    },
    db, state,
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;

#[path = "active_reblit_desired_publication_tests/bounds_and_deadlines.rs"]
mod bounds_and_deadlines;
#[path = "active_reblit_desired_publication_tests/integration.rs"]
mod integration;
#[path = "active_reblit_desired_publication_tests/sensitivity.rs"]
mod sensitivity;

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn fixture_outputs() -> Vec<DesiredActiveReblitBootPublication> {
    vec![
        DesiredActiveReblitBootPublication {
            root: ActiveReblitBootDestinationRoot::Esp,
            phase: ActiveReblitBootPublicationPhase::Bootloader,
            role: ActiveReblitBootPublicationRole::FallbackBootloader,
            relative_path: PathBuf::from("EFI/Boot/BOOTX64.EFI"),
            mode: ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
            checksum: 0x112233445566778899aabbccddeeff00,
            length: 11,
            content_identity: BootContentIdentity::hash(b"first-bytes"),
        },
        DesiredActiveReblitBootPublication {
            root: ActiveReblitBootDestinationRoot::Boot,
            phase: ActiveReblitBootPublicationPhase::Payload,
            role: ActiveReblitBootPublicationRole::Payload,
            relative_path: PathBuf::from("EFI/head/xxh3-00112233445566778899aabbccddeeff-l000000000000000b/vmlinuz"),
            mode: ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
            checksum: 0x00112233445566778899aabbccddeeff,
            length: 11,
            content_identity: BootContentIdentity::hash(b"other-bytes"),
        },
    ]
}

fn prepare_fixture(
    outputs: &[DesiredActiveReblitBootPublication],
    layout: ActiveReblitBootDestinationLayout,
) -> PreparedActiveReblitDesiredPublicationInventory {
    prepare_fixture_with_policy(outputs, layout, DESIRED_PUBLICATION_POLICY).unwrap()
}

fn prepare_fixture_with_policy(
    outputs: &[DesiredActiveReblitBootPublication],
    layout: ActiveReblitBootDestinationLayout,
    policy: DesiredPublicationPolicy,
) -> Result<PreparedActiveReblitDesiredPublicationInventory, ActiveReblitDesiredPublicationError> {
    let mut now = Instant::now;
    prepare_fixture_with_policy_and_clocks(outputs, layout, policy, future_deadline(), &mut now, Instant::now)
}

fn prepare_fixture_with_policy_and_clocks<Clock, TerminalClock>(
    outputs: &[DesiredActiveReblitBootPublication],
    layout: ActiveReblitBootDestinationLayout,
    policy: DesiredPublicationPolicy,
    deadline: Instant,
    now: &mut Clock,
    terminal_now: TerminalClock,
) -> Result<PreparedActiveReblitDesiredPublicationInventory, ActiveReblitDesiredPublicationError>
where
    Clock: FnMut() -> Instant,
    TerminalClock: FnOnce() -> Instant,
{
    let mut builder = DesiredPublicationBuilder::new(layout, outputs.len(), policy, deadline, now)?;
    for output in outputs {
        builder.push(
            output.root,
            output.phase,
            output.role,
            &output.relative_path,
            output.mode,
            output.checksum,
            output.length,
            output.content_identity,
        )?;
    }
    builder.finish(terminal_now)
}

fn prepare_publication_plan_fixture(
    plan: &PreparedActiveReblitBootPublicationPlan,
    layout: ActiveReblitBootDestinationLayout,
) -> PreparedActiveReblitDesiredPublicationInventory {
    let deadline = future_deadline();
    let mut now = Instant::now;
    let mut builder = DesiredPublicationBuilder::new(
        layout,
        plan.outputs().len(),
        DESIRED_PUBLICATION_POLICY,
        deadline,
        &mut now,
    )
    .unwrap();
    for output in plan.outputs() {
        builder
            .push(
                output.root(),
                output.phase(),
                output.role(),
                output.relative_path(),
                output.mode(),
                output.source().digest(),
                output.source().length(),
                output.source().content_identity(),
            )
            .unwrap();
    }
    builder.finish(Instant::now).unwrap()
}

macro_rules! with_bound_alias_plan {
    (|$fixture:ident, $plan:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = $fixture.local_policy();
        let root_intent = $fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$fixture.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture = AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture.prepare_until(deadline).unwrap();
        let topology = topology_prepared
            .revalidate_until(topology_fixture.installation(), deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

pub(super) use with_bound_alias_plan;
