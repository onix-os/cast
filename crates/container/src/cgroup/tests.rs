use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use tempfile::TempDir;

use super::*;

const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn limits() -> CgroupLimits {
    CgroupLimits::new(128, 1_073_741_824, 0, 200_000, 100_000).unwrap()
}

fn simulated_root(temporary: &TempDir) -> DelegatedCgroupRoot {
    let directory = File::open(temporary.path()).unwrap();
    DelegatedCgroupRoot::simulated(&directory, temporary.path().to_owned())
}

fn create_simulated_controls(path: &Path) {
    for (name, value) in [
        ("pids.max", "max"),
        ("memory.max", "max"),
        ("memory.swap.max", "max"),
        ("memory.oom.group", "0"),
        ("cgroup.max.depth", "max"),
        ("cgroup.max.descendants", "max"),
        ("cpu.max.burst", "0"),
        ("cpu.max", "max 100000"),
        ("cgroup.procs", ""),
        ("cgroup.threads", ""),
        ("cgroup.events", "populated 0\nfrozen 0\n"),
        ("cgroup.kill", ""),
    ] {
        let control = path.join(name);
        fs::write(&control, value).unwrap();
        // The live cgroup2 pseudo-files are never group- or
        // world-writable. Reproduce that authority boundary regardless
        // of the developer's ambient umask.
        fs::set_permissions(control, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn deactivate(leaf: &mut CgroupLeaf) {
    leaf.active = false;
}

#[test]
fn leaf_identity_is_exactly_lowercase_hex() {
    validate_leaf_identity(ID).unwrap();
    for invalid in [
        "",
        "0",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
        "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
        "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcde/0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        assert!(matches!(
            validate_leaf_identity(invalid),
            Err(CgroupError::InvalidLeafIdentity { .. })
        ));
    }
}

#[test]
fn zero_limits_and_zero_drain_intervals_are_rejected() {
    assert_eq!(limits().memory_swap_max(), 0);
    let page_size = system_page_size().unwrap();
    for arguments in [
        (0, page_size, 0, 1_000, 1_000),
        (1, 0, 0, 1_000, 1_000),
        (1, page_size, 0, 0, 1_000),
        (1, page_size, 0, 1_000, 0),
    ] {
        assert!(matches!(
            CgroupLimits::new(arguments.0, arguments.1, arguments.2, arguments.3, arguments.4),
            Err(CgroupError::ZeroLimit { .. })
        ));
    }
    assert!(DrainPolicy::new(Duration::ZERO, Duration::from_millis(1)).is_err());
    assert!(DrainPolicy::new(Duration::from_millis(1), Duration::ZERO).is_err());
}

#[test]
fn pid_limit_matches_the_supported_kernel_abi_boundary() {
    let page_size = system_page_size().unwrap();
    CgroupLimits::new(MAX_PIDS, page_size, 0, 1_000, 1_000).unwrap();
    assert!(matches!(
        CgroupLimits::new(MAX_PIDS + 1, page_size, 0, 1_000, 1_000),
        Err(CgroupError::InvalidPidsMax {
            value,
            maximum: MAX_PIDS,
        }) if value == MAX_PIDS + 1
    ));
}

#[test]
fn memory_limits_must_be_page_aligned_before_any_kernel_write() {
    let page_size = system_page_size().unwrap();
    CgroupLimits::new(1, page_size, 0, 1_000, 1_000).unwrap();
    CgroupLimits::new(1, page_size, page_size, 1_000, 1_000).unwrap();
    for (memory_max, memory_swap_max, field) in [
        (page_size + 1, 0, "memory.max"),
        (page_size, page_size + 1, "memory.swap.max"),
    ] {
        assert!(matches!(
            CgroupLimits::new(1, memory_max, memory_swap_max, 1_000, 1_000),
            Err(CgroupError::UnalignedMemoryLimit { field: found, .. }) if found == field
        ));
    }
}

#[test]
fn cpu_bandwidth_limits_match_the_kernel_abi_at_every_boundary() {
    let page_size = system_page_size().unwrap();
    for (quota, period) in [
        (MIN_CPU_BANDWIDTH_MICROS, MIN_CPU_BANDWIDTH_MICROS),
        (MAX_CPU_QUOTA_MICROS, MAX_CPU_PERIOD_MICROS),
        (2 * MAX_CPU_PERIOD_MICROS, MAX_CPU_PERIOD_MICROS),
    ] {
        CgroupLimits::new(1, page_size, 0, quota, period).unwrap();
    }

    for quota in [MIN_CPU_BANDWIDTH_MICROS - 1, MAX_CPU_QUOTA_MICROS + 1] {
        assert!(matches!(
            CgroupLimits::new(1, page_size, 0, quota, MIN_CPU_BANDWIDTH_MICROS),
            Err(CgroupError::InvalidCpuQuota { value, .. }) if value == quota
        ));
    }
    for period in [MIN_CPU_BANDWIDTH_MICROS - 1, MAX_CPU_PERIOD_MICROS + 1] {
        assert!(matches!(
            CgroupLimits::new(1, page_size, 0, MIN_CPU_BANDWIDTH_MICROS, period),
            Err(CgroupError::InvalidCpuPeriod { value, .. }) if value == period
        ));
    }
}

#[test]
fn generated_leaf_names_retain_identity_and_add_128_bits_of_entropy() {
    let first = random_leaf_name(ID).unwrap();
    let second = random_leaf_name(ID).unwrap();
    let prefix = format!("{LEAF_NAME_PREFIX}{ID}-");
    for name in [&first, &second] {
        let name = name.to_str().unwrap();
        let suffix = name.strip_prefix(&prefix).unwrap();
        assert_eq!(suffix.len(), LEAF_RANDOM_BYTES * 2);
        assert!(
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
    assert_ne!(first, second);
}

#[test]
fn ordinary_directory_locking_rejects_shared_write_and_a_second_supervisor() {
    let temporary = tempfile::tempdir().unwrap();
    let label = temporary.path();
    fs::set_permissions(label, fs::Permissions::from_mode(0o770)).unwrap();
    let shared: OwnedFd = File::open(label).unwrap().into();
    assert!(matches!(
        acquire_exclusive_delegation(&shared, label),
        Err(CgroupError::DelegationSharedWritable { .. })
    ));

    fs::set_permissions(label, fs::Permissions::from_mode(0o700)).unwrap();
    let first: OwnedFd = File::open(label).unwrap().into();
    acquire_exclusive_delegation(&first, label).unwrap();
    let second: OwnedFd = File::open(label).unwrap().into();
    assert!(matches!(
        acquire_exclusive_delegation(&second, label),
        Err(CgroupError::DelegationAlreadyOwned { .. })
    ));
}

#[test]
fn writable_authority_files_are_authenticated_independently_of_the_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let directory: OwnedFd = File::open(temporary.path()).unwrap().into();
    let control = temporary.path().join("cgroup.threads");
    fs::write(&control, "").unwrap();
    fs::set_permissions(&control, fs::Permissions::from_mode(0o600)).unwrap();
    drop(open_owned_writable_control(&directory, c"cgroup.threads", temporary.path()).unwrap());

    fs::set_permissions(&control, fs::Permissions::from_mode(0o620)).unwrap();
    assert!(matches!(
        open_owned_writable_control(&directory, c"cgroup.threads", temporary.path()),
        Err(CgroupError::DelegationSharedWritable { path, mode })
            if path.ends_with("cgroup.threads") && mode == 0o620
    ));
}

#[test]
fn temporary_directories_are_rejected_as_live_cgroup_filesystems() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("delegated")).unwrap();
    let result = DelegatedCgroupRoot::open(temporary.path(), Path::new("delegated"));
    assert!(matches!(result, Err(CgroupError::NotCgroupV2 { .. })));
}

