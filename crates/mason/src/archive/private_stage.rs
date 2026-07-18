struct PrivateStage {
    parent: File,
    root: File,
    name: CString,
    active: bool,
}

impl PrivateStage {
    fn create(parent: File) -> Result<Self, Error> {
        static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(".cast-archive-stage-{}-{sequence}", std::process::id()))
                .expect("stage name has no NUL");
            if !mkdirat_directory(&parent, &name, 0o700, "create private archive stage")? {
                continue;
            }
            let opened = (|| -> Result<File, Error> {
                inject_test_stage_open_failure()?;
                let path = openat2(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )
                .map_err(|source| Error::DescriptorOperation {
                    operation: "pin private archive stage",
                    source,
                })?;
                let path = unsafe { File::from_raw_fd(path) };
                validate_directory(&path, "pin private archive stage")?;
                open_and_normalize_archive_directory(&parent, &name, &path, true, "open private archive stage")
            })();
            let root = match opened {
                Ok(root) => root,
                Err(failure) => {
                    let cleanup = remove_empty_stage(&parent, &name);
                    return match cleanup {
                        Ok(()) => Err(failure),
                        Err(cleanup) => Err(Error::CleanupAfterFailure {
                            failure: Box::new(failure),
                            cleanup,
                        }),
                    };
                }
            };
            return Ok(Self {
                parent,
                root,
                name,
                active: true,
            });
        }
        Err(Error::StageNameExhausted)
    }

    fn root(&self) -> &File {
        &self.root
    }

    fn publish(&mut self, destination: &[u8]) -> Result<(), Error> {
        self.root.sync_all()?;
        let destination = CString::new(destination).map_err(|_| Error::InteriorNul)?;
        let removed = unsafe { libc::unlinkat(self.parent.as_raw_fd(), destination.as_ptr(), libc::AT_REMOVEDIR) };
        if removed == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::NotFound {
                return Err(Error::DestinationNotEmpty { source });
            }
        }
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                self.parent.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == -1 {
            return Err(Error::DescriptorOperation {
                operation: "publish verified archive stage",
                source: io::Error::last_os_error(),
            });
        }
        // The rename is the irreversible publication point. A later durability
        // error must not run stage-name cleanup against a name which no longer
        // owns this descriptor or destructively empty the published tree.
        self.active = false;
        inject_test_publish_failure_after_rename()?;
        self.parent.sync_all()?;
        Ok(())
    }

    fn discard(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut budget = StageCleanupBudget::new(self.root.metadata()?.dev());
        purge_directory_contents(&self.root, &mut budget, 0)?;
        budget.operation()?;
        let result = unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        self.parent.sync_all()?;
        self.active = false;
        Ok(())
    }
}

fn remove_empty_stage(parent: &File, name: &CStr) -> io::Result<()> {
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Drop for PrivateStage {
    fn drop(&mut self) {
        if self.active {
            // Only an empty failed stage can be removed without walking
            // attacker-authored entries. Explicit error paths call the bounded
            // descriptor purge above; this is only a last-resort unwind guard.
            unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR);
            }
        }
    }
}

const STAGE_CLEANUP_MAX_ENTRIES: u64 = 1_000_000;
const STAGE_CLEANUP_MAX_OPERATIONS: u64 = 2_000_000;
const STAGE_CLEANUP_MAX_NAME_BYTES: u64 = 64 * MIB;
const STAGE_CLEANUP_MAX_DEPTH: usize = 128;
const STAGE_CLEANUP_WALL_TIME: Duration = Duration::from_secs(300);

struct StageCleanupBudget {
    entries: u64,
    operations: u64,
    name_bytes: u64,
    device: u64,
    deadline: Instant,
}

impl StageCleanupBudget {
    fn new(device: u64) -> Self {
        Self {
            entries: 0,
            operations: 0,
            name_bytes: 0,
            device,
            deadline: Instant::now() + STAGE_CLEANUP_WALL_TIME,
        }
    }

