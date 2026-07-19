use std::{
    os::{
        fd::AsRawFd as _,
        unix::fs::{PermissionsExt, symlink},
    },
    path::Path,
};

use stone_recipe::derivation::ProcFilesystem;

use super::*;
use crate::{BuildPolicy, Recipe, package};

fn create_production_frozen_root(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

fn assert_opath_directory(file: &File) {
    // SAFETY: F_GETFL only inspects the status flags of a live descriptor.
    let flags = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL) };
    assert_ne!(flags, -1);
    assert_eq!(flags & nix::libc::O_PATH, nix::libc::O_PATH);
    assert!(file.metadata().unwrap().is_dir());
}

#[test]
fn current_cgroup_discovers_only_the_explicit_systemd_supervisor_topology() {
    let membership = b"0::/user.slice/user-1000.slice/user@1000.service/app.slice/cast-build.service/cast-supervisor\n";
    assert_eq!(
        delegated_relative_from_current_cgroup(membership).unwrap(),
        Path::new("user.slice/user-1000.slice/user@1000.service/app.slice/cast-build.service")
    );
}

#[test]
fn current_cgroup_requires_exactly_one_canonical_unified_entry() {
    for malformed in [
        b"".as_slice(),
        b"0::/unit/cast-supervisor".as_slice(),
        b"0::/unit/cast-supervisor\0\n".as_slice(),
        b"0::/unit/cast-supervisor\n0::/other/cast-supervisor\n".as_slice(),
        b"1::/unit/cast-supervisor\n".as_slice(),
        b"0:cpu:/unit/cast-supervisor\n".as_slice(),
        b"0::unit/cast-supervisor\n".as_slice(),
        b"0::/unit//cast-supervisor\n".as_slice(),
        b"0::/unit/./cast-supervisor\n".as_slice(),
        b"0::/unit/../cast-supervisor\n".as_slice(),
        b"0::/unit/cast-supervisor\n\n".as_slice(),
    ] {
        assert!(matches!(
            delegated_relative_from_current_cgroup(malformed),
            Err(Error::MalformedCurrentCgroup { .. })
        ));
    }
}

#[test]
fn current_cgroup_never_infers_delegation_from_an_ordinary_parent() {
    for membership in [
        b"0::/\n".as_slice(),
        b"0::/user.slice/session.scope\n".as_slice(),
        b"0::/cast-supervisor\n".as_slice(),
    ] {
        assert!(matches!(
            delegated_relative_from_current_cgroup(membership),
            Err(Error::FrozenCgroupDelegationRequired { .. })
        ));
    }
}

#[test]
fn current_cgroup_path_and_component_budgets_fail_closed() {
    let oversized_component = "a".repeat(MAX_CURRENT_CGROUP_COMPONENT_BYTES + 1);
    let membership = format!("0::/{oversized_component}/cast-supervisor\n");
    assert!(matches!(
        delegated_relative_from_current_cgroup(membership.as_bytes()),
        Err(Error::CurrentCgroupComponentTooLarge {
            limit: MAX_CURRENT_CGROUP_COMPONENT_BYTES,
            ..
        })
    ));

    let exact = std::iter::repeat_n("a", MAX_CURRENT_CGROUP_COMPONENTS - 1)
        .chain(std::iter::once("cast-supervisor"))
        .collect::<Vec<_>>()
        .join("/");
    delegated_relative_from_current_cgroup(format!("0::/{exact}\n").as_bytes()).unwrap();
    let over = format!("a/{exact}");
    assert!(matches!(
        delegated_relative_from_current_cgroup(format!("0::/{over}\n").as_bytes()),
        Err(Error::CurrentCgroupComponentLimit {
            limit: MAX_CURRENT_CGROUP_COMPONENTS,
            ..
        })
    ));

    let long = std::iter::repeat_n("a".repeat(MAX_CURRENT_CGROUP_COMPONENT_BYTES), 17)
        .chain(std::iter::once("cast-supervisor".to_owned()))
        .collect::<Vec<_>>()
        .join("/");
    assert!(matches!(
        delegated_relative_from_current_cgroup(format!("0::/{long}\n").as_bytes()),
        Err(Error::CurrentCgroupPathTooLarge {
            limit: MAX_CURRENT_CGROUP_PATH_BYTES,
            ..
        })
    ));
}