#[test]
fn event_parser_accepts_unknown_counters_but_rejects_malformed_core_state() {
    let path = Path::new("cgroup.events");
    assert_eq!(
        parse_events(b"populated 1\nfrozen 0\nfuture_counter 42\n", path).unwrap(),
        CgroupEvents {
            populated: true,
            frozen: false
        }
    );
    for malformed in [
        b"frozen 0\n".as_slice(),
        b"populated 0\n".as_slice(),
        b"populated 2\nfrozen 0\n".as_slice(),
        b"populated 0\npopulated 0\nfrozen 0\n".as_slice(),
        b"populated nope\nfrozen 0\n".as_slice(),
        b"populated 0 extra\nfrozen 0\n".as_slice(),
        b"populated 0\nfrozen 0\ninvalid-key 1\n".as_slice(),
    ] {
        assert!(matches!(
            parse_events(malformed, path),
            Err(CgroupError::MalformedControl { .. })
        ));
    }
    assert!(matches!(
        require_empty_unfrozen_delegation(
            CgroupEvents {
                populated: false,
                frozen: true,
            },
            path,
        ),
        Err(CgroupError::DelegationFrozen { .. })
    ));
    assert!(matches!(
        require_empty_unfrozen_delegation(
            CgroupEvents {
                populated: true,
                frozen: false,
            },
            path,
        ),
        Err(CgroupError::DelegationSubtreePopulated { .. })
    ));
    require_empty_unfrozen_delegation(
        CgroupEvents {
            populated: false,
            frozen: false,
        },
        path,
    )
    .unwrap();
}

