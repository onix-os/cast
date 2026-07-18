use std::{
    cell::Cell,
    ffi::{CStr, CString},
    fs::{self, FileTimes},
    io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tempfile::TempDir;

use super::{
    ActiveReblitLocalBootPolicyError, BoundActiveReblitLocalCmdlineEntry, LOCAL_BOOT_POLICY, LocalBootPolicy,
    LocalBootPolicyBudget, PreparedActiveReblitLocalBootPolicy, RetainedLocalCmdlineEntry, filesystem,
    normalize_cmdline, prepare_with_policy_and_checkpoint,
};
use crate::Installation;

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
    installation: Installation,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        let installation = Installation::open(&root, None).unwrap();
        Self {
            _temporary: temporary,
            root,
            installation,
        }
    }

    fn policy_directory(&self) -> PathBuf {
        let path = self.root.join("etc/kernel/cmdline.d");
        fs::create_dir_all(&path).unwrap();
        for directory in [self.root.join("etc"), self.root.join("etc/kernel"), path.clone()] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn prepare(&self) -> Result<PreparedActiveReblitLocalBootPolicy, ActiveReblitLocalBootPolicyError> {
        PreparedActiveReblitLocalBootPolicy::prepare(&self.installation)
    }
}

fn write_policy_file(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    fs::write(path.as_ref(), bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn absent_local_policy_is_retained_and_revalidated_without_entries() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare().unwrap();
    assert!(prepared.is_absent());
    assert_eq!(prepared.entry_count(), 0);
    assert_eq!(prepared.total_file_bytes(), 0);

    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    assert!(revalidated.is_absent());
    assert_eq!(revalidated.entries().len(), 0);
}

#[test]
fn exact_cmdline_files_and_dev_null_masks_are_sorted_and_normalized() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    write_policy_file(
        policy.join("20-local.cmdline"),
        b"  quiet splash\n  # ignored comment\r\nmodule.option=yes\r\n",
    );
    symlink("/dev/null", policy.join("10-package.cmdline")).unwrap();
    write_policy_file(policy.join("README"), b"not part of the policy");

    let prepared = fixture.prepare().unwrap();
    assert!(!prepared.is_absent());
    assert_eq!(prepared.entry_count(), 2);
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    let entries = revalidated.entries().collect::<Vec<_>>();
    assert_eq!(
        entries,
        vec![
            BoundActiveReblitLocalCmdlineEntry::Mask {
                name: std::ffi::OsStr::new("10-package.cmdline")
            },
            BoundActiveReblitLocalCmdlineEntry::Append {
                name: std::ffi::OsStr::new("20-local.cmdline"),
                snippet: "quiet splash module.option=yes",
            },
        ]
    );
}

#[test]
fn non_cmdline_entries_are_inventoried_but_not_interpreted() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    symlink("somewhere-else", policy.join("ignored-link")).unwrap();
    fs::create_dir(policy.join("ignored-directory")).unwrap();

    let prepared = fixture.prepare().unwrap();
    assert_eq!(prepared.entry_count(), 0);
    assert_eq!(prepared.revalidate(&fixture.installation).unwrap().entries().len(), 0);
}

#[test]
fn a_non_dev_null_cmdline_symlink_is_a_hard_failure() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    symlink("../other", policy.join("10-invalid.cmdline")).unwrap();

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::UnsafeInode { reason, .. })
            if reason.contains("not an exact /dev/null mask")
    ));
}

#[test]
fn hardlinked_or_group_writable_cmdline_files_are_rejected() {
    let hardlink_fixture = Fixture::new();
    let policy = hardlink_fixture.policy_directory();
    let source = policy.join("10-hardlinked.cmdline");
    write_policy_file(&source, b"quiet");
    fs::hard_link(&source, policy.join("second-name")).unwrap();
    assert!(matches!(
        hardlink_fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::UnsafeInode { .. })
    ));

    let writable_fixture = Fixture::new();
    let policy = writable_fixture.policy_directory();
    let source = policy.join("10-writable.cmdline");
    write_policy_file(&source, b"quiet");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(matches!(
        writable_fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::UnsafeInode { .. })
    ));
}

#[test]
fn control_bytes_and_oversized_cmdline_files_fail_closed() {
    let control_fixture = Fixture::new();
    let policy = control_fixture.policy_directory();
    write_policy_file(policy.join("10-control.cmdline"), b"quiet\x1bunsafe");
    assert!(matches!(
        control_fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineContent { .. })
    ));

    let oversized_fixture = Fixture::new();
    let policy = oversized_fixture.policy_directory();
    write_policy_file(
        policy.join("10-oversized.cmdline"),
        vec![b'x'; LOCAL_BOOT_POLICY.max_file_bytes + 1],
    );
    assert!(matches!(
        oversized_fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::FileBytesLimit { .. })
    ));
}

