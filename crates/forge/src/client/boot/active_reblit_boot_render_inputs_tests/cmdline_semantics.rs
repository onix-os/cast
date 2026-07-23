use super::support::*;

#[test]
fn exact_state_version_scope_masks_and_source_order_are_deterministic() {
    let fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12")
            .with_kernel(KernelSpec::new("6.6"))
            .with_cmdline("lib/kernel/6.12/10-kernel.cmdline", b"kernel.only=yes".as_slice())
            .with_cmdline("lib/kernel/6.12/20-masked.cmdline", b"masked.kernel=yes".as_slice())
            .with_cmdline(
                "lib/kernel/cmdline.d/20-masked.cmdline",
                b"masked.package=yes".as_slice(),
            )
            .with_cmdline(
                "lib/kernel/cmdline.d/30-global.cmdline",
                b"global.package=yes".as_slice(),
            ),
        vec![StateSpec::one_kernel("5.15").with_cmdline(
            "lib/kernel/cmdline.d/10-history.cmdline",
            b"history.package=yes".as_slice(),
        )],
    );
    fixture.mask_local("20-masked.cmdline");
    fixture.write_local("40-local.cmdline", b"local.append=yes");
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    let prepared = prepare_static(&fixture, &stone, &roots);
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

    let kernels = attempt.kernels().collect::<Vec<_>>();
    assert_eq!(kernels.len(), 3);
    let head_612 = kernels
        .iter()
        .find(|kernel| kernel.state_id() == fixture.head.id && kernel.version() == "6.12")
        .unwrap();
    assert_eq!(
        head_612.cmdline_tokens().collect::<Vec<_>>(),
        [
            "kernel.only=yes",
            "global.package=yes",
            "local.append=yes",
            &format!("root={ROOT_LOCATOR}"),
            &format!("cast.fstx={}", i32::from(fixture.head.id)),
        ]
    );
    let head_66 = kernels
        .iter()
        .find(|kernel| kernel.state_id() == fixture.head.id && kernel.version() == "6.6")
        .unwrap();
    assert!(!head_66.cmdline().contains("kernel.only"));
    assert!(head_66.cmdline().starts_with("global.package=yes local.append=yes "));
    let history = kernels
        .iter()
        .find(|kernel| kernel.state_id() != fixture.head.id)
        .unwrap();
    assert!(history.cmdline().starts_with("history.package=yes local.append=yes "));
    assert!(!history.cmdline().contains("global.package"));
    assert!(
        kernels
            .iter()
            .all(|kernel| !kernel.cmdline().contains("masked.package") && !kernel.cmdline().contains("masked.kernel"))
    );
}

#[test]
fn same_name_regular_local_entry_appends_instead_of_overriding_package_data() {
    let fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12").with_cmdline("lib/kernel/6.12/10-same.cmdline", b"package.same=yes".as_slice()),
        Vec::new(),
    );
    fixture.write_local("10-same.cmdline", b"local.same=yes");
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    let prepared = prepare_static(&fixture, &stone, &roots);
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
    let tokens = kernel.cmdline_tokens().collect::<Vec<_>>();

    assert_eq!(&tokens[..2], ["package.same=yes", "local.same=yes"]);
}