#[test]
fn delegated_root_requires_zero_visible_and_dying_descendants() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let path = temporary.path().join("cgroup.stat");

    fs::write(&path, "nr_descendants 0\nnr_dying_descendants 0\nnr_subsys_cpu 1\n").unwrap();
    assert_eq!(
        read_descendant_counts(&root.authority.directory, root.label()).unwrap(),
        (0, 0)
    );

    fs::write(&path, "nr_descendants 1\nnr_dying_descendants 2\n").unwrap();
    assert_eq!(
        read_descendant_counts(&root.authority.directory, root.label()).unwrap(),
        (1, 2)
    );

    for malformed in [
        "nr_descendants 0\n",
        "nr_dying_descendants 0\n",
        "nr_descendants 0\nnr_descendants 0\nnr_dying_descendants 0\n",
        "nr_descendants max\nnr_dying_descendants 0\n",
    ] {
        fs::write(&path, malformed).unwrap();
        assert!(matches!(
            read_descendant_counts(&root.authority.directory, root.label()),
            Err(CgroupError::MalformedControl { .. })
        ));
    }
}

#[test]
fn pid_parser_enforces_positive_i32_membership_values() {
    let path = Path::new("cgroup.procs");
    assert_eq!(parse_pid_list(b"1\n42\n42\n", path).unwrap(), [1, 42, 42]);
    for malformed in [
        b"0\n".as_slice(),
        b"-1\n".as_slice(),
        b"2147483648\n".as_slice(),
        b"pid\n".as_slice(),
    ] {
        assert!(matches!(
            parse_pid_list(malformed, path),
            Err(CgroupError::MalformedControl { .. })
        ));
    }
}

#[test]
fn supervisor_membership_is_the_unique_current_tgid_set() {
    let path = Path::new("cast-supervisor/cgroup.procs");
    require_exact_supervisor_membership(&[42], 42, path).unwrap();
    require_exact_supervisor_membership(&[42, 42, 42], 42, path).unwrap();

    for members in [&[][..], &[7][..], &[42, 7][..], &[7, 7][..]] {
        assert!(matches!(
            require_exact_supervisor_membership(members, 42, path),
            Err(CgroupError::SupervisorMembership { .. })
        ));
    }
}

#[test]
fn activated_leaf_membership_is_exactly_the_blocked_clone_child() {
    let path = Path::new("cast-derivation/cgroup.procs");
    require_exact_leaf_membership(&[77], 77, path).unwrap();
    require_exact_leaf_membership(&[77, 77], 77, path).unwrap();

    for (members, expected) in [(&[][..], 77), (&[77][..], 0), (&[12][..], 77), (&[77, 12][..], 77)] {
        assert!(matches!(
            require_exact_leaf_membership(members, expected, path),
            Err(CgroupError::LeafMembership { .. })
        ));
    }
}

#[test]
fn exact_topology_admits_n_and_rejects_n_plus_one_or_dying_state() {
    let path = Path::new("cgroup.stat");
    validate_descendant_topology(1, 0, 1, false, path).unwrap();
    validate_descendant_topology(2, 0, 2, false, path).unwrap();
    validate_descendant_topology(1, 7, 1, true, path).unwrap();

    for (descendants, dying, expected) in [(2, 0, 1), (3, 0, 2), (1, 1, 1), (2, 1, 2)] {
        assert!(matches!(
            validate_descendant_topology(descendants, dying, expected, false, path),
            Err(CgroupError::DelegationTopology {
                expected_descendants,
                descendants: found,
                dying_descendants,
                ..
            }) if expected_descendants == expected && found == descendants && dying_descendants == dying
        ));
    }
}

