use std::{cell::Cell, time::Instant};

use super::{support::*, *};

#[test]
fn command_line_bytes_admit_2047_reject_2048_before_output_and_bound_aggregate_bytes() {
    let admitted_fixture = simple_fixture();
    let admitted_token = exact_single_token(2_047, admitted_fixture.head.id);
    admitted_fixture.write_local("10-bound.cmdline", admitted_token);
    let stone = admitted_fixture.stone();
    let roots = admitted_fixture.roots(&stone);
    let prepared = prepare_static(&admitted_fixture, &stone, &roots);
    let local = admitted_fixture.local_policy();
    let root = admitted_fixture.root_intent();
    let attempt = prepared
        .revalidate_until(
            &admitted_fixture.state_db,
            &admitted_fixture.layout_db,
            &admitted_fixture.installation,
            &local,
            &root,
            future_deadline(),
        )
        .unwrap();
    assert_eq!(attempt.kernels().next().unwrap().cmdline().len(), 2_047);
    drop(attempt);

    let local_view = local
        .revalidate_until(&admitted_fixture.installation, future_deadline())
        .unwrap();
    let root_view = root
        .revalidate_until(&admitted_fixture.installation, future_deadline())
        .unwrap();
    let audited =
        cmdline::AuditedCmdlineInputs::prepare(&prepared.package_cmdlines, &local_view, future_deadline()).unwrap();
    let checkpoint = Cell::new(None);
    let mut aggregate_bytes = 0;
    let mut aggregate_tokens = 0;
    cmdline::materialize_kernel_cmdline_with_admission_checkpoint(
        &audited,
        admitted_fixture.head.id,
        "6.12",
        root_view.kernel_argument(),
        RENDER_INPUT_POLICY,
        &mut aggregate_bytes,
        &mut aggregate_tokens,
        future_deadline(),
        |point| checkpoint.set(Some(point)),
    )
    .unwrap();
    assert_eq!(
        checkpoint.get(),
        Some(cmdline::CmdlineMaterializationCheckpoint::Admitted {
            bytes: 2_047,
            tokens: 3,
        })
    );

    let rejected_fixture = simple_fixture();
    rejected_fixture.write_local("10-bound.cmdline", exact_single_token(2_048, rejected_fixture.head.id));
    let stone = rejected_fixture.stone();
    let roots = rejected_fixture.roots(&stone);
    let prepared = prepare_static(&rejected_fixture, &stone, &roots);
    let local = rejected_fixture.local_policy();
    let root = rejected_fixture.root_intent();
    assert!(matches!(
        prepared.revalidate_until(
            &rejected_fixture.state_db,
            &rejected_fixture.layout_db,
            &rejected_fixture.installation,
            &local,
            &root,
            future_deadline(),
        ),
        Err(ActiveReblitBootRenderInputsError::KernelCmdlineByteLimit {
            limit: 2_047,
            actual: 2_048,
            ..
        })
    ));
    let local_view = local
        .revalidate_until(&rejected_fixture.installation, future_deadline())
        .unwrap();
    let root_view = root
        .revalidate_until(&rejected_fixture.installation, future_deadline())
        .unwrap();
    let audited =
        cmdline::AuditedCmdlineInputs::prepare(&prepared.package_cmdlines, &local_view, future_deadline()).unwrap();
    let checkpoint = Cell::new(None);
    let mut aggregate_bytes = 0;
    let mut aggregate_tokens = 0;
    assert!(matches!(
        cmdline::materialize_kernel_cmdline_with_admission_checkpoint(
            &audited,
            rejected_fixture.head.id,
            "6.12",
            root_view.kernel_argument(),
            RENDER_INPUT_POLICY,
            &mut aggregate_bytes,
            &mut aggregate_tokens,
            future_deadline(),
            |point| checkpoint.set(Some(point)),
        ),
        Err(ActiveReblitBootRenderInputsError::KernelCmdlineByteLimit {
            limit: 2_047,
            actual: 2_048,
            ..
        })
    ));
    assert_eq!(checkpoint.get(), None, "rejected bytes must not reach output admission");

    assert_eq!(
        MAX_RENDER_TOTAL_CMDLINE_BYTES,
        MAX_RENDER_KERNELS * MAX_RENDER_CMDLINE_BYTES
    );
    let aggregate_fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12").with_kernel(KernelSpec::new("6.6")),
        Vec::new(),
    );
    let stone = aggregate_fixture.stone();
    let roots = aggregate_fixture.roots(&stone);
    let prepared = prepare_static(&aggregate_fixture, &stone, &roots);
    let local = aggregate_fixture.local_policy();
    let root = aggregate_fixture.root_intent();
    let baseline = prepared
        .revalidate_until(
            &aggregate_fixture.state_db,
            &aggregate_fixture.layout_db,
            &aggregate_fixture.installation,
            &local,
            &root,
            future_deadline(),
        )
        .unwrap();
    let actual = baseline.total_cmdline_bytes();
    drop(baseline);
    let policy = BootRenderInputPolicy {
        max_total_cmdline_bytes: actual - 1,
        ..RENDER_INPUT_POLICY
    };
    assert!(matches!(
        revalidate_with_policy_until_and_checkpoints(
            &prepared,
            &aggregate_fixture.state_db,
            &aggregate_fixture.layout_db,
            &aggregate_fixture.installation,
            &local,
            &root,
            policy,
            future_deadline(),
            || {},
            Instant::now,
        ),
        Err(ActiveReblitBootRenderInputsError::AggregateCmdlineByteLimit { .. })
    ));
}

