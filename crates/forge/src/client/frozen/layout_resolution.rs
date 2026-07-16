fn bounded_frozen_layouts(
    client: &Client,
    packages: &[package::Id],
    deadline: Instant,
    operation: FrozenLayoutQueryOperation,
) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
    let outcome = client.layout_db.query_bounded(
        packages,
        db::layout::QueryBounds {
            max_rows: MAX_FROZEN_EXECUTABLE_LAYOUTS,
            max_string_bytes: MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES,
        },
        || Instant::now() <= deadline,
    )?;
    match outcome {
        db::layout::BoundedQueryOutcome::Complete(layouts) => Ok(layouts),
        db::layout::BoundedQueryOutcome::PackageLimit { limit, actual } => {
            Err(Error::FrozenExecutablePackageLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::PackageIdByteLimit { limit, actual } => {
            Err(Error::FrozenExecutableClosureIdByteLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::RowLimit { limit, actual } => {
            Err(Error::FrozenExecutableLayoutLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::StringByteLimit { limit, actual } => {
            Err(Error::FrozenLayoutStorageByteLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::Cancelled => Err(operation.timeout()),
    }
}

fn require_frozen_executables<F>(
    client: &Client,
    materialized_root: MaterializedFrozenRoot,
    packages: &[package::Id],
    bindings: &[FrozenExecutableBinding],
    mut checkpoint: F,
) -> Result<FrozenRootGuard, Error>
where
    F: FnMut(&FrozenExecutableBinding, FrozenExecutableCheckpoint),
{
    // The clock and all input bounds begin before any closure set or database
    // layout is discovered. An oversized request must not first allocate or
    // query an unbounded representation and only then be rejected.
    let deadline = Instant::now() + FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT;
    require_frozen_executable_deadline(deadline)?;
    require_frozen_executable_package_count(packages.len())?;
    require_frozen_executable_binding_count(bindings.len())?;

    let mut closure_id_bytes = 0usize;
    let mut package_set = BTreeSet::new();
    for package in packages {
        require_frozen_executable_deadline(deadline)?;
        account_frozen_closure_id_bytes(package, &mut closure_id_bytes)?;
        if !package_set.insert(package) {
            return Err(Error::DuplicateFrozenPackage(package.clone()));
        }
    }

    let mut binding_bytes = 0usize;
    for binding in bindings {
        require_frozen_executable_deadline(deadline)?;
        // Inspect the borrowed raw path before provider lookup or any path
        // clone. Oversized and non-UTF-8 attacker input therefore produces a
        // bounded diagnostic rather than being copied into an error value.
        let path = require_frozen_executable_path(binding)?;
        account_frozen_binding_bytes(binding, path.len(), &mut binding_bytes)?;
        if !package_set.contains(&binding.package) {
            return Err(Error::FrozenExecutableProviderOutsideClosure {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
    }

    let mut bindings = bindings.to_vec();
    bindings.sort();
    bindings.dedup();

    // Consume the exact descriptor opened before publication.  No successful
    // verification path is allowed to reopen the configured destination and
    // treat that newly found inode as the materialized root.
    let (root_path, root, root_witness) = materialized_root.into_guard_root()?;
    if bindings.is_empty() {
        let guard = FrozenRootGuard {
            root_path,
            root,
            root_witness,
            executables: Vec::new(),
            root_aliases: BTreeMap::new(),
        };
        guard.revalidate_until(deadline)?;
        return Ok(guard);
    }

    // Interpreter ownership is discovered from the complete frozen closure,
    // never from the host or from a provider lookup performed after planning.
    let layouts = bounded_frozen_layouts(
        client,
        packages,
        deadline,
        FrozenLayoutQueryOperation::ExecutableVerification,
    )?;
    require_frozen_executable_deadline(deadline)?;
    require_frozen_executable_layout_count(layouts.len())?;

    let mut prepared_layouts = Vec::with_capacity(layouts.len());
    let mut layout_bytes = 0usize;
    for (package, layout) in layouts {
        require_frozen_executable_deadline(deadline)?;
        if !package_set.contains(&package) {
            return Err(Error::UnexpectedFrozenLayoutPackage(package));
        }
        // Direct database fixtures can bypass normal Stone ingestion. Apply
        // the same canonical raw-target contract again before executable
        // verification materializes or accounts any layout path.
        let raw_path = require_usr_relative_stone_layout(&package, &layout)?;
        let path = PathBuf::from(materialized_frozen_layout_path(raw_path));
        let Some(path_str) = path.to_str() else {
            return Err(Error::InvalidFrozenLayoutPath {
                package,
                path: path.to_string_lossy().into_owned(),
            });
        };
        if path_str.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES || !is_normalized_frozen_path(path_str) {
            return Err(Error::InvalidFrozenLayoutPath {
                package,
                path: path_str.to_owned(),
            });
        }
        let auxiliary_bytes = frozen_executable_layout_auxiliary_bytes(&layout.file);
        account_frozen_layout_bytes(
            &package,
            &path,
            package
                .as_str()
                .len()
                .saturating_add(path_str.len())
                .saturating_add(auxiliary_bytes),
            &mut layout_bytes,
        )?;
        let is_directory = matches!(layout.file, StonePayloadLayoutFile::Directory(_));
        let entry = match layout.file {
            StonePayloadLayoutFile::Regular(digest, _) => FrozenExecutableLayout::Regular {
                digest,
                mode: layout.mode,
            },
            StonePayloadLayoutFile::Symlink(target, _) => {
                require_frozen_layout_symlink_target(&package, &target)?;
                FrozenExecutableLayout::Symlink {
                    target: target.to_string(),
                    mode: layout.mode,
                }
            }
            StonePayloadLayoutFile::Directory(_) => FrozenExecutableLayout::Directory {
                uid: layout.uid,
                gid: layout.gid,
                mode: layout.mode,
                tag: layout.tag,
            },
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => FrozenExecutableLayout::Other,
        };
        prepared_layouts.push(PreparedFrozenExecutableLayout {
            package,
            path,
            entry,
            is_directory,
        });
    }

    let directory_redirects = frozen_executable_directory_redirects(&prepared_layouts, deadline)?;
    let mut provider_layouts = BTreeMap::<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>::new();
    let mut path_providers = BTreeMap::<PathBuf, BTreeSet<package::Id>>::new();
    for PreparedFrozenExecutableLayout {
        package,
        path,
        entry,
        is_directory: _,
    } in prepared_layouts
    {
        require_frozen_executable_deadline(deadline)?;
        let layouts = provider_layouts.entry(package.clone()).or_default();
        if let Some(previous) = layouts.get(&path) {
            if previous.is_identical_directory(&entry) {
                continue;
            }
            return Err(Error::DuplicateFrozenExecutableLayout { package, path });
        }
        layouts.insert(path.clone(), entry);
        path_providers.entry(path).or_default().insert(package);
    }

    let mut expected = BTreeMap::<(package::Id, PathBuf), ExpectedFrozenExecutable>::new();
    for binding in &bindings {
        require_frozen_executable_deadline(deadline)?;
        let executable = resolve_frozen_executable_layout(
            binding,
            &provider_layouts,
            &path_providers,
            &directory_redirects,
            deadline,
        )?;
        expected.insert((binding.package.clone(), binding.path.clone()), executable);
    }

    let mut total_bytes = 0u64;
    let mut pinned_file_count = 0usize;
    let mut verified = BTreeMap::<FrozenExecutableBinding, Option<FrozenExecutableInterpreter>>::new();
    let mut pinned = Vec::<PinnedFrozenExecutable>::new();
    let mut pinned_root_aliases = BTreeMap::<PathBuf, PinnedFrozenRootAlias>::new();

    for declared in &bindings {
        let key = (declared.package.clone(), declared.path.clone());
        let mut binding = declared.clone();
        let mut executable = expected
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::MissingFrozenExecutableLayout {
                package: declared.package.clone(),
                path: declared.path.clone(),
            })?;
        let mut chain = BTreeSet::<FrozenExecutableBinding>::new();
        let mut shebang_interpreter_count = 0usize;
        let mut interpreter_count = 0usize;
        let mut require_terminal_elf = false;

        loop {
            if !chain.insert(binding.clone()) {
                return Err(Error::FrozenExecutableInterpreterCycle {
                    package: binding.package,
                    path: binding.path,
                });
            }

            let interpreter = if let Some(interpreter) = verified.get(&binding) {
                interpreter.clone()
            } else {
                reserve_frozen_pinned_files(&binding, &mut pinned_file_count, executable.symlinks.len() + 1)?;
                let (interpreter, retained) =
                    verify_frozen_executable(&root, &binding, executable, deadline, &mut total_bytes, &mut checkpoint)?;
                verified.insert(binding.clone(), interpreter.clone());
                pinned.push(retained);
                interpreter
            };

            if require_terminal_elf && interpreter.is_some() {
                return Err(Error::FrozenElfInterpreterIsInterpreted {
                    package: binding.package,
                    path: binding.path,
                });
            }

            let Some(interpreter_kind) = interpreter else {
                break;
            };
            let interpreter = interpreter_kind.binding();
            if let Some(alias) = interpreter.root_alias.clone()
                && !pinned_root_aliases.contains_key(&alias.path)
            {
                require_frozen_executable_deadline(deadline)?;
                reserve_frozen_pinned_files(declared, &mut pinned_file_count, 1)?;
                let pinned = pin_frozen_root_alias(&root, &alias)?;
                pinned_root_aliases.insert(alias.path.clone(), pinned);
            }
            interpreter_count = interpreter_count.saturating_add(1);
            require_frozen_executable_interpreter_count(declared, interpreter_count)?;
            if interpreter_kind.is_shebang() {
                shebang_interpreter_count = shebang_interpreter_count.saturating_add(1);
                require_frozen_shebang_interpreter_count(declared, shebang_interpreter_count)?;
            }
            require_terminal_elf = matches!(interpreter_kind, FrozenExecutableInterpreter::Elf(_));
            (binding, executable) = resolve_frozen_interpreter_layout(
                &interpreter.path,
                &provider_layouts,
                &path_providers,
                &directory_redirects,
                deadline,
            )?;
        }
    }

    // Keep every inspected inode pinned in the returned proof. Revalidate the
    // complete descriptor/name graph before handing it to the caller so a
    // writer racing a later interpreter cannot invalidate an earlier proof.
    let guard = FrozenRootGuard {
        root_path,
        root,
        root_witness,
        executables: pinned,
        root_aliases: pinned_root_aliases,
    };
    guard.revalidate_until(deadline)?;
    Ok(guard)
}

fn resolve_frozen_executable_layout(
    binding: &FrozenExecutableBinding,
    provider_layouts: &BTreeMap<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>,
    path_providers: &BTreeMap<PathBuf, BTreeSet<package::Id>>,
    directory_redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<ExpectedFrozenExecutable, Error> {
    let mut current = binding.path.clone();
    // Capability resolution pins the owner of the declared entry point.  A
    // symlink may intentionally hand off to another package in the exact
    // frozen closure (for example `cp -> gnu-cp`), but every later hop must
    // have one unambiguous owner.  We never resolve through the host or choose
    // a provider by iteration order.
    let mut provider = binding.package.clone();
    let mut visited = BTreeSet::new();
    let mut symlinks = Vec::new();
    loop {
        reject_frozen_executable_directory_redirect(&current, directory_redirects, deadline)?;
        require_frozen_executable_deadline(deadline)?;
        if !visited.insert(current.clone()) {
            return Err(Error::FrozenExecutableSymlinkCycle {
                package: binding.package.clone(),
                path: current,
            });
        }
        let Some(layout) = provider_layouts
            .get(&provider)
            .and_then(|layouts| layouts.get(&current))
        else {
            if symlinks.is_empty() {
                return Err(Error::MissingFrozenExecutableLayout {
                    package: binding.package.clone(),
                    path: binding.path.clone(),
                });
            }
            return Err(Error::MissingFrozenExecutableSymlinkTarget {
                package: binding.package.clone(),
                binding: binding.path.clone(),
                target: current,
            });
        };
        match layout {
            FrozenExecutableLayout::Regular { digest, mode } => {
                if mode & nix::libc::S_IFMT != nix::libc::S_IFREG || mode & 0o111 == 0 {
                    return Err(Error::FrozenExecutableLayoutNotExecutable {
                        package: provider,
                        path: current,
                        mode: *mode,
                    });
                }
                return Ok(ExpectedFrozenExecutable {
                    digest: *digest,
                    mode: *mode,
                    resolved_path: current,
                    symlinks,
                });
            }
            FrozenExecutableLayout::Symlink { target, mode } => {
                if symlinks.len() == MAX_FROZEN_EXECUTABLE_SYMLINKS {
                    return Err(Error::FrozenExecutableSymlinkLimit {
                        package: binding.package.clone(),
                        path: binding.path.clone(),
                        limit: MAX_FROZEN_EXECUTABLE_SYMLINKS,
                    });
                }
                if mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || mode & 0o7777 != 0o777 {
                    return Err(Error::FrozenExecutableLayoutNotRegular {
                        package: provider,
                        path: current,
                    });
                }
                let next = resolve_frozen_symlink_target(&current, target).ok_or_else(|| {
                    Error::InvalidFrozenExecutableSymlinkTarget {
                        package: provider.clone(),
                        path: current.clone(),
                        target: target.clone(),
                    }
                })?;
                symlinks.push(ExpectedFrozenSymlink {
                    package: provider,
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
                let providers =
                    path_providers
                        .get(&current)
                        .ok_or_else(|| Error::MissingFrozenExecutableSymlinkTarget {
                            package: binding.package.clone(),
                            binding: binding.path.clone(),
                            target: current.clone(),
                        })?;
                if providers.is_empty() {
                    return Err(Error::MissingFrozenExecutableSymlinkTarget {
                        package: binding.package.clone(),
                        binding: binding.path.clone(),
                        target: current,
                    });
                }
                if providers.len() > 1 {
                    return Err(Error::AmbiguousFrozenExecutableSymlinkTarget {
                        package: binding.package.clone(),
                        binding: binding.path.clone(),
                        target: current,
                        providers: providers.iter().cloned().collect(),
                    });
                }
                provider =
                    providers
                        .iter()
                        .next()
                        .cloned()
                        .ok_or_else(|| Error::MissingFrozenExecutableSymlinkTarget {
                            package: binding.package.clone(),
                            binding: binding.path.clone(),
                            target: current.clone(),
                        })?;
            }
            FrozenExecutableLayout::Directory { .. } | FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: provider,
                    path: current,
                });
            }
        }
    }
}

fn resolve_frozen_interpreter_layout(
    interpreter: &Path,
    provider_layouts: &BTreeMap<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>,
    path_providers: &BTreeMap<PathBuf, BTreeSet<package::Id>>,
    directory_redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<(FrozenExecutableBinding, ExpectedFrozenExecutable), Error> {
    let mut current = interpreter.to_owned();
    let mut visited = BTreeSet::new();
    let mut symlinks = Vec::new();
    let mut initial_provider = None;

    loop {
        reject_frozen_executable_directory_redirect(&current, directory_redirects, deadline)?;
        require_frozen_executable_deadline(deadline)?;
        if !visited.insert(current.clone()) {
            return Err(Error::FrozenInterpreterSymlinkCycle { path: current });
        }
        let providers = path_providers
            .get(&current)
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;
        if providers.is_empty() {
            return Err(Error::MissingFrozenInterpreterProvider { path: current });
        }
        if providers.len() > 1 {
            return Err(Error::AmbiguousFrozenInterpreterProvider {
                path: current,
                providers: providers.iter().cloned().collect(),
            });
        }
        let provider = providers
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;
        initial_provider.get_or_insert_with(|| provider.clone());
        let layout = provider_layouts
            .get(&provider)
            .and_then(|layouts| layouts.get(&current))
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;

        match layout {
            FrozenExecutableLayout::Regular { digest, mode } => {
                if mode & nix::libc::S_IFMT != nix::libc::S_IFREG || mode & 0o111 == 0 {
                    return Err(Error::FrozenExecutableLayoutNotExecutable {
                        package: provider,
                        path: current,
                        mode: *mode,
                    });
                }
                let binding = FrozenExecutableBinding {
                    package: provider.clone(),
                    path: interpreter.to_owned(),
                };
                return Ok((
                    binding,
                    ExpectedFrozenExecutable {
                        digest: *digest,
                        mode: *mode,
                        resolved_path: current,
                        symlinks,
                    },
                ));
            }
            FrozenExecutableLayout::Symlink { target, mode } => {
                if symlinks.len() == MAX_FROZEN_EXECUTABLE_SYMLINKS {
                    return Err(Error::FrozenExecutableSymlinkLimit {
                        package: initial_provider.clone().unwrap_or_else(|| provider.clone()),
                        path: interpreter.to_owned(),
                        limit: MAX_FROZEN_EXECUTABLE_SYMLINKS,
                    });
                }
                if mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || mode & 0o7777 != 0o777 {
                    return Err(Error::FrozenExecutableLayoutNotRegular {
                        package: provider,
                        path: current,
                    });
                }
                let next = resolve_frozen_symlink_target(&current, target).ok_or_else(|| {
                    Error::InvalidFrozenExecutableSymlinkTarget {
                        package: provider.clone(),
                        path: current.clone(),
                        target: target.clone(),
                    }
                })?;
                symlinks.push(ExpectedFrozenSymlink {
                    package: provider,
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
            }
            FrozenExecutableLayout::Directory { .. } | FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: provider,
                    path: current,
                });
            }
        }
    }
}

fn frozen_executable_directory_redirects(
    layouts: &[PreparedFrozenExecutableLayout],
    deadline: Instant,
) -> Result<BTreeMap<PathBuf, PathBuf>, Error> {
    let mut directories = BTreeSet::new();
    let mut directory_bytes = 0usize;
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        let mut parent = layout.path.parent();
        while let Some(path) = parent {
            require_frozen_executable_deadline(deadline)?;
            insert_frozen_executable_directory(path, &mut directories, &mut directory_bytes)?;
            parent = path.parent();
        }
    }
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        if layout.is_directory {
            insert_frozen_executable_directory(&layout.path, &mut directories, &mut directory_bytes)?;
        } else if directories.remove(&layout.path) {
            directory_bytes = directory_bytes.saturating_sub(layout.path.as_os_str().len());
        }
    }

    let mut redirects = BTreeMap::new();
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        let FrozenExecutableLayout::Symlink { target, .. } = &layout.entry else {
            continue;
        };
        let Some(target) = resolve_frozen_symlink_target(&layout.path, target) else {
            continue;
        };
        if directories.contains(&target) {
            redirects.insert(layout.path.clone(), target);
        }
    }
    Ok(redirects)
}

fn insert_frozen_executable_directory(
    path: &Path,
    directories: &mut BTreeSet<PathBuf>,
    total_bytes: &mut usize,
) -> Result<(), Error> {
    if directories.contains(path) {
        return Ok(());
    }
    require_frozen_executable_directory_count(directories.len().saturating_add(1))?;
    account_frozen_executable_directory_bytes(path.as_os_str().len(), total_bytes)?;
    directories.insert(path.to_owned());
    Ok(())
}

fn reject_frozen_executable_directory_redirect(
    path: &Path,
    redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<(), Error> {
    let mut ancestor = path.parent();
    while let Some(source) = ancestor {
        require_frozen_executable_deadline(deadline)?;
        if let Some(target) = redirects.get(source) {
            return Err(Error::FrozenExecutableDirectoryRedirect {
                path: path.to_owned(),
                redirect_source: Box::new(source.to_owned()),
                target: Box::new(target.clone()),
            });
        }
        ancestor = source.parent();
    }
    Ok(())
}
