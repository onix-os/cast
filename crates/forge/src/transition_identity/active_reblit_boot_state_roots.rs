//! Descriptor-rooted state trees admitted to one ActiveReblit boot repair.
//!
//! The active head is mandatory. Historical wrappers are optional, but an
//! admitted wrapper must prove its exact tree marker, state ID, optional slot
//! marker, wrapper shape, directory inode, and runtime mount identity. Missing
//! or structurally inexact historical wrappers are recorded as exclusions and
//! are never promoted later in the same attempt.

use std::{
    collections::BTreeSet,
    io,
    marker::PhantomData,
    os::{
        fd::{AsFd as _, AsRawFd as _, BorrowedFd},
        unix::ffi::OsStrExt as _,
    },
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};

#[cfg(test)]
use std::time::Duration;

use thiserror::Error;

use crate::{
    Installation, linux_fs, state,
    transition_journal::{RuntimeEpoch, RuntimeEvidenceError, RuntimeTreeIdentity},
    tree_marker::TreeMarkerStore,
};

use super::{
    Error as IdentityError, ROOTS_RELATIVE, RetainedDirectory, RetainedIdentity,
    archived_state_identity::{ArchivedStateIdentityError, RetainedArchivedStateIdentity},
    canonical_state_name,
};

use self::error_classification::{
    archived_identity_exclusion, identity_error_is_structural, runtime_error_is_structural,
};
#[cfg(test)]
use self::runtime_policy::{arm_between_revalidation_passes, arm_runtime_epoch_mismatch};
use self::runtime_policy::{
    between_revalidation_passes, capture_runtime_epoch, require_runtime_epoch, require_runtime_identity,
};

mod error_classification;
mod runtime_policy;

pub(crate) const MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS: usize = 5;
pub(crate) const MAX_ACTIVE_REBLIT_ARCHIVED_BOOT_STATE_ROOTS: usize = 4;
const MAX_STATE_ROOT_WORK: usize = 128;
const MAX_STATE_ROOT_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
const MAX_STATE_ROOT_PATH_COMPONENTS: usize = 128;
#[cfg(test)]
const STATE_ROOT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct StateRootPolicy {
    max_work: usize,
    #[cfg(test)]
    timeout: Duration,
}

impl StateRootPolicy {
    const fn production() -> Self {
        Self {
            max_work: MAX_STATE_ROOT_WORK,
            #[cfg(test)]
            timeout: STATE_ROOT_TIMEOUT,
        }
    }
}

/// One retained set of exact state-root capabilities.
///
/// No method exposes wrapper or roots-parent descriptors. The only descriptor
/// borrow is the already-opened read-only `/usr` root used by later bounded
/// metadata readers. Preparation itself is repeatable; the final client binds
/// this value to its unique in-process repair-attempt capability before any
/// effect authority is released.
pub(crate) struct PreparedActiveReblitBootStateRoots {
    epoch: RuntimeEpoch,
    head: RetainedBootStateRoot,
    roots: RetainedDirectory,
    archived: Vec<RetainedArchivedBootStateRoot>,
    eligible_state_ids: Vec<state::Id>,
    exclusions: Vec<ArchivedBootStateRootExclusion>,
}

/// Descriptor views released only after an epoch-sandwiched exact-name
/// revalidation. Retaining the installation borrow also retains its external
/// cooperating-writer lock; final client composition must additionally retain
/// its unique in-process repair-attempt capability. The `Rc` marker keeps the
/// view on the thread whose mount namespace was authenticated.
pub(crate) struct RevalidatedActiveReblitBootStateRoots<'a> {
    authority: &'a PreparedActiveReblitBootStateRoots,
    _installation: &'a Installation,
    _same_thread: PhantomData<Rc<()>>,
}

struct RetainedBootStateRoot {
    state: state::Id,
    identity: RetainedIdentity,
    runtime: RuntimeTreeIdentity,
}

struct RetainedArchivedBootStateRoot {
    identity: RetainedArchivedStateIdentity,
    runtime: RuntimeTreeIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveReblitBootStateRootKind {
    LiveHead,
    Archived,
}

/// A lifetime-bound read view into one admitted state tree.
pub(crate) struct BoundActiveReblitBootStateRoot<'a> {
    state: state::Id,
    kind: ActiveReblitBootStateRootKind,
    usr: BorrowedFd<'a>,
    _same_thread: PhantomData<Rc<()>>,
}

