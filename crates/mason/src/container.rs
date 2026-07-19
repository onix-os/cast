// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    collections::BTreeSet,
    ffi::{CString, OsString},
    fs::{File, Permissions},
    io::{self, Read as _},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

use crate::Paths;
use container::{
    Container, DevPolicy, LoopbackPolicy, ProcPolicy, PseudoFilesystemPolicy, RootFilesystemPolicy, SysPolicy,
    TmpPolicy, TmpfsLimits,
};
use stone_recipe::derivation::{
    BuilderLayout, DerivationPlan, DevFilesystem, ExecutionCredentials, FilesystemPolicy, NetworkMode, SysFilesystem,
    TmpFilesystem,
};

mod anchored_locators;
mod error;
mod frozen_revalidation;
pub use error::Error;

const CGROUP2_MOUNT_PATH: &str = "/sys/fs/cgroup";
const CURRENT_CGROUP_MEMBERSHIP_PATH: &str = "/proc/self/cgroup";
const CAST_SUPERVISOR_CGROUP: &[u8] = b"cast-supervisor";
const MAX_CURRENT_CGROUP_BYTES: usize = 16 * 1024;
const MAX_CURRENT_CGROUP_PATH_BYTES: usize = 4_095;
const MAX_CURRENT_CGROUP_COMPONENTS: usize = 128;
const MAX_CURRENT_CGROUP_COMPONENT_BYTES: usize = 255;

// Frozen declarations do not select these values. They are executor-owned,
// deliberately generous, finite safety ceilings around one complete
// derivation process tree. CPU capacity is the sole scaled value because the
// plan already declares and enforces its exact parallel-job count.
const FROZEN_CGROUP_PIDS_MAX: u64 = 4_096;
const FROZEN_CGROUP_MEMORY_GIB: u64 = 32;
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;
const FROZEN_CGROUP_SWAP_MAX: u64 = 0;
const FROZEN_CGROUP_CPU_PERIOD_MICROS: u64 = 100_000;
const FROZEN_TMPFS_SIZE_BYTES: u64 = 16 * BYTES_PER_GIB;
const FROZEN_TMPFS_INODES: u64 = 1_048_576;
const FROZEN_TMPFS_LIMITS: TmpfsLimits = match TmpfsLimits::new(FROZEN_TMPFS_SIZE_BYTES, FROZEN_TMPFS_INODES) {
    Ok(limits) => limits,
    Err(_) => panic!("frozen tmpfs limits are non-zero"),
};

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
mod preflight;
#[cfg(any(test, feature = "delegated-fixture-test-support"))]
pub(crate) use preflight::execution_namespace_capability_unavailable;
#[cfg(feature = "delegated-fixture-test-support")]
pub(crate) use preflight::preflight_delegated_execution_capability;

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
    let root_locator = sandbox.root_locator(guard.root_path(), &anchor)?;
    let mut container = Container::new_anchored(root_locator)
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
            FrozenMountSource::Pinned(source) => {
                let locator = sandbox.pinned_source_locator(source, &mount.host)?;
                container.bind_rw_pinned(locator, &mount.guest)?
            }
            FrozenMountSource::RootRelative(source) => container.bind_rw_from_root(source, &mount.guest)?,
        };
    }
    let identity = plan.derivation_id();
    require_derivation_cgroup_identity(identity.as_str())?;
    let limits = frozen_cgroup_limits(plan.execution.jobs)?;
    let delegated = discover_delegated_cgroup()?;
    let leaf = delegated
        .create_leaf(identity.as_str(), limits)
        .map_err(Error::CreateDerivationCgroup)?;

    // Frozen execution has no legacy-clone or post-clone migration path. The
    // kernel must place the child in this authenticated leaf atomically.
    container.run_in_cgroup::<E>(leaf, f)?;
    Ok(())
}

fn discover_delegated_cgroup() -> Result<container::cgroup::DelegatedCgroupRoot, Error> {
    let membership_path = Path::new(CURRENT_CGROUP_MEMBERSHIP_PATH);
    let membership = read_bounded_current_cgroup(membership_path)?;
    let delegated_relative = delegated_relative_from_current_cgroup(&membership)?;
    container::cgroup::DelegatedCgroupRoot::open(Path::new(CGROUP2_MOUNT_PATH), delegated_relative)
        .map_err(Error::OpenDelegatedCgroup)
}

fn read_bounded_current_cgroup(path: &Path) -> Result<Vec<u8>, Error> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| Error::ReadCurrentCgroup {
            path: path.to_owned(),
            source,
        })?;
    let mut membership = Vec::with_capacity(MAX_CURRENT_CGROUP_BYTES + 1);
    file.take((MAX_CURRENT_CGROUP_BYTES + 1) as u64)
        .read_to_end(&mut membership)
        .map_err(|source| Error::ReadCurrentCgroup {
            path: path.to_owned(),
            source,
        })?;
    if membership.len() > MAX_CURRENT_CGROUP_BYTES {
        return Err(Error::CurrentCgroupTooLarge {
            path: path.to_owned(),
            limit: MAX_CURRENT_CGROUP_BYTES,
        });
    }
    Ok(membership)
}

