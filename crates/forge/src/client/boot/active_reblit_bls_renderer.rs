//! Pure, deterministic BLS rendering for one authenticated ActiveReblit attempt.
//!
//! Rendering consumes only the already-revalidated semantic aggregate and the
//! value view of an already-revalidated mounted topology. It neither reads a
//! source descriptor nor receives any destination or mutation authority.

use std::{collections::TryReserveError, path::PathBuf, time::Instant};

#[cfg(test)]
use super::active_reblit_mounted_boot_topology::BoundActiveReblitMountedBootTopology;
use super::{
    active_reblit_boot_inputs::BoundActiveReblitBootAsset,
    active_reblit_boot_render_inputs::RevalidatedActiveReblitBootRenderInputs,
    active_reblit_boot_schema_inputs::ValidatedActiveReblitBootSchema,
    active_reblit_mounted_boot_topology::RevalidatedActiveReblitMountedBootTopology,
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
        ActiveReblitBootPublicationRequest, ActiveReblitBootPublicationRole, ActiveReblitBootPublicationSource,
        PlannedActiveReblitBootPublication, PreparedActiveReblitBootPublicationPlan,
    },
};

#[path = "active_reblit_bls_renderer/document.rs"]
mod document;
#[path = "active_reblit_bls_renderer/error.rs"]
mod error;
#[path = "active_reblit_bls_renderer/paths.rs"]
mod paths;
#[path = "active_reblit_bls_renderer/payload_catalog.rs"]
mod payload_catalog;

pub(in crate::client) use error::{
    ActiveReblitBlsComponentKind, ActiveReblitBlsComponentReason, ActiveReblitBlsRendererError,
};
use payload_catalog::{PayloadCandidate, RetainedSealedSource, SealedSourceCatalog};

const MAX_BLS_REQUESTS: usize = 8_322;
const MAX_BLS_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_BLS_GENERATED_FILE_BYTES: usize = 1024 * 1024;
const MAX_BLS_GENERATED_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_BLS_INITRDS_PER_KERNEL: usize = 8_192;
const MAX_BLS_WORK: usize = 1_000_000;
const SORT_WORK_PER_ELEMENT_LEVEL: usize = 4;

const BLS_POLICY: BlsRenderPolicy = BlsRenderPolicy {
    max_requests: MAX_BLS_REQUESTS,
    max_path_bytes: MAX_BLS_PATH_BYTES,
    max_generated_file_bytes: MAX_BLS_GENERATED_FILE_BYTES,
    max_generated_total_bytes: MAX_BLS_GENERATED_TOTAL_BYTES,
    max_initrds_per_kernel: MAX_BLS_INITRDS_PER_KERNEL,
    max_work: MAX_BLS_WORK,
};

/// Rendered requests that still retain the exact authenticated input attempt.
/// This value is deliberately non-`Clone` and exposes no request detachment.
pub(in crate::client) struct RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots> {
    inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    deadline: Instant,
    requests: Vec<ActiveReblitBootPublicationRequest>,
    sealed_sources: SealedSourceCatalog<'input>,
    path_bytes: usize,
    generated_bytes: usize,
    render_work: usize,
}

/// A topology-planned output which cannot outlive either revalidated view.
/// The inner publication plan is never detachable by value.
pub(in crate::client) struct BoundActiveReblitBlsPublicationPlan<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
> {
    inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    topology: &'topology_view RevalidatedActiveReblitMountedBootTopology<'topology_authority>,
    plan: PreparedActiveReblitBootPublicationPlan,
    sealed_sources: SealedSourceCatalog<'input>,
    render_path_bytes: usize,
    render_generated_bytes: usize,
    render_work: usize,
}

/// One output borrowed from the lifetime-bound plan.
pub(in crate::client) struct BoundActiveReblitBlsPublication<'plan, 'asset> {
    planned: &'plan PlannedActiveReblitBootPublication,
    sealed_sources: &'plan SealedSourceCatalog<'asset>,
}

struct RenderedInitrdCandidate<'asset> {
    path: PathBuf,
    basename: Box<str>,
    binding_index: u16,
    digest: u128,
    length: u64,
    asset: BoundActiveReblitBootAsset<'asset>,
}

