use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt as _,
    path::Path,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    Installation, db, state,
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
    },
    linux_fs::descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState,
        RetainedBootNamespaceAssessmentLimits, assess_retained_boot_namespace_until,
    },
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;

const BOOTLOADER_BYTES: &[u8] = b"render fixture systemd bootloader";
const KERNEL_BYTES: &[u8] = b"render kernel 6.12";

macro_rules! with_bound_alias_plan {
    (|$fixture:ident, $plan:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = $fixture.local_policy();
        let root_intent = $fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$fixture.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture = AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture.prepare_until(deadline).unwrap();
        let topology = topology_prepared
            .revalidate_until(topology_fixture.installation(), deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

#[test]
fn alias_plan_binds_one_ordered_domain_and_streams_exact_generated_and_sealed_sources() {
    with_bound_alias_plan!(|fixture, plan| {
        let original = support::TreeSnapshot::capture(&fixture.installation.root);
        let destination = tempfile::tempdir().unwrap();
        for output in plan.outputs() {
            let target = destination.path().join(output.relative_path());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            let bytes = if let Some(generated) = output.generated_bytes() {
                generated
            } else if output.relative_path().ends_with(Path::new("vmlinuz")) {
                KERNEL_BYTES
            } else {
                BOOTLOADER_BYTES
            };
            assert_eq!(output.expected_length(), bytes.len() as u64);
            assert_eq!(output.expected_digest(), xxhash_rust::xxh3::xxh3_128(bytes));
            fs::write(target, bytes).unwrap();
        }

        let bound = plan.bind_boot_namespace_inputs().unwrap();
        let BoundActiveReblitBootNamespaceInputs::BootAliasesEsp { shared } = &bound else {
            panic!("alias publication layout must produce one shared domain")
        };
        assert_eq!(shared.requests().len(), plan.publication_count());
        assert_eq!(shared.expected_sources().len(), plan.publication_count());
        assert_eq!(shared.plan_indices(), (0..plan.publication_count()).collect::<Vec<_>>());

        let root = OpenOptions::new()
            .read(true)
            .custom_flags(
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            )
            .open(destination.path())
            .unwrap();
        let assessment = assess_retained_boot_namespace_until(
            &root,
            shared.requests(),
            shared.expected_sources(),
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            support::future_deadline(),
        )
        .unwrap();
        assert_eq!(
            assessment.states(),
            vec![BootNamespaceDestinationState::Exact; plan.publication_count()]
        );
        assert_eq!(original, support::TreeSnapshot::capture(&fixture.installation.root));
    });
}

#[test]
fn retained_layout_routes_distinct_roots_without_reordering_global_indices() {
    let roots = [
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootDestinationRoot::Boot,
    ];
    let metrics = BindingMetrics {
        publications: roots.len(),
        esp_publications: 2,
        boot_publications: 3,
        ..BindingMetrics::default()
    };
    let mut now = Instant::now;
    let mut budget = BindingBudget::new(BOOT_NAMESPACE_INPUT_POLICY, support::future_deadline(), &mut now).unwrap();
    let mut builders = DestinationBuilders::allocate(
        ActiveReblitBootDestinationLayout::DistinctXbootldr,
        metrics,
        &mut budget,
    )
    .unwrap();
    let bytes = b"borrowed fixture source";
    for (index, root) in roots.iter().copied().enumerate() {
        builders
            .push(
                destination_slot(ActiveReblitBootDestinationLayout::DistinctXbootldr, root),
                BootNamespaceRequest::new("loader/fixture", bytes.len() as u64, xxhash_rust::xxh3::xxh3_128(bytes)),
                RetainedBootNamespaceExpectedSource::generated(bytes),
                index,
            )
            .unwrap();
    }
    let BoundActiveReblitBootNamespaceInputs::DistinctXbootldr { esp, xbootldr } = builders.finish().unwrap()
    else {
        panic!("distinct retained layout must produce two domains")
    };
    assert_eq!(esp.plan_indices(), [1, 3]);
    assert_eq!(esp.requests().len(), 2);
    assert_eq!(esp.expected_sources().len(), 2);
    assert_eq!(xbootldr.plan_indices(), [0, 2, 4]);
    assert_eq!(xbootldr.requests().len(), 3);
    assert_eq!(xbootldr.expected_sources().len(), 3);
    assert!(roots.iter().copied().all(|root| {
        destination_slot(ActiveReblitBootDestinationLayout::BootAliasesEsp, root) == DestinationSlot::Shared
    }));
}

#[test]
fn count_path_logical_generated_and_work_bounds_accept_n_and_reject_n_minus_one() {
    with_bound_alias_plan!(|_fixture, plan| {
        let mut now = Instant::now;
        let mut budget = BindingBudget::new(BOOT_NAMESPACE_INPUT_POLICY, plan.input_deadline(), &mut now).unwrap();
        let metrics = scan_plan(&plan, &mut budget).unwrap();
        let largest_generated = plan
            .outputs()
            .filter_map(|output| output.generated_bytes().map(<[u8]>::len))
            .max()
            .unwrap();
        let exact = BootNamespaceInputPolicy {
            max_publications: metrics.publications,
            max_path_bytes: metrics.path_bytes,
            max_logical_bytes: metrics.logical_bytes,
            max_generated_bytes: metrics.generated_bytes,
            max_generated_file_bytes: largest_generated,
            max_work: metrics.publications * 2,
        };

        let mut exact_now = Instant::now;
        bind_with_policy_and_clocks(&plan, exact, &mut exact_now, Instant::now).unwrap();

        let mut count = exact;
        count.max_publications -= 1;
        assert_bound_error(&plan, count, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::PublicationCountLimit { .. })
        });
        let mut path = exact;
        path.max_path_bytes -= 1;
        assert_bound_error(&plan, path, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::PathByteLimit { .. })
        });
        let mut logical = exact;
        logical.max_logical_bytes -= 1;
        assert_bound_error(&plan, logical, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::LogicalByteLimit { .. })
        });
        let mut generated = exact;
        generated.max_generated_bytes -= 1;
        assert_bound_error(&plan, generated, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::GeneratedTotalByteLimit { .. })
        });
        let mut generated_file = exact;
        generated_file.max_generated_file_bytes -= 1;
        assert_bound_error(&plan, generated_file, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::GeneratedFileByteLimit { .. })
        });
        let mut work = exact;
        work.max_work -= 1;
        assert_bound_error(&plan, work, |error| {
            matches!(error, ActiveReblitBootNamespaceInputError::WorkLimit { .. })
        });
    });
}