impl BoundActiveReblitBootStateRoot<'_> {
    pub(crate) fn state_id(&self) -> state::Id {
        self.state
    }

    pub(crate) fn kind(&self) -> ActiveReblitBootStateRootKind {
        self.kind
    }

    pub(crate) fn usr(&self) -> BorrowedFd<'_> {
        self.usr
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedBootStateRootExclusionReason {
    Absent,
    WrapperInexact,
    UsrInexact,
    TreeMarkerInexact,
    StateIdInexact,
    SlotMarkerInexact,
    WrapperLayoutInexact,
    ChangedDuringPreparation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArchivedBootStateRootExclusion {
    state: state::Id,
    reason: ArchivedBootStateRootExclusionReason,
}

impl ArchivedBootStateRootExclusion {
    pub(crate) fn state_id(&self) -> state::Id {
        self.state
    }

    pub(crate) fn reason(&self) -> ArchivedBootStateRootExclusionReason {
        self.reason
    }
}

impl PreparedActiveReblitBootStateRoots {
    #[cfg(test)]
    pub(crate) fn prepare(
        installation: &Installation,
        selected_head_usr: &std::fs::File,
        expected_head: state::Id,
        projected_state_ids: &[state::Id],
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        let policy = StateRootPolicy::production();
        let deadline = state_root_deadline(policy.timeout, &installation.root)?;
        Self::prepare_with_policy_until_and_checkpoint(
            installation,
            selected_head_usr,
            expected_head,
            projected_state_ids,
            policy,
            deadline,
            || {},
        )
    }

    /// Prepare without replacing the caller-owned absolute deadline.
    pub(crate) fn prepare_until(
        installation: &Installation,
        selected_head_usr: &std::fs::File,
        expected_head: state::Id,
        projected_state_ids: &[state::Id],
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        Self::prepare_with_policy_until_and_checkpoint(
            installation,
            selected_head_usr,
            expected_head,
            projected_state_ids,
            StateRootPolicy::production(),
            deadline,
            || {},
        )
    }

    fn prepare_with_policy_until_and_checkpoint<F>(
        installation: &Installation,
        selected_head_usr: &std::fs::File,
        expected_head: state::Id,
        projected_state_ids: &[state::Id],
        policy: StateRootPolicy,
        deadline: Instant,
        before_terminal_deadline: F,
    ) -> Result<Self, ActiveReblitBootStateRootsError>
    where
        F: FnOnce(),
    {
        let budget = StateRootBudget::new_until(policy, deadline, &installation.root)?;
        Self::prepare_with_budget_and_checkpoint(
            installation,
            selected_head_usr,
            expected_head,
            projected_state_ids,
            budget,
            before_terminal_deadline,
        )
    }

    fn prepare_with_budget_and_checkpoint<F>(
        installation: &Installation,
        selected_head_usr: &std::fs::File,
        expected_head: state::Id,
        projected_state_ids: &[state::Id],
        mut budget: StateRootBudget,
        before_terminal_deadline: F,
    ) -> Result<Self, ActiveReblitBootStateRootsError>
    where
        F: FnOnce(),
    {
        validate_projection(expected_head, projected_state_ids)?;
        let head_path = installation.root.join("usr");
        let roots_path = installation.root_path("");
        validate_diagnostic_path(&head_path)?;
        validate_diagnostic_path(&roots_path)?;

        budget.step(&installation.root)?;
        installation.revalidate_root_directory_until(budget.deadline)?;
        let epoch = capture_runtime_epoch(&installation.root, &mut budget)?;
        let head = retain_head(selected_head_usr, expected_head, &head_path, &mut budget)?;

        budget.step(&roots_path)?;
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())
            .map_err(|source| ActiveReblitBootStateRootsError::Roots {
                path: roots_path.clone(),
                source,
            })?;

        let mut archived = Vec::with_capacity(projected_state_ids.len().saturating_sub(1));
        let mut exclusions = Vec::with_capacity(projected_state_ids.len().saturating_sub(1));
        for &state in &projected_state_ids[1..] {
            let canonical_name = canonical_state_name(state).map_err(|source| {
                ActiveReblitBootStateRootsError::InvalidProjectedState {
                    state: i32::from(state),
                    source,
                }
            })?;
            let wrapper_path = roots.path.join(canonical_name.to_string_lossy().as_ref());
            validate_diagnostic_path(&wrapper_path)?;
            budget.step(&wrapper_path)?;
            match roots.child_name_exists(&canonical_name, wrapper_path.clone()) {
                Ok(false) => exclusions.push(exclusion(state, ArchivedBootStateRootExclusionReason::Absent)),
                Ok(true) => match retain_archived(&roots, state, &wrapper_path, &mut budget) {
                    Ok(root) => archived.push(root),
                    Err(ArchivedPreparationFailure::Excluded(reason)) => exclusions.push(exclusion(state, reason)),
                    Err(ArchivedPreparationFailure::Identity(source)) => {
                        return Err(ActiveReblitBootStateRootsError::ArchivedIdentity {
                            state: i32::from(state),
                            source,
                        });
                    }
                    Err(ArchivedPreparationFailure::Runtime(source)) => {
                        return Err(ActiveReblitBootStateRootsError::RuntimeIdentity {
                            state: i32::from(state),
                            path: wrapper_path.join("usr"),
                            source,
                        });
                    }
                    Err(ArchivedPreparationFailure::Boundary(source)) => return Err(source),
                },
                Err(source) if identity_error_is_structural(&source) => {
                    exclusions.push(exclusion(state, ArchivedBootStateRootExclusionReason::WrapperInexact));
                }
                Err(source) => {
                    return Err(ActiveReblitBootStateRootsError::ArchiveProbe {
                        state: i32::from(state),
                        path: wrapper_path,
                        source,
                    });
                }
            }
        }

        let mut eligible_state_ids = Vec::with_capacity(1 + archived.len());
        eligible_state_ids.push(expected_head);
        eligible_state_ids.extend(archived.iter().map(|root| root.identity.state));
        require_unique_tree_tokens(&head, &archived, &mut budget)?;
        let prepared = Self {
            epoch,
            head,
            roots,
            archived,
            eligible_state_ids,
            exclusions,
        };
        prepared.revalidate_with_budget(installation, &mut budget)?;
        before_terminal_deadline();
        budget.require_deadline(&installation.root)?;
        Ok(prepared)
    }

    #[cfg(test)]
    pub(crate) fn revalidate<'authority>(
        &'authority self,
        installation: &'authority Installation,
    ) -> Result<RevalidatedActiveReblitBootStateRoots<'authority>, ActiveReblitBootStateRootsError> {
        let policy = StateRootPolicy::production();
        let deadline = state_root_deadline(policy.timeout, &installation.root)?;
        self.revalidate_until(installation, deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(crate) fn revalidate_until<'authority>(
        &'authority self,
        installation: &'authority Installation,
        deadline: Instant,
    ) -> Result<RevalidatedActiveReblitBootStateRoots<'authority>, ActiveReblitBootStateRootsError> {
        let budget = StateRootBudget::new_until(StateRootPolicy::production(), deadline, &installation.root)?;
        self.revalidate_with_budget_and_checkpoint(installation, budget, || {})
    }

    fn revalidate_with_budget_and_checkpoint<'authority, F>(
        &'authority self,
        installation: &'authority Installation,
        mut budget: StateRootBudget,
        before_terminal_deadline: F,
    ) -> Result<RevalidatedActiveReblitBootStateRoots<'authority>, ActiveReblitBootStateRootsError>
    where
        F: FnOnce(),
    {
        self.revalidate_with_budget(installation, &mut budget)?;
        let view = RevalidatedActiveReblitBootStateRoots {
            authority: self,
            _installation: installation,
            _same_thread: PhantomData,
        };
        before_terminal_deadline();
        budget.require_deadline(&installation.root)?;
        Ok(view)
    }

    fn revalidate_with_budget(
        &self,
        installation: &Installation,
        budget: &mut StateRootBudget,
    ) -> Result<(), ActiveReblitBootStateRootsError> {
        let epoch_before = capture_runtime_epoch(&installation.root, budget)?;
        require_runtime_epoch(&self.epoch, &epoch_before, &installation.root)?;
        self.revalidate_exact_names_once(installation, budget)?;
        // A second deterministic pass catches an earlier child being renamed
        // after its first turn. The view returned by `revalidate` additionally
        // retains the Installation and therefore its cooperating-writer lock.
        between_revalidation_passes();
        self.revalidate_exact_names_once(installation, budget)?;
        let epoch_after = capture_runtime_epoch(&installation.root, budget)?;
        require_runtime_epoch(&epoch_before, &epoch_after, &installation.root)?;
        require_runtime_epoch(&self.epoch, &epoch_after, &installation.root)?;
        budget.require_deadline(&installation.root)
    }

    fn revalidate_exact_names_once(
        &self,
        installation: &Installation,
        budget: &mut StateRootBudget,
    ) -> Result<(), ActiveReblitBootStateRootsError> {
        let head_path = installation.root.join("usr");
        let roots_path = installation.root_path("");

        budget.step(&installation.root)?;
        installation.revalidate_root_directory_until(budget.deadline)?;
        budget.step(&head_path)?;
        let named_head = open_named_head(installation, self.head.state, &head_path)?;
        budget.require_deadline(&head_path)?;
        self.head
            .identity
            .verify_store_with_state_id(&named_head)
            .map_err(|source| ActiveReblitBootStateRootsError::HeadIdentity {
                state: i32::from(self.head.state),
                path: head_path.clone(),
                source,
            })?;
        budget.step(&head_path)?;
        require_runtime_identity(
            self.head.state,
            &head_path,
            &self.head.runtime,
            named_head.retained_directory(),
        )?;
        budget.require_deadline(&head_path)?;

        budget.step(&roots_path)?;
        self.roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
            .map_err(|source| ActiveReblitBootStateRootsError::Roots {
                path: roots_path.clone(),
                source,
            })?;
        budget.require_deadline(&roots_path)?;
        for archived in &self.archived {
            budget.step(&archived.identity.wrapper.path)?;
            archived.identity.revalidate_named(&self.roots).map_err(|source| {
                ActiveReblitBootStateRootsError::ArchivedChanged {
                    state: i32::from(archived.identity.state),
                    source,
                }
            })?;
            budget.step(&archived.identity.wrapper.path)?;
            require_runtime_identity(
                archived.identity.state,
                &archived.identity.wrapper.path.join("usr"),
                &archived.runtime,
                archived.identity.usr(),
            )?;
            budget.require_deadline(&archived.identity.wrapper.path)?;
        }
        budget.step(&installation.root)?;
        installation.revalidate_root_directory_until(budget.deadline)?;
        budget.step(&roots_path)?;
        self.roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
            .map_err(|source| ActiveReblitBootStateRootsError::Roots {
                path: installation.root_path(""),
                source,
            })?;
        budget.require_deadline(&installation.root)
    }

    pub(crate) fn eligible_state_ids(&self) -> &[state::Id] {
        &self.eligible_state_ids
    }

    pub(crate) fn exclusions(&self) -> &[ArchivedBootStateRootExclusion] {
        &self.exclusions
    }

    #[cfg(test)]
    fn prepare_for_test(
        installation: &Installation,
        selected_head_usr: &std::fs::File,
        expected_head: state::Id,
        projected_state_ids: &[state::Id],
        max_work: usize,
        timeout: Duration,
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        let policy = StateRootPolicy { max_work, timeout };
        let deadline = state_root_deadline(policy.timeout, &installation.root)?;
        Self::prepare_with_policy_until_and_checkpoint(
            installation,
            selected_head_usr,
            expected_head,
            projected_state_ids,
            policy,
            deadline,
            || {},
        )
    }
}

