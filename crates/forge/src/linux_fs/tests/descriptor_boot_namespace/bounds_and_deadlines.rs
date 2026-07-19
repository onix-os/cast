use std::time::{Duration, Instant};

use super::{support::*, *};

fn classify_one_file_with(
    limits: BootNamespaceAssessmentLimits,
) -> Result<FixtureBootNamespaceUsage, BootNamespaceAssessmentError> {
    let expected = b"bounded-stream";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", expected, expected);
    assess_with_limits(&requests, limits, &mut fixture).map(|(_, usage)| usage)
}

#[test]
fn request_count_limit_rejects_n_plus_one() {
    let expected = b"x";
    let requests = [request("a", expected), request("b", expected)];
    let mut limits = BootNamespaceAssessmentLimits::default();
    limits.max_requests = 1;
    let mut fixture = empty_fixture(vec![
        FixtureExpectedStream::new(expected),
        FixtureExpectedStream::new(expected),
    ]);

    let error = assess_with_limits(&requests, limits, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::RequestLimitExceeded { limit: 1, found: 2 }
    ));
}

#[test]
fn path_and_component_limits_are_independent() {
    let expected = b"x";

    let component_requests = [request("abc", expected)];
    let mut component_limits = BootNamespaceAssessmentLimits::default();
    component_limits.max_component_bytes = 2;
    let mut component_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
    let component_error =
        assess_with_limits(&component_requests, component_limits, &mut component_fixture).unwrap_err();
    assert!(matches!(
        component_error,
        BootNamespaceAssessmentError::RequestComponentNameLimitExceeded { .. }
    ));

    let path_requests = [request("abc", expected)];
    let mut path_limits = BootNamespaceAssessmentLimits::default();
    path_limits.max_path_bytes = 2;
    let mut path_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
    let path_error = assess_with_limits(&path_requests, path_limits, &mut path_fixture).unwrap_err();
    assert!(matches!(
        path_error,
        BootNamespaceAssessmentError::RequestPathLimitExceeded { .. }
    ));
}

#[test]
fn component_count_limit_rejects_n_plus_one() {
    let expected = b"x";
    let requests = [request("a/b", expected)];
    let mut limits = BootNamespaceAssessmentLimits::default();
    limits.max_components_per_request = 1;
    let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);

    let error = assess_with_limits(&requests, limits, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::RequestComponentLimitExceeded { limit: 1, found: 2, .. }
    ));
}

#[test]
fn hard_production_ceilings_reject_n_plus_one() {
    let production = BootNamespaceAssessmentLimits::default();
    let mut cases = Vec::new();
    macro_rules! above_ceiling {
        ($field:ident) => {{
            let mut limits = production;
            limits.$field += 1;
            cases.push((stringify!($field), limits));
        }};
    }
    above_ceiling!(max_requests);
    above_ceiling!(max_components_per_request);
    above_ceiling!(max_path_bytes);
    above_ceiling!(max_total_path_bytes);
    above_ceiling!(max_component_bytes);
    above_ceiling!(max_directory_entries);
    above_ceiling!(max_total_entries);
    above_ceiling!(max_name_bytes);
    above_ceiling!(max_total_name_bytes);
    above_ceiling!(max_read_bytes);
    above_ceiling!(max_work);
    above_ceiling!(max_descriptors);
    above_ceiling!(max_allocations);

    let expected = b"x";
    let requests = [request("target", expected)];
    for (field, limits) in cases {
        let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
        let error = assess_with_limits(&requests, limits, &mut fixture).unwrap_err();
        assert!(matches!(
            error,
            BootNamespaceAssessmentError::InvalidLimit { field: found } if found == field
        ));
        assert_eq!(fixture.now_calls(), 0);
    }
}

#[test]
fn single_request_path_ceiling_accepts_4095_and_rejects_4096() {
    let expected = b"x";
    let exact_path = vec!["a".repeat(255); 16].join("/");
    assert_eq!(exact_path.len(), 4_095);
    let exact_requests = [request(&exact_path, expected)];
    let mut exact_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
    let (assessment, usage) = assess(&exact_requests, &mut exact_fixture).unwrap();
    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Absent]);
    assert_eq!(usage.path_bytes, 4_095);

    let oversized_path = "a".repeat(4_096);
    let oversized_requests = [request(&oversized_path, expected)];
    let mut oversized_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
    let error = assess(&oversized_requests, &mut oversized_fixture).unwrap_err();
    assert!(matches!(
        error,
        BootNamespaceAssessmentError::RequestPathLimitExceeded {
            limit: 4_095,
            found: 4_096,
            ..
        }
    ));
}

