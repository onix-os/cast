//! Pure authenticated installed-versus-desired boot-publication delta.
//!
//! This module turns one exact database receipt snapshot, one bound desired
//! plan and an unforgeable retained-preflight assessment into inert actions.
//! It owns no file, descriptor, database handle, journal handle or mutation callback. In
//! particular, a decoded receipt or caller-authored provenance claim cannot be
//! passed directly: ownership is considered only after the receipt is proven
//! to be the sole promoted database head named by its exact durable
//! correlation.

use std::{
    collections::{BTreeMap, TryReserveError},
    os::unix::ffi::OsStrExt as _,
    time::Instant,
};

use thiserror::Error;

use super::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_boot_publication_receipt::BorrowedActiveReblitBootPublicationProvenanceClaim,
    active_reblit_desired_publication::{
        DesiredActiveReblitBootPublication, PreparedActiveReblitDesiredPublicationInventory,
    },
    active_reblit_mounted_boot_topology::{
        BoundActiveReblitMountedBootTarget, BoundActiveReblitMountedBootTopology,
    },
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot,
        ActiveReblitBootPublicationPhase, ActiveReblitBootPublicationRole,
    },
    boot_content_identity::BootContentIdentity,
};
use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations, BootPublicationOutput,
        BootPublicationOutputProvenanceClaim, BootPublicationOutputRole, BootPublicationSha256,
        BootPublicationPublicationPhase, BootPublicationRoot,
    },
    db::state::{BootPublicationReceiptState, ExactPromotedBootPublicationReceiptChain},
};

/// Opaque exact installed chain derived from one strict database snapshot.
///
/// The constructor derives every identity from the sole committed head/body;
/// callers cannot inject a detached transition ID, fingerprint or predecessor.
/// An empty value is admitted only from a completely empty strict snapshot.
#[derive(Clone, Copy, Debug)]
pub(in crate::client) struct AuthenticatedActiveReblitInstalledBootPublication<'state> {
    receipt: Option<&'state crate::boot_publication::CanonicalBootPublicationReceipt>,
}

impl<'state> AuthenticatedActiveReblitInstalledBootPublication<'state> {
    /// Authenticate the absence of an installed receipt from a strict empty
    /// database snapshot. Any head or body, including pending state, fails.
    pub(in crate::client) fn from_strict_empty_state(
        state: &'state BootPublicationReceiptState,
    ) -> Result<Self, ActiveReblitBootPublicationDeltaError> {
        if state.head().pending().is_some() || state.pending().is_some() {
            return Err(ActiveReblitBootPublicationDeltaError::InstalledStatePending);
        }
        if state.head().committed().is_some() || state.committed().is_some() {
            return Err(ActiveReblitBootPublicationDeltaError::UnexpectedInstalledReceipt);
        }
        Ok(Self { receipt: None })
    }

    /// Borrow the installed body only from the opaque exact promoted-chain
    /// loader, which has already authenticated head, body, transition and
    /// predecessor correlation in one database transaction.
    pub(in crate::client) fn from_exact_promoted_chain(
        chain: &'state ExactPromotedBootPublicationReceiptChain,
    ) -> Self {
        Self {
            receipt: Some(chain.installed_receipt()),
        }
    }

    pub(in crate::client) const fn receipt(
        self,
    ) -> Option<&'state crate::boot_publication::CanonicalBootPublicationReceipt> {
        self.receipt
    }
}

/// Exact expected bytes for one side of a union assessment request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitBootPublicationDeltaExpected {
    checksum: u128,
    length: u64,
    content_identity: BootContentIdentity,
}

impl ActiveReblitBootPublicationDeltaExpected {
    pub(in crate::client) const fn checksum(self) -> u128 {
        self.checksum
    }

    pub(in crate::client) const fn length(self) -> u64 {
        self.length
    }

    pub(in crate::client) const fn content_identity(self) -> BootContentIdentity {
        self.content_identity
    }
}

/// One canonical union key to assess against desired and/or installed bytes.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ActiveReblitBootPublicationDeltaRequest {
    root: ActiveReblitBootDestinationRoot,
    relative_path: Box<str>,
    desired: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed_owned: bool,
}

impl ActiveReblitBootPublicationDeltaRequest {
    pub(in crate::client) const fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub(in crate::client) const fn desired_expected(
        &self,
    ) -> Option<ActiveReblitBootPublicationDeltaExpected> {
        self.desired
    }

