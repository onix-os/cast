fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
            }
        }
    }
    if normalized != path {
        return Err(CgroupError::InvalidMountPath { path: path.to_owned() });
    }
    Ok(normalized)
}

fn normalized_relative(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() || path.as_os_str().is_empty() {
        return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
                return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
            }
        }
    }
    if normalized.as_os_str().is_empty() || normalized != path {
        return Err(CgroupError::InvalidDelegatedPath { path: path.to_owned() });
    }
    Ok(normalized)
}

fn validate_leaf_identity(identity: &str) -> Result<()> {
    if identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CgroupError::InvalidLeafIdentity {
            identity: identity.to_owned(),
        })
    }
}

fn system_page_size() -> Result<u64> {
    // SAFETY: sysconf has no pointer arguments and `_SC_PAGESIZE` is valid.
    let found = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(found)
        .ok()
        .filter(|page_size| *page_size > 0)
        .ok_or(CgroupError::InvalidPageSize { found })
}

fn random_leaf_name(identity: &str) -> io::Result<CString> {
    let mut random = [0_u8; LEAF_RANDOM_BYTES];
    let mut filled = 0;
    let mut interrupted = 0;
    while filled < random.len() {
        // SAFETY: the remaining slice is writable for exactly the supplied
        // length; getrandom retains no pointer after returning.
        let result = unsafe { libc::getrandom(random[filled..].as_mut_ptr().cast(), random.len() - filled, 0) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interrupted < MAX_GETRANDOM_EINTR_RETRIES {
                interrupted += 1;
                continue;
            }
            return Err(source);
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned no cgroup leaf-name entropy",
            ));
        }
        filled += read;
    }

    let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    CString::new(format!("{LEAF_NAME_PREFIX}{identity}-{suffix}"))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "generated cgroup leaf name contains NUL"))
}

fn current_tgid() -> Result<u32> {
    // SAFETY: getpid has no arguments and cannot fail on Linux.
    let found = unsafe { libc::getpid() };
    u32::try_from(found)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(CgroupError::InvalidSupervisorTgid { found })
}

fn capture_supervisor(root: &OwnedFd, root_label: &Path) -> Result<SupervisorAuthority> {
    let opener_tgid = current_tgid()?;
    let (supervisor, label) = open_supervisor(root, root_label)?;
    probe_supervisor_descriptor(&supervisor, &label, opener_tgid)?;
    Ok(SupervisorAuthority {
        identity_witness: descriptor_identity(&supervisor, &label)?,
        opener_tgid,
    })
}

fn probe_root_authority(directory: &OwnedFd, label: &Path) -> Result<()> {
    let enabled = probe_root_authority_pre_enable(directory, label)?;
    require_controllers(&enabled, &label.join("cgroup.subtree_control"))
}

/// Authenticate every delegated-root invariant except the initially-empty
/// enabled-controller set, returning that set to the one-time activation
/// path. No caller may mutate `cgroup.subtree_control` until the surrounding
/// supervisor topology has also been authenticated.
fn probe_root_authority_pre_enable(directory: &OwnedFd, label: &Path) -> Result<BTreeSet<String>> {
    require_directory(directory, label)?;
    require_cgroup2(directory, label)?;
    // Recheck owner/mode and reassert the same open-file-description lock on
    // every probe, not only at initial acquisition.
    acquire_exclusive_delegation(directory, label)?;
    require_domain(directory, label)?;

    let available = read_word_set(directory, c"cgroup.controllers", label)?;
    require_controllers(&available, &label.join("cgroup.controllers"))?;
    let enabled = read_word_set(directory, c"cgroup.subtree_control", label)?;

    let members = read_pid_list(directory, c"cgroup.procs", label)?;
    if let Some(pid) = members.first() {
        return Err(CgroupError::DelegationPopulated {
            path: label.join("cgroup.procs"),
            pid: *pid,
        });
    }

    // Authenticate both process and thread migration controls even though the
    // accepted topology is a domain hierarchy. This prevents a separately
    // delegated threaded-migration authority from being shared behind the
    // directory's otherwise-private mode bits.
    drop(open_owned_writable_control(directory, c"cgroup.procs", label)?);
    drop(open_owned_writable_control(directory, c"cgroup.threads", label)?);
    drop(open_owned_writable_control(
        directory,
        c"cgroup.subtree_control",
        label,
    )?);
    require_populated_unfrozen_delegation(read_events(directory, label)?, &label.join("cgroup.events"))?;
    Ok(enabled)
}

