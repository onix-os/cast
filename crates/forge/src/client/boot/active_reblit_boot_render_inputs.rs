//! Pure semantic inputs for deterministic ActiveReblit boot rendering.
//!
//! The prepared layer binds static schemas, package fragments, and canonical
//! kernel coordinates to exact non-cloneable Stone and state-root owners.  The
//! revalidated layer is released only after every owner, machine-local policy,
//! and semantic command line has been revalidated beneath one caller-owned
//! absolute deadline.  Neither layer renders BLS bytes, discovers boot
//! destinations, or performs any storage operation.

use std::{
    collections::TryReserveError,
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    Installation, db, state,
    transition_identity::{PreparedActiveReblitBootStateRoots, RevalidatedActiveReblitBootStateRoots},
};

use super::{
    active_reblit_boot_inputs::{BoundActiveReblitBootAsset, PreparedActiveReblitStoneBootInputs},
    active_reblit_boot_schema_inputs::{
        ActiveReblitBootSchemaSourceBinding, PreparedActiveReblitBootSchemas, PreparedActiveReblitStateBootSchema,
        ValidatedActiveReblitBootSchema,
    },
    active_reblit_local_boot_policy::{PreparedActiveReblitLocalBootPolicy, RevalidatedActiveReblitLocalBootPolicy},
    active_reblit_package_cmdline_inputs::PreparedActiveReblitPackageCmdlineInputs,
    active_reblit_root_filesystem_intent::{
        PreparedActiveReblitRootFilesystemIntent, RevalidatedActiveReblitRootFilesystemIntent,
    },
};

#[path = "active_reblit_boot_render_inputs/cmdline.rs"]
mod cmdline;
#[path = "active_reblit_boot_render_inputs/error.rs"]
mod error;
#[path = "active_reblit_boot_render_inputs/kernel_join.rs"]
mod kernel_join;

pub(in crate::client) use error::{
    ActiveReblitBootRenderInputsError, ActiveReblitCmdlineSource, ActiveReblitCmdlineTokenReason,
};

const MAX_RENDER_KERNELS: usize = 128;
const MAX_RENDER_CMDLINE_BYTES: usize = 2_047;
const MAX_RENDER_CMDLINE_TOKENS: usize = 1_024;
const MAX_RENDER_TOTAL_CMDLINE_BYTES: usize = 262_016;
const MAX_RENDER_TOTAL_CMDLINE_TOKENS: usize = 131_072;

const RENDER_INPUT_POLICY: BootRenderInputPolicy = BootRenderInputPolicy {
    max_kernels: MAX_RENDER_KERNELS,
    max_cmdline_bytes: MAX_RENDER_CMDLINE_BYTES,
    max_cmdline_tokens: MAX_RENDER_CMDLINE_TOKENS,
    max_total_cmdline_bytes: MAX_RENDER_TOTAL_CMDLINE_BYTES,
    max_total_cmdline_tokens: MAX_RENDER_TOTAL_CMDLINE_TOKENS,
};

/// Static semantic children tied to exact non-cloneable source owners.
///
/// Callers cannot inject separately prepared package or schema children.  The
/// exact Stone owner and exact state-root owner are retained for the later
/// all-source revalidation boundary.
pub(in crate::client) struct PreparedActiveReblitBootRenderInputs<'stone, 'roots> {
    source_owner: &'stone PreparedActiveReblitStoneBootInputs,
    roots_owner: &'roots PreparedActiveReblitBootStateRoots,
    package_cmdlines: PreparedActiveReblitPackageCmdlineInputs<'stone>,
    schemas: PreparedActiveReblitBootSchemas,
    systemd_boot: RetainedBootAssetCoordinate,
    kernels: Box<[PreparedActiveReblitKernelRenderInput]>,
}

/// Fresh same-thread attempt view retaining every revalidated authority while
/// exposing only owned semantic command lines and exact input coordinates.
pub(in crate::client) struct RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots> {
    prepared: &'attempt PreparedActiveReblitBootRenderInputs<'stone, 'roots>,
    _roots: RevalidatedActiveReblitBootStateRoots<'attempt>,
    _local_policy: RevalidatedActiveReblitLocalBootPolicy<'attempt>,
    _root_intent: RevalidatedActiveReblitRootFilesystemIntent<'attempt>,
    deadline: Instant,
    cmdlines: Box<[MaterializedActiveReblitKernelCmdline]>,
    total_cmdline_bytes: usize,
    total_cmdline_tokens: usize,
}

struct PreparedActiveReblitKernelRenderInput {
    state_id: state::Id,
    version: Box<str>,
    kernel: RetainedBootAssetCoordinate,
    initrds: Box<[RetainedBootAssetCoordinate]>,
}