    pub(in crate::client) const fn installed_expected(
        &self,
    ) -> Option<ActiveReblitBootPublicationDeltaExpected> {
        self.installed
    }

    pub(in crate::client) const fn installed_is_owned(&self) -> bool {
        self.installed_owned
    }
}

/// Pure action class. Deletion is deliberately named as post-promotion work;
/// this value contains no capability to perform it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootPublicationDeltaAction {
    PublishDesired,
    RetainOwnedDesired,
    PreserveBorrowedDesired,
    ReplaceOwnedDesired,
    DeleteOwnedStaleAfterPromotion,
    PreserveUnownedStale,
}

/// One inert classified action, retaining its exact canonical union key and
/// both old and new byte identities when those sides exist.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ClassifiedActiveReblitBootPublicationDeltaEntry {
    root: ActiveReblitBootDestinationRoot,
    relative_path: Box<str>,
    desired_expected: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed_expected: Option<ActiveReblitBootPublicationDeltaExpected>,
    action: ActiveReblitBootPublicationDeltaAction,
}

impl ClassifiedActiveReblitBootPublicationDeltaEntry {
    pub(in crate::client) const fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub(in crate::client) const fn desired_expected(
        &self,
    ) -> Option<ActiveReblitBootPublicationDeltaExpected> {
        self.desired_expected
    }

    pub(in crate::client) const fn installed_expected(
        &self,
    ) -> Option<ActiveReblitBootPublicationDeltaExpected> {
        self.installed_expected
    }

    pub(in crate::client) const fn action(&self) -> ActiveReblitBootPublicationDeltaAction {
        self.action
    }
}

/// Complete inert action set. It is not publication, replacement or deletion
/// authority and cannot recover the database or topology objects used to
/// prepare it.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct ClassifiedActiveReblitBootPublicationDelta {
    entries: Vec<ClassifiedActiveReblitBootPublicationDeltaEntry>,
}

impl ClassifiedActiveReblitBootPublicationDelta {
    pub(in crate::client) fn entries(&self) -> &[ClassifiedActiveReblitBootPublicationDeltaEntry] {
        &self.entries
    }

    /// Derive inert receipt claims in the exact canonical desired-inventory
    /// order. The returned bindings borrow only desired paths and carry data;
    /// they cannot publish, replace, remove, or otherwise mutate an output.
    ///
    /// Every desired output must have exactly one desired action under its
    /// exact logical root and byte-for-byte path. Stale actions deliberately
    /// produce no claim, while an extra desired action or a stale action under
    /// a desired key fails closed.
    pub(in crate::client) fn derive_receipt_provenance_claims<'inventory>(
        &self,
        desired: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    ) -> Result<
        Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory>>,
        ActiveReblitBootPublicationDeltaError,
    > {
        let mut keyed_entries = BTreeMap::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let key = (entry.root, entry.relative_path.as_bytes());
            if keyed_entries
                .insert(key, (index, entry.action, entry.desired_expected))
                .is_some()
            {
                return Err(ActiveReblitBootPublicationDeltaError::DuplicateClassifiedKey {
                    index,
                });
            }
        }

        let mut claims = Vec::new();
        claims
            .try_reserve_exact(desired.outputs().len())
            .map_err(|source| ActiveReblitBootPublicationDeltaError::Allocation {
                resource: "derived receipt provenance claims",
                source,
            })?;
        for (desired_index, output) in desired.outputs().iter().enumerate() {
            let key = (
                output.root(),
                output.relative_path().as_os_str().as_bytes(),
            );
            let Some((delta_index, action, classified_expected)) = keyed_entries.remove(&key) else {
                return Err(ActiveReblitBootPublicationDeltaError::MissingDesiredClassifiedKey {
                    desired_index,
                });
            };
            let Some(claim) = receipt_claim_for_desired_action(action) else {
                return Err(ActiveReblitBootPublicationDeltaError::StaleActionForDesiredKey {
                    desired_index,
                    delta_index,
                });
            };
            if classified_expected != Some(desired_expected(output)) {
                return Err(
                    ActiveReblitBootPublicationDeltaError::DesiredClassifiedExpectationMismatch {
                        desired_index,
                        delta_index,
                    },
                );
            }
            claims.push(BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                BootPublicationSha256::from_bytes(*output.content_identity().as_bytes()),
                claim,
            ));
        }

        if let Some((_, (delta_index, _, _))) = keyed_entries
            .iter()
            .find(|(_, (_, action, _))| receipt_claim_for_desired_action(*action).is_some())
        {
            return Err(ActiveReblitBootPublicationDeltaError::UnmatchedDesiredClassifiedKey {
                delta_index: *delta_index,
            });
        }
        if let Some((_, (delta_index, _, _))) = keyed_entries
            .iter()
            .find(|(_, (_, _, expected))| expected.is_some())
        {
            return Err(ActiveReblitBootPublicationDeltaError::StaleClassifiedExpectation {
                delta_index: *delta_index,
            });
        }
        Ok(claims)
    }
}

