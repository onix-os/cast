use super::*;

#[derive(Debug)]
struct OpenedLiveUsr {
    pinned: std::fs::File,
    readable: std::fs::File,
}

pub(super) fn open_retained_exchange_tree(parent: &std::fs::File, path: &Path) -> Result<TreeMarkerStore, Error> {
    let tree = openat2_file(
        parent.as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| retained_exchange_io("open retained /usr exchange child", path, source))?;
    TreeMarkerStore::open(&tree, path).map_err(Error::from)
}

pub(super) fn open_optional_retained_tree(
    parent: &RetainedDirectory,
    path: &Path,
) -> Result<Option<TreeMarkerStore>, Error> {
    let tree = match openat2_file(
        parent.file.as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    ) {
        Ok(tree) => tree,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(previous_move_io("open retained previous-tree child", path, source)),
    };
    TreeMarkerStore::open(&tree, path).map(Some).map_err(Error::from)
}

pub(super) fn canonical_state_name(state: state::Id) -> Result<std::ffi::CString, Error> {
    let value = i32::from(state);
    if value <= 0 {
        return Err(Error::InvalidPreviousArchiveState { state: value });
    }
    let encoded = value.to_string();
    if encoded.starts_with('0') || !encoded.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::InvalidPreviousArchiveState { state: value });
    }
    std::ffi::CString::new(encoded).map_err(|_| Error::InvalidPreviousArchiveState { state: value })
}

pub(super) fn previous_slot_parking_name(
    state: state::Id,
    previous_tree_token: &str,
    index: usize,
) -> Result<std::ffi::CString, Error> {
    let name = QuarantineName::parse(format!(
        ".previous-slot-{}-{previous_tree_token}-{index}",
        i32::from(state)
    ))
    .map_err(Error::InvalidPreviousArchiveParkingName)?;
    Ok(std::ffi::CString::new(name.as_str()).expect("validated previous-slot parking name contains no NUL"))
}

pub(super) fn previous_slot_canonical_path(attempt: &RetainedPreviousArchiveAttempt) -> PathBuf {
    attempt.roots.path.join(attempt.name.to_string_lossy().as_ref())
}

pub(super) fn previous_slot_parking_path(attempt: &RetainedPreviousArchiveAttempt) -> PathBuf {
    attempt.roots.path.join(attempt.parking_name.to_string_lossy().as_ref())
}

pub(super) fn require_previous_attempt_name(
    attempt: &RetainedPreviousArchiveAttempt,
    state: state::Id,
    name: &CStr,
) -> Result<(), Error> {
    if attempt.name.as_c_str() == name {
        Ok(())
    } else {
        Err(Error::PreviousArchiveAttemptChanged {
            expected: attempt.name.to_string_lossy().into_owned(),
            actual: i32::from(state).to_string(),
        })
    }
}