struct MaterializedActiveReblitKernelCmdline {
    cmdline: Box<str>,
    tokens: Box<[CmdlineTokenRange]>,
}

/// One attempt-bound kernel input joined to exact assets, schema, and owned
/// canonical command-line meaning.
pub(in crate::client) struct BoundActiveReblitKernelRenderInput<'a> {
    source_owner: &'a PreparedActiveReblitStoneBootInputs,
    prepared: &'a PreparedActiveReblitKernelRenderInput,
    schema: &'a PreparedActiveReblitStateBootSchema,
    cmdline: &'a MaterializedActiveReblitKernelCmdline,
}

/// One canonical initrd coordinate rebound to the exact Stone owner retained
/// by the attempt. This view is intentionally not `Clone`.
pub(in crate::client) struct BoundActiveReblitInitrdRenderInput<'a> {
    source_owner: &'a PreparedActiveReblitStoneBootInputs,
    coordinate: &'a RetainedBootAssetCoordinate,
    state_id: state::Id,
    version: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CmdlineTokenRange {
    start: u16,
    end: u16,
}

#[derive(Debug, Eq, PartialEq)]
struct RetainedBootAssetCoordinate {
    binding_index: u16,
    state_id: state::Id,
    digest: u128,
    length: u64,
    logical_path: PathBuf,
}

#[derive(Clone, Copy)]
struct BootRenderInputPolicy {
    max_kernels: usize,
    max_cmdline_bytes: usize,
    max_cmdline_tokens: usize,
    max_total_cmdline_bytes: usize,
    max_total_cmdline_tokens: usize,
}

impl<'stone, 'roots> PreparedActiveReblitBootRenderInputs<'stone, 'roots> {
    /// Internally construct all static children from exact owners beneath one
    /// caller deadline.  The state-root view used for schema selection cannot
    /// be substituted because it is revalidated from the retained owner here.
    pub(in crate::client) fn prepare_until(
        stone: &'stone PreparedActiveReblitStoneBootInputs,
        roots_owner: &'roots PreparedActiveReblitBootStateRoots,
        installation: &Installation,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootRenderInputsError> {
        prepare_with_policy_until(stone, roots_owner, installation, RENDER_INPUT_POLICY, deadline)
    }

    pub(in crate::client) fn kernel_count(&self) -> usize {
        self.kernels.len()
    }

    /// Revalidate every retained and machine-local authority with the same
    /// absolute deadline, then materialize a capability-retaining attempt view.
    pub(in crate::client) fn revalidate_until<'attempt>(
        &'attempt self,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        installation: &'attempt Installation,
        local_policy: &'attempt PreparedActiveReblitLocalBootPolicy,
        root_intent: &'attempt PreparedActiveReblitRootFilesystemIntent,
        deadline: Instant,
    ) -> Result<RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>, ActiveReblitBootRenderInputsError>
    where
        'stone: 'attempt,
        'roots: 'attempt,
    {
        revalidate_with_policy_until_and_checkpoints(
            self,
            state_db,
            layout_db,
            installation,
            local_policy,
            root_intent,
            RENDER_INPUT_POLICY,
            deadline,
            || {},
            Instant::now,
        )
    }
}

impl RevalidatedActiveReblitBootRenderInputs<'_, '_, '_> {
    /// The caller-owned absolute deadline covering this complete attempt.
    /// Later pure rendering and planning layers must reuse it rather than
    /// minting a fresh timeout after authenticated input work has completed.
    pub(in crate::client) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(in crate::client) fn global_state(&self) -> state::Id {
        self.prepared.schemas.global_state()
    }

    pub(in crate::client) fn global_schema(&self) -> &ValidatedActiveReblitBootSchema {
        self.prepared
            .schemas
            .schema_for_state(self.global_state())
            .expect("the authenticated global schema remains owned by the prepared aggregate")
            .schema()
    }

    pub(in crate::client) fn systemd_boot_binding_index(&self) -> u16 {
        self.prepared.systemd_boot.binding_index
    }

    pub(in crate::client) fn systemd_boot_digest(&self) -> u128 {
        self.prepared.systemd_boot.digest
    }

    pub(in crate::client) fn systemd_boot_length(&self) -> u64 {
        self.prepared.systemd_boot.length
    }

    pub(in crate::client) fn systemd_boot_asset(&self) -> BoundActiveReblitBootAsset<'_> {
        kernel_join::bind_systemd_boot_coordinate(self.prepared.source_owner, &self.prepared.systemd_boot)
            .expect("retained systemd-boot coordinate remains bound to the immutable Stone owner")
    }

    pub(in crate::client) fn kernels(&self) -> impl ExactSizeIterator<Item = BoundActiveReblitKernelRenderInput<'_>> {
        debug_assert_eq!(self.prepared.kernels.len(), self.cmdlines.len());
        self.prepared
            .kernels
            .iter()
            .zip(self.cmdlines.iter())
            .map(|(prepared, cmdline)| {
                let schema = self
                    .prepared
                    .schemas
                    .schema_for_state(prepared.state_id)
                    .expect("every retained kernel remains joined to one owned schema");
                BoundActiveReblitKernelRenderInput {
                    source_owner: self.prepared.source_owner,
                    prepared,
                    schema,
                    cmdline,
                }
            })
    }

    pub(in crate::client) fn kernel_count(&self) -> usize {
        self.cmdlines.len()
    }

    pub(in crate::client) fn total_cmdline_bytes(&self) -> usize {
        self.total_cmdline_bytes
    }

    pub(in crate::client) fn total_cmdline_tokens(&self) -> usize {
        self.total_cmdline_tokens
    }
}

