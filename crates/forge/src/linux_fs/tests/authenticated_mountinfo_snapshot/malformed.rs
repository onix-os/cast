use std::io;

use super::super::super::{
    mount_namespace::{
        FIXTURE_MOUNTINFO_PROCFS_MAGIC, FixtureMountInfoSnapshotLimits, validate_fixture_mountinfo_file_authentication,
    },
    mountinfo::MOUNTINFO_LIMITS,
};
use super::support::{RECORD, SyntheticMountInfoContext, assert_error_kind, deadline};

fn read(bytes: &[u8]) -> io::Result<super::super::super::mount_namespace::AuthenticatedMountInfoSnapshot> {
    let fixture = SyntheticMountInfoContext::stable()?;
    let prepared = fixture.prepared()?;
    prepared.read_fixture_mountinfo_bytes_with(
        bytes,
        FixtureMountInfoSnapshotLimits::default(),
        deadline(),
        &mut |_| Ok(()),
    )
}

#[test]
fn empty_unterminated_and_nul_snapshots_fail_closed() {
    assert_error_kind(read(b""), io::ErrorKind::UnexpectedEof);
    assert_error_kind(read(&RECORD[..RECORD.len() - 1]), io::ErrorKind::UnexpectedEof);
    let mut nul = RECORD.to_vec();
    nul[8] = 0;
    assert_error_kind(read(&nul), io::ErrorKind::InvalidData);
}

#[test]
fn oversized_cursor_snapshot_is_rejected_without_truncation() {
    let oversized = vec![b'x'; MOUNTINFO_LIMITS.max_bytes + 1];
    assert_error_kind(read(&oversized), io::ErrorKind::InvalidData);
}

#[test]
fn pure_file_classifier_accepts_only_stable_regular_procfs_identity() {
    validate_fixture_mountinfo_file_authentication(
        FIXTURE_MOUNTINFO_PROCFS_MAGIC,
        FIXTURE_MOUNTINFO_PROCFS_MAGIC,
        7,
        11,
        nix::libc::S_IFREG,
        7,
        11,
        nix::libc::S_IFREG,
    )
    .unwrap();
}

#[test]
fn pure_file_classifier_rejects_nonproc_wrong_kind_and_zero_identity() {
    for result in [
        validate_fixture_mountinfo_file_authentication(
            0,
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            7,
            11,
            nix::libc::S_IFREG,
            7,
            11,
            nix::libc::S_IFREG,
        ),
        validate_fixture_mountinfo_file_authentication(
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            0,
            7,
            11,
            nix::libc::S_IFREG,
            7,
            11,
            nix::libc::S_IFREG,
        ),
        validate_fixture_mountinfo_file_authentication(
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            7,
            11,
            nix::libc::S_IFDIR,
            7,
            11,
            nix::libc::S_IFDIR,
        ),
        validate_fixture_mountinfo_file_authentication(
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            0,
            11,
            nix::libc::S_IFREG,
            0,
            11,
            nix::libc::S_IFREG,
        ),
    ] {
        assert_error_kind(result, io::ErrorKind::InvalidData);
    }
}

#[test]
fn pure_file_classifier_rejects_identity_and_terminal_kind_changes() {
    for result in [
        validate_fixture_mountinfo_file_authentication(
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            7,
            11,
            nix::libc::S_IFREG,
            7,
            12,
            nix::libc::S_IFREG,
        ),
        validate_fixture_mountinfo_file_authentication(
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            FIXTURE_MOUNTINFO_PROCFS_MAGIC,
            7,
            11,
            nix::libc::S_IFREG,
            7,
            11,
            nix::libc::S_IFLNK,
        ),
    ] {
        assert_error_kind(result, io::ErrorKind::InvalidData);
    }
}
