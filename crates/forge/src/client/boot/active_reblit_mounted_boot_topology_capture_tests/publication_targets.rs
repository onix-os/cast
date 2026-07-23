use std::time::{Duration, Instant};

use super::super::{
    BootTargetRole, ObservationPhase,
    capture::{
        ActiveReblitBootPublicationTargetsError as Error,
        ActiveReblitMountedBootTopologyCaptureError, ObservationBoundary,
        RevalidatedActiveReblitBootPublicationTargets,
        validate_fixture_publication_target_binding,
    },
};
use super::support::{AliasFixture, MOUNT_POINT, deadline};

#[test]
fn alias_bridge_brackets_exact_attachment_and_retains_original_deadline() {
    let fixture = AliasFixture::stable().unwrap();
    let feed = fixture.feed();
    let operation_deadline = deadline();
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();

    let targets = topology.revalidate_publication_targets().unwrap();

    let RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { esp } = &targets else {
        panic!("alias topology returned distinct publication targets")
    };
    let (device, inode) = fixture.destination_identity();
    assert_eq!(esp.role(), BootTargetRole::Esp);
    assert_eq!((esp.destination().raw_device(), esp.destination().inode()), (device, inode));
    assert_eq!(esp.mount_id(), inode);
    assert_eq!(esp.deadline(), operation_deadline);
    assert_eq!(targets.deadline(), operation_deadline);
    assert_eq!(
        feed.read_count(),
        13,
        "bootstrap, initial view, and opening/closing bridge topology passes all ran"
    );
    let debug = format!("{targets:?}");
    assert!(debug.contains("descriptor hidden"));
    assert!(!debug.contains(MOUNT_POINT));
    fixture.assert_outside_unchanged();
}

#[test]
fn bridge_rejects_intent_drift_in_its_opening_complete_topology_pass() {
    let fixture = AliasFixture::stable().unwrap();
    let operation_deadline = deadline();
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();
    fixture.change_intent_source().unwrap();

    let error = topology.revalidate_publication_targets().unwrap_err();

    assert!(matches!(
        error,
        Error::OpeningTopology {
            source: ActiveReblitMountedBootTopologyCaptureError::Intent {
                phase: ObservationPhase::Pass1,
                boundary: ObservationBoundary::Opening,
                ..
            }
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn bridge_rejects_attachment_drift_in_its_closing_complete_topology_pass() {
    let fixture = AliasFixture::stable().unwrap();
    let operation_deadline = deadline();
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();
    let mut now = Instant::now;

    let error = topology
        .revalidate_publication_targets_fixture_with(&mut now, || {
            fixture.replace_attachment_identity().unwrap();
        })
        .unwrap_err();

    assert!(matches!(
        error,
        Error::ClosingTopology {
            source: ActiveReblitMountedBootTopologyCaptureError::Attachment {
                phase: ObservationPhase::Pass1,
                role: BootTargetRole::Esp,
                boundary: ObservationBoundary::Opening,
                ..
            }
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn bridge_terminal_checkpoint_cannot_outlive_original_deadline() {
    let fixture = AliasFixture::stable().unwrap();
    let operation_deadline = deadline();
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();
    let admitted = Instant::now();
    let expired = operation_deadline + Duration::from_nanos(1);
    let mut calls = 0usize;
    let mut now = || {
        calls += 1;
        if calls == 4 { expired } else { admitted }
    };

    let error = topology
        .revalidate_publication_targets_fixture_with(&mut now, || {})
        .unwrap_err();

    assert!(matches!(
        error,
        Error::DeadlineExceeded {
            checkpoint: "terminal",
            deadline,
        } if deadline == operation_deadline
    ));
    assert_eq!(calls, 4);
    fixture.assert_outside_unchanged();
}

#[test]
fn scalar_binding_rejects_each_role_typed_identity_component_drift() {
    for role in [BootTargetRole::Esp, BootTargetRole::Xbootldr] {
        validate_fixture_publication_target_binding(role, (10, 20, 30), (10, 20, 30)).unwrap();
        for found in [(11, 20, 30), (10, 21, 30), (10, 20, 31)] {
            let error = validate_fixture_publication_target_binding(role, (10, 20, 30), found).unwrap_err();
            assert!(matches!(
                error,
                Error::TargetIdentityMismatch {
                    role: found_role,
                    expected_device: 10,
                    expected_inode: 20,
                    expected_mount_id: 30,
                    found_device,
                    found_inode,
                    found_mount_id,
                } if found_role == role
                    && (found_device, found_inode, found_mount_id) == found
            ));
        }
    }
}