impl BoundActiveReblitKernelRenderInput<'_> {
    pub(in crate::client) fn state_id(&self) -> state::Id {
        self.prepared.state_id
    }

    pub(in crate::client) fn version(&self) -> &str {
        &self.prepared.version
    }

    pub(in crate::client) fn schema(&self) -> &ValidatedActiveReblitBootSchema {
        self.schema.schema()
    }

    pub(in crate::client) fn schema_source(&self) -> ActiveReblitBootSchemaSourceBinding {
        self.schema.source()
    }

    pub(in crate::client) fn kernel_binding_index(&self) -> u16 {
        self.prepared.kernel.binding_index
    }

    pub(in crate::client) fn kernel_digest(&self) -> u128 {
        self.prepared.kernel.digest
    }

    pub(in crate::client) fn kernel_length(&self) -> u64 {
        self.prepared.kernel.length
    }

    pub(in crate::client) fn kernel_asset(&self) -> BoundActiveReblitBootAsset<'_> {
        kernel_join::bind_kernel_coordinate(
            self.source_owner,
            &self.prepared.kernel,
            self.prepared.state_id,
            &self.prepared.version,
        )
        .expect("retained kernel coordinate remains bound to the immutable Stone owner")
    }

    pub(in crate::client) fn initrds(&self) -> impl ExactSizeIterator<Item = BoundActiveReblitInitrdRenderInput<'_>> {
        self.prepared
            .initrds
            .iter()
            .map(|coordinate| BoundActiveReblitInitrdRenderInput {
                source_owner: self.source_owner,
                coordinate,
                state_id: self.prepared.state_id,
                version: &self.prepared.version,
            })
    }

    pub(in crate::client) fn cmdline(&self) -> &str {
        &self.cmdline.cmdline
    }

    pub(in crate::client) fn cmdline_tokens(&self) -> impl ExactSizeIterator<Item = &str> {
        self.cmdline
            .tokens
            .iter()
            .map(|range| &self.cmdline.cmdline[usize::from(range.start)..usize::from(range.end)])
    }
}

impl BoundActiveReblitInitrdRenderInput<'_> {
    pub(in crate::client) fn state_id(&self) -> state::Id {
        self.state_id
    }

    pub(in crate::client) fn version(&self) -> &str {
        self.version
    }

    pub(in crate::client) fn binding_index(&self) -> u16 {
        self.coordinate.binding_index
    }

    pub(in crate::client) fn digest(&self) -> u128 {
        self.coordinate.digest
    }

    pub(in crate::client) fn length(&self) -> u64 {
        self.coordinate.length
    }

    pub(in crate::client) fn logical_path(&self) -> &Path {
        &self.coordinate.logical_path
    }

    pub(in crate::client) fn logical_basename(&self) -> &OsStr {
        self.coordinate
            .logical_path
            .file_name()
            .expect("authenticated initrd coordinate has a canonical basename")
    }

    pub(in crate::client) fn asset(&self) -> BoundActiveReblitBootAsset<'_> {
        kernel_join::bind_initrd_coordinate(self.source_owner, self.coordinate, self.state_id, self.version)
            .expect("retained initrd coordinate remains bound to the immutable Stone owner")
    }
}