pub(super) fn open_or_synthesize_live_usr(installation: &Installation) -> Result<TreeMarkerStore, Error> {
    let path = installation.root.join("usr");
    installation.revalidate_root_directory()?;
    if let Some(opened) = open_live_usr(installation, &path)? {
        require_named_live_usr(installation, &opened.pinned, &path)?;
        let store = TreeMarkerStore::open(&opened.readable, &path)?;
        // With no active-state evidence, an existing nonempty tree is neither
        // the synthesized empty baseline nor an authenticated legacy active
        // tree. Refuse to bless it with a permanent token.
        if installation.active_state.is_none() {
            require_no_access_acl(&opened.readable, &path)
                .map_err(|source| live_usr_io("reject access ACL on unowned live /usr", &path, source))?;
            let marker_only = require_empty_or_marker_only_directory(&opened.readable, &path)?;
            if marker_only {
                // A failed first-install attempt may already have durably
                // published the baseline marker. Validate and adopt that exact
                // evidence rather than permanently making the next attempt
                // reject its own marker.
                store.read_for_recovery()?;
            }
            opened
                .readable
                .sync_all()
                .map_err(|source| live_usr_io("sync pre-existing empty live /usr", &path, source))?;
            installation
                .root_directory()
                .sync_all()
                .map_err(|source| live_usr_io("sync pre-existing live /usr name", &path, source))?;
        }
        require_named_live_usr(installation, &opened.pinned, &path)?;
        installation.revalidate_root_directory()?;
        return Ok(store);
    }

    before_live_usr_mkdir();
    loop {
        // SAFETY: the retained root descriptor and static component remain
        // live. mkdirat never follows or replaces the final component.
        if unsafe {
            nix::libc::mkdirat(
                installation.root_directory().as_raw_fd(),
                LIVE_USR_NAME.as_ptr(),
                SYNTHESIZED_USR_MODE,
            )
        } == 0
        {
            break;
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::AlreadyExists => return Err(Error::LiveUsrAppeared { path }),
            _ => return Err(live_usr_io("create empty live /usr", &path, source)),
        }
    }

    let opened = open_live_usr(installation, &path)?.ok_or_else(|| Error::LiveUsrDisappeared { path: path.clone() })?;
    require_fresh_synthesized_usr(&opened.readable, &path)?;
    chmod_path_descriptor(&opened.pinned, SYNTHESIZED_USR_MODE)
        .map_err(|source| live_usr_io("normalize empty live /usr mode", &path, source))?;
    require_exact_synthesized_usr(&opened.readable, &path)?;

    // Persist the empty child and its name before a marker can be generated.
    opened
        .readable
        .sync_all()
        .map_err(|source| live_usr_io("sync empty live /usr", &path, source))?;
    installation
        .root_directory()
        .sync_all()
        .map_err(|source| live_usr_io("sync installation root after live /usr creation", &path, source))?;
    require_named_live_usr(installation, &opened.pinned, &path)?;
    let reopened =
        open_live_usr(installation, &path)?.ok_or_else(|| Error::LiveUsrDisappeared { path: path.clone() })?;
    require_same_directory(&opened.pinned, &reopened.pinned, &path)?;
    require_exact_synthesized_usr(&reopened.readable, &path)?;
    reopened
        .readable
        .sync_all()
        .map_err(|source| live_usr_io("resync authenticated empty live /usr", &path, source))?;
    installation
        .root_directory()
        .sync_all()
        .map_err(|source| live_usr_io("resync authenticated installation root", &path, source))?;
    installation.revalidate_root_directory()?;

    let store = TreeMarkerStore::open(&reopened.readable, &path)?;
    require_named_live_usr(installation, &opened.pinned, &path)?;
    Ok(store)
}

fn open_live_usr(installation: &Installation, path: &Path) -> Result<Option<OpenedLiveUsr>, Error> {
    let pinned = match openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(live_usr_io("pin live /usr", path, source)),
    };
    let readable = openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("open live /usr", path, source))?;
    require_same_directory(&pinned, &readable, path)?;
    Ok(Some(OpenedLiveUsr { pinned, readable }))
}

pub(super) fn require_named_live_usr(
    installation: &Installation,
    retained: &std::fs::File,
    path: &Path,
) -> Result<(), Error> {
    installation.revalidate_root_directory()?;
    let named = openat2_file(
        installation.root_directory().as_raw_fd(),
        LIVE_USR_NAME,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("revalidate live /usr name", path, source))?;
    require_same_directory(retained, &named, path)
}