fn probe_supervisor(root: &OwnedFd, root_label: &Path, expected: &SupervisorAuthority) -> Result<()> {
    let found_tgid = current_tgid()?;
    if found_tgid != expected.opener_tgid {
        return Err(CgroupError::SupervisorProcessChanged {
            expected: expected.opener_tgid,
            found: found_tgid,
        });
    }

    let (supervisor, label) = open_supervisor(root, root_label)?;
    let found = descriptor_identity(&supervisor, &label)?;
    if found != expected.identity_witness {
        return Err(CgroupError::SupervisorReplaced {
            path: label,
            expected_device: expected.identity_witness.device,
            expected_inode: expected.identity_witness.inode,
            found_device: found.device,
            found_inode: found.inode,
        });
    }
    probe_supervisor_descriptor(&supervisor, &label, expected.opener_tgid)
}

fn open_supervisor(root: &OwnedFd, root_label: &Path) -> Result<(OwnedFd, PathBuf)> {
    let label = root_label.join(os_str(SUPERVISOR_NAME));
    let supervisor = open_control_path(
        root,
        SUPERVISOR_NAME,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("open fixed Cast supervisor subgroup", &label, source))?;
    Ok((supervisor, label))
}

fn probe_supervisor_descriptor(directory: &OwnedFd, label: &Path, expected_tgid: u32) -> Result<()> {
    require_directory(directory, label)?;
    require_cgroup2(directory, label)?;
    require_owned_private(directory, label)?;
    require_domain(directory, label)?;
    require_exact_supervisor_membership(
        &read_pid_list(directory, c"cgroup.procs", label)?,
        expected_tgid,
        &label.join("cgroup.procs"),
    )?;
    drop(open_owned_writable_control(directory, c"cgroup.procs", label)?);
    drop(open_owned_writable_control(directory, c"cgroup.threads", label)?);
    drop(open_owned_writable_control(
        directory,
        c"cgroup.subtree_control",
        label,
    )?);
    require_populated_unfrozen_delegation(read_events(directory, label)?, &label.join("cgroup.events"))?;
    require_descendant_topology(directory, label, 0, 0)
}

fn probe_leaf(root: &OwnedFd, leaf: &CgroupLeaf) -> Result<()> {
    probe_leaf_witness(root, leaf)?;
    require_directory(&leaf.directory, &leaf.label)?;
    require_cgroup2(&leaf.directory, &leaf.label)?;
    require_owned_private(&leaf.directory, &leaf.label)?;
    require_domain(&leaf.directory, &leaf.label)?;
    let members = read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?;
    if !members.is_empty() {
        return Err(CgroupError::LeafPopulatedDuringConfiguration {
            path: leaf.label.join("cgroup.procs"),
        });
    }
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.procs",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.threads",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.kill",
        &leaf.label,
    )?);
    require_empty_unfrozen_delegation(
        read_events(&leaf.directory, &leaf.label)?,
        &leaf.label.join("cgroup.events"),
    )?;
    require_descendant_topology(&leaf.directory, &leaf.label, 0, 0)?;
    probe_leaf_witness(root, leaf)
}

