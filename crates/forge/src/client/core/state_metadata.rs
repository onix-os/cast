const STATE_TREE_DIRECTORY_MODE: u32 = 0o755;
const STATE_ID_MODE: u32 = 0o644;
const STATE_ID_TEMPORARY_MODE: u32 = 0o600;
const STATE_ID_NAME: &str = ".stateID";
const STATE_ID_TEMPORARY_NAME: &str = ".cast-state-id.tmp";
const STATE_ID_C_NAME: &CStr = c".stateID";
const STATE_ID_TEMPORARY_C_NAME: &CStr = c".cast-state-id.tmp";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StateMetadataDirectoryWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
}

impl StateMetadataDirectoryWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode() & 0o7777,
            owner: metadata.uid(),
        }
    }
}

#[derive(Debug)]
struct StateMetadataDirectory {
    path: PathBuf,
    file: std::fs::File,
    witness: StateMetadataDirectoryWitness,
}

fn record_state_id(root: &Path, state: state::Id) -> Result<(), Error> {
    let root_path = state_metadata_absolute_path(root)?;
    let root = open_or_create_state_metadata_root(&root_path)?;
    let usr_path = root_path.join("usr");
    let usr =
        open_or_create_state_metadata_directory(&root.file, OsStr::new("usr"), &usr_path, STATE_TREE_DIRECTORY_MODE)?;

    write_state_id(&usr, state.to_string().as_bytes())?;
    usr.file.sync_all()?;
    root.file.sync_all()?;
    require_state_metadata_directory(&root.file, &usr)?;
    require_named_state_metadata_root(&root_path, &root)?;
    Ok(())
}

/// Write `.stateID` beneath the exact wrapper retained before candidate
/// materialization. No pathname is reopened as write authority; the returned
/// `/usr` descriptor is the same inode subsequently handed to tree-identity
/// preparation.
fn record_state_id_retained(
    root: &fixed_staging::RetainedFixedStaging,
    candidate_usr: &std::fs::File,
    state: state::Id,
) -> Result<(), Error> {
    let root_file = root.directory();
    let root_path = root.path();
    let root_witness = state_metadata_directory_witness(root_file, root_path)?;
    require_no_default_acl(root_file, root_path)?;

    let usr_path = root_path.join("usr");
    let usr_file = candidate_usr.try_clone()?;
    let usr = StateMetadataDirectory {
        path: usr_path.clone(),
        witness: state_metadata_directory_witness(&usr_file, &usr_path)?,
        file: usr_file,
    };
    require_state_metadata_directory(root_file, &usr)?;

    fixed_staging::before_retained_state_metadata();
    write_state_id(&usr, state.to_string().as_bytes())?;
    usr.file.sync_all()?;
    root_file.sync_all()?;
    require_state_metadata_directory(root_file, &usr)?;
    if state_metadata_directory_witness(root_file, root_path)? != root_witness {
        return Err(io::Error::other(format!(
            "retained state metadata root changed while writing {}",
            usr_path.display()
        ))
        .into());
    }
    require_no_default_acl(root_file, root_path)?;
    require_state_metadata_directory(root_file, &usr)?;
    Ok(())
}

fn revalidate_fixed_staging(
    retained: Option<&fixed_staging::RetainedFixedStaging>,
    installation: &Installation,
) -> Result<(), Error> {
    retained
        .map(|retained| retained.revalidate(installation))
        .transpose()
        .map(|_| ())
        .map_err(|source| Error::StatefulCandidateMaterialization {
            source: Box::new(source),
        })
}

fn state_metadata_absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state metadata root path is empty",
        ));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            PathComponent::RootDir | PathComponent::CurDir => {}
            PathComponent::Normal(component) => normalized.push(component),
            PathComponent::ParentDir | PathComponent::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root contains a parent or platform prefix component",
                ));
            }
        }
    }
    Ok(normalized)
}

