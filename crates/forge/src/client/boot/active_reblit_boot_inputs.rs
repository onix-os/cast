//! Coherent, immutable Stone inputs for one ActiveReblit boot repair attempt.
//!
//! Only package-owned bytes consumed by the pinned publisher are bound here.
//! A `GeneratedOsRelease` schema requirement is an explicit handoff to the
//! future retained state-root-authority layer and is never bound as Stone/CAS
//! data here. Kernel `boot.json`, `config`, and `System.map` are not snapshotted
//! because the pinned publisher does not consume them.

use std::{
    os::fd::BorrowedFd,
    path::Path,
    time::{Duration, Instant},
};

use crate::{Installation, State, db, state};

use super::{
    active_reblit_boot_projection::{
        ActiveReblitBootAssetPlanError, ActiveReblitBootProjectionError, BootAssetPlanNotApplicable,
        BootAssetPlanOutcome, BootAssetRole, MAX_BOOT_PLAN_ASSETS, MAX_BOOT_PLAN_SNAPSHOT_DIGESTS, PlannedBootAsset,
        PlannedBootSchemaRequirement, PreparedActiveReblitBootAssetPlan, PreparedActiveReblitBootProjection,
    },
    boot_asset_snapshots::{BootAssetSnapshotError, PreparedBootAssetSnapshots, SealedBootAssetSnapshot},
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const MAX_BINDING_WORK: usize = MAX_BOOT_PLAN_ASSETS + 2 * MAX_BOOT_PLAN_SNAPSHOT_DIGESTS;
const BINDING_TIMEOUT: Duration = Duration::from_secs(30);

const STONE_BOOT_INPUT_POLICY: StoneBootInputPolicy = StoneBootInputPolicy {
    max_systemd_boot_bytes: 64 * MIB,
    max_os_info_bytes: MIB,
    max_global_cmdline_bytes: 64 * KIB,
    max_kernel_bytes: 512 * MIB,
    max_initrd_bytes: 512 * MIB,
    max_kernel_cmdline_bytes: 64 * KIB,
    max_control_input_bytes: 16 * MIB,
    max_referenced_input_bytes: 10 * GIB,
    max_work: MAX_BINDING_WORK,
    timeout: BINDING_TIMEOUT,
};

/// One pre-claim input whose database evidence, Stone plan, and sealed CAS
/// descriptors were prepared and revalidated as a single coherent value.
///
/// This owner is intentionally not `Clone`. It is not itself permission to
/// publish boot state: a later descriptor-rooted output plan and one-attempt
/// claim must consume it. That claim must call [`Self::revalidate`] again as
/// its final fallible database operation.
pub(in crate::client) struct PreparedActiveReblitStoneBootInputs {
    projection: PreparedActiveReblitBootProjection,
    plan: PreparedActiveReblitBootAssetPlan,
    snapshots: PreparedBootAssetSnapshots,
    bindings: Vec<BootAssetBinding>,
    referenced_input_bytes: u64,
    control_input_bytes: u64,
    binding_work: usize,
}

#[derive(Clone, Copy)]
struct BootAssetBinding {
    snapshot_index: u16,
    length: u64,
}

pub(in crate::client) enum ActiveReblitStoneBootInputsOutcome {
    NotApplicable(BootAssetPlanNotApplicable),
    Ready(PreparedActiveReblitStoneBootInputs),
}

/// A borrow tying one declarative Stone role to its exact sealed CAS bytes.
pub(in crate::client) struct BoundActiveReblitBootAsset<'a> {
    planned: &'a PlannedBootAsset,
    snapshot: &'a SealedBootAssetSnapshot,
    length: u64,
}

impl PreparedActiveReblitStoneBootInputs {
    pub(in crate::client) fn prepare(
        installation: &Installation,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        expected_head: &State,
    ) -> Result<ActiveReblitStoneBootInputsOutcome, ActiveReblitStoneBootInputsError> {
        prepare_with_policy_and_checkpoint(
            installation,
            state_db,
            layout_db,
            expected_head,
            StoneBootInputPolicy::production(),
            |_| {},
        )
    }

    /// Repeat the exact bounded state-and-layout capture retained by this
    /// value. The eventual capability claim must invoke this immediately
    /// before consuming mutable-system ownership.
    pub(in crate::client) fn revalidate(
        &self,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
    ) -> Result<(), ActiveReblitStoneBootInputsError> {
        self.projection
            .revalidate(state_db, layout_db)
            .map_err(ActiveReblitStoneBootInputsError::RevalidateProjection)
    }

