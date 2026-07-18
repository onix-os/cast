//! Pure Stone-layout planning for an ActiveReblit boot projection.
//!
//! The plan contains only inputs consumed by the pinned `blsforme` publisher:
//! systemd-boot, optional package-owned `os-info.json`, command-line
//! fragments, kernels, and initrds. Each relevant state also records whether
//! its schema must instead come from Cast's generated `os-release`, and
//! whether failure may fall back to the mandatory global head schema; that
//! file is never misrepresented as a Stone asset. Kernel `boot.json`,
//! `config`, and `System.map` files are intentionally not boot-repair inputs.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::{package, state};

use super::PreparedActiveReblitBootProjection;

pub(crate) const MAX_BOOT_PLAN_ASSETS: usize = 8_192;
const MAX_BOOT_PLAN_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_BOOT_PLAN_KERNELS: usize = 128;
pub(crate) const MAX_BOOT_PLAN_SNAPSHOT_DIGESTS: usize = 256;
const MAX_BOOT_PLAN_SYMLINK_HOPS: usize = 32;
const MAX_BOOT_PLAN_WORK: usize = 2_000_000;
const MAX_BOOT_PLAN_SINGLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
const MAX_BOOT_PLAN_PATH_COMPONENTS: usize = 128;
const BOOT_PLAN_TIMEOUT: Duration = Duration::from_secs(30);

/// A complete pure inventory of Stone-owned bytes needed by boot repair.
///
/// The plan is deliberately non-`Clone`. The following slice replaces each
/// digest with one sealed anonymous snapshot before any repair attempt can be
/// claimed.
pub(crate) struct PreparedActiveReblitBootAssetPlan {
    state_ids: Vec<state::Id>,
    assets: Vec<PlannedBootAsset>,
    schema_requirements: Vec<PlannedBootSchemaRequirement>,
    systemd_boot_index: usize,
    kernel_count: usize,
    snapshot_digests: Vec<u128>,
}

impl PreparedActiveReblitBootAssetPlan {
    pub(crate) fn state_ids(&self) -> &[state::Id] {
        &self.state_ids
    }

    pub(crate) fn assets(&self) -> &[PlannedBootAsset] {
        &self.assets
    }

    pub(crate) fn schema_requirements(&self) -> &[PlannedBootSchemaRequirement] {
        &self.schema_requirements
    }

    pub(crate) fn systemd_boot(&self) -> &PlannedBootAsset {
        &self.assets[self.systemd_boot_index]
    }

    pub(crate) fn kernel_count(&self) -> usize {
        self.kernel_count
    }