fn open_or_create_state_metadata_root(path: &Path) -> Result<StateMetadataDirectory, Error> {
    match open_absolute_state_metadata_path(path) {
        Ok(pinned) => {
            let expected =
                normalize_recoverable_state_metadata_directory(&pinned, path, STATE_TREE_DIRECTORY_MODE, false)?;
            let file = open_absolute_state_metadata_directory(path)?;
            let actual = state_metadata_directory_witness(&file, path)?;
            require_no_default_acl(&file, path)?;
            if actual != expected {
                return Err(io::Error::other(format!(
                    "state metadata root was replaced while reopening {}",
                    path.display()
                ))
                .into());
            }
            Ok(StateMetadataDirectory {
                path: path.to_owned(),
                file,
                witness: expected,
            })
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound && path != Path::new("/") => {
            let parent_path = path.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root has no parent directory",
                )
            })?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root has no final component",
                )
            })?;
            let parent = open_absolute_state_metadata_directory(parent_path)?;
            let parent_witness = state_metadata_directory_witness(&parent, parent_path)?;
            let root = open_or_create_state_metadata_directory(&parent, name, path, STATE_TREE_DIRECTORY_MODE)?;
            let named_parent = open_absolute_state_metadata_directory(parent_path)?;
            if state_metadata_directory_witness(&named_parent, parent_path)? != parent_witness {
                return Err(io::Error::other(format!(
                    "state metadata root parent was replaced while creating {}",
                    path.display()
                ))
                .into());
            }
            Ok(root)
        }
        Err(source) => Err(source.into()),
    }
}

fn open_or_create_state_metadata_directory(
    parent: &std::fs::File,
    name: &OsStr,
    path: &Path,
    creation_mode: u32,
) -> Result<StateMetadataDirectory, Error> {
    let name_c = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "state metadata component contains NUL"))?;
    let created = mkdirat_state_metadata(parent.as_raw_fd(), &name_c, creation_mode)?;
    let pinned = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    )?;

    let expected = normalize_recoverable_state_metadata_directory(&pinned, path, creation_mode, created)?;

    let file = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )?;
    let actual = state_metadata_directory_witness(&file, path)?;
    require_no_default_acl(&file, path)?;
    if actual != expected {
        return Err(io::Error::other(format!(
            "state metadata directory was replaced while opening: {}",
            path.display()
        ))
        .into());
    }

    let directory = StateMetadataDirectory {
        path: path.to_owned(),
        file,
        witness: expected,
    };
    directory.file.sync_all()?;
    parent.sync_all()?;
    require_state_metadata_directory(parent, &directory)?;
    Ok(directory)
}

fn normalize_recoverable_state_metadata_directory(
    file: &std::fs::File,
    path: &Path,
    requested_mode: u32,
    created: bool,
) -> io::Result<StateMetadataDirectoryWitness> {
    if !created && let Ok(witness) = state_metadata_directory_witness(file, path) {
        return Ok(witness);
    }

    require_fresh_state_metadata_directory(file, path, requested_mode)?;
    chmod_path_descriptor(file, requested_mode)?;
    let witness = state_metadata_directory_witness(file, path)?;
    if witness.mode != requested_mode {
        return Err(io::Error::other(format!(
            "recovered state metadata directory has mode {:04o}, expected {requested_mode:04o}: {}",
            witness.mode,
            path.display()
        )));
    }
    Ok(witness)
}

fn open_absolute_state_metadata_path(path: &Path) -> io::Result<std::fs::File> {
    open_state_metadata_at(
        AT_FDCWD,
        path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    )
}

fn open_absolute_state_metadata_directory(path: &Path) -> io::Result<std::fs::File> {
    open_state_metadata_at(
        AT_FDCWD,
        path,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )
}

