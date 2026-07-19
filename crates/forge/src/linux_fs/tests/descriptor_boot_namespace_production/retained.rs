use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    mem::MaybeUninit,
    os::fd::AsRawFd as _,
    os::unix::fs::OpenOptionsExt as _,
    path::Path,
    time::{Duration, Instant},
};

use xxhash_rust::xxh3::xxh3_128;

use super::*;

fn target_temporary(prefix: &str) -> tempfile::TempDir {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).unwrap();
    tempfile::Builder::new().prefix(prefix).tempdir_in(target).unwrap()
}

fn retained_root(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .unwrap()
}

fn retained_descriptor_identity(file: &File) -> (u64, u64, u64) {
    let mut status = MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: fstat initializes the supplied storage on success and the
    // retained fixture descriptor remains live for the complete call.
    assert_eq!(unsafe { nix::libc::fstat(file.as_raw_fd(), status.as_mut_ptr()) }, 0);
    // SAFETY: successful fstat initialized every stat field.
    let status = unsafe { status.assume_init() };
    // SAFETY: zero is a valid initial value for every statx field.
    let mut extended: nix::libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: the empty C string and output remain live for this descriptor-
    // relative fixture observation.
    assert_eq!(
        unsafe {
            nix::libc::syscall(
                nix::libc::SYS_statx,
                file.as_raw_fd(),
                c"".as_ptr(),
                nix::libc::AT_EMPTY_PATH,
                nix::libc::STATX_MNT_ID,
                &mut extended,
            )
        },
        0
    );
    assert_ne!(extended.stx_mask & nix::libc::STATX_MNT_ID, 0);
    (status.st_dev, status.st_ino, extended.stx_mnt_id)
}

fn request<'a>(name: &'a str, expected: &[u8]) -> BootNamespaceRequest<'a> {
    BootNamespaceRequest::new(name, expected.len() as u64, xxh3_128(expected))
}

fn assess<'request, 'expected>(
    root: &File,
    requests: &'request [BootNamespaceRequest<'request>],
    expected: &'expected [&'expected [u8]],
) -> Result<ValidatedRetainedBootNamespaceAssessment, RetainedBootNamespaceAssessmentError> {
    assess_retained_boot_namespace_until(
        root,
        requests,
        expected,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
    )
}

#[test]
fn ordinary_target_fixture_classifies_exact_different_and_absent() {
    let temporary = target_temporary("forge-retained-namespace-");
    std::fs::write(temporary.path().join("exact"), b"same").unwrap();
    std::fs::write(temporary.path().join("different"), b"else").unwrap();
    let root = retained_root(temporary.path());
    let expected_exact = b"same".as_slice();
    let expected_different = b"wish".as_slice();
    let expected_absent = b"".as_slice();
    let requests = [
        request("exact", expected_exact),
        request("different", expected_different),
        request("absent", expected_absent),
    ];
    let streams = [expected_exact, expected_different, expected_absent];

    let assessment = assess(&root, &requests, &streams).unwrap();

    assert_eq!(
        assessment.states(),
        &[
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Different,
            BootNamespaceDestinationState::Absent,
        ]
    );
    assert!(assessment.fixture_usage().observation_io_attempts > 0);
    assert_eq!(assessment.fixture_usage().inventory_passes, 2);
    assert!(assessment.fixture_usage().raw_read_calls >= 2);
}

#[test]
fn nonempty_result_exposes_exact_retained_root_identity() {
    let temporary = target_temporary("forge-retained-root-evidence-");
    let root = retained_root(temporary.path());
    let expected_identity = retained_descriptor_identity(&root);
    let expected = b"".as_slice();
    let requests = [request("missing", expected)];
    let streams = [expected];

    let assessment = assess(&root, &requests, &streams).unwrap();
    let observed = assessment
        .observed_root_identity()
        .expect("nonempty successful assessment must retain observed-root evidence");

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Absent]);
    assert_eq!(
        (observed.device, observed.inode, observed.mount_id),
        expected_identity
    );
}

