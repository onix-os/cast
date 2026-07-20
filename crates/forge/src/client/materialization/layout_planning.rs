/// Build a [`vfs::Tree`] for the specified layouts
///
/// Returns a newly built vfs Tree to plan the filesystem operations for blitting
/// and conflict detection.
pub fn vfs(layouts: Vec<(package::Id, StonePayloadLayoutRecord)>) -> Result<vfs::Tree<PendingFile>, Error> {
    let mut tbuild = TreeBuilder::new();

    for (id, layout) in layouts {
        // Revalidate database rows and direct Stone extraction input at the
        // final stateful/ephemeral VFS boundary. Normal cache ingestion already
        // enforces this contract atomically, but corrupted or test-populated
        // databases must not recover an escape path here.
        require_usr_relative_stone_layout(&id, &layout)?;
        require_materializable_stone_layout(&id, &layout)?;
        tbuild.push(PendingFile { id: id.clone(), layout });
    }

    tbuild.bake();

    Ok(tbuild.tree()?)
}

const MAX_STONE_LAYOUT_COMPONENT_BYTES: usize = nix::libc::NAME_MAX as usize;
const MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES: usize = 256;

/// Proof that a raw Stone target has the one admitted representation.
///
/// Keeping materialization behind this type prevents a future caller from
/// accidentally restoring the old absolute-path compatibility spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsrRelativeStoneTarget<'a>(&'a str);

/// Store decoded Stone layouts only after proving that every raw target uses
/// the one canonical package namespace.
///
/// Stone omits the leading `/usr/`; [`PendingFile::path`] adds it exactly once.
/// Requiring a non-empty normalized relative target here therefore makes every
/// persisted row strictly `/usr`-only and prevents alternate spellings of the
/// same materialized path. The iterator is cloned so the complete batch is
/// preflighted before `batch_add` can remove or insert any database rows.
fn ingest_stone_layouts<'a, I>(layout_db: &db::layout::Database, layouts: I) -> Result<(), Error>
where
    I: Iterator<Item = (&'a package::Id, &'a StonePayloadLayoutRecord)> + Clone,
{
    for (package, layout) in layouts.clone() {
        require_usr_relative_stone_layout(package, layout)?;
    }
    layout_db.batch_add(layouts)?;
    Ok(())
}

fn require_usr_relative_stone_layout<'a>(
    package: &package::Id,
    layout: &'a StonePayloadLayoutRecord,
) -> Result<UsrRelativeStoneTarget<'a>, Error> {
    let target = layout.file.target();
    require_usr_relative_stone_target(target).map_err(|reason| Error::InvalidStoneLayoutTarget {
        package: package.clone(),
        target: stone_layout_target_diagnostic(target),
        reason,
    })
}

fn require_materializable_stone_layout(
    package: &package::Id,
    layout: &StonePayloadLayoutRecord,
) -> Result<(), Error> {
    match &layout.file {
        StonePayloadLayoutFile::Regular(..)
        | StonePayloadLayoutFile::Symlink(..)
        | StonePayloadLayoutFile::Directory(_) => Ok(()),
        StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_)
        | StonePayloadLayoutFile::Unknown(..) => Err(Error::UnsupportedFrozenLayout {
            package: package.clone(),
            path: format!("/usr/{}", layout.file.target()),
        }),
    }
}

fn stone_layout_target_diagnostic(target: &str) -> String {
    if target.len() <= MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES {
        return target.to_owned();
    }

    let mut end = MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES;
    while !target.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &target[..end])
}

fn require_usr_relative_stone_target(target: &str) -> Result<UsrRelativeStoneTarget<'_>, &'static str> {
    if target.is_empty() {
        return Err("the target is empty");
    }
    if target.starts_with('/') {
        return Err("the target is absolute");
    }
    if target.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("the target contains an ASCII control byte");
    }
    if target.ends_with('/') {
        return Err("the target has a trailing separator");
    }
    if target.contains("//") {
        return Err("the target contains a repeated separator");
    }

    let materialized_bytes = "/usr/"
        .len()
        .checked_add(target.len())
        .ok_or("the materialized path length overflows")?;
    if materialized_bytes > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err("the materialized path exceeds Linux PATH_MAX");
    }

    let mut components = 1usize; // the materialized `/usr` component
    for component in target.split('/') {
        if component == "." || component == ".." {
            return Err("the target contains a dot component");
        }
        if component.len() > MAX_STONE_LAYOUT_COMPONENT_BYTES {
            return Err("a target component exceeds Linux NAME_MAX");
        }
        components = components
            .checked_add(1)
            .ok_or("the materialized component count overflows")?;
        if components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
            return Err("the materialized path is too deep");
        }
    }
    if package::is_reserved_usr_layout_target(target) {
        return Err("the target is reserved for Cast system metadata");
    }
    Ok(UsrRelativeStoneTarget(target))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenLayoutPathPolicyError {
    TooLong { actual: usize },
    TooDeep { actual: usize },
}

