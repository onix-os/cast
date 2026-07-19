//! Pure syntactic destination planning for an ActiveReblit boot publication.
//!
//! This module owns no filesystem descriptors and performs no path I/O.  It
//! turns already-sealed payloads and already-generated control bytes into one
//! bounded, canonical publication list for a later descriptor-safe worker.
//! The resulting value is not destination authority, a complete blsforme
//! projection, or a durability proof. Validated scalar topology scopes only
//! the collision domains and is retained for later layout revalidation. The
//! later aggregate must still bind every Stone binding to the same non-`Clone`
//! input owner, revalidate descriptor-retained ESP/BOOT authority, inspect
//! existing directories, and own the mutation barriers.

use std::{
    collections::BTreeMap,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::client::active_reblit_mounted_boot_topology::BoundActiveReblitMountedBootTopology;

#[path = "active_reblit_publication_plan/error.rs"]
mod error;
#[path = "active_reblit_publication_plan/role_binding.rs"]
mod role_binding;

pub(in crate::client) use error::ActiveReblitBootPublicationPlanError;

pub(in crate::client) const ACTIVE_REBLIT_BOOT_OUTPUT_MODE: u32 = 0o644;
pub(in crate::client) const MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS: usize = 8_336;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_COMPONENTS: usize = 16;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_SINGLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
const MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES: usize = 255;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_LOGICAL_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_SEALED_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_GENERATED_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_GENERATED_FILE_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_WORK: usize = 1_000_000;
const SORT_WORK_PER_ELEMENT_LEVEL: usize = 4;

const ACTIVE_REBLIT_LOADER_CONTROL_PATH: &str = "loader/loader.conf";
const ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH: &str = "EFI/Boot/BOOTX64.EFI";
const ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH: &str = "EFI/systemd/systemd-bootx64.efi";

/// Logical destination root later matched to retained descriptor authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::client) enum ActiveReblitBootDestinationRoot {
    Esp,
    Boot,
}

/// Retained scalar collision layout; never fresh topology or authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootDestinationLayout {
    BootAliasesEsp,
    DistinctXbootldr,
}

/// Physical destination namespace used only for pure collision planning.
///
/// This is derived from an already-validated mounted topology. It is not a
/// descriptor, destination authority, or permission to perform I/O.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ActiveReblitBootDestinationCollisionDomain {
    SharedEspAndBoot,
    Esp,
    Boot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveReblitBootDestinationCollisionDomains {
    esp: ActiveReblitBootDestinationCollisionDomain,
    boot: ActiveReblitBootDestinationCollisionDomain,
}

impl ActiveReblitBootDestinationCollisionDomains {
    fn from_topology(topology: BoundActiveReblitMountedBootTopology<'_>) -> Self {
        match topology {
            BoundActiveReblitMountedBootTopology::BootAliasesEsp { .. } => Self::boot_aliases_esp(),
            BoundActiveReblitMountedBootTopology::DistinctXbootldr { .. } => Self::distinct_xbootldr(),
        }
    }

    const fn boot_aliases_esp() -> Self {
        Self {
            esp: ActiveReblitBootDestinationCollisionDomain::SharedEspAndBoot,
            boot: ActiveReblitBootDestinationCollisionDomain::SharedEspAndBoot,
        }
    }

    const fn distinct_xbootldr() -> Self {
        Self {
            esp: ActiveReblitBootDestinationCollisionDomain::Esp,
            boot: ActiveReblitBootDestinationCollisionDomain::Boot,
        }
    }

    const fn for_root(self, root: ActiveReblitBootDestinationRoot) -> ActiveReblitBootDestinationCollisionDomain {
        match root {
            ActiveReblitBootDestinationRoot::Esp => self.esp,
            ActiveReblitBootDestinationRoot::Boot => self.boot,
        }
    }

    const fn layout(self) -> ActiveReblitBootDestinationLayout {
        match (self.esp, self.boot) {
            (
                ActiveReblitBootDestinationCollisionDomain::SharedEspAndBoot,
                ActiveReblitBootDestinationCollisionDomain::SharedEspAndBoot,
            ) => ActiveReblitBootDestinationLayout::BootAliasesEsp,
            (ActiveReblitBootDestinationCollisionDomain::Esp, ActiveReblitBootDestinationCollisionDomain::Boot) => {
                ActiveReblitBootDestinationLayout::DistinctXbootldr
            }
            _ => panic!("invalid boot destination collision layout"),
        }
    }
}

