#[derive(Debug)]
struct VerifiedBundle {
    root: DirectoryHandle,
    entries: Vec<VerifiedEntry>,
}

impl VerifiedBundle {
    fn open(
        root: DirectoryHandle,
        specs: &[BundleSpec],
        role: &'static str,
        expected_mtime: Option<i64>,
        max_bundle_bytes: u64,
        deadline: &Deadline,
    ) -> Result<Self, PublishError> {
        let expected = expected_names(specs)?;
        root.require_inventory(role, &expected, deadline)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(specs.len())
            .map_err(|source| PublishError::Allocation {
                resource: "verified bundle entries",
                requested: specs.len(),
                detail: source.to_string(),
            })?;
        let mut total = 0_u64;
        for spec in specs {
            deadline.check("open published bundle entries")?;
            let entry = VerifiedEntry::open(&root, spec, role, expected_mtime)?;
            total = total
                .checked_add(entry.witness.length)
                .ok_or(PublishError::BundleTooLarge {
                    maximum: max_bundle_bytes,
                    found: u64::MAX,
                })?;
            if total > max_bundle_bytes {
                return Err(PublishError::BundleTooLarge {
                    maximum: max_bundle_bytes,
                    found: total,
                });
            }
            entries.push(entry);
        }
        root.require_inventory(role, &expected, deadline)?;
        for entry in &entries {
            entry.require_named(&root, role)?;
        }
        root.require_path_identity(role)?;
        Ok(Self { root, entries })
    }

    fn reject_manifest_alias(&self, name: &[u8], reference: &ReferenceManifest) -> Result<(), PublishError> {
        let entry =
            self.entries
                .iter()
                .find(|entry| entry.name == name)
                .ok_or_else(|| PublishError::OwnershipChanged {
                    path: self.root.display(name),
                })?;
        if entry.witness.identity == reference.witness.identity {
            Err(PublishError::ReferenceAliasesStagedManifest {
                generated: entry.path.clone(),
                expected: reference.path.clone(),
            })
        } else {
            Ok(())
        }
    }

    fn compare_manifest(
        &mut self,
        name: &[u8],
        reference: &mut ReferenceManifest,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.root.display(name),
            })?;
        let root = &self.root;
        let entry = &mut self.entries[index];
        entry.require_named(root, "verified binary manifest")?;
        let digest = reference.compare_file(&mut entry.file, &entry.path, entry.witness, deadline)?;
        entry.require_named(root, "verified binary manifest")?;
        if let Some(expected) = entry.digest
            && expected != digest
        {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
        entry.digest = Some(digest);
        Ok(digest)
    }

    fn verify_manifest_digest(
        &mut self,
        name: &[u8],
        expected_digest: [u8; 32],
        expected_path: &Path,
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.root.display(name),
            })?;
        entry.require_named(&self.root, "verified binary manifest")?;
        let digest = entry.digest(deadline)?;
        entry.require_named(&self.root, "verified binary manifest")?;
        if let Some(previous) = entry.digest
            && previous != digest
        {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
        if digest != expected_digest {
            return Err(PublishError::ManifestVerificationMismatch {
                generated: entry.path.clone(),
                expected: expected_path.to_owned(),
            });
        }
        entry.digest = Some(digest);
        Ok(())
    }
}

fn digest_round(
    bundle: &mut VerifiedBundle,
    expected: &[Vec<u8>],
    deadline: &Deadline,
) -> Result<Vec<[u8; 32]>, PublishError> {
    bundle.root.require_inventory("verified", expected, deadline)?;
    let mut digests = Vec::new();
    digests
        .try_reserve_exact(bundle.entries.len())
        .map_err(|source| PublishError::Allocation {
            resource: "bundle digests",
            requested: bundle.entries.len(),
            detail: source.to_string(),
        })?;
    for entry in &mut bundle.entries {
        entry.require_named(&bundle.root, "verified")?;
        digests.push(entry.digest(deadline)?);
        entry.require_named(&bundle.root, "verified")?;
    }
    bundle.root.require_inventory("verified", expected, deadline)?;
    bundle.root.require_path_identity("verified")?;
    Ok(digests)
}

fn verify_digest_round(
    bundle: &mut VerifiedBundle,
    expected: &[Vec<u8>],
    deadline: &Deadline,
) -> Result<(), PublishError> {
    let digests = digest_round(bundle, expected, deadline)?;
    for (entry, digest) in bundle.entries.iter().zip(digests) {
        if entry.digest != Some(digest) {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
    }
    Ok(())
}

fn hash_file(
    file: &mut File,
    path: &Path,
    witness: FileWitness,
    deadline: &Deadline,
) -> Result<[u8; 32], PublishError> {
    let before = file.metadata().map_err(|source| PublishError::Io {
        operation: "inspect artefact before digest",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&before) != witness {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    file.seek(SeekFrom::Start(0)).map_err(|source| PublishError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut remaining = witness.length;
    while remaining > 0 {
        deadline.check("digest published artefact")?;
        let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = file.read(&mut buffer[..amount]).map_err(|source| PublishError::Read {
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            return Err(PublishError::ArtifactChanged { path: path.to_owned() });
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing).map_err(|source| PublishError::Read {
        path: path.to_owned(),
        source,
    })? != 0
    {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    let after = file.metadata().map_err(|source| PublishError::Io {
        operation: "inspect artefact after digest",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&after) != witness {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    Ok(hasher.finalize().into())
}

#[derive(Debug)]
struct OwnedEntry {
    name: Vec<u8>,
    identity: Identity,
    witness: Option<FileWitness>,
    digest: Option<[u8; 32]>,
    file: Option<File>,
}

impl OwnedEntry {
    fn require_named(&self, directory: &DirectoryHandle, operation: &'static str) -> Result<(), PublishError> {
        let path = directory.display(&self.name);
        let witness = self
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        let Some((metadata, identity)) = directory.inspect(&self.name, operation)? else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if identity != self.identity || FileWitness::from_metadata(&metadata) != witness {
            return Err(PublishError::ArtifactChanged { path });
        }
        Ok(())
    }

    fn open_readonly(&self, directory: &DirectoryHandle, operation: &'static str) -> Result<File, PublishError> {
        let path = directory.display(&self.name);
        let witness = self
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        self.require_named(directory, operation)?;
        let file = openat2_file(
            directory.file.as_raw_fd(),
            &self.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation,
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation,
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != self.identity || FileWitness::from_metadata(&metadata) != witness {
            return Err(PublishError::ArtifactChanged { path });
        }
        self.require_named(directory, operation)?;
        Ok(file)
    }
}