#[test]
fn required_controller_sets_accept_exact_n_and_n_plus_one() {
    let exact = ["cpu", "memory", "pids"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let plus_one = ["cpu", "io", "memory", "pids"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let missing_one = ["cpu", "pids"].into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
    let path = Path::new("cgroup.controllers");

    require_controllers(&exact, path).unwrap();
    require_controllers(&plus_one, path).unwrap();
    assert_eq!(controller_enable_request(&exact), None);
    assert_eq!(controller_enable_request(&plus_one), None);
    assert!(matches!(
        require_controllers(&missing_one, path),
        Err(CgroupError::MissingControllers { missing, .. }) if missing == "memory"
    ));
}

#[test]
fn controller_enable_requests_are_canonical_and_only_name_missing_requirements() {
    for (enabled, expected) in [
        (&[][..], Some("+cpu +memory +pids")),
        (&["cpu"][..], Some("+memory +pids")),
        (&["memory", "pids"][..], Some("+cpu")),
        (&["cpu", "memory", "pids"][..], None),
        (&["io"][..], Some("+cpu +memory +pids")),
    ] {
        let enabled = enabled.iter().copied().map(str::to_owned).collect::<BTreeSet<_>>();
        assert_eq!(controller_enable_request(&enabled).as_deref(), expected);
    }
}

#[test]
fn already_enabled_controller_set_performs_no_write_and_is_still_verified() {
    let enabled = ["cpu", "memory", "pids"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut writes = 0;
    let mut readbacks = 0;

    enable_required_controllers_with(
        &enabled,
        Path::new("cgroup.subtree_control"),
        &mut |_| {
            writes += 1;
            Ok(())
        },
        &mut || {
            readbacks += 1;
            Ok(enabled.clone())
        },
    )
    .unwrap();

    assert_eq!(writes, 0);
    assert_eq!(readbacks, 1);
}

#[test]
fn controller_enablement_fails_closed_on_short_write_or_readback_mismatch() {
    let path = Path::new("cgroup.subtree_control");
    let request = b"+cpu +memory +pids";
    assert!(matches!(
        write_exact_control_value(path, request, &mut |_| Ok(request.len() - 1)),
        Err(CgroupError::ShortControlWrite {
            expected,
            written,
            ..
        }) if expected == request.len() && written == request.len() - 1
    ));

    let enabled = BTreeSet::new();
    let mut written = Vec::new();
    let readback = ["cpu", "memory"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let result = enable_required_controllers_with(
        &enabled,
        path,
        &mut |value| {
            written.extend_from_slice(value);
            Ok(())
        },
        &mut || Ok(readback.clone()),
    );
    assert_eq!(written, request);
    assert!(matches!(
        result,
        Err(CgroupError::ControlVerification {
            expected,
            found,
            ..
        }) if expected == "cpu memory pids" && found == "cpu memory"
    ));
}

#[test]
fn supervisor_and_leaf_event_polarities_are_both_fail_closed() {
    let path = Path::new("cgroup.events");
    require_populated_unfrozen_delegation(
        CgroupEvents {
            populated: true,
            frozen: false,
        },
        path,
    )
    .unwrap();
    assert!(matches!(
        require_populated_unfrozen_delegation(
            CgroupEvents {
                populated: false,
                frozen: false,
            },
            path,
        ),
        Err(CgroupError::DelegationSubtreeUnpopulated { .. })
    ));
    assert!(matches!(
        require_populated_unfrozen_delegation(
            CgroupEvents {
                populated: true,
                frozen: true,
            },
            path,
        ),
        Err(CgroupError::DelegationFrozen { .. })
    ));
}

#[test]
fn bounded_control_reader_accepts_exact_n_and_rejects_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let exact = vec![b'a'; CONTROL_READ_LIMIT_BYTES];
    fs::write(temporary.path().join("exact"), &exact).unwrap();
    assert_eq!(
        read_control(&root.authority.directory, c"exact", root.label()).unwrap(),
        exact
    );

    fs::write(temporary.path().join("over"), vec![b'a'; CONTROL_READ_LIMIT_BYTES + 1]).unwrap();
    assert!(matches!(
        read_control(&root.authority.directory, c"over", root.label()),
        Err(CgroupError::ControlTooLarge {
            limit: CONTROL_READ_LIMIT_BYTES,
            ..
        })
    ));
}

#[test]
fn simulated_controls_receive_exact_canonical_writes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    fs::write(leaf.label().join("cpu.max.burst"), "40000").unwrap();
    leaf.configure(limits()).unwrap();

    for (name, expected) in [
        ("pids.max", "128"),
        ("memory.max", "1073741824"),
        ("memory.swap.max", "0"),
        ("memory.oom.group", "1"),
        ("cgroup.max.depth", "0"),
        ("cgroup.max.descendants", "0"),
        ("cpu.max.burst", "0"),
        ("cpu.max", "200000 100000"),
    ] {
        assert_eq!(fs::read_to_string(leaf.label().join(name)).unwrap(), expected);
    }
    deactivate(&mut leaf);
}

#[test]
fn configured_leaf_requires_placement_and_terminal_domain_controls() {
    for missing in [
        "cgroup.procs",
        "cgroup.threads",
        "cgroup.max.depth",
        "cgroup.max.descendants",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = simulated_root(&temporary);
        let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
        create_simulated_controls(leaf.label());
        fs::remove_file(leaf.label().join(missing)).unwrap();

        assert!(matches!(
            leaf.configure(limits()),
            Err(CgroupError::DescriptorOperation {
                operation: "open cgroup control",
                ..
            })
        ));
        deactivate(&mut leaf);
    }
}

#[test]
fn missing_cpu_max_burst_preserves_zero_burst_compatibility() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    fs::remove_file(leaf.label().join("cpu.max.burst")).unwrap();

    leaf.configure(limits()).unwrap();

    assert!(!leaf.label().join("cpu.max.burst").exists());
    assert_eq!(
        fs::read_to_string(leaf.label().join("cpu.max")).unwrap(),
        "200000 100000"
    );
    deactivate(&mut leaf);
}

#[test]
fn non_file_or_symlink_cpu_max_burst_is_not_treated_as_absent() {
    for replacement in ["directory", "symlink"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = simulated_root(&temporary);
        let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
        create_simulated_controls(leaf.label());
        let burst = leaf.label().join("cpu.max.burst");
        fs::remove_file(&burst).unwrap();
        match replacement {
            "directory" => fs::create_dir(&burst).unwrap(),
            "symlink" => symlink("cpu.max", &burst).unwrap(),
            _ => unreachable!(),
        }

        let error = leaf.configure(limits()).unwrap_err();
        match error {
            CgroupError::DescriptorOperation {
                operation,
                path,
                source,
            } => {
                assert_eq!(operation, "open cgroup control");
                assert_eq!(path, burst);
                assert_ne!(source.raw_os_error(), Some(libc::ENOENT));
            }
            error => panic!("{replacement} cpu.max.burst returned unexpected error: {error}"),
        }
        deactivate(&mut leaf);
    }
}

#[test]
fn cgroup_kill_remains_mandatory_when_cpu_max_burst_is_absent() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    fs::remove_file(leaf.label().join("cpu.max.burst")).unwrap();
    let kill = leaf.label().join("cgroup.kill");
    fs::remove_file(&kill).unwrap();

    assert!(matches!(
        leaf.configure(limits()),
        Err(CgroupError::DescriptorOperation {
            operation: "open cgroup control",
            path,
            source,
        }) if path == kill && source.raw_os_error() == Some(libc::ENOENT)
    ));
    deactivate(&mut leaf);
}

