/// Establish one dedicated owner-private workspace root without trusting the
/// ambient umask or following a final symlink.
///
/// The nearest existing parent is canonicalized and authenticated first. Every
/// missing descendant is then created through that descriptor. Existing
/// shared-writable components fail before any chmod; only an already-safe leaf
/// or a directory created by this call is normalized to exact mode 0700.
#[cfg(test)]
pub(crate) fn prepare_private_workspace_root(path: &Path) -> io::Result<PathBuf> {
    prepare_private_workspace_root_pinned(path).map(|(path, _anchor)| path)
}

/// Establish and retain one exact owner-private workspace root.
///
/// Keeping the descriptor returned by this operation lets a later destructive
/// caller prove that the pathname still denotes the root selected here rather
/// than merely pinning whichever directory happens to occupy the name later.
pub(crate) fn prepare_private_workspace_root_pinned(path: &Path) -> io::Result<(PathBuf, StdFile)> {
    prepare_private_workspace_root_with_policy_pinned(path, WorkspaceRootLeafPolicy::NormalizeExisting)
}

/// Create a missing owner-private workspace root without changing an existing
/// final entry.
///
/// Forge applies its own broader installation-root policy: for example, a
/// safe read-only root or a root owned by uid 0 may be valid. Mason therefore
/// owns only creation here. A final entry that exists before this call, or
/// wins the final `mkdirat` race, must still pin as a real directory without
/// symlinks, but its inode and mode are left for Forge to validate unchanged.
#[cfg(test)]
pub(crate) fn prepare_missing_private_workspace_root(path: &Path) -> io::Result<PathBuf> {
    prepare_missing_private_workspace_root_pinned(path).map(|(path, _anchor)| path)
}

/// Create a missing Forge root and retain the exact selected directory.
///
/// Existing roots remain mode-for-mode unchanged so Forge can apply its wider
/// root-owned/read-only policy. The retained `O_PATH` descriptor is valid for
/// those roots even when Mason cannot open them for reading or writing.
pub(crate) fn prepare_missing_private_workspace_root_pinned(path: &Path) -> io::Result<(PathBuf, StdFile)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    match std::fs::symlink_metadata(&absolute) {
        Ok(_) => {
            let anchor = pin_workspace_root(&absolute)?;
            return Ok((absolute, anchor));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(source),
    }

    prepare_private_workspace_root_with_policy_pinned(&absolute, WorkspaceRootLeafPolicy::PreserveExisting)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceRootLeafPolicy {
    NormalizeExisting,
    PreserveExisting,
}

enum EnsuredPrivateDirectory {
    Controlled(StdFile),
    ExistingLeaf(StdFile),
}

#[cfg(test)]
fn prepare_private_workspace_root_with_policy(
    path: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<PathBuf> {
    prepare_private_workspace_root_with_policy_pinned(path, leaf_policy).map(|(path, _anchor)| path)
}

fn prepare_private_workspace_root_with_policy_pinned(
    path: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<(PathBuf, StdFile)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let leaf = absolute
        .file_name()
        .ok_or_else(|| invalid_binding(format!("private workspace root has no leaf name: {path:?}")))?
        .to_owned();
    let mut ancestor = absolute
        .parent()
        .ok_or_else(|| invalid_binding(format!("private workspace root has no parent: {path:?}")))?
        .to_owned();
    let mut missing = vec![leaf];

    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => break,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    invalid_binding(format!(
                        "cannot find an existing parent for private workspace root {path:?}"
                    ))
                })?;
                missing.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| {
                        invalid_binding(format!(
                            "cannot find an existing parent for private workspace root {path:?}"
                        ))
                    })?
                    .to_owned();
            }
            Err(source) => return Err(source),
        }
    }

    // Parent aliases are resolved once, before the authoritative descriptor is
    // opened. The returned root path uses that canonical parent, so later
    // retained-anchor checks do not depend on the alias remaining unchanged.
    let ancestor = ancestor.canonicalize()?;
    let anchor = open_directory_nofollow(&ancestor)?;
    require_controlled_directory(&anchor, &ancestor, false)?;
    missing.reverse();
    let relative = missing.iter().collect::<PathBuf>();
    let root_path = ancestor.join(&relative);
    let root = match ensure_private_directory_at_with_policy(&anchor, &relative, &root_path, leaf_policy)? {
        EnsuredPrivateDirectory::Controlled(root) => {
            require_controlled_directory(&root, &root_path, true)?;
            let reopened = open_directory_nofollow(&root_path)?;
            require_same_directory(&root, &reopened, &root_path)?;
            root
        }
        EnsuredPrivateDirectory::ExistingLeaf(root) => {
            debug_assert_eq!(leaf_policy, WorkspaceRootLeafPolicy::PreserveExisting);
            let reopened = pin_workspace_root(&root_path)?;
            require_same_directory(&root, &reopened, &root_path)?;
            root
        }
    };
    Ok((root_path, root))
}

