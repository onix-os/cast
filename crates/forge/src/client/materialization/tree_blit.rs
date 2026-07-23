/// Resolve an existing destination, or its existing parent plus final name,
/// and prove that materialization cannot reach any installation-root
/// namespace. This rejects canonical pathname/symlink aliases as well as
/// lexical descendants; retained capabilities provide the later write bound.
pub(crate) fn require_disjoint_materialization_target(
    installation: &Installation,
    requested: &Path,
) -> Result<PathBuf, Error> {
    installation.revalidate_root_directory()?;
    let installation_root = installation.root.canonicalize()?;
    let target = match requested.canonicalize() {
        Ok(target) => target,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let name = requested.file_name().ok_or(Error::EphemeralInstallationRoot)?;
            let parent = requested
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .canonicalize()?;
            parent.join(name)
        }
        Err(source) => return Err(source.into()),
    };
    installation.revalidate_root_directory()?;

    if target.starts_with(&installation_root)
        || installation_root.starts_with(&target)
        || target.ancestors().any(has_cast_control_topology)
    {
        Err(Error::EphemeralInstallationRoot)
    } else {
        installation.revalidate_root_directory()?;
        Ok(target)
    }
}

pub(super) fn has_cast_control_topology(path: &Path) -> bool {
    [
        path.join(".cast"),
        path.join(".cast/root"),
        path.join(".cast/root/staging"),
    ]
    .into_iter()
    .all(|component| {
        fs::symlink_metadata(component)
            .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
    })
}

/// Blit the packages to a filesystem root
///
/// This functionality is core to all Cast filesystem transactions, forming the entire
/// staging logic. For all the [`crate::package::Id`] present in the staging state,
/// query their stored [`StonePayloadLayoutBody`] and cache into a [`vfs::Tree`].
///
/// The new `/usr` filesystem is written in optimal order to a staging tree by making
/// use of the "at" family of functions (`mkdirat`, `openat`, etc) with relative
/// directory file descriptors. Writable outputs receive digest-verified private
/// inodes rather than aliases into the content-addressed store.
///
/// This produces a digest-verified private candidate which can then be
/// published atomically via [`Self::promote_staging`] without aliasing writable
/// state to the content-addressed store.
#[cfg(test)]
pub(crate) fn blit_root(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
) -> Result<(), Error> {
    let _staging_coordinator = fixed_staging::lock_coordinator()?;
    blit_root_with_materialization(
        installation,
        tree,
        blit_target,
        AssetMaterialization::IndependentCopy,
        BlitExecution::Parallel,
    )
}

fn blit_root_from_admission(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    admission: &ExternalMaterializationAdmission,
) -> Result<(), Error> {
    let _writer_coordinator = fixed_staging::lock_coordinator()?;
    let mut target = RetainedExternalMaterializationTarget::prepare_from(installation, admission)?;
    target
        .materialize(
            installation,
            tree,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Parallel,
        )
        .map(drop)
}

#[cfg(test)]
fn blit_root_with_materialization(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
    materialization: AssetMaterialization,
    execution: BlitExecution,
) -> Result<(), Error> {
    let mut target = RetainedExternalMaterializationTarget::prepare(installation, blit_target)?;
    target
        .materialize(installation, tree, materialization, execution)
        .map(drop)
}