const fn receipt_claim_for_desired_action(
    action: ActiveReblitBootPublicationDeltaAction,
) -> Option<BootPublicationOutputProvenanceClaim> {
    match action {
        ActiveReblitBootPublicationDeltaAction::PublishDesired => {
            Some(BootPublicationOutputProvenanceClaim::UnclaimedAbsent)
        }
        ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
        | ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {
            Some(BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast)
        }
        ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => {
            Some(BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption)
        }
        ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
        | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => None,
    }
}

/// Authenticated, bounded union request set awaiting one private retained-
/// preflight assessment seal. It retains no database state or effect authority.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct PreparedActiveReblitBootPublicationDelta {
    destination_layout: ActiveReblitBootDestinationLayout,
    requests: Vec<ActiveReblitBootPublicationDeltaRequest>,
}

impl PreparedActiveReblitBootPublicationDelta {
    pub(in crate::client) fn requests(&self) -> &[ActiveReblitBootPublicationDeltaRequest] {
        &self.requests
    }

}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PhysicalDestinationDomain {
    Shared,
    Esp,
    Boot,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PhysicalOutputKey {
    domain: PhysicalDestinationDomain,
    folded_relative_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OutputSemantics {
    phase: ActiveReblitBootPublicationPhase,
    role: ActiveReblitBootPublicationRole,
}

struct DeltaRequestBuilder {
    root: ActiveReblitBootDestinationRoot,
    relative_path: Box<str>,
    semantics: OutputSemantics,
    desired: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed_owned: bool,
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
    /// Prepare the pure union assessment for the exact bound desired plan.
    ///
    /// An empty authenticated value represents first adoption. A non-empty
    /// value retains the sole promoted committed body. The returned value
    /// copies bounded inert facts, so the database snapshot is not retained as
    /// ongoing authority.
    pub(in crate::client) fn prepare_installed_boot_publication_delta(
        &self,
        desired: &PreparedActiveReblitDesiredPublicationInventory,
        installed: AuthenticatedActiveReblitInstalledBootPublication<'_>,
    ) -> Result<PreparedActiveReblitBootPublicationDelta, ActiveReblitBootPublicationDeltaError> {
        require_desired_plan(self, desired)?;
        let installed_receipt = installed.receipt();
        if let Some(receipt) = installed_receipt
            && !destinations_match(receipt.body().destinations(), self.mounted_topology())
        {
            return Err(ActiveReblitBootPublicationDeltaError::InstalledDestinationMismatch);
        }
        prepare_union(
            self.destination_layout(),
            desired,
            installed_receipt.map(|receipt| receipt.body().outputs()),
        )
    }
}

fn require_desired_plan(
    plan: &BoundActiveReblitBlsPublicationPlan<'_, '_, '_, '_, '_, '_>,
    desired: &PreparedActiveReblitDesiredPublicationInventory,
) -> Result<(), ActiveReblitBootPublicationDeltaError> {
    if Instant::now() > plan.input_deadline() {
        return Err(ActiveReblitBootPublicationDeltaError::DeadlineExceeded);
    }
    if !plan.collision_domains_still_match() {
        return Err(ActiveReblitBootPublicationDeltaError::CollisionDomainDrift);
    }
    if desired.destination_layout() != plan.destination_layout()
        || desired.outputs().len() != plan.publication_count()
        || desired.path_bytes() != plan.publication_path_bytes()
        || desired.logical_bytes() != plan.logical_bytes()
    {
        return Err(ActiveReblitBootPublicationDeltaError::DesiredPlanMismatch);
    }
    for (bound, desired) in plan.outputs().zip(desired.outputs()) {
        if bound.root() != desired.root()
            || bound.phase() != desired.phase()
            || bound.role() != desired.role()
            || bound.relative_path() != desired.relative_path()
            || bound.mode() != desired.mode()
            || bound.expected_digest() != desired.checksum()
            || bound.expected_length() != desired.length()
            || bound.expected_content_identity() != desired.content_identity()
        {
            return Err(ActiveReblitBootPublicationDeltaError::DesiredPlanMismatch);
        }
    }
    Ok(())
}

fn prepare_union(
    layout: ActiveReblitBootDestinationLayout,
    desired: &PreparedActiveReblitDesiredPublicationInventory,
    installed: Option<&[BootPublicationOutput]>,
) -> Result<PreparedActiveReblitBootPublicationDelta, ActiveReblitBootPublicationDeltaError> {
    let installed_count = installed.map_or(0, <[BootPublicationOutput]>::len);
    let capacity = desired
        .outputs()
        .len()
        .checked_add(installed_count)
        .ok_or(ActiveReblitBootPublicationDeltaError::UnionCountOverflow)?;
    let mut union = BTreeMap::new();
    for output in installed.into_iter().flatten() {
        let root = map_installed_root(output.root());
        let key = physical_key(layout, root, output.relative_path())?;
        let previous = union.insert(
            key,
            DeltaRequestBuilder {
                root,
                relative_path: clone_text(output.relative_path(), "installed relative path")?,
                semantics: OutputSemantics {
                    phase: map_installed_phase(output.phase()),
                    role: map_installed_role(output.role()),
                },
                desired: None,
                installed: Some(installed_expected(output)),
                installed_owned: installed_claim_is_owned(output.provenance_claim()),
            },
        );
        if previous.is_some() {
            return Err(ActiveReblitBootPublicationDeltaError::DuplicatePhysicalKey);
        }
    }
    for output in desired.outputs() {
        let path = output
            .relative_path()
            .to_str()
            .ok_or(ActiveReblitBootPublicationDeltaError::NonUtf8DesiredPath)?;
        let key = physical_key(layout, output.root(), path)?;
        let expected = desired_expected(output);
        let semantics = OutputSemantics {
            phase: output.phase(),
            role: output.role(),
        };
        if let Some(existing) = union.get_mut(&key) {
            if existing.root != output.root()
                || &*existing.relative_path != path
                || existing.semantics != semantics
                || existing.desired.is_some()
            {
                return Err(ActiveReblitBootPublicationDeltaError::CrossSetPhysicalKeyConflict);
            }
            existing.desired = Some(expected);
        } else {
            union.insert(
                key,
                DeltaRequestBuilder {
                    root: output.root(),
                    relative_path: clone_text(path, "desired relative path")?,
                    semantics,
                    desired: Some(expected),
                    installed: None,
                    installed_owned: false,
                },
            );
        }
    }
    require_no_union_hierarchy_conflicts(&union)?;
    let mut requests = Vec::new();
    requests.try_reserve_exact(capacity.min(union.len())).map_err(|source| {
        ActiveReblitBootPublicationDeltaError::Allocation {
            resource: "delta union requests",
            source,
        }
    })?;
    requests.extend(union.into_values().map(|entry| ActiveReblitBootPublicationDeltaRequest {
        root: entry.root,
        relative_path: entry.relative_path,
        desired: entry.desired,
        installed: entry.installed,
        installed_owned: entry.installed_owned,
    }));
    Ok(PreparedActiveReblitBootPublicationDelta {
        destination_layout: layout,
        requests,
    })
}

fn require_no_union_hierarchy_conflicts(
    union: &BTreeMap<PhysicalOutputKey, DeltaRequestBuilder>,
) -> Result<(), ActiveReblitBootPublicationDeltaError> {
    for key in union.keys() {
        let mut ancestor = key.folded_relative_path.as_str();
        while let Some(separator) = ancestor.rfind('/') {
            ancestor = &ancestor[..separator];
            if union.contains_key(&PhysicalOutputKey {
                domain: key.domain,
                folded_relative_path: ancestor.to_owned(),
            }) {
                return Err(ActiveReblitBootPublicationDeltaError::CrossSetHierarchyConflict);
            }
        }
    }
    Ok(())
}

fn physical_key(
    layout: ActiveReblitBootDestinationLayout,
    root: ActiveReblitBootDestinationRoot,
    path: &str,
) -> Result<PhysicalOutputKey, ActiveReblitBootPublicationDeltaError> {
    let domain = match (layout, root) {
        (ActiveReblitBootDestinationLayout::BootAliasesEsp, _) => PhysicalDestinationDomain::Shared,
        (ActiveReblitBootDestinationLayout::DistinctXbootldr, ActiveReblitBootDestinationRoot::Esp) => {
            PhysicalDestinationDomain::Esp
        }
        (ActiveReblitBootDestinationLayout::DistinctXbootldr, ActiveReblitBootDestinationRoot::Boot) => {
            PhysicalDestinationDomain::Boot
        }
    };
    let mut folded_relative_path = String::new();
    folded_relative_path.try_reserve_exact(path.len()).map_err(|source| {
        ActiveReblitBootPublicationDeltaError::Allocation {
            resource: "FAT-folded delta path",
            source,
        }
    })?;
    folded_relative_path.extend(path.bytes().map(|byte| char::from(byte.to_ascii_lowercase())));
    Ok(PhysicalOutputKey {
        domain,
        folded_relative_path,
    })
}

fn clone_text(
    value: &str,
    resource: &'static str,
) -> Result<Box<str>, ActiveReblitBootPublicationDeltaError> {
    let mut cloned = String::new();
    cloned.try_reserve_exact(value.len()).map_err(|source| {
        ActiveReblitBootPublicationDeltaError::Allocation { resource, source }
    })?;
    cloned.push_str(value);
    Ok(cloned.into_boxed_str())
}

fn desired_expected(output: &DesiredActiveReblitBootPublication) -> ActiveReblitBootPublicationDeltaExpected {
    ActiveReblitBootPublicationDeltaExpected {
        checksum: output.checksum(),
        length: output.length(),
        content_identity: output.content_identity(),
    }
}

fn installed_expected(output: &BootPublicationOutput) -> ActiveReblitBootPublicationDeltaExpected {
    ActiveReblitBootPublicationDeltaExpected {
        checksum: output.xxh3().as_u128(),
        length: output.length(),
        content_identity: BootContentIdentity::from_sha256(*output.content_sha256().as_bytes()),
    }
}

const fn installed_claim_is_owned(claim: BootPublicationOutputProvenanceClaim) -> bool {
    matches!(
        claim,
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent
            | BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
    )
}

fn destinations_match(
    installed: &BootPublicationDestinations,
    current: BoundActiveReblitMountedBootTopology<'_>,
) -> bool {
    match (installed, current) {
        (BootPublicationDestinations::BootAliasesEsp { esp: installed }, BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp }) => destination_matches(installed, esp),
        (BootPublicationDestinations::DistinctXbootldr { esp: installed_esp, xbootldr: installed_xbootldr }, BoundActiveReblitMountedBootTopology::DistinctXbootldr { esp, xbootldr }) => destination_matches(installed_esp, esp) && destination_matches(installed_xbootldr, xbootldr),
        _ => false,
    }
}

fn destination_matches(
    installed: &BootPublicationDestination,
    current: BoundActiveReblitMountedBootTarget<'_>,
) -> bool {
    current.partuuid == current.partition_uuid.as_str()
        && installed.partuuid() == current.partuuid
        && installed.partition_number() == current.partition_number.get()
}

const fn map_installed_root(root: BootPublicationRoot) -> ActiveReblitBootDestinationRoot {
    match root {
        BootPublicationRoot::Esp => ActiveReblitBootDestinationRoot::Esp,
        BootPublicationRoot::Boot => ActiveReblitBootDestinationRoot::Boot,
    }
}

const fn map_installed_phase(
    phase: BootPublicationPublicationPhase,
) -> ActiveReblitBootPublicationPhase {
    match phase {
        BootPublicationPublicationPhase::Payload => ActiveReblitBootPublicationPhase::Payload,
        BootPublicationPublicationPhase::Entry => ActiveReblitBootPublicationPhase::Entry,
        BootPublicationPublicationPhase::LoaderControl => ActiveReblitBootPublicationPhase::LoaderControl,
        BootPublicationPublicationPhase::Bootloader => ActiveReblitBootPublicationPhase::Bootloader,
    }
}

const fn map_installed_role(role: BootPublicationOutputRole) -> ActiveReblitBootPublicationRole {
    match role {
        BootPublicationOutputRole::Payload => ActiveReblitBootPublicationRole::Payload,
        BootPublicationOutputRole::Entry => ActiveReblitBootPublicationRole::Entry,
        BootPublicationOutputRole::LoaderControl => ActiveReblitBootPublicationRole::LoaderControl,
        BootPublicationOutputRole::FallbackBootloader => ActiveReblitBootPublicationRole::FallbackBootloader,
        BootPublicationOutputRole::SystemdBootloader => ActiveReblitBootPublicationRole::SystemdBootloader,
    }
}

/// Closed pure-preparation and classification failures.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootPublicationDeltaError {
    #[error("the bound boot-publication deadline has elapsed")]
    DeadlineExceeded,
    #[error("the bound boot-publication collision domains drifted")]
    CollisionDomainDrift,
    #[error("the desired inventory does not exactly match the bound plan")]
    DesiredPlanMismatch,
    #[error("the installed receipt state contains a pending head or body")]
    InstalledStatePending,
    #[error("an installed receipt exists without an installed correlation")]
    UnexpectedInstalledReceipt,
    #[error("the installed receipt destinations conflict with the desired bound topology")]
    InstalledDestinationMismatch,
    #[error("the desired publication path is unexpectedly non-UTF-8")]
    NonUtf8DesiredPath,
    #[error("the installed and desired output counts overflow")]
    UnionCountOverflow,
    #[error("a physical boot-publication key occurs more than once")]
    DuplicatePhysicalKey,
    #[error("classified delta entry {index} duplicates an exact logical output key")]
    DuplicateClassifiedKey { index: usize },
    #[error("desired output {desired_index} has no exact classified delta key")]
    MissingDesiredClassifiedKey { desired_index: usize },
    #[error("classified delta entry {delta_index} uses a stale action for desired output {desired_index}")]
    StaleActionForDesiredKey {
        desired_index: usize,
        delta_index: usize,
    },
    #[error("classified delta entry {delta_index} does not bind the exact expected bytes for desired output {desired_index}")]
    DesiredClassifiedExpectationMismatch {
        desired_index: usize,
        delta_index: usize,
    },
    #[error("classified delta entry {delta_index} is a desired action without an exact desired key")]
    UnmatchedDesiredClassifiedKey { delta_index: usize },
    #[error("stale classified delta entry {delta_index} unexpectedly carries a desired-byte expectation")]
    StaleClassifiedExpectation { delta_index: usize },
    #[error("an installed and desired output collide after alias-aware FAT folding")]
    CrossSetPhysicalKeyConflict,
    #[error("an installed and desired output have an ancestor/descendant key conflict")]
    CrossSetHierarchyConflict,
    #[error("allocate {resource}")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
    #[error("the sealed preflight expected {expected} desired outputs but the delta contains {actual}")]
    PreflightDesiredCountMismatch { expected: usize, actual: usize },
    #[error("the sealed preflight and installed delta have different destination layouts")]
    PreflightDestinationLayoutMismatch,
    #[error("delta desired output {index} has no exact root/path in the sealed preflight")]
    MissingPreflightDesiredKey { index: usize },
    #[error("delta desired output {index} does not match the sealed preflight byte identity")]
    PreflightDesiredExpectationMismatch { index: usize },
    #[error("the sealed preflight contains a duplicate desired root/path")]
    DuplicatePreflightDesiredKey,
    #[error("sealed preflight desired output {plan_index} was not consumed by the exact delta")]
    UnmatchedPreflightDesiredKey { plan_index: usize },
    #[error("delta output {index} is marked owned without an authenticated installed-byte identity")]
    OwnedOutputWithoutInstalledIdentity { index: usize },
    #[error("desired output {index} is different and not authenticated as exact predecessor-owned content")]
    UnownedDifferentDesired { index: usize },
}

#[path = "active_reblit_installed_boot_publication_delta/live_classification.rs"]
mod live_classification;

#[cfg(test)]
#[path = "active_reblit_installed_boot_publication_delta_tests.rs"]
mod tests;
