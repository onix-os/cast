#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleLocation {
    Temporary,
    Published,
}

#[derive(Debug)]
struct TemporaryBundle<'a> {
    output: &'a DirectoryHandle,
    directory: DirectoryHandle,
    temporary_name: Vec<u8>,
    final_name: Vec<u8>,
    entries: Vec<OwnedEntry>,
    source_date_epoch: i64,
    location: BundleLocation,
    active: bool,
}

impl<'a> TemporaryBundle<'a> {
    fn create(
        output: &'a DirectoryHandle,
        final_name: &[u8],
        entry_capacity: usize,
        source_date_epoch: i64,
        deadline: &Deadline,
    ) -> Result<Self, PublishError> {
        output.require_path_identity("output")?;
        let final_name = copy_bytes(final_name, "final publication bundle name")?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_capacity)
            .map_err(|source| PublishError::Allocation {
                resource: "owned publication entries",
                requested: entry_capacity,
                detail: source.to_string(),
            })?;
        let mut last_collision = None;
        for _ in 0..TEMPORARY_ATTEMPTS {
            deadline.check("create private publication bundle")?;
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".mason-publish-{}-{}-{sequence}",
                std::process::id(),
                hex_prefix(&final_name)
            )
            .into_bytes();
            validate_component(&name, "temporary publication bundle")?;
            let path = output.display(&name);
            let c_name = c_name(&name, &path)?;
            // SAFETY: output and the NUL-terminated component remain live.
            if unsafe { libc::mkdirat(output.file.as_raw_fd(), c_name.as_ptr(), 0o700) } == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::AlreadyExists {
                    last_collision = Some(source);
                    continue;
                }
                return Err(PublishError::CreateTemporary {
                    output: output.path.clone(),
                    source,
                });
            }
            let pin = match openat2_file(
                output.file.as_raw_fd(),
                &name,
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0,
                descendant_resolution(),
            ) {
                Ok(file) => file,
                Err(source) => {
                    return Err(PublishError::Rollback {
                        primary: Box::new(PublishError::Io {
                            operation: "pin new publication directory",
                            path: path.clone(),
                            source,
                        }),
                        cleanup: Box::new(PublishError::UnprovenCleanup { path }),
                    });
                }
            };
            let metadata = pin.metadata().map_err(|source| PublishError::Rollback {
                primary: Box::new(PublishError::Io {
                    operation: "inspect new publication directory",
                    path: path.clone(),
                    source,
                }),
                cleanup: Box::new(PublishError::UnprovenCleanup { path: path.clone() }),
            })?;
            let identity = Identity::from_metadata(&metadata);
            if !metadata.file_type().is_dir() {
                return Err(with_cleanup(
                    PublishError::UnexpectedRoot {
                        role: "new temporary bundle",
                        path: path.clone(),
                    },
                    remove_owned_entry(output, &name, identity, true, "remove umask-rejected temporary bundle"),
                ));
            }
            if let Err(primary) = require_effective_owner("new temporary bundle", &path, &metadata) {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove foreign-owned temporary bundle"),
                ));
            }
            // mkdirat applies the ambient process umask. Never normalize by
            // name: the name could be replaced between authentication and
            // chmod. Instead, open the pinned inode as a usable directory,
            // authenticate that descriptor against the O_PATH pin, and chmod
            // only through the authenticated descriptor. A restrictive umask
            // which prevents that open fails closed and removes the empty
            // directory by its recorded identity.
            let file = match openat2_file(
                output.file.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
                descendant_resolution(),
            ) {
                Ok(file) => file,
                Err(source) => {
                    let primary = PublishError::Io {
                        operation: "open new publication directory",
                        path: path.clone(),
                        source,
                    };
                    return Err(with_cleanup(
                        primary,
                        remove_owned_entry(output, &name, identity, true, "remove unopened temporary bundle"),
                    ));
                }
            };
            let opened = file.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "inspect opened publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove uninspectable temporary bundle"),
                )
            })?;
            if !opened.file_type().is_dir() || Identity::from_metadata(&opened) != identity {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove replaced temporary bundle"),
                ));
            }
            if let Err(primary) = set_mode(&file, &path, 0o700, "new temporary bundle") {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove unnormalized temporary bundle"),
                ));
            }
            let normalized = file.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "inspect normalized publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove unverified temporary bundle"),
                )
            })?;
            if !normalized.file_type().is_dir()
                || Identity::from_metadata(&normalized) != identity
                || normalized.mode() & 0o7777 != 0o700
            {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove misnormalized temporary bundle"),
                ));
            }
            let pinned_after = pin.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "reinspect pinned publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove unverified temporary bundle"),
                )
            })?;
            if !pinned_after.file_type().is_dir()
                || Identity::from_metadata(&pinned_after) != identity
                || pinned_after.mode() & 0o7777 != 0o700
            {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove misnormalized temporary bundle"),
                ));
            }
            let directory = DirectoryHandle { path, file, identity };
            if let Err(primary) = output.require_named_directory(&name, identity, 0o700, None) {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove unauthenticated temporary bundle"),
                ));
            }
            return Ok(Self {
                output,
                directory,
                temporary_name: name,
                final_name,
                entries,
                source_date_epoch,
                location: BundleLocation::Temporary,
                active: true,
            });
        }
        Err(PublishError::CreateTemporary {
            output: output.path.clone(),
            source: last_collision.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)),
        })
    }

    fn copy_from(
        &mut self,
        index: usize,
        source: &mut VerifiedEntry,
        spec: &BundleSpec,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        deadline.check("copy published artefact")?;
        let path = self.directory.display(&spec.name);
        // Complete the only fallible owned-name allocation before O_EXCL
        // creates an inode. The entry vector was reserved before the bundle
        // directory was created, so the push immediately after fstat cannot
        // allocate and every created inode is tracked for rollback.
        let owned_name = copy_bytes(&spec.name, "owned publication entry name")?;
        let file = openat2_file(
            self.directory.file.as_raw_fd(),
            &spec.name,
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
            0o600,
            descendant_resolution(),
        )
        .map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        let metadata = file.metadata().map_err(|source_error| PublishError::Rollback {
            primary: Box::new(PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            }),
            cleanup: Box::new(PublishError::UnprovenCleanup { path: path.clone() }),
        })?;
        let identity = Identity::from_metadata(&metadata);
        self.entries.push(OwnedEntry {
            name: owned_name,
            identity,
            witness: None,
            digest: None,
            file: None,
        });
        if !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(PublishError::UnexpectedEntry {
                role: "temporary",
                path,
            });
        }
        require_effective_owner("temporary", &path, &metadata)?;
        // Normalize through the authenticated descriptor so the ambient umask
        // cannot silently weaken or strengthen the temporary file mode and no
        // name-based chmod can be redirected to a replacement inode.
        set_mode(&file, &path, 0o600, "new temporary artefact")?;
        let normalized = file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if !normalized.file_type().is_file()
            || normalized.nlink() != 1
            || Identity::from_metadata(&normalized) != identity
            || normalized.mode() & 0o7777 != 0o600
        {
            return Err(PublishError::UnexpectedEntry {
                role: "temporary",
                path,
            });
        }
        let mut target = file;
        let source_before = source.file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if FileWitness::from_metadata(&source_before) != source.witness {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        source
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|source_error| PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            })?;
        let mut hasher = Sha256::new();
        let mut remaining = source.witness.length;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        while remaining > 0 {
            deadline.check("copy published artefact")?;
            let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
            let read = source
                .file
                .read(&mut buffer[..amount])
                .map_err(|source_error| PublishError::Copy {
                    staged: source.path.clone(),
                    temporary: path.clone(),
                    source: source_error,
                })?;
            if read == 0 {
                return Err(PublishError::ArtifactChanged {
                    path: source.path.clone(),
                });
            }
            target
                .write_all(&buffer[..read])
                .map_err(|source_error| PublishError::Copy {
                    staged: source.path.clone(),
                    temporary: path.clone(),
                    source: source_error,
                })?;
            hasher.update(&buffer[..read]);
            remaining -= read as u64;
        }
        let mut trailing = [0_u8; 1];
        if source
            .file
            .read(&mut trailing)
            .map_err(|source_error| PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            })?
            != 0
        {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        let source_after = source.file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if FileWitness::from_metadata(&source_after) != source.witness {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        let source_digest: [u8; 32] = hasher.finalize().into();
        target.flush().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        set_mode(&target, &path, PUBLISHED_ARTEFACT_MODE, "temporary artefact")?;
        set_timestamp(&target, &path, self.source_date_epoch)?;
        target.sync_all().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        let final_metadata = target.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        require_regular(
            "temporary",
            &path,
            &final_metadata,
            spec.maximum,
            Some(self.source_date_epoch),
        )?;
        if Identity::from_metadata(&final_metadata) != identity || final_metadata.len() != source.witness.length {
            return Err(PublishError::ArtifactChanged { path });
        }
        let witness = FileWitness::from_metadata(&final_metadata);
        // chmod does not revoke access held by an existing O_RDWR descriptor.
        // Close the construction descriptor before the bundle can be sealed,
        // then authenticate a fresh descriptor-relative O_RDONLY handle.
        drop(target);
        self.entries[index].witness = Some(witness);
        let mut readonly = self.entries[index].open_readonly(&self.directory, "open sealed temporary artefact")?;
        let target_digest = hash_file(&mut readonly, &path, witness, deadline)?;
        if target_digest != source_digest {
            return Err(PublishError::ContentMismatch {
                staged: source.path.clone(),
                published: path,
            });
        }
        self.entries[index].digest = Some(target_digest);
        Ok(source_digest)
    }

    fn seal(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        self.directory.require_inventory("temporary", expected, deadline)?;
        set_mode(
            &self.directory.file,
            &self.directory.path,
            PUBLISHED_BUNDLE_MODE,
            "temporary bundle",
        )?;
        set_timestamp(&self.directory.file, &self.directory.path, self.source_date_epoch)?;
        self.directory.sync("temporary bundle")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)?;
        self.verify_entries(expected, deadline)
    }

    fn verify_manifest_digest(
        &mut self,
        name: &[u8],
        expected_digest: [u8; 32],
        expected_path: &Path,
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.directory.display(name),
            })?;
        let directory = &self.directory;
        let entry = &mut self.entries[index];
        let path = directory.display(&entry.name);
        let witness = entry
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        entry.require_named(directory, "authenticate copied binary manifest")?;
        if entry.file.is_none() {
            entry.file = Some(entry.open_readonly(directory, "retain copied binary manifest")?);
        }
        let file = entry
            .file
            .as_mut()
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        require_file_witness(file, &path, witness, "authenticate retained copied binary manifest")?;
        let digest = hash_file(file, &path, witness, deadline)?;
        if entry.digest != Some(digest) {
            return Err(PublishError::ArtifactChanged { path });
        }
        if digest != expected_digest {
            return Err(PublishError::ManifestVerificationMismatch {
                generated: path,
                expected: expected_path.to_owned(),
            });
        }
        entry.require_named(directory, "reauthenticate copied binary manifest")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)
    }

    fn install(&mut self) -> Result<InstallOutcome, PublishError> {
        self.output.require_path_identity("output")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)?;
        match rename_noreplace_at(
            self.output,
            &self.temporary_name,
            self.output,
            &self.final_name,
            "atomically install derivation bundle",
        ) {
            Ok(()) => {
                self.location = BundleLocation::Published;
                self.directory.path = self.output.display(&self.final_name);
                Ok(InstallOutcome::Installed)
            }
            Err(PublishError::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
                Ok(InstallOutcome::AlreadyExists)
            }
            Err(error) => Err(error),
        }
    }

    fn verify_final(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        if self.location != BundleLocation::Published {
            return Err(PublishError::OwnershipChanged {
                path: self.directory.path.clone(),
            });
        }
        self.output.require_named_directory(
            &self.final_name,
            self.directory.identity,
            PUBLISHED_BUNDLE_MODE,
            Some(self.source_date_epoch),
        )?;
        self.verify_entries(expected, deadline)?;
        self.output.require_path_identity("output")
    }

    fn verify_entries(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        self.directory.require_inventory("published", expected, deadline)?;
        for entry in &mut self.entries {
            let path = self.directory.display(&entry.name);
            let witness = entry
                .witness
                .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
            let mut file = entry.open_readonly(&self.directory, "authenticate owned bundle entry")?;
            let digest = hash_file(&mut file, &path, witness, deadline)?;
            if entry.digest != Some(digest) {
                return Err(PublishError::ArtifactChanged { path });
            }
        }
        self.directory.require_inventory("published", expected, deadline)?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)
    }

    fn require_directory(&self, mode: u32) -> Result<(), PublishError> {
        let name = match self.location {
            BundleLocation::Temporary => &self.temporary_name,
            BundleLocation::Published => &self.final_name,
        };
        let expected_mtime = (mode == PUBLISHED_BUNDLE_MODE).then_some(self.source_date_epoch);
        self.output
            .require_named_directory(name, self.directory.identity, mode, expected_mtime)
    }

    fn commit(&mut self) {
        self.active = false;
    }

    fn rollback_error(&mut self, primary: PublishError) -> PublishError {
        let published = self.location == BundleLocation::Published;
        match self.abort() {
            Ok(()) => primary,
            Err(cleanup) if published => PublishError::PublishedDurabilityUnknown {
                final_path: self.output.display(&self.final_name),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            },
            Err(cleanup) => PublishError::Rollback {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            },
        }
    }

    fn abort(&mut self) -> Result<(), PublishError> {
        if !self.active {
            return Ok(());
        }
        // Never retry implicitly after an ownership failure: a later retry
        // could observe a different foreign name and turn a fail-closed cleanup
        // into destructive path-based cleanup.
        self.active = false;
        let mut failures = Vec::new();
        if let Err(error) = set_mode(&self.directory.file, &self.directory.path, 0o700, "rollback bundle") {
            failures.push(error.to_string());
        }
        for entry in self.entries.iter().rev() {
            if let Err(error) = remove_owned_entry(
                &self.directory,
                &entry.name,
                entry.identity,
                false,
                "remove owned publication entry",
            ) {
                failures.push(error.to_string());
            }
        }
        if let Err(error) = self.directory.sync("rollback bundle") {
            failures.push(error.to_string());
        }
        let name = match self.location {
            BundleLocation::Temporary => &self.temporary_name,
            BundleLocation::Published => &self.final_name,
        };
        if let Err(error) = remove_owned_entry(
            self.output,
            name,
            self.directory.identity,
            true,
            "remove owned publication bundle",
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = self.output.sync("output after publication rollback") {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(PublishError::Cleanup { failures })
        }
    }
}

impl Drop for TemporaryBundle<'_> {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}