#[test]
fn current_cgroup_reader_stops_at_n_plus_one_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("cgroup");
    std::fs::write(&path, vec![b'a'; MAX_CURRENT_CGROUP_BYTES]).unwrap();
    assert_eq!(
        read_bounded_current_cgroup(&path).unwrap().len(),
        MAX_CURRENT_CGROUP_BYTES
    );

    std::fs::write(&path, vec![b'a'; MAX_CURRENT_CGROUP_BYTES + 1]).unwrap();
    assert!(matches!(
        read_bounded_current_cgroup(&path),
        Err(Error::CurrentCgroupTooLarge {
            limit: MAX_CURRENT_CGROUP_BYTES,
            ..
        })
    ));
}

#[test]
fn frozen_cgroup_policy_is_finite_and_cpu_scales_only_with_explicit_jobs() {
    let one = frozen_cgroup_limits(1).unwrap();
    assert_eq!(one.pids_max(), FROZEN_CGROUP_PIDS_MAX);
    assert_eq!(one.memory_max(), 32 * BYTES_PER_GIB);
    assert_eq!(one.memory_swap_max(), 0);
    assert_eq!(one.cpu_quota_micros(), FROZEN_CGROUP_CPU_PERIOD_MICROS);
    assert_eq!(one.cpu_period_micros(), FROZEN_CGROUP_CPU_PERIOD_MICROS);

    let eight = frozen_cgroup_limits(8).unwrap();
    assert_eq!(eight.pids_max(), one.pids_max());
    assert_eq!(eight.memory_max(), one.memory_max());
    assert_eq!(eight.memory_swap_max(), one.memory_swap_max());
    assert_eq!(eight.cpu_quota_micros(), 8 * FROZEN_CGROUP_CPU_PERIOD_MICROS);
    assert_eq!(eight.cpu_period_micros(), one.cpu_period_micros());

    assert!(matches!(frozen_cgroup_limits(0), Err(Error::InvalidFrozenCgroupJobs)));
    assert!(matches!(
        frozen_cgroup_limits(u32::MAX),
        Err(Error::InvalidFrozenCgroupLimits(_))
    ));
}

#[test]
fn cgroup_leaf_identity_is_validated_before_activation() {
    let plan = package::test_derivation_plan();
    require_derivation_cgroup_identity(plan.derivation_id().as_str()).unwrap();
    for invalid in [
        "",
        "0",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg",
        "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        assert!(matches!(
            require_derivation_cgroup_identity(invalid),
            Err(Error::InvalidDerivationCgroupIdentity)
        ));
    }
}

#[test]
fn frozen_mount_targets_are_created_beneath_one_owned_root_before_verification() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    create_production_frozen_root(&root);
    let targets = [
        PathBuf::from("/mason/artefacts"),
        PathBuf::from("/mason/build"),
        PathBuf::from("/mason/install"),
        PathBuf::from("/tmp"),
        PathBuf::from("/dev"),
    ];

    prepare_mount_targets_at(&root, &targets, Path::new("/mason/install")).unwrap();

    for target in &targets {
        let target = root.join(target.strip_prefix("/").unwrap());
        assert!(target.is_dir());
        assert_eq!(std::fs::metadata(target).unwrap().permissions().mode() & 0o7777, 0o700);
    }
    assert_eq!(
        std::fs::metadata(root.join("mason/install"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
}

#[test]
fn frozen_mount_target_creation_rejects_symlink_components_without_touching_the_target() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    create_production_frozen_root(&root);
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("mason")).unwrap();

    let error =
        prepare_mount_targets_at(&root, &[PathBuf::from("/mason/build")], Path::new("/mason/install")).unwrap_err();
    assert!(matches!(error, Error::PrepareFrozenMountTarget { .. }));
    assert!(std::fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn retained_root_descriptor_never_creates_targets_in_a_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let published = temporary.path().join("published");
    create_production_frozen_root(&published);
    let root = open_mount_directory(&published).unwrap();

    let retained = temporary.path().join("retained");
    std::fs::rename(&published, &retained).unwrap();
    create_production_frozen_root(&published);
    prepare_mount_targets_in(&root, &[PathBuf::from("/mason/build")], Path::new("/mason/install")).unwrap();

    assert!(retained.join("mason/build").is_dir());
    assert!(!published.join("mason").exists());
}

#[test]
fn frozen_mount_targets_reject_hidden_content_and_overlapping_topology() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    create_production_frozen_root(&root);
    std::fs::create_dir(root.join("tmp")).unwrap();
    std::fs::write(root.join("tmp/hidden"), b"must not be hidden by tmpfs").unwrap();

    assert!(matches!(
        prepare_mount_targets_at(&root, &[PathBuf::from("/tmp")], Path::new("/install")),
        Err(Error::FrozenMountTargetNotEmpty(path)) if path == Path::new("/tmp")
    ));
    assert!(matches!(
        canonical_mount_targets(&[PathBuf::from("/mason"), PathBuf::from("/mason/build")]),
        Err(Error::OverlappingFrozenMountTargets { .. })
    ));
}