#[derive(Clone, Copy)]
struct BlsRenderPolicy {
    max_requests: usize,
    max_path_bytes: usize,
    max_generated_file_bytes: usize,
    max_generated_total_bytes: usize,
    max_initrds_per_kernel: usize,
    max_work: usize,
}

struct RenderBudget {
    policy: BlsRenderPolicy,
    deadline: Instant,
    work: usize,
    request_count: usize,
    path_bytes: usize,
    generated_bytes: usize,
}

impl<'input, 'attempt, 'stone, 'roots> RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots> {
    pub(in crate::client) fn render(
        inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    ) -> Result<Self, ActiveReblitBlsRendererError> {
        render_with_policy_until(inputs, BLS_POLICY, inputs.deadline(), Instant::now)
    }

    /// Consume requests into the existing topology-aware plan without minting
    /// a new deadline or releasing either revalidated owner.
    pub(in crate::client) fn into_publication_plan<'topology_view, 'topology_authority>(
        self,
        topology: &'topology_view RevalidatedActiveReblitMountedBootTopology<'topology_authority>,
    ) -> Result<
        BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>,
        ActiveReblitBlsRendererError,
    > {
        let topology_deadline = topology.deadline();
        require_matching_deadlines(self.deadline, topology_deadline)?;
        require_deadline(self.deadline, "publication planning entry", Instant::now())?;
        let plan =
            PreparedActiveReblitBootPublicationPlan::prepare_until(self.requests, topology.topology(), self.deadline)?;
        let bound = BoundActiveReblitBlsPublicationPlan {
            inputs: self.inputs,
            topology,
            plan,
            sealed_sources: self.sealed_sources,
            render_path_bytes: self.path_bytes,
            render_generated_bytes: self.generated_bytes,
            render_work: self.render_work,
        };
        require_deadline(self.deadline, "terminal bound publication plan", Instant::now())?;
        Ok(bound)
    }

    #[cfg(test)]
    fn into_fixture_publication_plan<N>(
        self,
        topology: BoundActiveReblitMountedBootTopology<'_>,
        topology_deadline: Instant,
        terminal_now: N,
    ) -> Result<(PreparedActiveReblitBootPublicationPlan, SealedSourceCatalog<'input>), ActiveReblitBlsRendererError>
    where
        N: FnOnce() -> Instant,
    {
        require_matching_deadlines(self.deadline, topology_deadline)?;
        require_deadline(self.deadline, "fixture publication planning entry", Instant::now())?;
        let plan = PreparedActiveReblitBootPublicationPlan::prepare_until(self.requests, topology, self.deadline)?;
        require_deadline(self.deadline, "terminal fixture publication plan", terminal_now())?;
        Ok((plan, self.sealed_sources))
    }
}

impl<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>
    BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    pub(in crate::client) fn outputs<'plan>(
        &'plan self,
    ) -> impl ExactSizeIterator<Item = BoundActiveReblitBlsPublication<'plan, 'input>> + 'plan
    where
        'input: 'plan,
    {
        self.plan
            .outputs()
            .iter()
            .map(move |planned| BoundActiveReblitBlsPublication {
                planned,
                sealed_sources: &self.sealed_sources,
            })
    }

    pub(in crate::client) fn collision_domains_still_match(&self) -> bool {
        self.plan.collision_domains_match(self.topology.topology())
    }

    pub(in crate::client) fn input_deadline(&self) -> Instant {
        self.inputs.deadline()
    }

    pub(in crate::client) fn logical_bytes(&self) -> u64 {
        self.plan.logical_bytes()
    }

    pub(in crate::client) fn publication_count(&self) -> usize {
        self.plan.outputs().len()
    }

    pub(in crate::client) fn publication_path_bytes(&self) -> usize {
        self.plan.path_bytes()
    }

    pub(in crate::client) fn publication_generated_bytes(&self) -> usize {
        self.plan.generated_bytes()
    }

    pub(in crate::client) fn render_path_bytes(&self) -> usize {
        self.render_path_bytes
    }

    pub(in crate::client) fn render_generated_bytes(&self) -> usize {
        self.render_generated_bytes
    }

    pub(in crate::client) fn render_work(&self) -> usize {
        self.render_work
    }

    pub(in crate::client) fn destination_layout(&self) -> ActiveReblitBootDestinationLayout {
        self.plan.destination_layout()
    }
}