#[test]
fn inherited_deadline_is_checked_at_entry_and_after_complete_binding() {
    with_bound_alias_plan!(|_fixture, plan| {
        let expired = plan.input_deadline().checked_add(Duration::from_nanos(1)).unwrap();
        let mut expired_now = || expired;
        assert!(matches!(
            bind_with_policy_and_clocks(&plan, BOOT_NAMESPACE_INPUT_POLICY, &mut expired_now, || expired),
            Err(ActiveReblitBootNamespaceInputError::DeadlineExceeded {
                checkpoint: "binding entry"
            })
        ));

        let safe = Instant::now();
        let mut safe_now = || safe;
        assert!(matches!(
            bind_with_policy_and_clocks(&plan, BOOT_NAMESPACE_INPUT_POLICY, &mut safe_now, || expired),
            Err(ActiveReblitBootNamespaceInputError::DeadlineExceeded {
                checkpoint: "terminal binding"
            })
        ));
    });
}

fn assert_bound_error(
    plan: &BoundActiveReblitBlsPublicationPlan<'_, '_, '_, '_, '_, '_>,
    policy: BootNamespaceInputPolicy,
    predicate: impl FnOnce(&ActiveReblitBootNamespaceInputError) -> bool,
) {
    let mut now = Instant::now;
    let error = bind_with_policy_and_clocks(plan, policy, &mut now, Instant::now)
        .err()
        .expect("N-1 policy must fail closed");
    assert!(predicate(&error), "unexpected error: {error:?}");
}