/// Publication order, from independent payloads through control-plane files.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::client) enum ActiveReblitBootPublicationPhase {
    Payload,
    Entry,
    LoaderControl,
    Bootloader,
}

/// Semantic output role. Each role fixes its source kind, root and phase.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::client) enum ActiveReblitBootPublicationRole {
    Payload,
    Entry,
    LoaderControl,
    FallbackBootloader,
    SystemdBootloader,
}

impl ActiveReblitBootPublicationRole {
    const fn root(self) -> ActiveReblitBootDestinationRoot {
        match self {
            Self::Payload | Self::Entry | Self::LoaderControl => ActiveReblitBootDestinationRoot::Boot,
            Self::FallbackBootloader | Self::SystemdBootloader => ActiveReblitBootDestinationRoot::Esp,
        }
    }

    const fn phase(self) -> ActiveReblitBootPublicationPhase {
        match self {
            Self::Payload => ActiveReblitBootPublicationPhase::Payload,
            Self::Entry => ActiveReblitBootPublicationPhase::Entry,
            Self::LoaderControl => ActiveReblitBootPublicationPhase::LoaderControl,
            Self::FallbackBootloader | Self::SystemdBootloader => ActiveReblitBootPublicationPhase::Bootloader,
        }
    }

    const fn requires_sealed_source(self) -> bool {
        matches!(self, Self::Payload | Self::FallbackBootloader | Self::SystemdBootloader)
    }
}

/// Immutable bytes backing one planned destination.
///
/// This type is deliberately non-`Clone`: generated bytes have one owner and
/// sealed snapshot identity must not silently fan out outside the plan.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootPublicationSource {
    /// `binding_index` identifies an asset binding in the exact Stone owner
    /// from which this plan was rendered. It is not authority by itself. The
    /// final aggregate must match index, digest and length to that same owner
    /// before execution.
    SealedSnapshot {
        binding_index: u16,
        digest: u128,
        length: u64,
    },
    Generated {
        bytes: Box<[u8]>,
        digest: u128,
    },
}

/// Unprepared source declaration. Generated content deliberately carries no
/// caller-supplied digest: hashing begins only after preparation has admitted
/// the per-file and aggregate byte bounds.
enum ActiveReblitBootPublicationRequestSource {
    SealedSnapshot {
        binding_index: u16,
        digest: u128,
        length: u64,
    },
    Generated {
        bytes: Box<[u8]>,
    },
}

impl ActiveReblitBootPublicationRequestSource {
    const fn is_sealed(&self) -> bool {
        matches!(self, Self::SealedSnapshot { .. })
    }
}

impl ActiveReblitBootPublicationSource {
    pub(in crate::client) fn digest(&self) -> u128 {
        match self {
            Self::SealedSnapshot { digest, .. } | Self::Generated { digest, .. } => *digest,
        }
    }

    pub(in crate::client) fn length(&self) -> u64 {
        match self {
            Self::SealedSnapshot { length, .. } => *length,
            Self::Generated { bytes, .. } => u64::try_from(bytes.len()).expect("usize fits in u64"),
        }
    }

    pub(in crate::client) fn generated_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Generated { bytes, .. } => Some(bytes),
            Self::SealedSnapshot { .. } => None,
        }
    }

    pub(in crate::client) fn binding_index(&self) -> Option<u16> {
        match self {
            Self::SealedSnapshot { binding_index, .. } => Some(*binding_index),
            Self::Generated { .. } => None,
        }
    }

    const fn is_sealed(&self) -> bool {
        matches!(self, Self::SealedSnapshot { .. })
    }
}

/// An owned declaration admitted by [`PreparedActiveReblitBootPublicationPlan`].
///
/// The destination mode is intentionally absent. Every admitted output has the
/// single fixed mode [`ACTIVE_REBLIT_BOOT_OUTPUT_MODE`].
pub(in crate::client) struct ActiveReblitBootPublicationRequest {
    role: ActiveReblitBootPublicationRole,
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    relative_path: PathBuf,
    source: ActiveReblitBootPublicationRequestSource,
}

impl ActiveReblitBootPublicationRequest {
    pub(in crate::client) fn sealed_payload(
        relative_path: PathBuf,
        binding_index: u16,
        digest: u128,
        length: u64,
    ) -> Self {
        Self::sealed(
            ActiveReblitBootPublicationRole::Payload,
            relative_path,
            binding_index,
            digest,
            length,
        )
    }

