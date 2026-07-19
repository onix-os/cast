use std::time::{Duration, Instant};

use super::super::{
    ObservationPhase,
    capture::{ActiveReblitMountedBootTopologyCaptureError as Error, ObservationBoundary},
};
use super::support::{AliasFixture, deadline};

#[test]
fn expired_caller_deadline_is_rejected_at_coordinator_entry() {
    let fixture = AliasFixture::stable().unwrap();
    let expired = Instant::now() - Duration::from_secs(1);

    let error = fixture.prepare_until(expired).unwrap_err();
    assert!(matches!(
        error,
        Error::DeadlineExceeded {
            phase: ObservationPhase::Bootstrap,
            boundary: ObservationBoundary::Preparation,
            deadline,
        } if deadline == expired
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn successful_fixture_revalidation_retains_exact_caller_deadline() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    let operation_deadline = deadline();
    let admitted = Instant::now();
    let mut clock = || admitted;

    let view = prepared
        .revalidate_fixture_until_with_clock(&fixture.installation, operation_deadline, &mut clock)
        .unwrap();

    assert_eq!(view.deadline(), operation_deadline);
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_at_the_final_terminal_checkpoint_cannot_return_a_view() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    let operation_deadline = deadline();
    let admitted = Instant::now();
    let expired = operation_deadline + Duration::from_nanos(1);
    let mut calls = 0usize;
    let mut clock = || {
        calls += 1;
        if calls == 9 { expired } else { admitted }
    };

    let error = prepared
        .revalidate_fixture_until_with_clock(&fixture.installation, operation_deadline, &mut clock)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::DeadlineExceeded {
            phase: ObservationPhase::Terminal,
            boundary: ObservationBoundary::Terminal,
            deadline,
        } if deadline == operation_deadline
    ));
    assert_eq!(
        calls, 9,
        "the same clock reached entry plus pre/post-consumer checks in all three passes"
    );
    fixture.assert_outside_unchanged();
}