#[test]
fn command_line_tokens_admit_1024_reject_1025_and_bound_aggregate_tokens() {
    let policy = BootRenderInputPolicy {
        max_cmdline_bytes: 16 * 1_024,
        max_total_cmdline_bytes: 128 * 16 * 1_024,
        ..RENDER_INPUT_POLICY
    };
    let admitted_fixture = simple_fixture();
    admitted_fixture.write_local("10-tokens.cmdline", repeated_tokens(1_022));
    let stone = admitted_fixture.stone();
    let roots = admitted_fixture.roots(&stone);
    let prepared = prepare_static(&admitted_fixture, &stone, &roots);
    let local = admitted_fixture.local_policy();
    let root = admitted_fixture.root_intent();
    let attempt = revalidate_with_policy_until_and_checkpoints(
        &prepared,
        &admitted_fixture.state_db,
        &admitted_fixture.layout_db,
        &admitted_fixture.installation,
        &local,
        &root,
        policy,
        future_deadline(),
        || {},
        Instant::now,
    )
    .unwrap();
    assert_eq!(attempt.kernels().next().unwrap().cmdline_tokens().len(), 1_024);

    let rejected_fixture = simple_fixture();
    rejected_fixture.write_local("10-tokens.cmdline", repeated_tokens(1_023));
    let stone = rejected_fixture.stone();
    let roots = rejected_fixture.roots(&stone);
    let prepared = prepare_static(&rejected_fixture, &stone, &roots);
    let local = rejected_fixture.local_policy();
    let root = rejected_fixture.root_intent();
    assert!(matches!(
        revalidate_with_policy_until_and_checkpoints(
            &prepared,
            &rejected_fixture.state_db,
            &rejected_fixture.layout_db,
            &rejected_fixture.installation,
            &local,
            &root,
            policy,
            future_deadline(),
            || {},
            Instant::now,
        ),
        Err(ActiveReblitBootRenderInputsError::KernelCmdlineTokenLimit {
            limit: 1_024,
            actual: 1_025,
            ..
        })
    ));

    assert_eq!(
        MAX_RENDER_TOTAL_CMDLINE_TOKENS,
        MAX_RENDER_KERNELS * MAX_RENDER_CMDLINE_TOKENS
    );
    let aggregate_fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12").with_kernel(KernelSpec::new("6.6")),
        Vec::new(),
    );
    let stone = aggregate_fixture.stone();
    let roots = aggregate_fixture.roots(&stone);
    let prepared = prepare_static(&aggregate_fixture, &stone, &roots);
    let local = aggregate_fixture.local_policy();
    let root = aggregate_fixture.root_intent();
    let aggregate_policy = BootRenderInputPolicy {
        max_total_cmdline_tokens: 3,
        ..policy
    };
    assert!(matches!(
        revalidate_with_policy_until_and_checkpoints(
            &prepared,
            &aggregate_fixture.state_db,
            &aggregate_fixture.layout_db,
            &aggregate_fixture.installation,
            &local,
            &root,
            aggregate_policy,
            future_deadline(),
            || {},
            Instant::now,
        ),
        Err(ActiveReblitBootRenderInputsError::AggregateCmdlineTokenLimit { limit: 3, actual: 4 })
    ));
}

