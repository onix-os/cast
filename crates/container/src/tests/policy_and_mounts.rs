#[test]
fn default_policy_preserves_historical_mounts() {
    let container = Container::new("/");

    assert_eq!(container.pseudo_filesystems, PseudoFilesystemPolicy::default());
    assert_eq!(container.loopback, LoopbackPolicy::HostIpIfAvailable);
    assert_eq!(
        pseudo_mount_decisions(PseudoFilesystemPolicy::default()),
        vec![
            PseudoMountDecision::Proc { read_only: false },
            PseudoMountDecision::Tmp { limits: None },
            PseudoMountDecision::HostSys { read_only: false },
            PseudoMountDecision::HostDev { read_only: false },
        ]
    );
}

#[test]
fn bounded_tmpfs_limits_reject_each_zero_ceiling() {
    assert!(matches!(
        TmpfsLimits::new(0, 1),
        Err(TmpfsLimitsError::ZeroCeiling { field: "size bytes" })
    ));
    assert!(matches!(
        TmpfsLimits::new(1, 0),
        Err(TmpfsLimitsError::ZeroCeiling { field: "inodes" })
    ));
    assert!(matches!(
        TmpfsLimits::new(0, 0),
        Err(TmpfsLimitsError::ZeroCeiling { field: "size bytes" })
    ));
}

#[test]
fn bounded_tmpfs_emits_exact_mount_and_fsconfig_values() {
    for (size_bytes, inodes) in [(4_096, 63), (4_097, 64), (u64::MAX, u64::MAX)] {
        let limits = TmpfsLimits::new(size_bytes, inodes).unwrap();
        assert_eq!(limits.size_bytes(), size_bytes);
        assert_eq!(limits.inodes(), inodes);
        assert_eq!(limits.mount_options(), format!("size={size_bytes},nr_inodes={inodes}"));
        let options = limits.fsconfig_options();
        assert_eq!(options[0].0, c"size");
        assert_eq!(options[0].1.to_bytes(), size_bytes.to_string().as_bytes());
        assert_eq!(options[1].0, c"nr_inodes");
        assert_eq!(options[1].1.to_bytes(), inodes.to_string().as_bytes());
        assert_eq!(
            pseudo_mount_decisions(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Bounded(limits),
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            }),
            vec![PseudoMountDecision::Tmp { limits: Some(limits) }]
        );
    }
}

#[test]
fn bounded_tmpfs_verification_reports_fstatfs_failure() {
    let label = Path::new("/diagnostic/tmp");
    let limits = TmpfsLimits::new(4_096, 8).unwrap();
    let error = verify_tmpfs_limits(-1, label, limits).unwrap_err();

    assert!(matches!(
        error,
        ContainerError::Mount { source, target }
            if source == Errno::EBADF && target == label
    ));
}

#[test]
fn bounded_tmpfs_readback_rejects_wrong_filesystem_magic() {
    let label = Path::new("/diagnostic/tmp");
    let limits = TmpfsLimits::new(4_096, 8).unwrap();
    let error = validate_tmpfs_limit_readback(
        label,
        limits,
        TmpfsLimitReadback {
            filesystem: TMPFS_MAGIC + 1,
            block_size: 4_096,
            blocks: 1,
            inodes: 8,
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ContainerError::UnexpectedTmpfsFilesystem { target, filesystem }
            if target == label && filesystem == TMPFS_MAGIC + 1
    ));
}

#[test]
fn bounded_tmpfs_readback_rejects_representation_and_multiplication_overflow() {
    let label = Path::new("/diagnostic/tmp");
    let limits = TmpfsLimits::new(4_096, 8).unwrap();
    for observed in [
        TmpfsLimitReadback {
            filesystem: TMPFS_MAGIC,
            block_size: -1,
            blocks: 1,
            inodes: 8,
        },
        TmpfsLimitReadback {
            filesystem: TMPFS_MAGIC,
            block_size: nix::libc::c_long::MAX,
            blocks: u64::MAX,
            inodes: 8,
        },
    ] {
        assert!(matches!(
            validate_tmpfs_limit_readback(label, limits, observed),
            Err(ContainerError::InvalidTmpfsLimitReadback { target }) if target == label
        ));
    }
}