fn private_host_relative(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(root).map_err(|_| {
        invalid_binding(format!(
            "private host path {path:?} is not beneath workspace root {root:?}"
        ))
    })?;
    let raw = relative.as_os_str().as_bytes();
    if raw.is_empty()
        || raw.len() > MAX_PRIVATE_HOST_PATH_BYTES
        || raw.contains(&0)
        || relative.components().count() > MAX_PRIVATE_HOST_PATH_COMPONENTS
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_binding(format!(
            "invalid private host path beneath workspace root: {path:?}"
        )));
    }
    Ok(relative.to_owned())
}

fn ensure_private_directory_at(root: &StdFile, relative: &Path, display: &Path) -> io::Result<StdFile> {
    match ensure_private_directory_at_with_policy(root, relative, display, WorkspaceRootLeafPolicy::NormalizeExisting)?
    {
        EnsuredPrivateDirectory::Controlled(directory) => Ok(directory),
        EnsuredPrivateDirectory::ExistingLeaf(_) => {
            unreachable!("normalizing private-directory traversal cannot preserve an existing leaf")
        }
    }
}

fn ensure_private_directory_at_with_policy(
    root: &StdFile,
    relative: &Path,
    display: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<EnsuredPrivateDirectory> {
    let root_metadata = root.metadata()?;
    let mut current = root.try_clone()?;
    let mut traversed = PathBuf::new();
    let component_count = relative.components().count();

    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return Err(invalid_binding(format!("invalid private host path: {display:?}")));
        };
        traversed.push(name);
        let name = CString::new(name.as_bytes())
            .map_err(|_| invalid_binding(format!("private host path contains NUL: {display:?}")))?;
        let leaf = index + 1 == component_count;

        if leaf && leaf_policy == WorkspaceRootLeafPolicy::PreserveExisting {
            if !mkdir_private_directory_at(&current, &name, &traversed)? {
                let existing = open_path_child(&current, &name).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!("pin existing private host leaf {traversed:?}: {source}"),
                    )
                })?;
                require_workspace_root_directory(&existing, display)?;
                return Ok(EnsuredPrivateDirectory::ExistingLeaf(existing));
            }
            current = recover_created_private_directory(&root_metadata, &current, &name, &traversed, display)?;
            continue;
        }

        let mut next = open_private_child(&current, &name);
        if next
            .as_ref()
            .is_err_and(|source| source.kind() == io::ErrorKind::NotFound)
        {
            let created = mkdir_private_directory_at(&current, &name, &traversed)?;
            if created {
                current = recover_created_private_directory(&root_metadata, &current, &name, &traversed, display)?;
                continue;
            }
            next = open_private_child(&current, &name);
        }
        let next = next.map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "open private host directory component {traversed:?} without links or mount crossings: {source}"
                ),
            )
        })?;
        require_same_device(&root_metadata, &next, display)?;
        require_controlled_directory(&next, display, false)?;

        if leaf {
            next.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            require_controlled_directory(&next, display, true)?;
        }
        current = next;
    }
    Ok(EnsuredPrivateDirectory::Controlled(current))
}

fn mkdir_private_directory_at(parent: &StdFile, name: &CStr, display: &Path) -> io::Result<bool> {
    loop {
        // SAFETY: `parent` and `name` remain live. mkdirat interprets one
        // validated normal component relative to the authenticated parent.
        if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            return Ok(true);
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => return Ok(false),
            _ => {
                return Err(io::Error::new(
                    source.kind(),
                    format!("create private host directory {display:?}: {source}"),
                ));
            }
        }
    }
}