impl<'plan, 'asset> BoundActiveReblitBlsPublication<'plan, 'asset>
where
    'asset: 'plan,
{
    pub(in crate::client) fn role(&self) -> ActiveReblitBootPublicationRole {
        self.planned.role()
    }

    pub(in crate::client) fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.planned.root()
    }

    pub(in crate::client) fn phase(&self) -> ActiveReblitBootPublicationPhase {
        self.planned.phase()
    }

    pub(in crate::client) fn relative_path(&self) -> &'plan std::path::Path {
        self.planned.relative_path()
    }

    pub(in crate::client) fn generated_bytes(&self) -> Option<&'plan [u8]> {
        self.planned.source().generated_bytes()
    }

    pub(in crate::client) fn expected_digest(&self) -> u128 {
        self.planned.source().digest()
    }

    pub(in crate::client) fn expected_length(&self) -> u64 {
        self.planned.source().length()
    }

    pub(in crate::client) fn sealed_coordinate(
        &self,
    ) -> Result<Option<(u16, u128, u64)>, ActiveReblitBlsRendererError> {
        match self.planned.source() {
            ActiveReblitBootPublicationSource::Generated { .. } => Ok(None),
            ActiveReblitBootPublicationSource::SealedSnapshot {
                binding_index,
                digest,
                length,
            } if self.sealed_sources.contains_publication_source(self.planned.source()) => {
                Ok(Some((*binding_index, *digest, *length)))
            }
            ActiveReblitBootPublicationSource::SealedSnapshot { .. } => {
                Err(ActiveReblitBlsRendererError::MissingSealedSource)
            }
        }
    }

    /// Rebind a sealed output to the exact aggregate-returned asset view. A
    /// coordinate in the private catalog is never treated as authority.
    pub(in crate::client) fn sealed_asset(
        &self,
    ) -> Result<Option<&'plan BoundActiveReblitBootAsset<'asset>>, ActiveReblitBlsRendererError> {
        match self.planned.source() {
            ActiveReblitBootPublicationSource::Generated { .. } => Ok(None),
            source @ ActiveReblitBootPublicationSource::SealedSnapshot { .. } => self
                .sealed_sources
                .asset_for_publication_source(source)
                .map(Some)
                .ok_or(ActiveReblitBlsRendererError::MissingSealedSource),
        }
    }
}

fn render_with_policy_until<'input, 'attempt, 'stone, 'roots, N>(
    inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    policy: BlsRenderPolicy,
    deadline: Instant,
    terminal_now: N,
) -> Result<RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots>, ActiveReblitBlsRendererError>
where
    N: FnOnce() -> Instant,
{
    render_with_policy_and_clocks(inputs, policy, deadline, Instant::now, terminal_now)
}

#[cfg(test)]
fn render_with_policy_and_checkpoints<'input, 'attempt, 'stone, 'roots, P, N>(
    inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    policy: BlsRenderPolicy,
    deadline: Instant,
    post_payload_sort_now: P,
    terminal_now: N,
) -> Result<RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots>, ActiveReblitBlsRendererError>
where
    P: FnOnce() -> Instant,
    N: FnOnce() -> Instant,
{
    render_with_policy_and_clocks(inputs, policy, deadline, post_payload_sort_now, terminal_now)
}

