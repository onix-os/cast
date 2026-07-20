const MAX_PURGE_ENTRIES: usize = 1_000_000;
const MAX_PURGE_OPERATIONS: usize = 2_000_000;
const MAX_PURGE_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_PURGE_DEPTH: usize = 128;
const PURGE_TIMEOUT: Duration = Duration::from_secs(300);

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
pub(crate) struct BoundedTempTree {
    path: PathBuf,
    parent: StdFile,
    root: StdFile,
    name: CString,
    identity: (u64, u64),
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
impl BoundedTempTree {
    pub(crate) fn retain(path: &Path) -> io::Result<Self> {
        let absolute = std::path::absolute(path)?;
        let parent_path = absolute
            .parent()
            .ok_or_else(|| invalid_binding(format!("temporary tree has no parent: {absolute:?}")))?;
        let name = match absolute.file_name() {
            Some(name) if !name.is_empty() => CString::new(name.as_bytes())
                .map_err(|_| invalid_binding(format!("temporary tree name contains NUL: {absolute:?}")))?,
            _ => return Err(invalid_binding(format!("temporary tree has no normal leaf: {absolute:?}"))),
        };
        let parent = pin_workspace_root(parent_path)?;
        let pinned = open_path_child(&parent, &name)?;
        let metadata = pinned.metadata()?;
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        let owner = unsafe { nix::libc::geteuid() };
        if !metadata.file_type().is_dir()
            || metadata.uid() != owner
            || metadata.mode() & 0o7777 != 0o700
            || metadata.dev() != parent.metadata()?.dev()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "temporary tree is not one exact private same-device directory: {absolute:?} \
                     (uid={}, mode={:#06o}, device={})",
                    metadata.uid(),
                    metadata.mode() & 0o7777,
                    metadata.dev()
                ),
            ));
        }
        let root = open_private_child(&parent, &name)?;
        let identity = directory_identity(&pinned)?;
        if identity != directory_identity(&root)? {
            return Err(io::Error::other(format!(
                "temporary tree changed while its cleanup descriptor was retained: {absolute:?}"
            )));
        }
        Ok(Self {
            path: absolute,
            parent,
            root,
            name,
            identity,
        })
    }

    pub(crate) fn create_private_directory(&self, relative: &Path, display: &Path) -> io::Result<()> {
        ensure_private_directory_at(&self.root, relative, display).map(drop)
    }

    pub(crate) fn remove(self) -> io::Result<()> {
        self.require_attached()?;
        chmod_path_descriptor(&self.root, 0o700)?;
        let mut budget = PurgeBudget::new(&self.root)?;
        for child in sorted_directory_names(&self.root, &mut budget)? {
            purge_named_entry(&self.root, &child, &mut budget, 1, &self.path)?;
        }
        self.require_attached()?;
        budget.account(0, false)?;
        // SAFETY: parent and name remain live. The retained identity was
        // reauthenticated immediately above and the directory is now empty.
        if unsafe { nix::libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), nix::libc::AT_REMOVEDIR) } == -1 {
            return Err(io::Error::last_os_error());
        }
        match metadata_at(&self.parent, &self.name) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(source),
            Ok(_) => Err(io::Error::other(format!(
                "temporary tree remained named after bounded cleanup: {:?}",
                self.path
            ))),
        }
    }

    fn require_attached(&self) -> io::Result<()> {
        let metadata = metadata_at(&self.parent, &self.name)?;
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        let owner = unsafe { nix::libc::geteuid() };
        if metadata.st_dev == self.identity.0
            && metadata.st_ino == self.identity.1
            && metadata.st_uid == owner
            && metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR
        {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "temporary tree name no longer denotes its retained cleanup directory: {:?}",
                self.path
            )))
        }
    }
}

struct PurgeBudget {
    entries: usize,
    operations: usize,
    name_bytes: usize,
    deadline: Instant,
    device: u64,
}

impl PurgeBudget {
    fn new(root: &StdFile) -> io::Result<Self> {
        Ok(Self {
            entries: 0,
            operations: 0,
            name_bytes: 0,
            deadline: Instant::now() + PURGE_TIMEOUT,
            device: root.metadata()?.dev(),
        })
    }

