use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::*;

fn sealed(
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    path: impl Into<PathBuf>,
    digest: u128,
    length: u64,
) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::sealed_snapshot(root, phase, path.into(), digest, length)
}

fn generated(
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    path: impl Into<PathBuf>,
    bytes: &[u8],
) -> ActiveReblitBootPublicationRequest {
    ActiveReblitBootPublicationRequest::generated(
        root,
        phase,
        path.into(),
        bytes.into(),
        xxhash_rust::xxh3::xxh3_128(bytes),
    )
}

fn prepare_with_policy(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    prepare_publication_plan(requests, policy, Some(Instant::now() + Duration::from_secs(5)))
}

#[test]
fn canonical_order_is_phase_then_root_then_case_folded_path() {
    let generated_bytes = b"loader control";
    let plan = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Bootloader,
            "EFI/systemd/systemd-bootx64.efi",
            5,
            50,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/zeta.efi",
            1,
            10,
        ),
        generated(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::LoaderControl,
            "loader/loader.conf",
            generated_bytes,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/Alpha.efi",
            2,
            20,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/beta.efi",
            3,
            30,
        ),
        generated(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Entry,
            "loader/entries/a.conf",
            b"entry",
        ),
    ])
    .unwrap();

    let order = plan
        .outputs()
        .iter()
        .map(|output| (output.phase(), output.root(), output.relative_path().to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            (
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Esp,
                PathBuf::from("EFI/Linux/Alpha.efi"),
            ),
            (
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Esp,
                PathBuf::from("EFI/Linux/zeta.efi"),
            ),
            (
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from("EFI/Linux/beta.efi"),
            ),
            (
                ActiveReblitBootPublicationPhase::Entry,
                ActiveReblitBootDestinationRoot::Esp,
                PathBuf::from("loader/entries/a.conf"),
            ),
            (
                ActiveReblitBootPublicationPhase::LoaderControl,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from("loader/loader.conf"),
            ),
            (
                ActiveReblitBootPublicationPhase::Bootloader,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from("EFI/systemd/systemd-bootx64.efi"),
            ),
        ]
    );
    assert!(plan.outputs().iter().all(|output| output.mode() == 0o644));
    assert_eq!(
        plan.logical_bytes(),
        10 + 20 + 30 + 50 + 5 + generated_bytes.len() as u64
    );
    assert_eq!(plan.generated_bytes(), 5 + generated_bytes.len());
    assert_eq!(
        plan.path_bytes(),
        plan.outputs()
            .iter()
            .map(|output| output.relative_path().as_os_str().as_bytes().len())
            .sum::<usize>()
    );
    assert!(plan.planning_work() > plan.outputs().len());

    let loader = plan
        .outputs()
        .iter()
        .find(|output| output.phase() == ActiveReblitBootPublicationPhase::LoaderControl)
        .unwrap();
    assert_eq!(loader.source().generated_bytes(), Some(generated_bytes.as_slice()));
    assert_eq!(loader.source().digest(), xxhash_rust::xxh3::xxh3_128(generated_bytes));
    assert_eq!(loader.source().length(), generated_bytes.len() as u64);
}

#[test]
fn only_every_field_identical_duplicates_are_deduplicated() {
    let plan = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            7,
            11,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            7,
            11,
        ),
        generated(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Entry,
            "loader/entries/a.conf",
            b"same",
        ),
        generated(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Entry,
            "loader/entries/a.conf",
            b"same",
        ),
    ])
    .unwrap();

    assert_eq!(plan.outputs().len(), 2);
    assert_eq!(plan.logical_bytes(), 15);
    assert_eq!(plan.generated_bytes(), 4);
}

#[test]
fn same_path_with_different_role_content_or_source_kind_is_rejected() {
    let same_source_different_phase = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            7,
            11,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Bootloader,
            "EFI/Linux/a.efi",
            7,
            11,
        ),
    ])
    .unwrap_err();
    assert!(matches!(
        same_source_different_phase,
        ActiveReblitBootPublicationPlanError::PublicationCollision { .. }
    ));

    let different_content = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            7,
            11,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            8,
            11,
        ),
    ])
    .unwrap_err();
    assert!(matches!(
        different_content,
        ActiveReblitBootPublicationPlanError::PublicationCollision { .. }
    ));

    let bytes = b"generated";
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    let different_source_kind = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Entry,
            "loader/entries/a.conf",
            digest,
            bytes.len() as u64,
        ),
        generated(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Entry,
            "loader/entries/a.conf",
            bytes,
        ),
    ])
    .unwrap_err();
    assert!(matches!(
        different_source_kind,
        ActiveReblitBootPublicationPlanError::PublicationCollision { .. }
    ));
}