#[test]
fn bounded_tmpfs_readback_reports_size_and_inode_normalization_exactly() {
    let label = Path::new("/diagnostic/tmp");
    let limits = TmpfsLimits::new(4_096, 8).unwrap();

    for (observed, expected_error) in [
        (
            TmpfsLimitReadback {
                filesystem: TMPFS_MAGIC,
                block_size: 4_096,
                blocks: 2,
                inodes: 8,
            },
            (8_192, 8),
        ),
        (
            TmpfsLimitReadback {
                filesystem: TMPFS_MAGIC,
                block_size: 4_096,
                blocks: 1,
                inodes: 9,
            },
            (4_096, 9),
        ),
    ] {
        let error = validate_tmpfs_limit_readback(label, limits, observed).unwrap_err();
        assert!(matches!(
            error,
            ContainerError::TmpfsLimitsNormalized {
                target,
                expected_size_bytes: 4_096,
                observed_size_bytes,
                expected_inodes: 8,
                observed_inodes,
            } if target == label
                && observed_size_bytes == expected_error.0
                && observed_inodes == expected_error.1
        ));
    }

    validate_tmpfs_limit_readback(
        label,
        limits,
        TmpfsLimitReadback {
            filesystem: TMPFS_MAGIC,
            block_size: 4_096,
            blocks: 1,
            inodes: 8,
        },
    )
    .unwrap();
}

#[test]
fn pseudo_mount_targets_are_prepared_before_a_root_can_be_sealed() {
    let root = tempfile::tempdir().unwrap();
    let limits = TmpfsLimits::new(4_096, 8).unwrap();
    let decisions = pseudo_mount_decisions(PseudoFilesystemPolicy {
        proc: ProcPolicy::ReadOnly,
        tmp: TmpPolicy::Bounded(limits),
        sys: SysPolicy::HostReadOnly,
        dev: DevPolicy::Minimal,
    });

    prepare_pseudo_mount_targets(root.path(), &decisions).unwrap();

    for target in ["proc", "tmp", "sys", "dev"] {
        assert!(root.path().join(target).is_dir(), "missing prepared /{target}");
    }
}

#[test]
fn atomic_cgroup_execution_never_exposes_writable_host_sysfs() {
    let root = tempfile::tempdir().unwrap();
    let root_anchor = open_path_directory(root.path());
    let mut container = anchored_container(root.path(), &root_anchor);
    let authenticated = authenticate_anchored_inputs(&container).unwrap().unwrap();
    assert!(matches!(
        require_atomic_cgroup_policy(&container, Some(&authenticated)),
        Err(ContainerRunError::UnsafeCgroupSysPolicy)
    ));

    container.pseudo_filesystems.sys = SysPolicy::HostReadOnly;
    require_atomic_cgroup_policy(&container, Some(&authenticated)).unwrap();
    container.pseudo_filesystems.sys = SysPolicy::None;
    require_atomic_cgroup_policy(&container, Some(&authenticated)).unwrap();

    assert!(matches!(
        require_atomic_cgroup_policy(&Container::new(root.path()), None),
        Err(ContainerRunError::AtomicCgroupRequiresAnchoredRoot)
    ));
}

