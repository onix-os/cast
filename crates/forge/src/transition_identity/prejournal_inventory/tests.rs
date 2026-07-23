use std::{
    ffi::CString,
    fs::{self, File},
    io,
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use nix::sys::stat::Mode;
use tempfile::TempDir;

use super::{
    CandidateInventoryBoundary, CandidateInventoryError, CandidateInventoryLimits, RetainedCandidateDurabilitySeal,
    WorkBudget, filesystem::open_relative,
};
use crate::tree_marker::{RetainedTreeMarker, TreeMarkerStore};

struct PublishedFixture {
    _temporary: TempDir,
    path: PathBuf,
    seal: RetainedCandidateDurabilitySeal,
    _marker: RetainedTreeMarker,
}

fn candidate_root(path: &Path) -> File {
    normalize_candidate_modes(path);
    File::open(path).unwrap()
}

fn raw_candidate_root(path: &Path) -> File {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    File::open(path).unwrap()
}

fn normalize_candidate_modes(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.file_type().is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        for entry in fs::read_dir(path).unwrap() {
            normalize_candidate_modes(&entry.unwrap().path());
        }
    } else if metadata.file_type().is_file() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}

fn seal(root: &File, path: &Path) -> RetainedCandidateDurabilitySeal {
    RetainedCandidateDurabilitySeal::seal_before_marker(root, path, CandidateInventoryLimits::default()).unwrap()
}

fn publish_marker(root: &File, path: &Path) -> RetainedTreeMarker {
    TreeMarkerStore::open(root, path)
        .unwrap()
        .adopt_or_create_before_journal()
        .unwrap()
}

fn published_payload(bytes: &[u8]) -> PublishedFixture {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    fs::write(path.join("payload"), bytes).unwrap();
    let root = candidate_root(&path);
    let seal = seal(&root, &path);
    let marker = publish_marker(&root, &path);
    PublishedFixture {
        _temporary: temporary,
        path,
        seal,
        _marker: marker,
    }
}

fn assert_changed(result: Result<(), CandidateInventoryError>) {
    assert!(
        matches!(
            result,
            Err(CandidateInventoryError::EntryChanged { .. }
                | CandidateInventoryError::ChildNamesChanged { .. }
                | CandidateInventoryError::SymlinkTargetChanged { .. }
                | CandidateInventoryError::MarkerChanged { .. }
                | CandidateInventoryError::MarkerMissingAfterPublication { .. }
                | CandidateInventoryError::Io { .. })
        ),
        "unexpected candidate validation result: {result:?}"
    );
}

#[test]
fn candidate_pre_journal_nested_tree_seals_and_allows_sole_new_marker() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path();
    fs::create_dir_all(path.join("bin/nested")).unwrap();
    fs::write(path.join("bin/tool"), b"declarative payload").unwrap();
    let raw_name = std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', 0xff]);
    fs::write(path.join("bin/nested").join(raw_name), b"raw name").unwrap();
    symlink("../tool", path.join("bin/tool-link")).unwrap();

    let root = candidate_root(path);
    let seal = seal(&root, path);
    assert_eq!(seal.baseline_entry_count(), 5);
    let _marker = publish_marker(&root, path);
    seal.validate_after_marker().unwrap();

    fs::write(path.join("foreign"), b"not part of the sole delta").unwrap();
    fs::set_permissions(path.join("foreign"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        seal.validate_after_marker(),
        Err(CandidateInventoryError::ChildNamesChanged { .. })
    ));
}

#[test]
fn candidate_pre_journal_existing_marker_is_adopted_without_delta() {
    let outer = tempfile::tempdir().unwrap();
    let path = outer.path().join("candidate");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("payload"), b"existing identity").unwrap();
    let root = candidate_root(&path);
    let marker = publish_marker(&root, &path);

    let initial_seal = seal(&root, &path);
    initial_seal.validate_after_marker().unwrap();

    let slot_link = outer.path().join("authenticated-slot-link");
    fs::hard_link(path.join(".cast-tree-id"), &slot_link).unwrap();
    let two_link_seal = seal(&root, &path);
    two_link_seal.validate_after_marker().unwrap();
    drop(marker);
}