#[test]
fn present_cpu_max_burst_requires_exact_zero_readback() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    leaf.configure(limits()).unwrap();
    fs::write(leaf.label().join("cpu.max.burst"), "1\n").unwrap();

    assert!(matches!(
        leaf.verify_configured_controls(limits(), true),
        Err(CgroupError::ControlVerification { path, expected, found })
            if path.ends_with("cpu.max.burst") && expected == "0" && found == "1"
    ));
    deactivate(&mut leaf);
}

#[test]
fn ordinary_control_readback_rejects_any_effective_value_mismatch() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    leaf.configure(limits()).unwrap();
    fs::write(leaf.label().join("memory.max"), "536870912\n").unwrap();

    assert!(matches!(
        leaf.verify_configured_controls(limits(), true),
        Err(CgroupError::ControlVerification { expected, found, .. })
            if expected == "1073741824" && found == "536870912"
    ));
    deactivate(&mut leaf);
}

#[test]
fn ordinary_control_plumbing_refuses_configuration_after_population() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    fs::write(leaf.label().join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();

    assert!(matches!(
        leaf.configure(limits()),
        Err(CgroupError::LeafPopulatedDuringConfiguration { .. })
    ));
    assert_eq!(fs::read_to_string(leaf.label().join("pids.max")).unwrap(), "max");
    deactivate(&mut leaf);
}