fn prepare_with_policy_until<'stone, 'roots>(
    stone: &'stone PreparedActiveReblitStoneBootInputs,
    roots_owner: &'roots PreparedActiveReblitBootStateRoots,
    installation: &Installation,
    policy: BootRenderInputPolicy,
    deadline: Instant,
) -> Result<PreparedActiveReblitBootRenderInputs<'stone, 'roots>, ActiveReblitBootRenderInputsError> {
    require_deadline(deadline, "prepared coordinator entry", Instant::now())?;
    let roots = roots_owner.revalidate_until(installation, deadline)?;
    let package_cmdlines = PreparedActiveReblitPackageCmdlineInputs::prepare_until(stone, deadline)?;
    if package_cmdlines.projected_state_ids() != stone.state_ids() {
        return Err(ActiveReblitBootRenderInputsError::PackageProjectionChanged);
    }
    let schemas = PreparedActiveReblitBootSchemas::prepare_until(stone, &roots, deadline)?;
    let systemd_boot = kernel_join::derive_systemd_boot_coordinate(stone, deadline)?;
    let kernels = kernel_join::derive_kernel_seeds(stone, &roots, &schemas, policy, deadline)?.into_boxed_slice();
    let prepared = PreparedActiveReblitBootRenderInputs {
        source_owner: stone,
        roots_owner,
        package_cmdlines,
        schemas,
        systemd_boot,
        kernels,
    };
    require_deadline(deadline, "terminal prepared aggregate", Instant::now())?;
    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
fn revalidate_with_policy_until_and_checkpoints<'attempt, 'stone, 'roots, F, N>(
    prepared: &'attempt PreparedActiveReblitBootRenderInputs<'stone, 'roots>,
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
    installation: &'attempt Installation,
    local_policy: &'attempt PreparedActiveReblitLocalBootPolicy,
    root_intent: &'attempt PreparedActiveReblitRootFilesystemIntent,
    policy: BootRenderInputPolicy,
    deadline: Instant,
    before_final_stone_revalidation: F,
    mut terminal_now: N,
) -> Result<RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>, ActiveReblitBootRenderInputsError>
where
    'stone: 'attempt,
    'roots: 'attempt,
    F: FnOnce(),
    N: FnMut() -> Instant,
{
    require_deadline(deadline, "revalidated coordinator entry", Instant::now())?;
    prepared.source_owner.revalidate_until(state_db, layout_db, deadline)?;
    let roots = prepared.roots_owner.revalidate_until(installation, deadline)?;
    prepared.package_cmdlines.revalidate_until(deadline)?;
    prepared
        .schemas
        .revalidate_sources_until(prepared.source_owner, &roots, deadline)?;
    let local_policy = local_policy.revalidate_until(installation, deadline)?;
    let root_intent = root_intent.revalidate_until(installation, deadline)?;
    kernel_join::revalidate_asset_coordinates(
        prepared.source_owner,
        &prepared.systemd_boot,
        &prepared.kernels,
        deadline,
    )?;

    let audited = cmdline::AuditedCmdlineInputs::prepare(&prepared.package_cmdlines, &local_policy, deadline)?;
    let mut cmdlines = Vec::new();
    cmdlines
        .try_reserve_exact(prepared.kernels.len())
        .map_err(|source| allocation("revalidated kernel command lines", source))?;
    let mut total_cmdline_bytes = 0usize;
    let mut total_cmdline_tokens = 0usize;
    for kernel in &prepared.kernels {
        require_deadline(deadline, "kernel command-line materialization", Instant::now())?;
        let rendered = cmdline::materialize_kernel_cmdline(
            &audited,
            kernel.state_id,
            &kernel.version,
            root_intent.kernel_argument(),
            policy,
            &mut total_cmdline_bytes,
            &mut total_cmdline_tokens,
            deadline,
        )?;
        cmdlines.push(MaterializedActiveReblitKernelCmdline {
            cmdline: rendered.cmdline,
            tokens: rendered.tokens,
        });
    }
    drop(audited);

    // Close the database sandwich only after every other authority and every
    // owned semantic byte is complete.  A projection change during the long
    // validation path must never escape in an attempt view.
    before_final_stone_revalidation();
    prepared.source_owner.revalidate_until(state_db, layout_db, deadline)?;

    let revalidated = RevalidatedActiveReblitBootRenderInputs {
        prepared,
        _roots: roots,
        _local_policy: local_policy,
        _root_intent: root_intent,
        deadline,
        cmdlines: cmdlines.into_boxed_slice(),
        total_cmdline_bytes,
        total_cmdline_tokens,
    };
    require_deadline(deadline, "terminal revalidated aggregate", terminal_now())?;
    Ok(revalidated)
}

fn require_deadline(
    deadline: Instant,
    checkpoint: &'static str,
    now: Instant,
) -> Result<(), ActiveReblitBootRenderInputsError> {
    if now > deadline {
        Err(ActiveReblitBootRenderInputsError::DeadlineExceeded { checkpoint })
    } else {
        Ok(())
    }
}

fn allocation(resource: &'static str, source: TryReserveError) -> ActiveReblitBootRenderInputsError {
    ActiveReblitBootRenderInputsError::Allocation { resource, source }
}

#[cfg(test)]
#[path = "active_reblit_boot_render_inputs_tests.rs"]
mod tests;
