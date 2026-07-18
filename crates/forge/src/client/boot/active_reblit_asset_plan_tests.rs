use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use astr::AStr;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::{State, client::EMPTY_FILE_DIGEST, db, package, state::Selection};

fn package(name: &str) -> package::Id {
    package::Id::from(name.to_owned())
}

fn regular(digest: u128, path: &str) -> StonePayloadLayoutRecord {
    regular_with_mode(digest, path, nix::libc::S_IFREG | 0o644)
}

fn regular_with_mode(digest: u128, path: &str, mode: u32) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(digest, AStr::from(path.to_owned())),
    }
}

fn symlink(target: &str, path: &str) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFLNK | 0o777,
        tag: 0,
        file: StonePayloadLayoutFile::Symlink(AStr::from(target.to_owned()), AStr::from(path.to_owned())),
    }
}

fn directory(path: &str) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory(AStr::from(path.to_owned())),
    }
}

fn build_projection(
    state_packages: &[&[&str]],
    layouts: &[(&str, StonePayloadLayoutRecord)],
) -> (PreparedActiveReblitBootProjection, Vec<State>) {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let mut packages = BTreeMap::new();
    for package_name in state_packages.iter().flat_map(|packages| packages.iter()).copied() {
        packages
            .entry(package_name.to_owned())
            .or_insert_with(|| package(package_name));
    }
    for (package_name, _) in layouts {
        packages
            .entry((*package_name).to_owned())
            .or_insert_with(|| package(package_name));
    }

    let states = state_packages
        .iter()
        .enumerate()
        .map(|(index, selected)| {
            let selections = selected
                .iter()
                .map(|name| Selection::explicit(packages[*name].clone()))
                .collect::<Vec<_>>();
            state_db
                .add(&selections, Some(&format!("state-{index}")), None)
                .unwrap()
        })
        .collect::<Vec<_>>();

    let rows = layouts
        .iter()
        .map(|(name, layout)| (&packages[*name], layout))
        .collect::<Vec<_>>();
    layout_db.batch_add(rows).unwrap();
    let prepared =
        PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, states.first().expect("head state").id)
            .unwrap();
    (prepared, states)
}

fn complete_layouts(package: &str) -> Vec<(&str, StonePayloadLayoutRecord)> {
    vec![
        (package, regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        (package, regular(2, "lib/os-info.json")),
        (package, regular(3, "lib/kernel/cmdline.d/00-global.cmdline")),
        (package, regular(4, "lib/kernel/6.12/vmlinuz")),
        (package, regular(5, "lib/kernel/6.12/boot.initrd")),
        (package, regular(6, "lib/kernel/6.12/kernel.cmdline")),
        (package, regular(7, "lib/kernel/6.12/boot.json")),
        (package, regular(8, "lib/kernel/6.12/config")),
        (package, regular(9, "lib/kernel/6.12/System.map")),
        (package, regular(10, "share/not-a-boot-asset")),
    ]
}

fn ready(outcome: BootAssetPlanOutcome) -> PreparedActiveReblitBootAssetPlan {
    match outcome {
        BootAssetPlanOutcome::Ready(plan) => plan,
        BootAssetPlanOutcome::NotApplicable(reason) => {
            panic!("expected a ready plan, got {reason:?}")
        }
    }
}

fn planning_error(
    result: Result<BootAssetPlanOutcome, ActiveReblitBootAssetPlanError>,
) -> ActiveReblitBootAssetPlanError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("expected boot asset planning to fail"),
    }
}