fn blit_tree_into_open_root(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    root_fd: RawFd,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_top_level_usr: Option<&std::fs::File>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;

    let progress = ProgressBar::new(1).with_style(
        ProgressStyle::with_template("\n|{bar:20.red/blue}| {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("■≡=- "),
    );
    progress.set_message("Blitting filesystem");
    progress.enable_steady_tick(Duration::from_millis(150));
    progress.tick();

    let now = Instant::now();

    progress.set_length(tree.len());
    progress.set_position(0_u64);

    // Metadata-only closures and packages containing only directories,
    // symlinks, or canonical empty files have no asset-store dependency.
    // Opening assets/v2 unconditionally made those valid closures fail with
    // ENOENT after cache unpacking correctly produced no asset directory.
    let mut requires_asset_cache = false;
    for item in tree.iter() {
        require_blit_deadline(deadline)?;
        if matches!(
            &item.layout.file,
            StonePayloadLayoutFile::Regular(digest, _) if *digest != EMPTY_FILE_DIGEST
        ) {
            requires_asset_cache = true;
            break;
        }
    }
    let cache = if requires_asset_cache {
        Some(AssetPool::open(installation)?)
    } else {
        None
    };

    let blit = || -> Result<BlitStats, Error> {
        let mut stats = BlitStats::default();
        require_blit_deadline(deadline)?;
        if let Some(root) = tree.structured() {
            if let Element::Directory(_, _, children) = root {
                if tree.len() != 0
                    && retained_top_level_usr.is_some()
                    && (children.len() != 1
                        || !matches!(children.first(), Some(Element::Directory(name, _, _)) if *name == "usr"))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stateful candidate must contain exactly one top-level usr directory",
                    )
                    .into());
                }
                stats = stats.merge(blit_children(
                    root_fd,
                    cache.as_ref(),
                    children,
                    &progress,
                    materialization,
                    execution,
                    copy_manifest,
                    deadline,
                    retained_top_level_usr,
                )?);
            }
        }

        Ok(stats)
    };

    let stats = match execution {
        BlitExecution::Parallel => {
            // Stateful transactions retain the established parallel blit
            // path. The pool is dropped before Mason enters a namespace.
            let rayon_runtime = rayon::ThreadPoolBuilder::new().build().expect("rayon runtime");
            rayon_runtime.install(blit)?
        }
        // Frozen roots use canonical tree order without host-sized scheduling.
        BlitExecution::Sequential => blit()?,
    };
    require_blit_deadline(deadline)?;

    progress.finish_and_clear();

    let elapsed = now.elapsed();
    let num_entries = stats.num_entries();

    println!(
        "\n{} entries blitted in {} {}",
        num_entries.to_string().bold(),
        format!("{:.2}s", elapsed.as_secs_f32()).bold(),
        format!("({:.1}k / s)", num_entries as f32 / elapsed.as_secs_f32() / 1_000.0).dim()
    );

    require_blit_deadline(deadline)?;
    Ok(())
}

/// Recursively write a directory, or a single flat inode, to the staging tree.
/// Care is taken to retain the directory file descriptor to avoid costly path
/// resolution at runtime.
fn blit_element(
    parent: RawFd,
    cache: Option<&AssetPool>,
    element: Element<'_, PendingFile>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_usr: Option<&std::fs::File>,
) -> Result<BlitStats, Error> {
    require_blit_deadline(deadline)?;
    let mut stats = BlitStats::default();

    progress.inc(1);

    let (_, item) = match &element {
        Element::Directory(_, item, _) => ("directory", item),
        Element::Child(_, item) => ("file", item),
    };

    trace!(
        progress = progress.position() as f32 / progress.length().unwrap_or(1) as f32,
        current = progress.position() as usize,
        total = progress.length().unwrap_or(0) as usize,
        event_type = "progress_update",
        "Blitting {}",
        item.path()
    );

    match element {
        Element::Directory(name, item, children) => {
            if name == "usr"
                && let Some(retained_usr) = retained_usr
            {
                let active_mode = match materialization {
                    AssetMaterialization::HardLink => item.layout.mode,
                    AssetMaterialization::IndependentCopy => item.layout.mode | 0o700,
                };
                fchmod(retained_usr.as_raw_fd(), Mode::from_bits_truncate(active_mode))?;
                stats.num_dirs += 1;
                stats = stats.merge(blit_children(
                    retained_usr.as_raw_fd(),
                    cache,
                    children,
                    progress,
                    materialization,
                    execution,
                    copy_manifest,
                    deadline,
                    None,
                )?);
                if materialization == AssetMaterialization::IndependentCopy {
                    fchmod(retained_usr.as_raw_fd(), Mode::from_bits_truncate(item.layout.mode))?;
                }
                return Ok(stats);
            }

            // Construct within the parent
            blit_element_item(
                parent,
                cache,
                name,
                item,
                &mut stats,
                materialization,
                copy_manifest,
                deadline,
            )?;

            // open the new dir
            let newdir = openat_owned(
                parent,
                name,
                OFlag::O_CLOEXEC | OFlag::O_RDONLY | OFlag::O_DIRECTORY,
                Mode::empty(),
            )?;

            stats = stats.merge(blit_children(
                newdir.as_raw_fd(),
                cache,
                children,
                progress,
                materialization,
                execution,
                copy_manifest,
                deadline,
                None,
            )?);

            // Frozen directories are created owner-accessible so restrictive
            // final modes cannot prevent their own children from being
            // materialized. Apply the declared mode only after the complete
            // subtree exists. Stateful blits retain their established mode
            // timing.
            if materialization == AssetMaterialization::IndependentCopy {
                fchmod(newdir.as_raw_fd(), Mode::from_bits_truncate(item.layout.mode))?;
            }

            Ok(stats)
        }
        Element::Child(name, item) => {
            if name == "usr" && retained_usr.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "candidate top-level usr entry is not a directory",
                )
                .into());
            }
            blit_element_item(
                parent,
                cache,
                name,
                item,
                &mut stats,
                materialization,
                copy_manifest,
                deadline,
            )?;

            Ok(stats)
        }
    }
}

