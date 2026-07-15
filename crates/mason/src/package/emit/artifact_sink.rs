use super::{artifact_directory::DirectoryHandle, artifact_verification::*, *};

#[derive(Debug)]
pub(super) struct BoundedFile {
    pub(super) file: File,
    path: PathBuf,
    maximum: u64,
    position: u64,
    length: u64,
}

impl BoundedFile {
    fn new(file: File, path: PathBuf, maximum: u64) -> Self {
        Self {
            file,
            path,
            maximum,
            position: 0,
            length: 0,
        }
    }

    fn reset(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.position = 0;
        self.length = 0;
        Ok(())
    }

    fn seek_target(&self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(self.maximum)).contains(&target) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "artifact seek would leave the 0..={} byte range for {}",
                    self.maximum,
                    self.path.display()
                ),
            ));
        }
        Ok(target as u64)
    }
}

impl Write for BoundedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.position);
        if remaining == 0 && !buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "artifact write exceeds {} byte limit for {}",
                    self.maximum,
                    self.path.display()
                ),
            ));
        }
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let written = self.file.write(&buffer[..allowed])?;
        self.position = self
            .position
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("artifact write counter overflow"))?;
        self.length = self.length.max(self.position);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Read for BoundedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.position);
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = self.file.read(&mut buffer[..allowed])?;
        self.position = self
            .position
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("artifact read counter overflow"))?;
        Ok(read)
    }
}

impl Seek for BoundedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = self.seek_target(position)?;
        let actual = self.file.seek(SeekFrom::Start(target))?;
        if actual != target {
            return Err(io::Error::other("artifact descriptor sought to an unexpected offset"));
        }
        self.position = actual;
        Ok(actual)
    }
}

#[derive(Debug)]
struct ArtifactSlot {
    final_name: Vec<u8>,
    stage_name: Vec<u8>,
    identity: Identity,
    file: BoundedFile,
    activated: bool,
    sealed: bool,
    witness: Option<FileWitness>,
    digest: Option<[u8; 32]>,
    published: bool,
    cleaned: bool,
}

#[derive(Debug)]
struct ScratchSlot {
    identity: Identity,
    file: BoundedFile,
}

#[derive(Debug)]
pub(super) struct ArtifactSink {
    root: DirectoryHandle,
    stage: DirectoryHandle,
    slots: Vec<ArtifactSlot>,
    scratch: Option<ScratchSlot>,
    stage_removed: bool,
    active: bool,
}