    pub(in crate::client) fn state_ids(&self) -> &[state::Id] {
        self.plan.state_ids()
    }

    pub(in crate::client) fn kernel_count(&self) -> usize {
        self.plan.kernel_count()
    }

    /// Ordered schema requirements from the exact Stone plan owned by this
    /// composite. Generated metadata is still resolved only beneath retained
    /// state-root descriptors; exposing these copy-only declarations does not
    /// expose either database or filesystem authority.
    pub(in crate::client) fn schema_requirements(&self) -> &[PlannedBootSchemaRequirement] {
        self.plan.schema_requirements()
    }

    /// Bind one stable plan coordinate back to the sealed snapshot retained by
    /// this exact owner. Later generated publication records carry this index,
    /// digest and length together and must re-check all three before use.
    pub(in crate::client) fn asset_at(&self, index: usize) -> Option<BoundActiveReblitBootAsset<'_>> {
        let planned = self.plan.assets().get(index)?;
        let binding = self.bindings.get(index)?;
        let snapshot = self.snapshots.snapshot_at(usize::from(binding.snapshot_index))?;
        debug_assert_eq!(planned.digest(), snapshot.digest());
        Some(BoundActiveReblitBootAsset {
            planned,
            snapshot,
            length: binding.length,
        })
    }

    pub(in crate::client) fn assets(&self) -> impl ExactSizeIterator<Item = BoundActiveReblitBootAsset<'_>> {
        (0..self.plan.assets().len()).map(|index| {
            self.asset_at(index)
                .expect("validated asset and snapshot coordinates remain owned by the composite")
        })
    }

    pub(in crate::client) fn referenced_input_bytes(&self) -> u64 {
        self.referenced_input_bytes
    }

    pub(in crate::client) fn control_input_bytes(&self) -> u64 {
        self.control_input_bytes
    }

    pub(in crate::client) fn binding_work(&self) -> usize {
        self.binding_work
    }

    #[cfg(test)]
    fn plan(&self) -> &PreparedActiveReblitBootAssetPlan {
        &self.plan
    }
}

impl BoundActiveReblitBootAsset<'_> {
    pub(in crate::client) fn state_id(&self) -> state::Id {
        self.planned.state_id()
    }

    pub(in crate::client) fn logical_path(&self) -> &Path {
        self.planned.logical_path()
    }

    pub(in crate::client) fn resolved_path(&self) -> &Path {
        self.planned.resolved_path()
    }

    pub(in crate::client) fn digest(&self) -> u128 {
        self.planned.digest()
    }

    pub(in crate::client) fn role(&self) -> &BootAssetRole {
        self.planned.role()
    }

    pub(in crate::client) fn length(&self) -> u64 {
        self.length
    }

    /// Borrow the sealed input without transferring ownership. Consumers must
    /// use `pread`/`FileExt::*_at` (and later `pwrite`) with explicit offsets;
    /// descriptor duplication does not provide an independent cursor.
    pub(in crate::client) fn descriptor(&self) -> BorrowedFd<'_> {
        self.snapshot.descriptor()
    }
}

#[derive(Clone, Copy)]
struct StoneBootInputPolicy {
    max_systemd_boot_bytes: u64,
    max_os_info_bytes: u64,
    max_global_cmdline_bytes: u64,
    max_kernel_bytes: u64,
    max_initrd_bytes: u64,
    max_kernel_cmdline_bytes: u64,
    max_control_input_bytes: u64,
    max_referenced_input_bytes: u64,
    max_work: usize,
    timeout: Duration,
}

impl StoneBootInputPolicy {
    const fn production() -> Self {
        STONE_BOOT_INPUT_POLICY
    }

    fn role_limit(self, role: &BootAssetRole) -> u64 {
        match role {
            BootAssetRole::SystemdBoot => self.max_systemd_boot_bytes,
            BootAssetRole::OsInfo => self.max_os_info_bytes,
            BootAssetRole::GlobalCmdline => self.max_global_cmdline_bytes,
            BootAssetRole::Kernel { .. } => self.max_kernel_bytes,
            BootAssetRole::Initrd { .. } => self.max_initrd_bytes,
            BootAssetRole::KernelCmdline { .. } => self.max_kernel_cmdline_bytes,
        }
    }
}

