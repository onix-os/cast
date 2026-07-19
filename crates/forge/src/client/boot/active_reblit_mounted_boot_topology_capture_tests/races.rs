use super::super::capture::{ActiveReblitMountedBootTopologyCaptureError as Error, ObservationBoundary};
use super::super::{BootTargetRole, ObservationPhase};
use super::support::{AliasFixture, deadline};

#[test]
fn changed_declarative_intent_fails_at_the_opening_boundary() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    fixture.change_intent_source().unwrap();

    let error = prepared
        .revalidate_until(&fixture.installation, deadline())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Intent {
            phase: ObservationPhase::Pass1,
            boundary: ObservationBoundary::Opening,
            ..
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn changed_mount_namespace_identity_fails_before_attachment_use() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    fixture.replace_namespace_identity().unwrap();

    let error = prepared
        .revalidate_until(&fixture.installation, deadline())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::MountNamespace {
            phase: ObservationPhase::Pass1,
            boundary: ObservationBoundary::Opening,
            ..
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn changed_attachment_identity_fails_before_mountinfo_selection() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    fixture.replace_attachment_identity().unwrap();

    let error = prepared
        .revalidate_until(&fixture.installation, deadline())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Attachment {
            phase: ObservationPhase::Pass1,
            role: BootTargetRole::Esp,
            boundary: ObservationBoundary::Opening,
            ..
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn changed_mountinfo_identity_is_a_role_typed_selection_failure() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    fixture.replace_mountinfo_with_wrong_mount_id();

    let error = prepared
        .revalidate_until(&fixture.installation, deadline())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::MountInfoSelection {
            phase: ObservationPhase::Pass1,
            role: BootTargetRole::Esp,
            ..
        }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn changed_sysfs_identity_fails_after_exact_mountinfo_selection() {
    let fixture = AliasFixture::stable().unwrap();
    let feed = fixture.feed();
    let prepared = fixture.prepare().unwrap();
    let reads_before = feed.read_count();
    fixture.change_sysfs_partuuid().unwrap();

    let error = prepared
        .revalidate_until(&fixture.installation, deadline())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Sysfs {
            phase: ObservationPhase::Pass1,
            role: BootTargetRole::Esp,
            boundary: ObservationBoundary::Opening,
            ..
        }
    ));
    assert_eq!(
        feed.read_count(),
        reads_before + 1,
        "mountinfo selection preceded sysfs failure"
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn attachment_selector_mismatch_is_role_typed_before_mountinfo_use() {
    let error = super::super::capture::validate_fixture_attachment_selector(
        ObservationPhase::Pass1,
        BootTargetRole::Esp,
        "/declared-firmware",
        "/retained-other",
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::AttachmentSelectorMismatch {
            phase: ObservationPhase::Pass1,
            role: BootTargetRole::Esp,
        }
    ));
}