impl RevalidatedActiveReblitBootStateRoots<'_> {
    pub(crate) fn is_bound_to_installation(&self, installation: &Installation) -> bool {
        std::ptr::eq(self._installation, installation)
    }

    pub(crate) fn head(&self) -> BoundActiveReblitBootStateRoot<'_> {
        bound_root(
            self.authority.head.state,
            ActiveReblitBootStateRootKind::LiveHead,
            self.authority.head.identity.store.retained_directory(),
        )
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = BoundActiveReblitBootStateRoot<'_>> {
        std::iter::once(self.head()).chain(self.authority.archived.iter().map(|root| {
            bound_root(
                root.identity.state,
                ActiveReblitBootStateRootKind::Archived,
                root.identity.usr(),
            )
        }))
    }

    pub(crate) fn eligible_state_ids(&self) -> &[state::Id] {
        self.authority.eligible_state_ids()
    }

    pub(crate) fn exclusions(&self) -> &[ArchivedBootStateRootExclusion] {
        self.authority.exclusions()
    }
}

fn require_unique_tree_tokens(
    head: &RetainedBootStateRoot,
    archived: &[RetainedArchivedBootStateRoot],
    budget: &mut StateRootBudget,
) -> Result<(), ActiveReblitBootStateRootsError> {
    for (index, current) in archived.iter().enumerate() {
        budget.step(&current.identity.wrapper.path)?;
        if current.identity.identity.marker.token() == head.identity.marker.token() {
            return Err(ActiveReblitBootStateRootsError::DuplicateTreeToken {
                first_state: i32::from(head.state),
                second_state: i32::from(current.identity.state),
                token: head.identity.marker.token().as_str().to_owned(),
            });
        }
        for previous in &archived[..index] {
            budget.step(&current.identity.wrapper.path)?;
            if current.identity.identity.marker.token() == previous.identity.identity.marker.token() {
                return Err(ActiveReblitBootStateRootsError::DuplicateTreeToken {
                    first_state: i32::from(previous.identity.state),
                    second_state: i32::from(current.identity.state),
                    token: current.identity.identity.marker.token().as_str().to_owned(),
                });
            }
        }
    }
    budget.require_deadline(head.identity.store.display_path())
}