#[test]
fn complete_plan_is_state_scoped_deterministic_and_role_complete() {
    let mut layouts = complete_layouts("head");
    layouts.extend([
        ("history", regular(12, "lib/kernel/6.6/vmlinuz")),
        ("history", regular(13, "lib/kernel/6.6/history.initrd")),
    ]);
    let (projection, states) = build_projection(&[&["head"], &["history"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    assert_eq!(plan.state_ids(), [states[0].id, states[1].id]);
    assert_eq!(plan.kernel_count(), 2);
    assert_eq!(plan.assets().len(), 8);
    assert_eq!(plan.systemd_boot().state_id(), states[0].id);
    assert_eq!(plan.systemd_boot().digest(), 1);
    assert_eq!(
        plan.assets()
            .iter()
            .map(|asset| asset.role().clone())
            .collect::<Vec<_>>(),
        [
            BootAssetRole::Initrd {
                version: "6.12".to_owned(),
            },
            BootAssetRole::KernelCmdline {
                version: "6.12".to_owned(),
            },
            BootAssetRole::Kernel {
                version: "6.12".to_owned(),
            },
            BootAssetRole::GlobalCmdline,
            BootAssetRole::OsInfo,
            BootAssetRole::SystemdBoot,
            BootAssetRole::Initrd {
                version: "6.6".to_owned(),
            },
            BootAssetRole::Kernel {
                version: "6.6".to_owned(),
            },
        ]
    );
    assert_eq!(
        plan.schema_requirements(),
        [
            PlannedBootSchemaRequirement {
                state_id: states[0].id,
                source: BootSchemaSource::OsInfoAsset,
                fallback: BootSchemaFallback::Required,
            },
            PlannedBootSchemaRequirement {
                state_id: states[1].id,
                source: BootSchemaSource::GeneratedOsRelease,
                fallback: BootSchemaFallback::Global,
            },
        ]
    );
    for excluded in [
        "/usr/lib/kernel/6.12/boot.json",
        "/usr/lib/kernel/6.12/config",
        "/usr/lib/kernel/6.12/System.map",
    ] {
        assert!(
            plan.assets()
                .iter()
                .all(|asset| asset.logical_path() != Path::new(excluded)),
            "os-info must win and unused metadata must stay outside the snapshot plan: {excluded}"
        );
    }
}

#[test]
fn canonical_empty_digest_is_rejected_for_every_critical_boot_role() {
    let cases = [
        ("lib/systemd/boot/efi/systemd-bootx64.efi", BootAssetRole::SystemdBoot),
        ("lib/os-info.json", BootAssetRole::OsInfo),
        (
            "lib/kernel/6.12/vmlinuz",
            BootAssetRole::Kernel {
                version: "6.12".to_owned(),
            },
        ),
        (
            "lib/kernel/6.12/boot.initrd",
            BootAssetRole::Initrd {
                version: "6.12".to_owned(),
            },
        ),
    ];

    for (critical_path, expected_role) in cases {
        let digest = |path: &str, nonempty| {
            if path == critical_path {
                EMPTY_FILE_DIGEST
            } else {
                nonempty
            }
        };
        let layouts = vec![
            (
                "head",
                regular(
                    digest("lib/systemd/boot/efi/systemd-bootx64.efi", 1),
                    "lib/systemd/boot/efi/systemd-bootx64.efi",
                ),
            ),
            ("head", regular(digest("lib/os-info.json", 2), "lib/os-info.json")),
            (
                "head",
                regular(digest("lib/kernel/6.12/vmlinuz", 3), "lib/kernel/6.12/vmlinuz"),
            ),
            (
                "head",
                regular(digest("lib/kernel/6.12/boot.initrd", 4), "lib/kernel/6.12/boot.initrd"),
            ),
        ];
        let (projection, states) = build_projection(&[&["head"]], &layouts);

        match planning_error(projection.prepare_asset_plan()) {
            ActiveReblitBootAssetPlanError::EmptyCriticalAsset { state_id, path, role } => {
                assert_eq!(state_id, i32::from(states[0].id));
                assert_eq!(path, PathBuf::from("/usr").join(critical_path));
                assert_eq!(role, expected_role);
            }
            error => panic!("expected empty critical asset error for {critical_path}, got {error:?}"),
        }
    }
}

#[test]
fn optional_empty_assets_share_one_sorted_snapshot_digest() {
    let layouts = [
        ("head", regular(31, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(23, "lib/os-info.json")),
        (
            "head",
            regular(EMPTY_FILE_DIGEST, "lib/kernel/cmdline.d/00-global.cmdline"),
        ),
        ("head", regular(17, "lib/kernel/6.12/vmlinuz")),
        ("head", regular(EMPTY_FILE_DIGEST, "lib/kernel/6.12/kernel.cmdline")),
        ("head", regular(EMPTY_FILE_DIGEST, "lib/kernel/6.12/boot.json")),
        ("head", regular(EMPTY_FILE_DIGEST, "lib/kernel/6.12/config")),
        ("head", regular(EMPTY_FILE_DIGEST, "lib/kernel/6.12/System.map")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    let empty_roles = plan
        .assets()
        .iter()
        .filter(|asset| asset.digest() == EMPTY_FILE_DIGEST)
        .map(|asset| asset.role().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        empty_roles,
        BTreeSet::from([
            BootAssetRole::GlobalCmdline,
            BootAssetRole::KernelCmdline {
                version: "6.12".to_owned(),
            },
        ])
    );

    let mut expected_digests = vec![31, 23, 17, EMPTY_FILE_DIGEST];
    expected_digests.sort_unstable();
    assert_eq!(plan.snapshot_digests(), expected_digests);
}

#[test]
fn role_references_above_snapshot_limit_share_a_bounded_digest_inventory() {
    let repeated_initrd_count = MAX_BOOT_PLAN_SNAPSHOT_DIGESTS + 1;
    let shared_digest = 42;
    let mut layouts = vec![
        (
            "head",
            regular(shared_digest, "lib/systemd/boot/efi/systemd-bootx64.efi"),
        ),
        ("head", regular(shared_digest, "lib/kernel/6.12/vmlinuz")),
    ];
    layouts.extend((0..repeated_initrd_count).map(|index| {
        (
            "head",
            regular(shared_digest, &format!("lib/kernel/6.12/fragment-{index}.initrd")),
        )
    }));
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    assert_eq!(plan.assets().len(), repeated_initrd_count + 2);
    assert!(plan.assets().len() > MAX_BOOT_PLAN_SNAPSHOT_DIGESTS);
    assert_eq!(plan.snapshot_digests(), [shared_digest]);
}

#[test]
fn no_head_systemd_boot_asset_is_not_applicable() {
    let mut layouts = (0..=MAX_BOOT_PLAN_KERNELS)
        .map(|index| {
            (
                "head",
                regular(index as u128 + 1, &format!("lib/kernel/version-{index}/vmlinuz")),
            )
        })
        .collect::<Vec<_>>();
    layouts.push(("history", symlink("/usr/lib/missing", "lib/kernel/6.6/vmlinuz")));
    let (projection, _) = build_projection(&[&["head"], &["history"]], &layouts);

    assert!(matches!(
        projection.prepare_asset_plan().unwrap(),
        BootAssetPlanOutcome::NotApplicable(BootAssetPlanNotApplicable::NoSystemdBootAsset)
    ));
}

#[test]
fn systemd_boot_without_any_kernel_is_not_applicable() {
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink("/usr/lib/missing", "lib/os-info.json")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    assert!(matches!(
        projection.prepare_asset_plan().unwrap(),
        BootAssetPlanOutcome::NotApplicable(BootAssetPlanNotApplicable::NoKernel)
    ));
}

#[test]
fn kernel_less_history_does_not_contribute_unused_schema_or_cmdline_assets() {
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(2, "lib/os-info.json")),
        ("head", regular(3, "lib/kernel/6.12/vmlinuz")),
        ("history", symlink("/usr/lib/missing-os-info", "lib/os-info.json")),
        (
            "history",
            symlink("/usr/lib/missing-cmdline", "lib/kernel/cmdline.d/history.cmdline"),
        ),
    ];
    let (projection, states) = build_projection(&[&["head"], &["history"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    assert_eq!(plan.state_ids(), [states[0].id, states[1].id]);
    assert!(plan.assets().iter().all(|asset| asset.state_id() != states[1].id));
    assert_eq!(
        plan.schema_requirements(),
        [PlannedBootSchemaRequirement {
            state_id: states[0].id,
            source: BootSchemaSource::OsInfoAsset,
            fallback: BootSchemaFallback::Required,
        }]
    );
}

#[test]
fn historical_systemd_boot_assets_are_not_bootloader_authority() {
    let layouts = [
        ("head", regular(1, "lib/kernel/6.12/vmlinuz")),
        ("history", regular(2, "lib/systemd/boot/efi/systemd-bootx64.efi")),
    ];
    let (projection, _) = build_projection(&[&["head"], &["history"]], &layouts);

    assert!(matches!(
        projection.prepare_asset_plan().unwrap(),
        BootAssetPlanOutcome::NotApplicable(BootAssetPlanNotApplicable::NoSystemdBootAsset)
    ));
}

#[test]
fn two_head_systemd_boot_assets_are_rejected() {
    let mut layouts = vec![
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(2, "lib64/systemd/boot/efi/systemd-bootx64.efi")),
    ];
    layouts.extend((0..=MAX_BOOT_PLAN_KERNELS).map(|index| {
        (
            "head",
            regular(index as u128 + 3, &format!("lib/kernel/version-{index}/vmlinuz")),
        )
    }));
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::AmbiguousSystemdBootAssets { count: 2 }
    ));
}

#[test]
fn unselected_layouts_cannot_enter_or_poison_a_state_plan() {
    let mut layouts = complete_layouts("head");
    layouts.push(("foreign", regular(99, "/absolute/invalid")));
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    assert!(plan.assets().iter().all(|asset| asset.digest() != 99));
}

#[test]
fn identical_multi_package_owners_collapse_but_conflicts_fail() {
    let boot = regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi");
    let kernel = regular(2, "lib/kernel/6.12/vmlinuz");
    let layouts = [
        ("alpha", boot.clone()),
        ("beta", boot.clone()),
        ("alpha", kernel.clone()),
        ("beta", kernel),
    ];
    let (projection, _) = build_projection(&[&["alpha", "beta"]], &layouts);
    let plan = ready(projection.prepare_asset_plan().unwrap());
    assert_eq!(plan.assets().len(), 2);

    let conflicting = [
        ("alpha", boot.clone()),
        ("beta", regular(3, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("alpha", regular(2, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["alpha", "beta"]], &conflicting);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::ConflictingPath { .. }
    ));
}

#[test]
fn the_same_logical_path_may_resolve_differently_in_distinct_states() {
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(2, "lib/kernel/6.12/vmlinuz")),
        ("history", regular(3, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, states) = build_projection(&[&["head"], &["history"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    let kernels = plan
        .assets()
        .iter()
        .filter(|asset| matches!(asset.role(), BootAssetRole::Kernel { .. }))
        .map(|asset| (asset.state_id(), asset.digest()))
        .collect::<Vec<_>>();
    assert_eq!(kernels, [(states[0].id, 2), (states[1].id, 3)]);
}

#[test]
fn final_boot_asset_symlinks_resolve_to_regular_cas_bytes() {
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        (
            "head",
            symlink("/usr/lib/kernel-real/vmlinuz", "lib/kernel/6.12/vmlinuz"),
        ),
        ("head", regular(2, "lib/kernel-real/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    let plan = ready(projection.prepare_asset_plan().unwrap());
    let kernel = plan
        .assets()
        .iter()
        .find(|asset| matches!(asset.role(), BootAssetRole::Kernel { .. }))
        .unwrap();
    assert_eq!(kernel.logical_path(), Path::new("/usr/lib/kernel/6.12/vmlinuz"));
    assert_eq!(kernel.resolved_path(), Path::new("/usr/lib/kernel-real/vmlinuz"));
    assert_eq!(kernel.digest(), 2);
}

#[test]
fn symlink_cycles_and_missing_targets_fail_closed() {
    let cyclic = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink("/usr/lib/kernel/6.12/other", "lib/kernel/6.12/vmlinuz")),
        ("head", symlink("/usr/lib/kernel/6.12/vmlinuz", "lib/kernel/6.12/other")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &cyclic);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::SymlinkCycle { .. }
    ));

    let missing = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        (
            "head",
            symlink("/usr/lib/kernel-real/missing", "lib/kernel/6.12/vmlinuz"),
        ),
    ];
    let (projection, _) = build_projection(&[&["head"]], &missing);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::MissingPath { .. }
    ));
}

#[test]
fn invalid_symlink_targets_and_file_modes_fail_closed() {
    let escaped = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink("/etc/passwd", "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &escaped);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::SymlinkEscape { .. }
    ));

    let relative_escape = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink("../../../../etc/passwd", "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &relative_escape);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::SymlinkEscape { .. }
    ));

    let wrong_mode = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        (
            "head",
            regular_with_mode(2, "lib/kernel/6.12/vmlinuz", nix::libc::S_IFDIR | 0o755),
        ),
    ];
    let (projection, _) = build_projection(&[&["head"]], &wrong_mode);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::InvalidRegularMode { .. }
    ));
}