#[test]
fn expired_entry_and_injected_terminal_deadlines_fail_closed() {
    let fixture = simple_fixture();
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    assert!(matches!(
        PreparedActiveReblitBootRenderInputs::prepare_until(&stone, &roots, &fixture.installation, expired_deadline(),),
        Err(ActiveReblitBootRenderInputsError::DeadlineExceeded {
            checkpoint: "prepared coordinator entry"
        })
    ));

    let prepared = prepare_static(&fixture, &stone, &roots);
    let local = fixture.local_policy();
    let root = fixture.root_intent();
    assert!(matches!(
        prepared.revalidate_until(
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.installation,
            &local,
            &root,
            expired_deadline(),
        ),
        Err(ActiveReblitBootRenderInputsError::DeadlineExceeded {
            checkpoint: "revalidated coordinator entry"
        })
    ));

    let deadline = future_deadline();
    let after_deadline = deadline.checked_add(std::time::Duration::from_secs(1)).unwrap();
    assert!(matches!(
        revalidate_with_policy_until_and_checkpoints(
            &prepared,
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.installation,
            &local,
            &root,
            RENDER_INPUT_POLICY,
            deadline,
            || {},
            move || after_deadline,
        ),
        Err(ActiveReblitBootRenderInputsError::DeadlineExceeded {
            checkpoint: "terminal revalidated aggregate"
        })
    ));

    let boundary_fixture = RenderFixture::new(
        StateSpec {
            kernels: (0..128)
                .map(|index| KernelSpec::new(format!("6.12.{index:03}")))
                .collect(),
            cmdlines: Vec::new(),
        },
        Vec::new(),
    );
    let stone = boundary_fixture.stone();
    assert_eq!(stone.kernel_count(), 128);
    let roots = boundary_fixture.roots(&stone);
    let prepared = prepare_static(&boundary_fixture, &stone, &roots);
    assert_eq!(prepared.kernel_count(), 128);
}

#[test]
fn trailing_stone_sandwich_rejects_database_change_after_semantic_materialization() {
    let fixture = simple_fixture();
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    let prepared = prepare_static(&fixture, &stone, &roots);
    let local = fixture.local_policy();
    let root = fixture.root_intent();

    assert!(matches!(
        revalidate_with_policy_until_and_checkpoints(
            &prepared,
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.installation,
            &local,
            &root,
            RENDER_INPUT_POLICY,
            future_deadline(),
            || fixture.add_irrelevant_head_layout(),
            Instant::now,
        ),
        Err(ActiveReblitBootRenderInputsError::Stone(
            crate::client::active_reblit_boot_inputs::ActiveReblitStoneBootInputsError::RevalidateProjection(
                crate::client::active_reblit_boot_projection::ActiveReblitBootProjectionError::LayoutChanged,
            )
        ))
    ));
}

fn exact_single_token(total_cmdline_bytes: usize, state_id: state::Id) -> String {
    let root = format!("root={ROOT_LOCATOR}");
    let cast = format!("cast.fstx={}", i32::from(state_id));
    let token_bytes = total_cmdline_bytes - root.len() - cast.len() - 2;
    assert!(token_bytes >= 2);
    format!("x={}", "v".repeat(token_bytes - 2))
}

fn repeated_tokens(count: usize) -> String {
    std::iter::repeat_n("x", count).collect::<Vec<_>>().join(" ")
}
