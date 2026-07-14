// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{File, Permissions},
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

use container::{
    Container, DevPolicy, LoopbackPolicy, ProcPolicy, PseudoFilesystemPolicy, RootFilesystemPolicy, SysPolicy,
    TmpPolicy,
};
use stone_recipe::derivation::{
    BuilderLayout, DerivationPlan, DevFilesystem, ExecutionCredentials, FilesystemPolicy, NetworkMode, SysFilesystem,
    TmpFilesystem,
};
use thiserror::Error;

use crate::Paths;

pub fn exec<E>(paths: &Paths, networking: bool, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    run(paths, networking, f)
}

/// Execute a frozen plan without exposing mutable recipe or global cache
/// inputs to build steps.
pub(crate) fn exec_frozen<E>(
    paths: &Paths,
    plan: &DerivationPlan,
    sandbox: &FrozenSandbox,
    guard: &forge::FrozenRootGuard,
    f: impl FnMut() -> Result<(), E>,
) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let rootfs = paths.rootfs().host;
    if guard.root_path() != rootfs {
        return Err(Error::FrozenRootMismatch {
            expected: rootfs,
            found: guard.root_path().to_owned(),
        });
    }
    sandbox.revalidate()?;
    let anchor = guard.revalidated_anchor()?;
    let mut container = Container::new_anchored(guard.root_path(), &anchor)
        .map_err(Error::AnchorFrozenRoot)?
        .hostname(&sandbox.hostname)
        .networking(matches!(plan.execution.network, NetworkMode::Enabled))
        .loopback(frozen_loopback_policy())
        .pseudo_filesystems(frozen_pseudo_filesystems(plan.execution.filesystems))
        .root_filesystem(sandbox.root_filesystem)
        .ignore_host_sigint(true)
        .work_dir(&sandbox.work_dir);

    for mount in &sandbox.mounts {
        container = match &mount.source {
            FrozenMountSource::Pinned(source) => container.bind_rw_pinned(&source.file, &mount.host, &mount.guest)?,
            FrozenMountSource::RootRelative(source) => container.bind_rw_from_root(source, &mount.guest)?,
        };
    }
    container.run::<E>(f)?;
    Ok(())
}

fn frozen_loopback_policy() -> LoopbackPolicy {
    LoopbackPolicy::KernelDefault
}

fn frozen_pseudo_filesystems(filesystems: FilesystemPolicy) -> PseudoFilesystemPolicy {
    PseudoFilesystemPolicy {
        proc: ProcPolicy::None,
        tmp: match filesystems.tmp {
            TmpFilesystem::Empty => TmpPolicy::Empty,
        },
        sys: match filesystems.sys {
            SysFilesystem::None => SysPolicy::None,
        },
        dev: match filesystems.dev {
            DevFilesystem::None => DevPolicy::None,
            DevFilesystem::Minimal => DevPolicy::Minimal,
        },
    }
}

#[derive(Debug)]
enum FrozenMountSource {
    Pinned(PinnedFrozenMountSource),
    RootRelative(PathBuf),
}

#[derive(Debug)]
struct PinnedFrozenMountSource {
    file: File,
    relative: PathBuf,
    witness: DirectoryWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
}

impl DirectoryWitness {
    fn for_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[derive(Debug)]
struct FrozenWorkspace {
    path: PathBuf,
    file: File,
    witness: DirectoryWitness,
}

#[derive(Debug)]
struct FrozenMount {
    role: FrozenMountRole,
    host: PathBuf,
    guest: PathBuf,
    source: FrozenMountSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenMountRole {
    Artefacts,
    Build,
    Install,
    Cache,
}

#[derive(Debug)]
#[must_use = "prepared frozen mount descriptors must survive through container activation"]
pub(crate) struct FrozenSandbox {
    workspace: FrozenWorkspace,
    hostname: String,
    work_dir: PathBuf,
    root_filesystem: RootFilesystemPolicy,
    mounts: Vec<FrozenMount>,
}

impl FrozenSandbox {
    fn revalidate(&self) -> Result<(), Error> {
        require_owned_directory(
            &self.workspace.file,
            &self.workspace.path,
            false,
            DirectoryRole::Workspace,
        )?;
        if DirectoryWitness::for_file(&self.workspace.file).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })? != self.workspace.witness
        {
            return Err(Error::FrozenWorkspaceReplaced(self.workspace.path.clone()));
        }

