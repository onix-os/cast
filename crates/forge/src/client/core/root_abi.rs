const ROOT_ABI_LINKS: [(&str, &str); 5] = [
    ("usr/sbin", "sbin"),
    ("usr/bin", "bin"),
    ("usr/lib", "lib"),
    ("usr/lib", "lib64"),
    ("usr/lib32", "lib32"),
];

/// Establish the stable merged-/usr root ABI links without replacing anything.
fn create_root_links(root: &Path) -> Result<RetainedRootAbi, Error> {
    create_root_links_with(root, |_| {}, |directory| directory.sync_all())
}

/// Establish the merged-/usr ABI through an already-retained root descriptor.
/// The path is diagnostic and is used only to prove that the retained inode is
/// still publicly named; no write authority is reacquired through it.
pub(super) fn create_root_links_retained(root: &Path, retained: &std::fs::File) -> Result<RetainedRootAbi, Error> {
    RootAbiPreflight::open_retained(root, retained)?.publish()
}

/// Inspect every stable and legacy staging name without mutating the root.
///
/// Stateful activation performs this half before candidate identity preparation
/// so a static foreign occupant leaves the already-materialized candidate and
/// its allocated database row unchanged.
/// Publication is deliberately separate: it runs only after the canonical
/// journal guard has proved a clean baseline, using this same retained root
/// descriptor rather than reopening the public pathname.
fn preflight_root_links(root: &Path) -> Result<RootAbiPreflight, Error> {
    RootAbiPreflight::open_with(root, &mut |_| {})
}