fn retain_head(
    selected_head_usr: &std::fs::File,
    expected_head: state::Id,
    path: &Path,
    budget: &mut StateRootBudget,
) -> Result<RetainedBootStateRoot, ActiveReblitBootStateRootsError> {
    budget.step(path)?;
    let store = TreeMarkerStore::open(selected_head_usr, path)
        .map_err(IdentityError::from)
        .map_err(|source| ActiveReblitBootStateRootsError::HeadIdentity {
            state: i32::from(expected_head),
            path: path.to_owned(),
            source,
        })?;
    budget.step(path)?;
    let marker = store
        .read_for_recovery()
        .map_err(IdentityError::from)
        .map_err(|source| ActiveReblitBootStateRootsError::HeadIdentity {
            state: i32::from(expected_head),
            path: path.to_owned(),
            source,
        })?;
    budget.step(path)?;
    let identity = RetainedIdentity::with_marker(store, marker, Some(expected_head)).map_err(|source| {
        ActiveReblitBootStateRootsError::HeadIdentity {
            state: i32::from(expected_head),
            path: path.to_owned(),
            source,
        }
    })?;
    budget.step(path)?;
    identity.verify_store_with_state_id(&identity.store).map_err(|source| {
        ActiveReblitBootStateRootsError::HeadIdentity {
            state: i32::from(expected_head),
            path: path.to_owned(),
            source,
        }
    })?;
    budget.step(path)?;
    let runtime = RuntimeTreeIdentity::capture_directory(identity.store.retained_directory()).map_err(|source| {
        ActiveReblitBootStateRootsError::RuntimeIdentity {
            state: i32::from(expected_head),
            path: path.to_owned(),
            source,
        }
    })?;
    budget.require_deadline(path)?;
    Ok(RetainedBootStateRoot {
        state: expected_head,
        identity,
        runtime,
    })
}

