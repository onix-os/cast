use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

#[test]
fn staged_alias_attempt_publishes_in_phase_order_and_terminally_observes_exact() {
    support::with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let exact_index = plan
                .outputs()
                .enumerate()
                .find_map(|(index, output)| output.generated_bytes().map(|bytes| (index, output.relative_path().to_owned(), bytes.to_vec())))
                .expect("render plan must contain generated output");
            let exact_path = topology_fixture.publication_root().join(&exact_index.1);
            fs::create_dir_all(exact_path.parent().unwrap()).unwrap();
            support::set_safe_publication_parents(
                topology_fixture.publication_root(),
                &exact_index.1,
            );
            fs::write(&exact_path, &exact_index.2).unwrap();
            fs::set_permissions(&exact_path, fs::Permissions::from_mode(0o644)).unwrap();
            let exact_inode = fs::metadata(&exact_path).unwrap().ino();

            let root = topology_fixture.publication_root().to_owned();
            let _aggregate = arm_fixture_boot_namespace_assessments([
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
                FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone()),
            ]);
            let _leaf = arm_fixture_immutable_leaf_assessments(root, plan.publication_count());

            let result = staged.attempt_immutable_boot_publication(&client).unwrap();
            assert_eq!(result.publication_count(), plan.publication_count());
            assert_eq!(result.published_count(), plan.publication_count() - 1);
            assert_eq!(result.already_exact_count(), 1);
            assert_eq!(result.evidence().len(), plan.publication_count());
            let mut previous_phase = None;
            for (evidence, output) in result.evidence().iter().zip(plan.outputs()) {
                assert_eq!(evidence.length(), output.expected_length());
                assert_eq!(evidence.xxh3(), output.expected_digest());
                assert_eq!(evidence.sha256(), *output.expected_content_identity().as_bytes());
                if let Some(previous) = previous_phase {
                    assert!(previous <= output.phase());
                }
                previous_phase = Some(output.phase());
                let path = topology_fixture.publication_root().join(output.relative_path());
                assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o644);
            }
            assert_eq!(fs::metadata(&exact_path).unwrap().ino(), exact_inode);
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            assert_eq!(fixture_immutable_leaf_assessments_remaining(), 0);
            drop(result);
            support::assert_pending_boot_sync_started(
                &fixture.state_db,
                &fixture.installation,
                &expected_record,
                fingerprint,
            );
        }
    );
}