pub(super) fn require_fresh_synthesized_usr(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| live_usr_io("inspect fresh empty live /usr", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode & !SYNTHESIZED_USR_MODE != 0 {
        return Err(Error::UnsafeSynthesizedUsr {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }
    require_no_access_acl(file, path)
        .map_err(|source| live_usr_io("reject access ACL on empty live /usr", path, source))?;
    require_no_default_acl(file, path)
        .map_err(|source| live_usr_io("reject default ACL on empty live /usr", path, source))?;
    require_empty_directory(file, path)
}

pub(super) fn require_exact_synthesized_usr(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| live_usr_io("inspect normalized empty live /usr", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode != SYNTHESIZED_USR_MODE {
        return Err(Error::UnsafeSynthesizedUsr {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }
    require_no_access_acl(file, path)
        .map_err(|source| live_usr_io("reject access ACL on normalized live /usr", path, source))?;
    require_no_default_acl(file, path)
        .map_err(|source| live_usr_io("reject default ACL on normalized live /usr", path, source))?;
    require_empty_directory(file, path)
}

pub(super) fn require_empty_directory(file: &std::fs::File, path: &Path) -> Result<(), Error> {
    inspect_baseline_directory(file, path, false).map(drop)
}

/// Return true only for the exact marker-only retry baseline. Every other
/// entry, including marker temporaries and a marker plus foreign content,
/// fails closed without cleanup.
pub(super) fn require_empty_or_marker_only_directory(file: &std::fs::File, path: &Path) -> Result<bool, Error> {
    inspect_baseline_directory(file, path, true)
}

pub(super) fn inspect_baseline_directory(file: &std::fs::File, path: &Path, allow_marker: bool) -> Result<bool, Error> {
    // SAFETY: fcntl receives one live directory descriptor and returns a fresh
    // close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(live_usr_io(
            "duplicate empty live /usr for enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // dup shares a directory offset with the retained descriptor. Reset it so
    // repeated emptiness proofs never mistake a prior EOF for a new scan.
    // SAFETY: duplicate is one fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: duplicate is still uniquely owned here.
        unsafe { nix::libc::close(duplicate) };
        return Err(live_usr_io("rewind empty live /usr enumeration", path, source));
    }
    // SAFETY: fdopendir consumes the fresh duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(live_usr_io("enumerate empty live /usr", path, source));
    }

    let mut marker_seen = false;
    let result = loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(marker_seen)
            } else {
                Err(live_usr_io("enumerate empty live /usr", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for this live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = name.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if allow_marker && name == TREE_MARKER_NAME && !marker_seen {
            marker_seen = true;
        } else {
            break Err(Error::LiveUsrNotEmpty {
                path: path.to_owned(),
                entry: String::from_utf8_lossy(name).into_owned(),
            });
        }
    };
    // SAFETY: stream was returned by fdopendir and remains open.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(live_usr_io(
            "close empty live /usr enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

pub(super) fn require_same_directory(
    expected: &std::fs::File,
    actual: &std::fs::File,
    path: &Path,
) -> Result<(), Error> {
    let expected = expected
        .metadata()
        .map_err(|source| live_usr_io("inspect retained live /usr", path, source))?;
    let actual = actual
        .metadata()
        .map_err(|source| live_usr_io("inspect reopened live /usr", path, source))?;
    if (expected.dev(), expected.ino()) == (actual.dev(), actual.ino()) {
        Ok(())
    } else {
        Err(Error::LiveUsrChanged { path: path.to_owned() })
    }
}

pub(super) fn retained_directory_witness(file: &std::fs::File, path: &Path) -> Result<RetainedDirectoryWitness, Error> {
    let metadata = file
        .metadata()
        .map_err(|source| quarantine_io("inspect retained directory", path, source))?;
    let witness = RetainedDirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
    };
    if metadata.file_type().is_dir()
        && witness.owner == unsafe { nix::libc::geteuid() }
        && witness.mode & 0o7000 == 0
        && witness.mode & 0o022 == 0
        && witness.mode & 0o700 == 0o700
    {
        Ok(witness)
    } else {
        Err(Error::UnsafeQuarantineDirectory {
            path: path.to_owned(),
            owner: witness.owner,
            mode: witness.mode,
        })
    }
}

pub(super) fn retained_directory_entries(
    file: &std::fs::File,
    path: &Path,
    limit: usize,
) -> Result<Vec<Vec<u8>>, Error> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(quarantine_io(
            "duplicate retained directory for enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: duplicate is a fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: duplicate remains uniquely owned here.
        unsafe { nix::libc::close(duplicate) };
        return Err(quarantine_io("rewind retained directory enumeration", path, source));
    }
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(quarantine_io("enumerate retained directory", path, source));
    }

    let mut entries = Vec::new();
    let result = loop {
        // SAFETY: errno is thread-local on Linux.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(entries)
            } else {
                Err(quarantine_io("enumerate retained directory", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for the returned dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        entries.push(name.to_vec());
        if entries.len() > limit {
            break Err(Error::UnexpectedQuarantineEntries {
                path: path.to_owned(),
                entries: entries
                    .into_iter()
                    .map(|name| String::from_utf8_lossy(&name).into_owned())
                    .collect(),
            });
        }
    };
    // SAFETY: stream was returned by fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(quarantine_io(
            "close retained directory enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}