        let reopened = open_mount_directory(&self.workspace.path).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })?;
        require_owned_directory(&reopened, &self.workspace.path, false, DirectoryRole::Workspace)?;
        if DirectoryWitness::for_file(&reopened).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })? != self.workspace.witness
        {
            return Err(Error::FrozenWorkspaceReplaced(self.workspace.path.clone()));
        }

        for mount in &self.mounts {
            let FrozenMountSource::Pinned(source) = &mount.source else {
                continue;
            };
            require_owned_directory(&source.file, &mount.host, true, DirectoryRole::BindSource)?;
            if DirectoryWitness::for_file(&source.file).map_err(|io| Error::PrepareFrozenBindSource {
                path: mount.host.clone(),
                source: io,
            })? != source.witness
            {
                return Err(Error::FrozenBindSourceReplaced(mount.host.clone()));
            }
            let reopened = open_workspace_directory(&self.workspace, &source.relative, &mount.host, true)?;
            if DirectoryWitness::for_file(&reopened).map_err(|io| Error::PrepareFrozenBindSource {
                path: mount.host.clone(),
                source: io,
            })? != source.witness
            {
                return Err(Error::FrozenBindSourceReplaced(mount.host.clone()));
            }
        }
        Ok(())
    }

    /// Revalidate the complete external sandbox and borrow the exact artefact
    /// directory which was mounted into the container.
    ///
    /// Host publication must consume this descriptor rather than reopening
    /// `Paths::artefacts()` after the payload exits. Otherwise a pathname
    /// replacement could make the publisher consume bytes which the frozen
    /// build never produced.
    pub(crate) fn revalidated_artefacts(&self) -> Result<&File, Error> {
        self.revalidate()?;
        let mount = self
            .mounts
            .iter()
            .find(|mount| mount.role == FrozenMountRole::Artefacts)
            .ok_or(Error::MissingFrozenArtefactMount)?;
        let FrozenMountSource::Pinned(source) = &mount.source else {
            return Err(Error::MissingFrozenArtefactMount);
        };
        Ok(&source.file)
    }
}

pub(crate) fn prepare_frozen_sandbox(paths: &Paths, plan: &DerivationPlan) -> Result<FrozenSandbox, Error> {
    if !matches!(plan.execution.credentials, ExecutionCredentials::IsolatedRoot) {
        return Err(Error::FrozenCredentialPolicyMismatch {
            found: plan.execution.credentials.as_str(),
        });
    }
    if paths.layout() != &plan.layout {
        return Err(Error::FrozenLayoutMismatch);
    }
    if matches!(plan.execution.network, NetworkMode::Enabled) {
        return Err(Error::FrozenNetworkPolicyMismatch);
    }
    paths.require_plan(plan).map_err(Error::InvalidFrozenPaths)?;
    let (workspace_path, workspace_file) =
        paths
            .frozen_workspace_anchor()
            .map_err(|source| Error::OpenFrozenWorkspace {
                path: paths.workspace_path().to_owned(),
                source,
            })?;
    require_owned_directory(&workspace_file, &workspace_path, false, DirectoryRole::Workspace)?;
    let workspace = FrozenWorkspace {
        witness: DirectoryWitness::for_file(&workspace_file).map_err(|source| Error::OpenFrozenWorkspace {
            path: workspace_path.clone(),
            source,
        })?,
        path: workspace_path,
        file: workspace_file,
    };
    let sandbox = FrozenSandbox {
        mounts: frozen_mounts(
            paths,
            &workspace,
            &plan.layout,
            plan.execution.compiler_cache,
            plan.derivation_id().as_str(),
        )?,
        workspace,
        hostname: plan.layout.hostname.clone(),
        work_dir: plan.layout.build_dir.clone().into(),
        root_filesystem: RootFilesystemPolicy::ReadOnly,
    };
    sandbox.revalidate()?;
    Ok(sandbox)
}

fn frozen_mounts(
    paths: &Paths,
    workspace: &FrozenWorkspace,
    layout: &BuilderLayout,
    compiler_cache: bool,
    derivation_id: &str,
) -> Result<Vec<FrozenMount>, Error> {
    let mut mounts = vec![
        frozen_host_mount(
            paths,
            workspace,
            paths.artefacts().host,
            layout.artifacts_dir.clone().into(),
            FrozenMountPersistence::FreshScratch,
            FrozenMountRole::Artefacts,
        )?,
        frozen_host_mount(
            paths,
            workspace,
            paths.build().host,
            layout.build_dir.clone().into(),
            FrozenMountPersistence::FreshScratch,
            FrozenMountRole::Build,
        )?,
        FrozenMount {
            role: FrozenMountRole::Install,
            host: paths.install().host,
            guest: layout.install_dir.clone().into(),
            source: FrozenMountSource::RootRelative(PathBuf::from(&layout.install_dir)),
        },
    ];
    if compiler_cache {
        for (name, guest) in layout.cache_destinations() {
            mounts.push(frozen_host_mount(
                paths,
                workspace,
                paths.derivation_cache_host(derivation_id, name),
                guest.into(),
                FrozenMountPersistence::RetainedCache,
                FrozenMountRole::Cache,
            )?);
        }
    }
    Ok(mounts)
}