fn retain_archived(
    roots: &RetainedDirectory,
    state: state::Id,
    path: &Path,
    budget: &mut StateRootBudget,
) -> Result<RetainedArchivedBootStateRoot, ArchivedPreparationFailure> {
    budget.step(path).map_err(ArchivedPreparationFailure::Boundary)?;
    let identity = match RetainedArchivedStateIdentity::retain(roots, state) {
        Ok(identity) => identity,
        Err(source) => {
            return match archived_identity_exclusion(&source) {
                Some(reason) => Err(ArchivedPreparationFailure::Excluded(reason)),
                None => Err(ArchivedPreparationFailure::Identity(source)),
            };
        }
    };
    budget
        .require_deadline(path)
        .map_err(ArchivedPreparationFailure::Boundary)?;
    budget.step(path).map_err(ArchivedPreparationFailure::Boundary)?;
    let runtime = match RuntimeTreeIdentity::capture_directory(identity.usr()) {
        Ok(runtime) => runtime,
        Err(source) if runtime_error_is_structural(&source) => {
            return Err(ArchivedPreparationFailure::Excluded(
                ArchivedBootStateRootExclusionReason::ChangedDuringPreparation,
            ));
        }
        Err(source) => return Err(ArchivedPreparationFailure::Runtime(source)),
    };
    budget
        .require_deadline(path)
        .map_err(ArchivedPreparationFailure::Boundary)?;
    budget.step(path).map_err(ArchivedPreparationFailure::Boundary)?;
    if let Err(source) = identity.revalidate_named(roots) {
        return match archived_identity_exclusion(&source) {
            Some(_) => Err(ArchivedPreparationFailure::Excluded(
                ArchivedBootStateRootExclusionReason::ChangedDuringPreparation,
            )),
            None => Err(ArchivedPreparationFailure::Identity(source)),
        };
    }
    budget
        .require_deadline(path)
        .map_err(ArchivedPreparationFailure::Boundary)?;
    budget.step(path).map_err(ArchivedPreparationFailure::Boundary)?;
    match RuntimeTreeIdentity::capture_directory(identity.usr()) {
        Ok(current) if current == runtime => {
            budget
                .require_deadline(path)
                .map_err(ArchivedPreparationFailure::Boundary)?;
            Ok(RetainedArchivedBootStateRoot { identity, runtime })
        }
        Ok(_) => Err(ArchivedPreparationFailure::Excluded(
            ArchivedBootStateRootExclusionReason::ChangedDuringPreparation,
        )),
        Err(source) if runtime_error_is_structural(&source) => Err(ArchivedPreparationFailure::Excluded(
            ArchivedBootStateRootExclusionReason::ChangedDuringPreparation,
        )),
        Err(source) => Err(ArchivedPreparationFailure::Runtime(source)),
    }
}