fn probe_activated_leaf(root: &OwnedFd, leaf: &CgroupLeaf, expected_tgid: u32) -> Result<()> {
    probe_leaf_witness(root, leaf)?;
    require_directory(&leaf.directory, &leaf.label)?;
    require_cgroup2(&leaf.directory, &leaf.label)?;
    require_owned_private(&leaf.directory, &leaf.label)?;
    require_domain(&leaf.directory, &leaf.label)?;
    require_exact_leaf_membership(
        &read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?,
        expected_tgid,
        &leaf.label.join("cgroup.procs"),
    )?;
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.procs",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.threads",
        &leaf.label,
    )?);
    drop(open_owned_writable_control(
        &leaf.directory,
        c"cgroup.kill",
        &leaf.label,
    )?);
    require_populated_unfrozen_delegation(
        read_events(&leaf.directory, &leaf.label)?,
        &leaf.label.join("cgroup.events"),
    )?;
    require_descendant_topology(&leaf.directory, &leaf.label, 0, 0)?;
    probe_leaf_witness(root, leaf)
}

fn probe_leaf_witness(root: &OwnedFd, leaf: &CgroupLeaf) -> Result<()> {
    let pinned = open_control_path(
        root,
        &leaf.name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("reopen named cgroup leaf for topology probe", &leaf.label, source))?;
    require_directory(&pinned, &leaf.label)?;
    let found = descriptor_identity(&pinned, &leaf.label)?;
    if found == leaf.identity_witness {
        Ok(())
    } else {
        Err(CgroupError::LeafReplaced {
            path: leaf.label.clone(),
            expected_device: leaf.identity_witness.device,
            expected_inode: leaf.identity_witness.inode,
            found_device: found.device,
            found_inode: found.inode,
        })
    }
}

fn require_domain(directory: &OwnedFd, label: &Path) -> Result<()> {
    let group_type = read_single_value(directory, c"cgroup.type", label)?;
    if group_type == "domain" {
        Ok(())
    } else {
        Err(CgroupError::InvalidCgroupType {
            path: label.join("cgroup.type"),
            found: group_type,
        })
    }
}

fn require_exact_supervisor_membership(members: &[u32], expected: u32, path: &Path) -> Result<()> {
    // Kernel documentation permits duplicate entries while a process moves.
    // Membership is therefore exact when the unique set is exactly {self}.
    let unique = members.iter().copied().collect::<BTreeSet<_>>();
    let expected_present = unique.contains(&expected);
    let first_foreign = unique.iter().copied().find(|pid| *pid != expected);
    if expected_present && first_foreign.is_none() && unique.len() == 1 {
        Ok(())
    } else {
        Err(CgroupError::SupervisorMembership {
            path: path.to_owned(),
            expected,
            expected_present,
            first_foreign,
            unique_members: unique.len(),
        })
    }
}

fn require_exact_leaf_membership(members: &[u32], expected: u32, path: &Path) -> Result<()> {
    let unique = members.iter().copied().collect::<BTreeSet<_>>();
    let expected_present = unique.contains(&expected);
    let first_foreign = unique.iter().copied().find(|pid| *pid != expected);
    if expected > 0 && expected_present && first_foreign.is_none() && unique.len() == 1 {
        Ok(())
    } else {
        Err(CgroupError::LeafMembership {
            path: path.to_owned(),
            expected,
            expected_present,
            first_foreign,
            unique_members: unique.len(),
        })
    }
}

fn require_descendant_topology(
    directory: &OwnedFd,
    label: &Path,
    expected_descendants: u64,
    maximum_dying_descendants: u64,
) -> Result<()> {
    let (descendants, dying_descendants) = read_descendant_counts(directory, label)?;
    validate_descendant_topology(
        descendants,
        dying_descendants,
        expected_descendants,
        maximum_dying_descendants,
        &label.join("cgroup.stat"),
    )
}

fn validate_descendant_topology(
    descendants: u64,
    dying_descendants: u64,
    expected_descendants: u64,
    maximum_dying_descendants: u64,
    path: &Path,
) -> Result<()> {
    if descendants == expected_descendants && dying_descendants <= maximum_dying_descendants {
        Ok(())
    } else {
        Err(CgroupError::DelegationTopology {
            path: path.to_owned(),
            expected_descendants,
            maximum_dying_descendants,
            descendants,
            dying_descendants,
        })
    }
}