#[test]
fn ownership_and_unsupported_mode_bits_fail_before_planning() {
    let mut foreign_owner = regular(2, "lib/kernel/6.12/vmlinuz");
    foreign_owner.uid = 1000;
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", foreign_owner),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::UnsupportedOwnership { uid: 1000, .. }
    ));

    let extra_bit = 1 << 20;
    let cases = [
        regular_with_mode(2, "lib/kernel/6.12/vmlinuz", nix::libc::S_IFREG | 0o644 | extra_bit),
        StonePayloadLayoutRecord {
            mode: nix::libc::S_IFDIR | 0o755 | extra_bit,
            ..directory("share/invalid-directory")
        },
        StonePayloadLayoutRecord {
            mode: nix::libc::S_IFLNK | 0o777 | extra_bit,
            ..symlink("target", "share/invalid-symlink")
        },
    ];
    for invalid in cases {
        let layouts = [
            ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
            ("head", regular(2, "lib/kernel/6.12/vmlinuz")),
            ("head", invalid),
        ];
        let (projection, _) = build_projection(&[&["head"]], &layouts);
        assert!(matches!(
            planning_error(projection.prepare_asset_plan()),
            ActiveReblitBootAssetPlanError::InvalidRegularMode { .. }
                | ActiveReblitBootAssetPlanError::InvalidDirectoryMode { .. }
                | ActiveReblitBootAssetPlanError::InvalidSymlinkMode { .. }
        ));
    }
}