enum ArchivedPreparationFailure {
    Excluded(ArchivedBootStateRootExclusionReason),
    Identity(ArchivedStateIdentityError),
    Runtime(RuntimeEvidenceError),
    Boundary(ActiveReblitBootStateRootsError),
}

fn open_named_head(
    installation: &Installation,
    state: state::Id,
    path: &Path,
) -> Result<TreeMarkerStore, ActiveReblitBootStateRootsError> {
    let usr = linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        c"usr",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        linux_fs::controlled_resolution(),
    )
    .map_err(|source| ActiveReblitBootStateRootsError::HeadOpen {
        path: path.to_owned(),
        source,
    })?;
    TreeMarkerStore::open(&usr, path)
        .map_err(IdentityError::from)
        .map_err(|source| ActiveReblitBootStateRootsError::HeadIdentity {
            state: i32::from(state),
            path: path.to_owned(),
            source,
        })
}

fn validate_projection(
    expected_head: state::Id,
    projected_state_ids: &[state::Id],
) -> Result<(), ActiveReblitBootStateRootsError> {
    if projected_state_ids.is_empty() || projected_state_ids.len() > MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS {
        return Err(ActiveReblitBootStateRootsError::StateCount {
            actual: projected_state_ids.len(),
            limit: MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS,
        });
    }
    if projected_state_ids.first() != Some(&expected_head) {
        return Err(ActiveReblitBootStateRootsError::HeadOrder {
            expected: i32::from(expected_head),
            actual: projected_state_ids.first().copied().map(i32::from),
        });
    }
    let mut unique = BTreeSet::new();
    for &state in projected_state_ids {
        if i32::from(state) <= 0 {
            return Err(ActiveReblitBootStateRootsError::NonPositiveState {
                state: i32::from(state),
            });
        }
        if !unique.insert(state) {
            return Err(ActiveReblitBootStateRootsError::DuplicateState {
                state: i32::from(state),
            });
        }
    }
    if projected_state_ids.len().saturating_sub(1) > MAX_ACTIVE_REBLIT_ARCHIVED_BOOT_STATE_ROOTS {
        return Err(ActiveReblitBootStateRootsError::StateCount {
            actual: projected_state_ids.len(),
            limit: MAX_ACTIVE_REBLIT_BOOT_STATE_ROOTS,
        });
    }
    Ok(())
}

