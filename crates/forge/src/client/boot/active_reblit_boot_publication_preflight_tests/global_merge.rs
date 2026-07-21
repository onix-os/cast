use super::*;

#[test]
fn alias_and_distinct_domains_restore_exact_global_plan_order() {
    let expected_identity = support::identity(10, 20, 30);

    let mut alias = support::empty_global_states(4);
    let alias_assessment = support::fixture_assessment(
        expected_identity,
        [
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Absent,
        ],
    );
    merge_domain_assessment(
        BootTargetRole::Esp,
        expected_identity,
        &[0, 1, 2, 3],
        &alias_assessment,
        &mut alias,
    )
    .unwrap();
    assert_eq!(
        close_global_states(alias).unwrap().as_ref(),
        [
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Absent,
        ]
    );

    let mut distinct = support::empty_global_states(5);
    let esp = support::fixture_assessment(
        expected_identity,
        [
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Absent,
        ],
    );
    let xbootldr = support::fixture_assessment(
        expected_identity,
        [
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Absent,
        ],
    );
    merge_domain_assessment(
        BootTargetRole::Esp,
        expected_identity,
        &[1, 3],
        &esp,
        &mut distinct,
    )
    .unwrap();
    merge_domain_assessment(
        BootTargetRole::Xbootldr,
        expected_identity,
        &[0, 2, 4],
        &xbootldr,
        &mut distinct,
    )
    .unwrap();
    assert_eq!(
        close_global_states(distinct).unwrap().as_ref(),
        [
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Absent,
        ]
    );
}

#[test]
fn different_count_index_and_identity_evidence_fail_closed() {
    let identity = support::identity(10, 20, 30);
    let different = support::fixture_assessment(
        identity,
        [BootNamespaceDestinationState::Different],
    );
    let mut states = support::empty_global_states(2);
    assert!(matches!(
        merge_domain_assessment(
            BootTargetRole::Xbootldr,
            identity,
            &[1],
            &different,
            &mut states,
        ),
        Err(ActiveReblitBootPublicationPreflightError::DifferentDestination {
            role: BootTargetRole::Xbootldr,
            plan_index: 1,
        })
    ));

    assert!(matches!(
        require_publication_count(3, 2),
        Err(ActiveReblitBootPublicationPreflightError::PublicationCountMismatch {
            expected: 3,
            actual: 2,
        })
    ));
    let exact = support::fixture_assessment(identity, [BootNamespaceDestinationState::Exact]);
    let mut states = support::empty_global_states(2);
    assert!(matches!(
        merge_domain_assessment(
            BootTargetRole::Esp,
            identity,
            &[0, 1],
            &exact,
            &mut states,
        ),
        Err(ActiveReblitBootPublicationPreflightError::AssessmentLengthMismatch {
            role: BootTargetRole::Esp,
            states: 1,
            indices: 2,
        })
    ));

    for (indices, expected_error) in [
        (&[2][..], "out-of-range"),
        (&[1, 0][..], "order"),
    ] {
        let observed = support::fixture_assessment(
            identity,
            vec![BootNamespaceDestinationState::Exact; indices.len()],
        );
        let mut states = support::empty_global_states(2);
        let error = merge_domain_assessment(
            BootTargetRole::Esp,
            identity,
            indices,
            &observed,
            &mut states,
        )
        .unwrap_err();
        assert!(
            matches!(
                (&error, expected_error),
                (
                    ActiveReblitBootPublicationPreflightError::PlanIndexOutOfRange {
                        role: BootTargetRole::Esp,
                        plan_index: 2,
                        publication_count: 2,
                    },
                    "out-of-range",
                ) | (
                    ActiveReblitBootPublicationPreflightError::PlanIndexOrder {
                        role: BootTargetRole::Esp,
                        previous: 1,
                        plan_index: 0,
                    },
                    "order",
                )
            ),
            "unexpected {expected_error} error: {error:?}"
        );
    }

    let mut duplicate = support::empty_global_states(2);
    merge_domain_assessment(
        BootTargetRole::Esp,
        identity,
        &[0],
        &exact,
        &mut duplicate,
    )
    .unwrap();
    assert!(matches!(
        merge_domain_assessment(
            BootTargetRole::Xbootldr,
            identity,
            &[0],
            &exact,
            &mut duplicate,
        ),
        Err(ActiveReblitBootPublicationPreflightError::DuplicatePlanIndex {
            plan_index: 0,
        })
    ));
    assert!(matches!(
        close_global_states(duplicate),
        Err(ActiveReblitBootPublicationPreflightError::MissingPlanIndex {
            plan_index: 1,
        })
    ));

    for found in [
        support::identity(11, 20, 30),
        support::identity(10, 21, 30),
        support::identity(10, 20, 31),
    ] {
        let observed = support::fixture_assessment(
            found,
            [BootNamespaceDestinationState::Absent],
        );
        let mut states = support::empty_global_states(1);
        assert!(matches!(
            merge_domain_assessment(
                BootTargetRole::Xbootldr,
                identity,
                &[0],
                &observed,
                &mut states,
            ),
            Err(ActiveReblitBootPublicationPreflightError::AssessmentIdentityMismatch {
                role: BootTargetRole::Xbootldr,
                expected_device: 10,
                expected_inode: 20,
                expected_mount_id: 30,
                found_device,
                found_inode,
                found_mount_id,
            }) if (found_device, found_inode, found_mount_id)
                == (found.device, found.inode, found.mount_id)
        ));
    }
}