#[test]
fn symlink_hop_byte_and_depth_limits_are_exact() {
    fn chain(hops: usize) -> Vec<(&'static str, StonePayloadLayoutRecord)> {
        let mut layouts = vec![
            ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
            ("head", symlink("/usr/lib/boot-chain/0", "lib/kernel/6.12/vmlinuz")),
        ];
        for index in 0..hops.saturating_sub(1) {
            let target = if index + 1 == hops - 1 {
                "/usr/lib/boot-chain/final".to_owned()
            } else {
                format!("/usr/lib/boot-chain/{}", index + 1)
            };
            layouts.push(("head", symlink(&target, &format!("lib/boot-chain/{index}"))));
        }
        layouts.push(("head", regular(2, "lib/boot-chain/final")));
        layouts
    }

    let (projection, _) = build_projection(&[&["head"]], &chain(MAX_BOOT_PLAN_SYMLINK_HOPS));
    assert!(matches!(
        projection.prepare_asset_plan().unwrap(),
        BootAssetPlanOutcome::Ready(_)
    ));

    let (projection, _) = build_projection(&[&["head"]], &chain(MAX_BOOT_PLAN_SYMLINK_HOPS.saturating_add(1)));
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::SymlinkDepthLimit { .. }
    ));

    let oversized_relative = "a".repeat(MAX_BOOT_PLAN_SINGLE_PATH_BYTES - 8);
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink(&oversized_relative, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::ResolvedPathByteLimit { .. }
    ));

    let deep_target = format!(
        "/usr/{}",
        std::iter::repeat_n("a", MAX_BOOT_PLAN_PATH_COMPONENTS)
            .collect::<Vec<_>>()
            .join("/")
    );
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink(&deep_target, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::ResolvedPathDepthLimit { .. }
    ));
}