fn validate_diagnostic_path(path: &Path) -> Result<(), ActiveReblitBootStateRootsError> {
    let bytes = path.as_os_str().as_bytes().len();
    let components = path.components().count();
    if bytes > MAX_STATE_ROOT_PATH_BYTES {
        return Err(ActiveReblitBootStateRootsError::PathBytes {
            path: path.to_owned(),
            actual: bytes,
            limit: MAX_STATE_ROOT_PATH_BYTES,
        });
    }
    if components > MAX_STATE_ROOT_PATH_COMPONENTS {
        return Err(ActiveReblitBootStateRootsError::PathComponents {
            path: path.to_owned(),
            actual: components,
            limit: MAX_STATE_ROOT_PATH_COMPONENTS,
        });
    }
    Ok(())
}

fn exclusion(state: state::Id, reason: ArchivedBootStateRootExclusionReason) -> ArchivedBootStateRootExclusion {
    ArchivedBootStateRootExclusion { state, reason }
}

fn bound_root<'a>(
    state: state::Id,
    kind: ActiveReblitBootStateRootKind,
    usr: &'a std::fs::File,
) -> BoundActiveReblitBootStateRoot<'a> {
    BoundActiveReblitBootStateRoot {
        state,
        kind,
        usr: usr.as_fd(),
        _same_thread: PhantomData,
    }
}

/// Logical admission budget. One unit brackets one bounded primitive or one
/// bounded composite such as archived-wrapper authentication. Independent
/// state-count, directory-entry, byte and retry limits bound program-controlled
/// work inside those composites. The deadline is checked around each composite;
/// it is a fail-closed elapsed-time gate, not a preemptive syscall timeout or a
/// literal syscall counter.
struct StateRootBudget {
    max_work: usize,
    work: usize,
    deadline: Instant,
    #[cfg(test)]
    clock: Option<Box<dyn Fn() -> Instant>>,
}

impl StateRootBudget {
    fn new_until(
        policy: StateRootPolicy,
        deadline: Instant,
        path: &Path,
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        Self::new_until_at(policy, deadline, path, Instant::now())
    }

    fn new_until_at(
        policy: StateRootPolicy,
        deadline: Instant,
        path: &Path,
        admitted_at: Instant,
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        let budget = Self {
            max_work: policy.max_work,
            work: 0,
            deadline,
            #[cfg(test)]
            clock: None,
        };
        budget.require_deadline_at(path, admitted_at)?;
        Ok(budget)
    }

    #[cfg(test)]
    fn new_until_with_clock(
        policy: StateRootPolicy,
        deadline: Instant,
        path: &Path,
        clock: impl Fn() -> Instant + 'static,
    ) -> Result<Self, ActiveReblitBootStateRootsError> {
        let admitted_at = clock();
        let mut budget = Self::new_until_at(policy, deadline, path, admitted_at)?;
        budget.clock = Some(Box::new(clock));
        Ok(budget)
    }

    fn step(&mut self, path: &Path) -> Result<(), ActiveReblitBootStateRootsError> {
        self.require_deadline(path)?;
        self.work = self
            .work
            .checked_add(1)
            .ok_or_else(|| ActiveReblitBootStateRootsError::WorkLimit {
                path: path.to_owned(),
                limit: self.max_work,
            })?;
        if self.work > self.max_work {
            return Err(ActiveReblitBootStateRootsError::WorkLimit {
                path: path.to_owned(),
                limit: self.max_work,
            });
        }
        Ok(())
    }

    fn require_deadline(&self, path: &Path) -> Result<(), ActiveReblitBootStateRootsError> {
        self.require_deadline_at(path, self.now())
    }