#[derive(Clone, Copy)]
enum FrozenMountPersistence {
    FreshScratch,
    RetainedCache,
}

fn frozen_host_mount(
    paths: &Paths,
    workspace: &FrozenWorkspace,
    host: PathBuf,
    guest: PathBuf,
    persistence: FrozenMountPersistence,
    role: FrozenMountRole,
) -> Result<FrozenMount, Error> {
    let relative = workspace_relative(&workspace.path, &host)?;
    let source = match persistence {
        FrozenMountPersistence::FreshScratch => paths.prepare_fresh_private_host_directory(&host),
        FrozenMountPersistence::RetainedCache => paths.prepare_private_host_directory(&host),
    }
    .map_err(|source| Error::PrepareFrozenBindSource {
        path: host.clone(),
        source,
    })?;
    require_owned_directory(&source, &host, true, DirectoryRole::BindSource)?;
    let witness = DirectoryWitness::for_file(&source).map_err(|source| Error::PrepareFrozenBindSource {
        path: host.clone(),
        source,
    })?;
    let reopened = open_workspace_directory(workspace, &relative, &host, true)?;
    if DirectoryWitness::for_file(&reopened).map_err(|source| Error::PrepareFrozenBindSource {
        path: host.clone(),
        source,
    })? != witness
    {
        return Err(Error::FrozenBindSourceReplaced(host));
    }
    Ok(FrozenMount {
        role,
        host,
        guest,
        source: FrozenMountSource::Pinned(PinnedFrozenMountSource {
            file: source,
            relative,
            witness,
        }),
    })
}

const MAX_FROZEN_WORKSPACE_PATH_BYTES: usize = 4095;
const MAX_FROZEN_WORKSPACE_COMPONENTS: usize = 32;

#[derive(Clone, Copy)]
enum DirectoryRole {
    Workspace,
    BindSource,
}

fn workspace_relative(workspace: &Path, host: &Path) -> Result<PathBuf, Error> {
    let relative = host
        .strip_prefix(workspace)
        .map_err(|_| Error::InvalidFrozenBindSource(host.to_owned()))?;
    let raw = relative.as_os_str().as_bytes();
    if raw.is_empty()
        || raw.len() > MAX_FROZEN_WORKSPACE_PATH_BYTES
        || raw.contains(&0)
        || relative.components().count() > MAX_FROZEN_WORKSPACE_COMPONENTS
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidFrozenBindSource(host.to_owned()));
    }
    Ok(relative.to_owned())
}

fn open_workspace_directory(
    workspace: &FrozenWorkspace,
    relative: &Path,
    display: &Path,
    exact_private_leaf: bool,
) -> Result<File, Error> {
    let mut current = workspace
        .file
        .try_clone()
        .map_err(|source| Error::PrepareFrozenBindSource {
            path: display.to_owned(),
            source,
        })?;
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return Err(Error::InvalidFrozenBindSource(display.to_owned()));
        };
        let name = CString::new(name.as_bytes()).map_err(|_| Error::InvalidFrozenBindSource(display.to_owned()))?;
        current = open_mount_child(&current, &name).map_err(|source| Error::PrepareFrozenBindSource {
            path: display.to_owned(),
            source,
        })?;
        let leaf = index + 1 == component_count;
        require_owned_directory(&current, display, leaf && exact_private_leaf, DirectoryRole::BindSource)?;
        if DirectoryWitness::for_file(&current)
            .map_err(|source| Error::PrepareFrozenBindSource {
                path: display.to_owned(),
                source,
            })?
            .device
            != workspace.witness.device
        {
            return Err(Error::FrozenBindSourceCrossesMount(display.to_owned()));
        }
    }
    Ok(current)
}

fn require_owned_directory(
    directory: &File,
    path: &Path,
    exact_private: bool,
    role: DirectoryRole,
) -> Result<(), Error> {
    let metadata = directory.metadata().map_err(|source| match role {
        DirectoryRole::Workspace => Error::OpenFrozenWorkspace {
            path: path.to_owned(),
            source,
        },
        DirectoryRole::BindSource => Error::PrepareFrozenBindSource {
            path: path.to_owned(),
            source,
        },
    })?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.mode() & 0o022 != 0
        || (exact_private && mode != 0o700)
    {
        return Err(match role {
            DirectoryRole::Workspace => Error::UnsafeFrozenWorkspace {
                path: path.to_owned(),
                owner: metadata.uid(),
                mode,
            },
            DirectoryRole::BindSource => Error::UnsafeFrozenBindSource {
                path: path.to_owned(),
                owner: metadata.uid(),
                mode,
            },
        });
    }
    Ok(())
}

