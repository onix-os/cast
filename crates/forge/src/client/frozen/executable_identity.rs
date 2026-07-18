fn require_pinned_frozen_executable(root: &fs::File, pinned: &PinnedFrozenExecutable) -> Result<(), Error> {
    let descriptor = frozen_executable_witness(&pinned.file, &pinned.binding)?;
    let reopened = open_frozen_executable(root, &pinned.binding, &pinned.expected.resolved_path)?;
    let named = frozen_executable_witness(&reopened, &pinned.binding)?;
    if descriptor != pinned.witness || named != pinned.witness {
        return Err(Error::FrozenExecutablePathReplaced {
            package: pinned.binding.package.clone(),
            path: pinned.binding.path.clone(),
        });
    }
    for symlink in &pinned.symlinks {
        require_pinned_frozen_symlink(root, symlink)?;
    }
    Ok(())
}

fn pin_frozen_root_alias(root: &fs::File, expected: &ExpectedFrozenRootAlias) -> Result<PinnedFrozenRootAlias, Error> {
    let file = open_frozen_root_alias(root, &expected.path)?;
    let witness = frozen_root_alias_witness(&file, &expected.path)?;
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || witness.mode & 0o7777 != 0o777 || witness.links != 1 {
        return Err(Error::FrozenInterpreterRootAliasMetadata {
            path: expected.path.clone(),
            mode: witness.mode,
            links: witness.links,
        });
    }
    let target = read_frozen_root_alias(&file, &expected.path)?;
    if target.as_os_str().as_bytes() != expected.target.as_bytes() {
        return Err(Error::FrozenInterpreterRootAliasTarget {
            path: expected.path.clone(),
            expected: expected.target.clone(),
            actual: target,
        });
    }
    Ok(PinnedFrozenRootAlias {
        file,
        witness,
        expected: expected.clone(),
    })
}

fn require_pinned_frozen_root_alias(root: &fs::File, pinned: &PinnedFrozenRootAlias) -> Result<(), Error> {
    let descriptor = frozen_root_alias_witness(&pinned.file, &pinned.expected.path)?;
    let reopened = open_frozen_root_alias(root, &pinned.expected.path)?;
    let named = frozen_root_alias_witness(&reopened, &pinned.expected.path)?;
    let descriptor_target = read_frozen_root_alias(&pinned.file, &pinned.expected.path)?;
    let named_target = read_frozen_root_alias(&reopened, &pinned.expected.path)?;
    if descriptor != pinned.witness
        || named != pinned.witness
        || descriptor_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
        || named_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
    {
        return Err(Error::FrozenInterpreterRootAliasChanged {
            path: pinned.expected.path.clone(),
        });
    }
    Ok(())
}

fn open_frozen_root_alias(root: &fs::File, path: &Path) -> Result<fs::File, Error> {
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenInterpreterRootAlias { path: path.to_owned() })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV,
    )
    .map_err(|source| Error::OpenFrozenInterpreterRootAlias {
        path: path.to_owned(),
        source,
    })
}

fn frozen_root_alias_witness(file: &fs::File, path: &Path) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenInterpreterRootAlias {
            path: path.to_owned(),
            source,
        })
}