#[test]
fn ordinary_control_plumbing_refuses_configuration_after_freeze() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    fs::write(leaf.label().join("cgroup.events"), "populated 0\nfrozen 1\n").unwrap();

    assert!(matches!(
        leaf.configure(limits()),
        Err(CgroupError::LeafFrozenDuringConfiguration { .. })
    ));
    assert_eq!(fs::read_to_string(leaf.label().join("pids.max")).unwrap(), "max");
    deactivate(&mut leaf);
}

#[test]
fn every_post_mkdir_checkpoint_rolls_back_the_provisional_leaf() {
    for target in [
        CreationStage::Mkdir,
        CreationStage::Pinned,
        CreationStage::Witnessed,
        CreationStage::AuthorityTransferred,
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = simulated_root(&temporary);
        let mut checkpoint = |stage| {
            if stage == target {
                Err(io::Error::other(format!("injected failure at {stage:?}")))
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            root.create_unconfigured_leaf_with(ID, &mut checkpoint),
            Err(CgroupError::DescriptorOperation { .. })
        ));
        assert_eq!(
            fs::read_dir(temporary.path()).unwrap().count(),
            0,
            "failed at {target:?}"
        );
    }
}

#[test]
fn provisional_rollback_failure_returns_retryable_authenticated_recovery() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut checkpoint = |stage| {
        if stage == CreationStage::Mkdir {
            let leaf = fs::read_dir(temporary.path()).unwrap().next().unwrap().unwrap().path();
            fs::write(leaf.join("injected-cleanup-blocker"), "keep until explicit retry").unwrap();
            Err(io::Error::other("injected post-mkdir failure"))
        } else {
            Ok(())
        }
    };

    let error = root.create_unconfigured_leaf_with(ID, &mut checkpoint).unwrap_err();
    let mut recovery = error.into_recovery().expect("rollback failure retains recovery");
    let leaf_path = recovery.label().to_owned();
    assert!(recovery.is_active());
    fs::remove_file(leaf_path.join("injected-cleanup-blocker")).unwrap();
    recovery.retry_remove().unwrap();
    assert!(!recovery.is_active());
    assert!(!leaf_path.exists());
}

#[test]
fn configuration_rollback_failure_returns_retryable_authenticated_recovery() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let leaf = root.create_unconfigured_leaf(ID).unwrap();
    let leaf_path = leaf.label().to_owned();
    create_simulated_controls(&leaf_path);
    fs::remove_file(leaf_path.join("cgroup.kill")).unwrap();

    let error = configure_created_leaf(leaf, limits()).unwrap_err();
    let mut recovery = error.into_recovery().expect("configuration failure retains recovery");
    assert_eq!(recovery.label(), leaf_path);
    for entry in fs::read_dir(&leaf_path).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    recovery.retry_remove().unwrap();
    assert!(!leaf_path.exists());
}