fn render_with_policy_and_clocks<'input, 'attempt, 'stone, 'roots, P, N>(
    inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>,
    policy: BlsRenderPolicy,
    deadline: Instant,
    post_payload_sort_now: P,
    terminal_now: N,
) -> Result<RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots>, ActiveReblitBlsRendererError>
where
    P: FnOnce() -> Instant,
    N: FnOnce() -> Instant,
{
    if deadline != inputs.deadline() {
        return Err(ActiveReblitBlsRendererError::DeadlineMismatch {
            inputs: inputs.deadline(),
            topology: deadline,
        });
    }
    let mut budget = RenderBudget::new(policy, deadline)?;
    let global_schema = inputs.global_schema();
    paths::require_component(global_schema.namespace(), ActiveReblitBlsComponentKind::Namespace)?;

    // Count all semantic candidates before allocating candidate collections or
    // any generated document. This repeats the inherited bounds deliberately:
    // the renderer never relies on a prior layer to make its own allocation
    // budget finite.
    let mut total_initrds = 0usize;
    for kernel in inputs.kernels() {
        budget.step()?;
        let count = kernel.initrds().len();
        if count > policy.max_initrds_per_kernel {
            return Err(ActiveReblitBlsRendererError::RequestCountLimit {
                limit: policy.max_initrds_per_kernel,
                actual: count,
            });
        }
        total_initrds = total_initrds.checked_add(count).unwrap_or(usize::MAX);
    }
    let candidate_requests = inputs
        .kernel_count()
        .checked_mul(2)
        .and_then(|count| count.checked_add(total_initrds))
        .and_then(|count| count.checked_add(3))
        .unwrap_or(usize::MAX);
    budget.admit_requests(candidate_requests)?;

    let loader_bytes = document::render_loader_control(global_schema.namespace(), &mut budget)?;
    let _loader_path = paths::fixed_path("loader/loader.conf", &mut budget)?;

    let mut entries = Vec::new();
    entries
        .try_reserve_exact(inputs.kernel_count())
        .map_err(|source| allocation("BLS entry requests", source))?;
    let mut payload_candidates = Vec::new();
    payload_candidates
        .try_reserve_exact(inputs.kernel_count().saturating_add(total_initrds))
        .map_err(|source| allocation("BLS payload candidates", source))?;

    for kernel in inputs.kernels() {
        budget.step()?;
        let schema = kernel.schema();
        validate_schema_components(schema)?;
        paths::require_component(kernel.version(), ActiveReblitBlsComponentKind::KernelVersion)?;
        let kernel_binding_index = kernel.kernel_binding_index();
        let kernel_digest = kernel.kernel_digest();
        let kernel_length = kernel.kernel_length();
        let entry_path = paths::entry_path(
            schema.os_id(),
            kernel.version(),
            i32::from(kernel.state_id()),
            &mut budget,
        )?;
        let kernel_path = paths::payload_path(
            schema.namespace(),
            kernel_digest,
            kernel_length,
            "vmlinuz",
            ActiveReblitBlsComponentKind::InitrdBasename,
            &mut budget,
        )?;

        let initrd_count = kernel.initrds().len();
        if initrd_count > policy.max_initrds_per_kernel {
            return Err(ActiveReblitBlsRendererError::RequestCountLimit {
                limit: policy.max_initrds_per_kernel,
                actual: initrd_count,
            });
        }
        let mut initrds = Vec::new();
        initrds
            .try_reserve_exact(initrd_count)
            .map_err(|source| allocation("per-kernel BLS initrds", source))?;
        for initrd in kernel.initrds() {
            budget.step()?;
            let binding_index = initrd.binding_index();
            let digest = initrd.digest();
            let length = initrd.length();
            let basename =
                initrd
                    .logical_basename()
                    .to_str()
                    .ok_or(ActiveReblitBlsRendererError::InvalidComponent {
                        kind: ActiveReblitBlsComponentKind::InitrdBasename,
                        reason: ActiveReblitBlsComponentReason::NonAscii,
                    })?;
            paths::require_component(basename, ActiveReblitBlsComponentKind::InitrdBasename)?;
            let path = paths::payload_path(
                schema.namespace(),
                digest,
                length,
                basename,
                ActiveReblitBlsComponentKind::InitrdBasename,
                &mut budget,
            )?;
            initrds.push(RenderedInitrdCandidate {
                path,
                basename: clone_boxed(basename, "BLS initrd basename")?,
                binding_index,
                digest,
                length,
                asset: initrd.asset(),
            });
        }
        budget.reserve_sort_work(initrds.len())?;
        initrds.sort_unstable_by(|left, right| {
            payload_catalog::ascii_fold_cmp(left.basename.as_bytes(), right.basename.as_bytes())
                .then_with(|| left.basename.cmp(&right.basename))
                .then_with(|| left.binding_index.cmp(&right.binding_index))
        });
        reject_initrd_case_aliases(&initrds)?;
        budget.require_deadline("initrd sort completion")?;

        let entry_bytes = document::render_entry(
            &entry_path,
            schema.display_name(),
            kernel.version(),
            &kernel_path,
            &initrds,
            kernel.cmdline(),
            &mut budget,
        )?;
        entries.push(ActiveReblitBootPublicationRequest::generated_entry(
            entry_path,
            entry_bytes,
        ));
        payload_candidates.push(PayloadCandidate {
            path: kernel_path,
            binding_index: kernel_binding_index,
            digest: kernel_digest,
            length: kernel_length,
            asset: Some(kernel.kernel_asset()),
        });
        for initrd in initrds {
            payload_candidates.push(PayloadCandidate {
                path: initrd.path,
                binding_index: initrd.binding_index,
                digest: initrd.digest,
                length: initrd.length,
                asset: Some(initrd.asset),
            });
        }
    }

    debug_assert_eq!(entries.len() + payload_candidates.len() + 3, candidate_requests);
    let mut payloads = payload_catalog::canonicalize_payloads(payload_candidates, &mut budget, post_payload_sort_now)?;
    let systemd_boot = RetainedSealedSource::new(
        inputs.systemd_boot_binding_index(),
        inputs.systemd_boot_digest(),
        inputs.systemd_boot_length(),
        inputs.systemd_boot_asset(),
    );
    let sealed_sources = SealedSourceCatalog::prepare(systemd_boot, &mut payloads, &mut budget)?;

    let request_capacity = entries
        .len()
        .checked_add(payloads.len())
        .and_then(|count| count.checked_add(3))
        .unwrap_or(usize::MAX);
    let mut requests = Vec::new();
    requests
        .try_reserve_exact(request_capacity)
        .map_err(|source| allocation("canonical BLS publication requests", source))?;
    for payload in payloads {
        requests.push(ActiveReblitBootPublicationRequest::sealed_payload(
            payload.path,
            payload.binding_index,
            payload.digest,
            payload.length,
        ));
    }
    requests.append(&mut entries);
    requests.push(ActiveReblitBootPublicationRequest::generated_loader_control(
        loader_bytes,
    ));
    let _fallback = paths::fixed_path("EFI/Boot/BOOTX64.EFI", &mut budget)?;
    requests.push(ActiveReblitBootPublicationRequest::sealed_fallback_bootloader(
        inputs.systemd_boot_binding_index(),
        inputs.systemd_boot_digest(),
        inputs.systemd_boot_length(),
    ));
    let _systemd = paths::fixed_path("EFI/systemd/systemd-bootx64.efi", &mut budget)?;
    requests.push(ActiveReblitBootPublicationRequest::sealed_systemd_bootloader(
        inputs.systemd_boot_binding_index(),
        inputs.systemd_boot_digest(),
        inputs.systemd_boot_length(),
    ));
    debug_assert_eq!(requests.len(), request_capacity);

    let rendered = RenderedActiveReblitBlsRequests {
        inputs,
        deadline,
        requests,
        sealed_sources,
        path_bytes: budget.path_bytes,
        generated_bytes: budget.generated_bytes,
        render_work: budget.work,
    };
    require_deadline(deadline, "terminal rendered BLS requests", terminal_now())?;
    Ok(rendered)
}