    pub(in crate::client) fn generated_entry(relative_path: PathBuf, bytes: Box<[u8]>) -> Self {
        Self::generated(ActiveReblitBootPublicationRole::Entry, relative_path, bytes)
    }

    pub(in crate::client) fn generated_loader_control(bytes: Box<[u8]>) -> Self {
        Self::generated(
            ActiveReblitBootPublicationRole::LoaderControl,
            PathBuf::from(ACTIVE_REBLIT_LOADER_CONTROL_PATH),
            bytes,
        )
    }

    pub(in crate::client) fn sealed_fallback_bootloader(binding_index: u16, digest: u128, length: u64) -> Self {
        Self::sealed(
            ActiveReblitBootPublicationRole::FallbackBootloader,
            PathBuf::from(ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH),
            binding_index,
            digest,
            length,
        )
    }

    pub(in crate::client) fn sealed_systemd_bootloader(binding_index: u16, digest: u128, length: u64) -> Self {
        Self::sealed(
            ActiveReblitBootPublicationRole::SystemdBootloader,
            PathBuf::from(ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH),
            binding_index,
            digest,
            length,
        )
    }

    fn sealed(
        role: ActiveReblitBootPublicationRole,
        relative_path: PathBuf,
        binding_index: u16,
        digest: u128,
        length: u64,
    ) -> Self {
        Self {
            role,
            root: role.root(),
            phase: role.phase(),
            relative_path,
            source: ActiveReblitBootPublicationRequestSource::SealedSnapshot {
                binding_index,
                digest,
                length,
            },
        }
    }

    fn generated(role: ActiveReblitBootPublicationRole, relative_path: PathBuf, bytes: Box<[u8]>) -> Self {
        Self {
            role,
            root: role.root(),
            phase: role.phase(),
            relative_path,
            source: ActiveReblitBootPublicationRequestSource::Generated { bytes },
        }
    }

    /// Test-only raw declaration proving that syntactic preparation rejects
    /// impossible role/root/phase/source combinations even if constructors are
    /// bypassed inside this module.
    #[cfg(test)]
    fn raw(
        role: ActiveReblitBootPublicationRole,
        root: ActiveReblitBootDestinationRoot,
        phase: ActiveReblitBootPublicationPhase,
        relative_path: PathBuf,
        source: ActiveReblitBootPublicationRequestSource,
    ) -> Self {
        Self {
            role,
            root,
            phase,
            relative_path,
            source,
        }
    }
}

/// One canonical, executor-visible output.
///
/// Instances are created only by the prepared plan, after collision and bound
/// checks. The type is deliberately non-`Clone`.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct PlannedActiveReblitBootPublication {
    role: ActiveReblitBootPublicationRole,
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    relative_path: PathBuf,
    folded_relative_path: String,
    source: ActiveReblitBootPublicationSource,
}

impl PlannedActiveReblitBootPublication {
    pub(in crate::client) fn role(&self) -> ActiveReblitBootPublicationRole {
        self.role
    }

    pub(in crate::client) fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) fn phase(&self) -> ActiveReblitBootPublicationPhase {
        self.phase
    }

    pub(in crate::client) fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub(in crate::client) fn mode(&self) -> u32 {
        ACTIVE_REBLIT_BOOT_OUTPUT_MODE
    }

    pub(in crate::client) fn source(&self) -> &ActiveReblitBootPublicationSource {
        &self.source
    }
}

/// Bounded, canonical syntactic publication input.
///
/// There is intentionally no removal list, descriptor authority or mutation
/// API. A later aggregate must add the complete projection, freshly revalidate
/// the retained collision layout and physical root bindings, and own the
/// durable execution protocol before any output is writable.
#[derive(Debug)]
pub(in crate::client) struct PreparedActiveReblitBootPublicationPlan {
    outputs: Vec<PlannedActiveReblitBootPublication>,
    collision_domains: ActiveReblitBootDestinationCollisionDomains,
    logical_bytes: u64,
    generated_bytes: usize,
    path_bytes: usize,
    planning_work: usize,
}

impl PreparedActiveReblitBootPublicationPlan {
    /// Prepare a pure plan under the caller's absolute deadline and the exact
    /// alias/distinct collision layout proven by mounted-topology validation.
    pub(in crate::client) fn prepare_until(
        requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
        topology: BoundActiveReblitMountedBootTopology<'_>,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootPublicationPlanError> {
        prepare_publication_plan_until(
            requests,
            PublicationPlanPolicy::production(),
            ActiveReblitBootDestinationCollisionDomains::from_topology(topology),
            deadline,
        )
    }

