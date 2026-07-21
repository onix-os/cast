use super::*;
use std::os::unix::fs::MetadataExt as _;

#[test]
fn pre_effect_journal_identity_drift_fails_before_any_publication() {
    support::with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let canonical = fixture.installation.root.join(".cast/journal/state-transition");
            let displaced = canonical
                .parent()
                .unwrap()
                .join("pre-effect-displaced-transition");
            let original_inode = fs::metadata(&canonical).unwrap().ino();
            let root = topology_fixture.publication_root().to_owned();
            let _aggregate = arm_fixture_boot_namespace_assessments([
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()).after(
                    FixtureBootNamespaceMutation::ReplaceFileIdentity {
                        canonical: canonical.clone(),
                        displaced: displaced.clone(),
                    },
                ),
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
            ]);
            let _leaf = arm_fixture_immutable_leaf_assessments(root, plan.publication_count());

            let error = staged.attempt_immutable_boot_publication(&client).unwrap_err();
            assert!(matches!(
                &error,
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectStagedValidation(_),
            ), "unexpected pre-effect error: {error:?}");
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 1);
            assert_eq!(fixture_immutable_leaf_assessments_remaining(), plan.publication_count());
            for output in plan.outputs() {
                assert!(!topology_fixture.publication_root().join(output.relative_path()).exists());
            }
            let replacement_inode = fs::metadata(&canonical).unwrap().ino();
            assert_ne!(replacement_inode, original_inode);
            assert_eq!(fs::metadata(&displaced).unwrap().ino(), original_inode);
            fs::remove_file(&canonical).unwrap();
            fs::rename(&displaced, &canonical).unwrap();
            assert_eq!(fs::metadata(&canonical).unwrap().ino(), original_inode);
            support::assert_pending_boot_sync_started(
                &fixture.state_db,
                &fixture.installation,
                &expected_record,
                fingerprint,
            );
        }
    );
}