/// Create every mount target while the materialized root is still mutable.
///
/// Frozen-root verification happens after this function. Anchored container
/// activation subsequently requires every target to pre-exist and will never
/// create a path after the verification guard has been issued.
pub(crate) fn prepare_frozen_mount_targets(
    paths: &Paths,
    plan: &DerivationPlan,
    materialized_root: &forge::MaterializedFrozenRoot,
) -> Result<(), Error> {
    if paths.layout() != &plan.layout {
        return Err(Error::FrozenLayoutMismatch);
    }
    if materialized_root.root_path() != paths.rootfs().host {
        return Err(Error::FrozenRootMismatch {
            expected: paths.rootfs().host,
            found: materialized_root.root_path().to_owned(),
        });
    }

    let mut targets = vec![
        PathBuf::from(&plan.layout.artifacts_dir),
        PathBuf::from(&plan.layout.build_dir),
        PathBuf::from(&plan.layout.install_dir),
    ];
    if plan.execution.compiler_cache {
        targets.extend(
            plan.layout
                .cache_destinations()
                .into_iter()
                .map(|(_, target)| PathBuf::from(target)),
        );
    }
    match plan.execution.filesystems.tmp {
        TmpFilesystem::Empty => targets.push(PathBuf::from("/tmp")),
    }
    match plan.execution.filesystems.dev {
        DevFilesystem::None => {}
        DevFilesystem::Minimal => targets.push(PathBuf::from("/dev")),
    }
    // Frozen plans currently cannot express proc, sys, or networking. Keep
    // this target inventory exhaustive so extending those enums forces the
    // preparation boundary to grow with the activation policy.
    match plan.execution.filesystems.proc {
        stone_recipe::derivation::ProcFilesystem::None => {}
    }
    match plan.execution.filesystems.sys {
        SysFilesystem::None => {}
    }
    if matches!(plan.execution.network, NetworkMode::Enabled) {
        return Err(Error::FrozenNetworkPolicyMismatch);
    }

    let anchor = materialized_root.revalidated_anchor()?;
    let root = File::from(anchor.try_clone_to_owned().map_err(Error::AnchorFrozenRoot)?);
    require_private_owned_directory(&root, materialized_root.root_path())?;
    prepare_mount_targets_in(&root, &targets, Path::new(&plan.layout.install_dir))?;
    materialized_root.revalidate()?;
    Ok(())
}

#[cfg(test)]
fn prepare_mount_targets_at(root_path: &Path, targets: &[PathBuf], install: &Path) -> Result<(), Error> {
    let root = open_mount_directory(root_path).map_err(|source| Error::OpenFrozenMountRoot {
        path: root_path.to_owned(),
        source,
    })?;
    require_private_owned_directory(&root, root_path)?;

    prepare_mount_targets_in(&root, targets, install)?;

    // Test-only path wrapper proves its retained descriptor still matches the
    // name. Production preparation never enters through this path API.
    let reopened = open_mount_directory(root_path).map_err(|source| Error::OpenFrozenMountRoot {
        path: root_path.to_owned(),
        source,
    })?;
    let expected = root.metadata().map_err(|source| Error::OpenFrozenMountRoot {
        path: root_path.to_owned(),
        source,
    })?;
    let found = reopened.metadata().map_err(|source| Error::OpenFrozenMountRoot {
        path: root_path.to_owned(),
        source,
    })?;
    use std::os::unix::fs::MetadataExt as _;
    if expected.dev() != found.dev() || expected.ino() != found.ino() {
        return Err(Error::FrozenMountRootReplaced(root_path.to_owned()));
    }
    Ok(())
}

fn prepare_mount_targets_in(root: &File, targets: &[PathBuf], install: &Path) -> Result<(), Error> {
    let targets = canonical_mount_targets(targets)?;
    for target in targets {
        let directory = ensure_mount_target(&root, &target)?;
        require_empty_mount_target(&directory, &target)?;
        if target == install {
            directory
                .set_permissions(Permissions::from_mode(0o700))
                .map_err(|source| Error::PrepareFrozenMountTarget {
                    path: target.clone(),
                    source,
                })?;
        }
    }

    Ok(())
}