    pub(in crate::client) fn outputs(&self) -> &[PlannedActiveReblitBootPublication] {
        &self.outputs
    }

    /// Check a freshly revalidated topology before a later aggregate trusts
    /// the collision decisions retained by this pure plan.
    pub(in crate::client) fn collision_domains_match(
        &self,
        topology: BoundActiveReblitMountedBootTopology<'_>,
    ) -> bool {
        self.collision_domains == ActiveReblitBootDestinationCollisionDomains::from_topology(topology)
    }

    pub(in crate::client) const fn destination_layout(&self) -> ActiveReblitBootDestinationLayout {
        self.collision_domains.layout()
    }

    pub(in crate::client) fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub(in crate::client) fn generated_bytes(&self) -> usize {
        self.generated_bytes
    }

    pub(in crate::client) fn path_bytes(&self) -> usize {
        self.path_bytes
    }

    /// Conservative logical work charge used with the independent count,
    /// path-byte and wall-clock limits. This is not an instruction count:
    /// ordered-map comparisons remain bounded by those separate limits.
    pub(in crate::client) fn planning_work(&self) -> usize {
        self.planning_work
    }
}

#[derive(Clone, Copy)]
struct PublicationPlanPolicy {
    max_publications: usize,
    max_path_bytes: usize,
    max_single_path_bytes: usize,
    max_components: usize,
    max_logical_bytes: u64,
    max_sealed_file_bytes: u64,
    max_generated_bytes: usize,
    max_generated_file_bytes: usize,
    max_work: usize,
}

impl PublicationPlanPolicy {
    const fn production() -> Self {
        Self {
            max_publications: MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS,
            max_path_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_PATH_BYTES,
            max_single_path_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_SINGLE_PATH_BYTES,
            max_components: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_COMPONENTS,
            max_logical_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_LOGICAL_BYTES,
            max_sealed_file_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_SEALED_FILE_BYTES,
            max_generated_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_GENERATED_BYTES,
            max_generated_file_bytes: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_GENERATED_FILE_BYTES,
            max_work: MAX_ACTIVE_REBLIT_BOOT_PUBLICATION_WORK,
        }
    }
}

struct PublicationPlanBudget {
    policy: PublicationPlanPolicy,
    deadline: Instant,
    work: usize,
    publications: usize,
    path_bytes: usize,
    declared_generated_bytes: usize,
}