#[derive(Debug)]
struct RootAbiPreflight {
    root: PathBuf,
    directory: fs::File,
    identity: FrozenRootIdentity,
    links: Vec<(&'static str, &'static str, Option<PinnedRootAbiLink>)>,
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_STATEFUL_CANDIDATE_METADATA: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STATEFUL_ROOT_ABI_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_STATEFUL_ISOLATION_ROOT_RETENTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_stateful_candidate_metadata(hook: impl FnOnce() + 'static) {
    BEFORE_STATEFUL_CANDIDATE_METADATA.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_stateful_candidate_metadata() {
    BEFORE_STATEFUL_CANDIDATE_METADATA.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn arm_before_stateful_root_abi_publication(hook: impl FnOnce() + 'static) {
    BEFORE_STATEFUL_ROOT_ABI_PUBLICATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_stateful_root_abi_publication() {
    BEFORE_STATEFUL_ROOT_ABI_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn arm_after_stateful_isolation_root_retention(hook: impl FnOnce() + 'static) {
    AFTER_STATEFUL_ISOLATION_ROOT_RETENTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_stateful_isolation_root_retention() {
    AFTER_STATEFUL_ISOLATION_ROOT_RETENTION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

impl RootAbiPreflight {
    fn path(&self) -> &Path {
        &self.root
    }

    fn revalidate(&self) -> Result<(), Error> {
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        for (source, target, pinned) in &self.links {
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            match pinned {
                Some(pinned) => {
                    require_pinned_root_abi_link(&self.directory, &self.root, source, target, pinned)?;
                }
                None => {
                    if open_root_abi_entry(&self.directory, &self.root, target)?.is_some() {
                        return Err(Error::RootAbiLinkAppeared(self.root.join(target)));
                    }
                }
            }
        }
        require_root_abi_directory(&self.root, &self.directory, self.identity)
    }
}

/// Exact merged-/usr scratch-root capability established while the ABI links
/// are provisioned. Transaction containers must consume this retained inode;
/// reopening the public path would admit a replacement root after validation.
#[derive(Debug)]
pub(super) struct RetainedRootAbi {
    root: PathBuf,
    directory: fs::File,
    anchor: std::fs::File,
    identity: FrozenRootIdentity,
    links: Vec<(&'static str, &'static str, PinnedRootAbiLink)>,
}

impl RetainedRootAbi {
    pub(super) fn path(&self) -> &Path {
        &self.root
    }

    pub(super) fn directory(&self) -> &std::fs::File {
        &self.anchor
    }

    pub(super) fn revalidate(&self) -> Result<(), Error> {
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        let anchor = self
            .anchor
            .metadata()
            .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
            .map_err(|source| Error::StatRootAbiDirectory {
                root: self.root.clone(),
                source,
            })?;
        if anchor != self.identity {
            return Err(Error::RootAbiDirectoryReplaced(self.root.clone()));
        }
        for (source, target, link) in &self.links {
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            require_pinned_root_abi_link(&self.directory, &self.root, source, target, link)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootAbiLinkCheckpoint {
    RootOpened,
    PreflightComplete,
    AfterSync,
}

fn create_root_links_with<C, S>(root: &Path, mut checkpoint: C, mut sync: S) -> Result<RetainedRootAbi, Error>
where
    C: FnMut(RootAbiLinkCheckpoint),
    S: FnMut(&fs::File) -> io::Result<()>,
{
    RootAbiPreflight::open_with(root, &mut checkpoint)?.publish_with(&mut checkpoint, &mut sync)
}

impl RootAbiPreflight {
    fn open_with<C>(root: &Path, checkpoint: &mut C) -> Result<Self, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
    {
        let requested = absolute_root_abi_path(root)?;
        let requested_directory = open_root_abi_directory(&requested).map_err(|source| Error::OpenRootAbiDirectory {
            root: requested.clone(),
            source,
        })?;
        let root = normalized_root_abi_path(&requested)?;
        let directory = open_root_abi_directory(&root).map_err(|source| Error::OpenRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        require_same_root_abi_directory(&requested_directory, &directory, &root)?;
        Self::open_directory_with(root, directory, checkpoint)
    }

    fn open_retained(root: &Path, retained: &std::fs::File) -> Result<Self, Error> {
        let requested = absolute_root_abi_path(root)?;
        let root = normalized_root_abi_path(&requested)?;
        let retained = retained.try_clone().map_err(|source| Error::OpenRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        let directory = fs::File::from_parts(retained, root.clone());
        let requested_directory = open_root_abi_directory(&requested).map_err(|source| Error::OpenRootAbiDirectory {
            root: requested,
            source,
        })?;
        let normalized_directory = open_root_abi_directory(&root).map_err(|source| Error::OpenRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        require_same_root_abi_directory(&directory, &requested_directory, &root)?;
        require_same_root_abi_directory(&directory, &normalized_directory, &root)?;
        Self::open_directory_with(root, directory, &mut |_| {})
    }

    fn open_directory_with<C>(root: PathBuf, directory: fs::File, checkpoint: &mut C) -> Result<Self, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
    {
        let identity = root_abi_directory_identity(&directory).map_err(|source| Error::StatRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        checkpoint(RootAbiLinkCheckpoint::RootOpened);

        let mut links = Vec::with_capacity(ROOT_ABI_LINKS.len());
        for (source, target) in ROOT_ABI_LINKS {
            require_root_abi_staging_absent(&directory, &root, target)?;
            links.push((
                source,
                target,
                pin_root_abi_link(&directory, &root, source, target, true)?,
            ));
        }
        checkpoint(RootAbiLinkCheckpoint::PreflightComplete);

        Ok(Self {
            root,
            directory,
            identity,
            links,
        })
    }

    fn publish(self) -> Result<RetainedRootAbi, Error> {
        #[cfg(test)]
        before_stateful_root_abi_publication();
        self.publish_with(&mut |_| {}, &mut |directory| directory.sync_all())
    }

    fn publish_with<C, S>(mut self, checkpoint: &mut C, sync: &mut S) -> Result<RetainedRootAbi, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
        S: FnMut(&fs::File) -> io::Result<()>,
    {
        for (source, target, pinned) in &mut self.links {
            let source = *source;
            let target = *target;
            if pinned.is_some() {
                continue;
            }
            match symlinkat(source, Some(self.directory.as_raw_fd()), target) {
                Ok(()) => {}
                Err(Errno::EEXIST) => {
                    // A concurrent creator is authenticated by the common pin
                    // below. Never replace or remove what won the race.
                }
                Err(error) => {
                    return Err(Error::CreateRootAbiLink {
                        path: self.root.join(target),
                        target: source.to_owned(),
                        source: io::Error::from_raw_os_error(error as i32),
                    });
                }
            }
            *pinned = pin_root_abi_link(&self.directory, &self.root, source, target, false)?;
        }

        // Always sync, including an idempotent no-op retry after a prior sync
        // failure, so every successful return is a durability boundary.
        sync(&self.directory).map_err(|source| Error::SyncRootAbiDirectory {
            root: self.root.clone(),
            source,
        })?;
        checkpoint(RootAbiLinkCheckpoint::AfterSync);

        // Revalidate the complete namespace after publication and sync. This
        // also detects `.next` or final-name races without cleaning them up.
        for (source, target, pinned) in &self.links {
            let source = *source;
            let target = *target;
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            require_pinned_root_abi_link(
                &self.directory,
                &self.root,
                source,
                target,
                pinned.as_ref().expect("every root ABI link was pinned before sync"),
            )?;
        }
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        let anchor = openat2_file(
            self.directory.as_raw_fd(),
            c".",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            crate::linux_fs::controlled_resolution(),
        )
        .map_err(|source| Error::OpenRootAbiDirectory {
            root: self.root.clone(),
            source,
        })?;
        if anchor
            .metadata()
            .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
            .map_err(|source| Error::StatRootAbiDirectory {
                root: self.root.clone(),
                source,
            })?
            != self.identity
        {
            return Err(Error::RootAbiDirectoryReplaced(self.root));
        }
        Ok(RetainedRootAbi {
            root: self.root,
            directory: self.directory,
            anchor,
            identity: self.identity,
            links: self
                .links
                .into_iter()
                .map(|(source, target, link)| {
                    (
                        source,
                        target,
                        link.expect("every root ABI link was pinned before retention"),
                    )
                })
                .collect(),
        })
    }
}

fn absolute_root_abi_path(root: &Path) -> Result<PathBuf, Error> {
    if root.is_absolute() {
        Ok(root.to_owned())
    } else {
        std::env::current_dir()
            .map_err(|source| Error::OpenRootAbiDirectory {
                root: root.to_owned(),
                source,
            })
            .map(|current| current.join(root))
    }
}

fn normalized_root_abi_path(absolute: &Path) -> Result<PathBuf, Error> {
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if normalized != Path::new("/") {
                    normalized.pop();
                }
            }
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::Prefix(_) => {
                return Err(Error::OpenRootAbiDirectory {
                    root: absolute.to_owned(),
                    source: io::Error::new(io::ErrorKind::InvalidInput, "root ABI has a non-Unix path prefix"),
                });
            }
        }
    }
    Ok(normalized)
}

fn require_same_root_abi_directory(first: &fs::File, second: &fs::File, root: &Path) -> Result<(), Error> {
    let first = root_abi_directory_identity(first).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    let second = root_abi_directory_identity(second).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    if first == second {
        Ok(())
    } else {
        Err(Error::RootAbiDirectoryReplaced(root.to_owned()))
    }
}

fn open_root_abi_directory(root: &Path) -> io::Result<fs::File> {
    openat2_frozen(
        AT_FDCWD,
        root,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    )
}

fn open_root_abi_entry(directory: &fs::File, root: &Path, name: &str) -> Result<Option<fs::File>, Error> {
    match openat2_frozen(
        directory.as_raw_fd(),
        Path::new(name),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    ) {
        Ok(entry) => Ok(Some(entry)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::InspectRootAbiEntry {
            path: root.join(name),
            source,
        }),
    }
}

fn require_root_abi_staging_absent(directory: &fs::File, root: &Path, target: &str) -> Result<(), Error> {
    let name = format!("{target}.next");
    let Some(entry) = open_root_abi_entry(directory, root, &name)? else {
        return Ok(());
    };
    let metadata = entry.metadata().map_err(|source| Error::InspectRootAbiEntry {
        path: root.join(&name),
        source,
    })?;
    let symlink_target = metadata
        .file_type()
        .is_symlink()
        .then(|| read_root_abi_symlink(&entry, &root.join(&name)))
        .transpose()?;
    Err(Error::RootAbiStagingConflict {
        path: root.join(name),
        actual_type: root_abi_entry_type(metadata.mode()),
        symlink_target,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RootAbiLinkWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl RootAbiLinkWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct PinnedRootAbiLink {
    entry: fs::File,
    witness: RootAbiLinkWitness,
}

/// Return a retained exact link, or `None` only for an allowed absence.
fn pin_root_abi_link(
    directory: &fs::File,
    root: &Path,
    source: &'static str,
    target: &'static str,
    allow_missing: bool,
) -> Result<Option<PinnedRootAbiLink>, Error> {
    let path = root.join(target);
    let Some(entry) = open_root_abi_entry(directory, root, target)? else {
        return if allow_missing {
            Ok(None)
        } else {
            Err(Error::RootAbiLinkMissing {
                path,
                target: source.to_owned(),
            })
        };
    };
    let metadata = entry.metadata().map_err(|source| Error::InspectRootAbiEntry {
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_symlink() {
        return Err(Error::RootAbiLinkTypeConflict {
            path,
            target: source.to_owned(),
            actual_type: root_abi_entry_type(metadata.mode()),
        });
    }
    let actual = read_root_abi_symlink(&entry, &path)?;
    if actual.as_bytes() != source.as_bytes() {
        return Err(Error::RootAbiLinkTargetConflict {
            path,
            expected: source.to_owned(),
            actual,
        });
    }
    Ok(Some(PinnedRootAbiLink {
        witness: RootAbiLinkWitness::from_metadata(&metadata),
        entry,
    }))
}

fn require_pinned_root_abi_link(
    directory: &fs::File,
    root: &Path,
    source: &'static str,
    target: &'static str,
    expected: &PinnedRootAbiLink,
) -> Result<(), Error> {
    let path = root.join(target);
    let retained = expected
        .entry
        .metadata()
        .map(|metadata| RootAbiLinkWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectRootAbiEntry {
            path: path.clone(),
            source,
        })?;
    let named =
        pin_root_abi_link(directory, root, source, target, false)?.expect("a required root ABI link cannot be absent");
    if retained != expected.witness || named.witness != expected.witness {
        return Err(Error::RootAbiLinkReplaced(path));
    }
    Ok(())
}

fn read_root_abi_symlink(entry: &fs::File, path: &Path) -> Result<OsString, Error> {
    let mut target = vec![0_u8; nix::libc::PATH_MAX as usize + 1];
    // O_PATH|O_NOFOLLOW pins the symlink inode; an empty readlinkat path reads
    // that exact inode rather than resolving its public name a second time.
    // SAFETY: `entry` is live and `target` is writable for its full length.
    let read = unsafe {
        nix::libc::readlinkat(
            entry.as_raw_fd(),
            c"".as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if read < 0 {
        return Err(Error::ReadRootAbiLink {
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadRootAbiLink {
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read == target.len() {
        return Err(Error::RootAbiLinkTargetTooLong {
            path: path.to_owned(),
            limit: target.len() - 1,
        });
    }
    target.truncate(read);
    Ok(OsString::from_vec(target))
}

fn root_abi_entry_type(mode: u32) -> &'static str {
    match mode & nix::libc::S_IFMT {
        nix::libc::S_IFREG => "regular file",
        nix::libc::S_IFDIR => "directory",
        nix::libc::S_IFLNK => "symlink",
        nix::libc::S_IFIFO => "fifo",
        nix::libc::S_IFSOCK => "socket",
        nix::libc::S_IFCHR => "character device",
        nix::libc::S_IFBLK => "block device",
        _ => "unknown inode",
    }
}

fn root_abi_directory_identity(directory: &fs::File) -> io::Result<FrozenRootIdentity> {
    directory
        .metadata()
        .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
}

fn require_root_abi_directory(root: &Path, directory: &fs::File, expected: FrozenRootIdentity) -> Result<(), Error> {
    let retained = root_abi_directory_identity(directory).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    let Ok(named) = open_root_abi_directory(root) else {
        return Err(Error::RootAbiDirectoryReplaced(root.to_owned()));
    };
    let named = root_abi_directory_identity(&named).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    if retained != expected || named != expected {
        return Err(Error::RootAbiDirectoryReplaced(root.to_owned()));
    }
    Ok(())
}

/// Create only the stable root ABI links required by a frozen build root.
///
/// The root has just been recreated, so any pre-existing entry is an invariant
/// violation rather than something to merge or replace.
fn create_frozen_root_links(root: RawFd, deadline: Instant) -> Result<(), Error> {
    for (source, target) in ROOT_ABI_LINKS {
        require_frozen_materialization_deadline(deadline)?;
        symlinkat(source, Some(root), target)?;
    }
    Ok(())
}
