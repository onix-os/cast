/// Compile-time witness used by Mason to prove its delegated-fixture feature
/// really enables this otherwise absent API.
pub const FIXTURE_TEST_SUPPORT_ENABLED: bool = true;

impl Repository {
    /// Clone one direct, private Git bundle into a canonical mirror for an
    /// exact HTTPS identity. This API is compiled only for fixture proofs.
    pub async fn clone_fixture_bundle_mirror_with_limits(
        path: &Path,
        bundle: &Path,
        origin: &Url,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        validate_fixture_origin(origin)?;
        let path = path::absolute(path).map_err(InnerError::from)?;
        ensure_destination_absent(&path)?;
        let parent = path.parent().ok_or_else(|| {
            InnerError::Io(std_io::Error::new(
                std_io::ErrorKind::InvalidInput,
                "Git fixture clone destination has no parent",
            ))
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".gitwrap-fixture-mirror-")
            .tempdir_in(parent)
            .map_err(InnerError::from)?;
        let private_bundle = staging.path().join("source.bundle");
        if let Err(error) = copy_fixture_bundle(bundle, &private_bundle, limits.repository_bytes) {
            return close_staging_after_error(staging, error);
        }
        let staged_path = staging.path().join("repository.git");
        let result = run_git_monitored(
            [
                OsStr::new("clone"),
                OsStr::new("--mirror"),
                OsStr::new("--no-hardlinks"),
                OsStr::new("--no-recurse-submodules"),
                OsStr::new("--no-progress"),
                private_bundle.as_os_str(),
                staged_path.as_os_str(),
            ],
            limits,
            &staged_path,
            None::<fn(FetchProgress)>,
        )
        .await;
        if let Err(error) = result {
            return close_staging_after_error(staging, error);
        }
        if let Err(error) = verify_repository_usage(&staged_path, limits) {
            return close_staging_after_error(staging, error);
        }
        let root = match open_repository_directory(&staged_path) {
            Ok(root) => root,
            Err(error) => return close_staging_after_error(staging, error),
        };
        let reset_origin = run_git_in_directory(
            [
                OsStr::new("remote"),
                OsStr::new("set-url"),
                OsStr::new("--"),
                OsStr::new("origin"),
                OsStr::new(origin.as_str()),
            ],
            limits,
            &root,
            Some(MonitoredRepository::directory(&root)?),
            None::<fn(FetchProgress)>,
        )
        .await;
        if let Err(error) = reset_origin {
            return close_staging_after_error(staging, error);
        }
        if let Err(error) = verify_repository_usage_directory(&root, limits) {
            return close_staging_after_error(staging, error);
        }
        if let Err(error) = secure_mirror_permissions(&root) {
            return close_staging_after_error(staging, error);
        }
        let object_format = match inspect_private_mirror_config(&root, origin, limits).await {
            Ok(object_format) => object_format,
            Err(error) => return close_staging_after_error(staging, error),
        };
        if let Err(error) = write_canonical_mirror_config(&root, origin, object_format) {
            return close_staging_after_error(staging, error);
        }
        if let Err(error) = rename_noreplace(&staged_path, &path) {
            return close_staging_after_error(staging, error);
        }
        if let Err(error) = verify_repository_path_identity(&path, &root) {
            return close_staging_and_remove_install(staging, &path, &root, error);
        }
        if let Err(error) =
            secure_mirror_permissions(&root).and_then(|()| verify_canonical_mirror_config(&root, origin, object_format))
        {
            return close_staging_and_remove_install(staging, &path, &root, error);
        }
        let repository = match Self::open_private_mirror_with_limits(&path, origin, limits).await {
            Ok(repository) => repository,
            Err(error) => return close_staging_and_remove_install(staging, &path, &root, error),
        };
        if let Err(source) = staging.close() {
            drop(repository);
            let cleanup = if verify_repository_path_identity(&path, &root).is_ok() {
                remove_path(&path).err().unwrap_or(source)
            } else {
                std_io::Error::other("refusing to remove a replacement Git fixture mirror")
            };
            return Err(InnerError::Cleanup(cleanup).into());
        }
        drop(root);
        Ok(repository)
    }
}

fn validate_fixture_origin(origin: &Url) -> Result<(), Error> {
    validate_transport_url(origin)?;
    if origin.scheme() == "https" {
        Ok(())
    } else {
        Err(InnerError::InvalidFixtureOrigin.into())
    }
}

fn copy_fixture_bundle(source: &Path, destination: &Path, byte_limit: u64) -> Result<(), Error> {
    use fs_err::os::unix::fs::OpenOptionsExt as _;
    use std::io::{Read as _, Write as _};

    let mut input_options = fs::OpenOptions::new();
    input_options
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    let input = input_options.open(source).map_err(InnerError::from)?;
    let metadata = input.metadata().map_err(InnerError::from)?;
    let allocated = metadata.blocks().saturating_mul(512);
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > byte_limit
        || allocated > byte_limit
    {
        return Err(InnerError::InvalidFixtureBundle.into());
    }

    let mut output_options = fs::OpenOptions::new();
    output_options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    let mut output = output_options.open(destination).map_err(InnerError::from)?;
    let copied = std_io::copy(&mut input.take(byte_limit.saturating_add(1)), &mut output)
        .map_err(InnerError::from)?;
    if copied != metadata.len() {
        return Err(InnerError::InvalidFixtureBundle.into());
    }
    output.flush().map_err(InnerError::from)?;
    output.sync_all().map_err(InnerError::from)?;
    Ok(())
}