#[test]
fn frozen_mount_target_preparation_rejects_a_shared_writable_root() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, Permissions::from_mode(0o777)).unwrap();

    assert!(matches!(
        prepare_mount_targets_at(&root, &[PathBuf::from("/build")], Path::new("/install")),
        Err(Error::UnsafeFrozenMountRoot(path)) if path == root
    ));
}

#[test]
fn descriptor_child_open_rejects_mount_crossings() {
    let root = open_mount_directory(Path::new("/")).unwrap();
    let proc = CString::new("proc").unwrap();
    let error = open_mount_child(&root, &proc).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(nix::libc::EXDEV));
}

fn non_default_layout() -> BuilderLayout {
    let mut policy = BuildPolicy::repository_for_tests();
    policy.spec.sandbox.hostname = "forge-builder".to_owned();
    policy.spec.sandbox.guest_root = "/forge".to_owned();
    policy.spec.sandbox.artifacts_dir = "/forge/output".to_owned();
    policy.spec.sandbox.build_dir = "/forge/work".to_owned();
    policy.spec.sandbox.source_dir = "/forge/sources".to_owned();
    policy.spec.sandbox.recipe_dir = "/forge/recipe".to_owned();
    policy.spec.sandbox.package_dir = "/forge/recipe/package".to_owned();
    policy.spec.sandbox.install_dir = "/forge/destination".to_owned();
    {
        let cache = &mut policy.spec.build_root.compiler_cache;
        cache.ccache_dir = "/forge/cache-cc".to_owned();
        cache.sccache_dir = "/forge/cache-rust".to_owned();
        cache.go_cache_dir = "/forge/cache-go".to_owned();
        cache.go_mod_cache_dir = "/forge/cache-go-mod".to_owned();
        cache.cargo_cache_dir = "/forge/cache-cargo".to_owned();
        cache.zig_cache_dir = "/forge/cache-zig".to_owned();
    }
    policy.spec.validate().unwrap();
    BuilderLayout::from_policy(&policy.spec.sandbox, &policy.spec.build_root.compiler_cache)
}

#[test]
fn frozen_filesystems_override_legacy_container_mounts() {
    let frozen = FilesystemPolicy {
        proc: ProcFilesystem::None,
        tmp: TmpFilesystem::Empty,
        sys: SysFilesystem::None,
        dev: DevFilesystem::None,
    };

    let mapped = frozen_pseudo_filesystems(frozen);
    assert_eq!(mapped.proc, ProcPolicy::None);
    assert_eq!(mapped.tmp, TmpPolicy::Bounded(FROZEN_TMPFS_LIMITS));
    assert_eq!(FROZEN_TMPFS_LIMITS.size_bytes(), 16 * BYTES_PER_GIB);
    assert_eq!(FROZEN_TMPFS_LIMITS.inodes(), 1_048_576);
    assert_eq!(mapped.sys, SysPolicy::None);
    assert_eq!(mapped.dev, DevPolicy::None);
    assert_ne!(mapped, PseudoFilesystemPolicy::default());
    assert_eq!(frozen_loopback_policy(), LoopbackPolicy::KernelDefault);
}