#[test]
fn empty_result_has_no_observed_root_identity() {
    let temporary = target_temporary("forge-retained-empty-root-evidence-");
    let root = retained_root(temporary.path());
    let requests: [BootNamespaceRequest<'static>; 0] = [];
    let streams: [&[u8]; 0] = [];

    let assessment = assess(&root, &requests, &streams).unwrap();

    assert!(assessment.states().is_empty());
    assert_eq!(assessment.observed_root_identity(), None);
}

#[test]
fn root_protocol_failure_cannot_emit_validated_result() {
    let temporary = target_temporary("forge-retained-root-protocol-failure-");
    let root = retained_root(temporary.path());
    let expected = b"".as_slice();
    let requests = [request("missing", expected)];
    let streams = [expected];
    let root_event = Cell::new(false);
    let complete_event = Cell::new(false);
    let mut hook = |event| match event {
        FixtureRetainedBootNamespaceProtocolEvent::RootRetained { .. } => {
            root_event.set(true);
            Err(std::io::Error::other("injected retained-root protocol failure"))
        }
        FixtureRetainedBootNamespaceProtocolEvent::Complete => {
            complete_event.set(true);
            Ok(())
        }
        _ => Ok(()),
    };

    let result = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    );

    assert!(matches!(
        result,
        Err(RetainedBootNamespaceAssessmentError::Filesystem {
            action: "running the retained-root protocol hook",
            ..
        })
    ));
    assert!(root_event.get());
    assert!(!complete_event.get());
}

#[test]
fn every_inventory_pass_starts_from_a_fresh_offset_zero_description() {
    let temporary = target_temporary("forge-retained-fresh-inventory-");
    std::fs::write(temporary.path().join("alpha"), b"a").unwrap();
    std::fs::write(temporary.path().join("beta"), b"b").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"a".as_slice();
    let requests = [request("alpha", expected)];
    let streams = [expected];
    let mut parsed = Vec::new();
    let mut hook = |event| {
        if let FixtureRetainedBootNamespaceProtocolEvent::InventoryParsed { boundary, entries } = event {
            parsed.push((boundary, entries));
        }
        Ok(())
    };

    let assessment = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Exact]);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].1, 2);
    assert_eq!(parsed[1].1, 2);
    assert_ne!(parsed[0].0, parsed[1].0);
}

#[test]
fn nested_nodes_release_in_strict_lifo_order_before_completion() {
    let temporary = target_temporary("forge-retained-lifo-");
    std::fs::create_dir(temporary.path().join("EFI")).unwrap();
    std::fs::create_dir(temporary.path().join("EFI/BOOT")).unwrap();
    std::fs::write(temporary.path().join("EFI/BOOT/loader"), b"loader").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"loader".as_slice();
    let requests = [request("EFI/BOOT/loader", expected)];
    let streams = [expected];
    let mut events = Vec::new();
    let mut hook = |event| {
        events.push(event);
        Ok(())
    };

    assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap();

    let root_identity = match events.first().copied() {
        Some(FixtureRetainedBootNamespaceProtocolEvent::RootRetained { identity }) => identity,
        found => panic!("unexpected first event: {found:?}"),
    };
    let releases = events
        .iter()
        .filter_map(|event| match event {
            FixtureRetainedBootNamespaceProtocolEvent::NodeReleased { identity } => Some(*identity),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(releases.len(), 4);
    assert_eq!(releases.last(), Some(&root_identity));
    assert!(matches!(
        events.last(),
        Some(FixtureRetainedBootNamespaceProtocolEvent::Complete)
    ));
}

#[test]
fn injected_change_after_opening_hash_is_failed_closed() {
    let temporary = target_temporary("forge-retained-content-race-");
    let file = temporary.path().join("loader");
    std::fs::write(&file, b"before").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"before".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let changed = Cell::new(false);
    let mut hook = |event| {
        if matches!(
            event,
            FixtureRetainedBootNamespaceProtocolEvent::RegularHashComplete {
                boundary: BootNamespaceObservationBoundary::Opening,
                ..
            }
        ) && !changed.replace(true)
        {
            std::fs::write(&file, b"change").unwrap();
        }
        Ok(())
    };

    let error = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap_err();

    assert!(changed.get());
    assert!(matches!(error, RetainedBootNamespaceAssessmentError::Filesystem { .. }));
}

#[test]
fn aggregate_inventory_pass_budget_accepts_n_and_rejects_n_minus_one() {
    let temporary = target_temporary("forge-retained-budget-");
    std::fs::write(temporary.path().join("loader"), b"data").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"data".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let baseline = assess(&root, &requests, &streams).unwrap();
    let required = baseline.fixture_usage().inventory_passes;
    assert_eq!(required, 2);

    let mut exact = RetainedBootNamespaceAssessmentLimits::default();
    exact.max_inventory_passes = required;
    assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        exact,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();

    let mut short = exact;
    short.max_inventory_passes = required - 1;
    let error = assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        short,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "inventory passes",
            ..
        }
    ));
}