fn blit_children(
    parent: RawFd,
    cache: Option<&AssetPool>,
    children: Vec<Element<'_, PendingFile>>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_usr: Option<&std::fs::File>,
) -> Result<BlitStats, Error> {
    require_blit_deadline(deadline)?;
    match execution {
        BlitExecution::Parallel => {
            let current_span = tracing::Span::current();
            children
                .into_par_iter()
                .map(|child| {
                    let _guard = current_span.enter();
                    blit_element(
                        parent,
                        cache,
                        child,
                        progress,
                        materialization,
                        execution,
                        copy_manifest,
                        deadline,
                        retained_usr,
                    )
                })
                .try_reduce(BlitStats::default, |left, right| Ok(left.merge(right)))
        }
        BlitExecution::Sequential => children.into_iter().try_fold(BlitStats::default(), |stats, child| {
            blit_element(
                parent,
                cache,
                child,
                progress,
                materialization,
                execution,
                copy_manifest,
                deadline,
                retained_usr,
            )
            .map(|child_stats| stats.merge(child_stats))
        }),
    }
}

/// Write a single inode into the staging tree.
///
/// # Arguments
///
/// * `parent`  - raw file descriptor for parent directory in which the inode is being record to
/// * `cache`   - raw file descriptor for the system asset pool tree
/// * `subpath` - the base name of the new inode
/// * `item`    - New inode being recorded
fn blit_element_item(
    parent: RawFd,
    cache: Option<&AssetPool>,
    subpath: &str,
    item: &PendingFile,
    stats: &mut BlitStats,
    materialization: AssetMaterialization,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;
    match &item.layout.file {
        StonePayloadLayoutFile::Regular(id, _) => {
            // Link relative from cache to target.
            let fp = frozen_asset_path(*id);

            match *id {
                // Mystery empty-file hash. Do not allow dupes!
                // https://github.com/serpent-os/tools/issues/372
                EMPTY_FILE_DIGEST => {
                    let _file = openat_owned(
                        parent,
                        subpath,
                        OFlag::O_CLOEXEC | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY,
                        Mode::from_bits_truncate(0o600),
                    )?;
                }
                // Regular file
                _ => {
                    let cache = cache.ok_or_else(|| {
                        io::Error::other("non-empty regular blit entry has no authenticated asset-cache descriptor")
                    })?;
                    match materialization {
                        AssetMaterialization::HardLink => {
                            link_asset(cache, &fp, parent, subpath)?;
                        }
                        AssetMaterialization::IndependentCopy => {
                            copy_asset(
                                cache,
                                &fp,
                                *id,
                                parent,
                                subpath,
                                item.layout.mode,
                                copy_manifest,
                                deadline,
                            )?;
                        }
                    }
                }
            }

            // Creation modes are filtered through the process umask. Apply
            // the package's complete mode after materialization instead. An
            // independent copy applies it through the still-pinned output
            // descriptor inside `copy_asset`; do not reopen that trust
            // boundary by chmodding a pathname here.
            if materialization == AssetMaterialization::HardLink || *id == EMPTY_FILE_DIGEST {
                fchmodat(
                    Some(parent),
                    subpath,
                    Mode::from_bits_truncate(item.layout.mode),
                    nix::sys::stat::FchmodatFlags::NoFollowSymlink,
                )?;
            }

            stats.num_files += 1;
        }
        StonePayloadLayoutFile::Symlink(source, _) => {
            symlinkat(source.as_str(), Some(parent), subpath)?;
            stats.num_symlinks += 1;
        }
        StonePayloadLayoutFile::Directory(_) => {
            let mode = match materialization {
                AssetMaterialization::HardLink => item.layout.mode,
                AssetMaterialization::IndependentCopy => item.layout.mode | 0o700,
            };
            mkdirat(parent, subpath, Mode::from_bits_truncate(mode))?;
            fchmodat(
                Some(parent),
                subpath,
                Mode::from_bits_truncate(mode),
                nix::sys::stat::FchmodatFlags::NoFollowSymlink,
            )?;
            stats.num_dirs += 1;
        }

        // Unimplemented
        StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_)
        | StonePayloadLayoutFile::Unknown(..) => {
            return Err(Error::UnsupportedFrozenLayout {
                package: item.id.clone(),
                path: format!("/usr/{}", item.layout.file.target()),
            });
        }
    };

    Ok(())
}
