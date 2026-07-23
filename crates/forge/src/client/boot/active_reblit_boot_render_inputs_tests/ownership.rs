use std::ptr;

use super::super::{BoundActiveReblitBootAsset, RevalidatedActiveReblitBootRenderInputs};
use super::support::*;

fn kernel_asset_from_temporary_view<'a, 'attempt, 'stone, 'roots>(
    inputs: &'a RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
) -> BoundActiveReblitBootAsset<'a> {
    inputs
        .kernels()
        .next()
        .expect("fixture has one retained kernel")
        .kernel_asset()
}

fn initrd_asset_from_temporary_views<'a, 'attempt, 'stone, 'roots>(
    inputs: &'a RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
) -> BoundActiveReblitBootAsset<'a> {
    inputs
        .kernels()
        .find_map(|kernel| kernel.initrds().next().map(|initrd| initrd.asset()))
        .expect("fixture has one retained initrd")
}

#[test]
fn exact_owners_coordinates_and_namespace_are_retained_without_mutation() {
    let fixture = RenderFixture::new(
        StateSpec::one_kernel("6.12").with_kernel(
            KernelSpec::new("6.6")
                .with_initrd("10-base.initrd", b"base initrd".as_slice())
                .with_initrd("20-extra.initrd", b"extra initrd".as_slice()),
        ),
        Vec::new(),
    );
    let stone = fixture.stone();
    let roots = fixture.roots(&stone);
    let local = fixture.local_policy();
    let root = fixture.root_intent();
    let before = TreeSnapshot::capture(&fixture.installation.root);
    let prepared = prepare_static(&fixture, &stone, &roots);

    assert!(ptr::eq(prepared.source_owner, &stone));
    assert!(ptr::eq(prepared.roots_owner, &roots));
    assert_eq!(prepared.package_cmdlines.projected_state_ids(), stone.state_ids());
    let deadline = future_deadline();
    let attempt = prepared
        .revalidate_until(
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.installation,
            &local,
            &root,
            deadline,
        )
        .unwrap();
    assert_eq!(attempt.deadline(), deadline);
    let kernels = attempt.kernels().collect::<Vec<_>>();
    let systemd_boot = attempt.systemd_boot_asset();
    assert_eq!(systemd_boot.digest(), attempt.systemd_boot_digest());
    assert_eq!(systemd_boot.length(), attempt.systemd_boot_length());
    assert_eq!(
        stone
            .asset_at(usize::from(attempt.systemd_boot_binding_index()))
            .unwrap()
            .digest(),
        attempt.systemd_boot_digest()
    );
    assert_eq!(attempt.global_state(), fixture.head.id);
    assert_eq!(attempt.global_schema().os_id(), "head");
    assert_eq!(kernel_asset_from_temporary_view(&attempt).state_id(), fixture.head.id);
    assert_eq!(initrd_asset_from_temporary_views(&attempt).state_id(), fixture.head.id);
    assert_eq!(
        kernels.iter().map(|kernel| kernel.version()).collect::<Vec<_>>(),
        ["6.12", "6.6"]
    );
    for kernel in kernels {
        let asset = kernel.kernel_asset();
        assert_eq!(asset.state_id(), kernel.state_id());
        assert_eq!(asset.digest(), kernel.kernel_digest());
        assert_eq!(asset.length(), kernel.kernel_length());
        assert_eq!(
            stone
                .asset_at(usize::from(kernel.kernel_binding_index()))
                .unwrap()
                .digest(),
            kernel.kernel_digest()
        );
        assert_eq!(kernel.schema().os_id(), "head");
        assert_eq!(kernel.initrds().len(), usize::from(kernel.version() == "6.6") * 2);
        if kernel.version() == "6.6" {
            let initrds = kernel.initrds().collect::<Vec<_>>();
            assert_eq!(
                initrds
                    .iter()
                    .map(|initrd| initrd.logical_basename().to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                ["10-base.initrd", "20-extra.initrd"]
            );
            for initrd in initrds {
                let rebound = initrd.asset();
                assert_eq!(initrd.state_id(), kernel.state_id());
                assert_eq!(initrd.version(), kernel.version());
                assert_eq!(rebound.digest(), initrd.digest());
                assert_eq!(rebound.length(), initrd.length());
                assert_eq!(rebound.logical_path(), initrd.logical_path());
                assert_eq!(
                    stone.asset_at(usize::from(initrd.binding_index())).unwrap().digest(),
                    initrd.digest()
                );
            }
        }
    }
    drop(attempt);
    assert_eq!(TreeSnapshot::capture(&fixture.installation.root), before);
}