#[test]
fn aggregate_request_path_limit_accepts_exact_n_and_rejects_n_plus_one() {
    let expected = b"x";
    let mut limits = BootNamespaceAssessmentLimits::default();
    assert_eq!(limits.max_total_path_bytes, 8 * 1024 * 1024);
    limits.max_total_path_bytes = 3;

    let exact_requests = [request("a", expected), request("bb", expected)];
    let mut exact_fixture = empty_fixture(vec![
        FixtureExpectedStream::new(expected),
        FixtureExpectedStream::new(expected),
    ]);
    let (assessment, usage) = assess_with_limits(&exact_requests, limits, &mut exact_fixture).unwrap();
    assert_eq!(
        assessment.states(),
        &[
            BootNamespaceDestinationState::Absent,
            BootNamespaceDestinationState::Absent,
        ]
    );
    assert_eq!(usage.path_bytes, 3);

    let oversized_requests = [request("a", expected), request("bb", expected), request("c", expected)];
    let mut oversized_fixture = empty_fixture(vec![
        FixtureExpectedStream::new(expected),
        FixtureExpectedStream::new(expected),
        FixtureExpectedStream::new(expected),
    ]);
    let error = assess_with_limits(&oversized_requests, limits, &mut oversized_fixture).unwrap_err();
    assert!(matches!(
        error,
        BootNamespaceAssessmentError::TotalRequestPathBytesLimitExceeded { limit: 3, found: 4 }
    ));
}

#[test]
fn noncanonical_relative_requests_are_rejected() {
    let expected = b"x";
    for path in ["", "/absolute", "trailing/", "a//b", "a/./b", "a/../b", "a\\b", "café"] {
        let requests = [request(path, expected)];
        let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);
        let error = assess(&requests, &mut fixture).unwrap_err();
        assert!(matches!(error, BootNamespaceAssessmentError::InvalidRequestPath { .. }));
    }
}

#[test]
fn exact_casefold_and_hierarchy_request_collisions_are_rejected() {
    let expected = b"x";
    for paths in [["same", "same"], ["same", "SAME"]] {
        let requests = [request(paths[0], expected), request(paths[1], expected)];
        let mut fixture = empty_fixture(vec![
            FixtureExpectedStream::new(expected),
            FixtureExpectedStream::new(expected),
        ]);
        let error = assess(&requests, &mut fixture).unwrap_err();
        assert!(matches!(error, BootNamespaceAssessmentError::RequestCollision { .. }));
    }

    let hierarchy = [request("parent", expected), request("parent/child", expected)];
    let mut fixture = empty_fixture(vec![
        FixtureExpectedStream::new(expected),
        FixtureExpectedStream::new(expected),
    ]);
    let error = assess(&hierarchy, &mut fixture).unwrap_err();
    assert!(matches!(
        error,
        BootNamespaceAssessmentError::RequestHierarchyCollision { .. }
    ));
}

#[test]
fn directory_entry_and_total_entry_limits_are_independent() {
    let expected = b"x";
    let requests = [request("target", expected)];
    let entries = vec![
        entry(b"a".to_vec(), FILE_A, BootNamespaceNodeKind::Regular),
        entry(b"b".to_vec(), FILE_B, BootNamespaceNodeKind::Regular),
    ];

    let mut per_directory_limits = BootNamespaceAssessmentLimits::default();
    per_directory_limits.max_directory_entries = 1;
    let mut per_directory_fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries.clone())],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    );
    let per_directory_error =
        assess_with_limits(&requests, per_directory_limits, &mut per_directory_fixture).unwrap_err();
    assert!(matches!(
        per_directory_error,
        BootNamespaceAssessmentError::DirectoryEntryLimitExceeded { .. }
    ));

    let mut total_limits = BootNamespaceAssessmentLimits::default();
    total_limits.max_total_entries = 1;
    let mut total_fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries)],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    );
    let total_error = assess_with_limits(&requests, total_limits, &mut total_fixture).unwrap_err();
    assert!(matches!(
        total_error,
        BootNamespaceAssessmentError::TotalEntryLimitExceeded { .. }
    ));
}

