use std::{
    ffi::CString,
    fs, io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use crate::tree_marker::TreeMarkerError;

use super::*;

#[test]
fn duplicate_permanent_tree_token_across_states_is_a_hard_failure() {
    let fixture = Fixture::new();
    let archived = state(86);
    fixture.create_archive(archived);
    let head_marker = fixture.installation.root.join("usr/.cast-tree-id");
    let archived_marker = fixture.archive_path(archived).join("usr/.cast-tree-id");
    fs::set_permissions(&archived_marker, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&archived_marker, fs::read(head_marker).unwrap()).unwrap();
    fs::set_permissions(&archived_marker, fs::Permissions::from_mode(0o444)).unwrap();

    assert!(matches!(
        fixture.prepare(&[head_state(), archived]),
        Err(ActiveReblitBootStateRootsError::DuplicateTreeToken {
            first_state,
            second_state,
            ..
        }) if first_state == i32::from(head_state()) && second_state == i32::from(archived)
    ));
}

#[test]
fn runtime_epoch_change_blocks_descriptor_view_creation() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare(&[head_state()]).unwrap();
    arm_runtime_epoch_mismatch();
    assert!(matches!(
        prepared.revalidate(&fixture.installation),
        Err(ActiveReblitBootStateRootsError::RuntimeEpochChanged { .. })
    ));
}

#[test]
fn operational_and_retry_errors_are_never_structural_exclusions() {
    let path = PathBuf::from("/retained/usr/.stateID");
    let permission = IdentityError::LiveUsr {
        operation: "open retained state ID",
        path: path.clone(),
        source: io::Error::from_raw_os_error(nix::libc::EACCES),
    };
    assert!(!identity_error_is_structural(&permission));

    let retry_exhaustion = IdentityError::LiveUsr {
        operation: "read complete retained state ID",
        path: path.clone(),
        source: io::Error::other("bounded retry exhaustion"),
    };
    assert!(!identity_error_is_structural(&retry_exhaustion));

    let malformed = IdentityError::LiveUsr {
        operation: "validate retained state ID contents",
        path: path.clone(),
        source: io::Error::other("stable malformed contents"),
    };
    assert!(identity_error_is_structural(&malformed));

    let marker_timeout = IdentityError::TreeMarker(TreeMarkerError::Io {
        operation: "read tree marker within interrupted retry bound",
        path,
        source: io::Error::new(io::ErrorKind::TimedOut, "retry exhaustion"),
    });
    assert!(!identity_error_is_structural(&marker_timeout));
}

#[test]
fn retained_state_id_reads_preserve_atime() {
    let fixture = Fixture::new();
    let state_id = fixture.installation.root.join("usr/.stateID");
    set_old_atime(&state_id);
    let before = fs::metadata(&state_id).unwrap();

    let prepared = fixture.prepare(&[head_state()]).unwrap();
    let _revalidated = prepared.revalidate(&fixture.installation).unwrap();

    let after = fs::metadata(&state_id).unwrap();
    assert_eq!(
        (after.atime(), after.atime_nsec()),
        (before.atime(), before.atime_nsec())
    );
}

#[test]
fn retained_archived_slot_marker_reads_preserve_atime() {
    let fixture = Fixture::new();
    let archived = state(87);
    let token = fixture.create_archive(archived);
    let wrapper = fixture.archive_path(archived);
    let canonical_marker = wrapper.join("usr/.cast-tree-id");
    let slot_marker = wrapper.join(format!(".cast-state-slot-{archived}-{token}"));
    fs::hard_link(&canonical_marker, &slot_marker).unwrap();
    set_old_atime(&slot_marker);
    let before = fs::metadata(&slot_marker).unwrap();

    let prepared = fixture.prepare(&[head_state(), archived]).unwrap();
    let _revalidated = prepared.revalidate(&fixture.installation).unwrap();

    let after = fs::metadata(&slot_marker).unwrap();
    assert_eq!(
        (after.atime(), after.atime_nsec()),
        (before.atime(), before.atime_nsec())
    );
}

fn set_old_atime(path: &Path) {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let times = [
        nix::libc::timespec { tv_sec: 1, tv_nsec: 0 },
        nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: nix::libc::UTIME_OMIT,
        },
    ];
    // SAFETY: the path is NUL-terminated and `times` contains two initialized
    // timespec values as required by utimensat.
    assert_eq!(
        unsafe { nix::libc::utimensat(nix::libc::AT_FDCWD, encoded.as_ptr(), times.as_ptr(), 0) },
        0
    );
}