#[test]
fn candidate_pre_journal_add_delete_replace_and_content_changes_fail() {
    let added = published_payload(b"baseline");
    fs::write(added.path.join("added"), b"foreign").unwrap();
    fs::set_permissions(added.path.join("added"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_changed(added.seal.validate_after_marker());

    let deleted = published_payload(b"baseline");
    fs::remove_file(deleted.path.join("payload")).unwrap();
    assert_changed(deleted.seal.validate_after_marker());

    let replaced = published_payload(b"baseline");
    fs::remove_file(replaced.path.join("payload")).unwrap();
    fs::write(replaced.path.join("payload"), b"baseline").unwrap();
    fs::set_permissions(replaced.path.join("payload"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_changed(replaced.seal.validate_after_marker());

    let changed = published_payload(b"baseline");
    fs::write(changed.path.join("payload"), b"different-length payload").unwrap();
    assert_changed(changed.seal.validate_after_marker());
}

#[test]
fn candidate_pre_journal_same_metadata_content_rewrite_fails() {
    let fixture = published_payload(b"same-size-a");
    let payload = fixture.path.join("payload");
    let metadata = fs::metadata(&payload).unwrap();
    let modified = filetime::FileTime::from_last_modification_time(&metadata);
    let accessed = filetime::FileTime::from_last_access_time(&metadata);
    fs::write(&payload, b"same-size-b").unwrap();
    filetime::set_file_times(&payload, accessed, modified).unwrap();
    assert_eq!(fs::metadata(&payload).unwrap().size(), metadata.size());
    assert!(matches!(
        fixture.seal.validate_after_marker(),
        Err(CandidateInventoryError::EntryChanged {
            field: "content digest",
            ..
        })
    ));
}

#[test]
fn candidate_pre_journal_symlink_is_opaque_and_target_change_fails() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path();
    fs::write(path.join("payload"), b"payload").unwrap();
    symlink("/proc/self/fd/0", path.join("magic-looking-link")).unwrap();
    let root = candidate_root(path);
    let seal = seal(&root, path);
    let _marker = publish_marker(&root, path);
    seal.validate_after_marker().unwrap();

    fs::remove_file(path.join("magic-looking-link")).unwrap();
    symlink("/proc/self/fd/1", path.join("magic-looking-link")).unwrap();
    assert_changed(seal.validate_after_marker());
}

#[test]
fn candidate_pre_journal_special_mount_and_hardlink_entries_fail() {
    let special = tempfile::tempdir().unwrap();
    nix::unistd::mkfifo(&special.path().join("fifo"), Mode::from_bits_truncate(0o600)).unwrap();
    let root = candidate_root(special.path());
    assert!(matches!(
        RetainedCandidateDurabilitySeal::seal_before_marker(&root, special.path(), CandidateInventoryLimits::default()),
        Err(CandidateInventoryError::SpecialInode { .. })
    ));

    let linked = tempfile::tempdir().unwrap();
    fs::write(linked.path().join("one"), b"payload").unwrap();
    fs::hard_link(linked.path().join("one"), linked.path().join("two")).unwrap();
    let root = candidate_root(linked.path());
    assert!(matches!(
        RetainedCandidateDurabilitySeal::seal_before_marker(&root, linked.path(), CandidateInventoryLimits::default()),
        Err(CandidateInventoryError::UnexpectedHardlink { .. })
    ));

    let root_mount = File::open("/").unwrap();
    let mut budget = WorkBudget::new(CandidateInventoryLimits::default(), Path::new("/")).unwrap();
    assert!(matches!(
        open_relative(
            &root_mount,
            c"proc",
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            Path::new("/proc"),
            "probe mounted entry",
            &mut budget,
        ),
        Err(CandidateInventoryError::MountedEntry { .. })
    ));
}

#[test]
fn candidate_pre_journal_entry_depth_and_name_bounds_are_inclusive() {
    let entries = tempfile::tempdir().unwrap();
    fs::write(entries.path().join("a"), b"").unwrap();
    fs::write(entries.path().join("b"), b"").unwrap();
    let root = candidate_root(entries.path());
    let exact = CandidateInventoryLimits {
        entries: 2,
        name_bytes: 2,
        ..CandidateInventoryLimits::default()
    };
    RetainedCandidateDurabilitySeal::seal_before_marker(&root, entries.path(), exact).unwrap();
    let too_few = CandidateInventoryLimits { entries: 1, ..exact };
    assert_boundary(
        RetainedCandidateDurabilitySeal::seal_before_marker(&root, entries.path(), too_few),
        CandidateInventoryBoundary::EntryCount,
    );

    let nested = tempfile::tempdir().unwrap();
    fs::create_dir(nested.path().join("a")).unwrap();
    fs::write(nested.path().join("a/b"), b"").unwrap();
    let root = candidate_root(nested.path());
    let exact = CandidateInventoryLimits {
        depth: 2,
        ..CandidateInventoryLimits::default()
    };
    RetainedCandidateDurabilitySeal::seal_before_marker(&root, nested.path(), exact).unwrap();
    assert_boundary(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            nested.path(),
            CandidateInventoryLimits { depth: 1, ..exact },
        ),
        CandidateInventoryBoundary::Depth,
    );

    let names = tempfile::tempdir().unwrap();
    fs::write(names.path().join("four"), b"").unwrap();
    let root = candidate_root(names.path());
    let exact = CandidateInventoryLimits {
        name_bytes: 4,
        ..CandidateInventoryLimits::default()
    };
    RetainedCandidateDurabilitySeal::seal_before_marker(&root, names.path(), exact).unwrap();
    assert_boundary(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            names.path(),
            CandidateInventoryLimits { name_bytes: 3, ..exact },
        ),
        CandidateInventoryBoundary::NameBytes,
    );

    let marker_only = tempfile::tempdir().unwrap();
    let root = candidate_root(marker_only.path());
    let _marker = publish_marker(&root, marker_only.path());
    let zero_payload = CandidateInventoryLimits {
        entries: 0,
        name_bytes: 0,
        regular_bytes: 0,
        ..CandidateInventoryLimits::default()
    };
    let marker_only_seal =
        RetainedCandidateDurabilitySeal::seal_before_marker(&root, marker_only.path(), zero_payload).unwrap();
    marker_only_seal.validate_after_marker().unwrap();

    let nested_marker = tempfile::tempdir().unwrap();
    fs::create_dir(nested_marker.path().join("nested")).unwrap();
    fs::write(nested_marker.path().join("nested/.cast-tree-id"), b"").unwrap();
    let root = candidate_root(nested_marker.path());
    let exact_nested_marker = CandidateInventoryLimits {
        entries: 2,
        depth: 2,
        name_bytes: b"nested.cast-tree-id".len(),
        regular_bytes: 0,
        ..CandidateInventoryLimits::default()
    };
    RetainedCandidateDurabilitySeal::seal_before_marker(&root, nested_marker.path(), exact_nested_marker).unwrap();
    assert_boundary(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            nested_marker.path(),
            CandidateInventoryLimits {
                name_bytes: exact_nested_marker.name_bytes - 1,
                ..exact_nested_marker
            },
        ),
        CandidateInventoryBoundary::NameBytes,
    );
}

