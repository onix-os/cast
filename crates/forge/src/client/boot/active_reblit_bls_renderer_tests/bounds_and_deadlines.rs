use super::*;

#[test]
fn fat_unsafe_version_initrd_and_entry_components_fail_closed() {
    let deadline = support::future_deadline();
    with_render_inputs!(
        support::RenderFixture::new(support::StateSpec::one_kernel("6:12"), Vec::new()),
        deadline,
        |_fixture, inputs| {
            assert!(matches!(
                RenderedActiveReblitBlsRequests::render(&inputs),
                Err(ActiveReblitBlsRendererError::InvalidComponent {
                    kind: ActiveReblitBlsComponentKind::KernelVersion,
                    reason: ActiveReblitBlsComponentReason::FatForbidden,
                })
            ));
        }
    );

    assert!(matches!(
        paths::require_component("6/12", ActiveReblitBlsComponentKind::KernelVersion),
        Err(ActiveReblitBlsRendererError::InvalidComponent {
            kind: ActiveReblitBlsComponentKind::KernelVersion,
            reason: ActiveReblitBlsComponentReason::FatForbidden,
        })
    ));

    let spec = support::StateSpec::one_kernel("6.12")
        .with_kernel(support::KernelSpec::new("6.13").with_initrd("bad~name.initrd", b"x".as_slice()));
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |_fixture, inputs| {
            assert!(matches!(
                RenderedActiveReblitBlsRequests::render(&inputs),
                Err(ActiveReblitBlsRendererError::InvalidComponent {
                    kind: ActiveReblitBlsComponentKind::InitrdBasename,
                    reason: ActiveReblitBlsComponentReason::FatShortNameMarker,
                })
            ));
        }
    );

    let composed_entry_overflow = "v".repeat(250);
    with_render_inputs!(
        support::RenderFixture::new(support::StateSpec::one_kernel(composed_entry_overflow), Vec::new(),),
        deadline,
        |_fixture, inputs| {
            assert!(matches!(
                RenderedActiveReblitBlsRequests::render(&inputs),
                Err(ActiveReblitBlsRendererError::InvalidComponent {
                    kind: ActiveReblitBlsComponentKind::EntryFilename,
                    reason: ActiveReblitBlsComponentReason::TooLong,
                })
            ));
        }
    );
}

#[test]
fn checksum_identity_has_fixed_lowercase_widths_and_no_version_or_state_component() {
    let mut budget = RenderBudget::new(BLS_POLICY, support::future_deadline()).unwrap();
    let path = paths::payload_path(
        "head",
        0xabcdef,
        0x12,
        "vmlinuz",
        ActiveReblitBlsComponentKind::InitrdBasename,
        &mut budget,
    )
    .unwrap();
    assert_eq!(
        path,
        Path::new("EFI/head/xxh3-00000000000000000000000000abcdef-l0000000000000012/vmlinuz")
    );
    let identity = path.to_str().unwrap().split('/').nth(2).unwrap();
    assert_eq!(identity.len(), 55);
    assert!(identity.bytes().all(|byte| !byte.is_ascii_uppercase()));
    assert_eq!(path.components().count(), 4);
}

#[test]
fn initrd_leaf_fat_limit_admits_255_bytes_and_rejects_256() {
    let deadline = support::future_deadline();
    let admitted = format!("{}.initrd", "a".repeat(248));
    assert_eq!(admitted.len(), 255);
    let spec = support::StateSpec::one_kernel("6.12")
        .with_kernel(support::KernelSpec::new("6.13").with_initrd(admitted.clone(), b"at-limit".as_slice()));
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |_fixture, inputs| {
            let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let topology = topology::alias_topology();
            let (plan, _) = fixture_plan(rendered, &topology);
            assert!(
                plan.outputs()
                    .iter()
                    .any(|output| output.relative_path().ends_with(&admitted))
            );
        }
    );

    let rejected = format!("{}.initrd", "a".repeat(249));
    assert_eq!(rejected.len(), 256);
    assert!(matches!(
        paths::require_component(&rejected, ActiveReblitBlsComponentKind::InitrdBasename),
        Err(ActiveReblitBlsRendererError::InvalidComponent {
            kind: ActiveReblitBlsComponentKind::InitrdBasename,
            reason: ActiveReblitBlsComponentReason::TooLong,
        })
    ));
}