#[test]
fn raw_name_and_total_name_byte_limits_are_independent() {
    let expected = b"x";
    let requests = [request("target", expected)];
    let entries = vec![entry(b"four".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)];

    let mut raw_limits = BootNamespaceAssessmentLimits::default();
    raw_limits.max_name_bytes = 3;
    let mut raw_fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries.clone())],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    );
    let raw_error = assess_with_limits(&requests, raw_limits, &mut raw_fixture).unwrap_err();
    assert!(matches!(
        raw_error,
        BootNamespaceAssessmentError::RawNameLimitExceeded { .. }
    ));

    let mut total_limits = BootNamespaceAssessmentLimits::default();
    total_limits.max_total_name_bytes = 7;
    let mut total_fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries)],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    );
    let total_error = assess_with_limits(&requests, total_limits, &mut total_fixture).unwrap_err();
    assert!(matches!(
        total_error,
        BootNamespaceAssessmentError::TotalNameBytesLimitExceeded { .. }
    ));
}

#[test]
fn read_limit_accepts_exact_n_and_rejects_n_minus_one() {
    let baseline = classify_one_file_with(BootNamespaceAssessmentLimits::default()).unwrap();
    assert!(baseline.read_bytes > 0);

    let mut exact = BootNamespaceAssessmentLimits::default();
    exact.max_read_bytes = baseline.read_bytes;
    classify_one_file_with(exact).unwrap();

    let mut short = exact;
    short.max_read_bytes -= 1;
    let error = classify_one_file_with(short).unwrap_err();
    assert!(matches!(error, BootNamespaceAssessmentError::ReadLimitExceeded { .. }));
}

#[test]
fn read_budget_preflight_blocks_data_and_eof_observer_calls() {
    let expected = b"x";
    let requests = [request("loader.conf", expected)];

    let mut data_limits = BootNamespaceAssessmentLimits::default();
    data_limits.max_read_bytes = 1;
    let mut data_fixture = one_file_fixture(b"loader.conf", expected, expected);
    let data_error = assess_with_limits(&requests, data_limits, &mut data_fixture).unwrap_err();
    assert!(matches!(
        data_error,
        BootNamespaceAssessmentError::ReadLimitExceeded { limit: 1 }
    ));
    assert_eq!(data_fixture.read_calls(), (1, 0));

    let mut eof_limits = BootNamespaceAssessmentLimits::default();
    eof_limits.max_read_bytes = 3;
    let mut eof_fixture = one_file_fixture(b"loader.conf", expected, expected);
    let eof_error = assess_with_limits(&requests, eof_limits, &mut eof_fixture).unwrap_err();
    assert!(matches!(
        eof_error,
        BootNamespaceAssessmentError::ReadLimitExceeded { limit: 3 }
    ));
    assert_eq!(eof_fixture.read_calls(), (2, 1));
}

#[test]
fn work_limit_accepts_exact_n_and_rejects_n_minus_one() {
    let baseline = classify_one_file_with(BootNamespaceAssessmentLimits::default()).unwrap();
    assert!(baseline.work > 1);

    let mut exact = BootNamespaceAssessmentLimits::default();
    exact.max_work = baseline.work;
    classify_one_file_with(exact).unwrap();

    let mut short = exact;
    short.max_work -= 1;
    let error = classify_one_file_with(short).unwrap_err();
    assert!(matches!(error, BootNamespaceAssessmentError::WorkLimitExceeded { .. }));
}

#[test]
fn inventory_sort_work_accepts_exact_n_and_rejects_n_minus_one() {
    let expected = b"x";
    let requests = [request("target", expected)];
    let make_fixture = || {
        let entries = (0..8)
            .map(|index| {
                entry(
                    format!("entry-{index}"),
                    BootNamespaceNodeIdentity::new(7, 300 + index, ROOT.mount_id),
                    BootNamespaceNodeKind::Regular,
                )
            })
            .collect();
        FixtureBootNamespace::new(
            ROOT,
            vec![FixtureDirectory::stable(ROOT, entries)],
            Vec::new(),
            vec![FixtureExpectedStream::new(expected)],
            Instant::now(),
        )
    };

    let mut baseline_fixture = make_fixture();
    let (_, baseline) = assess(&requests, &mut baseline_fixture).unwrap();
    let mut exact = BootNamespaceAssessmentLimits::default();
    exact.max_work = baseline.work;
    let mut exact_fixture = make_fixture();
    assess_with_limits(&requests, exact, &mut exact_fixture).unwrap();

    let mut short = exact;
    short.max_work -= 1;
    let mut short_fixture = make_fixture();
    let error = assess_with_limits(&requests, short, &mut short_fixture).unwrap_err();
    assert!(matches!(error, BootNamespaceAssessmentError::WorkLimitExceeded { .. }));
}