#[test]
fn candidate_pre_journal_regular_byte_bound_is_inclusive() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("payload"), b"four").unwrap();
    let root = candidate_root(temporary.path());
    let exact = CandidateInventoryLimits {
        regular_bytes: 4,
        ..CandidateInventoryLimits::default()
    };
    RetainedCandidateDurabilitySeal::seal_before_marker(&root, temporary.path(), exact).unwrap();
    assert_boundary(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            temporary.path(),
            CandidateInventoryLimits {
                regular_bytes: 3,
                ..exact
            },
        ),
        CandidateInventoryBoundary::RegularBytes,
    );
}

#[test]
fn candidate_pre_journal_operation_and_deadline_bounds_are_inclusive() {
    let path = Path::new("candidate");
    let limits = CandidateInventoryLimits {
        operations: 2,
        ..CandidateInventoryLimits::default()
    };
    let mut budget = WorkBudget::new(limits, path).unwrap();
    budget.operation(path).unwrap();
    budget.operation(path).unwrap();
    assert!(matches!(
        budget.operation(path),
        Err(CandidateInventoryError::Boundary {
            boundary: CandidateInventoryBoundary::OperationCount,
            limit: 2,
            ..
        })
    ));

    assert!(matches!(
        WorkBudget::new(
            CandidateInventoryLimits {
                time: Duration::ZERO,
                ..CandidateInventoryLimits::default()
            },
            path,
        ),
        Err(CandidateInventoryError::Deadline { .. })
    ));
}