#[test]
fn generated_file_and_total_byte_bounds_admit_n_and_reject_n_plus_one_before_materialization() {
    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |_fixture, inputs| {
        document::reset_materialization_starts();
        let baseline = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let baseline_materializations = document::take_materialization_starts();
        let total = baseline.generated_bytes;
        let topology = topology::alias_topology();
        let (baseline_plan, _) = fixture_plan(baseline, &topology);
        let largest = baseline_plan
            .outputs()
            .iter()
            .filter_map(|output| output.source().generated_bytes().map(<[u8]>::len))
            .max()
            .unwrap();

        let mut policy = BLS_POLICY;
        policy.max_generated_file_bytes = largest;
        policy.max_generated_total_bytes = total;
        document::reset_materialization_starts();
        render_with_policy_until(&inputs, policy, deadline, Instant::now).unwrap();
        assert_eq!(document::take_materialization_starts(), baseline_materializations);

        policy.max_generated_file_bytes = largest - 1;
        document::reset_materialization_starts();
        let failed_path = match render_with_policy_until(&inputs, policy, deadline, Instant::now) {
            Err(ActiveReblitBlsRendererError::GeneratedFileByteLimit { path, .. }) => path,
            Err(other) => panic!("unexpected file-bound error: {other}"),
            Ok(_) => panic!("N + 1 generated file unexpectedly passed admission"),
        };
        let starts_before_file_rejection = document::take_materialization_starts();
        assert!(!starts_before_file_rejection.contains(&failed_path));

        policy.max_generated_file_bytes = largest;
        policy.max_generated_total_bytes = total - 1;
        document::reset_materialization_starts();
        assert!(matches!(
            render_with_policy_until(&inputs, policy, deadline, Instant::now),
            Err(ActiveReblitBlsRendererError::GeneratedTotalByteLimit { .. })
        ));
        let starts_before_total_rejection = document::take_materialization_starts();
        assert!(starts_before_total_rejection.len() < baseline_materializations.len());
        assert_eq!(
            starts_before_total_rejection,
            baseline_materializations[..starts_before_total_rejection.len()]
        );
    });
}

#[test]
fn request_path_initrd_and_work_bounds_admit_n_and_reject_n_plus_one() {
    let deadline = support::future_deadline();
    let spec = support::StateSpec::one_kernel("6.12").with_kernel(
        support::KernelSpec::new("6.13")
            .with_initrd("one.initrd", b"1".as_slice())
            .with_initrd("two.initrd", b"2".as_slice()),
    );
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |_fixture, inputs| {
            let baseline = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let request_count = baseline.requests.len();
            let path_bytes = baseline.path_bytes;
            let work = baseline.render_work;

            let mut policy = BLS_POLICY;
            policy.max_requests = request_count;
            policy.max_path_bytes = path_bytes;
            policy.max_work = work;
            policy.max_initrds_per_kernel = 2;
            render_with_policy_until(&inputs, policy, deadline, Instant::now).unwrap();

            policy.max_requests = request_count - 1;
            assert!(matches!(
                render_with_policy_until(&inputs, policy, deadline, Instant::now),
                Err(ActiveReblitBlsRendererError::RequestCountLimit { .. })
            ));
            policy.max_requests = request_count;
            policy.max_path_bytes = path_bytes - 1;
            assert!(matches!(
                render_with_policy_until(&inputs, policy, deadline, Instant::now),
                Err(ActiveReblitBlsRendererError::PathByteLimit { .. })
            ));
            policy.max_path_bytes = path_bytes;
            policy.max_work = work - 1;
            assert!(matches!(
                render_with_policy_until(&inputs, policy, deadline, Instant::now),
                Err(ActiveReblitBlsRendererError::WorkLimit { .. })
            ));
            policy.max_work = work;
            policy.max_initrds_per_kernel = 1;
            assert!(matches!(
                render_with_policy_until(&inputs, policy, deadline, Instant::now),
                Err(ActiveReblitBlsRendererError::RequestCountLimit { .. })
            ));
        }
    );
}

#[test]
fn mismatched_input_and_topology_deadlines_fail_before_publication_planning() {
    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |_fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let different = deadline.checked_sub(Duration::from_millis(1)).unwrap();
        assert!(matches!(
            rendered.into_fixture_publication_plan(topology.bound(), different, Instant::now),
            Err(ActiveReblitBlsRendererError::DeadlineMismatch { .. })
        ));
    });
}

#[test]
fn expired_and_injected_post_sort_post_plan_and_terminal_deadlines_fail_closed() {
    let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        require_deadline(expired, "injected renderer entry", Instant::now()),
        Err(ActiveReblitBlsRendererError::DeadlineExceeded { .. })
    ));

    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |_fixture, inputs| {
        assert!(matches!(
            render_with_policy_and_checkpoints(
                &inputs,
                BLS_POLICY,
                deadline,
                || deadline + Duration::from_millis(1),
                Instant::now,
            ),
            Err(ActiveReblitBlsRendererError::DeadlineExceeded {
                checkpoint: "payload sort completion"
            })
        ));
        assert!(matches!(
            render_with_policy_until(&inputs, BLS_POLICY, deadline, || deadline + Duration::from_millis(1)),
            Err(ActiveReblitBlsRendererError::DeadlineExceeded {
                checkpoint: "terminal rendered BLS requests"
            })
        ));
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        assert!(matches!(
            rendered.into_fixture_publication_plan(topology.bound(), deadline, || deadline + Duration::from_millis(1)),
            Err(ActiveReblitBlsRendererError::DeadlineExceeded {
                checkpoint: "terminal fixture publication plan"
            })
        ));
    });
}