    fn account(&mut self, name_bytes: usize, entry: bool) -> io::Result<()> {
        self.operations = self.operations.checked_add(1).ok_or_else(purge_limit_error)?;
        if entry {
            self.entries = self.entries.checked_add(1).ok_or_else(purge_limit_error)?;
            self.name_bytes = self.name_bytes.checked_add(name_bytes).ok_or_else(purge_limit_error)?;
        }
        if self.operations > MAX_PURGE_OPERATIONS
            || self.entries > MAX_PURGE_ENTRIES
            || self.name_bytes > MAX_PURGE_NAME_BYTES
            || Instant::now() > self.deadline
        {
            return Err(purge_limit_error());
        }
        Ok(())
    }
}

fn purge_limit_error() -> io::Error {
    io::Error::other("private host quarantine exceeds bounded cleanup limits")
}

fn purge_named_entry(
    parent: &StdFile,
    name: &CStr,
    budget: &mut PurgeBudget,
    depth: usize,
    display: &Path,
) -> io::Result<()> {
    budget.account(name.to_bytes().len(), false)?;
    let metadata = match metadata_at(parent, name) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source),
    };
    if metadata.st_dev != budget.device {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!("private host quarantine crosses a mount: {display:?}"),
        ));
    }
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR {
        require_purge_depth(depth)?;
        let directory = open_directory_for_purge(parent, name, budget.device, display)?;
        for child in sorted_directory_names(&directory, budget)? {
            purge_named_entry(&directory, &child, budget, depth + 1, display)?;
        }
        budget.account(0, false)?;
        // SAFETY: parent and name remain live; AT_REMOVEDIR removes only this
        // now-empty directory and never follows a link.
        if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        budget.account(0, false)?;
        // SAFETY: unlinkat with flags 0 removes the named non-directory entry;
        // a symlink is removed rather than followed.
        if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn require_purge_depth(depth: usize) -> io::Result<()> {
    if depth > MAX_PURGE_DEPTH {
        Err(purge_limit_error())
    } else {
        Ok(())
    }
}

fn metadata_at(parent: &StdFile, name: &CStr) -> io::Result<nix::libc::stat> {
    // SAFETY: all-zero stat is valid output storage and the arguments remain live.
    let mut metadata: nix::libc::stat = unsafe { zeroed() };
    // SAFETY: parent/name are valid and AT_SYMLINK_NOFOLLOW authenticates the
    // named entry itself.
    if unsafe {
        nix::libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut metadata,
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(metadata)
}

fn open_directory_for_purge(parent: &StdFile, name: &CStr, device: u64, display: &Path) -> io::Result<StdFile> {
    let pinned = open_path_child(parent, name)?;
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.dev() != device {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe directory inside private host quarantine: {display:?}"),
        ));
    }
    // The detached root is exact 0700, so arbitrary build-produced descendant
    // modes are no longer reachable through a shared path. Normalize each
    // pinned owned directory only to make the bounded cleanup walk possible.
    chmod_path_descriptor(&pinned, 0o700)?;
    let directory = open_private_child(parent, name)?;
    if directory_identity(&pinned)? != directory_identity(&directory)? {
        return Err(io::Error::other(format!(
            "quarantine directory changed during cleanup: {display:?}"
        )));
    }
    Ok(directory)
}

fn sorted_directory_names(directory: &StdFile, budget: &mut PurgeBudget) -> io::Result<Vec<CString>> {
    let cursor = open_private_child(directory, c".")?;
    let raw = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(raw) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the descriptor.
        unsafe { nix::libc::close(raw) };
        return Err(source);
    }
    let mut names = Vec::new();
    let result = (|| -> io::Result<()> {
        loop {
            // SAFETY: this process targets Linux and owns the DIR stream.
            unsafe { *nix::libc::__errno_location() = 0 };
            // SAFETY: stream remains valid until closed below.
            let entry = unsafe { nix::libc::readdir(stream) };
            if entry.is_null() {
                // SAFETY: errno is thread-local.
                let errno = unsafe { *nix::libc::__errno_location() };
                if errno != 0 {
                    return Err(io::Error::from_raw_os_error(errno));
                }
                break;
            }
            // SAFETY: d_name is NUL-terminated for the live dirent.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            budget.account(name.to_bytes().len(), true)?;
            names.push(name.to_owned());
        }
        Ok(())
    })();
    // SAFETY: closedir consumes and closes the descriptor held by stream.
    let close_result = unsafe { nix::libc::closedir(stream) };
    result?;
    if close_result == -1 {
        return Err(io::Error::last_os_error());
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}
