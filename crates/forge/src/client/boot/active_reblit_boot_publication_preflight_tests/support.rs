use super::*;

macro_rules! with_bound_alias_plan {
    (|$fixture:ident, $topology_fixture:ident, $plan:ident| $body:block) => {{
        let deadline = render_support::future_deadline();
        let $fixture = render_support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = render_support::prepare_static(&$fixture, &stone, &roots);
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
        let $topology_fixture = AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = $topology_fixture.prepare_until(deadline).unwrap();
        let topology = topology_prepared
            .revalidate_until($topology_fixture.installation(), deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        $body
        $topology_fixture.assert_outside_unchanged();
    }};
}

pub(super) use with_bound_alias_plan;

pub(super) fn target_assessment(
    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
    states: impl Into<Box<[BootNamespaceDestinationState]>>,
) -> BootPublicationNamespaceAssessment {
    BootPublicationNamespaceAssessment::fixture(target_identity(target), states)
}

pub(super) fn alternating_states(count: usize) -> Box<[BootNamespaceDestinationState]> {
    (0..count)
        .map(|index| {
            if index % 2 == 0 {
                BootNamespaceDestinationState::Absent
            } else {
                BootNamespaceDestinationState::Exact
            }
        })
        .collect()
}

pub(super) const fn identity(
    device: u64,
    inode: u64,
    mount_id: u64,
) -> BootPublicationAssessmentIdentity {
    BootPublicationAssessmentIdentity {
        device,
        inode,
        mount_id,
    }
}

pub(super) fn fixture_assessment(
    identity: BootPublicationAssessmentIdentity,
    states: impl Into<Box<[BootNamespaceDestinationState]>>,
) -> BootPublicationNamespaceAssessment {
    BootPublicationNamespaceAssessment::fixture(identity, states)
}

pub(super) fn empty_global_states(count: usize) -> Vec<BootNamespaceDestinationState> {
    vec![BootNamespaceDestinationState::Different; count]
}
