use super::*;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

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

#[test]
fn sealed_classification_drift_fails_before_effect_authority_exists() {
    support::with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let exact = plan
                .outputs()
                .find_map(|output| {
                    output.generated_bytes().map(|bytes| {
                        (output.relative_path().to_owned(), bytes.to_vec())
                    })
                })
                .expect("fixture plan contains generated output");
            let exact_path = topology_fixture.publication_root().join(&exact.0);
            fs::create_dir_all(exact_path.parent().unwrap()).unwrap();
            support::set_safe_publication_parents(
                topology_fixture.publication_root(),
                &exact.0,
            );
            fs::write(&exact_path, &exact.1).unwrap();
            fs::set_permissions(&exact_path, fs::Permissions::from_mode(0o644)).unwrap();
            let root = topology_fixture.publication_root().to_owned();
            let _aggregate = arm_fixture_boot_namespace_assessments([
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
            ]);
            let _leaf = arm_fixture_immutable_leaf_assessments(root, plan.publication_count());

            let error = staged.attempt_immutable_boot_publication(&client).unwrap_err();
            assert!(matches!(
                error,
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectDeltaClassificationDrift,
            ));
            assert_eq!(fixture_immutable_leaf_assessments_remaining(), plan.publication_count());
            for output in plan.outputs() {
                let path = topology_fixture.publication_root().join(output.relative_path());
                if output.relative_path() == exact.0 {
                    assert_eq!(fs::read(path).unwrap(), exact.1);
                } else {
                    assert!(!path.exists());
                }
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

#[test]
fn immediate_post_schedule_journal_drift_fails_before_any_effect() {
    support::with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let canonical = fixture.installation.root.join(".cast/journal/state-transition");
            let displaced = canonical
                .parent()
                .unwrap()
                .join("post-schedule-displaced-transition");
            let hook_canonical = canonical.clone();
            let hook_displaced = displaced.clone();
            arm_after_pre_effect_schedule_validation(move || {
                let bytes = fs::read(&hook_canonical).unwrap();
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                fs::write(&hook_canonical, bytes).unwrap();
            });
            let root = topology_fixture.publication_root().to_owned();
            let _aggregate = arm_fixture_boot_namespace_assessments([
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
            ]);
            let _leaf = arm_fixture_immutable_leaf_assessments(root, plan.publication_count());

            let error = staged.attempt_immutable_boot_publication(&client).unwrap_err();
            assert!(matches!(
                error,
                ActiveReblitBootImmutablePublicationAttemptError::PreEffectStagedValidation(_),
            ));
            assert_eq!(fixture_immutable_leaf_assessments_remaining(), plan.publication_count());
            for output in plan.outputs() {
                assert!(!topology_fixture.publication_root().join(output.relative_path()).exists());
            }
            fs::remove_file(&canonical).unwrap();
            fs::rename(&displaced, &canonical).unwrap();
            support::assert_pending_boot_sync_started(
                &fixture.state_db,
                &fixture.installation,
                &expected_record,
                fingerprint,
            );
        }
    );
}
