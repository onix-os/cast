use super::*;

#[test]
fn bound_renderer_plan_projects_to_owned_authority_free_canonical_records() {
    with_bound_alias_plan!(|fixture, plan| {
        let original = support::TreeSnapshot::capture(&fixture.installation.root);
        let mismatched_deadline = plan.input_deadline() + Duration::from_nanos(1);
        let mut now = Instant::now;
        assert!(matches!(
            prepare_bound_inventory_with_policy_and_clocks(
                &plan,
                DESIRED_PUBLICATION_POLICY,
                mismatched_deadline,
                &mut now,
                Instant::now,
            ),
            Err(ActiveReblitDesiredPublicationError::DeadlineMismatch { .. })
        ));

        let expected = plan
            .outputs()
            .map(|output| {
                (
                    output.root(),
                    output.phase(),
                    output.role(),
                    output.relative_path().to_path_buf(),
                    output.mode(),
                    output.expected_digest(),
                    output.expected_length(),
                    output.expected_content_identity(),
                )
            })
            .collect::<Vec<_>>();
        let expected_path_bytes = plan.publication_path_bytes();
        let expected_logical_bytes = plan.logical_bytes();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let count = plan.publication_count();
        drop(plan);

        let actual = inventory
            .outputs()
            .iter()
            .map(|output| {
                (
                    output.root(),
                    output.phase(),
                    output.role(),
                    output.relative_path().to_path_buf(),
                    output.mode(),
                    output.checksum(),
                    output.length(),
                    output.content_identity(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            inventory.destination_layout(),
            ActiveReblitBootDestinationLayout::BootAliasesEsp
        );
        assert_eq!(inventory.outputs().len(), count);
        assert_eq!(actual, expected);
        assert_eq!(inventory.path_bytes(), expected_path_bytes);
        assert_eq!(inventory.logical_bytes(), expected_logical_bytes);
        assert!(inventory.outputs().iter().all(|output| {
            output.mode() == ACTIVE_REBLIT_BOOT_OUTPUT_MODE
                && !output.relative_path().is_absolute()
                && output.length() != 0
                && output.content_identity().as_bytes() != &[0; 32]
        }));
        assert!(inventory.outputs().iter().any(|output| {
            output.root() == ActiveReblitBootDestinationRoot::Esp
                && output.phase() == ActiveReblitBootPublicationPhase::Bootloader
                && output.role() == ActiveReblitBootPublicationRole::FallbackBootloader
        }));
        assert_ne!(inventory.fingerprint().as_bytes(), &[0; 32]);
        assert_eq!(original, support::TreeSnapshot::capture(&fixture.installation.root));
    });
}

#[test]
fn sealed_stone_binding_index_is_excluded_from_the_desired_fingerprint() {
    let topology_fixture = AliasFixture::stable().unwrap();
    let deadline = future_deadline();
    let prepared = topology_fixture.prepare_until(deadline).unwrap();
    let topology = prepared
        .revalidate_until(topology_fixture.installation(), deadline)
        .unwrap();
    let digest = 0x00112233445566778899aabbccddeeff;
    let length = 11;
    let content_identity = BootContentIdentity::hash(b"first-bytes");
    let path = PathBuf::from(format!("EFI/head/xxh3-{digest:032x}-l{length:016x}/vmlinuz"));
    let request = |binding_index| {
        ActiveReblitBootPublicationRequest::sealed_payload(
            path.clone(),
            binding_index,
            digest,
            length,
            content_identity,
        )
    };
    let first =
        PreparedActiveReblitBootPublicationPlan::prepare_until([request(3)], topology.topology(), deadline).unwrap();
    let second =
        PreparedActiveReblitBootPublicationPlan::prepare_until([request(900)], topology.topology(), deadline).unwrap();

    let first = prepare_publication_plan_fixture(&first, ActiveReblitBootDestinationLayout::BootAliasesEsp);
    let second = prepare_publication_plan_fixture(&second, ActiveReblitBootDestinationLayout::BootAliasesEsp);
    assert_eq!(first.outputs(), second.outputs());
    assert_eq!(first.fingerprint(), second.fingerprint());
    topology_fixture.assert_outside_unchanged();
}