#[test]
fn case_insensitive_collisions_are_scoped_to_one_destination_root() {
    let collision = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/A.efi",
            1,
            1,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "efi/linux/a.EFI",
            1,
            1,
        ),
    ])
    .unwrap_err();
    assert!(matches!(
        collision,
        ActiveReblitBootPublicationPlanError::CaseInsensitiveCollision { .. }
    ));

    let plan = PreparedActiveReblitBootPublicationPlan::prepare([
        sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/A.efi",
            1,
            1,
        ),
        sealed(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Payload,
            "efi/linux/a.EFI",
            1,
            1,
        ),
    ])
    .unwrap();
    assert_eq!(plan.outputs().len(), 2);
}

#[test]
fn unsafe_relative_paths_are_rejected_instead_of_normalized() {
    let error_for = |path: PathBuf| {
        PreparedActiveReblitBootPublicationPlan::prepare([sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            path,
            1,
            1,
        )])
        .unwrap_err()
    };

    assert!(matches!(
        error_for(PathBuf::new()),
        ActiveReblitBootPublicationPlanError::EmptyPath
    ));
    assert!(matches!(
        error_for(PathBuf::from("/EFI/Linux/a.efi")),
        ActiveReblitBootPublicationPlanError::AbsolutePath { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI//Linux/a.efi")),
        ActiveReblitBootPublicationPlanError::EmptyPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/Linux/")),
        ActiveReblitBootPublicationPlanError::EmptyPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("./EFI/Linux/a.efi")),
        ActiveReblitBootPublicationPlanError::DotPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/./Linux/a.efi")),
        ActiveReblitBootPublicationPlanError::DotPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/../Linux/a.efi")),
        ActiveReblitBootPublicationPlanError::ParentPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/Lin\nux/a.efi")),
        ActiveReblitBootPublicationPlanError::ControlPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/Lin\0ux/a.efi")),
        ActiveReblitBootPublicationPlanError::NulPath { .. }
    ));
}

#[test]
fn non_utf8_paths_are_rejected() {
    let path = PathBuf::from(OsString::from_vec(b"EFI/Linux/\xff.efi".to_vec()));
    let error = PreparedActiveReblitBootPublicationPlan::prepare([sealed(
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootPublicationPhase::Payload,
        path,
        1,
        1,
    )])
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::NonUtf8Path { .. }
    ));
}

#[test]
fn publication_and_aggregate_path_bounds_admit_n_and_reject_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_publications = 2;
    policy.max_path_bytes = 100;
    prepare_with_policy(
        [
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "a",
                1,
                1,
            ),
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "b",
                2,
                1,
            ),
        ],
        policy,
    )
    .unwrap();
    let error = prepare_with_policy(
        [
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "a",
                1,
                1,
            ),
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "b",
                2,
                1,
            ),
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "c",
                3,
                1,
            ),
        ],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::PublicationCountLimit { limit: 2, actual: 3 }
    );

    policy.max_publications = 3;
    policy.max_path_bytes = 3;
    prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "abc",
            1,
            1,
        )],
        policy,
    )
    .unwrap();
    let error = prepare_with_policy(
        [
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "abc",
                1,
                1,
            ),
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "d",
                2,
                1,
            ),
        ],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::PathByteLimit { limit: 3, actual: 4 }
    );
}

#[test]
fn single_path_and_component_bounds_admit_n_and_reject_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_single_path_bytes = 3;
    policy.max_components = 3;
    prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "a/b",
            1,
            1,
        )],
        policy,
    )
    .unwrap();
    let error = prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "abcd",
            1,
            1,
        )],
        policy,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::SinglePathByteLimit {
            limit: 3,
            actual: 4,
            ..
        }
    ));

    policy.max_single_path_bytes = 100;
    prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "a/b/c",
            1,
            1,
        )],
        policy,
    )
    .unwrap();
    let error = prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "a/b/c/d",
            1,
            1,
        )],
        policy,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::PathComponentLimit {
            limit: 3,
            actual: 4,
            ..
        }
    ));
}