    /// Canonical CAS inputs for the sealed snapshot phase.
    ///
    /// Structural planning cannot know cached file lengths. The snapshot
    /// phase still applies its per-file and aggregate byte bounds before the
    /// durable boot-repair attempt is claimed.
    pub(crate) fn snapshot_digests(&self) -> &[u128] {
        &self.snapshot_digests
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootAssetPlanNotApplicable {
    NoSystemdBootAsset,
    NoKernel,
}

pub(crate) enum BootAssetPlanOutcome {
    NotApplicable(BootAssetPlanNotApplicable),
    Ready(PreparedActiveReblitBootAssetPlan),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum BootAssetRole {
    SystemdBoot,
    OsInfo,
    GlobalCmdline,
    Kernel { version: String },
    Initrd { version: String },
    KernelCmdline { version: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootSchemaSource {
    OsInfoAsset,
    GeneratedOsRelease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootSchemaFallback {
    Required,
    Global,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlannedBootSchemaRequirement {
    state_id: state::Id,
    source: BootSchemaSource,
    fallback: BootSchemaFallback,
}

impl PlannedBootSchemaRequirement {
    pub(crate) fn state_id(self) -> state::Id {
        self.state_id
    }

    pub(crate) fn source(self) -> BootSchemaSource {
        self.source
    }

    pub(crate) fn fallback(self) -> BootSchemaFallback {
        self.fallback
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PlannedBootAsset {
    state_id: state::Id,
    logical_path: PathBuf,
    resolved_path: PathBuf,
    digest: u128,
    role: BootAssetRole,
}

impl PlannedBootAsset {
    pub(crate) fn state_id(&self) -> state::Id {
        self.state_id
    }

    pub(crate) fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub(crate) fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    pub(crate) fn digest(&self) -> u128 {
        self.digest
    }

    pub(crate) fn role(&self) -> &BootAssetRole {
        &self.role
    }
}

impl PreparedActiveReblitBootProjection {
    pub(crate) fn prepare_asset_plan(&self) -> Result<BootAssetPlanOutcome, ActiveReblitBootAssetPlanError> {
        prepare_asset_plan(self, BootAssetPlanPolicy::production())
    }
}

#[derive(Clone, Copy)]
struct BootAssetPlanPolicy {
    max_assets: usize,
    max_path_bytes: usize,
    max_kernels: usize,
    max_snapshot_digests: usize,
    max_symlink_hops: usize,
    max_work: usize,
    timeout: Duration,
}

impl BootAssetPlanPolicy {
    const fn production() -> Self {
        Self {
            max_assets: MAX_BOOT_PLAN_ASSETS,
            max_path_bytes: MAX_BOOT_PLAN_PATH_BYTES,
            max_kernels: MAX_BOOT_PLAN_KERNELS,
            max_snapshot_digests: MAX_BOOT_PLAN_SNAPSHOT_DIGESTS,
            max_symlink_hops: MAX_BOOT_PLAN_SYMLINK_HOPS,
            max_work: MAX_BOOT_PLAN_WORK,
            timeout: BOOT_PLAN_TIMEOUT,
        }
    }
}

struct PlanBudget {
    deadline: Instant,
    policy: BootAssetPlanPolicy,
    work: usize,
    candidates: usize,
    path_bytes: usize,
}

impl PlanBudget {
    fn new(policy: BootAssetPlanPolicy) -> Result<Self, ActiveReblitBootAssetPlanError> {
        let deadline =
            Instant::now()
                .checked_add(policy.timeout)
                .ok_or(ActiveReblitBootAssetPlanError::InvalidDeadline {
                    timeout: policy.timeout,
                })?;
        Ok(Self {
            deadline,
            policy,
            work: 0,
            candidates: 0,
            path_bytes: 0,
        })
    }

    fn step(&mut self) -> Result<(), ActiveReblitBootAssetPlanError> {
        if Instant::now() > self.deadline {
            return Err(ActiveReblitBootAssetPlanError::DeadlineExceeded {
                timeout: self.policy.timeout,
            });
        }
        self.work = self.work.checked_add(1).unwrap_or(usize::MAX);
        if self.work > self.policy.max_work {
            return Err(ActiveReblitBootAssetPlanError::WorkLimit {
                limit: self.policy.max_work,
                actual: self.work,
            });
        }
        Ok(())
    }

    fn admit_candidate(&mut self, logical: &Path) -> Result<(), ActiveReblitBootAssetPlanError> {
        self.step()?;
        let actual = self.candidates.saturating_add(1);
        if actual > self.policy.max_assets {
            return Err(ActiveReblitBootAssetPlanError::AssetCountLimit {
                limit: self.policy.max_assets,
                actual,
            });
        }
        let bytes = logical.as_os_str().len();
        let actual_bytes = self.path_bytes.checked_add(bytes).unwrap_or(usize::MAX);
        if actual_bytes > self.policy.max_path_bytes {
            return Err(ActiveReblitBootAssetPlanError::PathByteLimit {
                limit: self.policy.max_path_bytes,
                actual: actual_bytes,
            });
        }
        self.candidates = actual;
        self.path_bytes = actual_bytes;
        Ok(())
    }

    fn admit_resolved_path(&mut self, resolved: &Path) -> Result<(), ActiveReblitBootAssetPlanError> {
        self.step()?;
        let actual = self
            .path_bytes
            .checked_add(resolved.as_os_str().len())
            .unwrap_or(usize::MAX);
        if actual > self.policy.max_path_bytes {
            return Err(ActiveReblitBootAssetPlanError::PathByteLimit {
                limit: self.policy.max_path_bytes,
                actual,
            });
        }
        self.path_bytes = actual;
        Ok(())
    }
}

struct IndexedLayout<'a> {
    package: &'a package::Id,
    record: &'a StonePayloadLayoutRecord,
}

struct StateLayoutIndex<'a> {
    entries: BTreeMap<PathBuf, IndexedLayout<'a>>,
}

impl<'a> StateLayoutIndex<'a> {
    fn build(
        projection: &'a PreparedActiveReblitBootProjection,
        state_id: state::Id,
        selected: &BTreeSet<&str>,
        budget: &mut PlanBudget,
    ) -> Result<Self, ActiveReblitBootAssetPlanError> {
        let mut entries = BTreeMap::<PathBuf, IndexedLayout<'a>>::new();
        for (package, record) in projection.layouts() {
            budget.step()?;
            if !selected.contains(package.as_str()) {
                continue;
            }
            super::super::require_usr_relative_stone_layout(package, record).map_err(|source| {
                ActiveReblitBootAssetPlanError::InvalidLayout {
                    state_id: i32::from(state_id),
                    package: package.clone(),
                    source: Box::new(source),
                }
            })?;
            let path = PathBuf::from("/usr").join(record.file.target());
            require_boot_layout_metadata(state_id, package, &path, record)?;
            match entries.get(&path) {
                Some(existing) if existing.record == record => {}
                Some(existing) => {
                    return Err(ActiveReblitBootAssetPlanError::ConflictingPath {
                        state_id: i32::from(state_id),
                        path,
                        first: existing.package.clone(),
                        second: package.clone(),
                    });
                }
                None => {
                    entries.insert(path, IndexedLayout { package, record });
                }
            }
        }
        let index = Self { entries };
        index.require_layout_hierarchy(state_id, budget)?;
        Ok(index)
    }

    fn require_layout_hierarchy(
        &self,
        state_id: state::Id,
        budget: &mut PlanBudget,
    ) -> Result<(), ActiveReblitBootAssetPlanError> {
        for path in self.entries.keys() {
            let mut ancestor = path.parent();
            while let Some(path) = ancestor.filter(|path| *path != Path::new("/usr")) {
                budget.step()?;
                if let Some(indexed) = self.entries.get(path) {
                    match &indexed.record.file {
                        StonePayloadLayoutFile::Directory(_) => {}
                        StonePayloadLayoutFile::Symlink(..) => {
                            return Err(ActiveReblitBootAssetPlanError::SymlinkAncestor {
                                state_id: i32::from(state_id),
                                ancestor: path.to_owned(),
                            });
                        }
                        _ => {
                            return Err(ActiveReblitBootAssetPlanError::AncestorNotDirectory {
                                state_id: i32::from(state_id),
                                path: path.to_owned(),
                            });
                        }
                    }
                }
                ancestor = path.parent();
            }
        }
        Ok(())
    }

    fn resolve_regular(
        &self,
        state_id: state::Id,
        logical_path: &Path,
        budget: &mut PlanBudget,
    ) -> Result<(PathBuf, u128), ActiveReblitBootAssetPlanError> {
        let mut current = logical_path.to_owned();
        let mut visited = BTreeSet::new();
        for _ in 0..=budget.policy.max_symlink_hops {
            budget.step()?;
            require_resolved_boot_path(state_id, logical_path, &current, budget)?;
            if !visited.insert(current.clone()) {
                return Err(ActiveReblitBootAssetPlanError::SymlinkCycle {
                    state_id: i32::from(state_id),
                    logical: logical_path.to_owned(),
                });
            }

            let Some(indexed) = self.entries.get(&current) else {
                return Err(ActiveReblitBootAssetPlanError::MissingPath {
                    state_id: i32::from(state_id),
                    logical: logical_path.to_owned(),
                    missing: current,
                });
            };
            match &indexed.record.file {
                StonePayloadLayoutFile::Regular(digest, _) => {
                    if indexed.record.mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
                        return Err(ActiveReblitBootAssetPlanError::InvalidRegularMode {
                            state_id: i32::from(state_id),
                            path: current,
                            mode: indexed.record.mode,
                        });
                    }
                    return Ok((current, *digest));
                }
                StonePayloadLayoutFile::Symlink(target, _) => {
                    require_symlink_mode(state_id, &current, indexed.record.mode)?;
                    current = resolve_boot_symlink(state_id, logical_path, &current, target.as_str(), budget)?;
                }
                _ => {
                    return Err(ActiveReblitBootAssetPlanError::BootAssetNotRegular {
                        state_id: i32::from(state_id),
                        path: current,
                    });
                }
            }
        }
        Err(ActiveReblitBootAssetPlanError::SymlinkDepthLimit {
            state_id: i32::from(state_id),
            logical: logical_path.to_owned(),
            limit: budget.policy.max_symlink_hops,
        })
    }
}

fn prepare_asset_plan(
    projection: &PreparedActiveReblitBootProjection,
    policy: BootAssetPlanPolicy,
) -> Result<BootAssetPlanOutcome, ActiveReblitBootAssetPlanError> {
    let mut budget = PlanBudget::new(policy)?;
    match head_systemd_candidate_count(projection, &mut budget)? {
        0 => {
            return Ok(BootAssetPlanOutcome::NotApplicable(
                BootAssetPlanNotApplicable::NoSystemdBootAsset,
            ));
        }
        1 => {}
        count => {
            return Err(ActiveReblitBootAssetPlanError::AmbiguousSystemdBootAssets { count });
        }
    }

    let state_ids = projection.states().iter().map(|state| state.id).collect::<Vec<_>>();
    let mut state_plans = Vec::with_capacity(projection.states().len());
    let mut schema_requirements = Vec::with_capacity(projection.states().len());
    let mut kernel_count = 0usize;
    for (state_index, state) in projection.states().iter().enumerate() {
        budget.step()?;
        let selected = selected_packages(state);
        let index = StateLayoutIndex::build(projection, state.id, &selected, &mut budget)?;
        let kernel_versions = index
            .entries
            .keys()
            .filter_map(|path| {
                kernel_candidate(path).and_then(|(version, name)| (name == "vmlinuz").then_some(version))
            })
            .collect::<BTreeSet<_>>();
        kernel_count = kernel_count.saturating_add(kernel_versions.len());
        if kernel_count > policy.max_kernels {
            return Err(ActiveReblitBootAssetPlanError::KernelCountLimit {
                limit: policy.max_kernels,
                actual: kernel_count,
            });
        }

        // `os_schema_for_root` prefers package-owned os-info whenever that
        // logical path is present and consults Cast's generated os-release
        // only when it is absent. Preserve that requirement without claiming
        // the reserved generated file is a Stone/CAS asset. The head schema
        // is mandatory; a missing or invalid historical local schema falls
        // back to that already-authenticated global schema.
        let schema_fallback = if state_index == 0 {
            Some(BootSchemaFallback::Required)
        } else if !kernel_versions.is_empty() {
            Some(BootSchemaFallback::Global)
        } else {
            None
        };
        let schema_source = if schema_fallback.is_some() {
            if index.entries.contains_key(Path::new("/usr/lib/os-info.json")) {
                Some(BootSchemaSource::OsInfoAsset)
            } else {
                Some(BootSchemaSource::GeneratedOsRelease)
            }
        } else {
            None
        };
        if let Some(source) = schema_source {
            schema_requirements.push(PlannedBootSchemaRequirement {
                state_id: state.id,
                source,
                fallback: schema_fallback.expect("a schema source is selected only when schema policy applies"),
            });
        }

        let mut candidates = Vec::new();
        for path in index.entries.keys() {
            budget.step()?;
            let role = if state_index == 0 && is_systemd_boot_candidate(path) {
                Some(BootAssetRole::SystemdBoot)
            } else if path == Path::new("/usr/lib/os-info.json") && schema_source == Some(BootSchemaSource::OsInfoAsset)
            {
                Some(BootAssetRole::OsInfo)
            } else if !kernel_versions.is_empty() && is_global_cmdline(path) {
                Some(BootAssetRole::GlobalCmdline)
            } else if let Some((version, name)) = kernel_candidate(path) {
                if !kernel_versions.contains(&version) {
                    None
                } else {
                    classify_kernel_asset(version, name)
                }
            } else {
                None
            };
            let Some(role) = role else {
                continue;
            };
            budget.admit_candidate(path)?;
            candidates.push((path.clone(), role));
        }
        state_plans.push((state.id, candidates));
    }

    if kernel_count == 0 {
        return Ok(BootAssetPlanOutcome::NotApplicable(
            BootAssetPlanNotApplicable::NoKernel,
        ));
    }

    let mut assets = Vec::new();
    let mut snapshot_digests = BTreeSet::new();
    let mut systemd_boot_index = None;
    for (state, (state_id, candidates)) in projection.states().iter().zip(state_plans) {
        debug_assert_eq!(state.id, state_id);
        let selected = selected_packages(state);
        let index = StateLayoutIndex::build(projection, state_id, &selected, &mut budget)?;
        for (path, role) in candidates {
            let (resolved_path, digest) = index.resolve_regular(state_id, &path, &mut budget)?;
            budget.admit_resolved_path(&resolved_path)?;
            if digest == super::super::EMPTY_FILE_DIGEST && role_requires_nonempty(&role) {
                return Err(ActiveReblitBootAssetPlanError::EmptyCriticalAsset {
                    state_id: i32::from(state_id),
                    path,
                    role,
                });
            }
            if snapshot_digests.insert(digest) && snapshot_digests.len() > policy.max_snapshot_digests {
                return Err(ActiveReblitBootAssetPlanError::SnapshotDigestCountLimit {
                    limit: policy.max_snapshot_digests,
                    actual: snapshot_digests.len(),
                });
            }
            if role == BootAssetRole::SystemdBoot {
                systemd_boot_index = Some(assets.len());
            }
            assets.push(PlannedBootAsset {
                state_id,
                logical_path: path,
                resolved_path,
                digest,
                role,
            });
        }
    }

    Ok(BootAssetPlanOutcome::Ready(PreparedActiveReblitBootAssetPlan {
        state_ids,
        assets,
        schema_requirements,
        systemd_boot_index: systemd_boot_index.expect("exactly one systemd-boot candidate classified"),
        kernel_count,
        snapshot_digests: snapshot_digests.into_iter().collect(),
    }))
}

fn selected_packages(state: &crate::State) -> BTreeSet<&str> {
    state
        .selections
        .iter()
        .map(|selection| selection.package.as_str())
        .collect()
}

fn head_systemd_candidate_count(
    projection: &PreparedActiveReblitBootProjection,
    budget: &mut PlanBudget,
) -> Result<usize, ActiveReblitBootAssetPlanError> {
    let head = projection.head();
    let selected = selected_packages(head);
    let mut paths = BTreeSet::new();
    for (package, record) in projection.layouts() {
        budget.step()?;
        if !selected.contains(package.as_str()) {
            continue;
        }
        let path = PathBuf::from("/usr").join(record.file.target());
        if !is_systemd_boot_candidate(&path) {
            continue;
        }
        super::super::require_usr_relative_stone_layout(package, record).map_err(|source| {
            ActiveReblitBootAssetPlanError::InvalidLayout {
                state_id: i32::from(head.id),
                package: package.clone(),
                source: Box::new(source),
            }
        })?;
        paths.insert(path);
        if paths.len() == 2 {
            return Ok(2);
        }
    }
    Ok(paths.len())
}

fn kernel_candidate(path: &Path) -> Option<(String, &str)> {
    let raw = path.strip_prefix("/usr").ok()?;
    let components = raw.components().collect::<Vec<_>>();
    if components.len() != 4 || components[0].as_os_str() != "lib" || components[1].as_os_str() != "kernel" {
        return None;
    }
    let version = components[2].as_os_str().to_str()?.to_owned();
    let name = components[3].as_os_str().to_str()?;
    Some((version, name))
}

fn classify_kernel_asset(version: String, name: &str) -> Option<BootAssetRole> {
    match name {
        "vmlinuz" => Some(BootAssetRole::Kernel { version }),
        _ if name.ends_with(".initrd") => Some(BootAssetRole::Initrd { version }),
        _ if name.ends_with(".cmdline") => Some(BootAssetRole::KernelCmdline { version }),
        _ => None,
    }
}

fn role_requires_nonempty(role: &BootAssetRole) -> bool {
    matches!(
        role,
        BootAssetRole::SystemdBoot
            | BootAssetRole::OsInfo
            | BootAssetRole::Kernel { .. }
            | BootAssetRole::Initrd { .. }
    )
}

fn is_global_cmdline(path: &Path) -> bool {
    let Ok(raw) = path.strip_prefix("/usr/lib/kernel/cmdline.d") else {
        return false;
    };
    raw.components().count() == 1 && path.extension().is_some_and(|extension| extension == "cmdline")
}

fn is_systemd_boot_candidate(path: &Path) -> bool {
    let Ok(raw) = path.strip_prefix("/usr") else {
        return false;
    };
    let components = raw.components().collect::<Vec<_>>();
    components.len() == 5
        && components[0]
            .as_os_str()
            .to_str()
            .is_some_and(|component| component.starts_with("lib"))
        && components[1].as_os_str() == "systemd"
        && components[2].as_os_str() == "boot"
        && components[3].as_os_str() == "efi"
        && components[4].as_os_str() == "systemd-bootx64.efi"
}

fn require_boot_layout_metadata(
    state_id: state::Id,
    package: &package::Id,
    path: &Path,
    record: &StonePayloadLayoutRecord,
) -> Result<(), ActiveReblitBootAssetPlanError> {
    if record.uid != 0 || record.gid != 0 {
        return Err(ActiveReblitBootAssetPlanError::UnsupportedOwnership {
            state_id: i32::from(state_id),
            package: package.clone(),
            path: path.to_owned(),
            uid: record.uid,
            gid: record.gid,
        });
    }

    let expected = match &record.file {
        StonePayloadLayoutFile::Regular(..) => nix::libc::S_IFREG,
        StonePayloadLayoutFile::Directory(_) => nix::libc::S_IFDIR,
        StonePayloadLayoutFile::Symlink(target, _) => {
            if target.is_empty() || target.len() > MAX_BOOT_PLAN_SINGLE_PATH_BYTES || target.as_bytes().contains(&0) {
                return Err(ActiveReblitBootAssetPlanError::InvalidSymlinkTarget {
                    state_id: i32::from(state_id),
                    path: path.to_owned(),
                    target: bounded_diagnostic(target.as_str()),
                });
            }
            nix::libc::S_IFLNK
        }
        _ => {
            return Err(ActiveReblitBootAssetPlanError::UnsupportedLayoutKind {
                state_id: i32::from(state_id),
                package: package.clone(),
                path: path.to_owned(),
            });
        }
    };
    let actual = record.mode & nix::libc::S_IFMT;
    let unsupported = record.mode & !(nix::libc::S_IFMT | 0o7777);
    let enforceable_symlink = expected != nix::libc::S_IFLNK || record.mode & 0o7777 == 0o777;
    if actual == expected && unsupported == 0 && enforceable_symlink {
        return Ok(());
    }

    let error = match &record.file {
        StonePayloadLayoutFile::Regular(..) => ActiveReblitBootAssetPlanError::InvalidRegularMode {
            state_id: i32::from(state_id),
            path: path.to_owned(),
            mode: record.mode,
        },
        StonePayloadLayoutFile::Directory(_) => ActiveReblitBootAssetPlanError::InvalidDirectoryMode {
            state_id: i32::from(state_id),
            path: path.to_owned(),
            mode: record.mode,
        },
        StonePayloadLayoutFile::Symlink(..) => ActiveReblitBootAssetPlanError::InvalidSymlinkMode {
            state_id: i32::from(state_id),
            path: path.to_owned(),
            mode: record.mode,
        },
        _ => unreachable!("unsupported layout kind returned above"),
    };
    Err(error)
}

fn require_resolved_boot_path(
    state_id: state::Id,
    logical: &Path,
    resolved: &Path,
    budget: &mut PlanBudget,
) -> Result<(), ActiveReblitBootAssetPlanError> {
    if !resolved.starts_with("/usr") || resolved == Path::new("/usr") {
        return Err(ActiveReblitBootAssetPlanError::SymlinkEscape {
            state_id: i32::from(state_id),
            logical: logical.to_owned(),
            resolved: resolved.to_owned(),
        });
    }
    let bytes = resolved.as_os_str().len();
    if bytes > MAX_BOOT_PLAN_SINGLE_PATH_BYTES {
        return Err(ActiveReblitBootAssetPlanError::ResolvedPathByteLimit {
            state_id: i32::from(state_id),
            logical: logical.to_owned(),
            limit: MAX_BOOT_PLAN_SINGLE_PATH_BYTES,
            actual: bytes,
        });
    }
    let components = resolved
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    if components > MAX_BOOT_PLAN_PATH_COMPONENTS {
        return Err(ActiveReblitBootAssetPlanError::ResolvedPathDepthLimit {
            state_id: i32::from(state_id),
            logical: logical.to_owned(),
            limit: MAX_BOOT_PLAN_PATH_COMPONENTS,
            actual: components,
        });
    }
    for _ in 0..components {
        budget.step()?;
    }
    Ok(())
}

fn resolve_boot_symlink(
    state_id: state::Id,
    logical: &Path,
    link: &Path,
    target: &str,
    budget: &mut PlanBudget,
) -> Result<PathBuf, ActiveReblitBootAssetPlanError> {
    let resolved = super::super::normalize_frozen_symlink_target(link, target).ok_or_else(|| {
        ActiveReblitBootAssetPlanError::InvalidSymlinkTarget {
            state_id: i32::from(state_id),
            path: link.to_owned(),
            target: bounded_diagnostic(target),
        }
    })?;
    require_resolved_boot_path(state_id, logical, &resolved, budget)?;
    Ok(resolved)
}

fn require_symlink_mode(state_id: state::Id, path: &Path, mode: u32) -> Result<(), ActiveReblitBootAssetPlanError> {
    if mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || mode & 0o7777 != 0o777 {
        return Err(ActiveReblitBootAssetPlanError::InvalidSymlinkMode {
            state_id: i32::from(state_id),
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

fn bounded_diagnostic(value: &str) -> String {
    const LIMIT: usize = 256;
    if value.len() <= LIMIT {
        return value.to_owned();
    }
    let mut end = LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ActiveReblitBootAssetPlanError {
    #[error("boot asset-plan deadline could not represent {timeout:?}")]
    InvalidDeadline { timeout: Duration },
    #[error("boot asset planning exceeded its {timeout:?} deadline")]
    DeadlineExceeded { timeout: Duration },
    #[error("boot asset planning exceeds {limit} bounded steps (got {actual})")]
    WorkLimit { limit: usize, actual: usize },
    #[error("boot asset plan exceeds {limit} assets (got {actual})")]
    AssetCountLimit { limit: usize, actual: usize },
    #[error("boot asset paths exceed {limit} bytes (got {actual})")]
    PathByteLimit { limit: usize, actual: usize },
    #[error("boot asset plan exceeds {limit} kernels (got {actual})")]
    KernelCountLimit { limit: usize, actual: usize },
    #[error("boot asset plan exceeds {limit} unique CAS snapshot digests (got {actual})")]
    SnapshotDigestCountLimit { limit: usize, actual: usize },
    #[error("state {state_id} boot asset {path:?} for {role:?} is the canonical empty file")]
    EmptyCriticalAsset {
        state_id: i32,
        path: PathBuf,
        role: BootAssetRole,
    },
    #[error("state {state_id} contains invalid package layout for {package}")]
    InvalidLayout {
        state_id: i32,
        package: package::Id,
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("state {state_id} package {package} gives unsupported ownership {uid}:{gid} to {path:?}")]
    UnsupportedOwnership {
        state_id: i32,
        package: package::Id,
        path: PathBuf,
        uid: u32,
        gid: u32,
    },
    #[error("state {state_id} package {package} gives {path:?} an unsupported inode kind")]
    UnsupportedLayoutKind {
        state_id: i32,
        package: package::Id,
        path: PathBuf,
    },
    #[error("state {state_id} has conflicting owners {first} and {second} for {path:?}")]
    ConflictingPath {
        state_id: i32,
        path: PathBuf,
        first: package::Id,
        second: package::Id,
    },
    #[error("state {state_id} boot path {logical:?} is missing at {missing:?}")]
    MissingPath {
        state_id: i32,
        logical: PathBuf,
        missing: PathBuf,
    },
    #[error("state {state_id} boot path {path:?} is not a regular file")]
    BootAssetNotRegular { state_id: i32, path: PathBuf },
    #[error("state {state_id} regular boot path {path:?} has invalid mode {mode:o}")]
    InvalidRegularMode { state_id: i32, path: PathBuf, mode: u32 },
    #[error("state {state_id} directory {path:?} has invalid mode {mode:o}")]
    InvalidDirectoryMode { state_id: i32, path: PathBuf, mode: u32 },
    #[error("state {state_id} symlink {path:?} has invalid mode {mode:o}")]
    InvalidSymlinkMode { state_id: i32, path: PathBuf, mode: u32 },
    #[error("state {state_id} symlink {path:?} has invalid target {target:?}")]
    InvalidSymlinkTarget {
        state_id: i32,
        path: PathBuf,
        target: String,
    },
    #[error("state {state_id} boot symlink for {logical:?} escapes to {resolved:?}")]
    SymlinkEscape {
        state_id: i32,
        logical: PathBuf,
        resolved: PathBuf,
    },
    #[error("state {state_id} boot symlink for {logical:?} resolves to {actual} path bytes, exceeding {limit}")]
    ResolvedPathByteLimit {
        state_id: i32,
        logical: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("state {state_id} boot symlink for {logical:?} resolves to {actual} components, exceeding {limit}")]
    ResolvedPathDepthLimit {
        state_id: i32,
        logical: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("state {state_id} boot symlink for {logical:?} forms a cycle")]
    SymlinkCycle { state_id: i32, logical: PathBuf },
    #[error("state {state_id} boot symlink for {logical:?} exceeds {limit} hops")]
    SymlinkDepthLimit {
        state_id: i32,
        logical: PathBuf,
        limit: usize,
    },
    #[error("state {state_id} boot path ancestor {path:?} is not a directory")]
    AncestorNotDirectory { state_id: i32, path: PathBuf },
    #[error("state {state_id} layout stores a descendant beneath symlink {ancestor:?}")]
    SymlinkAncestor { state_id: i32, ancestor: PathBuf },
    #[error("active state declares at least {count} distinct systemd-bootx64 EFI assets")]
    AmbiguousSystemdBootAssets { count: usize },
}

#[cfg(test)]
#[path = "active_reblit_asset_plan_tests.rs"]
mod tests;