    fn require_deadline_at(&self, path: &Path, now: Instant) -> Result<(), ActiveReblitBootStateRootsError> {
        if now >= self.deadline {
            Err(ActiveReblitBootStateRootsError::Deadline { path: path.to_owned() })
        } else {
            Ok(())
        }
    }

    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.clock {
            return clock();
        }
        Instant::now()
    }
}

#[cfg(test)]
fn state_root_deadline(timeout: Duration, path: &Path) -> Result<Instant, ActiveReblitBootStateRootsError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| ActiveReblitBootStateRootsError::Deadline { path: path.to_owned() })
}

#[derive(Debug, Error)]
pub(crate) enum ActiveReblitBootStateRootsError {
    #[error("ActiveReblit boot roots require 1..={limit} projected states, found {actual}")]
    StateCount { actual: usize, limit: usize },
    #[error("ActiveReblit boot roots require head {expected} first, found {actual:?}")]
    HeadOrder { expected: i32, actual: Option<i32> },
    #[error("ActiveReblit boot roots require positive state IDs, found {state}")]
    NonPositiveState { state: i32 },
    #[error("ActiveReblit boot roots contain duplicate state {state}")]
    DuplicateState { state: i32 },
    #[error("validate projected ActiveReblit boot state {state}")]
    InvalidProjectedState {
        state: i32,
        #[source]
        source: IdentityError,
    },
    #[error("ActiveReblit boot-root diagnostic path `{}` has {actual} bytes, exceeding {limit}", path.display())]
    PathBytes { path: PathBuf, actual: usize, limit: usize },
    #[error(
        "ActiveReblit boot-root diagnostic path `{}` has {actual} components, exceeding {limit}",
        path.display()
    )]
    PathComponents { path: PathBuf, actual: usize, limit: usize },
    #[error("revalidate retained installation root for ActiveReblit boot roots")]
    Installation(#[from] crate::installation::Error),
    #[error("open mandatory live ActiveReblit head at `{}`", path.display())]
    HeadOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("authenticate mandatory live ActiveReblit head state {state} at `{}`", path.display())]
    HeadIdentity {
        state: i32,
        path: PathBuf,
        #[source]
        source: IdentityError,
    },
    #[error("retain or revalidate ActiveReblit archived-state parent `{}`", path.display())]
    Roots {
        path: PathBuf,
        #[source]
        source: IdentityError,
    },
    #[error("probe projected ActiveReblit archived state {state} at `{}`", path.display())]
    ArchiveProbe {
        state: i32,
        path: PathBuf,
        #[source]
        source: IdentityError,
    },
    #[error("authenticate projected ActiveReblit archived state {state}")]
    ArchivedIdentity {
        state: i32,
        #[source]
        source: ArchivedStateIdentityError,
    },
    #[error("admitted ActiveReblit archived state {state} changed")]
    ArchivedChanged {
        state: i32,
        #[source]
        source: ArchivedStateIdentityError,
    },
    #[error("capture ActiveReblit boot-root runtime epoch at `{}`", path.display())]
    RuntimeEpoch {
        path: PathBuf,
        #[source]
        source: RuntimeEvidenceError,
    },
    #[error("ActiveReblit boot-root runtime epoch changed at `{}`", path.display())]
    RuntimeEpochChanged { path: PathBuf },
    #[error("capture runtime identity for ActiveReblit state {state} at `{}`", path.display())]
    RuntimeIdentity {
        state: i32,
        path: PathBuf,
        #[source]
        source: RuntimeEvidenceError,
    },
    #[error("runtime identity for ActiveReblit state {state} changed at `{}`", path.display())]
    RuntimeIdentityChanged { state: i32, path: PathBuf },
    #[error("ActiveReblit boot states {first_state} and {second_state} carry duplicate permanent tree token {token}")]
    DuplicateTreeToken {
        first_state: i32,
        second_state: i32,
        token: String,
    },
    #[error("ActiveReblit boot-root preparation exceeded {limit} work units at `{}`", path.display())]
    WorkLimit { path: PathBuf, limit: usize },
    #[error("ActiveReblit boot-root preparation exceeded its deadline at `{}`", path.display())]
    Deadline { path: PathBuf },
}

#[cfg(test)]
#[path = "active_reblit_boot_state_roots_tests.rs"]
mod tests;
