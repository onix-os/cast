const MAX_GIT_IDENTIFIER_BYTES: usize = 4096;
const MAX_GIT_VALUE_BYTES: usize = 64 * 1024;

fn validate_revision_argument(value: &str) -> Result<(), Error> {
    validate_non_option_argument(value, "revision")
}

fn validate_remote_argument(value: &str) -> Result<(), Error> {
    validate_non_option_argument(value, "remote name")
}

fn validate_non_option_argument(value: &str, argument: &'static str) -> Result<(), Error> {
    if value.is_empty() || value.starts_with('-') || value.len() > MAX_GIT_IDENTIFIER_BYTES {
        Err(InnerError::InvalidArgument { argument }.into())
    } else {
        Ok(())
    }
}

fn validate_value_argument(value: &str, argument: &'static str) -> Result<(), Error> {
    if value.is_empty() || value.len() > MAX_GIT_VALUE_BYTES {
        Err(InnerError::InvalidArgument { argument }.into())
    } else {
        Ok(())
    }
}

/// Reject unknown schemes before Git can dispatch a `git-remote-*` helper from
/// PATH. Every accepted scheme is handled by Git itself or its explicitly
/// constrained SSH transport.
fn validate_transport_url(url: &Url) -> Result<(), Error> {
    validate_value_argument(url.as_str(), "transport URL")?;
    match url.scheme() {
        "file" | "https" | "ssh" => Ok(()),
        scheme => Err(InnerError::UnsupportedTransportScheme {
            scheme: scheme.to_owned(),
        }
        .into()),
    }
}

async fn clone_mirror_impl<F>(path: &Path, url: &Url, limits: Limits, callback: Option<F>) -> Result<Repository, Error>
where
    F: Fn(FetchProgress),
{
    let limits = limits.validate()?;
    validate_transport_url(url)?;
    let path = path::absolute(path).map_err(InnerError::from)?;
    ensure_destination_absent(&path)?;
    let parent = path.parent().ok_or_else(|| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "Git clone destination has no parent",
        ))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".gitwrap-mirror-")
        .tempdir_in(parent)
        .map_err(InnerError::from)?;
    let staged_path = staging.path().join("repository.git");
    let progress = callback.is_some();
    let result = run_git_monitored(
        [
            OsStr::new("clone"),
            OsStr::new("--mirror"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            if progress {
                OsStr::new("--progress")
            } else {
                OsStr::new("--no-progress")
            },
            OsStr::new(url.as_str()),
            staged_path.as_os_str(),
        ],
        limits,
        &staged_path,
        callback,
    )
    .await;
    if let Err(error) = result {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_usage(&staged_path, limits) {
        return close_staging_after_error(staging, error);
    }
    // Pin the exact staged inode before exposing its name in the caller's
    // directory. Opening the final path after rename would allow a concurrent
    // replacement to make us return a handle for a repository we did not
    // validate.
    let root = match open_repository_directory(&staged_path) {
        Ok(root) => root,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = secure_mirror_permissions(&root) {
        return close_staging_after_error(staging, error);
    }
    let object_format = match inspect_private_mirror_config(&root, url, limits).await {
        Ok(object_format) => object_format,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = write_canonical_mirror_config(&root, url, object_format) {
        return close_staging_after_error(staging, error);
    }
    let identity = match RepositoryIdentity::from_directory(&root) {
        Ok(identity) => identity,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = rename_noreplace(&staged_path, &path) {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_path_identity(&path, &root) {
        return close_staging_and_remove_install(staging, &path, &root, error);
    }
    if let Err(error) =
        secure_mirror_permissions(&root).and_then(|()| verify_canonical_mirror_config(&root, url, object_format))
    {
        return close_staging_and_remove_install(staging, &path, &root, error);
    }
    if let Err(source) = staging.close() {
        let cleanup = remove_path(&path).err().unwrap_or(source);
        return Err(InnerError::Cleanup(cleanup).into());
    }

    Ok(Repository {
        path,
        limits,
        identity: Some(identity),
        mirror: Some(MirrorIdentity {
            origin: url.clone(),
            object_format,
        }),
    })
}

async fn clone_to_staged(
    source: &fs::File,
    source_path: &Path,
    path: &Path,
    limits: Limits,
) -> Result<fs::File, Error> {
    ensure_destination_absent(path)?;
    let parent = path.parent().ok_or_else(|| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "Git clone destination has no parent",
        ))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".gitwrap-checkout-")
        .tempdir_in(parent)
        .map_err(InnerError::from)?;
    let staged_path = staging.path().join("checkout");
    let result = run_git_in_directory(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new("."),
            staged_path.as_os_str(),
        ],
        limits,
        source,
        Some(MonitoredRepository::Path(staged_path.clone())),
        None::<fn(FetchProgress)>,
    )
    .await;
    if let Err(error) = result {
        return close_staging_after_error(staging, error);
    }
    let reset_origin = run_git(
        [
            OsStr::new("-C"),
            staged_path.as_os_str(),
            OsStr::new("remote"),
            OsStr::new("set-url"),
            OsStr::new("origin"),
            source_path.as_os_str(),
        ],
        limits,
    )
    .await;
    if let Err(error) = reset_origin {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_usage(&staged_path, limits) {
        return close_staging_after_error(staging, error);
    }
    let root = match open_repository_directory(&staged_path) {
        Ok(root) => root,
        Err(error) => return close_staging_after_error(staging, error),
    };
    if let Err(error) = rename_noreplace(&staged_path, path) {
        return close_staging_after_error(staging, error);
    }
    if let Err(error) = verify_repository_path_identity(path, &root) {
        return close_staging_and_remove_install(staging, path, &root, error);
    }
    if let Err(source) = staging.close() {
        let cleanup = remove_path(path).err().unwrap_or(source);
        return Err(InnerError::Cleanup(cleanup).into());
    }
    Ok(root)
}

fn close_staging_after_error<T>(staging: tempfile::TempDir, error: Error) -> Result<T, Error> {
    match staging.close() {
        Ok(()) => Err(error),
        Err(source) => Err(InnerError::Cleanup(source).into()),
    }
}

fn close_staging_and_remove_install<T>(
    staging: tempfile::TempDir,
    installed: &Path,
    installed_root: &fs::File,
    error: Error,
) -> Result<T, Error> {
    let remove_error = if verify_repository_path_identity(installed, installed_root).is_ok() {
        remove_path(installed).err()
    } else {
        None
    };
    let staging_error = staging.close().err();
    if let Some(source) = remove_error.or(staging_error) {
        Err(InnerError::Cleanup(source).into())
    } else {
        Err(error)
    }
}

fn ensure_destination_absent(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InnerError::DestinationExists.into()),
        Err(error) if error.kind() == std_io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InnerError::Io(error).into()),
    }
}

/// Atomically install a private clone without replacing a destination that
/// appeared after preflight.
fn rename_noreplace(source: &Path, target: &Path) -> Result<(), Error> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "staged Git path contains NUL",
        ))
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        InnerError::Io(std_io::Error::new(
            std_io::ErrorKind::InvalidInput,
            "final Git path contains NUL",
        ))
    })?;
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            source.as_ptr(),
            nix::libc::AT_FDCWD,
            target.as_ptr(),
            1_u32, // RENAME_NOREPLACE
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let error = std_io::Error::last_os_error();
        if error.kind() == std_io::ErrorKind::AlreadyExists {
            Err(InnerError::DestinationExists.into())
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