impl PublicationPlanBudget {
    fn new_until(
        policy: PublicationPlanPolicy,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootPublicationPlanError> {
        let budget = Self {
            policy,
            deadline,
            work: 0,
            publications: 0,
            path_bytes: 0,
            declared_generated_bytes: 0,
        };
        budget.require_deadline()?;
        Ok(budget)
    }

    fn step(&mut self) -> Result<(), ActiveReblitBootPublicationPlanError> {
        self.reserve_work(1)
    }

    fn reserve_sort_work(&mut self, publications: usize) -> Result<(), ActiveReblitBootPublicationPlanError> {
        self.reserve_work(conservative_sort_work(publications))
    }

    fn reserve_work(&mut self, amount: usize) -> Result<(), ActiveReblitBootPublicationPlanError> {
        self.require_deadline()?;
        let actual = self.work.checked_add(amount).unwrap_or(usize::MAX);
        if actual > self.policy.max_work {
            return Err(ActiveReblitBootPublicationPlanError::WorkLimit {
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn require_deadline(&self) -> Result<(), ActiveReblitBootPublicationPlanError> {
        if Instant::now() > self.deadline {
            Err(ActiveReblitBootPublicationPlanError::DeadlineExceeded {
                deadline: self.deadline,
            })
        } else {
            Ok(())
        }
    }

    fn admit_request(&mut self, path: &Path) -> Result<(), ActiveReblitBootPublicationPlanError> {
        self.step()?;
        let publications = self.publications.saturating_add(1);
        if publications > self.policy.max_publications {
            return Err(ActiveReblitBootPublicationPlanError::PublicationCountLimit {
                limit: self.policy.max_publications,
                actual: publications,
            });
        }
        let path_bytes = self
            .path_bytes
            .checked_add(path.as_os_str().as_bytes().len())
            .unwrap_or(usize::MAX);
        if path_bytes > self.policy.max_path_bytes {
            return Err(ActiveReblitBootPublicationPlanError::PathByteLimit {
                limit: self.policy.max_path_bytes,
                actual: path_bytes,
            });
        }
        self.publications = publications;
        self.path_bytes = path_bytes;
        Ok(())
    }

    fn admit_generated(&mut self, path: &Path, bytes: usize) -> Result<(), ActiveReblitBootPublicationPlanError> {
        self.step()?;
        if bytes > self.policy.max_generated_file_bytes {
            return Err(ActiveReblitBootPublicationPlanError::GeneratedFileByteLimit {
                path: path.to_owned(),
                limit: self.policy.max_generated_file_bytes,
                actual: bytes,
            });
        }
        let total = self.declared_generated_bytes.checked_add(bytes).unwrap_or(usize::MAX);
        if total > self.policy.max_generated_bytes {
            return Err(ActiveReblitBootPublicationPlanError::GeneratedTotalByteLimit {
                limit: self.policy.max_generated_bytes,
                actual: total,
            });
        }
        self.declared_generated_bytes = total;
        Ok(())
    }
}

/// Reserve four comparison units for every element at every level of a
/// balanced comparison sort. This deliberately over-reserves the comparison
/// count while keeping the production maximum below the global work budget.
fn conservative_sort_work(publications: usize) -> usize {
    if publications < 2 {
        return 0;
    }
    let levels = usize::BITS as usize - (publications - 1).leading_zeros() as usize;
    publications
        .checked_mul(levels)
        .and_then(|work| work.checked_mul(SORT_WORK_PER_ELEMENT_LEVEL))
        .unwrap_or(usize::MAX)
}

fn prepare_publication_plan_until(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
    collision_domains: ActiveReblitBootDestinationCollisionDomains,
    deadline: Instant,
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    prepare_publication_plan_until_with_sort_checkpoint(requests, policy, collision_domains, deadline, || {})
}

fn prepare_publication_plan_until_with_sort_checkpoint(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
    collision_domains: ActiveReblitBootDestinationCollisionDomains,
    deadline: Instant,
    after_sort: impl FnOnce(),
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    prepare_publication_plan_until_with_checkpoints(requests, policy, collision_domains, deadline, after_sort, || {})
}

fn prepare_publication_plan_until_with_checkpoints(
    requests: impl IntoIterator<Item = ActiveReblitBootPublicationRequest>,
    policy: PublicationPlanPolicy,
    collision_domains: ActiveReblitBootDestinationCollisionDomains,
    deadline: Instant,
    after_sort: impl FnOnce(),
    after_plan_materialized: impl FnOnce(),
) -> Result<PreparedActiveReblitBootPublicationPlan, ActiveReblitBootPublicationPlanError> {
    let mut budget = PublicationPlanBudget::new_until(policy, deadline)?;
    let mut outputs = Vec::<PlannedActiveReblitBootPublication>::new();
    let mut destinations_by_domain =
        BTreeMap::<ActiveReblitBootDestinationCollisionDomain, BTreeMap<String, usize>>::new();

    for request in requests {
        budget.admit_request(&request.relative_path)?;
        let relative_path = require_normalized_relative_path(&request.relative_path, &mut budget)?;
        role_binding::require_role_binding(&request, &relative_path)?;
        let source = prepare_source(&relative_path, request.source, &mut budget)?;
        budget.step()?;

        let folded = case_folded_path(&relative_path);
        let collision_domain = collision_domains.for_root(request.root);
        let destinations = destinations_by_domain.entry(collision_domain).or_default();
        if let Some(existing_index) = destinations.get(folded.as_str()).copied() {
            let existing = &outputs[existing_index];
            if exact_duplicate(
                existing,
                request.role,
                request.root,
                request.phase,
                &relative_path,
                &source,
            ) {
                continue;
            }
            if existing.relative_path == relative_path {
                return Err(ActiveReblitBootPublicationPlanError::PublicationCollision {
                    first_root: existing.root,
                    second_root: request.root,
                    path: relative_path,
                });
            }
            return Err(ActiveReblitBootPublicationPlanError::CaseInsensitiveCollision {
                first_root: existing.root,
                second_root: request.root,
                first: existing.relative_path.clone(),
                second: relative_path,
            });
        }

        if let Some(existing_index) = existing_ancestor_index(&destinations, &folded, &mut budget)? {
            let existing = &outputs[existing_index];
            return Err(ActiveReblitBootPublicationPlanError::PublicationHierarchyCollision {
                ancestor_root: existing.root,
                descendant_root: request.root,
                ancestor: existing.relative_path.clone(),
                descendant: relative_path,
            });
        }
        if let Some(existing_index) = existing_descendant_index(&destinations, &folded, &mut budget)? {
            let existing = &outputs[existing_index];
            return Err(ActiveReblitBootPublicationPlanError::PublicationHierarchyCollision {
                ancestor_root: request.root,
                descendant_root: existing.root,
                ancestor: relative_path,
                descendant: existing.relative_path.clone(),
            });
        }

        destinations.insert(folded.clone(), outputs.len());
        outputs.push(PlannedActiveReblitBootPublication {
            role: request.role,
            root: request.root,
            phase: request.phase,
            relative_path,
            folded_relative_path: folded,
            source,
        });
    }

    budget.step()?;
    budget.reserve_sort_work(outputs.len())?;
    outputs.sort_by(|left, right| {
        (
            left.phase,
            left.root,
            left.folded_relative_path.as_str(),
            &left.relative_path,
        )
            .cmp(&(
                right.phase,
                right.root,
                right.folded_relative_path.as_str(),
                &right.relative_path,
            ))
    });
    after_sort();
    budget.require_deadline()?;

    let mut logical_bytes = 0u64;
    let mut generated_bytes = 0usize;
    let mut canonical_path_bytes = 0usize;
    for output in &outputs {
        budget.step()?;
        canonical_path_bytes = canonical_path_bytes
            .checked_add(output.relative_path.as_os_str().as_bytes().len())
            .expect("canonical path bytes are bounded by the admission budget");
        logical_bytes = logical_bytes.checked_add(output.source.length()).unwrap_or(u64::MAX);
        if logical_bytes > policy.max_logical_bytes {
            return Err(ActiveReblitBootPublicationPlanError::LogicalByteLimit {
                limit: policy.max_logical_bytes,
                actual: logical_bytes,
            });
        }
        if let Some(bytes) = output.source.generated_bytes() {
            generated_bytes = generated_bytes.checked_add(bytes.len()).unwrap_or(usize::MAX);
        }
    }

    let prepared = PreparedActiveReblitBootPublicationPlan {
        outputs,
        collision_domains,
        logical_bytes,
        generated_bytes,
        path_bytes: canonical_path_bytes,
        planning_work: budget.work,
    };
    after_plan_materialized();
    budget.require_deadline()?;
    Ok(prepared)
}

fn existing_ancestor_index(
    destinations: &BTreeMap<String, usize>,
    folded: &str,
    budget: &mut PublicationPlanBudget,
) -> Result<Option<usize>, ActiveReblitBootPublicationPlanError> {
    for (index, byte) in folded.bytes().enumerate() {
        if byte != b'/' {
            continue;
        }
        budget.step()?;
        if let Some(existing) = destinations.get(&folded[..index]).copied() {
            return Ok(Some(existing));
        }
    }
    Ok(None)
}

fn existing_descendant_index(
    destinations: &BTreeMap<String, usize>,
    folded: &str,
    budget: &mut PublicationPlanBudget,
) -> Result<Option<usize>, ActiveReblitBootPublicationPlanError> {
    budget.step()?;
    let mut prefix = String::with_capacity(folded.len().saturating_add(1));
    prefix.push_str(folded);
    prefix.push('/');
    Ok(destinations
        .range(prefix.clone()..)
        .next()
        .filter(|(candidate, _)| candidate.starts_with(&prefix))
        .map(|(_, index)| *index))
}

fn require_normalized_relative_path(
    path: &Path,
    budget: &mut PublicationPlanBudget,
) -> Result<PathBuf, ActiveReblitBootPublicationPlanError> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return Err(ActiveReblitBootPublicationPlanError::EmptyPath);
    }
    if bytes.contains(&0) {
        return Err(ActiveReblitBootPublicationPlanError::NulPath { path: path.to_owned() });
    }
    let Some(text) = path.to_str() else {
        return Err(ActiveReblitBootPublicationPlanError::NonUtf8Path { path: path.to_owned() });
    };
    if path.is_absolute() || text.starts_with('/') {
        return Err(ActiveReblitBootPublicationPlanError::AbsolutePath { path: path.to_owned() });
    }
    if bytes.len() > budget.policy.max_single_path_bytes {
        return Err(ActiveReblitBootPublicationPlanError::SinglePathByteLimit {
            path: path.to_owned(),
            limit: budget.policy.max_single_path_bytes,
            actual: bytes.len(),
        });
    }

    let mut component_count = 0usize;
    for component in text.split('/') {
        budget.step()?;
        component_count = component_count.saturating_add(1);
        if component_count > budget.policy.max_components {
            return Err(ActiveReblitBootPublicationPlanError::PathComponentLimit {
                path: path.to_owned(),
                limit: budget.policy.max_components,
                actual: component_count,
            });
        }
        if component.is_empty() {
            return Err(ActiveReblitBootPublicationPlanError::EmptyPathComponent { path: path.to_owned() });
        }
        if component == "." {
            return Err(ActiveReblitBootPublicationPlanError::DotPathComponent { path: path.to_owned() });
        }
        if component == ".." {
            return Err(ActiveReblitBootPublicationPlanError::ParentPathComponent { path: path.to_owned() });
        }
        if component.chars().any(char::is_control) {
            return Err(ActiveReblitBootPublicationPlanError::ControlPathComponent { path: path.to_owned() });
        }
        if !component.is_ascii() {
            return Err(ActiveReblitBootPublicationPlanError::NonAsciiPathComponent { path: path.to_owned() });
        }
        if component.len() > MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES {
            return Err(ActiveReblitBootPublicationPlanError::FatComponentByteLimit {
                path: path.to_owned(),
                limit: MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES,
                actual: component.len(),
            });
        }
        if let Some(character) = component
            .chars()
            .find(|character| matches!(character, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
        {
            return Err(ActiveReblitBootPublicationPlanError::FatForbiddenCharacter {
                path: path.to_owned(),
                character,
            });
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(ActiveReblitBootPublicationPlanError::FatTrailingDotOrSpace { path: path.to_owned() });
        }
        if component.contains('~') {
            return Err(ActiveReblitBootPublicationPlanError::FatShortNameMarker { path: path.to_owned() });
        }
        if is_dos_reserved_component(component) {
            return Err(ActiveReblitBootPublicationPlanError::FatReservedName {
                path: path.to_owned(),
                component: component.to_owned(),
            });
        }
    }

    Ok(PathBuf::from(text))
}

fn is_dos_reserved_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4 && (&bytes[..3] == b"COM" || &bytes[..3] == b"LPT") && matches!(bytes[3], b'1'..=b'9')
}

fn prepare_source(
    path: &Path,
    source: ActiveReblitBootPublicationRequestSource,
    budget: &mut PublicationPlanBudget,
) -> Result<ActiveReblitBootPublicationSource, ActiveReblitBootPublicationPlanError> {
    match source {
        ActiveReblitBootPublicationRequestSource::SealedSnapshot {
            binding_index,
            digest,
            length,
        } => {
            budget.step()?;
            if length > budget.policy.max_sealed_file_bytes {
                return Err(ActiveReblitBootPublicationPlanError::SealedSnapshotFileByteLimit {
                    path: path.to_owned(),
                    limit: budget.policy.max_sealed_file_bytes,
                    actual: length,
                });
            }
            Ok(ActiveReblitBootPublicationSource::SealedSnapshot {
                binding_index,
                digest,
                length,
            })
        }
        ActiveReblitBootPublicationRequestSource::Generated { bytes } => {
            budget.admit_generated(path, bytes.len())?;
            let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
            // Hashing is bounded by `admit_generated`; this post-hash step also
            // enforces the wall-clock deadline before the source is admitted.
            budget.step()?;
            Ok(ActiveReblitBootPublicationSource::Generated { bytes, digest })
        }
    }
}

fn exact_duplicate(
    existing: &PlannedActiveReblitBootPublication,
    role: ActiveReblitBootPublicationRole,
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    relative_path: &Path,
    source: &ActiveReblitBootPublicationSource,
) -> bool {
    existing.role == role
        && existing.root == root
        && existing.phase == phase
        && existing.relative_path == relative_path
        && &existing.source == source
}

fn case_folded_path(path: &Path) -> String {
    path.to_str()
        .expect("validated publication paths are UTF-8")
        .to_ascii_lowercase()
}

#[cfg(test)]
#[path = "active_reblit_publication_plan_tests.rs"]
mod tests;