#[test]
fn descendants_beneath_symlink_or_regular_ancestors_fail_closed() {
    let beneath_symlink = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", symlink("/usr/lib/kernel-real", "lib/kernel")),
        ("head", regular(2, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &beneath_symlink);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::SymlinkAncestor { .. }
    ));

    let beneath_regular = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(2, "lib/kernel")),
        ("head", regular(3, "lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &beneath_regular);
    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::AncestorNotDirectory { .. }
    ));
}

#[test]
fn selected_invalid_stone_target_is_rejected_before_classification() {
    let layouts = [
        ("head", regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi")),
        ("head", regular(2, "/usr/lib/kernel/6.12/vmlinuz")),
    ];
    let (projection, _) = build_projection(&[&["head"]], &layouts);

    assert!(matches!(
        planning_error(projection.prepare_asset_plan()),
        ActiveReblitBootAssetPlanError::InvalidLayout { .. }
    ));
}

#[test]
fn asset_path_kernel_snapshot_and_work_bounds_fail_with_typed_errors() {
    let layouts = complete_layouts("head");
    let (projection, _) = build_projection(&[&["head"]], &layouts);
    let baseline = ready(projection.prepare_asset_plan().unwrap());
    let exact_assets = baseline.assets().len();
    let exact_path_bytes = baseline
        .assets()
        .iter()
        .map(|asset| asset.logical_path().as_os_str().len() + asset.resolved_path().as_os_str().len())
        .sum::<usize>();
    let exact_kernels = baseline.kernel_count();
    let exact_snapshot_digests = baseline.snapshot_digests().len();

    let mut policy = BootAssetPlanPolicy::production();
    policy.max_assets = exact_assets;
    assert!(matches!(
        prepare_asset_plan(&projection, policy).unwrap(),
        BootAssetPlanOutcome::Ready(_)
    ));
    policy.max_assets = exact_assets - 1;
    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::AssetCountLimit { limit, actual }
            if limit == exact_assets - 1 && actual == exact_assets
    ));

    let mut policy = BootAssetPlanPolicy::production();
    policy.max_path_bytes = exact_path_bytes;
    assert!(matches!(
        prepare_asset_plan(&projection, policy).unwrap(),
        BootAssetPlanOutcome::Ready(_)
    ));
    policy.max_path_bytes = exact_path_bytes - 1;
    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::PathByteLimit { limit, actual }
            if limit == exact_path_bytes - 1 && actual == exact_path_bytes
    ));

    let mut policy = BootAssetPlanPolicy::production();
    policy.max_kernels = exact_kernels;
    assert!(matches!(
        prepare_asset_plan(&projection, policy).unwrap(),
        BootAssetPlanOutcome::Ready(_)
    ));
    policy.max_kernels = exact_kernels - 1;
    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::KernelCountLimit { limit, actual }
            if limit == exact_kernels - 1 && actual == exact_kernels
    ));

    let mut policy = BootAssetPlanPolicy::production();
    policy.max_snapshot_digests = exact_snapshot_digests;
    assert!(matches!(
        prepare_asset_plan(&projection, policy).unwrap(),
        BootAssetPlanOutcome::Ready(_)
    ));
    policy.max_snapshot_digests = exact_snapshot_digests - 1;
    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::SnapshotDigestCountLimit { limit, actual }
            if limit == exact_snapshot_digests - 1 && actual == exact_snapshot_digests
    ));

    let mut policy = BootAssetPlanPolicy::production();
    policy.max_work = 0;
    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::WorkLimit { limit: 0, actual: 1 }
    ));
}

#[test]
fn expired_planning_deadline_fails_before_asset_admission() {
    let layouts = complete_layouts("head");
    let (projection, _) = build_projection(&[&["head"]], &layouts);
    let mut policy = BootAssetPlanPolicy::production();
    policy.timeout = Duration::ZERO;

    assert!(matches!(
        planning_error(prepare_asset_plan(&projection, policy)),
        ActiveReblitBootAssetPlanError::DeadlineExceeded {
            timeout: Duration::ZERO
        }
    ));
}