fn read_frozen_root_alias(file: &fs::File, path: &Path) -> Result<OsString, Error> {
    let mut target = [0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    // SAFETY: the O_PATH descriptor pins the exact symlink and the output
    // buffer is writable for its complete length.
    let read =
        unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read < 0 {
        return Err(Error::ReadFrozenInterpreterRootAlias {
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadFrozenInterpreterRootAlias {
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenInterpreterRootAliasTargetTooLong {
            path: path.to_owned(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: read,
        });
    }
    Ok(OsString::from_vec(target[..read].to_vec()))
}

fn require_frozen_executable_binding_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_BINDINGS {
        Err(Error::FrozenExecutableBindingLimit {
            limit: MAX_FROZEN_EXECUTABLE_BINDINGS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn require_frozen_executable_path(binding: &FrozenExecutableBinding) -> Result<&str, Error> {
    let raw = binding.path.as_os_str().as_bytes();
    if raw.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err(Error::FrozenExecutablePathByteLimit {
            limit: MAX_FROZEN_EXECUTABLE_PATH_BYTES,
            actual: raw.len(),
        });
    }

    let components = raw
        .split(|byte| *byte == b'/')
        .filter(|component| !component.is_empty())
        .count();
    if components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(Error::FrozenExecutablePathDepthLimit {
            limit: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
            actual: components,
        });
    }

    let path = std::str::from_utf8(raw).map_err(|_| Error::FrozenExecutablePathEncoding { bytes: raw.len() })?;
    if require_materialized_frozen_path_policy(path).is_err() || !is_normalized_frozen_path(path) {
        // The path is known to be bounded before it is copied into this
        // diagnostic. Oversized or non-UTF-8 inputs never reach this branch.
        return Err(Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    Ok(path)
}

fn require_frozen_executable_package_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_PACKAGES {
        Err(Error::FrozenExecutablePackageLimit {
            limit: MAX_FROZEN_EXECUTABLE_PACKAGES,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_closure_id_bytes(package: &package::Id, total: &mut usize) -> Result<(), Error> {
    let actual = total.checked_add(package.as_str().len()).unwrap_or(usize::MAX);
    if actual > MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES {
        return Err(Error::FrozenExecutableClosureIdByteLimit {
            limit: MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn account_frozen_binding_bytes(
    binding: &FrozenExecutableBinding,
    additional: usize,
    total: &mut usize,
) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES {
        return Err(Error::FrozenExecutableBindingByteLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn require_frozen_executable_layout_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_LAYOUTS {
        Err(Error::FrozenExecutableLayoutLimit {
            limit: MAX_FROZEN_EXECUTABLE_LAYOUTS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_layout_bytes(
    package: &package::Id,
    path: &Path,
    additional: usize,
    total: &mut usize,
) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES {
        return Err(Error::FrozenExecutableLayoutByteLimit {
            package: package.clone(),
            path: path.to_owned(),
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn require_frozen_executable_directory_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS {
        Err(Error::FrozenExecutableDirectoryLimit {
            limit: MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_executable_directory_bytes(additional: usize, total: &mut usize) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES {
        return Err(Error::FrozenExecutableDirectoryByteLimit {
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn frozen_executable_layout_auxiliary_bytes(file: &StonePayloadLayoutFile) -> usize {
    match file {
        StonePayloadLayoutFile::Symlink(target, _) => target.len(),
        StonePayloadLayoutFile::Unknown(source, _) => source.len(),
        StonePayloadLayoutFile::Regular(..)
        | StonePayloadLayoutFile::Directory(_)
        | StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_) => 0,
    }
}

fn require_frozen_shebang_interpreter_count(binding: &FrozenExecutableBinding, actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_SHEBANG_INTERPRETERS {
        Err(Error::FrozenShebangInterpreterLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_SHEBANG_INTERPRETERS,
        })
    } else {
        Ok(())
    }
}

fn require_frozen_executable_interpreter_count(binding: &FrozenExecutableBinding, actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_INTERPRETERS {
        Err(Error::FrozenExecutableInterpreterLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_INTERPRETERS,
        })
    } else {
        Ok(())
    }
}

fn reserve_frozen_pinned_files(
    binding: &FrozenExecutableBinding,
    current: &mut usize,
    additional: usize,
) -> Result<(), Error> {
    let actual = current.saturating_add(additional);
    if actual > MAX_FROZEN_EXECUTABLE_PINNED_FILES {
        return Err(Error::FrozenExecutablePinnedFileLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_PINNED_FILES,
            actual,
        });
    }
    *current = actual;
    Ok(())
}

fn account_frozen_executable_bytes(
    binding: &FrozenExecutableBinding,
    length: u64,
    total: &mut u64,
) -> Result<(), Error> {
    if length > MAX_FROZEN_EXECUTABLE_BYTES {
        return Err(Error::FrozenExecutableByteLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_BYTES,
            actual: length,
        });
    }
    let next = total.checked_add(length).ok_or(Error::FrozenExecutableTotalByteLimit {
        limit: MAX_TOTAL_FROZEN_EXECUTABLE_BYTES,
        actual: u64::MAX,
    })?;
    if next > MAX_TOTAL_FROZEN_EXECUTABLE_BYTES {
        return Err(Error::FrozenExecutableTotalByteLimit {
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_BYTES,
            actual: next,
        });
    }
    *total = next;
    Ok(())
}

fn resolve_frozen_symlink_target(link: &Path, target: &str) -> Option<PathBuf> {
    normalize_frozen_symlink_target(link, target)
        .filter(|resolved| resolved.to_str().is_some_and(is_normalized_frozen_path))
}

/// Lexically normalize one bounded symlink target without deciding which
/// absolute namespace the caller is allowed to consume.
///
/// Frozen executable admission applies the `/usr` policy in
/// [`resolve_frozen_symlink_target`]. Boot projection planning needs the
/// namespace-neutral result first so it can distinguish a malformed target
/// from a well-formed escape and report the latter through its dedicated
/// fail-closed error.
fn normalize_frozen_symlink_target(link: &Path, target: &str) -> Option<PathBuf> {
    if target.is_empty()
        || !frozen_executable_symlink_target_length_is_admitted(target.len())
        || target.as_bytes().contains(&0)
        || target.ends_with('/')
        || target.contains("//")
    {
        return None;
    }

    let target_path = Path::new(target);
    let mut components = Vec::<OsString>::new();
    if !target_path.is_absolute() {
        for component in link.parent()?.components() {
            match component {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(component) => components.push(component.to_owned()),
                std::path::Component::CurDir | std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                    return None;
                }
            }
        }
    }
    for component in target_path.components() {
        match component {
            std::path::Component::RootDir => {
                if !target_path.is_absolute() {
                    return None;
                }
                components.clear();
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop()?;
            }
            std::path::Component::Normal(component) => components.push(component.to_owned()),
            std::path::Component::Prefix(_) => return None,
        }
    }

    let mut resolved = PathBuf::from("/");
    resolved.extend(components);
    Some(resolved)
}

fn frozen_executable_symlink_target_length_is_admitted(actual: usize) -> bool {
    actual <= MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
}

fn pin_frozen_symlink(root: &fs::File, expected: &ExpectedFrozenSymlink) -> Result<PinnedFrozenSymlink, Error> {
    let binding = FrozenExecutableBinding {
        package: expected.package.clone(),
        path: expected.path.clone(),
    };
    let file = open_frozen_symlink(root, &binding, &expected.path)?;
    let witness = frozen_symlink_witness(&file, &binding, &expected.path)?;
    if witness.mode != expected.mode || witness.mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || witness.links != 1 {
        return Err(Error::FrozenExecutableSymlinkMetadataMismatch {
            package: binding.package.clone(),
            path: expected.path.clone(),
            expected: expected.mode,
            actual: witness.mode,
            links: witness.links,
        });
    }
    let actual = read_frozen_symlink(&file, &binding, &expected.path)?;
    if actual.as_os_str().as_bytes() != expected.target.as_bytes() {
        return Err(Error::FrozenExecutableSymlinkTargetMismatch {
            package: binding.package.clone(),
            path: expected.path.clone(),
            expected: expected.target.clone(),
            actual,
        });
    }
    Ok(PinnedFrozenSymlink {
        file,
        witness,
        expected: expected.clone(),
    })
}

fn require_pinned_frozen_symlink(root: &fs::File, pinned: &PinnedFrozenSymlink) -> Result<(), Error> {
    let binding = FrozenExecutableBinding {
        package: pinned.expected.package.clone(),
        path: pinned.expected.path.clone(),
    };
    let descriptor = frozen_symlink_witness(&pinned.file, &binding, &pinned.expected.path)?;
    let reopened = open_frozen_symlink(root, &binding, &pinned.expected.path)?;
    let named = frozen_symlink_witness(&reopened, &binding, &pinned.expected.path)?;
    if descriptor != pinned.witness || named != pinned.witness {
        return Err(Error::FrozenExecutableSymlinkChanged {
            package: binding.package.clone(),
            path: pinned.expected.path.clone(),
        });
    }
    let descriptor_target = read_frozen_symlink(&pinned.file, &binding, &pinned.expected.path)?;
    let named_target = read_frozen_symlink(&reopened, &binding, &pinned.expected.path)?;
    if descriptor_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
        || named_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
    {
        return Err(Error::FrozenExecutableSymlinkChanged {
            package: binding.package.clone(),
            path: pinned.expected.path.clone(),
        });
    }
    Ok(())
}

fn require_frozen_executable_metadata(
    binding: &FrozenExecutableBinding,
    expected: &ExpectedFrozenExecutable,
    witness: FrozenExecutableWitness,
) -> Result<(), Error> {
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG || witness.links != 1 {
        return Err(Error::FrozenExecutableNotIndependentRegular {
            package: binding.package.clone(),
            path: binding.path.clone(),
            mode: witness.mode,
            links: witness.links,
        });
    }
    if witness.mode != expected.mode || witness.mode & 0o111 == 0 {
        return Err(Error::FrozenExecutableModeMismatch {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected.mode,
            actual: witness.mode,
        });
    }
    Ok(())
}