#[test]
fn atomic_cgroup_execution_rejects_direct_cgroup_filesystem_authority() {
    let cgroup_anchor = open_path_directory(Path::new("/sys/fs/cgroup"));
    let mut cgroup_root = anchored_container(Path::new("/sys/fs/cgroup"), &cgroup_anchor);
    cgroup_root.pseudo_filesystems.sys = SysPolicy::None;
    let cgroup_inputs = authenticate_anchored_inputs(&cgroup_root).unwrap().unwrap();
    assert!(matches!(
        require_atomic_cgroup_policy(&cgroup_root, Some(&cgroup_inputs)),
        Err(ContainerRunError::UnsafeCgroupRootFilesystem { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("work")).unwrap();
    let root_anchor = open_path_directory(root.path());
    let writable = anchored_container(root.path(), &root_anchor)
        .bind_rw_pinned(exact_locator(Path::new("/sys/fs/cgroup"), &cgroup_anchor), "/work")
        .unwrap();
    let writable_inputs = authenticate_anchored_inputs(&writable).unwrap().unwrap();
    assert!(matches!(
        require_atomic_cgroup_bind_policy(&writable_inputs.bind_sources),
        Err(ContainerRunError::UnsafeCgroupBindSource { .. })
    ));

    let read_only = anchored_container(root.path(), &root_anchor)
        .bind_ro_pinned(exact_locator(Path::new("/sys/fs/cgroup"), &cgroup_anchor), "/work")
        .unwrap();
    let read_only_inputs = authenticate_anchored_inputs(&read_only).unwrap().unwrap();
    require_atomic_cgroup_bind_policy(&read_only_inputs.bind_sources).unwrap();
}

#[test]
fn disabled_policy_produces_no_mount_decisions() {
    let policy = PseudoFilesystemPolicy {
        proc: ProcPolicy::None,
        tmp: TmpPolicy::Disabled,
        sys: SysPolicy::None,
        dev: DevPolicy::None,
    };

    assert!(pseudo_mount_decisions(policy).is_empty());
}

#[test]
fn policy_maps_to_ordered_mount_decisions() {
    let policy = PseudoFilesystemPolicy {
        proc: ProcPolicy::ReadOnly,
        tmp: TmpPolicy::Disabled,
        sys: SysPolicy::HostReadOnly,
        dev: DevPolicy::Minimal,
    };
    let container = Container::new("/").pseudo_filesystems(policy);

    assert_eq!(container.pseudo_filesystems, policy);
    assert_eq!(
        pseudo_mount_decisions(policy),
        vec![
            PseudoMountDecision::Proc { read_only: true },
            PseudoMountDecision::HostSys { read_only: true },
            PseudoMountDecision::PrivateMinimalDev,
        ]
    );
}

#[test]
fn deterministic_loopback_policy_is_explicit() {
    let container = Container::new("/").loopback(LoopbackPolicy::KernelDefault);

    assert_eq!(container.loopback, LoopbackPolicy::KernelDefault);
}

#[test]
fn read_only_root_reopens_only_explicit_read_write_binds() {
    let default = Container::new("/root");
    assert_eq!(default.root_filesystem, RootFilesystemPolicy::ReadWrite);
    assert!(root_mount_decisions(&default.root, &default.binds, default.root_filesystem).is_empty());

    let restricted = Container::new("/root")
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .bind_rw("/host/work", "/work")
        .bind_ro("/host/input", "/work/input")
        .bind_rw("/host/cache", "/work/cache");

    assert_eq!(
        root_mount_decisions(&restricted.root, &restricted.binds, restricted.root_filesystem),
        vec![
            RootMountDecision::ReadOnlyRecursive("/root".into()),
            RootMountDecision::ReadWriteExact("/root/work".into()),
            RootMountDecision::ReadWriteExact("/root/work/cache".into()),
        ]
    );
}

#[test]
fn empty_payload_capability_state_removes_every_live_capability() {
    let capabilities = [
        CapabilityData {
            effective: u32::MAX,
            permitted: u32::MAX,
            inheritable: u32::MAX,
        },
        CapabilityData {
            effective: u32::MAX,
            permitted: u32::MAX,
            inheritable: u32::MAX,
        },
    ];
    for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
        assert!(capability_is_set(&capabilities, capability));
    }

    let empty = [CapabilityData::default(); 2];
    for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
        assert!(!capability_is_set(&empty, capability));
    }
}

#[test]
fn standard_descriptors_reject_pathname_capabilities() {
    assert!(standard_descriptor_is_unsafe(nix::libc::S_IFDIR, nix::libc::O_RDONLY));
    assert!(standard_descriptor_is_unsafe(nix::libc::S_IFREG, nix::libc::O_PATH));
    assert!(!standard_descriptor_is_unsafe(nix::libc::S_IFREG, nix::libc::O_RDONLY));
    assert!(!standard_descriptor_is_unsafe(nix::libc::S_IFIFO, nix::libc::O_WRONLY));
}

#[test]
fn activation_capability_skip_honors_the_exact_strict_override() {
    assert!(activation_capability_skip_allowed(None));
    assert!(activation_capability_skip_allowed(Some(std::ffi::OsStr::new("0"))));
    assert!(activation_capability_skip_allowed(Some(std::ffi::OsStr::new("true"))));
    assert!(!activation_capability_skip_allowed(Some(std::ffi::OsStr::new("1"))));
}

#[test]
fn special_file_bind_gets_a_file_mountpoint() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("device.fifo");
    mkfifo(&source, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let target = temporary.path().join("mountpoints/device");

    assert!(fs::metadata(&source).unwrap().file_type().is_fifo());
    prepare_bind_target(&source, &target).unwrap();

    let target_metadata = fs::metadata(target).unwrap();
    assert!(target_metadata.is_file());
    assert_eq!(target_metadata.len(), 0);
}