#[test]
fn logical_byte_limit_counts_each_canonical_output_including_generated_bytes() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_logical_bytes = 10;
    let plan = prepare_with_policy(
        [
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "a",
                1,
                6,
            ),
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "b",
                b"1234",
            ),
        ],
        policy,
    )
    .unwrap();
    assert_eq!(plan.logical_bytes(), 10);

    let error = prepare_with_policy(
        [
            sealed(
                ActiveReblitBootDestinationRoot::Esp,
                ActiveReblitBootPublicationPhase::Payload,
                "a",
                1,
                7,
            ),
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "b",
                b"1234",
            ),
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
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "a",
                b"123",
            ),
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "b",
                b"456",
            ),
        ],
        policy,
    )
    .unwrap();

    let per_file = prepare_with_policy(
        [generated(
            ActiveReblitBootDestinationRoot::Boot,
            ActiveReblitBootPublicationPhase::Entry,
            "a",
            b"12345",
        )],
        policy,
    )
    .unwrap_err();
    assert!(matches!(
        per_file,
        ActiveReblitBootPublicationPlanError::GeneratedFileByteLimit {
            limit: 4,
            actual: 5,
            ..
        }
    ));

    let total = prepare_with_policy(
        [
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "a",
                b"123",
            ),
            generated(
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootPublicationPhase::Entry,
                "b",
                b"4567",
            ),
        ],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        total,
        ActiveReblitBootPublicationPlanError::GeneratedTotalByteLimit { limit: 6, actual: 7 }
    );
}

#[test]
fn generated_digest_must_match_the_owned_bytes() {
    let error = PreparedActiveReblitBootPublicationPlan::prepare([ActiveReblitBootPublicationRequest::generated(
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootPublicationPhase::Entry,
        PathBuf::from("loader/entries/a.conf"),
        b"entry".as_slice().into(),
        0,
    )])
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::GeneratedDigestMismatch { .. }
    ));
}

#[test]
fn work_and_deadline_failures_are_typed() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_work = 0;
    let work = prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "a",
            1,
            1,
        )],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        work,
        ActiveReblitBootPublicationPlanError::WorkLimit { limit: 0, actual: 1 }
    );

    policy.max_work = 100;
    let deadline = prepare_publication_plan(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "a",
            1,
            1,
        )],
        policy,
        Some(Instant::now() - Duration::from_millis(1)),
    )
    .unwrap_err();
    assert_eq!(
        deadline,
        ActiveReblitBootPublicationPlanError::DeadlineExceeded {
            timeout: policy.timeout
        }
    );
}

#[test]
fn non_ascii_paths_fail_closed_before_case_collision_planning() {
    let error = PreparedActiveReblitBootPublicationPlan::prepare([sealed(
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootPublicationPhase::Entry,
        "loader/entries/Ä.conf",
        1,
        1,
    )])
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::NonAsciiPathComponent { .. }
    ));
}

#[test]
fn sealed_snapshot_file_bound_admits_n_and_rejects_n_plus_one() {
    let mut policy = PublicationPlanPolicy::production();
    policy.max_sealed_file_bytes = 8;
    policy.max_logical_bytes = 100;
    prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            1,
            8,
        )],
        policy,
    )
    .unwrap();

    let error = prepare_with_policy(
        [sealed(
            ActiveReblitBootDestinationRoot::Esp,
            ActiveReblitBootPublicationPhase::Payload,
            "EFI/Linux/a.efi",
            1,
            9,
        )],
        policy,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ActiveReblitBootPublicationPlanError::SealedSnapshotFileByteLimit {
            path: PathBuf::from("EFI/Linux/a.efi"),
            limit: 8,
            actual: 9,
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
    assert_eq!(policy.max_logical_bytes, 10 * 1024 * 1024 * 1024);
    assert_eq!(policy.max_sealed_file_bytes, 512 * 1024 * 1024);
    assert_eq!(policy.max_generated_bytes, 16 * 1024 * 1024);
    assert_eq!(policy.max_generated_file_bytes, 1024 * 1024);
    assert_eq!(policy.timeout, Duration::from_secs(30));
    assert_eq!(ACTIVE_REBLIT_BOOT_OUTPUT_MODE, 0o644);
    assert_eq!(MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS, 8_336);
    assert_eq!(Path::new("a/b").components().count(), 2);
}