#[test]
fn frozen_minimal_dev_is_exact_and_sys_is_absent() {
    let mapped = frozen_pseudo_filesystems(FilesystemPolicy::default());

    assert_eq!(mapped.proc, ProcPolicy::None);
    assert_eq!(mapped.tmp, TmpPolicy::Bounded(FROZEN_TMPFS_LIMITS));
    assert_eq!(mapped.sys, SysPolicy::None);
    assert_eq!(mapped.dev, DevPolicy::Minimal);
    assert_eq!(::container::MINIMAL_DEV_NODES, ["null", "zero", "full"]);
}

#[test]
fn frozen_container_excludes_recipe_and_disabled_global_caches() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let plan = package::test_derivation_plan();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();

    let disabled = prepare_frozen_sandbox(&paths, &plan).unwrap().mounts;
    assert_eq!(disabled.len(), 3);
    assert_eq!(
        disabled.iter().map(|mount| mount.guest.as_path()).collect::<Vec<_>>(),
        [
            Path::new(&plan.layout.artifacts_dir),
            Path::new(&plan.layout.build_dir),
            Path::new(&plan.layout.install_dir),
        ]
    );
    assert_eq!(disabled[2].host, paths.install().host);
    assert!(!disabled.iter().any(|mount| mount.host == paths.recipe().host));

    let enabled_runtime = crate::private_tempdir();
    let mut enabled_plan = plan.clone();
    package::set_test_compiler_cache(&mut enabled_plan, true);
    enabled_plan.validate().unwrap();
    let mut enabled_paths = Paths::new(
        &recipe,
        enabled_plan.layout.clone(),
        enabled_runtime.path(),
        output.path(),
    )
    .unwrap();
    enabled_paths.bind_to_plan(&enabled_plan).unwrap();
    let enabled = prepare_frozen_sandbox(&enabled_paths, &enabled_plan).unwrap().mounts;
    assert_eq!(enabled.len(), 9);
    assert!(enabled.iter().skip(3).all(|mount| {
        mount.host.starts_with(
            enabled_runtime
                .path()
                .join("derivations")
                .join(enabled_plan.derivation_id().as_str()),
        )
    }));
    assert!(!enabled.iter().any(|mount| mount.host == enabled_paths.recipe().host));
}

#[test]
fn frozen_sandbox_retains_parallel_opath_identity_witnesses() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let plan = package::test_derivation_plan();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();

    let sandbox = prepare_frozen_sandbox(&paths, &plan).unwrap();
    assert_opath_directory(&sandbox.workspace.path_anchor);
    assert_eq!(
        DirectoryWitness::for_file(&sandbox.workspace.path_anchor).unwrap(),
        sandbox.workspace.witness
    );
    assert_eq!(
        DirectoryWitness::for_file(&sandbox.workspace.file).unwrap(),
        sandbox.workspace.witness
    );

    let mut pinned = 0;
    for mount in &sandbox.mounts {
        let FrozenMountSource::Pinned(source) = &mount.source else {
            continue;
        };
        pinned += 1;
        assert_opath_directory(&source.path_anchor);
        assert_eq!(DirectoryWitness::for_file(&source.path_anchor).unwrap(), source.witness);
        assert_eq!(DirectoryWitness::for_file(&source.file).unwrap(), source.witness);
    }
    assert_eq!(pinned, 2);
}

#[test]
fn frozen_bind_locator_rejects_replacement_without_touching_either_directory() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let plan = package::test_derivation_plan();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();
    let sandbox = prepare_frozen_sandbox(&paths, &plan).unwrap();

    let mount = sandbox
        .mounts
        .iter()
        .find(|mount| mount.role == FrozenMountRole::Build)
        .unwrap();
    let FrozenMountSource::Pinned(source) = &mount.source else {
        panic!("the frozen build mount must have an external pinned source");
    };
    sandbox.pinned_source_locator(source, &mount.host).unwrap();
    std::fs::write(mount.host.join("original"), b"keep original").unwrap();
    let displaced = mount.host.with_file_name("displaced-build");
    std::fs::rename(&mount.host, &displaced).unwrap();
    std::fs::create_dir(&mount.host).unwrap();
    std::fs::set_permissions(&mount.host, Permissions::from_mode(0o700)).unwrap();
    std::fs::write(mount.host.join("replacement"), b"keep replacement").unwrap();

    assert!(matches!(
        sandbox.pinned_source_locator(source, &mount.host),
        Err(Error::FrozenBindSourceLocator { .. })
    ));
    assert!(matches!(
        sandbox.revalidate(),
        Err(Error::FrozenBindSourceReplaced(path)) if path == mount.host
    ));
    assert_eq!(std::fs::read(displaced.join("original")).unwrap(), b"keep original");
    assert_eq!(
        std::fs::read(mount.host.join("replacement")).unwrap(),
        b"keep replacement"
    );
}