impl RenderBudget {
    fn new(policy: BlsRenderPolicy, deadline: Instant) -> Result<Self, ActiveReblitBlsRendererError> {
        let budget = Self {
            policy,
            deadline,
            work: 0,
            request_count: 0,
            path_bytes: 0,
            generated_bytes: 0,
        };
        budget.require_deadline("renderer entry")?;
        Ok(budget)
    }

    fn step(&mut self) -> Result<(), ActiveReblitBlsRendererError> {
        self.reserve_work(1)
    }

    fn reserve_sort_work(&mut self, count: usize) -> Result<(), ActiveReblitBlsRendererError> {
        self.reserve_work(conservative_sort_work(count))
    }

    fn reserve_work(&mut self, amount: usize) -> Result<(), ActiveReblitBlsRendererError> {
        self.require_deadline("bounded renderer work")?;
        let actual = self.work.checked_add(amount).unwrap_or(usize::MAX);
        if actual > self.policy.max_work {
            return Err(ActiveReblitBlsRendererError::WorkLimit {
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn admit_requests(&mut self, actual: usize) -> Result<(), ActiveReblitBlsRendererError> {
        self.require_deadline("request count admission")?;
        if actual > self.policy.max_requests {
            return Err(ActiveReblitBlsRendererError::RequestCountLimit {
                limit: self.policy.max_requests,
                actual,
            });
        }
        self.request_count = actual;
        Ok(())
    }

    fn admit_path(&mut self, length: usize) -> Result<(), ActiveReblitBlsRendererError> {
        self.step()?;
        let actual = self.path_bytes.checked_add(length).unwrap_or(usize::MAX);
        if actual > self.policy.max_path_bytes {
            return Err(ActiveReblitBlsRendererError::PathByteLimit {
                limit: self.policy.max_path_bytes,
                actual,
            });
        }
        self.path_bytes = actual;
        Ok(())
    }

    fn admit_generated(&mut self, path: &std::path::Path, length: usize) -> Result<(), ActiveReblitBlsRendererError> {
        self.step()?;
        if length > self.policy.max_generated_file_bytes {
            return Err(ActiveReblitBlsRendererError::GeneratedFileByteLimit {
                path: path.to_owned(),
                limit: self.policy.max_generated_file_bytes,
                actual: length,
            });
        }
        let actual = self.generated_bytes.checked_add(length).unwrap_or(usize::MAX);
        if actual > self.policy.max_generated_total_bytes {
            return Err(ActiveReblitBlsRendererError::GeneratedTotalByteLimit {
                limit: self.policy.max_generated_total_bytes,
                actual,
            });
        }
        self.generated_bytes = actual;
        Ok(())
    }

    fn require_deadline(&self, checkpoint: &'static str) -> Result<(), ActiveReblitBlsRendererError> {
        require_deadline(self.deadline, checkpoint, Instant::now())
    }

    fn require_deadline_at(&self, checkpoint: &'static str, now: Instant) -> Result<(), ActiveReblitBlsRendererError> {
        require_deadline(self.deadline, checkpoint, now)
    }
}

fn validate_schema_components(schema: &ValidatedActiveReblitBootSchema) -> Result<(), ActiveReblitBlsRendererError> {
    paths::require_component(schema.namespace(), ActiveReblitBlsComponentKind::Namespace)?;
    paths::require_component(schema.os_id(), ActiveReblitBlsComponentKind::OsId)
}

fn reject_initrd_case_aliases(initrds: &[RenderedInitrdCandidate<'_>]) -> Result<(), ActiveReblitBlsRendererError> {
    for pair in initrds.windows(2) {
        if pair[0].basename.eq_ignore_ascii_case(&pair[1].basename) {
            return Err(ActiveReblitBlsRendererError::InitrdCaseCollision {
                first: pair[0].basename.clone(),
                second: pair[1].basename.clone(),
            });
        }
    }
    Ok(())
}

fn conservative_sort_work(count: usize) -> usize {
    if count < 2 {
        return 0;
    }
    let levels = usize::BITS as usize - (count - 1).leading_zeros() as usize;
    count
        .checked_mul(levels)
        .and_then(|work| work.checked_mul(SORT_WORK_PER_ELEMENT_LEVEL))
        .unwrap_or(usize::MAX)
}

fn clone_boxed(value: &str, resource: &'static str) -> Result<Box<str>, ActiveReblitBlsRendererError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|source| allocation(resource, source))?;
    cloned.push_str(value);
    Ok(cloned.into_boxed_str())
}

fn require_matching_deadlines(inputs: Instant, topology: Instant) -> Result<(), ActiveReblitBlsRendererError> {
    if inputs == topology {
        Ok(())
    } else {
        Err(ActiveReblitBlsRendererError::DeadlineMismatch { inputs, topology })
    }
}

fn require_deadline(
    deadline: Instant,
    checkpoint: &'static str,
    now: Instant,
) -> Result<(), ActiveReblitBlsRendererError> {
    if now > deadline {
        Err(ActiveReblitBlsRendererError::DeadlineExceeded { checkpoint })
    } else {
        Ok(())
    }
}

fn allocation(resource: &'static str, source: TryReserveError) -> ActiveReblitBlsRendererError {
    ActiveReblitBlsRendererError::Allocation { resource, source }
}

#[cfg(test)]
#[path = "active_reblit_bls_renderer_tests.rs"]
mod tests;
