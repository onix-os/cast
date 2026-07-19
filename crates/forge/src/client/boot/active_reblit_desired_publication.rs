//! Canonical desired boot-publication data for one ActiveReblit attempt.
//!
//! The inventory is pure owned data. It grants no filesystem I/O, mutation,
//! deletion, persistence, descriptor, topology, or ownership authority. Its
//! fingerprint binds only canonical desired-output facts. Stone binding
//! indices, source kind and bytes, runtime mount identities, the fingerprint
//! itself, and provenance claims are deliberately excluded.

use std::{
    cmp::Ordering,
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
    time::Instant,
};

use sha2::{Digest as _, Sha256};

use super::{
    active_reblit_bls_renderer::{BoundActiveReblitBlsPublication, BoundActiveReblitBlsPublicationPlan},
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
        ActiveReblitBootPublicationRole, MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS,
    },
    boot_content_identity::BootContentIdentity,
};

#[path = "active_reblit_desired_publication/error.rs"]
mod error;

pub(in crate::client) use error::ActiveReblitDesiredPublicationError;

const DESIRED_PUBLICATION_DOMAIN: &[u8] = b"os-tools/forge/active-reblit-desired-publication/v1\0";
const MAX_DESIRED_PUBLICATION_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESIRED_PUBLICATION_SINGLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
const MAX_DESIRED_PUBLICATION_LOGICAL_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_DESIRED_PUBLICATION_CANONICAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_DESIRED_PUBLICATION_WORK: usize = 32 * 1024 * 1024;
const SORT_WORK_PER_ELEMENT_LEVEL: usize = 4;

const DESIRED_PUBLICATION_POLICY: DesiredPublicationPolicy = DesiredPublicationPolicy {
    max_publications: MAX_ACTIVE_REBLIT_BOOT_PUBLICATIONS,
    max_path_bytes: MAX_DESIRED_PUBLICATION_PATH_BYTES,
    max_single_path_bytes: MAX_DESIRED_PUBLICATION_SINGLE_PATH_BYTES,
    max_logical_bytes: MAX_DESIRED_PUBLICATION_LOGICAL_BYTES,
    max_canonical_bytes: MAX_DESIRED_PUBLICATION_CANONICAL_BYTES,
    max_work: MAX_DESIRED_PUBLICATION_WORK,
};

/// SHA-256 of the canonical desired-publication body.
///
/// This type is intentionally distinct from boot-content SHA-256: it identifies
/// an entire desired output set, not the bytes of one output.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::client) struct ActiveReblitDesiredPublicationFingerprint([u8; 32]);

impl ActiveReblitDesiredPublicationFingerprint {
    pub(in crate::client) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One authority-free owned output in canonical inventory order.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::client) struct DesiredActiveReblitBootPublication {
    root: ActiveReblitBootDestinationRoot,
    phase: ActiveReblitBootPublicationPhase,
    role: ActiveReblitBootPublicationRole,
    relative_path: PathBuf,
    mode: u32,
    checksum: u128,
    length: u64,
    content_identity: BootContentIdentity,
}

impl DesiredActiveReblitBootPublication {
    pub(in crate::client) const fn root(&self) -> ActiveReblitBootDestinationRoot {
        self.root
    }

    pub(in crate::client) const fn phase(&self) -> ActiveReblitBootPublicationPhase {
        self.phase
    }

    pub(in crate::client) const fn role(&self) -> ActiveReblitBootPublicationRole {
        self.role
    }

    pub(in crate::client) fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub(in crate::client) const fn mode(&self) -> u32 {
        self.mode
    }

    /// Non-cryptographic XXH3 checksum retained by the existing path protocol.
    pub(in crate::client) const fn checksum(&self) -> u128 {
        self.checksum
    }

    pub(in crate::client) const fn length(&self) -> u64 {
        self.length
    }

    pub(in crate::client) const fn content_identity(&self) -> BootContentIdentity {
        self.content_identity
    }
}

/// Complete canonical desired state, detached from all attempt authority.
#[derive(Debug)]
pub(in crate::client) struct PreparedActiveReblitDesiredPublicationInventory {
    destination_layout: ActiveReblitBootDestinationLayout,
    outputs: Vec<DesiredActiveReblitBootPublication>,
    fingerprint: ActiveReblitDesiredPublicationFingerprint,
    path_bytes: usize,
    logical_bytes: u64,
    canonical_bytes: usize,
    work: usize,
}