#[test]
fn candidate_pre_journal_access_and_default_acls_fail() {
    let access = tempfile::tempdir().unwrap();
    let payload = access.path().join("payload");
    fs::write(&payload, b"acl").unwrap();
    if set_posix_acl(&payload, c"system.posix_acl_access").unwrap() {
        let root = candidate_root(access.path());
        assert!(matches!(
            RetainedCandidateDurabilitySeal::seal_before_marker(
                &root,
                access.path(),
                CandidateInventoryLimits::default()
            ),
            Err(CandidateInventoryError::Io {
                operation: "reject POSIX access ACL on regular file",
                ..
            })
        ));
    }

    let defaults = tempfile::tempdir().unwrap();
    let directory = defaults.path().join("directory");
    fs::create_dir(&directory).unwrap();
    if set_posix_acl(&directory, c"system.posix_acl_default").unwrap() {
        let root = candidate_root(defaults.path());
        assert!(matches!(
            RetainedCandidateDurabilitySeal::seal_before_marker(
                &root,
                defaults.path(),
                CandidateInventoryLimits::default()
            ),
            Err(CandidateInventoryError::Io {
                operation: "reject POSIX default ACL on directory",
                ..
            })
        ));
    }

    let file_xattr = tempfile::tempdir().unwrap();
    let payload = file_xattr.path().join("payload");
    fs::write(&payload, b"xattr").unwrap();
    let root = candidate_root(file_xattr.path());
    if set_test_xattr(&payload).unwrap() {
        assert!(matches!(
            RetainedCandidateDurabilitySeal::seal_before_marker(
                &root,
                file_xattr.path(),
                CandidateInventoryLimits::default()
            ),
            Err(CandidateInventoryError::ExtendedAttributes { .. })
        ));
    }

    let root_xattr = tempfile::tempdir().unwrap();
    let root = candidate_root(root_xattr.path());
    if set_test_xattr(root_xattr.path()).unwrap() {
        assert!(matches!(
            RetainedCandidateDurabilitySeal::seal_before_marker(
                &root,
                root_xattr.path(),
                CandidateInventoryLimits::default()
            ),
            Err(CandidateInventoryError::ExtendedAttributes { .. })
        ));
    }

    let marker_xattr = tempfile::tempdir().unwrap();
    let root = candidate_root(marker_xattr.path());
    let seal = seal(&root, marker_xattr.path());
    let _marker = publish_marker(&root, marker_xattr.path());
    if set_test_xattr(&marker_xattr.path().join(".cast-tree-id")).unwrap() {
        assert!(matches!(
            seal.validate_after_marker(),
            Err(CandidateInventoryError::ExtendedAttributes { .. })
        ));
    }

    let writable = tempfile::tempdir().unwrap();
    let payload = writable.path().join("payload");
    fs::write(&payload, b"writable").unwrap();
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o664)).unwrap();
    let root = raw_candidate_root(writable.path());
    assert!(matches!(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            writable.path(),
            CandidateInventoryLimits::default()
        ),
        Err(CandidateInventoryError::UnsafeMode { mode: 0o664, .. })
    ));

    let writable_directory = tempfile::tempdir().unwrap();
    let directory = writable_directory.path().join("group-writable");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o775)).unwrap();
    let root = raw_candidate_root(writable_directory.path());
    assert!(matches!(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            writable_directory.path(),
            CandidateInventoryLimits::default()
        ),
        Err(CandidateInventoryError::UnsafeMode { mode: 0o775, .. })
    ));

    let special_bits = tempfile::tempdir().unwrap();
    let directory = special_bits.path().join("sticky");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o1755)).unwrap();
    let root = raw_candidate_root(special_bits.path());
    assert!(matches!(
        RetainedCandidateDurabilitySeal::seal_before_marker(
            &root,
            special_bits.path(),
            CandidateInventoryLimits::default()
        ),
        Err(CandidateInventoryError::UnsafeMode { mode: 0o1755, .. })
    ));
}

fn assert_boundary(
    result: Result<RetainedCandidateDurabilitySeal, CandidateInventoryError>,
    expected: CandidateInventoryBoundary,
) {
    assert!(matches!(
        result,
        Err(CandidateInventoryError::Boundary { boundary, .. }) if boundary == expected
    ));
}

fn set_posix_acl(path: &Path, name: &std::ffi::CStr) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    // A canonical Linux POSIX ACL xattr: owner, one named user, group, mask,
    // and other. The named entry prevents the kernel from collapsing it into
    // ordinary mode bits.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    let effective = unsafe { nix::libc::geteuid() };
    let named = effective.wrapping_add(1);
    for (tag, permissions, id) in [
        (0x01_u16, 0x07_u16, u32::MAX),
        (0x02, 0x04, named),
        (0x04, 0x05, u32::MAX),
        (0x10, 0x05, u32::MAX),
        (0x20, 0x05, u32::MAX),
    ] {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&permissions.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    let result = unsafe { nix::libc::setxattr(encoded.as_ptr(), name.as_ptr(), bytes.as_ptr().cast(), bytes.len(), 0) };
    if result == 0 {
        Ok(true)
    } else {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM) | Some(nix::libc::EINVAL)
        ) {
            Ok(false)
        } else {
            Err(source)
        }
    }
}

fn set_test_xattr(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"rejected";
    // SAFETY: both C strings and the value buffer remain live for this call.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.cast-test".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(true)
    } else {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM) | Some(nix::libc::EACCES) | Some(nix::libc::EINVAL)
        ) {
            Ok(false)
        } else {
            Err(source)
        }
    }
}