#[test]
fn placement_exposes_the_target_and_same_sole_root_descriptor_inside_this_crate() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let original_root_fd = root.authority.directory.as_raw_fd();
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    leaf.configure(limits()).unwrap();

    let placement = leaf.placement().unwrap();
    assert_eq!(placement.as_fd().as_raw_fd(), leaf.directory.as_raw_fd());
    assert_eq!(placement.target().as_raw_fd(), leaf.directory.as_raw_fd());
    assert_eq!(
        placement.root().as_raw_fd(),
        leaf.authority().unwrap().directory.as_raw_fd()
    );
    assert_eq!(placement.root().as_raw_fd(), original_root_fd);
    assert_ne!(placement.root().as_raw_fd(), placement.target().as_raw_fd());
    assert_eq!(
        placement.inherited_raw_fds(),
        [placement.root().as_raw_fd(), placement.target().as_raw_fd()]
    );
    assert_eq!(fs::read_to_string(leaf.label().join("cgroup.procs")).unwrap(), "");
    deactivate(&mut leaf);
}

#[test]
fn placement_release_requires_the_exact_child_membership() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    leaf.configure(limits()).unwrap();

    fs::write(leaf.label().join("cgroup.procs"), "91\n91\n").unwrap();
    leaf.require_sole_member(91).unwrap();
    fs::write(leaf.label().join("cgroup.procs"), "91\n92\n").unwrap();
    assert!(matches!(
        leaf.require_sole_member(91),
        Err(CgroupError::LeafMembership {
            expected: 91,
            first_foreign: Some(92),
            ..
        })
    ));
    deactivate(&mut leaf);
}

#[test]
fn control_lookup_refuses_symlink_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    let outside = temporary.path().join("outside");
    fs::write(&outside, "unchanged").unwrap();
    fs::remove_file(leaf.label().join("pids.max")).unwrap();
    symlink(&outside, leaf.label().join("pids.max")).unwrap();

    assert!(matches!(
        leaf.configure(limits()),
        Err(CgroupError::DescriptorOperation { .. })
    ));
    assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
    deactivate(&mut leaf);
}

#[test]
fn bounded_wait_observes_empty_and_times_out_on_populated_state() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    create_simulated_controls(leaf.label());
    let policy = DrainPolicy::new(Duration::from_millis(5), Duration::from_millis(1)).unwrap();
    leaf.wait_until_empty(policy).unwrap();

    fs::write(leaf.label().join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    assert!(matches!(
        leaf.wait_until_empty(policy),
        Err(CgroupError::DrainTimeout { .. })
    ));
    deactivate(&mut leaf);
}

#[test]
fn explicit_cleanup_timeout_retains_retry_authority_without_hidden_drop() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    let leaf_path = leaf.label().to_owned();
    create_simulated_controls(&leaf_path);
    fs::write(leaf_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    let policy = DrainPolicy::new(Duration::from_millis(5), Duration::from_millis(1)).unwrap();

    let started = Instant::now();
    assert!(matches!(
        leaf.kill_and_remove(policy),
        Err(CgroupError::DrainTimeout { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(leaf_path.exists());

    // Ordinary directories cannot model cgroupfs rmdir semantics because
    // their control files are real children. Removing the simulation files
    // lets the still-owned authenticated capability prove an explicit
    // later retry can remove the exact witnessed leaf.
    for entry in fs::read_dir(&leaf_path).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    leaf.remove_authenticated().unwrap();
    assert!(!leaf_path.exists());
}

#[test]
fn ordinary_directory_plumbing_removes_a_matching_witness_directly() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    let leaf_path = leaf.label().to_owned();
    leaf.remove_authenticated().unwrap();
    assert!(!leaf_path.exists());
}

#[test]
fn ordinary_directory_plumbing_preserves_a_replacement_before_precheck() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let mut leaf = root.create_unconfigured_leaf(ID).unwrap();
    let leaf_path = leaf.label().to_owned();
    fs::remove_dir(&leaf_path).unwrap();
    fs::create_dir(&leaf_path).unwrap();
    fs::write(leaf_path.join("foreign"), "keep").unwrap();

    assert!(matches!(
        leaf.remove_authenticated(),
        Err(CgroupError::LeafReplaced { .. })
    ));
    assert_eq!(fs::read_to_string(leaf_path.join("foreign")).unwrap(), "keep");
    deactivate(&mut leaf);
}

#[test]
fn write_helper_replaces_longer_fake_contents_in_one_exact_value() {
    let temporary = tempfile::tempdir().unwrap();
    let root = simulated_root(&temporary);
    let path = temporary.path().join("value");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"a much longer previous value").unwrap();
    drop(file);

    write_control(&root.authority.directory, c"value", b"1", root.label()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"1");
}