fn require_materialized_frozen_path_policy(path: &str) -> Result<(), FrozenLayoutPathPolicyError> {
    if path.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err(FrozenLayoutPathPolicyError::TooLong { actual: path.len() });
    }

    let materialized_components = path.split('/').filter(|component| !component.is_empty()).count();
    if materialized_components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(FrozenLayoutPathPolicyError::TooDeep {
            actual: materialized_components,
        });
    }
    Ok(())
}

fn require_frozen_layout_symlink_target(package: &package::Id, target: &str) -> Result<(), Error> {
    if target.len() > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenLayoutSymlinkTargetTooLong {
            package: package.clone(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: target.len(),
        });
    }
    if target.is_empty() {
        return Err(Error::InvalidFrozenLayoutSymlinkTarget {
            package: package.clone(),
            reason: "the target is empty",
        });
    }
    if target.as_bytes().contains(&0) {
        return Err(Error::InvalidFrozenLayoutSymlinkTarget {
            package: package.clone(),
            reason: "the target contains NUL",
        });
    }
    Ok(())
}

fn materialized_frozen_layout_path(raw_path: UsrRelativeStoneTarget<'_>) -> String {
    format!("/usr/{}", raw_path.0)
}

#[derive(Debug)]
struct FrozenLayoutEntry {
    package: package::Id,
    layout: StonePayloadLayoutRecord,
    path: String,
    package_order: usize,
    kind_order: u8,
    source_order: String,
}

impl FrozenLayoutEntry {
    fn new(package: package::Id, layout: StonePayloadLayoutRecord, package_order: usize) -> Result<Self, Error> {
        // Keep frozen materialization fail-closed even for callers which
        // populate the layout database without going through Stone ingestion.
        let raw_path = require_usr_relative_stone_layout(&package, &layout)?;
        if let StonePayloadLayoutFile::Symlink(target, _) = &layout.file {
            require_frozen_layout_symlink_target(&package, target)?;
        }

        let path = materialized_frozen_layout_path(raw_path);
        if !is_normalized_frozen_path(&path) {
            return Err(Error::InvalidFrozenLayoutPath { package, path });
        }
        if layout.uid != 0 || layout.gid != 0 {
            return Err(Error::UnsupportedFrozenOwnership {
                package,
                path,
                uid: layout.uid,
                gid: layout.gid,
            });
        }

        let expected_file_type = match &layout.file {
            StonePayloadLayoutFile::Regular(..) => nix::libc::S_IFREG,
            StonePayloadLayoutFile::Directory(_) => nix::libc::S_IFDIR,
            StonePayloadLayoutFile::Symlink(..) => nix::libc::S_IFLNK,
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => {
                return Err(Error::UnsupportedFrozenLayout { package, path });
            }
        };
        let actual_file_type = layout.mode & nix::libc::S_IFMT;
        let unsupported_mode_bits = layout.mode & !(nix::libc::S_IFMT | 0o7777);
        let symlink_mode_is_enforceable = expected_file_type != nix::libc::S_IFLNK || layout.mode & 0o7777 == 0o777;
        if actual_file_type != expected_file_type || unsupported_mode_bits != 0 || !symlink_mode_is_enforceable {
            return Err(Error::InvalidFrozenLayoutMode {
                package,
                path,
                mode: layout.mode,
            });
        }

        let (kind_order, source_order) = match &layout.file {
            StonePayloadLayoutFile::Directory(_) => (0, String::new()),
            StonePayloadLayoutFile::Regular(source, _) => (1, format!("{source:032x}")),
            StonePayloadLayoutFile::Symlink(source, _) => (2, source.to_string()),
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => unreachable!("unsupported inode returned above"),
        };

        Ok(Self {
            package,
            layout,
            path,
            package_order,
            kind_order,
            source_order,
        })
    }