impl ArtifactSink {
    pub(super) fn new(root_path: &Path, mut specs: Vec<ArtifactSpec>) -> Result<Self, ArtifactError> {
        if specs.len() > MAX_EMITTED_ARTIFACTS {
            return Err(ArtifactError::ResourceLimit {
                resource: "emitted artifacts",
                limit: MAX_EMITTED_ARTIFACTS,
            });
        }
        specs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for spec in &specs {
            validate_artifact_name(&spec.name)?;
        }
        for pair in specs.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(ArtifactError::DuplicateName {
                    name: pair[0].name.clone(),
                });
            }
        }

        let root = DirectoryHandle::open_root(root_path)?;
        root.require_inventory("initial artifact root", &[])?;
        let stage_path = root.display(EMISSION_STAGE_NAME);
        let stage_name = c_name(EMISSION_STAGE_NAME, &stage_path)?;
        // SAFETY: root and stage_name remain live, and mkdirat interprets the
        // NUL-terminated name relative to the pinned root descriptor.
        if unsafe { libc::mkdirat(root.file.as_raw_fd(), stage_name.as_ptr(), 0o700) } == -1 {
            return Err(ArtifactError::Io {
                operation: "create private artifact staging directory",
                path: stage_path,
                source: io::Error::last_os_error(),
            });
        }
        let stage_pin = match openat2_file(
            root.file.as_raw_fd(),
            EMISSION_STAGE_NAME,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
            &stage_path,
        ) {
            Ok(stage_pin) => stage_pin,
            Err(source) => {
                let primary = ArtifactError::Io {
                    operation: "pin newly created artifact staging directory",
                    path: stage_path.clone(),
                    source,
                };
                return Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(ArtifactError::UnprovenCleanup { path: stage_path }),
                });
            }
        };
        let stage_metadata = stage_pin.metadata().map_err(|source| ArtifactError::Rollback {
            primary: Box::new(ArtifactError::Io {
                operation: "inspect newly pinned artifact staging directory",
                path: stage_path.clone(),
                source,
            }),
            cleanup: Box::new(ArtifactError::UnprovenCleanup {
                path: stage_path.clone(),
            }),
        })?;
        let stage_identity = Identity::from_metadata(&stage_metadata);
        let stage = match root.open_child_directory(EMISSION_STAGE_NAME) {
            Ok(stage) if stage.identity == stage_identity => stage,
            Ok(_) => {
                let primary = ArtifactError::OwnershipChanged {
                    path: stage_path.clone(),
                };
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(
                        &root,
                        EMISSION_STAGE_NAME,
                        stage_identity,
                        true,
                        false,
                        "remove replaced artifact staging directory",
                    ),
                ));
            }
            Err(primary) => {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(
                        &root,
                        EMISSION_STAGE_NAME,
                        stage_identity,
                        true,
                        false,
                        "remove artifact staging directory after open failure",
                    ),
                ));
            }
        };
        let mut sink = Self {
            root,
            stage,
            slots: Vec::new(),
            scratch: None,
            stage_removed: false,
            active: true,
        };

        let setup = sink.populate(specs);
        if let Err(primary) = setup {
            return match sink.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        Ok(sink)
    }

    fn populate(&mut self, specs: Vec<ArtifactSpec>) -> Result<(), ArtifactError> {
        // SAFETY: stage is a live descriptor for the directory created above;
        // this removes any ambient umask influence from its final mode.
        if unsafe { libc::fchmod(self.stage.file.as_raw_fd(), 0o700) } == -1 {
            return Err(ArtifactError::Io {
                operation: "normalize private artifact staging directory mode",
                path: self.stage.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        self.slots
            .try_reserve(specs.len())
            .map_err(|source| ArtifactError::Allocation {
                resource: "artifact staging slots",
                requested: specs.len(),
                detail: source.to_string(),
            })?;
        for (index, spec) in specs.into_iter().enumerate() {
            let stage_name = format!("artifact-{index:03}").into_bytes();
            let path = self.stage.display(&stage_name);
            let (file, metadata) = create_owned_file(&self.stage, &stage_name, &path)?;
            self.slots.push(ArtifactSlot {
                final_name: spec.name.into_bytes(),
                stage_name,
                identity: Identity::from_metadata(&metadata),
                file: BoundedFile::new(file, path, spec.max_bytes),
                activated: false,
                sealed: false,
                witness: None,
                digest: None,
                published: false,
                cleaned: false,
            });
        }

        let scratch_path = self.stage.display(EMISSION_SCRATCH_NAME);
        let (scratch_file, scratch_metadata) = create_owned_file(&self.stage, EMISSION_SCRATCH_NAME, &scratch_path)?;
        self.scratch = Some(ScratchSlot {
            identity: Identity::from_metadata(&scratch_metadata),
            file: BoundedFile::new(scratch_file, scratch_path, MAX_STONE_ARTIFACT_BYTES),
        });

        let root_inventory = one_name(EMISSION_STAGE_NAME)?;
        self.root
            .require_inventory("artifact root during staging", &root_inventory)?;
        self.require_stage_inventory(true)?;
        self.require_stage_identity()?;
        Ok(())
    }

    pub(super) fn writer(&mut self, final_name: &str) -> Result<&mut BoundedFile, ArtifactError> {
        let index = self.slot_index(final_name)?;
        let slot = &mut self.slots[index];
        if slot.activated {
            return Err(ArtifactError::AlreadyPrepared {
                name: final_name.to_owned(),
            });
        }
        slot.activated = true;
        Ok(&mut slot.file)
    }

    pub(super) fn package_writers(
        &mut self,
        final_name: &str,
    ) -> Result<(&mut BoundedFile, &mut BoundedFile), ArtifactError> {
        let index = self.slot_index(final_name)?;
        self.require_slot_identity(index, false)?;
        self.require_scratch_identity()?;
        let (slots, scratch) = (&mut self.slots, &mut self.scratch);
        let slot = &mut slots[index];
        if slot.activated {
            return Err(ArtifactError::AlreadyPrepared {
                name: final_name.to_owned(),
            });
        }
        slot.activated = true;
        let scratch = scratch.as_mut().ok_or(ArtifactError::ScratchUnavailable)?;
        scratch.file.reset().map_err(|source| ArtifactError::Io {
            operation: "reset bounded content scratch",
            path: scratch.file.path.clone(),
            source,
        })?;
        Ok((&mut slot.file, &mut scratch.file))
    }

    fn slot_index(&self, final_name: &str) -> Result<usize, ArtifactError> {
        self.slots
            .binary_search_by(|slot| slot.final_name.as_slice().cmp(final_name.as_bytes()))
            .map_err(|_| ArtifactError::UnexpectedName {
                name: final_name.to_owned(),
            })
    }

    pub(super) fn commit(&mut self) -> Result<(), ArtifactError> {
        self.commit_with_hooks(|_, _| {}, |_, _| {})
    }

    #[cfg(test)]
    pub(super) fn commit_with_hook<F>(&mut self, hook: F) -> Result<(), ArtifactError>
    where
        F: FnMut(usize, &Path),
    {
        self.commit_with_hooks(|_, _| {}, hook)
    }

    pub(super) fn commit_with_hooks<B, A>(&mut self, before_rename: B, after_rename: A) -> Result<(), ArtifactError>
    where
        B: FnMut(usize, &Path),
        A: FnMut(usize, &Path),
    {
        let result = self.commit_inner(before_rename, after_rename);
        if let Err(primary) = result {
            return match self.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        self.active = false;
        Ok(())
    }

    fn commit_inner<B, A>(&mut self, mut before_rename: B, mut after_rename: A) -> Result<(), ArtifactError>
    where
        B: FnMut(usize, &Path),
        A: FnMut(usize, &Path),
    {
        self.root.require_path_identity()?;
        self.require_stage_identity()?;
        let root_inventory = one_name(EMISSION_STAGE_NAME)?;
        self.root
            .require_inventory("artifact root before publication", &root_inventory)?;
        self.require_stage_inventory(true)?;

        for index in 0..self.slots.len() {
            self.seal_slot(index)?;
        }
        self.remove_scratch()?;
        self.require_stage_inventory(false)?;
        self.stage.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync artifact staging directory",
            path: self.stage.path.clone(),
            source,
        })?;
        self.require_stage_identity()?;

        for index in 0..self.slots.len() {
            let staged_path = self.stage.display(&self.slots[index].stage_name);
            self.require_slot_integrity(index, false)?;
            // Test-only fault injection lives after the final staged check so
            // the following publication refresh must independently prove the
            // retained inode's bytes, not merely inherit that check.
            before_rename(index, &staged_path);
            rename_noreplace_at(
                &self.stage,
                &self.slots[index].stage_name,
                &self.root,
                &self.slots[index].final_name,
            )?;
            self.slots[index].published = true;
            self.refresh_published_witness(index)?;
            let path = self.root.display(&self.slots[index].final_name);
            after_rename(index, &path);
            self.require_slot_integrity(index, true)?;
        }

        self.stage
            .require_inventory("drained artifact staging directory", &[])?;
        self.stage.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync drained artifact staging directory",
            path: self.stage.path.clone(),
            source,
        })?;
        remove_owned_entry(
            &self.root,
            EMISSION_STAGE_NAME,
            self.stage.identity,
            true,
            false,
            "remove drained artifact staging directory",
        )?;
        self.stage_removed = true;

        let final_names = self.final_names()?;
        self.root.require_inventory("published artifact root", &final_names)?;
        for index in 0..self.slots.len() {
            self.require_slot_identity(index, true)?;
        }
        self.root.require_path_identity()?;
        self.root.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync published artifact root",
            path: self.root.path.clone(),
            source,
        })?;

        // Recheck after the durability barrier. This catches a concurrent
        // same-name replacement before the held output capabilities close.
        self.root.require_inventory("confirmed artifact root", &final_names)?;
        for index in 0..self.slots.len() {
            self.require_slot_integrity(index, true)?;
        }
        self.root.require_path_identity()
    }

    fn seal_slot(&mut self, index: usize) -> Result<(), ArtifactError> {
        if !self.slots[index].activated {
            return Err(ArtifactError::NotPrepared {
                name: String::from_utf8_lossy(&self.slots[index].final_name).into_owned(),
            });
        }
        self.require_slot_identity(index, false)?;
        let slot = &mut self.slots[index];
        slot.file.flush().map_err(|source| ArtifactError::Io {
            operation: "flush staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        // SAFETY: the descriptor is live; fchmod changes only this owned inode.
        if unsafe { libc::fchmod(slot.file.file.as_raw_fd(), 0o444) } == -1 {
            return Err(ArtifactError::Io {
                operation: "make staged artifact read-only",
                path: slot.file.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        slot.file.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        let metadata = slot.file.file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect sealed staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        require_regular_witness(&slot.file.path, &metadata, slot.identity, 0o444, slot.file.maximum)?;
        let witness = FileWitness::from_metadata(&metadata);
        let digest = digest_descriptor(&slot.file.file, &slot.file.path, witness)?;
        slot.witness = Some(witness);
        slot.digest = Some(digest);
        slot.sealed = true;
        self.require_slot_integrity(index, false)
    }

    fn refresh_published_witness(&mut self, index: usize) -> Result<(), ArtifactError> {
        let slot = &self.slots[index];
        let previous = slot.witness.ok_or_else(|| ArtifactError::ArtifactChanged {
            path: slot.file.path.clone(),
        })?;
        let path = self.root.display(&slot.final_name);
        let Some((named_metadata, identity)) = self
            .root
            .inspect(&slot.final_name, "authenticate newly published artifact")?
        else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        let descriptor_metadata = slot.file.file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect retained published artifact descriptor",
            path: path.clone(),
            source,
        })?;
        require_regular_witness(&path, &named_metadata, slot.identity, 0o444, slot.file.maximum)?;
        require_regular_witness(&path, &descriptor_metadata, slot.identity, 0o444, slot.file.maximum)?;
        let named_witness = FileWitness::from_metadata(&named_metadata);
        if identity != slot.identity
            || named_witness != FileWitness::from_metadata(&descriptor_metadata)
            || !named_witness.unchanged_by_rename_from(previous)
        {
            return Err(ArtifactError::ArtifactChanged { path });
        }
        let expected_digest = slot
            .digest
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let found_digest = digest_descriptor(&slot.file.file, &path, named_witness)?;
        if found_digest != expected_digest {
            return Err(ArtifactError::DigestChanged { path });
        }
        self.slots[index].witness = Some(named_witness);
        Ok(())
    }

    fn require_slot_integrity(&self, index: usize, published: bool) -> Result<(), ArtifactError> {
        self.require_slot_identity(index, published)?;
        let slot = &self.slots[index];
        let (directory, name) = if published {
            (&self.root, slot.final_name.as_slice())
        } else {
            (&self.stage, slot.stage_name.as_slice())
        };
        let path = directory.display(name);
        let witness = slot
            .witness
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let expected_digest = slot
            .digest
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let found_digest = digest_descriptor(&slot.file.file, &path, witness)?;
        if found_digest != expected_digest {
            return Err(ArtifactError::DigestChanged { path });
        }
        Ok(())
    }

    fn require_slot_identity(&self, index: usize, published: bool) -> Result<(), ArtifactError> {
        let slot = &self.slots[index];
        let (directory, name, role) = if published {
            (&self.root, slot.final_name.as_slice(), "published artifact")
        } else {
            (&self.stage, slot.stage_name.as_slice(), "staged artifact")
        };
        let path = directory.display(name);
        let Some((metadata, identity)) = directory.inspect(name, "reopen owned artifact")? else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != slot.identity || !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        if slot.sealed {
            require_regular_witness(&path, &metadata, slot.identity, 0o444, slot.file.maximum)?;
            let witness = slot
                .witness
                .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
            if FileWitness::from_metadata(&metadata) != witness {
                return Err(ArtifactError::ArtifactChanged { path });
            }
        } else if metadata.mode() & 0o7777 != 0o600 {
            return Err(ArtifactError::ModeMismatch {
                role,
                path,
                expected: 0o600,
                found: metadata.mode() & 0o7777,
            });
        }
        Ok(())
    }

    fn require_scratch_identity(&self) -> Result<(), ArtifactError> {
        let scratch = self.scratch.as_ref().ok_or(ArtifactError::ScratchUnavailable)?;
        let path = self.stage.display(EMISSION_SCRATCH_NAME);
        let Some((metadata, identity)) = self.stage.inspect(EMISSION_SCRATCH_NAME, "reopen content scratch")? else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != scratch.identity
            || !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        Ok(())
    }

    fn require_stage_identity(&self) -> Result<(), ArtifactError> {
        let path = self.root.display(EMISSION_STAGE_NAME);
        let Some((metadata, identity)) = self
            .root
            .inspect(EMISSION_STAGE_NAME, "reopen artifact staging directory")?
        else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != self.stage.identity || !metadata.file_type().is_dir() || metadata.mode() & 0o7777 != 0o700 {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        Ok(())
    }

    fn require_stage_inventory(&self, include_scratch: bool) -> Result<(), ArtifactError> {
        let capacity =
            self.slots
                .len()
                .checked_add(usize::from(include_scratch))
                .ok_or(ArtifactError::ResourceLimit {
                    resource: "artifact staging inventory entries",
                    limit: MAX_EMITTED_ARTIFACTS + 1,
                })?;
        let mut names = Vec::new();
        names
            .try_reserve_exact(capacity)
            .map_err(|source| ArtifactError::Allocation {
                resource: "artifact staging inventory entries",
                requested: capacity,
                detail: source.to_string(),
            })?;
        for slot in &self.slots {
            names.push(copy_bytes(&slot.stage_name, "artifact staging inventory name")?);
        }
        if include_scratch {
            names.push(copy_bytes(EMISSION_SCRATCH_NAME, "artifact scratch inventory name")?);
        }
        names.sort_unstable();
        self.stage.require_inventory("artifact staging directory", &names)
    }

    fn final_names(&self) -> Result<Vec<Vec<u8>>, ArtifactError> {
        let mut names = Vec::new();
        names
            .try_reserve_exact(self.slots.len())
            .map_err(|source| ArtifactError::Allocation {
                resource: "published artifact inventory entries",
                requested: self.slots.len(),
                detail: source.to_string(),
            })?;
        for slot in &self.slots {
            names.push(copy_bytes(&slot.final_name, "published artifact inventory name")?);
        }
        Ok(names)
    }

    fn remove_scratch(&mut self) -> Result<(), ArtifactError> {
        self.require_scratch_identity()?;
        let identity = self.scratch.as_ref().ok_or(ArtifactError::ScratchUnavailable)?.identity;
        remove_owned_entry(
            &self.stage,
            EMISSION_SCRATCH_NAME,
            identity,
            false,
            false,
            "remove artifact content scratch",
        )?;
        self.scratch = None;
        Ok(())
    }

    pub(super) fn abort(&mut self) -> Result<(), ArtifactError> {
        if !self.active {
            return Ok(());
        }
        let mut failures = Vec::new();
        for index in (0..self.slots.len()).rev() {
            if self.slots[index].cleaned {
                continue;
            }
            let removal = {
                let slot = &self.slots[index];
                let (directory, name, role) = if slot.published {
                    (
                        &self.root,
                        slot.final_name.as_slice(),
                        "remove owned published artifact",
                    )
                } else {
                    (&self.stage, slot.stage_name.as_slice(), "remove owned staged artifact")
                };
                remove_owned_entry(directory, name, slot.identity, false, false, role)
            };
            match removal {
                Ok(()) => self.slots[index].cleaned = true,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if let Some(scratch) = self.scratch.as_ref() {
            match remove_owned_entry(
                &self.stage,
                EMISSION_SCRATCH_NAME,
                scratch.identity,
                false,
                false,
                "remove owned content scratch",
            ) {
                Ok(()) => self.scratch = None,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if !self.stage_removed {
            match remove_owned_entry(
                &self.root,
                EMISSION_STAGE_NAME,
                self.stage.identity,
                true,
                false,
                "remove owned artifact staging directory",
            ) {
                Ok(()) => self.stage_removed = true,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if let Err(source) = self.root.file.sync_all() {
            failures.push(
                ArtifactError::Io {
                    operation: "sync artifact root after rollback",
                    path: self.root.path.clone(),
                    source,
                }
                .to_string(),
            );
        }
        if failures.is_empty() {
            self.active = false;
            Ok(())
        } else {
            Err(ArtifactError::Cleanup { failures })
        }
    }
}
impl Drop for ArtifactSink {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}

fn create_owned_file(directory: &DirectoryHandle, name: &[u8], path: &Path) -> Result<(File, Metadata), ArtifactError> {
    let file = openat2_file(
        directory.file.as_raw_fd(),
        name,
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
        0o600,
        descendant_resolution(),
        path,
    )
    .map_err(|source| ArtifactError::Io {
        operation: "create exclusive staged artifact",
        path: path.to_owned(),
        source,
    })?;
    let initial_metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(source) => {
            return Err(ArtifactError::Rollback {
                primary: Box::new(ArtifactError::Io {
                    operation: "inspect exclusively created staged artifact",
                    path: path.to_owned(),
                    source,
                }),
                cleanup: Box::new(ArtifactError::UnprovenCleanup { path: path.to_owned() }),
            });
        }
    };
    let identity = Identity::from_metadata(&initial_metadata);
    let normalized = normalize_new_regular(&file, path, identity);
    match normalized {
        Ok(metadata) => Ok((file, metadata)),
        Err(primary) => Err(with_cleanup(
            primary,
            remove_owned_entry(
                directory,
                name,
                identity,
                false,
                false,
                "remove rejected newly created staged artifact",
            ),
        )),
    }
}

fn normalize_new_regular(file: &File, path: &Path, identity: Identity) -> Result<Metadata, ArtifactError> {
    // SAFETY: the descriptor is live; fchmod affects only the newly created
    // inode, independent of the process umask.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } == -1 {
        return Err(ArtifactError::Io {
            operation: "normalize staged artifact mode",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let metadata = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect newly created staged artifact",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
        || Identity::from_metadata(&metadata) != identity
    {
        return Err(ArtifactError::UnexpectedKind {
            role: "new staged artifact",
            path: path.to_owned(),
            expected: "single-link regular file with mode 0600",
        });
    }
    Ok(metadata)
}

fn with_cleanup(primary: ArtifactError, cleanup: Result<(), ArtifactError>) -> ArtifactError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => ArtifactError::Rollback {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}
