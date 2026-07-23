use std::cell::Cell;

use super::*;

#[test]
fn full_alias_preflight_retains_global_states_and_deadline_without_effects() {
    support::with_bound_alias_plan!(|fixture, topology_fixture, plan| {
        let before = render_support::TreeSnapshot::capture(&fixture.installation.root);
        let expected_states = support::alternating_states(plan.publication_count());
        let calls = Cell::new(0usize);
        let mut assess = |role,
                          target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
                          domain: &BoundActiveReblitBootNamespaceDomain<'_>| {
            calls.set(calls.get() + 1);
            assert_eq!(role, BootTargetRole::Esp);
            assert_eq!(target.role(), BootTargetRole::Esp);
            assert_eq!(target.deadline(), plan.input_deadline());
            assert_eq!(domain.requests().len(), plan.publication_count());
            assert_eq!(domain.expected_sources().len(), plan.publication_count());
            assert_eq!(
                domain.plan_indices(),
                (0..plan.publication_count()).collect::<Vec<_>>()
            );
            Ok(support::target_assessment(target, expected_states.to_vec()))
        };
        let admitted = Instant::now();
        let mut now = || admitted;

        let preflight = plan
            .prepare_boot_publication_preflight_fixture_with(&mut assess, &mut now)
            .unwrap();

        assert_eq!(calls.get(), 1);
        assert_eq!(preflight.publication_count(), plan.publication_count());
        assert_eq!(preflight.initial_states(), expected_states.as_ref());
        assert_eq!(preflight.deadline(), plan.input_deadline());
        let debug = format!("{preflight:?}");
        assert!(debug.contains("descriptors hidden"));
        assert!(!debug.contains("firmware"));
        drop(preflight);
        assert_eq!(before, render_support::TreeSnapshot::capture(&fixture.installation.root));
    });
}

#[test]
fn inherited_deadline_fails_closed_at_each_outer_preflight_boundary() {
    support::with_bound_alias_plan!(|_fixture, _topology_fixture, plan| {
        let deadline = plan.input_deadline();
        let admitted = Instant::now();
        let expired = deadline + Duration::from_nanos(1);

        for (expiry_call, checkpoint, expected_assessments) in [
            (1usize, "entry", 0usize),
            (2, "after namespace-input binding", 0),
            (3, "after namespace assessment", 1),
            (4, "terminal", 1),
        ] {
            let clock_calls = Cell::new(0usize);
            let assessment_calls = Cell::new(0usize);
            let mut now = || {
                let call = clock_calls.get() + 1;
                clock_calls.set(call);
                if call == expiry_call { expired } else { admitted }
            };
            let mut assess = |_role,
                              target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
                              domain: &BoundActiveReblitBootNamespaceDomain<'_>| {
                assessment_calls.set(assessment_calls.get() + 1);
                Ok(support::target_assessment(
                    target,
                    vec![BootNamespaceDestinationState::Absent; domain.plan_indices().len()],
                ))
            };

            let error = plan
                .prepare_boot_publication_preflight_fixture_with(&mut assess, &mut now)
                .unwrap_err();

            assert!(matches!(
                error,
                ActiveReblitBootPublicationPreflightError::DeadlineExceeded {
                    checkpoint: found,
                    deadline: found_deadline,
                } if found == checkpoint && found_deadline == deadline
            ));
            assert_eq!(assessment_calls.get(), expected_assessments);
        }
    });
}

#[test]
fn collision_and_terminal_topology_drift_fail_after_read_only_assessment() {
    support::with_bound_alias_plan!(|_fixture, topology_fixture, plan| {
        let mut collision_assess = |_role,
                                    target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
                                    domain: &BoundActiveReblitBootNamespaceDomain<'_>| {
            arm_bound_plan_collision_drift();
            Ok(support::target_assessment(
                target,
                vec![BootNamespaceDestinationState::Absent; domain.plan_indices().len()],
            ))
        };
        let admitted = Instant::now();
        let mut now = || admitted;
        assert!(matches!(
            plan.prepare_boot_publication_preflight_fixture_with(&mut collision_assess, &mut now),
            Err(ActiveReblitBootPublicationPreflightError::CollisionDomainDrift)
        ));

        let mutated = Cell::new(false);
        let mut topology_assess = |_role,
                                   target: &RevalidatedActiveReblitBootPublicationTarget<'_>,
                                   domain: &BoundActiveReblitBootNamespaceDomain<'_>| {
            topology_fixture.replace_attachment_identity().unwrap();
            mutated.set(true);
            Ok(support::target_assessment(
                target,
                vec![BootNamespaceDestinationState::Exact; domain.plan_indices().len()],
            ))
        };
        let mut now = || admitted;
        let error = plan
            .prepare_boot_publication_preflight_fixture_with(&mut topology_assess, &mut now)
            .unwrap_err();
        assert!(mutated.get());
        assert!(matches!(
            error,
            ActiveReblitBootPublicationPreflightError::TerminalTargets { .. }
        ));
    });
}
