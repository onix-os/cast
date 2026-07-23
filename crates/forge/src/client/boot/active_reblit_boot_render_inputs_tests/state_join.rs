use super::{support::*, *};

#[test]
fn excluded_history_is_not_promoted_from_stone_or_schema_history() {
    let fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12"),
        vec![StateSpec::one_kernel("6.6").with_cmdline(
            "lib/kernel/cmdline.d/10-history.cmdline",
            b"history.only=yes".as_slice(),
        )],
    );
    let stone = fixture.stone();
    assert_eq!(stone.kernel_count(), 2);
    fixture.exclude_history(0);
    let roots = fixture.roots(&stone);
    assert_eq!(roots.eligible_state_ids(), &[fixture.head.id]);
    let prepared = prepare_static(&fixture, &stone, &roots);
    assert_eq!(prepared.kernel_count(), 1);

    let local = fixture.local_policy();
    let root = fixture.root_intent();
    let attempt = prepared
        .revalidate_until(
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.installation,
            &local,
            &root,
            future_deadline(),
        )
        .unwrap();
    let kernel = attempt.kernels().next().unwrap();
    assert_eq!(kernel.state_id(), fixture.head.id);
    assert_eq!(kernel.version(), "6.12");
    assert!(!kernel.cmdline().contains("history.only"));

    let reserved_fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12"),
        vec![
            StateSpec::one_kernel("6.6")
                .with_cmdline("lib/kernel/cmdline.d/10-history.cmdline", b"root=forbidden".as_slice()),
        ],
    );
    let stone = reserved_fixture.stone();
    reserved_fixture.exclude_history(0);
    let roots = reserved_fixture.roots(&stone);
    let prepared = prepare_static(&reserved_fixture, &stone, &roots);
    let local = reserved_fixture.local_policy();
    let root = reserved_fixture.root_intent();
    assert!(matches!(
        prepared.revalidate_until(
            &reserved_fixture.state_db,
            &reserved_fixture.layout_db,
            &reserved_fixture.installation,
            &local,
            &root,
            future_deadline(),
        ),
        Err(ActiveReblitBootRenderInputsError::ReservedCmdlineKey {
            origin: ActiveReblitCmdlineSource::Package { .. },
            key: "root",
        })
    ));
}

#[test]
fn sole_excluded_history_kernel_cannot_produce_an_empty_render_attempt() {
    let fixture = RenderFixture::new(
        StateSpec {
            kernels: Vec::new(),
            cmdlines: Vec::new(),
        },
        vec![StateSpec::one_kernel("6.6")],
    );
    let stone = fixture.stone();
    assert_eq!(stone.kernel_count(), 1);
    fixture.exclude_history(0);
    let roots = fixture.roots(&stone);

    assert!(matches!(
        PreparedActiveReblitBootRenderInputs::prepare_until(&stone, &roots, &fixture.installation, future_deadline(),),
        Err(ActiveReblitBootRenderInputsError::NoRenderableKernel)
    ));
}