#[test]
fn allocation_limit_accepts_exact_n_and_rejects_n_minus_one() {
    let baseline = classify_one_file_with(BootNamespaceAssessmentLimits::default()).unwrap();
    assert!(baseline.allocations > 1);

    let mut exact = BootNamespaceAssessmentLimits::default();
    exact.max_allocations = baseline.allocations;
    classify_one_file_with(exact).unwrap();

    let mut short = exact;
    short.max_allocations -= 1;
    let error = classify_one_file_with(short).unwrap_err();
    assert!(matches!(
        error,
        BootNamespaceAssessmentError::AllocationLimitExceeded { .. }
    ));
}

#[test]
fn descriptor_limit_accepts_exact_n_and_rejects_n_minus_one() {
    let baseline = classify_one_file_with(BootNamespaceAssessmentLimits::default()).unwrap();
    assert!(baseline.peak_descriptors > 1);

    let mut exact = BootNamespaceAssessmentLimits::default();
    exact.max_descriptors = baseline.peak_descriptors;
    classify_one_file_with(exact).unwrap();

    let mut short = exact;
    short.max_descriptors -= 1;
    let error = classify_one_file_with(short).unwrap_err();
    assert!(matches!(
        error,
        BootNamespaceAssessmentError::DescriptorLimitExceeded { .. }
    ));
}

#[test]
fn injected_allocation_and_observation_failures_are_typed() {
    let expected = b"x";
    let requests = [request("target", expected)];
    let mut allocation_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]).fail_allocation_at(1);
    let allocation_error = assess(&requests, &mut allocation_fixture).unwrap_err();
    assert!(matches!(
        allocation_error,
        BootNamespaceAssessmentError::AllocationFailed { .. }
    ));

    let mut observation_fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]).fail_observation_at(1);
    let observation_error = assess(&requests, &mut observation_fixture).unwrap_err();
    assert!(matches!(
        observation_error,
        BootNamespaceAssessmentError::ObservationFailed { .. }
    ));
}

#[test]
fn zero_limits_are_rejected_before_namespace_observation() {
    let expected = b"x";
    let requests = [request("target", expected)];
    let mut limits = BootNamespaceAssessmentLimits::default();
    limits.max_work = 0;
    let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);

    let error = assess_with_limits(&requests, limits, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::InvalidLimit { field: "max_work" }
    ));
    assert_eq!(fixture.now_calls(), 0);
}

#[test]
fn deadline_equality_is_admitted_but_first_expired_checkpoint_fails() {
    let now = Instant::now();
    let expected = b"x";
    let requests = [request("missing", expected)];
    let mut admitted = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, Vec::new())],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        now,
    );
    assess_fixture_boot_namespace_until(&requests, BootNamespaceAssessmentLimits::default(), now, &mut admitted)
        .unwrap();

    let mut expired = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, Vec::new())],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        now,
    )
    .expire_after_now_call(0, now + Duration::from_nanos(1));
    let error =
        assess_fixture_boot_namespace_until(&requests, BootNamespaceAssessmentLimits::default(), now, &mut expired)
            .unwrap_err();
    assert!(matches!(error, BootNamespaceAssessmentError::DeadlineExceeded { .. }));
}

#[test]
fn terminal_deadline_checkpoint_catches_late_expiry() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    let expected = b"x";
    let requests = [request("missing", expected)];
    let make_fixture = || {
        FixtureBootNamespace::new(
            ROOT,
            vec![FixtureDirectory::stable(ROOT, Vec::new())],
            Vec::new(),
            vec![FixtureExpectedStream::new(expected)],
            now,
        )
    };
    let mut baseline = make_fixture();
    assess_fixture_boot_namespace_until(
        &requests,
        BootNamespaceAssessmentLimits::default(),
        deadline,
        &mut baseline,
    )
    .unwrap();
    let calls = baseline.now_calls();
    assert!(calls > 1);

    let mut late = make_fixture().expire_after_now_call(calls - 1, deadline + Duration::from_nanos(1));
    let error =
        assess_fixture_boot_namespace_until(&requests, BootNamespaceAssessmentLimits::default(), deadline, &mut late)
            .unwrap_err();
    assert!(matches!(error, BootNamespaceAssessmentError::DeadlineExceeded { .. }));
}