#[test]
fn frozen_root_locator_rejects_replacement_without_touching_either_directory() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let plan = package::test_derivation_plan();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();
    let sandbox = prepare_frozen_sandbox(&paths, &plan).unwrap();
    let root = paths.rootfs().host;
    paths.prepare_private_host_directory(&root).unwrap();
    let relative = workspace_relative(&sandbox.workspace.path, &root).unwrap();
    let root_anchor = anchored_locators::open_workspace_path_anchor(&sandbox.workspace.path_anchor, &relative).unwrap();
    sandbox.root_locator(&root, &root_anchor).unwrap();

    std::fs::write(root.join("original"), b"keep original").unwrap();
    let displaced = root.with_file_name("displaced-root");
    std::fs::rename(&root, &displaced).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.join("replacement"), b"keep replacement").unwrap();

    assert!(matches!(
        sandbox.root_locator(&root, &root_anchor),
        Err(Error::FrozenRootLocator(_))
    ));
    assert_eq!(std::fs::read(displaced.join("original")).unwrap(), b"keep original");
    assert_eq!(std::fs::read(root.join("replacement")).unwrap(), b"keep replacement");
}

#[test]
fn frozen_container_uses_non_default_policy_layout_as_one_authority() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let default_plan = package::test_derivation_plan();
    let default_id = default_plan.derivation_id();
    let mut plan = default_plan;
    plan.layout = non_default_layout();
    package::set_test_compiler_cache(&mut plan, true);
    plan.validate().unwrap();
    let derivation_id = plan.derivation_id();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();

    assert_ne!(default_id, derivation_id);
    assert_eq!(paths.install().guest, Path::new("/forge/destination"));
    assert_eq!(
        paths.install().host,
        paths.rootfs().host.join("forge").join("destination")
    );

    let sandbox = prepare_frozen_sandbox(&paths, &plan).unwrap();
    assert_eq!(sandbox.hostname, "forge-builder");
    assert_eq!(sandbox.work_dir, Path::new("/forge/work"));
    assert_eq!(sandbox.root_filesystem, RootFilesystemPolicy::ReadOnly);
    assert_eq!(
        sandbox
            .mounts
            .iter()
            .map(|mount| mount.guest.as_path())
            .collect::<Vec<_>>(),
        [
            Path::new("/forge/output"),
            Path::new("/forge/work"),
            Path::new("/forge/destination"),
            Path::new("/forge/cache-cc"),
            Path::new("/forge/cache-rust"),
            Path::new("/forge/cache-go"),
            Path::new("/forge/cache-go-mod"),
            Path::new("/forge/cache-cargo"),
            Path::new("/forge/cache-zig"),
        ]
    );
    assert!(sandbox.mounts.iter().skip(3).all(|mount| {
        mount
            .host
            .starts_with(runtime.path().join("derivations").join(derivation_id.as_str()))
    }));
}

#[test]
fn frozen_container_rejects_runtime_and_plan_layout_mismatch() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let mut plan = package::test_derivation_plan();
    let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    plan.layout.hostname = "different-builder".to_owned();
    plan.validate().unwrap();

    assert!(matches!(
        prepare_frozen_sandbox(&paths, &plan),
        Err(Error::FrozenLayoutMismatch)
    ));
}

#[test]
fn frozen_container_rejects_non_isolated_credentials() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let mut plan = package::test_derivation_plan();
    let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    plan.execution.credentials = ExecutionCredentials::Unspecified;

    assert!(matches!(
        prepare_frozen_sandbox(&paths, &plan),
        Err(Error::FrozenCredentialPolicyMismatch { found: "unspecified" })
    ));
}
