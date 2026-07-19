use std::time::Instant;

use super::super::capture::ActiveReblitMountedBootTopologyCaptureError as Error;
use super::super::{BootTargetRole, BoundActiveReblitMountedBootTopology, ObservationPhase};
use super::support::{AliasFixture, DistinctBootstrapFixture, deadline};
use crate::linux_fs::descriptor_boot_filesystem::{
    BootFilesystemAuthenticationError, BootFilesystemMagicFamily, FIXTURE_MSDOS_SUPER_MAGIC,
};

const WRONG_MAGIC: nix::libc::c_long = 0xef53;

#[test]
fn stable_msdos_family_evidence_is_retained_in_every_observation() {
    let fixture = AliasFixture::stable().unwrap();
    let feed = fixture.boot_filesystem_feed();
    let prepared = fixture.prepare().unwrap();

    assert_eq!(feed.read_count(), 4);
    let view = prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(feed.read_count(), 7, "each later pass authenticates again");
    let BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp } = view.topology() else {
        panic!("alias fixture must retain exactly one ESP target");
    };
    assert_eq!(esp.boot_filesystem.destination_device(), esp.destination.raw_device);
    assert_eq!(esp.boot_filesystem.destination_inode(), esp.destination.inode);
    assert_eq!(
        esp.boot_filesystem.magic_family(),
        BootFilesystemMagicFamily::LinuxMsdos
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn wrong_boot_filesystem_magic_is_a_bootstrap_role_typed_failure() {
    let fixture = AliasFixture::stable().unwrap();
    let (device, inode) = fixture.destination_identity();
    fixture.replace_boot_filesystem_evidence(device, inode, WRONG_MAGIC);

    let error = fixture.prepare().unwrap_err();
    assert!(matches!(
        error,
        Error::BootFilesystem {
            phase: ObservationPhase::Bootstrap,
            role: BootTargetRole::Esp,
            source: BootFilesystemAuthenticationError::UnsupportedFilesystemMagic { found, .. },
        } if found == WRONG_MAGIC
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn boot_filesystem_identity_mismatch_is_a_bootstrap_role_typed_failure() {
    let fixture = AliasFixture::stable().unwrap();
    let (device, inode) = fixture.destination_identity();
    fixture.replace_boot_filesystem_evidence(device, inode.saturating_add(1), FIXTURE_MSDOS_SUPER_MAGIC);

    let error = fixture.prepare().unwrap_err();
    assert!(matches!(
        error,
        Error::BootFilesystem {
            phase: ObservationPhase::Bootstrap,
            role: BootTargetRole::Esp,
            source: BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity {
                expected_device,
                expected_inode,
                found_device,
                found_inode,
            },
        } if expected_device == device
            && expected_inode == inode
            && found_device == device
            && found_inode == inode.saturating_add(1)
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn distinct_bootstrap_consumes_both_feeds_and_types_xbootldr_wrong_magic() {
    let fixture = DistinctBootstrapFixture::stable().unwrap();
    let (esp_feed, xbootldr_feed) = fixture.filesystem_feeds();
    let (device, inode) = fixture.xbootldr_identity();
    fixture.replace_xbootldr_evidence(device, inode, WRONG_MAGIC);

    let error = fixture.prepare().unwrap_err();
    assert!(matches!(
        error,
        Error::BootFilesystem {
            phase: ObservationPhase::Bootstrap,
            role: BootTargetRole::Xbootldr,
            source: BootFilesystemAuthenticationError::UnsupportedFilesystemMagic { found, .. },
        } if found == WRONG_MAGIC
    ));
    assert_eq!(esp_feed.read_count(), 1, "the ESP feed is independently consumed first");
    assert_eq!(
        xbootldr_feed.read_count(),
        1,
        "the distinct XBOOTLDR feed reports its own failure"
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn distinct_bootstrap_types_xbootldr_identity_mismatch_after_both_feeds() {
    let fixture = DistinctBootstrapFixture::stable().unwrap();
    let (esp_feed, xbootldr_feed) = fixture.filesystem_feeds();
    let (device, inode) = fixture.xbootldr_identity();
    fixture.replace_xbootldr_evidence(device, inode.saturating_add(1), FIXTURE_MSDOS_SUPER_MAGIC);

    let error = fixture.prepare().unwrap_err();
    assert!(matches!(
        error,
        Error::BootFilesystem {
            phase: ObservationPhase::Bootstrap,
            role: BootTargetRole::Xbootldr,
            source: BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity {
                expected_device,
                expected_inode,
                found_device,
                found_inode,
            },
        } if expected_device == device
            && expected_inode == inode
            && found_device == device
            && found_inode == inode.saturating_add(1)
    ));
    assert_eq!(esp_feed.read_count(), 1);
    assert_eq!(xbootldr_feed.read_count(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn evidence_drift_is_rejected_in_pass2_and_terminal_observations() {
    for (switch_at_clock_call, expected_phase) in
        [(4usize, ObservationPhase::Pass2), (7usize, ObservationPhase::Terminal)]
    {
        let fixture = AliasFixture::stable().unwrap();
        let feed = fixture.boot_filesystem_feed();
        let prepared = fixture.prepare().unwrap();
        let (device, inode) = fixture.destination_identity();
        let operation_deadline = deadline();
        let admitted = Instant::now();
        let mut clock_calls = 0usize;
        let mut clock = || {
            clock_calls += 1;
            if clock_calls == switch_at_clock_call {
                feed.replace_stable(device, inode.saturating_add(1), FIXTURE_MSDOS_SUPER_MAGIC);
            }
            admitted
        };

        let error = prepared
            .revalidate_fixture_until_with_clock(&fixture.installation, operation_deadline, &mut clock)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::BootFilesystem {
                phase,
                role: BootTargetRole::Esp,
                source: BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity { .. },
            } if phase == expected_phase
        ));
        assert_eq!(clock_calls, switch_at_clock_call);
        fixture.assert_outside_unchanged();
    }
}
