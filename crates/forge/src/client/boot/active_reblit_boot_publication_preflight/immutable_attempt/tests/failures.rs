use super::*;

#[test]
fn leaf_failure_stops_before_later_outputs_and_retains_pending_started() {
    support::with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let root = topology_fixture.publication_root().to_owned();
            let _aggregate = arm_fixture_boot_namespace_assessments([
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
            ]);
            let _leaf = arm_fixture_immutable_leaf_assessments(root, plan.publication_count());
            arm_retained_boot_file_publication_fault(
                FixtureRetainedBootFilePublicationFault::AfterExclusiveCreation,
            );

            let error = staged.attempt_immutable_boot_publication(&client).unwrap_err();
            assert!(matches!(
                &error,
                ActiveReblitBootImmutablePublicationAttemptError::LeafPublication {
                    plan_index: 0,
                    source: ActiveReblitBootImmutableLeafPublicationError::LeafPublication(
                        RetainedBootFilePublicationError::InjectedFault {
                            point: "after-exclusive-creation",
                        },
                    ),
                    ..
                },
            ), "unexpected leaf failure: {error:?}");
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            assert_eq!(fixture_immutable_leaf_assessments_remaining(), plan.publication_count() - 1);
            for output in plan.outputs() {
                assert!(!topology_fixture.publication_root().join(output.relative_path()).exists());
            }
            support::assert_pending_boot_sync_started(
                &fixture.state_db,
                &fixture.installation,
                &expected_record,
                fingerprint,
            );
        }
    );
}