fn open_state_metadata_at(parent: RawFd, path: &Path, flags: i32) -> io::Result<std::fs::File> {
    let resolve = if path.is_absolute() {
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64
    } else {
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64
    };
    openat2_frozen(parent, path, flags, resolve).map(|file| file.into_parts().0)
}

fn mkdirat_state_metadata(parent: RawFd, name: &CStr, mode: u32) -> io::Result<bool> {
    loop {
        // SAFETY: parent is a live directory descriptor and name is one
        // retained NUL-terminated component. mkdirat never follows that final
        // component.
        if unsafe { nix::libc::mkdirat(parent, name.as_ptr(), mode) } == 0 {
            return Ok(true);
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::AlreadyExists => return Ok(false),
            _ => return Err(source),
        }
    }
}

fn require_fresh_state_metadata_directory(file: &std::fs::File, path: &Path, requested_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    // A successful mkdir in this call may expose only an owner-owned subset
    // of the requested bits. Anything else could be a replacement and must
    // not be chmod-laundered through the retained descriptor.
    if metadata.file_type().is_dir() && metadata.uid() == effective_user_id() && mode & !requested_mode == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "fresh state metadata directory is not recoverable mkdir residue: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ))
    }
}

fn state_metadata_directory_witness(file: &std::fs::File, path: &Path) -> io::Result<StateMetadataDirectoryWitness> {
    let metadata = file.metadata()?;
    let witness = StateMetadataDirectoryWitness::from_metadata(&metadata);
    if metadata.file_type().is_dir()
        && witness.owner == effective_user_id()
        && witness.mode & 0o7000 == 0
        && witness.mode & 0o022 == 0
        && witness.mode & 0o700 == 0o700
    {
        Ok(witness)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state metadata component is not one safe owner-controlled directory: {} (uid={}, mode={:04o})",
                path.display(),
                witness.owner,
                witness.mode
            ),
        ))
    }
}

fn require_state_metadata_directory(parent: &std::fs::File, expected: &StateMetadataDirectory) -> Result<(), Error> {
    if state_metadata_directory_witness(&expected.file, &expected.path)? != expected.witness {
        return Err(io::Error::other(format!(
            "retained state metadata directory changed: {}",
            expected.path.display()
        ))
        .into());
    }
    require_no_default_acl(&expected.file, &expected.path)?;
    let name = expected
        .path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state metadata directory has no name"))?;
    let named = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )?;
    if state_metadata_directory_witness(&named, &expected.path)? != expected.witness {
        return Err(io::Error::other(format!(
            "named state metadata directory changed: {}",
            expected.path.display()
        ))
        .into());
    }
    require_no_default_acl(&named, &expected.path)?;
    Ok(())
}

fn require_named_state_metadata_root(path: &Path, expected: &StateMetadataDirectory) -> Result<(), Error> {
    if state_metadata_directory_witness(&expected.file, path)? != expected.witness {
        return Err(io::Error::other(format!("retained state metadata root changed: {}", path.display())).into());
    }
    require_no_default_acl(&expected.file, path)?;
    let named = open_absolute_state_metadata_directory(path)?;
    if state_metadata_directory_witness(&named, path)? != expected.witness {
        return Err(io::Error::other(format!("named state metadata root changed: {}", path.display())).into());
    }
    require_no_default_acl(&named, path)?;
    Ok(())
}