#[test]
fn live_observation_io_attempt_budget_accepts_exact_n_and_rejects_n_minus_one() {
    let temporary = target_temporary("forge-retained-syscall-budget-");
    std::fs::write(temporary.path().join("loader"), b"data").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"data".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let baseline = assess(&root, &requests, &streams).unwrap();
    let required = baseline.fixture_usage().observation_io_attempts;
    assert!(required > 1);

    let mut exact = RetainedBootNamespaceAssessmentLimits::default();
    exact.max_observation_io_attempts = required;
    assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        exact,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();

    let mut short = exact;
    short.max_observation_io_attempts = required - 1;
    let error = assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        short,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "observation I/O attempts",
            ..
        }
    ));
}

#[test]
fn descriptor_slot_budget_accepts_exact_peak_and_rejects_one_less() {
    let temporary = target_temporary("forge-retained-descriptor-budget-");
    std::fs::create_dir(temporary.path().join("EFI")).unwrap();
    std::fs::create_dir(temporary.path().join("EFI/BOOT")).unwrap();
    std::fs::write(temporary.path().join("EFI/BOOT/loader"), b"loader").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"loader".as_slice();
    let requests = [request("EFI/BOOT/loader", expected)];
    let streams = [expected];
    let baseline = assess(&root, &requests, &streams).unwrap();
    let required = baseline.fixture_usage().peak_descriptor_slots;
    assert!(required > baseline.fixture_usage().peak_retained_nodes);

    let mut exact = RetainedBootNamespaceAssessmentLimits::default();
    exact.max_descriptor_slots = required;
    assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        exact,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();

    let mut short = exact;
    short.max_descriptor_slots = required - 1;
    let error = assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        short,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "descriptor slots",
            ..
        }
    ));
}

#[test]
fn failed_open_reserves_and_releases_one_descriptor_slot() {
    let temporary = target_temporary("forge-retained-failed-open-slot-");
    let root = retained_root(temporary.path());

    let usage = probe_failed_open_descriptor_slot_until(
        &root,
        b"missing",
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(
        usage,
        FixtureFailedOpenDescriptorSlotUsage {
            slots_before_failed_open: 1,
            slots_after_failed_open: 1,
            peak_descriptor_slots: 2,
        }
    );

    let mut short = RetainedBootNamespaceAssessmentLimits::default();
    short.max_descriptor_slots = 1;
    let error =
        probe_failed_open_descriptor_slot_until(&root, b"missing", short, Instant::now() + Duration::from_secs(30))
            .unwrap_err();
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "descriptor slots",
            ..
        }
    ));

    let expected = b"".as_slice();
    let requests = [request("missing", expected)];
    let streams = [expected];
    let assessment = assess(&root, &requests, &streams).unwrap();
    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Absent]);
}

#[test]
fn empty_request_uses_no_descriptor_slots() {
    let temporary = target_temporary("forge-retained-empty-request-slots-");
    let root = retained_root(temporary.path());
    let requests: [BootNamespaceRequest<'static>; 0] = [];
    let streams: [&[u8]; 0] = [];

    let assessment = assess(&root, &requests, &streams).unwrap();

    assert!(assessment.states().is_empty());
    assert_eq!(assessment.observed_root_identity(), None);
    assert_eq!(assessment.fixture_usage().observation_io_attempts, 0);
    assert_eq!(assessment.fixture_usage().peak_descriptor_slots, 0);
    assert_eq!(assessment.fixture_usage().peak_retained_nodes, 0);
}

#[test]
fn logical_node_budget_remains_separate_from_descriptor_slots() {
    let temporary = target_temporary("forge-retained-node-budget-");
    std::fs::create_dir(temporary.path().join("EFI")).unwrap();
    std::fs::create_dir(temporary.path().join("EFI/BOOT")).unwrap();
    std::fs::write(temporary.path().join("EFI/BOOT/loader"), b"loader").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"loader".as_slice();
    let requests = [request("EFI/BOOT/loader", expected)];
    let streams = [expected];
    let baseline = assess(&root, &requests, &streams).unwrap();
    let required = baseline.fixture_usage().peak_retained_nodes;
    assert!(required > 1);

    let mut exact = RetainedBootNamespaceAssessmentLimits::default();
    exact.max_retained_nodes = required;
    assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        exact,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();

    let mut short = exact;
    short.max_retained_nodes = required - 1;
    let error = assess_retained_boot_namespace_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        short,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "retained nodes",
            ..
        }
    ));
}