impl PreparedActiveReblitDesiredPublicationInventory {
    pub(in crate::client) const fn destination_layout(&self) -> ActiveReblitBootDestinationLayout {
        self.destination_layout
    }

    pub(in crate::client) fn outputs(&self) -> &[DesiredActiveReblitBootPublication] {
        &self.outputs
    }

    pub(in crate::client) const fn fingerprint(&self) -> ActiveReblitDesiredPublicationFingerprint {
        self.fingerprint
    }

    pub(in crate::client) const fn path_bytes(&self) -> usize {
        self.path_bytes
    }

    pub(in crate::client) const fn canonical_bytes(&self) -> usize {
        self.canonical_bytes
    }

    pub(in crate::client) const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub(in crate::client) const fn work(&self) -> usize {
        self.work
    }
}

#[derive(Clone, Copy)]
struct DesiredPublicationPolicy {
    max_publications: usize,
    max_path_bytes: usize,
    max_single_path_bytes: usize,
    max_logical_bytes: u64,
    max_canonical_bytes: usize,
    max_work: usize,
}

struct DesiredPublicationBudget<'clock, Clock> {
    policy: DesiredPublicationPolicy,
    deadline: Instant,
    now: &'clock mut Clock,
    publications: usize,
    path_bytes: usize,
    logical_bytes: u64,
    canonical_bytes: usize,
    work: usize,
}

struct DesiredPublicationBuilder<'clock, Clock> {
    destination_layout: ActiveReblitBootDestinationLayout,
    expected_publications: usize,
    outputs: Vec<DesiredActiveReblitBootPublication>,
    budget: DesiredPublicationBudget<'clock, Clock>,
}

impl<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>
    BoundActiveReblitBlsPublicationPlan<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>
{
    /// Project the already-bound renderer plan into pure canonical owned data.
    pub(in crate::client) fn prepare_desired_publication_inventory(
        &self,
    ) -> Result<PreparedActiveReblitDesiredPublicationInventory, ActiveReblitDesiredPublicationError> {
        let deadline = self.input_deadline();
        let mut now = Instant::now;
        prepare_bound_inventory_with_policy_and_clocks(
            self,
            DESIRED_PUBLICATION_POLICY,
            deadline,
            &mut now,
            Instant::now,
        )
    }
}

fn prepare_bound_inventory_with_policy_and_clocks<
    'plan,
    'input: 'plan,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
    Clock,
    TerminalClock,
>(
    plan: &'plan BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    policy: DesiredPublicationPolicy,
    deadline: Instant,
    now: &mut Clock,
    terminal_now: TerminalClock,
) -> Result<PreparedActiveReblitDesiredPublicationInventory, ActiveReblitDesiredPublicationError>
where
    Clock: FnMut() -> Instant,
    TerminalClock: FnOnce() -> Instant,
{
    if deadline != plan.input_deadline() {
        return Err(ActiveReblitDesiredPublicationError::DeadlineMismatch {
            expected: plan.input_deadline(),
            actual: deadline,
        });
    }
    let mut builder = DesiredPublicationBuilder::new(
        plan.destination_layout(),
        plan.publication_count(),
        policy,
        deadline,
        now,
    )?;
    for output in plan.outputs() {
        builder.push_bound(&output)?;
    }
    if builder.budget.path_bytes != plan.publication_path_bytes() {
        return Err(ActiveReblitDesiredPublicationError::PreparedPathByteMismatch);
    }
    if builder.budget.logical_bytes != plan.logical_bytes() {
        return Err(ActiveReblitDesiredPublicationError::PreparedLogicalByteMismatch);
    }
    builder.finish(terminal_now)
}

