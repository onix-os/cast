use super::*;
use crate::{
    client::{
        active_reblit_boot_inputs::ActiveReblitStoneBootInputsOutcome,
        active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
        active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
    },
    transition_identity::PreparedActiveReblitBootStateRoots,
};

macro_rules! with_client_bound_staging_plan {
    (|$fixture:ident, $client:ident, $plan:ident, $inventory:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let $client = staging_client(&$fixture, $fixture.state_db.clone());
        let stone = match PreparedActiveReblitStoneBootInputs::prepare_until(
            &$client.installation,
            &$client.state_db,
            &$client.layout_db,
            &$fixture.head,
            deadline,
        )
        .unwrap()
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
                panic!("client-bound fixture must be bootable: {reason:?}")
            }
        };
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &$client.installation,
            &$fixture.head_usr,
            $fixture.head.id,
            stone.state_ids(),
            deadline,
        )
        .unwrap();
        let prepared = PreparedActiveReblitBootRenderInputs::prepare_until(
            &stone,
            &roots,
            &$client.installation,
            deadline,
        )
        .unwrap();
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let inputs = prepared
            .revalidate_until(
                &$client.state_db,
                &$client.layout_db,
                &$client.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture =
            AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture
            .prepare_for_installation_until(&$client.installation, deadline)
            .unwrap();
        let topology = topology_prepared
            .revalidate_until(&$client.installation, deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        let $inventory = $plan.prepare_desired_publication_inventory().unwrap();
        let _staging_preflight = arm_fixture_boot_namespace_assessments([
            FixtureBootNamespaceAssessment::new(
                BootTargetRole::Esp,
                topology_fixture.publication_root().to_owned(),
            ),
        ]);
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

fn handoff_for_test(
    fixture: &support::RenderFixture,
    active_reblit: crate::State,
    journal: TransitionJournalStore,
    record: TransitionRecord,
    record_binding: TransitionJournalRecordBinding,
) -> CoordinatorActiveReblitBootSyncHandoff {
    let active_state_reservation =
        CoordinatorActiveStateReservation::acquire().unwrap();
    CoordinatorActiveReblitBootSyncHandoff::from_parts_for_test(
        record,
        record_binding,
        journal,
        fixture.state_db.clone(),
        fixture.installation.clone(),
        active_reblit,
        active_state_reservation,
    )
}

#[test]
fn coordinator_handoff_stages_exact_state_and_rejects_same_installation_cross_state_plan() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
    with_client_bound_staging_plan!(|fixture, client, plan, inventory| {
        let (journal, predecessor, binding) = exact_boot_sync_journal_for_state(
            &fixture.installation,
            Some(fixture.head.id),
        );
        assert_eq!(plan.global_state(), fixture.head.id);
        assert_eq!(predecessor.phase, Phase::SystemTriggersComplete);
        assert_eq!(predecessor.generation, 10);
        let handoff = handoff_for_test(
            &fixture,
            fixture.head.clone(),
            journal,
            predecessor,
            binding,
        );

        let staged = stage_active_reblit_boot_sync_from_handoff_for_test(
            &client,
            &plan,
            &inventory,
            handoff,
        )
        .unwrap();

        assert_eq!(staged.record().phase, Phase::BootSyncStarted);
        assert_eq!(
            staged.record().candidate.id,
            Some(i32::from(fixture.head.id)),
        );
        assert_eq!(
            staged.record().previous.id,
            Some(i32::from(fixture.head.id)),
        );
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
    });

    with_client_bound_staging_plan!(|fixture, client, plan, inventory| {
        let other_state = fixture
            .state_db
            .add(&[], Some("cross-state handoff"), None)
            .unwrap();
        assert_ne!(plan.global_state(), other_state.id);
        let (journal, predecessor, binding) = exact_boot_sync_journal_for_state(
            &fixture.installation,
            Some(other_state.id),
        );
        let expected = predecessor.clone();
        let handoff = handoff_for_test(
            &fixture,
            other_state,
            journal,
            predecessor,
            binding,
        );

        let error = stage_active_reblit_boot_sync_from_handoff_for_test(
            &client,
            &plan,
            &inventory,
            handoff,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitCoordinatorBootSyncStagingError::PlanActiveStateMismatch,
        ));
        assert!(
            fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap()
                .pending()
                .is_none(),
        );
        assert_exact_journal_record(&fixture.installation, &expected);
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
    });
}