#[test]
fn a_regular_file_change_between_capture_and_final_revalidation_is_rejected() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    let path = policy.join("10-race.cmdline");
    write_policy_file(&path, b"before");

    let result = prepare_with_policy_and_checkpoint(&fixture.installation, LocalBootPolicy::production(), |_| {
        write_policy_file(&path, b"after!")
    });
    assert!(matches!(result, Err(ActiveReblitLocalBootPolicyError::Changed { .. })));
}

#[test]
fn an_absent_component_appearing_before_final_revalidation_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("etc/kernel/cmdline.d");
    let result = prepare_with_policy_and_checkpoint(&fixture.installation, LocalBootPolicy::production(), |_| {
        fs::create_dir_all(&path).unwrap()
    });
    assert!(matches!(result, Err(ActiveReblitLocalBootPolicyError::Changed { .. })));
}

#[test]
fn retained_mask_reads_ignore_public_name_substitution_and_bind_exact_length() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    let mask = policy.join("10-mask.cmdline");
    symlink("/dev/null", &mask).unwrap();
    let prepared = fixture.prepare().unwrap();
    let RetainedLocalCmdlineEntry::Mask { retained, witness, .. } = &prepared.entries[0] else {
        panic!("fixture must retain one mask");
    };

    fs::rename(&mask, policy.join("retained-mask")).unwrap();
    symlink("../other", &mask).unwrap();
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let target = filesystem::read_link_target(retained, *witness, &mask, &mut budget).unwrap();
    assert_eq!(target.as_ref(), b"/dev/null");

    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    assert!(matches!(
        filesystem::read_link_target(retained, witness.with_length(8), &mask, &mut budget),
        Err(ActiveReblitLocalBootPolicyError::Changed { reason, .. })
            if reason.contains("target length changed")
    ));
    assert!(matches!(
        prepared.revalidate(&fixture.installation),
        Err(ActiveReblitLocalBootPolicyError::Changed { .. })
    ));
}

#[test]
fn two_complete_passes_reject_an_earlier_entry_mutated_between_them() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    let first = policy.join("10-first.cmdline");
    write_policy_file(&first, b"before");
    write_policy_file(policy.join("20-second.cmdline"), b"stable");
    let prepared = fixture.prepare().unwrap();
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();

    let result = prepared.revalidate_with_budget_and_checkpoints(
        &fixture.installation,
        &mut budget,
        || {},
        || write_policy_file(&first, b"after!"),
    );
    assert!(matches!(result, Err(ActiveReblitLocalBootPolicyError::Changed { .. })));
}

#[test]
fn every_pass_rebinds_present_and_absent_public_locations() {
    let present = Fixture::new();
    let policy = present.policy_directory();
    write_policy_file(policy.join("10-stable.cmdline"), b"stable");
    let prepared = present.prepare().unwrap();
    let detached = present.root.join("etc/kernel/detached-cmdline.d");
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &present.installation,
        &mut budget,
        || {
            fs::rename(&policy, &detached).unwrap();
            fs::create_dir(&policy).unwrap();
            fs::set_permissions(&policy, fs::Permissions::from_mode(0o755)).unwrap();
            write_policy_file(policy.join("10-stable.cmdline"), b"stable");
        },
        || {},
    );
    assert!(matches!(result, Err(ActiveReblitLocalBootPolicyError::Changed { .. })));

    let absent = Fixture::new();
    let prepared = absent.prepare().unwrap();
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &absent.installation,
        &mut budget,
        || {
            absent.policy_directory();
        },
        || {},
    );
    assert!(matches!(result, Err(ActiveReblitLocalBootPolicyError::Changed { .. })));
}

#[test]
fn intermediate_ancestor_mode_acls_and_xattrs_are_revalidated() {
    assert_ancestor_mutation_rejected(|ancestor| {
        fs::set_permissions(ancestor, fs::Permissions::from_mode(0o777)).unwrap();
        true
    });
    assert_ancestor_mutation_rejected(|ancestor| set_posix_acl(ancestor, c"system.posix_acl_access").unwrap());
    assert_ancestor_mutation_rejected(|ancestor| set_posix_acl(ancestor, c"system.posix_acl_default").unwrap());
    assert_ancestor_mutation_rejected(|ancestor| set_test_xattr(ancestor).unwrap());
}