fn recover_created_private_directory(
    root_metadata: &std::fs::Metadata,
    parent: &StdFile,
    name: &CStr,
    traversed: &Path,
    display: &Path,
) -> io::Result<StdFile> {
    let pinned = open_path_child(parent, name).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("pin newly-created private host directory {traversed:?}: {source}"),
        )
    })?;
    require_same_device(root_metadata, &pinned, display)?;
    require_created_private_directory(&pinned, traversed)?;
    chmod_path_descriptor(&pinned, 0o700)?;

    let directory = open_private_child(parent, name).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("reopen newly-created private host directory {traversed:?}: {source}"),
        )
    })?;
    require_same_device(root_metadata, &directory, display)?;
    require_same_directory(&pinned, &directory, display)?;
    require_controlled_directory(&directory, display, true)?;
    Ok(directory)
}

fn require_created_private_directory(directory: &StdFile, path: &Path) -> io::Result<()> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode & !0o700 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "new private host directory is not a safe owner-only residue: {path:?} (uid={}, mode={mode:#06o})",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_workspace_root_directory(directory: &StdFile, path: &Path) -> io::Result<()> {
    if directory.metadata()?.file_type().is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("workspace root is not a directory and will not be followed: {path:?}"),
        ))
    }
}

fn private_parent_and_leaf(
    root: &StdFile,
    relative: &Path,
    display: &Path,
    create_parent: bool,
) -> io::Result<(Option<StdFile>, CString)> {
    let mut components = relative.components().collect::<Vec<_>>();
    let Some(Component::Normal(leaf)) = components.pop() else {
        return Err(invalid_binding(format!("invalid private host leaf: {display:?}")));
    };
    let leaf = CString::new(leaf.as_bytes())
        .map_err(|_| invalid_binding(format!("private host leaf contains NUL: {display:?}")))?;
    if components.is_empty() {
        return Ok((Some(root.try_clone()?), leaf));
    }
    let parent_relative = components.iter().collect::<PathBuf>();
    if create_parent {
        let parent_display = display.parent().unwrap_or(display);
        return ensure_private_directory_at(root, &parent_relative, parent_display).map(|file| (Some(file), leaf));
    }

    let mut current = root.try_clone()?;
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid_binding(format!("invalid private host parent: {display:?}")));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| invalid_binding(format!("private host parent contains NUL: {display:?}")))?;
        current = match open_private_child(&current, &name) {
            Ok(next) => next,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok((None, leaf)),
            Err(source) => return Err(source),
        };
        require_controlled_directory(&current, display, false)?;
    }
    Ok((Some(current), leaf))
}

fn stale_leaf_name(leaf: &CStr) -> io::Result<CString> {
    let mut bytes = Vec::with_capacity(leaf.to_bytes().len() + 13);
    bytes.push(b'.');
    bytes.extend_from_slice(leaf.to_bytes());
    bytes.extend_from_slice(b".cast-stale");
    if bytes.len() > 255 {
        return Err(invalid_binding(
            "private host quarantine name exceeds NAME_MAX".to_owned(),
        ));
    }
    CString::new(bytes).map_err(|_| invalid_binding("private host quarantine name contains NUL".to_owned()))
}

fn create_private_leaf(parent: &StdFile, leaf: &CStr, display: &Path) -> io::Result<()> {
    // SAFETY: parent and leaf remain live and leaf is one normal component.
    if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("create fresh private host directory {display:?}: {source}"),
        ));
    }
    let directory = open_controlled_named_directory(parent, leaf, display)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("new private host directory disappeared: {display:?}"),
        )
    })?;
    require_controlled_directory(&directory, display, true)
}

fn open_controlled_named_directory(parent: &StdFile, name: &CStr, display: &Path) -> io::Result<Option<StdFile>> {
    let pinned = match open_path_child(parent, name) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "existing private host leaf is unsafe: {display:?} (uid={}, mode={:#06o})",
                metadata.uid(),
                metadata.mode() & 0o7777
            ),
        ));
    }
    chmod_path_descriptor(&pinned, 0o700)?;
    let directory = open_private_child(parent, name)?;
    if directory_identity(&pinned)? != directory_identity(&directory)? {
        return Err(io::Error::other(format!(
            "private host leaf was replaced while opening: {display:?}"
        )));
    }
    require_controlled_directory(&directory, display, true)?;
    Ok(Some(directory))
}

fn open_path_child(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from((nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC) as u32);
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, component, and open_how remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { StdFile::from_raw_fd(descriptor) })
}

fn rename_noreplace(parent: &StdFile, from: &CStr, to: &CStr, display: &Path) -> io::Result<()> {
    // SAFETY: both names and the shared parent descriptor remain live.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("atomically detach private host directory {display:?}: {source}"),
        ));
    }
    Ok(())
}

fn directory_identity(file: &StdFile) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