impl<'clock, Clock> DesiredPublicationBuilder<'clock, Clock>
where
    Clock: FnMut() -> Instant,
{
    fn new(
        destination_layout: ActiveReblitBootDestinationLayout,
        expected_publications: usize,
        policy: DesiredPublicationPolicy,
        deadline: Instant,
        now: &'clock mut Clock,
    ) -> Result<Self, ActiveReblitDesiredPublicationError> {
        let mut budget = DesiredPublicationBudget::new(policy, deadline, now)?;
        budget.require_publication_count(expected_publications)?;
        budget.require_deadline("pre-allocation")?;
        let mut outputs = Vec::new();
        outputs.try_reserve_exact(expected_publications).map_err(|source| {
            ActiveReblitDesiredPublicationError::Allocation {
                resource: "canonical desired outputs",
                source,
            }
        })?;
        budget.require_deadline("post-allocation")?;
        Ok(Self {
            destination_layout,
            expected_publications,
            outputs,
            budget,
        })
    }

    fn push_bound(
        &mut self,
        output: &BoundActiveReblitBlsPublication<'_, '_>,
    ) -> Result<(), ActiveReblitDesiredPublicationError> {
        self.push(
            output.root(),
            output.phase(),
            output.role(),
            output.relative_path(),
            output.mode(),
            output.expected_digest(),
            output.expected_length(),
            output.expected_content_identity(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        root: ActiveReblitBootDestinationRoot,
        phase: ActiveReblitBootPublicationPhase,
        role: ActiveReblitBootPublicationRole,
        relative_path: &Path,
        mode: u32,
        checksum: u128,
        length: u64,
        content_identity: BootContentIdentity,
    ) -> Result<(), ActiveReblitDesiredPublicationError> {
        self.budget
            .admit_output(relative_path.as_os_str().as_bytes().len(), length)?;
        if self.outputs.len() >= self.expected_publications {
            return Err(ActiveReblitDesiredPublicationError::PublicationCountMismatch {
                expected: self.expected_publications,
                actual: self.outputs.len().saturating_add(1),
            });
        }
        let relative_path = clone_path_fallibly(relative_path, &mut self.budget)?;
        self.outputs.push(DesiredActiveReblitBootPublication {
            root,
            phase,
            role,
            relative_path,
            mode,
            checksum,
            length,
            content_identity,
        });
        Ok(())
    }

    fn finish<TerminalClock>(
        mut self,
        terminal_now: TerminalClock,
    ) -> Result<PreparedActiveReblitDesiredPublicationInventory, ActiveReblitDesiredPublicationError>
    where
        TerminalClock: FnOnce() -> Instant,
    {
        if self.outputs.len() != self.expected_publications {
            return Err(ActiveReblitDesiredPublicationError::PublicationCountMismatch {
                expected: self.expected_publications,
                actual: self.outputs.len(),
            });
        }
        // The pessimistic comparison charge is bounded by the admitted count;
        // aggregate and per-path caps bound every comparison. Check the same
        // caller-owned deadline immediately after the in-place sort.
        self.budget.reserve_sort_work(self.outputs.len())?;
        self.outputs.sort_unstable_by(compare_desired_output);
        self.budget.require_deadline("canonical output sort")?;

        let mut hasher = Sha256::new();
        hash_field(&mut hasher, b"domain", DESIRED_PUBLICATION_DOMAIN, &mut self.budget)?;
        hash_field(
            &mut hasher,
            b"destination-layout",
            &destination_layout_tag(self.destination_layout).to_le_bytes(),
            &mut self.budget,
        )?;
        let output_count = u64::try_from(self.outputs.len())
            .map_err(|_| ActiveReblitDesiredPublicationError::ScalarNotRepresentable { field: "output count" })?;
        hash_field(
            &mut hasher,
            b"output-count",
            &output_count.to_le_bytes(),
            &mut self.budget,
        )?;
        for output in &self.outputs {
            hash_output(&mut hasher, output, &mut self.budget)?;
        }
        let fingerprint = ActiveReblitDesiredPublicationFingerprint(hasher.finalize().into());
        self.budget
            .require_deadline_at("terminal desired-publication inventory", terminal_now())?;

        Ok(PreparedActiveReblitDesiredPublicationInventory {
            destination_layout: self.destination_layout,
            outputs: self.outputs,
            fingerprint,
            path_bytes: self.budget.path_bytes,
            logical_bytes: self.budget.logical_bytes,
            canonical_bytes: self.budget.canonical_bytes,
            work: self.budget.work,
        })
    }
}

impl<'clock, Clock> DesiredPublicationBudget<'clock, Clock>
where
    Clock: FnMut() -> Instant,
{
    fn new(
        policy: DesiredPublicationPolicy,
        deadline: Instant,
        now: &'clock mut Clock,
    ) -> Result<Self, ActiveReblitDesiredPublicationError> {
        let mut budget = Self {
            policy,
            deadline,
            now,
            publications: 0,
            path_bytes: 0,
            logical_bytes: 0,
            canonical_bytes: 0,
            work: 0,
        };
        budget.require_deadline("inventory entry")?;
        Ok(budget)
    }

    fn admit_output(&mut self, path_bytes: usize, length: u64) -> Result<(), ActiveReblitDesiredPublicationError> {
        self.reserve_work(1)?;
        let publications = self.publications.saturating_add(1);
        self.require_publication_count(publications)?;
        if path_bytes > self.policy.max_single_path_bytes {
            return Err(ActiveReblitDesiredPublicationError::SinglePathByteLimit {
                limit: self.policy.max_single_path_bytes,
                actual: path_bytes,
            });
        }
        let aggregate_path_bytes = self.path_bytes.checked_add(path_bytes).unwrap_or(usize::MAX);
        if aggregate_path_bytes > self.policy.max_path_bytes {
            return Err(ActiveReblitDesiredPublicationError::PathByteLimit {
                limit: self.policy.max_path_bytes,
                actual: aggregate_path_bytes,
            });
        }
        let logical_bytes = self.logical_bytes.checked_add(length).unwrap_or(u64::MAX);
        if logical_bytes > self.policy.max_logical_bytes {
            return Err(ActiveReblitDesiredPublicationError::LogicalByteLimit {
                limit: self.policy.max_logical_bytes,
                actual: logical_bytes,
            });
        }
        self.publications = publications;
        self.path_bytes = aggregate_path_bytes;
        self.logical_bytes = logical_bytes;
        Ok(())
    }

    fn require_publication_count(&self, actual: usize) -> Result<(), ActiveReblitDesiredPublicationError> {
        if actual > self.policy.max_publications {
            Err(ActiveReblitDesiredPublicationError::PublicationCountLimit {
                limit: self.policy.max_publications,
                actual,
            })
        } else {
            Ok(())
        }
    }

    fn admit_canonical_bytes(&mut self, amount: usize) -> Result<(), ActiveReblitDesiredPublicationError> {
        let canonical_bytes = self.canonical_bytes.checked_add(amount).unwrap_or(usize::MAX);
        if canonical_bytes > self.policy.max_canonical_bytes {
            return Err(ActiveReblitDesiredPublicationError::CanonicalByteLimit {
                limit: self.policy.max_canonical_bytes,
                actual: canonical_bytes,
            });
        }
        self.reserve_work(amount)?;
        self.canonical_bytes = canonical_bytes;
        Ok(())
    }

    fn reserve_sort_work(&mut self, count: usize) -> Result<(), ActiveReblitDesiredPublicationError> {
        self.reserve_work(conservative_sort_work(count))
    }

    fn reserve_work(&mut self, amount: usize) -> Result<(), ActiveReblitDesiredPublicationError> {
        self.require_deadline("bounded canonical work")?;
        let work = self.work.checked_add(amount).unwrap_or(usize::MAX);
        if work > self.policy.max_work {
            return Err(ActiveReblitDesiredPublicationError::WorkLimit {
                limit: self.policy.max_work,
                actual: work,
            });
        }
        self.work = work;
        Ok(())
    }

    fn require_deadline(&mut self, checkpoint: &'static str) -> Result<(), ActiveReblitDesiredPublicationError> {
        let now = (self.now)();
        self.require_deadline_at(checkpoint, now)
    }

    fn require_deadline_at(
        &self,
        checkpoint: &'static str,
        now: Instant,
    ) -> Result<(), ActiveReblitDesiredPublicationError> {
        if now > self.deadline {
            Err(ActiveReblitDesiredPublicationError::DeadlineExceeded { checkpoint })
        } else {
            Ok(())
        }
    }
}

fn clone_path_fallibly<Clock>(
    path: &Path,
    budget: &mut DesiredPublicationBudget<'_, Clock>,
) -> Result<PathBuf, ActiveReblitDesiredPublicationError>
where
    Clock: FnMut() -> Instant,
{
    budget.require_deadline("pre-path allocation")?;
    let bytes = path.as_os_str().as_bytes();
    budget.reserve_work(bytes.len())?;
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(bytes.len())
        .map_err(|source| ActiveReblitDesiredPublicationError::Allocation {
            resource: "desired publication path",
            source,
        })?;
    cloned.extend_from_slice(bytes);
    budget.require_deadline("post-path allocation")?;
    Ok(PathBuf::from(OsString::from_vec(cloned)))
}

fn hash_output<Clock>(
    hasher: &mut Sha256,
    output: &DesiredActiveReblitBootPublication,
    budget: &mut DesiredPublicationBudget<'_, Clock>,
) -> Result<(), ActiveReblitDesiredPublicationError>
where
    Clock: FnMut() -> Instant,
{
    hash_field(
        hasher,
        b"logical-root",
        &destination_root_tag(output.root).to_le_bytes(),
        budget,
    )?;
    hash_field(
        hasher,
        b"publication-phase",
        &publication_phase_tag(output.phase).to_le_bytes(),
        budget,
    )?;
    hash_field(
        hasher,
        b"semantic-role",
        &publication_role_tag(output.role).to_le_bytes(),
        budget,
    )?;
    hash_field(
        hasher,
        b"relative-path",
        output.relative_path.as_os_str().as_bytes(),
        budget,
    )?;
    hash_field(hasher, b"mode", &u64::from(output.mode).to_le_bytes(), budget)?;
    hash_field(hasher, b"xxh3-checksum", &output.checksum.to_le_bytes(), budget)?;
    hash_field(hasher, b"exact-length", &output.length.to_le_bytes(), budget)?;
    hash_field(hasher, b"content-sha256", output.content_identity.as_bytes(), budget)
}

fn hash_field<Clock>(
    hasher: &mut Sha256,
    name: &[u8],
    value: &[u8],
    budget: &mut DesiredPublicationBudget<'_, Clock>,
) -> Result<(), ActiveReblitDesiredPublicationError>
where
    Clock: FnMut() -> Instant,
{
    let name_length = u64::try_from(name.len())
        .map_err(|_| ActiveReblitDesiredPublicationError::ScalarNotRepresentable { field: "field-name" })?;
    let value_length = u64::try_from(value.len())
        .map_err(|_| ActiveReblitDesiredPublicationError::ScalarNotRepresentable { field: "field value" })?;
    let framed_bytes = name
        .len()
        .checked_add(value.len())
        .and_then(|length| length.checked_add(16))
        .unwrap_or(usize::MAX);
    budget.admit_canonical_bytes(framed_bytes)?;
    hasher.update(name_length.to_le_bytes());
    hasher.update(name);
    hasher.update(value_length.to_le_bytes());
    hasher.update(value);
    budget.require_deadline("canonical field hash")
}

fn compare_desired_output(
    left: &DesiredActiveReblitBootPublication,
    right: &DesiredActiveReblitBootPublication,
) -> Ordering {
    left.phase
        .cmp(&right.phase)
        .then_with(|| left.root.cmp(&right.root))
        .then_with(|| {
            ascii_fold_cmp(
                left.relative_path.as_os_str().as_bytes(),
                right.relative_path.as_os_str().as_bytes(),
            )
        })
        .then_with(|| {
            left.relative_path
                .as_os_str()
                .as_bytes()
                .cmp(right.relative_path.as_os_str().as_bytes())
        })
}

fn ascii_fold_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(right.iter().map(u8::to_ascii_lowercase))
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

const fn destination_layout_tag(layout: ActiveReblitBootDestinationLayout) -> u64 {
    match layout {
        ActiveReblitBootDestinationLayout::BootAliasesEsp => 1,
        ActiveReblitBootDestinationLayout::DistinctXbootldr => 2,
    }
}

const fn destination_root_tag(root: ActiveReblitBootDestinationRoot) -> u64 {
    match root {
        ActiveReblitBootDestinationRoot::Esp => 1,
        ActiveReblitBootDestinationRoot::Boot => 2,
    }
}

const fn publication_phase_tag(phase: ActiveReblitBootPublicationPhase) -> u64 {
    match phase {
        ActiveReblitBootPublicationPhase::Payload => 1,
        ActiveReblitBootPublicationPhase::Entry => 2,
        ActiveReblitBootPublicationPhase::LoaderControl => 3,
        ActiveReblitBootPublicationPhase::Bootloader => 4,
    }
}

const fn publication_role_tag(role: ActiveReblitBootPublicationRole) -> u64 {
    match role {
        ActiveReblitBootPublicationRole::Payload => 1,
        ActiveReblitBootPublicationRole::Entry => 2,
        ActiveReblitBootPublicationRole::LoaderControl => 3,
        ActiveReblitBootPublicationRole::FallbackBootloader => 4,
        ActiveReblitBootPublicationRole::SystemdBootloader => 5,
    }
}

#[cfg(test)]
#[path = "active_reblit_desired_publication_tests.rs"]
mod tests;