fn write_state_id(usr: &StateMetadataDirectory, contents: &[u8]) -> Result<(), Error> {
    let marker_path = usr.path.join(STATE_ID_NAME);
    let temporary_path = usr.path.join(STATE_ID_TEMPORARY_NAME);
    let previous = open_existing_state_id(usr, &marker_path)?;
    let (temporary, temporary_identity) = prepare_state_id_temporary(usr, &temporary_path)?;

    truncate_state_id(&temporary)?;
    fchmod(temporary.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_TEMPORARY_MODE))?;
    write_state_id_bytes(&temporary, contents)?;
    temporary.sync_all()?;
    fchmod(temporary.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_MODE))?;
    temporary.sync_all()?;
    require_complete_state_id(&temporary, &temporary_path, temporary_identity, contents.len() as u64)?;

    require_expected_state_id_name(usr, previous, &marker_path)?;
    rename_state_id_temporary(usr.file.as_raw_fd(), previous.is_some())?;
    usr.file.sync_all()?;

    let mut named = open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_NAME),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
    )?;
    require_complete_state_id(&named, &marker_path, temporary_identity, contents.len() as u64)?;
    let bound = contents.len().saturating_add(1);
    let mut actual = Vec::with_capacity(bound);
    (&mut named).take(bound as u64).read_to_end(&mut actual)?;
    if actual != contents {
        return Err(io::Error::other(format!("state ID marker content mismatch at {}", marker_path.display())).into());
    }
    require_complete_state_id(&named, &marker_path, temporary_identity, contents.len() as u64)?;
    match open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_TEMPORARY_NAME),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    ) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::other("state ID temporary still has a public name after publication").into()),
        Err(source) => Err(source.into()),
    }
}

fn open_existing_state_id(usr: &StateMetadataDirectory, marker_path: &Path) -> Result<Option<(u64, u64)>, Error> {
    let probe = match open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_NAME),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    ) {
        Ok(probe) => probe,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source.into()),
    };
    let identity = require_state_id_inode(&probe, marker_path)?;
    if probe.metadata()?.mode() & 0o7777 != STATE_ID_MODE {
        // Recover only a same-owner, single-link regular file whose mode is a
        // subset of the canonical marker mode. Atomic publication itself can
        // never expose this state, but older in-place writers could.
        chmod_path_descriptor(&probe, STATE_ID_MODE)?;
        require_state_id_inode(&probe, marker_path)?;
    }
    Ok(Some(identity))
}

fn prepare_state_id_temporary(
    usr: &StateMetadataDirectory,
    temporary_path: &Path,
) -> Result<(std::fs::File, (u64, u64)), Error> {
    loop {
        match open_state_metadata_at(
            usr.file.as_raw_fd(),
            Path::new(STATE_ID_TEMPORARY_NAME),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        ) {
            Ok(probe) => {
                require_state_id_temporary_inode(&probe, temporary_path)?;
                if probe.metadata()?.mode() & 0o7777 != STATE_ID_TEMPORARY_MODE {
                    chmod_path_descriptor(&probe, STATE_ID_TEMPORARY_MODE)?;
                }
                let identity = require_state_id_temporary_inode(&probe, temporary_path)?;
                let file = open_state_metadata_at(
                    usr.file.as_raw_fd(),
                    Path::new(STATE_ID_TEMPORARY_NAME),
                    nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                )?;
                if require_state_id_temporary_inode(&file, temporary_path)? != identity {
                    return Err(io::Error::other("state ID temporary was replaced before opening for write").into());
                }
                return Ok((file, identity));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                match open_state_metadata_at(
                    usr.file.as_raw_fd(),
                    Path::new(STATE_ID_TEMPORARY_NAME),
                    nix::libc::O_RDWR
                        | nix::libc::O_CLOEXEC
                        | nix::libc::O_CREAT
                        | nix::libc::O_EXCL
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_NONBLOCK,
                ) {
                    Ok(file) => {
                        require_state_id_temporary_inode(&file, temporary_path)?;
                        fchmod(file.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_TEMPORARY_MODE))?;
                        let identity = require_state_id_temporary_inode(&file, temporary_path)?;
                        return Ok((file, identity));
                    }
                    Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(source) => return Err(source.into()),
                }
            }
            Err(source) => return Err(source.into()),
        }
    }
}