fn canonical_mount_targets(targets: &[PathBuf]) -> Result<Vec<PathBuf>, Error> {
    const MAX_TARGETS: usize = 64;
    const MAX_TARGET_BYTES: usize = 4_095;
    const MAX_TARGET_COMPONENTS: usize = 32;
    if targets.len() > MAX_TARGETS {
        return Err(Error::FrozenMountTargetLimit {
            limit: MAX_TARGETS,
            actual: targets.len(),
        });
    }
    let mut unique = BTreeSet::new();
    for target in targets {
        if !target.is_absolute()
            || target.as_os_str().as_bytes().len() > MAX_TARGET_BYTES
            || target.as_os_str().as_bytes().contains(&0)
            || target.components().count() > MAX_TARGET_COMPONENTS + 1
            || target
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err(Error::InvalidFrozenMountTarget(target.clone()));
        }
        let relative = target
            .strip_prefix("/")
            .map_err(|_| Error::InvalidFrozenMountTarget(target.clone()))?;
        if relative.as_os_str().is_empty() || !unique.insert(target.clone()) {
            return Err(Error::InvalidFrozenMountTarget(target.clone()));
        }
    }
    let targets = unique.into_iter().collect::<Vec<_>>();
    for (index, target) in targets.iter().enumerate() {
        for other in &targets[index + 1..] {
            if target.starts_with(other) || other.starts_with(target) {
                return Err(Error::OverlappingFrozenMountTargets {
                    first: target.clone(),
                    second: other.clone(),
                });
            }
        }
    }
    Ok(targets)
}

fn ensure_mount_target(root: &File, target: &Path) -> Result<File, Error> {
    let mut current = root.try_clone().map_err(|source| Error::PrepareFrozenMountTarget {
        path: target.to_owned(),
        source,
    })?;
    let mut traversed = PathBuf::from("/");
    for component in target.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(Error::InvalidFrozenMountTarget(target.to_owned()));
        };
        traversed.push(name);
        let name = CString::new(name.as_bytes()).map_err(|_| Error::InvalidFrozenMountTarget(target.to_owned()))?;
        let mut next = open_mount_child(&current, &name);
        if next
            .as_ref()
            .is_err_and(|source| source.kind() == io::ErrorKind::NotFound)
        {
            // SAFETY: parent and component are live; mkdirat never follows the
            // final component and the subsequent openat2 authenticates it.
            let result = unsafe { nix::libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() != io::ErrorKind::AlreadyExists {
                    return Err(Error::PrepareFrozenMountTarget {
                        path: traversed,
                        source,
                    });
                }
            }
            next = open_mount_child(&current, &name);
        }
        current = next.map_err(|source| Error::PrepareFrozenMountTarget {
            path: traversed.clone(),
            source,
        })?;
    }
    Ok(current)
}

fn open_mount_directory(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
}

