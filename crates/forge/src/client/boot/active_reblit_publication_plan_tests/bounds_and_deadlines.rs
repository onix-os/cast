#[test]
fn publication_and_aggregate_path_bounds_admit_n_and_reject_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_publications = 2;
    prepare_with_policy([fallback_bootloader(0, 1, 1), systemd_bootloader(0, 1, 1)], policy).unwrap();
    let error = prepare_with_policy(
        [
            fallback_bootloader(0, 1, 1),
            systemd_bootloader(0, 1, 1),
            payload("EFI/Aeryn/6.1/vmlinuz", 1, 2, 1),
        ],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::PublicationCountLimit { limit: 2, actual: 3 }
    );

    policy.max_publications = 3;
    policy.max_path_bytes = ACTIVE_REBLIT_LOADER_CONTROL_PATH.len();
    prepare_with_policy([loader_control(b"loader")], policy).unwrap();
    let error = prepare_with_policy(
        [loader_control(b"loader"), entry("loader/entries/a.conf", b"entry")],
        policy,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::PathByteLimit { .. }
    ));
}

#[test]
fn single_path_and_component_count_bounds_admit_n_and_reject_n_plus_one() {
    let path = "EFI/Aeryn/6.1/vmlinuz";
    let mut policy = PublicationPlanPolicy::production();
    policy.max_single_path_bytes = path.len();
    policy.max_components = 4;
    prepare_with_policy([payload(path, 0, 1, 1)], policy).unwrap();

    policy.max_single_path_bytes = path.len() - 1;
    assert!(matches!(
        prepare_with_policy([payload(path, 0, 1, 1)], policy),
        Err(ActiveReblitBootPublicationPlanError::SinglePathByteLimit { .. })
    ));
    policy.max_single_path_bytes = 100;
    policy.max_components = 3;
    assert!(matches!(
        prepare_with_policy([payload(path, 0, 1, 1)], policy),
        Err(ActiveReblitBootPublicationPlanError::PathComponentLimit { .. })
    ));
}

#[test]
fn logical_byte_limit_counts_each_canonical_output_including_generated_bytes() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_logical_bytes = 10;
    let plan = prepare_with_policy(
        [
            payload("EFI/Aeryn/6.1/vmlinuz", 0, 1, 6),
            entry("loader/entries/a.conf", b"1234"),
        ],
        policy,
    )
    .unwrap();
    assert_eq!(plan.logical_bytes(), 10);

    let error = prepare_with_policy(
        [
            payload("EFI/Aeryn/6.1/vmlinuz", 0, 1, 7),
            entry("loader/entries/a.conf", b"1234"),
        ],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::LogicalByteLimit { limit: 10, actual: 11 }
    );
}

#[test]
fn generated_per_file_and_total_bounds_admit_n_and_reject_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_generated_file_bytes = 4;
    policy.max_generated_bytes = 6;
    prepare_with_policy(
        [
            entry("loader/entries/a.conf", b"123"),
            entry("loader/entries/b.conf", b"456"),
        ],
        policy,
    )
    .unwrap();

    assert!(matches!(
        prepare_with_policy([entry("loader/entries/a.conf", b"12345")], policy),
        Err(ActiveReblitBootPublicationPlanError::GeneratedFileByteLimit {
            limit: 4,
            actual: 5,
            ..
        })
    ));
    assert_eq!(
        prepare_with_policy(
            [
                entry("loader/entries/a.conf", b"123"),
                entry("loader/entries/b.conf", b"4567")
            ],
            policy,
        )
        .unwrap_err(),
        ActiveReblitBootPublicationPlanError::GeneratedTotalByteLimit { limit: 6, actual: 7 }
    );
}

#[test]
fn sealed_snapshot_file_bound_admits_n_and_rejects_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_sealed_file_bytes = 8;
    policy.max_logical_bytes = 100;
    prepare_with_policy([payload("EFI/Aeryn/6.1/vmlinuz", 0, 1, 8)], policy).unwrap();
    assert_eq!(
        prepare_with_policy([payload("EFI/Aeryn/6.1/vmlinuz", 0, 1, 9)], policy).unwrap_err(),
        ActiveReblitBootPublicationPlanError::SealedSnapshotFileByteLimit {
            path: PathBuf::from("EFI/Aeryn/6.1/vmlinuz"),
            limit: 8,
            actual: 9,
        }
    );
}

#[test]
fn work_sort_reservation_and_deadline_failures_are_typed() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_work = 0;
    assert_eq!(
        prepare_with_policy([fallback_bootloader(0, 1, 1)], policy).unwrap_err(),
        ActiveReblitBootPublicationPlanError::WorkLimit { limit: 0, actual: 1 }
    );

    policy.max_work = 100;
    assert_eq!(
        prepare_publication_plan(
            [fallback_bootloader(0, 1, 1)],
            policy,
            Some(Instant::now() - Duration::from_millis(1)),
        )
        .unwrap_err(),
        ActiveReblitBootPublicationPlanError::DeadlineExceeded {
            timeout: policy.timeout
        }
    );

    let expected_sort_work = 8 * 3 * SORT_WORK_PER_ELEMENT_LEVEL;
    assert_eq!(conservative_sort_work(8), expected_sort_work);
    let mut sort_policy = PublicationPlanPolicy::production();
    sort_policy.max_work = expected_sort_work - 1;
    let mut budget = PublicationPlanBudget::new(sort_policy, Some(Instant::now() + Duration::from_secs(1))).unwrap();
    assert_eq!(
        budget.reserve_sort_work(8).unwrap_err(),
        ActiveReblitBootPublicationPlanError::WorkLimit {
            limit: expected_sort_work - 1,
            actual: expected_sort_work,
        }
    );
}

#[test]
fn deadline_is_checked_again_after_sorting() {
    let policy = PublicationPlanPolicy::production();
    let checkpoint_ran = std::cell::Cell::new(false);
    let error = prepare_publication_plan_with_sort_checkpoint(
        [fallback_bootloader(0, 1, 1)],
        policy,
        Some(Instant::now() + Duration::from_secs(1)),
        || {
            checkpoint_ran.set(true);
            std::thread::sleep(Duration::from_millis(1_100));
        },
    )
    .unwrap_err();
    assert!(checkpoint_ran.get());
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::DeadlineExceeded {
            timeout: policy.timeout
        }
    );
}

#[test]
fn production_contract_constants_match_the_publication_limits() {
    let policy = PublicationPlanPolicy::production();
    assert_eq!(policy.max_publications, 8_336);
    assert_eq!(policy.max_path_bytes, 8 * 1024 * 1024);
    assert_eq!(policy.max_single_path_bytes, nix::libc::PATH_MAX as usize - 1);
    assert_eq!(policy.max_components, 16);
    assert_eq!(MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES, 255);
    assert_eq!(policy.max_logical_bytes, 10 * 1024 * 1024 * 1024);
    assert_eq!(policy.max_sealed_file_bytes, 512 * 1024 * 1024);
    assert_eq!(policy.max_generated_bytes, 16 * 1024 * 1024);
    assert_eq!(policy.max_generated_file_bytes, 1024 * 1024);
    assert_eq!(policy.max_work, 1_000_000);
    assert_eq!(policy.timeout, Duration::from_secs(30));
    assert_eq!(ACTIVE_REBLIT_BOOT_OUTPUT_MODE, 0o644);
    assert_eq!(MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS, 8_336);
    assert_eq!(Path::new("a/b").components().count(), 2);
}
