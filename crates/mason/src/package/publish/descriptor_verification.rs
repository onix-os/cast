#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    user: u32,
    group: u32,
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            user: metadata.uid(),
            group: metadata.gid(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileWitness {
    identity: Identity,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    identity: Identity,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl DirectoryStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct DirectoryHandle {
    path: PathBuf,
    file: File,
    identity: Identity,
}

impl DirectoryHandle {
    fn open_root(path: &Path, role: &'static str) -> Result<Self, PublishError> {
        Self::open_root_with_policy(path, role, false)
    }

    fn open_pinned_root(path: &Path, pinned: &File, role: &'static str) -> Result<Self, PublishError> {
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make pinned publication root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = pinned.try_clone().map_err(|source| PublishError::Io {
            operation: "duplicate pinned publication root",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect pinned publication root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        require_effective_owner(role, &path, &metadata)?;
        require_protected_root_mode(role, &path, &metadata)?;
        let root = Self {
            path,
            file,
            identity: Identity::from_metadata(&metadata),
        };
        root.require_path_identity(role)?;
        Ok(root)
    }

    fn open_reference_root(path: &Path) -> Result<Self, PublishError> {
        Self::open_root_with_policy(path, "expected manifest parent", true)
    }

    fn open_root_with_policy(path: &Path, role: &'static str, reference: bool) -> Result<Self, PublishError> {
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make publication root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened publication root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        if reference {
            require_reference_owner(&path, &metadata)?;
        } else {
            require_effective_owner(role, &path, &metadata)?;
        }
        require_protected_root_mode(role, &path, &metadata)?;
        Ok(Self {
            path,
            file,
            identity: Identity::from_metadata(&metadata),
        })
    }

    fn display(&self, name: &[u8]) -> PathBuf {
        self.path.join(OsStr::from_bytes(name))
    }

    fn metadata(&self, operation: &'static str) -> Result<Metadata, PublishError> {
        self.file.metadata().map_err(|source| PublishError::Io {
            operation,
            path: self.path.clone(),
            source,
        })
    }

    fn require_path_identity(&self, role: &'static str) -> Result<(), PublishError> {
        self.require_path_identity_with_policy(role, false)
    }

    fn require_reference_path_identity(&self) -> Result<(), PublishError> {
        self.require_path_identity_with_policy("expected manifest parent", true)
    }

    fn require_path_identity_with_policy(&self, role: &'static str, reference: bool) -> Result<(), PublishError> {
        let reopened = openat2_file(
            libc::AT_FDCWD,
            self.path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| PublishError::Io {
            operation: "reopen publication root",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| PublishError::Io {
            operation: "inspect reopened publication root",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(PublishError::OwnershipChanged {
                path: self.path.clone(),
            });
        }
        if reference {
            require_reference_owner(&self.path, &metadata)?;
        } else {
            require_effective_owner(role, &self.path, &metadata)?;
        }
        require_protected_root_mode(role, &self.path, &metadata)?;
        Ok(())
    }

    fn inspect(&self, name: &[u8], operation: &'static str) -> Result<Option<(Metadata, Identity)>, PublishError> {
        let path = self.display(name);
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
        ) {
            Ok(file) => {
                let metadata = file.metadata().map_err(|source| PublishError::Io {
                    operation,
                    path,
                    source,
                })?;
                let identity = Identity::from_metadata(&metadata);
                Ok(Some((metadata, identity)))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PublishError::Io {
                operation,
                path,
                source,
            }),
        }
    }

    fn open_child_directory(
        &self,
        name: &[u8],
        role: &'static str,
        expected_mode: u32,
        expected_mtime: Option<i64>,
    ) -> Result<Option<Self>, PublishError> {
        let path = self.display(name);
        let Some((before, identity)) = self.inspect(name, "inspect publication child")? else {
            return Ok(None);
        };
        if !before.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        require_effective_owner(role, &path, &before)?;
        require_mode(role, &path, &before, expected_mode)?;
        require_directory_timestamp(&path, &before, expected_mtime)?;
        let file = openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication child directory",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened publication child directory",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_effective_owner(role, &path, &metadata)?;
        require_mode(role, &path, &metadata, expected_mode)?;
        require_directory_timestamp(&path, &metadata, expected_mtime)?;
        Ok(Some(Self { path, file, identity }))
    }

    fn require_named_directory(
        &self,
        name: &[u8],
        identity: Identity,
        mode: u32,
        expected_mtime: Option<i64>,
    ) -> Result<(), PublishError> {
        let path = self.display(name);
        let Some((metadata, found)) = self.inspect(name, "authenticate named published bundle")? else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if !metadata.file_type().is_dir() || found != identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_effective_owner("published bundle", &path, &metadata)?;
        require_mode("published bundle", &path, &metadata, mode)?;
        require_directory_timestamp(&path, &metadata, expected_mtime)
    }

    fn require_inventory(
        &self,
        role: &'static str,
        expected: &[Vec<u8>],
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let maximum = expected.len().checked_add(1).ok_or(PublishError::ResourceLimit {
            resource: "publication directory entries",
            limit: expected.len(),
        })?;
        let before = DirectoryStamp::from_metadata(&self.metadata("inspect directory before inventory")?);
        let first = self.read_names(maximum, deadline)?;
        let between = DirectoryStamp::from_metadata(&self.metadata("inspect directory between inventories")?);
        if before != between {
            return Err(PublishError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        let second = self.read_names(maximum, deadline)?;
        let after = DirectoryStamp::from_metadata(&self.metadata("inspect directory after inventory")?);
        if between != after || first != second {
            return Err(PublishError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        if first != expected {
            return Err(PublishError::FrozenFileSetMismatch {
                role,
                path: self.path.clone(),
                expected: os_names(expected)?,
                found: os_names(&first)?,
            });
        }
        Ok(())
    }

    fn read_names(&self, maximum: usize, deadline: &Deadline) -> Result<Vec<Vec<u8>>, PublishError> {
        let cursor = openat2_file(
            self.file.as_raw_fd(),
            b".",
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication directory cursor",
            path: self.path.clone(),
            source,
        })?;
        let stream = DirectoryStream::from_file(cursor, &self.path)?;
        let mut names = Vec::new();
        names.try_reserve(maximum).map_err(|source| PublishError::Allocation {
            resource: "publication directory names",
            requested: maximum,
            detail: source.to_string(),
        })?;
        loop {
            deadline.check("enumerate publication directory")?;
            Errno::clear();
            // SAFETY: the live directory stream is exclusively used here.
            let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    break;
                }
                return Err(PublishError::Io {
                    operation: "enumerate publication directory",
                    path: self.path.clone(),
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: d_name is NUL-terminated and live until the next call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if names.len() == maximum {
                return Err(PublishError::ResourceLimit {
                    resource: "publication directory entries",
                    limit: maximum,
                });
            }
            names.push(copy_bytes(name, "publication directory entry name")?);
        }
        names.sort_unstable();
        Ok(names)
    }

    fn sync(&self, operation: &'static str) -> Result<(), PublishError> {
        self.file.sync_all().map_err(|source| PublishError::SyncDirectory {
            role: operation,
            path: self.path.clone(),
            source,
        })
    }
}

struct DirectoryStream(NonNull<libc::DIR>);

impl DirectoryStream {
    fn from_file(file: File, path: &Path) -> Result<Self, PublishError> {
        let descriptor = file.into_raw_fd();
        // SAFETY: descriptor is fresh and fdopendir consumes it on success.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and did not consume descriptor.
                unsafe { libc::close(descriptor) };
                Err(PublishError::Io {
                    operation: "open publication directory stream",
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the DIR pointer.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

#[derive(Debug)]
struct VerifiedEntry {
    name: Vec<u8>,
    path: PathBuf,
    file: File,
    witness: FileWitness,
    digest: Option<[u8; 32]>,
}

impl VerifiedEntry {
    fn open(
        root: &DirectoryHandle,
        spec: &BundleSpec,
        role: &'static str,
        expected_mtime: Option<i64>,
    ) -> Result<Self, PublishError> {
        let path = root.display(&spec.name);
        let Some((named_metadata, named_identity)) = root.inspect(&spec.name, "inspect verified bundle artefact")?
        else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if !named_metadata.file_type().is_file() {
            return Err(PublishError::UnexpectedEntry { role, path });
        }
        let file = openat2_file(
            root.file.as_raw_fd(),
            &spec.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open verified bundle artefact",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect verified bundle artefact",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != named_identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_regular(role, &path, &metadata, spec.maximum, expected_mtime)?;
        Ok(Self {
            name: copy_bytes(&spec.name, "verified artefact name")?,
            path,
            file,
            witness: FileWitness::from_metadata(&metadata),
            digest: None,
        })
    }

    fn require_named(&self, root: &DirectoryHandle, role: &'static str) -> Result<(), PublishError> {
        let Some((metadata, identity)) = root.inspect(&self.name, "reopen verified bundle artefact")? else {
            return Err(PublishError::OwnershipChanged {
                path: self.path.clone(),
            });
        };
        if identity != self.witness.identity || FileWitness::from_metadata(&metadata) != self.witness {
            return Err(PublishError::ArtifactChanged {
                path: self.path.clone(),
            });
        }
        let _ = role;
        Ok(())
    }

    fn digest(&mut self, deadline: &Deadline) -> Result<[u8; 32], PublishError> {
        hash_file(&mut self.file, &self.path, self.witness, deadline)
    }
}

#[derive(Debug)]
struct ReferenceManifest {
    parent: DirectoryHandle,
    name: Vec<u8>,
    path: PathBuf,
    file: File,
    witness: FileWitness,
    digest: Option<[u8; 32]>,
}

impl ReferenceManifest {
    fn open(path: &Path, maximum: u64, deadline: &Deadline) -> Result<Self, PublishError> {
        deadline.check("open expected binary manifest")?;
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make expected binary manifest path absolute",
            path: path.to_owned(),
            source,
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| PublishError::InvalidReferencePath { path: path.clone() })?
            .as_bytes();
        validate_component(name, "expected binary manifest")?;
        let name = copy_bytes(name, "expected binary manifest name")?;
        let parent_path = path
            .parent()
            .ok_or_else(|| PublishError::InvalidReferencePath { path: path.clone() })?;
        let parent = DirectoryHandle::open_reference_root(parent_path)?;
        let Some((named_metadata, named_identity)) = parent.inspect(&name, "inspect expected binary manifest")? else {
            return Err(PublishError::MissingReferenceManifest { path });
        };
        require_reference_regular(&path, &named_metadata, maximum)?;
        let file = openat2_file(
            parent.file.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open expected binary manifest",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened expected binary manifest",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != named_identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_reference_regular(&path, &metadata, maximum)?;
        let reference = Self {
            parent,
            name,
            path,
            file,
            witness: FileWitness::from_metadata(&metadata),
            digest: None,
        };
        reference.require_stable()?;
        Ok(reference)
    }

    fn require_stable(&self) -> Result<(), PublishError> {
        let Some((metadata, identity)) = self
            .parent
            .inspect(&self.name, "authenticate expected binary manifest")?
        else {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        };
        if identity != self.witness.identity || FileWitness::from_metadata(&metadata) != self.witness {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        let descriptor = self.file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect retained expected binary manifest",
            path: self.path.clone(),
            source,
        })?;
        if FileWitness::from_metadata(&descriptor) != self.witness {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        self.parent.require_reference_path_identity()
    }

    fn require_digest(&self, expected: [u8; 32]) -> Result<(), PublishError> {
        self.require_stable()?;
        if self.digest == Some(expected) {
            Ok(())
        } else {
            Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            })
        }
    }

    fn compare_file(
        &mut self,
        generated: &mut File,
        generated_path: &Path,
        generated_witness: FileWitness,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        self.require_stable()?;
        let digest = compare_manifest_files(
            generated,
            generated_path,
            generated_witness,
            &mut self.file,
            &self.path,
            self.witness,
            deadline,
        )?;
        self.require_stable()?;
        if let Some(expected) = self.digest
            && expected != digest
        {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        self.digest = Some(digest);
        Ok(digest)
    }
}

fn require_reference_regular(path: &Path, metadata: &Metadata, maximum: u64) -> Result<(), PublishError> {
    if !metadata.file_type().is_file() {
        return Err(PublishError::UnexpectedEntry {
            role: "expected manifest",
            path: path.to_owned(),
        });
    }
    require_reference_owner(path, metadata)?;
    let mode = metadata.mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(PublishError::WritableReferenceManifest {
            path: path.to_owned(),
            found: mode,
        });
    }
    if metadata.len() > maximum {
        return Err(PublishError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    Ok(())
}

fn compare_manifest_files(
    generated: &mut File,
    generated_path: &Path,
    generated_witness: FileWitness,
    reference: &mut File,
    reference_path: &Path,
    reference_witness: FileWitness,
    deadline: &Deadline,
) -> Result<[u8; 32], PublishError> {
    require_file_witness(
        generated,
        generated_path,
        generated_witness,
        "generated manifest before comparison",
    )?;
    require_file_witness(
        reference,
        reference_path,
        reference_witness,
        "expected manifest before comparison",
    )?;
    if generated_witness.length != reference_witness.length {
        require_file_witness(
            generated,
            generated_path,
            generated_witness,
            "generated manifest after comparison",
        )?;
        require_file_witness(
            reference,
            reference_path,
            reference_witness,
            "expected manifest after comparison",
        )?;
        return Err(PublishError::ManifestVerificationMismatch {
            generated: generated_path.to_owned(),
            expected: reference_path.to_owned(),
        });
    }
    generated
        .seek(SeekFrom::Start(0))
        .map_err(|source| PublishError::Read {
            path: generated_path.to_owned(),
            source,
        })?;
    reference
        .seek(SeekFrom::Start(0))
        .map_err(|source| PublishError::Read {
            path: reference_path.to_owned(),
            source,
        })?;
    let mut generated_hash = Sha256::new();
    let mut reference_hash = Sha256::new();
    let mut generated_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut reference_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut remaining = generated_witness.length;
    let mut mismatch = false;
    while remaining > 0 {
        deadline.check("compare binary manifests")?;
        let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(COPY_BUFFER_BYTES);
        read_exact_manifest_chunk(generated, &mut generated_buffer[..amount], generated_path, deadline)?;
        read_exact_manifest_chunk(reference, &mut reference_buffer[..amount], reference_path, deadline)?;
        generated_hash.update(&generated_buffer[..amount]);
        reference_hash.update(&reference_buffer[..amount]);
        if generated_buffer[..amount] != reference_buffer[..amount] {
            mismatch = true;
            break;
        }
        remaining -= amount as u64;
    }
    if !mismatch {
        let mut generated_trailing = [0_u8; 1];
        let mut reference_trailing = [0_u8; 1];
        if generated
            .read(&mut generated_trailing)
            .map_err(|source| PublishError::Read {
                path: generated_path.to_owned(),
                source,
            })?
            != 0
        {
            return Err(PublishError::ArtifactChanged {
                path: generated_path.to_owned(),
            });
        }
        if reference
            .read(&mut reference_trailing)
            .map_err(|source| PublishError::Read {
                path: reference_path.to_owned(),
                source,
            })?
            != 0
        {
            return Err(PublishError::ReferenceManifestChanged {
                path: reference_path.to_owned(),
            });
        }
    }
    deadline.check("finish binary manifest comparison")?;
    require_file_witness(
        generated,
        generated_path,
        generated_witness,
        "generated manifest after comparison",
    )?;
    require_file_witness(
        reference,
        reference_path,
        reference_witness,
        "expected manifest after comparison",
    )?;
    let generated_digest: [u8; 32] = generated_hash.finalize().into();
    let reference_digest: [u8; 32] = reference_hash.finalize().into();
    if mismatch || generated_digest != reference_digest {
        return Err(PublishError::ManifestVerificationMismatch {
            generated: generated_path.to_owned(),
            expected: reference_path.to_owned(),
        });
    }
    Ok(generated_digest)
}

fn read_exact_manifest_chunk(
    file: &mut File,
    mut buffer: &mut [u8],
    path: &Path,
    deadline: &Deadline,
) -> Result<(), PublishError> {
    while !buffer.is_empty() {
        deadline.check("read binary manifest")?;
        let read = file.read(buffer).map_err(|source| PublishError::Read {
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            return Err(PublishError::ArtifactChanged { path: path.to_owned() });
        }
        buffer = &mut buffer[read..];
    }
    Ok(())
}

fn require_file_witness(
    file: &File,
    path: &Path,
    witness: FileWitness,
    operation: &'static str,
) -> Result<(), PublishError> {
    let metadata = file.metadata().map_err(|source| PublishError::Io {
        operation,
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&metadata) == witness {
        Ok(())
    } else {
        Err(PublishError::ArtifactChanged { path: path.to_owned() })
    }
}