fn open_mount_child(parent: &File, name: &CString) -> io::Result<File> {
    // SAFETY: an all-zero open_how is valid before setting its public fields.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(
        (nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NONBLOCK) as u32,
    );
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, component, and open_how remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result).map_err(|_| io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn require_empty_mount_target(directory: &File, path: &Path) -> Result<(), Error> {
    let descriptor_path = PathBuf::from(format!("/proc/{}/fd/{}", std::process::id(), directory.as_raw_fd()));
    let mut entries = std::fs::read_dir(&descriptor_path).map_err(|source| Error::PrepareFrozenMountTarget {
        path: path.to_owned(),
        source,
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|source| Error::PrepareFrozenMountTarget {
            path: path.to_owned(),
            source,
        })?
        .is_some()
    {
        return Err(Error::FrozenMountTargetNotEmpty(path.to_owned()));
    }
    Ok(())
}

fn require_private_owned_directory(directory: &File, path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = directory.metadata().map_err(|source| Error::OpenFrozenMountRoot {
        path: path.to_owned(),
        source,
    })?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(Error::UnsafeFrozenMountRoot(path.to_owned()));
    }
    Ok(())
}

fn run<E>(paths: &Paths, networking: bool, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let rootfs = paths.rootfs().host;
    let artefacts = paths.artefacts();
    let build = paths.build();
    let compiler = paths.ccache();
    let gocache = paths.gocache();
    let gomodcache = paths.gomodcache();
    let cargocache = paths.cargocache();
    let zigcache = paths.zigcache();
    let rustc_wrapper = paths.sccache();
    let recipe = paths.recipe();

    let container = Container::new(rootfs)
        .hostname(&paths.layout().hostname)
        .networking(networking)
        .ignore_host_sigint(true)
        .work_dir(&build.guest)
        .bind_rw(&artefacts.host, &artefacts.guest)
        .bind_rw(&build.host, &build.guest)
        .bind_rw(&compiler.host, &compiler.guest)
        .bind_rw(&gocache.host, &gocache.guest)
        .bind_rw(&gomodcache.host, &gomodcache.guest)
        .bind_rw(&cargocache.host, &cargocache.guest)
        .bind_rw(&zigcache.host, &zigcache.guest)
        .bind_rw(&rustc_wrapper.host, &rustc_wrapper.guest)
        .bind_ro(&recipe.host, &recipe.guest);

    container.run::<E>(f)?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Container(#[from] container::Error),
    #[error("revalidate the frozen root immediately before container activation")]
    FrozenRoot(#[from] forge::client::Error),
    #[error("open the authenticated frozen-root anchor for container activation")]
    AnchorFrozenRoot(#[source] io::Error),
    #[error("prepare frozen mount")]
    Mount(#[from] io::Error),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
    #[error("frozen execution requires credential policy `isolated-root`, found `{found}`")]
    FrozenCredentialPolicyMismatch { found: &'static str },
    #[error("frozen execution forbids network-enabled sandbox policy")]
    FrozenNetworkPolicyMismatch,
    #[error("prepared frozen root does not match runtime path: expected {expected:?}, found {found:?}")]
    FrozenRootMismatch { expected: PathBuf, found: PathBuf },
    #[error("runtime paths are not bound to the frozen derivation")]
    InvalidFrozenPaths(#[source] io::Error),
    #[error("open the retained frozen workspace {path:?} without following links")]
    OpenFrozenWorkspace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the retained frozen workspace is not privately controlled: {path:?} (uid={owner}, mode={mode:#06o})")]
    UnsafeFrozenWorkspace { path: PathBuf, owner: u32, mode: u32 },
    #[error("the retained frozen workspace pathname was replaced: {0:?}")]
    FrozenWorkspaceReplaced(PathBuf),
    #[error("invalid frozen external bind source beneath the retained workspace: {0:?}")]
    InvalidFrozenBindSource(PathBuf),
    #[error("prepare frozen external bind source {path:?} without following links or crossing mounts")]
    PrepareFrozenBindSource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen external bind source is not an exact owner-private directory: {path:?} (uid={owner}, mode={mode:#06o})"
    )]
    UnsafeFrozenBindSource { path: PathBuf, owner: u32, mode: u32 },
    #[error("frozen external bind source crosses a mount beneath the retained workspace: {0:?}")]
    FrozenBindSourceCrossesMount(PathBuf),
    #[error("frozen external bind source pathname was replaced after pinning: {0:?}")]
    FrozenBindSourceReplaced(PathBuf),
    #[error("prepared frozen sandbox has no pinned artefact mount")]
    MissingFrozenArtefactMount,
    #[error("open the materialized frozen mount root {path:?} without following links")]
    OpenFrozenMountRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the materialized frozen mount root is not a privately controlled directory: {0:?}")]
    UnsafeFrozenMountRoot(PathBuf),
    #[error("the materialized frozen mount root was replaced during target preparation: {0:?}")]
    FrozenMountRootReplaced(PathBuf),
    #[error("invalid frozen mount target: {0:?}")]
    InvalidFrozenMountTarget(PathBuf),
    #[error("frozen mount target count exceeds {limit} (got {actual})")]
    FrozenMountTargetLimit { limit: usize, actual: usize },
    #[error("frozen mount targets overlap: {first:?} and {second:?}")]
    OverlappingFrozenMountTargets { first: PathBuf, second: PathBuf },
    #[error("prepare frozen mount target {path:?} without following links or crossing mounts")]
    PrepareFrozenMountTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen mount target must be empty before activation: {0:?}")]
    FrozenMountTargetNotEmpty(PathBuf),
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        path::Path,
    };

    use stone_recipe::derivation::ProcFilesystem;

    use super::*;
    use crate::{BuildPolicy, Recipe, package};

    fn create_production_frozen_root(path: &Path) {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn frozen_mount_targets_are_created_beneath_one_owned_root_before_verification() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        create_production_frozen_root(&root);
        let targets = [
            PathBuf::from("/mason/artefacts"),
            PathBuf::from("/mason/build"),
            PathBuf::from("/mason/install"),
            PathBuf::from("/tmp"),
            PathBuf::from("/dev"),
        ];

        prepare_mount_targets_at(&root, &targets, Path::new("/mason/install")).unwrap();

        for target in &targets {
            let target = root.join(target.strip_prefix("/").unwrap());
            assert!(target.is_dir());
            assert_eq!(std::fs::metadata(target).unwrap().permissions().mode() & 0o7777, 0o700);
        }
        assert_eq!(
            std::fs::metadata(root.join("mason/install"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
    }

    #[test]
    fn frozen_mount_target_creation_rejects_symlink_components_without_touching_the_target() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        create_production_frozen_root(&root);
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("mason")).unwrap();

        let error =
            prepare_mount_targets_at(&root, &[PathBuf::from("/mason/build")], Path::new("/mason/install")).unwrap_err();
        assert!(matches!(error, Error::PrepareFrozenMountTarget { .. }));
        assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn retained_root_descriptor_never_creates_targets_in_a_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let published = temporary.path().join("published");
        create_production_frozen_root(&published);
        let root = open_mount_directory(&published).unwrap();

        let retained = temporary.path().join("retained");
        std::fs::rename(&published, &retained).unwrap();
        create_production_frozen_root(&published);
        prepare_mount_targets_in(&root, &[PathBuf::from("/mason/build")], Path::new("/mason/install")).unwrap();

        assert!(retained.join("mason/build").is_dir());
        assert!(!published.join("mason").exists());
    }

    #[test]
    fn frozen_mount_targets_reject_hidden_content_and_overlapping_topology() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        create_production_frozen_root(&root);
        std::fs::create_dir(root.join("tmp")).unwrap();
        std::fs::write(root.join("tmp/hidden"), b"must not be hidden by tmpfs").unwrap();

        assert!(matches!(
            prepare_mount_targets_at(&root, &[PathBuf::from("/tmp")], Path::new("/install")),
            Err(Error::FrozenMountTargetNotEmpty(path)) if path == Path::new("/tmp")
        ));
        assert!(matches!(
            canonical_mount_targets(&[PathBuf::from("/mason"), PathBuf::from("/mason/build")]),
            Err(Error::OverlappingFrozenMountTargets { .. })
        ));
    }

    #[test]
    fn frozen_mount_target_preparation_rejects_a_shared_writable_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, Permissions::from_mode(0o777)).unwrap();

        assert!(matches!(
            prepare_mount_targets_at(&root, &[PathBuf::from("/build")], Path::new("/install")),
            Err(Error::UnsafeFrozenMountRoot(path)) if path == root
        ));
    }

    #[test]
    fn descriptor_child_open_rejects_mount_crossings() {
        let root = open_mount_directory(Path::new("/")).unwrap();
        let proc = CString::new("proc").unwrap();
        let error = open_mount_child(&root, &proc).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::EXDEV));
    }

    fn non_default_layout() -> BuilderLayout {
        let mut policy = BuildPolicy::repository_for_tests();
        policy.spec.sandbox.hostname = "forge-builder".to_owned();
        policy.spec.sandbox.guest_root = "/forge".to_owned();
        policy.spec.sandbox.artifacts_dir = "/forge/output".to_owned();
        policy.spec.sandbox.build_dir = "/forge/work".to_owned();
        policy.spec.sandbox.source_dir = "/forge/sources".to_owned();
        policy.spec.sandbox.recipe_dir = "/forge/recipe".to_owned();
        policy.spec.sandbox.package_dir = "/forge/recipe/package".to_owned();
        policy.spec.sandbox.install_dir = "/forge/destination".to_owned();
        {
            let cache = &mut policy.spec.build_root.compiler_cache;
            cache.ccache_dir = "/forge/cache-cc".to_owned();
            cache.sccache_dir = "/forge/cache-rust".to_owned();
            cache.go_cache_dir = "/forge/cache-go".to_owned();
            cache.go_mod_cache_dir = "/forge/cache-go-mod".to_owned();
            cache.cargo_cache_dir = "/forge/cache-cargo".to_owned();
            cache.zig_cache_dir = "/forge/cache-zig".to_owned();
        }
        policy.spec.validate().unwrap();
        BuilderLayout::from_policy(&policy.spec.sandbox, &policy.spec.build_root.compiler_cache)
    }

    #[test]
    fn frozen_filesystems_override_legacy_container_mounts() {
        let frozen = FilesystemPolicy {
            proc: ProcFilesystem::None,
            tmp: TmpFilesystem::Empty,
            sys: SysFilesystem::None,
            dev: DevFilesystem::None,
        };

        let mapped = frozen_pseudo_filesystems(frozen);
        assert_eq!(mapped.proc, ProcPolicy::None);
        assert_eq!(mapped.tmp, TmpPolicy::Empty);
        assert_eq!(mapped.sys, SysPolicy::None);
        assert_eq!(mapped.dev, DevPolicy::None);
        assert_ne!(mapped, PseudoFilesystemPolicy::default());
        assert_eq!(frozen_loopback_policy(), LoopbackPolicy::KernelDefault);
    }

    #[test]
    fn frozen_minimal_dev_is_exact_and_sys_is_absent() {
        let mapped = frozen_pseudo_filesystems(FilesystemPolicy::default());

        assert_eq!(mapped.proc, ProcPolicy::None);
        assert_eq!(mapped.tmp, TmpPolicy::Empty);
        assert_eq!(mapped.sys, SysPolicy::None);
        assert_eq!(mapped.dev, DevPolicy::Minimal);
        assert_eq!(::container::MINIMAL_DEV_NODES, ["null", "zero", "full"]);
    }

    #[test]
    fn frozen_container_excludes_recipe_and_disabled_global_caches() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let plan = package::test_derivation_plan();
        let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        paths.bind_to_plan(&plan).unwrap();

        let disabled = prepare_frozen_sandbox(&paths, &plan).unwrap().mounts;
        assert_eq!(disabled.len(), 3);
        assert_eq!(
            disabled.iter().map(|mount| mount.guest.as_path()).collect::<Vec<_>>(),
            [
                Path::new(&plan.layout.artifacts_dir),
                Path::new(&plan.layout.build_dir),
                Path::new(&plan.layout.install_dir),
            ]
        );
        assert_eq!(disabled[2].host, paths.install().host);
        assert!(!disabled.iter().any(|mount| mount.host == paths.recipe().host));

        let enabled_runtime = crate::private_tempdir();
        let mut enabled_plan = plan.clone();
        package::set_test_compiler_cache(&mut enabled_plan, true);
        enabled_plan.validate().unwrap();
        let mut enabled_paths = Paths::new(
            &recipe,
            enabled_plan.layout.clone(),
            enabled_runtime.path(),
            output.path(),
        )
        .unwrap();
        enabled_paths.bind_to_plan(&enabled_plan).unwrap();
        let enabled = prepare_frozen_sandbox(&enabled_paths, &enabled_plan).unwrap().mounts;
        assert_eq!(enabled.len(), 9);
        assert!(enabled.iter().skip(3).all(|mount| {
            mount.host.starts_with(
                enabled_runtime
                    .path()
                    .join("derivations")
                    .join(enabled_plan.derivation_id().as_str()),
            )
        }));
        assert!(!enabled.iter().any(|mount| mount.host == enabled_paths.recipe().host));
    }

    #[test]
    fn frozen_container_uses_non_default_policy_layout_as_one_authority() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let default_plan = package::test_derivation_plan();
        let default_id = default_plan.derivation_id();
        let mut plan = default_plan;
        plan.layout = non_default_layout();
        package::set_test_compiler_cache(&mut plan, true);
        plan.validate().unwrap();
        let derivation_id = plan.derivation_id();
        let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        paths.bind_to_plan(&plan).unwrap();

        assert_ne!(default_id, derivation_id);
        assert_eq!(paths.install().guest, Path::new("/forge/destination"));
        assert_eq!(
            paths.install().host,
            paths.rootfs().host.join("forge").join("destination")
        );

        let sandbox = prepare_frozen_sandbox(&paths, &plan).unwrap();
        assert_eq!(sandbox.hostname, "forge-builder");
        assert_eq!(sandbox.work_dir, Path::new("/forge/work"));
        assert_eq!(sandbox.root_filesystem, RootFilesystemPolicy::ReadOnly);
        assert_eq!(
            sandbox
                .mounts
                .iter()
                .map(|mount| mount.guest.as_path())
                .collect::<Vec<_>>(),
            [
                Path::new("/forge/output"),
                Path::new("/forge/work"),
                Path::new("/forge/destination"),
                Path::new("/forge/cache-cc"),
                Path::new("/forge/cache-rust"),
                Path::new("/forge/cache-go"),
                Path::new("/forge/cache-go-mod"),
                Path::new("/forge/cache-cargo"),
                Path::new("/forge/cache-zig"),
            ]
        );
        assert!(sandbox.mounts.iter().skip(3).all(|mount| {
            mount
                .host
                .starts_with(runtime.path().join("derivations").join(derivation_id.as_str()))
        }));
    }

    #[test]
    fn frozen_container_rejects_runtime_and_plan_layout_mismatch() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = package::test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.layout.hostname = "different-builder".to_owned();
        plan.validate().unwrap();

        assert!(matches!(
            prepare_frozen_sandbox(&paths, &plan),
            Err(Error::FrozenLayoutMismatch)
        ));
    }

    #[test]
    fn frozen_container_rejects_non_isolated_credentials() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let mut plan = package::test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.execution.credentials = ExecutionCredentials::Unspecified;

        assert!(matches!(
            prepare_frozen_sandbox(&paths, &plan),
            Err(Error::FrozenCredentialPolicyMismatch { found: "unspecified" })
        ));
    }
}