#[test]
fn late_deadline_after_opening_lookup_releases_retained_descriptors() {
    let temporary = target_temporary("forge-retained-late-deadline-");
    std::fs::write(temporary.path().join("loader"), b"data").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"data".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let deadline = Instant::now() + Duration::from_secs(1);
    let expired_lookup = Cell::new(false);
    let releases = Cell::new(0usize);
    let mut hook = |event| {
        match event {
            FixtureRetainedBootNamespaceProtocolEvent::LookupObserved {
                boundary: BootNamespaceObservationBoundary::Opening,
                present: true,
                ..
            } if !expired_lookup.replace(true) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining + Duration::from_millis(20));
            }
            FixtureRetainedBootNamespaceProtocolEvent::NodeReleased { .. } => {
                releases.set(releases.get() + 1);
            }
            _ => {}
        }
        Ok(())
    };

    let error = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        deadline,
        &mut hook,
    )
    .unwrap_err();

    assert!(expired_lookup.get());
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::DeadlineExceeded { .. }
    ));
    assert!(releases.get() >= 2);
}

#[test]
fn hook_failure_after_child_open_is_not_masked_by_cleanup() {
    let temporary = target_temporary("forge-retained-hook-failure-");
    std::fs::write(temporary.path().join("loader"), b"data").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"data".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let failed = Cell::new(false);
    let mut hook = |event| {
        if matches!(
            event,
            FixtureRetainedBootNamespaceProtocolEvent::LookupObserved {
                boundary: BootNamespaceObservationBoundary::Opening,
                present: true,
                ..
            }
        ) && !failed.replace(true)
        {
            return Err(std::io::Error::other("injected opened-child hook failure"));
        }
        Ok(())
    };

    let error = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap_err();

    assert!(failed.get());
    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::Filesystem {
            action: "running one opening-lookup protocol hook",
            ..
        }
    ));
}

#[test]
fn size_mismatch_skips_all_content_hashes_and_actual_reads() {
    let temporary = target_temporary("forge-retained-size-mismatch-");
    std::fs::write(temporary.path().join("loader"), b"abc").unwrap();
    let root = retained_root(temporary.path());
    let expected = b"four".as_slice();
    let requests = [request("loader", expected)];
    let streams = [expected];
    let hash_events = Cell::new(0usize);
    let read_events = Cell::new(0usize);
    let mut hook = |event| {
        match event {
            FixtureRetainedBootNamespaceProtocolEvent::RegularHashComplete { .. } => {
                hash_events.set(hash_events.get() + 1)
            }
            FixtureRetainedBootNamespaceProtocolEvent::ActualRead { .. } => read_events.set(read_events.get() + 1),
            _ => {}
        }
        Ok(())
    };

    let assessment = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Different]);
    assert_eq!(hash_events.get(), 0);
    assert_eq!(read_events.get(), 0);
}

#[test]
fn expected_digest_mismatch_fails_before_root_observation() {
    let temporary = target_temporary("forge-retained-expected-binding-");
    let root = retained_root(temporary.path());
    let requests = [BootNamespaceRequest::new("loader", 4, xxh3_128(b"good"))];
    let bad = b"evil".as_slice();
    let streams = [bad];
    let events = Cell::new(0usize);
    let mut hook = |_event| {
        events.set(events.get() + 1);
        Ok(())
    };

    let error = assess_retained_boot_namespace_with_hook_until(
        &root,
        &requests,
        &streams,
        BootNamespaceAssessmentLimits::default(),
        RetainedBootNamespaceAssessmentLimits::default(),
        Instant::now() + Duration::from_secs(30),
        &mut hook,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        RetainedBootNamespaceAssessmentError::ExpectedDigestMismatch { request_index: 0 }
    ));
    assert_eq!(events.get(), 0);
}

#[test]
fn non_opath_root_is_rejected_without_fallback() {
    let temporary = target_temporary("forge-retained-root-flags-");
    let root = File::open(temporary.path()).unwrap();
    let expected = b"".as_slice();
    let requests = [request("absent", expected)];
    let streams = [expected];

    let error = assess(&root, &requests, &streams).unwrap_err();

    assert!(matches!(error, RetainedBootNamespaceAssessmentError::Filesystem { .. }));
}