    fn entry(&mut self, name_bytes: usize) -> io::Result<()> {
        self.entries = self.entries.checked_add(1).ok_or_else(cleanup_limit_error)?;
        self.name_bytes = self
            .name_bytes
            .checked_add(u64::try_from(name_bytes).map_err(|_| cleanup_limit_error())?)
            .ok_or_else(cleanup_limit_error)?;
        self.operation()?;
        self.require_limits()
    }

    fn operation(&mut self) -> io::Result<()> {
        self.operations = self.operations.checked_add(1).ok_or_else(cleanup_limit_error)?;
        self.require_limits()
    }

    fn require_limits(&self) -> io::Result<()> {
        if self.entries > STAGE_CLEANUP_MAX_ENTRIES
            || self.operations > STAGE_CLEANUP_MAX_OPERATIONS
            || self.name_bytes > STAGE_CLEANUP_MAX_NAME_BYTES
            || Instant::now() > self.deadline
        {
            Err(cleanup_limit_error())
        } else {
            Ok(())
        }
    }
}

fn cleanup_limit_error() -> io::Error {
    io::Error::other("private archive stage exceeds bounded cleanup limits")
}

fn purge_directory_contents(directory: &File, budget: &mut StageCleanupBudget, depth: usize) -> io::Result<()> {
    if depth > STAGE_CLEANUP_MAX_DEPTH {
        return Err(cleanup_limit_error());
    }
    for name in sorted_directory_names(directory, budget)? {
        purge_named_entry(directory, &name, budget, depth + 1)?;
    }
    Ok(())
}

fn purge_named_entry(parent: &File, name: &CStr, budget: &mut StageCleanupBudget, depth: usize) -> io::Result<()> {
    budget.operation()?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(source)
        };
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_dev != budget.device {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "private archive stage crosses a mount boundary",
        ));
    }
    if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
        if depth > STAGE_CLEANUP_MAX_DEPTH {
            return Err(cleanup_limit_error());
        }
        let fd = openat2(
            parent.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?;
        let child = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(child.as_raw_fd(), 0o700) } == -1 {
            return Err(io::Error::last_os_error());
        }
        purge_directory_contents(&child, budget, depth)?;
        budget.operation()?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        budget.operation()?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn sorted_directory_names(directory: &File, budget: &mut StageCleanupBudget) -> io::Result<Vec<CString>> {
    let fd = openat2(
        directory.as_raw_fd(),
        c".",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let cursor = unsafe { File::from_raw_fd(fd) };
    let raw = cursor.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        unsafe { libc::close(raw) };
        return Err(source);
    }
    let result = (|| -> io::Result<Vec<CString>> {
        let mut names = Vec::new();
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = unsafe { *libc::__errno_location() };
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            budget.entry(name.to_bytes().len())?;
            names.push(name.to_owned());
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        Ok(names)
    })();
    let close = unsafe { libc::closedir(stream) };
    let names = result?;
    if close == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(names)
    }
}

fn require_limit(resource: &'static str, actual: u64, limit: u64) -> Result<(), Error> {
    if actual <= limit {
        Ok(())
    } else {
        Err(Error::LimitExceeded {
            resource,
            actual,
            limit,
        })
    }
}
fn aggregate_total(resource: &'static str, total: u64, value: u64, limit: u64) -> Result<u64, Error> {
    let next = total.checked_add(value).ok_or(Error::ArithmeticOverflow)?;
    require_limit(resource, next, limit)?;
    Ok(next)
}

fn require_usize_limit(resource: &'static str, actual: usize, limit: usize) -> Result<(), Error> {
    if actual <= limit {
        Ok(())
    } else {
        Err(Error::LimitExceeded {
            resource,
            actual: u64::try_from(actual).unwrap_or(u64::MAX),
            limit: u64::try_from(limit).unwrap_or(u64::MAX),
        })
    }
}