    fn is_directory(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Directory(_))
    }

    fn pending(&self) -> PendingFile {
        PendingFile {
            id: self.package.clone(),
            layout: self.layout.clone(),
        }
    }
}

/// Build a deterministic tree for a frozen package closure.
///
/// SQLite row order and concurrent cache completion are deliberately ignored.
/// Package IDs establish canonical precedence, entries are sorted by their
/// complete materialization data, byte-identical directory records collapse,
/// and every metadata disagreement or non-directory collision is rejected
/// before the destination root is touched.
#[cfg(test)]
fn frozen_vfs(
    packages: &[package::Id],
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
) -> Result<vfs::Tree<PendingFile>, Error> {
    frozen_vfs_until(packages, layouts, Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT)
}

fn frozen_vfs_until(
    packages: &[package::Id],
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
    deadline: Instant,
) -> Result<vfs::Tree<PendingFile>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let mut package_order = BTreeMap::new();
    for (order, package) in packages.iter().enumerate() {
        require_frozen_materialization_deadline(deadline)?;
        package_order.insert(package.clone(), order);
    }
    let mut entries = Vec::with_capacity(layouts.len());
    for (package, layout) in layouts {
        require_frozen_materialization_deadline(deadline)?;
        let order = package_order
            .get(&package)
            .copied()
            .ok_or_else(|| Error::UnexpectedFrozenLayoutPackage(package.clone()))?;
        entries.push(FrozenLayoutEntry::new(package, layout, order)?);
    }

    require_frozen_materialization_deadline(deadline)?;
    entries.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.package_order.cmp(&right.package_order))
            .then_with(|| left.kind_order.cmp(&right.kind_order))
            .then_with(|| left.source_order.cmp(&right.source_order))
            .then_with(|| left.layout.uid.cmp(&right.layout.uid))
            .then_with(|| left.layout.gid.cmp(&right.layout.gid))
            .then_with(|| left.layout.mode.cmp(&right.layout.mode))
            .then_with(|| left.layout.tag.cmp(&right.layout.tag))
    });
    require_frozen_materialization_deadline(deadline)?;

    let mut selected: Vec<FrozenLayoutEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        require_frozen_materialization_deadline(deadline)?;
        if let Some(previous) = selected.last()
            && previous.path == entry.path
        {
            if identical_directory_metadata(previous, &entry) {
                continue;
            }
            return Err(frozen_collision(&entry.path, previous, &entry));
        }
        selected.push(entry);
    }

    validate_frozen_tree_collisions_until(&selected, deadline)?;

    let mut builder = TreeBuilder::new();
    for entry in &selected {
        require_frozen_materialization_deadline(deadline)?;
        builder.push(entry.pending());
    }
    require_frozen_materialization_deadline(deadline)?;
    builder.bake();
    require_frozen_materialization_deadline(deadline)?;
    let tree = builder.tree()?;
    require_frozen_materialization_deadline(deadline)?;
    Ok(tree)
}

fn is_normalized_frozen_path(path: &str) -> bool {
    path.starts_with("/usr/")
        && !path.as_bytes().contains(&0)
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.split('/').any(|component| component == "." || component == "..")
}

fn validate_frozen_tree_collisions_until(entries: &[FrozenLayoutEntry], deadline: Instant) -> Result<(), Error> {
    validate_frozen_tree_collisions_with_limits_until(
        entries,
        MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS,
        MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES,
        Some(deadline),
    )
}

#[cfg(test)]
fn validate_frozen_tree_collisions_with_limits(
    entries: &[FrozenLayoutEntry],
    max_directory_paths: usize,
    max_directory_bytes: usize,
) -> Result<(), Error> {
    validate_frozen_tree_collisions_with_limits_until(entries, max_directory_paths, max_directory_bytes, None)
}