/// Return the delegated root relative to `/sys/fs/cgroup`.
///
/// With systemd's `DelegateSubgroup=cast-supervisor`, the current process is
/// the sole member of `<delegated-root>/cast-supervisor`. Accept no alternate
/// topology, compatibility hierarchy, normalization, or environment override.
fn delegated_relative_from_current_cgroup(membership: &[u8]) -> Result<PathBuf, Error> {
    if membership.is_empty() || !membership.ends_with(b"\n") || membership.contains(&0) {
        return Err(Error::MalformedCurrentCgroup {
            reason: "membership must be non-empty, NUL-free, and newline terminated",
        });
    }

    let mut entries = membership[..membership.len() - 1].split(|byte| *byte == b'\n');
    let entry = entries.next().ok_or(Error::MalformedCurrentCgroup {
        reason: "missing unified cgroup entry",
    })?;
    if entry.is_empty() || entries.next().is_some() {
        return Err(Error::MalformedCurrentCgroup {
            reason: "expected exactly one unified cgroup entry",
        });
    }

    let mut fields = entry.splitn(3, |byte| *byte == b':');
    let hierarchy = fields.next();
    let controllers = fields.next();
    let path = fields.next();
    let (Some(b"0"), Some(b""), Some(path)) = (hierarchy, controllers, path) else {
        return Err(Error::MalformedCurrentCgroup {
            reason: "expected the exact unified cgroup-v2 prefix `0::`",
        });
    };
    if path.len() > MAX_CURRENT_CGROUP_PATH_BYTES {
        return Err(Error::CurrentCgroupPathTooLarge {
            limit: MAX_CURRENT_CGROUP_PATH_BYTES,
            actual: path.len(),
        });
    }
    if !path.starts_with(b"/") {
        return Err(Error::MalformedCurrentCgroup {
            reason: "unified cgroup path must be absolute",
        });
    }

    let relative = &path[1..];
    let current = PathBuf::from(OsString::from_vec(path.to_vec()));
    if relative.is_empty() {
        return Err(Error::FrozenCgroupDelegationRequired { current });
    }
    let components = relative.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if components.len() > MAX_CURRENT_CGROUP_COMPONENTS {
        return Err(Error::CurrentCgroupComponentLimit {
            limit: MAX_CURRENT_CGROUP_COMPONENTS,
            actual: components.len(),
        });
    }
    if let Some(component) = components
        .iter()
        .find(|component| component.len() > MAX_CURRENT_CGROUP_COMPONENT_BYTES)
    {
        return Err(Error::CurrentCgroupComponentTooLarge {
            limit: MAX_CURRENT_CGROUP_COMPONENT_BYTES,
            actual: component.len(),
        });
    }
    if components
        .iter()
        .any(|component| component.is_empty() || *component == b"." || *component == b"..")
    {
        return Err(Error::MalformedCurrentCgroup {
            reason: "unified cgroup path must contain only normalized non-empty components",
        });
    }

    if components.last().copied() != Some(CAST_SUPERVISOR_CGROUP) || components.len() < 2 {
        return Err(Error::FrozenCgroupDelegationRequired { current });
    }

    // The final component and its separator were authenticated above. Strip
    // them byte-for-byte rather than applying lexical path normalization.
    let parent_len = relative.len() - CAST_SUPERVISOR_CGROUP.len() - 1;
    let parent = &relative[..parent_len];
    Ok(PathBuf::from(OsString::from_vec(parent.to_vec())))
}

fn require_derivation_cgroup_identity(identity: &str) -> Result<(), Error> {
    if identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(Error::InvalidDerivationCgroupIdentity)
    }
}

fn frozen_cgroup_limits(jobs: u32) -> Result<container::cgroup::CgroupLimits, Error> {
    if jobs == 0 {
        return Err(Error::InvalidFrozenCgroupJobs);
    }
    let memory_max = FROZEN_CGROUP_MEMORY_GIB
        .checked_mul(BYTES_PER_GIB)
        .ok_or(Error::FrozenCgroupLimitOverflow { field: "memory.max" })?;
    let cpu_quota_micros = u64::from(jobs)
        .checked_mul(FROZEN_CGROUP_CPU_PERIOD_MICROS)
        .ok_or(Error::FrozenCgroupLimitOverflow { field: "cpu.max quota" })?;
    container::cgroup::CgroupLimits::new(
        FROZEN_CGROUP_PIDS_MAX,
        memory_max,
        FROZEN_CGROUP_SWAP_MAX,
        cpu_quota_micros,
        FROZEN_CGROUP_CPU_PERIOD_MICROS,
    )
    .map_err(Error::InvalidFrozenCgroupLimits)
}

fn frozen_loopback_policy() -> LoopbackPolicy {
    LoopbackPolicy::KernelDefault
}

fn frozen_pseudo_filesystems(filesystems: FilesystemPolicy) -> PseudoFilesystemPolicy {
    PseudoFilesystemPolicy {
        proc: ProcPolicy::None,
        tmp: match filesystems.tmp {
            TmpFilesystem::Empty => TmpPolicy::Bounded(FROZEN_TMPFS_LIMITS),
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
    path_anchor: File,
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
    path_anchor: File,
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
    let (workspace_path, workspace_file, workspace_path_anchor) =
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
        path_anchor: workspace_path_anchor,
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
    let path_anchor =
        anchored_locators::open_workspace_path_anchor(&workspace.path_anchor, &relative).map_err(|source| {
            Error::PrepareFrozenBindSource {
                path: host.clone(),
                source,
            }
        })?;
    if DirectoryWitness::for_file(&path_anchor).map_err(|source| Error::PrepareFrozenBindSource {
        path: host.clone(),
        source,
    })? != witness
    {
        return Err(Error::FrozenBindSourceReplaced(host));
    }
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
            path_anchor,
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

#[cfg(test)]
mod tests;