#[test]
fn directory_entry_work_and_elapsed_time_bounds_are_inclusive() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    write_policy_file(policy.join("one"), b"1");
    write_policy_file(policy.join("two"), b"2");
    prepare_with_policy(
        &fixture,
        LocalBootPolicy {
            max_directory_entries: 2,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    let constrained = LocalBootPolicy {
        max_directory_entries: 1,
        ..LocalBootPolicy::production()
    };
    assert!(matches!(
        prepare_with_policy_and_checkpoint(&fixture.installation, constrained, |_| {}),
        Err(ActiveReblitLocalBootPolicyError::DirectoryEntryLimit { .. })
    ));

    let cmdline = Fixture::new();
    let policy = cmdline.policy_directory();
    write_policy_file(policy.join("10-one.cmdline"), b"a");
    write_policy_file(policy.join("20-two.cmdline"), b"b");
    prepare_with_policy(
        &cmdline,
        LocalBootPolicy {
            max_cmdline_entries: 2,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert!(matches!(
        prepare_with_policy(
            &cmdline,
            LocalBootPolicy {
                max_cmdline_entries: 1,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::CmdlineEntryLimit { .. })
    ));

    let bytes = Fixture::new();
    let policy = bytes.policy_directory();
    write_policy_file(policy.join("10-four.cmdline"), b"four");
    prepare_with_policy(
        &bytes,
        LocalBootPolicy {
            max_file_bytes: 4,
            max_total_file_bytes: 4,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert!(matches!(
        prepare_with_policy(
            &bytes,
            LocalBootPolicy {
                max_file_bytes: 3,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::FileBytesLimit { .. })
    ));

    let total = Fixture::new();
    let policy = total.policy_directory();
    write_policy_file(policy.join("10-two.cmdline"), b"ab");
    write_policy_file(policy.join("20-two.cmdline"), b"cd");
    prepare_with_policy(
        &total,
        LocalBootPolicy {
            max_total_file_bytes: 4,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert!(matches!(
        prepare_with_policy(
            &total,
            LocalBootPolicy {
                max_total_file_bytes: 3,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::TotalFileBytesLimit { .. })
    ));

    let names = Fixture::new();
    let policy = names.policy_directory();
    let relevant = "abc.cmdline";
    write_policy_file(policy.join(relevant), b"x");
    prepare_with_policy(
        &names,
        LocalBootPolicy {
            max_name_bytes: relevant.len(),
            max_total_name_bytes: relevant.len(),
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert!(matches!(
        prepare_with_policy(
            &names,
            LocalBootPolicy {
                max_name_bytes: relevant.len() - 1,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::NameBytesLimit { .. })
    ));

    let total_names = Fixture::new();
    let policy = total_names.policy_directory();
    write_policy_file(policy.join("aa"), b"x");
    write_policy_file(policy.join("bbb"), b"x");
    prepare_with_policy(
        &total_names,
        LocalBootPolicy {
            max_total_name_bytes: 5,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert!(matches!(
        prepare_with_policy(
            &total_names,
            LocalBootPolicy {
                max_total_name_bytes: 4,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::TotalNameBytesLimit { .. })
    ));

    let work = Fixture::new();
    write_policy_file(work.policy_directory().join("10-work.cmdline"), b"bounded");
    let observed = work.prepare().unwrap().preparation_work();
    let exact = prepare_with_policy(
        &work,
        LocalBootPolicy {
            max_work: observed,
            ..LocalBootPolicy::production()
        },
    )
    .unwrap();
    assert_eq!(exact.preparation_work(), observed);
    assert!(matches!(
        prepare_with_policy(
            &work,
            LocalBootPolicy {
                max_work: observed - 1,
                ..LocalBootPolicy::production()
            }
        ),
        Err(ActiveReblitLocalBootPolicyError::WorkLimit { limit, actual, .. })
            if limit == observed - 1 && actual == observed
    ));

    let expired = LocalBootPolicy {
        timeout: Duration::ZERO,
        ..LocalBootPolicy::production()
    };
    assert!(matches!(
        prepare_with_policy_and_checkpoint(&fixture.installation, expired, |_| {}),
        Err(ActiveReblitLocalBootPolicyError::DeadlineExceeded { .. })
    ));
}

#[test]
fn raw_syscall_interruption_ceiling_accepts_n_and_rejects_n_plus_one() {
    let path = Path::new("interrupted-local-policy-operation");
    let accepted_calls = Cell::new(0usize);
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let result = filesystem::retry_raw_syscall(path, "test bounded interruption", &mut budget, || {
        let call = accepted_calls.get();
        accepted_calls.set(call + 1);
        if call < 1_024 {
            set_errno(nix::libc::EINTR);
            -1
        } else {
            0
        }
    })
    .unwrap();
    assert_eq!(result, 0);
    assert_eq!(accepted_calls.get(), 1_025);

    let rejected_calls = Cell::new(0usize);
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let error = filesystem::retry_raw_syscall(path, "test bounded interruption", &mut budget, || {
        rejected_calls.set(rejected_calls.get() + 1);
        set_errno(nix::libc::EINTR);
        -1
    })
    .unwrap_err();
    assert!(matches!(error, ActiveReblitLocalBootPolicyError::Io { source, .. }
        if source.kind() == io::ErrorKind::Interrupted));
    assert_eq!(rejected_calls.get(), 1_025);
}

#[test]
fn descriptor_reads_do_not_change_regular_file_or_directory_atime() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    let snippet = policy.join("10-atime.cmdline");
    write_policy_file(&snippet, b"quiet");
    let times = FileTimes::new()
        .set_accessed(SystemTime::UNIX_EPOCH + Duration::from_secs(100))
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(200));
    fs::File::open(&snippet).unwrap().set_times(times).unwrap();
    fs::File::open(&policy).unwrap().set_times(times).unwrap();
    let file_atime = atime(&snippet);
    let directory_atime = atime(&policy);

    let prepared = fixture.prepare().unwrap();
    prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(atime(&snippet), file_atime);
    assert_eq!(atime(&policy), directory_atime);
}

#[test]
fn normalization_matches_the_accepted_upstream_subset_and_rejects_controls() {
    let path = Path::new("cmdline");
    for (input, expected) in [
        (b"".as_slice(), ""),
        (b"# comment only\n".as_slice(), ""),
        (b" one \n\n two \n".as_slice(), "one  two"),
        (b"one # inline\n".as_slice(), "one # inline"),
        (b"one\r\ntwo\r\n".as_slice(), "one two"),
    ] {
        assert_eq!(normalize_cmdline(input, path).unwrap().as_ref(), expected);
    }
    for input in [b"one\ttwo".as_slice(), b"one\rtwo".as_slice(), "café".as_bytes()] {
        assert!(matches!(
            normalize_cmdline(input, path),
            Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineContent { .. })
        ));
    }
}

#[test]
fn relevant_names_must_be_canonical_ascii() {
    let fixture = Fixture::new();
    let policy = fixture.policy_directory();
    write_policy_file(policy.join("-invalid.cmdline"), b"quiet");
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitLocalBootPolicyError::InvalidCmdlineName { .. })
    ));
}

fn prepare_with_policy(
    fixture: &Fixture,
    policy: LocalBootPolicy,
) -> Result<PreparedActiveReblitLocalBootPolicy, ActiveReblitLocalBootPolicyError> {
    prepare_with_policy_and_checkpoint(&fixture.installation, policy, |_| {})
}

fn assert_ancestor_mutation_rejected(mutate: impl FnOnce(&Path) -> bool) {
    let fixture = Fixture::new();
    write_policy_file(fixture.policy_directory().join("10-stable.cmdline"), b"stable");
    let prepared = fixture.prepare().unwrap();
    let ancestor = fixture.root.join("etc/kernel");
    let mut budget = LocalBootPolicyBudget::new(LocalBootPolicy::production()).unwrap();
    let mut applied = false;
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &fixture.installation,
        &mut budget,
        || {},
        || applied = mutate(&ancestor),
    );
    if applied {
        assert!(matches!(
            result,
            Err(ActiveReblitLocalBootPolicyError::UnsafeInode { .. })
                | Err(ActiveReblitLocalBootPolicyError::Changed { .. })
                | Err(ActiveReblitLocalBootPolicyError::Io { .. })
        ));
    }
}

fn set_posix_acl(path: &Path, name: &CStr) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    let named = unsafe { nix::libc::geteuid() }.wrapping_add(1);
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
    // SAFETY: both C strings and the complete ACL byte buffer remain live.
    let result = unsafe { nix::libc::setxattr(encoded.as_ptr(), name.as_ptr(), bytes.as_ptr().cast(), bytes.len(), 0) };
    classify_optional_metadata_mutation(result)
}

fn set_test_xattr(path: &Path) -> io::Result<bool> {
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"rejected";
    // SAFETY: both C strings and the value remain live for this syscall.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.cast-test".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    classify_optional_metadata_mutation(result)
}

fn classify_optional_metadata_mutation(result: i32) -> io::Result<bool> {
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

fn set_errno(errno: i32) {
    // SAFETY: errno is thread-local on the Linux target under test.
    unsafe { *nix::libc::__errno_location() = errno };
}

fn atime(path: &Path) -> (i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.atime(), metadata.atime_nsec())
}