fn validate_frozen_tree_collisions_with_limits_until(
    entries: &[FrozenLayoutEntry],
    max_directory_paths: usize,
    max_directory_bytes: usize,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;
    let explicit = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut directories = BTreeSet::new();
    let mut directory_bytes = 0usize;
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut parent = Path::new(&entry.path).parent();
        while let Some(parent_path) = parent {
            require_blit_deadline(deadline)?;
            let path = parent_path
                .to_str()
                .expect("validated frozen layout paths remain valid UTF-8");
            insert_frozen_materialized_directory(
                path,
                &mut directories,
                &mut directory_bytes,
                max_directory_paths,
                max_directory_bytes,
            )?;
            parent = parent_path.parent();
        }
    }
    for entry in entries {
        require_blit_deadline(deadline)?;
        if entry.is_directory() {
            insert_frozen_materialized_directory(
                &entry.path,
                &mut directories,
                &mut directory_bytes,
                max_directory_paths,
                max_directory_bytes,
            )?;
        } else if directories.remove(&entry.path) {
            directory_bytes = directory_bytes.saturating_sub(entry.path.len());
        }
    }

    let mut redirects = BTreeMap::new();
    for entry in entries {
        require_blit_deadline(deadline)?;
        if let StonePayloadLayoutFile::Symlink(target, _) = &entry.layout.file {
            let target = if target.starts_with('/') {
                target.to_string()
            } else {
                let parent = Path::new(&entry.path)
                    .parent()
                    .expect("validated frozen path has a parent")
                    .to_string_lossy();
                vfs::path::join(parent.as_ref(), target.as_str()).to_string()
            };
            if directories.contains(&target) {
                redirects.insert(entry.path.clone(), target);
            }
        }
    }

    // TreeBuilder's historical redirect cloning can place an explicit child
    // somewhere other than either its declared or validated effective path.
    // Frozen roots therefore reject every explicit descendant beneath a
    // directory symlink; packages must declare the canonical directory path.
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut ancestor = Path::new(&entry.path).parent();
        while let Some(ancestor_path) = ancestor {
            require_blit_deadline(deadline)?;
            let redirect = ancestor_path.to_string_lossy();
            if redirects.contains_key(redirect.as_ref()) {
                return Err(Error::FrozenDirectorySymlinkDescendant {
                    package: entry.package.clone(),
                    path: entry.path.clone().into_boxed_str(),
                    redirect: redirect.into_owned().into_boxed_str(),
                });
            }
            ancestor = ancestor_path.parent();
        }
    }

    // Redirect descendants have already failed closed, so every surviving
    // entry retains its declared path. Non-directory parents therefore need
    // only ancestor-key lookups; no redirect cross-product is necessary.
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut parent = Path::new(&entry.path).parent();
        while let Some(parent_path) = parent {
            require_blit_deadline(deadline)?;
            let parent_name = parent_path
                .to_str()
                .expect("validated frozen layout paths remain valid UTF-8");
            if let Some(ancestor) = explicit.get(parent_name)
                && !ancestor.is_directory()
            {
                return Err(frozen_collision(&entry.path, ancestor, entry));
            }
            parent = parent_path.parent();
        }
    }

    require_blit_deadline(deadline)?;
    Ok(())
}

fn insert_frozen_materialized_directory(
    path: &str,
    directories: &mut BTreeSet<String>,
    total_bytes: &mut usize,
    max_paths: usize,
    max_bytes: usize,
) -> Result<(), Error> {
    if directories.contains(path) {
        return Ok(());
    }
    let actual_paths = directories.len().saturating_add(1);
    if actual_paths > max_paths {
        return Err(Error::FrozenExecutableDirectoryLimit {
            limit: max_paths,
            actual: actual_paths,
        });
    }
    let actual_bytes = total_bytes.checked_add(path.len()).unwrap_or(usize::MAX);
    if actual_bytes > max_bytes {
        return Err(Error::FrozenExecutableDirectoryByteLimit {
            limit: max_bytes,
            actual: actual_bytes,
        });
    }
    directories.insert(path.to_owned());
    *total_bytes = actual_bytes;
    Ok(())
}

fn identical_directory_metadata(first: &FrozenLayoutEntry, second: &FrozenLayoutEntry) -> bool {
    first.is_directory()
        && second.is_directory()
        && first.layout.uid == second.layout.uid
        && first.layout.gid == second.layout.gid
        && first.layout.mode == second.layout.mode
        && first.layout.tag == second.layout.tag
}

fn frozen_collision(path: &str, first: &FrozenLayoutEntry, second: &FrozenLayoutEntry) -> Error {
    Error::FrozenPathCollision {
        path: path.to_owned(),
        first: first.package.clone(),
        second: second.package.clone(),
    }
}
