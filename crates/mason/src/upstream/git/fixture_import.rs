const FIXTURE_GIT_BUNDLE_MAX_BYTES: u64 = 1024 * 1024;

const FIXTURE_GIT_LIMITS: gitwrap::Limits = gitwrap::Limits {
    wall_timeout: Duration::from_secs(30),
    termination_timeout: Duration::from_secs(2),
    stdout_bytes: 1024 * 1024,
    stderr_bytes: 256 * 1024,
    progress_segment_bytes: 64 * 1024,
    repository_bytes: 8 * 1024 * 1024,
    repository_entries: 1024,
    open_files: 128,
    address_space_bytes: 512 * 1024 * 1024,
    quota_poll_interval: Duration::from_millis(25),
};

impl Git {
    /// Seed the URL-derived private mirror cache from one bounded, tracked Git
    /// bundle without making local-path sources part of production semantics.
    pub(crate) async fn import_fixture(
        &self,
        storage_dir: &Path,
        fixture: &Path,
        source_date_epoch: i64,
    ) -> Result<(), Error> {
        let cache_lock = self.acquire_cache_lock(storage_dir, CacheLockMode::Exclusive).await?;
        let stored_path = self.stored_path(storage_dir);
        let marker_path = self.mutation_marker_path(storage_dir);
        reject_existing_fixture_cache(&stored_path, &marker_path)?;
        validate_full_commit(&self.commit)?;

        let cache_parent = stored_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Git fixture cache has no parent directory")
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".cast-git-fixture-")
            .tempdir_in(cache_parent)
            .map_err(|source| Error::CreateStaging {
                parent: cache_parent.to_owned(),
                source,
            })?;
        fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))?;

        let private_bundle = staging.path().join("source.bundle");
        copy_bounded_fixture_bundle(fixture, &private_bundle)?;
        let staged_mirror = staging.path().join("mirror.git");
        let bundle_repo = gitwrap::Repository::clone_fixture_bundle_mirror_with_limits(
            &staged_mirror,
            &private_bundle,
            &self.url,
            FIXTURE_GIT_LIMITS,
        )
        .await?;
        drop(bundle_repo);

        let repo = gitwrap::Repository::open_private_mirror_with_limits(
            &staged_mirror,
            &self.url,
            FIXTURE_GIT_LIMITS,
        )
        .await
        .map_err(|source| {
            if source.mirror_origin_mismatch() {
                Error::OriginMismatch {
                    cache: staged_mirror.clone(),
                }
            } else {
                source.into()
            }
        })?;
        repo.secure_private_mirror()?;
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        if resolved_hash != self.commit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Git fixture resolved commit {resolved_hash}, but the lock requires {}",
                    self.commit
                ),
            )
            .into());
        }
        reject_gitlinks(&repo, &resolved_hash).await?;

        let expected = self.materialization_sha256.as_deref().ok_or_else(|| {
            Error::MissingMaterializationDigest {
                index: self.original_index,
                commit: resolved_hash.clone(),
            }
        })?;
        let stored = StoredGit {
            name: self.name.clone(),
            was_cached: false,
            repo,
            resolved_hash: resolved_hash.clone(),
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
        };
        let checkout = staging.path().join("normalized-checkout");
        let found = stored.export_normalized(&checkout, source_date_epoch).await?;
        if found != expected {
            return Err(Error::MaterializationDigestMismatch {
                index: self.original_index,
                commit: resolved_hash,
                expected: expected.to_owned(),
                found,
            });
        }
        drop(stored);

        let staged_parent = open_parent_directory(&staged_mirror, "staged fixture mirror")?;
        let staged_name = path_file_name(&staged_mirror, "staged fixture mirror")?;
        let staged_root = openat_directory(&staged_parent, &staged_name)?;
        let staged_identity = FileIdentity::from_file(&staged_root)?;
        let target_name = path_file_name(&stored_path, "fixture cache mirror")?;
        let marker = self.begin_cache_mutation(storage_dir)?;
        renameat_noreplace(&staged_parent, &staged_name, &cache_lock._parent, &target_name)?;
        if identity_at(&cache_lock._parent, &target_name)? != Some(staged_identity) {
            return Err(io::Error::other("published Git fixture cache is not the verified staged mirror").into());
        }
        cache_lock._parent.sync_all()?;
        #[cfg(test)]
        if take_fixture_import_post_publication_failure(&stored_path) {
            return Err(io::Error::other("injected Git fixture failure after durable publication").into());
        }

        let final_repo = gitwrap::Repository::open_private_mirror_with_limits(
            &stored_path,
            &self.url,
            FIXTURE_GIT_LIMITS,
        )
        .await?;
        final_repo.secure_private_mirror()?;
        let final_commit = final_repo.peel_commit(&self.commit).await?;
        if final_commit != self.commit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "published Git fixture cache no longer resolves the exact locked commit",
            )
            .into());
        }
        reject_gitlinks(&final_repo, &final_commit).await?;
        drop(final_repo);
        staging.close()?;
        marker.commit()?;
        Ok(())
    }
}

#[cfg(test)]
static FIXTURE_IMPORT_POST_PUBLICATION_FAILURE: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn arm_fixture_import_post_publication_failure(cache: PathBuf) {
    let mut armed = FIXTURE_IMPORT_POST_PUBLICATION_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(armed.replace(cache).is_none(), "a Git fixture publication fault is already armed");
}

#[cfg(test)]
fn take_fixture_import_post_publication_failure(cache: &Path) -> bool {
    let mut armed = FIXTURE_IMPORT_POST_PUBLICATION_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if armed.as_deref() == Some(cache) {
        armed.take();
        true
    } else {
        false
    }
}

fn reject_existing_fixture_cache(stored_path: &Path, marker_path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(marker_path) {
        Ok(_) => {
            return Err(Error::IncompleteCache {
                cache: stored_path.to_owned(),
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(source.into()),
    }
    match fs::symlink_metadata(stored_path) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to adopt pre-existing Git fixture cache at {stored_path:?}"),
            )
            .into());
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(source.into()),
    }
    Ok(())
}

fn validate_full_commit(commit: &str) -> Result<(), Error> {
    if matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Git fixture import requires a full lowercase commit object ID",
        )
        .into())
    }
}

fn copy_bounded_fixture_bundle(fixture: &Path, destination: &Path) -> Result<(), Error> {
    use std::io::{Read as _, Write as _};

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    let input = options.open(fixture)?;
    let metadata = input.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > FIXTURE_GIT_BUNDLE_MAX_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git fixture bundle must be one non-empty, singly-linked regular file within the byte ceiling",
        )
        .into());
    }

    let mut output_options = fs::OpenOptions::new();
    output_options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let mut output = output_options.open(destination)?;
    let copied = io::copy(&mut input.take(FIXTURE_GIT_BUNDLE_MAX_BYTES + 1), &mut output)?;
    if copied != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git fixture bundle changed length while it was copied into private staging",
        )
        .into());
    }
    output.flush()?;
    output.sync_all()?;
    Ok(())
}
