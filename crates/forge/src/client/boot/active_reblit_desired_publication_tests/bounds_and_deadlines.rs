use std::cell::Cell;

use super::*;

#[test]
fn count_path_canonical_and_work_bounds_admit_n_and_reject_n_minus_one() {
    let outputs = fixture_outputs();
    let baseline = prepare_fixture(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp);
    let exact = DesiredPublicationPolicy {
        max_publications: baseline.outputs().len(),
        max_path_bytes: baseline.path_bytes(),
        max_single_path_bytes: baseline
            .outputs()
            .iter()
            .map(|output| output.relative_path().to_str().unwrap().len())
            .max()
            .unwrap(),
        max_logical_bytes: baseline.logical_bytes(),
        max_canonical_bytes: baseline.canonical_bytes(),
        max_work: baseline.work(),
    };
    prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, exact).unwrap();

    let mut count = exact;
    count.max_publications -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, count),
        Err(ActiveReblitDesiredPublicationError::PublicationCountLimit { .. })
    ));

    let mut single_path = exact;
    single_path.max_single_path_bytes -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, single_path),
        Err(ActiveReblitDesiredPublicationError::SinglePathByteLimit { .. })
    ));

    let mut logical = exact;
    logical.max_logical_bytes -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, logical),
        Err(ActiveReblitDesiredPublicationError::LogicalByteLimit { .. })
    ));

    let mut paths = exact;
    paths.max_path_bytes -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, paths),
        Err(ActiveReblitDesiredPublicationError::PathByteLimit { .. })
    ));

    let mut canonical = exact;
    canonical.max_canonical_bytes -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, canonical),
        Err(ActiveReblitDesiredPublicationError::CanonicalByteLimit { .. })
    ));

    let mut work = exact;
    work.max_work -= 1;
    assert!(matches!(
        prepare_fixture_with_policy(&outputs, ActiveReblitBootDestinationLayout::BootAliasesEsp, work),
        Err(ActiveReblitDesiredPublicationError::WorkLimit { .. })
    ));
}

#[test]
fn caller_deadline_is_checked_at_entry_around_allocations_and_terminal_completion() {
    let outputs = fixture_outputs();
    let deadline = future_deadline();
    let expired = deadline + Duration::from_nanos(1);
    let mut expired_now = || expired;
    assert!(matches!(
        prepare_fixture_with_policy_and_clocks(
            &outputs,
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            DESIRED_PUBLICATION_POLICY,
            deadline,
            &mut expired_now,
            || expired,
        ),
        Err(ActiveReblitDesiredPublicationError::DeadlineExceeded {
            checkpoint: "inventory entry"
        })
    ));

    let safe = Instant::now();
    let mut safe_now = || safe;
    assert!(matches!(
        prepare_fixture_with_policy_and_clocks(
            &outputs,
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            DESIRED_PUBLICATION_POLICY,
            deadline,
            &mut safe_now,
            || expired,
        ),
        Err(ActiveReblitDesiredPublicationError::DeadlineExceeded {
            checkpoint: "terminal desired-publication inventory"
        })
    ));

    let allocation_calls = Cell::new(0usize);
    let mut expires_after_path_allocation = || {
        let call = allocation_calls.get().saturating_add(1);
        allocation_calls.set(call);
        if call >= 7 { expired } else { safe }
    };
    assert!(matches!(
        prepare_fixture_with_policy_and_clocks(
            &outputs,
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            DESIRED_PUBLICATION_POLICY,
            deadline,
            &mut expires_after_path_allocation,
            || safe,
        ),
        Err(ActiveReblitDesiredPublicationError::DeadlineExceeded {
            checkpoint: "post-path allocation"
        })
    ));
    assert_eq!(allocation_calls.get(), 7);
}

#[test]
fn output_count_mismatch_fails_closed_without_materializing_an_inventory() {
    let mut now = Instant::now;
    let builder = DesiredPublicationBuilder::new(
        ActiveReblitBootDestinationLayout::BootAliasesEsp,
        1,
        DESIRED_PUBLICATION_POLICY,
        future_deadline(),
        &mut now,
    )
    .unwrap();
    assert!(matches!(
        builder.finish(Instant::now),
        Err(ActiveReblitDesiredPublicationError::PublicationCountMismatch { expected: 1, actual: 0 })
    ));
}