struct BindingBudget {
    policy: StoneBootInputPolicy,
    deadline: Instant,
    work: usize,
}

impl BindingBudget {
    fn new(policy: StoneBootInputPolicy) -> Result<Self, ActiveReblitStoneBootInputsError> {
        let deadline = Instant::now().checked_add(policy.timeout).ok_or(
            ActiveReblitStoneBootInputsError::InvalidBindingDeadline {
                timeout: policy.timeout,
            },
        )?;
        Ok(Self {
            policy,
            deadline,
            work: 0,
        })
    }

    fn step(&mut self) -> Result<(), ActiveReblitStoneBootInputsError> {
        self.require_deadline()?;
        let actual = self.work.saturating_add(1);
        if actual > self.policy.max_work {
            return Err(ActiveReblitStoneBootInputsError::BindingWorkLimit {
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn require_deadline(&self) -> Result<(), ActiveReblitStoneBootInputsError> {
        if Instant::now() > self.deadline {
            Err(ActiveReblitStoneBootInputsError::BindingDeadlineExceeded {
                timeout: self.policy.timeout,
            })
        } else {
            Ok(())
        }
    }
}

fn prepare_with_policy_and_checkpoint<F>(
    installation: &Installation,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
    expected_head: &State,
    policy: StoneBootInputPolicy,
    before_revalidate: F,
) -> Result<ActiveReblitStoneBootInputsOutcome, ActiveReblitStoneBootInputsError>
where
    F: FnOnce(&PreparedBootAssetSnapshots),
{
    let projection = PreparedActiveReblitBootProjection::prepare(state_db, layout_db, expected_head.id)
        .map_err(ActiveReblitStoneBootInputsError::CaptureProjection)?;
    if projection.head() != expected_head {
        return Err(ActiveReblitStoneBootInputsError::ExpectedHeadMismatch {
            expected: i32::from(expected_head.id),
            actual: i32::from(projection.head().id),
        });
    }

    let plan = match projection
        .prepare_asset_plan()
        .map_err(ActiveReblitStoneBootInputsError::PlanAssets)?
    {
        BootAssetPlanOutcome::NotApplicable(reason) => {
            return Ok(ActiveReblitStoneBootInputsOutcome::NotApplicable(reason));
        }
        BootAssetPlanOutcome::Ready(plan) => plan,
    };
    let snapshots = PreparedBootAssetSnapshots::prepare(installation, &plan)
        .map_err(ActiveReblitStoneBootInputsError::SnapshotAssets)?;
    let (bindings, referenced_input_bytes, control_input_bytes, binding_work) =
        bind_snapshots(&plan, &snapshots, policy)?;
    before_revalidate(&snapshots);
    projection
        .revalidate(state_db, layout_db)
        .map_err(ActiveReblitStoneBootInputsError::RevalidateProjection)?;

    Ok(ActiveReblitStoneBootInputsOutcome::Ready(
        PreparedActiveReblitStoneBootInputs {
            projection,
            plan,
            snapshots,
            bindings,
            referenced_input_bytes,
            control_input_bytes,
            binding_work,
        },
    ))
}

fn bind_snapshots(
    plan: &PreparedActiveReblitBootAssetPlan,
    snapshots: &PreparedBootAssetSnapshots,
    policy: StoneBootInputPolicy,
) -> Result<(Vec<BootAssetBinding>, u64, u64, usize), ActiveReblitStoneBootInputsError> {
    let mut budget = BindingBudget::new(policy)?;

    for digest in plan.snapshot_digests() {
        budget.step()?;
        if snapshots.snapshot_index_for(*digest).is_none() {
            return Err(ActiveReblitStoneBootInputsError::MissingSnapshot { digest: *digest });
        }
    }
    for snapshot in snapshots.snapshots() {
        budget.step()?;
        if plan.snapshot_digests().binary_search(&snapshot.digest()).is_err() {
            return Err(ActiveReblitStoneBootInputsError::ExtraSnapshot {
                digest: snapshot.digest(),
            });
        }
    }

    let mut bindings = Vec::with_capacity(plan.assets().len());
    let mut referenced_input_bytes = 0u64;
    let mut control_input_bytes = 0u64;
    for asset in plan.assets() {
        budget.step()?;
        let snapshot_index = snapshots
            .snapshot_index_for(asset.digest())
            .ok_or(ActiveReblitStoneBootInputsError::MissingSnapshot { digest: asset.digest() })?;
        let snapshot = snapshots
            .snapshot_at(snapshot_index)
            .expect("digest lookup returns a live snapshot index");
        let length = snapshot.length();
        let role_limit = policy.role_limit(asset.role());
        if length > role_limit {
            return Err(ActiveReblitStoneBootInputsError::RoleByteLimit {
                state_id: i32::from(asset.state_id()),
                path: asset.logical_path().to_owned(),
                role: asset.role().clone(),
                limit: role_limit,
                actual: length,
            });
        }

        referenced_input_bytes = referenced_input_bytes.checked_add(length).unwrap_or(u64::MAX);
        if referenced_input_bytes > policy.max_referenced_input_bytes {
            return Err(ActiveReblitStoneBootInputsError::ReferencedInputByteLimit {
                limit: policy.max_referenced_input_bytes,
                actual: referenced_input_bytes,
            });
        }
        if role_is_control_input(asset.role()) {
            control_input_bytes = control_input_bytes.checked_add(length).unwrap_or(u64::MAX);
            if control_input_bytes > policy.max_control_input_bytes {
                return Err(ActiveReblitStoneBootInputsError::ControlInputByteLimit {
                    limit: policy.max_control_input_bytes,
                    actual: control_input_bytes,
                });
            }
        }

        let snapshot_index =
            u16::try_from(snapshot_index).map_err(|_| ActiveReblitStoneBootInputsError::SnapshotIndexLimit {
                limit: u16::MAX as usize,
                actual: snapshot_index,
            })?;
        bindings.push(BootAssetBinding { snapshot_index, length });
    }
    budget.require_deadline()?;
    Ok((bindings, referenced_input_bytes, control_input_bytes, budget.work))
}

fn role_is_control_input(role: &BootAssetRole) -> bool {
    matches!(
        role,
        BootAssetRole::OsInfo | BootAssetRole::GlobalCmdline | BootAssetRole::KernelCmdline { .. }
    )
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client) enum ActiveReblitStoneBootInputsError {
    #[error("capture bounded ActiveReblit boot database projection")]
    CaptureProjection(#[source] ActiveReblitBootProjectionError),
    #[error("captured ActiveReblit head row {actual} does not exactly match expected head row {expected}")]
    ExpectedHeadMismatch { expected: i32, actual: i32 },
    #[error("prepare pure Stone ActiveReblit boot asset plan")]
    PlanAssets(#[source] ActiveReblitBootAssetPlanError),
    #[error("seal authenticated CAS inputs for the ActiveReblit boot plan")]
    SnapshotAssets(#[source] BootAssetSnapshotError),
    #[error("prepared boot plan is missing sealed snapshot {digest:032x}")]
    MissingSnapshot { digest: u128 },
    #[error("sealed boot input contains undeclared snapshot {digest:032x}")]
    ExtraSnapshot { digest: u128 },
    #[error("sealed boot snapshot index {actual} exceeds {limit}")]
    SnapshotIndexLimit { limit: usize, actual: usize },
    #[error("state {state_id} boot input {path:?} for {role:?} exceeds {limit} bytes (got {actual})")]
    RoleByteLimit {
        state_id: i32,
        path: std::path::PathBuf,
        role: BootAssetRole,
        limit: u64,
        actual: u64,
    },
    #[error("parsed boot control inputs exceed {limit} bytes (got {actual})")]
    ControlInputByteLimit { limit: u64, actual: u64 },
    #[error("referenced boot input bytes exceed {limit} (got {actual})")]
    ReferencedInputByteLimit { limit: u64, actual: u64 },
    #[error("boot input binding exceeds {limit} bounded steps (got {actual})")]
    BindingWorkLimit { limit: usize, actual: usize },
    #[error("boot input binding deadline could not represent {timeout:?}")]
    InvalidBindingDeadline { timeout: Duration },
    #[error("boot input binding exceeded its {timeout:?} deadline")]
    BindingDeadlineExceeded { timeout: Duration },
    #[error("revalidate the coherent ActiveReblit boot database projection")]
    RevalidateProjection(#[source] ActiveReblitBootProjectionError),
}

#[cfg(test)]
#[path = "active_reblit_boot_inputs_tests.rs"]
mod tests;