fn require_expected_state_id_name(
    usr: &StateMetadataDirectory,
    expected: Option<(u64, u64)>,
    marker_path: &Path,
) -> Result<(), Error> {
    match (
        expected,
        open_state_metadata_at(
            usr.file.as_raw_fd(),
            Path::new(STATE_ID_NAME),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        ),
    ) {
        (None, Err(source)) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        (None, Ok(_)) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "state ID marker appeared before exclusive publication",
        )
        .into()),
        (None, Err(source)) => Err(source.into()),
        (Some(expected), Ok(file)) => {
            if require_state_id_inode(&file, marker_path)? == expected {
                Ok(())
            } else {
                Err(io::Error::other("state ID marker was replaced before atomic publication").into())
            }
        }
        (Some(_), Err(source)) => Err(source.into()),
    }
}

fn rename_state_id_temporary(directory: RawFd, replace: bool) -> io::Result<()> {
    let flags = if replace { 0 } else { RENAME_NOREPLACE };
    loop {
        // SAFETY: the retained directory and both static NUL-terminated names
        // remain live. Same-directory rename atomically replaces an existing
        // validated marker or exclusively publishes the first one.
        let result = unsafe {
            syscall(
                SYS_renameat2,
                directory,
                STATE_ID_TEMPORARY_C_NAME.as_ptr(),
                directory,
                STATE_ID_C_NAME.as_ptr(),
                flags,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn require_state_id_temporary_inode(file: &std::fs::File, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let recoverable_mode = mode & !STATE_ID_TEMPORARY_MODE == 0 || mode == STATE_ID_MODE;
    if metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && recoverable_mode
    {
        Ok((metadata.dev(), metadata.ino()))
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state ID temporary is not one recoverable owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_state_id_inode(file: &std::fs::File, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && mode & !STATE_ID_MODE == 0
    {
        Ok((metadata.dev(), metadata.ino()))
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state ID marker is not one safe independent owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_complete_state_id(
    file: &std::fs::File,
    path: &Path,
    expected_identity: (u64, u64),
    expected_length: u64,
) -> io::Result<()> {
    let metadata = file.metadata()?;
    if (metadata.dev(), metadata.ino()) == expected_identity
        && metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && metadata.mode() & 0o7777 == STATE_ID_MODE
        && metadata.len() == expected_length
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state ID marker metadata changed at {}", path.display()),
        ))
    }
}

fn truncate_state_id(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: file is a retained writable regular-file descriptor.
        if unsafe { nix::libc::ftruncate(file.as_raw_fd(), 0) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn write_state_id_bytes(file: &std::fs::File, contents: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written != contents.len() {
        match write(file.as_raw_fd(), &contents[written..]) {
            Ok(0) => return Err(io::Error::from_raw_os_error(nix::libc::EIO)),
            Ok(count) => written += count,
            Err(Errno::EINTR) => {}
            Err(source) => return Err(source.into()),
        }
    }
    Ok(())
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}

fn generate_system_snapshot(
    current: Option<LoadedSystemModel>,
    repositories: &repository::Manager,
    packages: &[Package],
) -> Result<SystemModel, Error> {
    let active_repos = repositories
        .active()
        .map(|repo| (repo.id, repo.repository))
        .collect::<repository::Map>();

    match current {
        // Update existing w/ incoming packages
        Some(existing) => SystemModel::try_from(existing)
            .map_err(system_model::UpdateError::from)?
            .sync_packages(packages)
            .map_err(Error::UpdateSystemModel),

        // Generate a fresh normalized state snapshot.
        None => {
            let packages = packages
                .iter()
                .map(|package| Provider::package_name(package.meta.name.as_str()))
                .collect();

            Ok(system_model::create(active_repos, packages))
        }
    }
}

#[cfg(test)]
fn record_system_snapshot(root: &Path, system_snapshot: SystemModel) -> Result<(), Error> {
    let path = system_model::snapshot_path(root);
    let dir = path.parent().expect("system snapshot path has a parent");
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    fs::write(&path, system_snapshot.encoded())?;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;

    Ok(())
}
