//! Journal-authorized, no-replace publication of an absent `.stateID`.

use std::{
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use thiserror::Error;

use super::{
    RetainedTreeStateId, STATE_ID_MODE, STATE_ID_NAME, STATE_ID_TEMPORARY_NAME, StateIdWitness,
    require_state_id_contents, state_id_witness,
};
use crate::{
    linux_fs::{
        chmod_path_descriptor, controlled_resolution, openat2_file, renameat2_noreplace_once, require_no_access_acl,
        require_no_default_acl,
    },
    state,
    tree_marker::TreeMarkerStore,
};

const TEMPORARY_MODE: u32 = 0o600;
const MAX_IO_ATTEMPTS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Reconciled current namespace location of the exact publication inode.
///
/// `Published` does not claim that the candidate-directory sync succeeded or
/// that `CandidatePrepared` is durable; those are separate later boundaries.
pub(in crate::transition_identity) enum StateIdPublicationOutcome {
    NotPublished,
    Published,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("candidate state-ID publication outcome is {outcome:?}")]
pub(crate) struct StateIdPublicationFailure {
    outcome: StateIdPublicationOutcome,
    #[source]
    source: PublicationError,
}

impl StateIdPublicationFailure {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::transition_identity) fn outcome(&self) -> StateIdPublicationOutcome {
        self.outcome
    }

    fn new(outcome: StateIdPublicationOutcome, source: PublicationError) -> Self {
        Self { outcome, source }
    }
}

#[derive(Debug, Error)]
enum PublicationError {
    #[error("authenticate candidate state-ID publication")]
    Identity(#[source] Box<super::super::Error>),
    #[error("{operation} at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("state-ID namespace mismatch: canonical={canonical}, temporary={temporary}")]
    Namespace {
        canonical: &'static str,
        temporary: &'static str,
    },
    #[error("state-ID publication failed ({primary}) and exact temporary retirement failed ({cleanup})")]
    Cleanup {
        primary: Box<PublicationError>,
        #[source]
        cleanup: Box<PublicationError>,
    },
    #[error("state-ID publication failed ({primary}) and outcome reconciliation failed ({reconciliation})")]
    Reconciliation {
        primary: Box<PublicationError>,
        #[source]
        reconciliation: Box<PublicationError>,
    },
    #[cfg(test)]
    #[error("injected state-ID publication fault at {point:?}")]
    Injected { point: StateIdPublicationFaultPoint },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::transition_identity) enum StateIdPublicationFaultPoint {
    TemporarySync,
    BeforeRename,
    DirectorySync,
    FinalRevalidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

impl From<StateIdWitness> for InodeIdentity {
    fn from(value: StateIdWitness) -> Self {
        Self {
            device: value.device,
            inode: value.inode,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameState {
    Absent,
    Exact,
    Foreign(InodeIdentity),
    OpaqueForeign(i32),
}

impl NameState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Exact => "exact",
            Self::Foreign(_) => "foreign",
            Self::OpaqueForeign(_) => "opaque-foreign",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamespaceState {
    canonical: NameState,
    temporary: NameState,
}

impl NamespaceState {
    fn outcome(self) -> StateIdPublicationOutcome {
        match (self.canonical, self.temporary) {
            (NameState::Exact, NameState::Absent) => StateIdPublicationOutcome::Published,
            (canonical, NameState::Exact) if canonical != NameState::Exact => StateIdPublicationOutcome::NotPublished,
            _ => StateIdPublicationOutcome::Ambiguous,
        }
    }

    fn error(self) -> PublicationError {
        PublicationError::Namespace {
            canonical: self.canonical.as_str(),
            temporary: self.temporary.as_str(),
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_RENAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static FAULT: std::cell::RefCell<Option<StateIdPublicationFaultPoint>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::transition_identity) fn arm_before_state_id_publish(hook: impl FnOnce() + 'static) {
    BEFORE_PUBLISH.with(|slot| assert!(slot.replace(Some(Box::new(hook))).is_none()));
}

#[cfg(test)]
pub(in crate::transition_identity) fn arm_after_state_id_rename(hook: impl FnOnce() + 'static) {
    AFTER_RENAME.with(|slot| assert!(slot.replace(Some(Box::new(hook))).is_none()));
}

#[cfg(test)]
pub(in crate::transition_identity) fn arm_state_id_publication_fault(point: StateIdPublicationFaultPoint) {
    FAULT.with(|slot| assert!(slot.replace(Some(point)).is_none()));
}

#[cfg(test)]
pub(in crate::transition_identity) fn assert_state_id_publication_fault_consumed() {
    FAULT.with(|slot| assert!(slot.borrow().is_none(), "state-ID publication fault was not consumed"));
}

#[cfg(test)]
fn before_publish() {
    BEFORE_PUBLISH.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_publish() {}

#[cfg(test)]
fn after_rename() {
    AFTER_RENAME.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_rename() {}

#[cfg(test)]
fn checkpoint(point: StateIdPublicationFaultPoint) -> Result<(), PublicationError> {
    FAULT.with(|slot| {
        if slot.borrow().as_ref() == Some(&point) {
            slot.replace(None);
            Err(PublicationError::Injected { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn checkpoint(point: StateIdPublicationFaultPoint) -> Result<(), PublicationError> {
    let _ = point;
    Ok(())
}

impl RetainedTreeStateId {
    /// Require both fixed state-ID names to be absent beneath the exact
    /// retained candidate. Every occupant kind is rejected without mutation.
    pub(in crate::transition_identity) fn require_absent(store: &TreeMarkerStore) -> Result<(), super::super::Error> {
        store.revalidate_directory()?;
        for (name, display) in [
            (STATE_ID_NAME, ".stateID"),
            (STATE_ID_TEMPORARY_NAME, ".cast-state-id.tmp"),
        ] {
            let path = store.display_path().join(display);
            let found = probe_name(store, name, None).map_err(|source| {
                super::super::live_usr_io(
                    "probe candidate state-ID name",
                    &path,
                    io::Error::other(source.to_string()),
                )
            })?;
            if found != NameState::Absent {
                return Err(super::super::live_usr_io(
                    "require absent candidate state-ID name",
                    &path,
                    io::Error::new(io::ErrorKind::AlreadyExists, format!("{display} already exists")),
                ));
            }
        }
        store.revalidate_directory()?;
        Ok(())
    }

    /// Publish an absent state ID only after the journal authorizes candidate
    /// decoration. This is shared by fresh allocation and active reblit; the
    /// temporary is exclusive, every rename is one-shot, and every error is
    /// reconciled before its outcome is reported.
    pub(in crate::transition_identity) fn publish_absent(
        store: &TreeMarkerStore,
        state: state::Id,
    ) -> Result<Self, StateIdPublicationFailure> {
        super::super::canonical_state_name(state)
            .map_err(|source| failure_identity(StateIdPublicationOutcome::NotPublished, source))?;
        Self::require_absent(store)
            .map_err(|source| failure_identity(StateIdPublicationOutcome::NotPublished, source))?;
        require_directory(store)
            .map_err(|source| StateIdPublicationFailure::new(StateIdPublicationOutcome::NotPublished, source))?;

        let expected = state.to_string().into_bytes();
        let temporary_path = store.display_path().join(".cast-state-id.tmp");
        let canonical_path = store.display_path().join(".stateID");
        let temporary = openat2_file(
            store.retained_directory().as_raw_fd(),
            STATE_ID_TEMPORARY_NAME,
            nix::libc::O_RDWR
                | nix::libc::O_CLOEXEC
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            TEMPORARY_MODE,
            controlled_resolution(),
        )
        .map_err(|source| {
            failure_io(
                StateIdPublicationOutcome::NotPublished,
                "create exclusive state-ID temporary",
                &temporary_path,
                source,
            )
        })?;
        let identity = inode_identity(&temporary).map_err(|source| {
            failure_io(
                StateIdPublicationOutcome::Ambiguous,
                "inspect new state-ID temporary",
                &temporary_path,
                source,
            )
        })?;

        let prepared = (|| -> Result<StateIdWitness, PublicationError> {
            chmod_path_descriptor(&temporary, TEMPORARY_MODE)
                .map_err(|source| io_error("normalize state-ID temporary mode", &temporary_path, source))?;
            require_file(&temporary, &temporary_path, 0, TEMPORARY_MODE)
                .map_err(|source| io_error("authenticate empty state-ID temporary", &temporary_path, source))?;
            write_all_at(&temporary, &expected)
                .map_err(|source| io_error("write complete state-ID temporary", &temporary_path, source))?;
            checkpoint(StateIdPublicationFaultPoint::TemporarySync)?;
            temporary
                .sync_all()
                .map_err(|source| io_error("sync state-ID temporary contents", &temporary_path, source))?;
            chmod_path_descriptor(&temporary, STATE_ID_MODE)
                .map_err(|source| io_error("set canonical state-ID mode", &temporary_path, source))?;
            temporary
                .sync_all()
                .map_err(|source| io_error("sync canonical state-ID metadata", &temporary_path, source))?;
            require_file(&temporary, &temporary_path, expected.len() as u64, STATE_ID_MODE)
                .map_err(|source| io_error("authenticate complete state-ID temporary", &temporary_path, source))?;
            let witness = state_id_witness(&temporary, &temporary_path, expected.len() as u64)
                .map_err(|source| PublicationError::Identity(Box::new(source)))?;
            require_state_id_contents(&temporary, &temporary_path, &expected, state)
                .map_err(|source| PublicationError::Identity(Box::new(source)))?;

            before_publish();
            require_directory(store)?;
            let namespace = inspect_namespace(store, identity)?;
            if namespace.canonical != NameState::Absent || namespace.temporary != NameState::Exact {
                return Err(namespace.error());
            }
            require_exact_file(&temporary, witness, &temporary_path, &expected, state)?;
            checkpoint(StateIdPublicationFaultPoint::BeforeRename)?;
            Ok(witness)
        })();
        let pre_rename_witness = match prepared {
            Ok(witness) => witness,
            Err(primary) => return Err(cleanup_failure(store, &temporary, identity, primary)),
        };

        let rename = renameat2_noreplace_once(
            store.retained_directory(),
            STATE_ID_TEMPORARY_NAME,
            store.retained_directory(),
            STATE_ID_NAME,
        );
        after_rename();
        let namespace = inspect_namespace(store, identity)
            .map_err(|source| StateIdPublicationFailure::new(StateIdPublicationOutcome::Ambiguous, source))?;
        match namespace.outcome() {
            StateIdPublicationOutcome::Published => {}
            StateIdPublicationOutcome::NotPublished => {
                let primary = match rename {
                    Ok(()) => namespace.error(),
                    Err(source) => io_error("publish state ID without replacement", &canonical_path, source),
                };
                return Err(cleanup_failure(store, &temporary, identity, primary));
            }
            StateIdPublicationOutcome::Ambiguous => {
                return Err(StateIdPublicationFailure::new(
                    StateIdPublicationOutcome::Ambiguous,
                    namespace.error(),
                ));
            }
        }

        if let Err(source) = checkpoint(StateIdPublicationFaultPoint::DirectorySync) {
            return Err(reconcile_post_rename_failure(store, identity, source));
        }
        if let Err(source) = store.retained_directory().sync_all() {
            return Err(reconcile_post_rename_failure(
                store,
                identity,
                io_error(
                    "sync candidate directory after state-ID publication",
                    store.display_path(),
                    source,
                ),
            ));
        }
        if let Err(source) = require_directory(store) {
            return Err(reconcile_post_rename_failure(store, identity, source));
        }
        let witness = match state_id_witness(&temporary, &canonical_path, expected.len() as u64) {
            Ok(witness) => witness,
            Err(source) => {
                return Err(reconcile_post_rename_failure(
                    store,
                    identity,
                    PublicationError::Identity(Box::new(source)),
                ));
            }
        };
        if InodeIdentity::from(witness) != InodeIdentity::from(pre_rename_witness) {
            return Err(reconcile_post_rename_failure(
                store,
                identity,
                PublicationError::Namespace {
                    canonical: "changed-inode",
                    temporary: "absent",
                },
            ));
        }
        if let Err(source) = require_file(&temporary, &canonical_path, expected.len() as u64, STATE_ID_MODE) {
            return Err(reconcile_post_rename_failure(
                store,
                identity,
                io_error("authenticate published state-ID inode", &canonical_path, source),
            ));
        }
        let retained = Self {
            file: temporary,
            path: canonical_path,
            state,
            expected,
            witness,
        };
        if let Err(source) = retained.revalidate(store, store) {
            return Err(reconcile_post_rename_failure(
                store,
                identity,
                PublicationError::Identity(Box::new(source)),
            ));
        }
        if let Err(source) = checkpoint(StateIdPublicationFaultPoint::FinalRevalidation) {
            return Err(reconcile_post_rename_failure(store, identity, source));
        }
        if let Err(source) = retained.revalidate(store, store) {
            return Err(reconcile_post_rename_failure(
                store,
                identity,
                PublicationError::Identity(Box::new(source)),
            ));
        }
        Ok(retained)
    }
}

/// Classify every failure after the one-shot rename from fresh descriptor-
/// relative observations. This path never cleans up: the move may have been
/// applied, even when the syscall or a later durability boundary failed.
fn reconcile_post_rename_failure(
    store: &TreeMarkerStore,
    identity: InodeIdentity,
    primary: PublicationError,
) -> StateIdPublicationFailure {
    match inspect_namespace(store, identity) {
        Ok(namespace) => StateIdPublicationFailure::new(namespace.outcome(), primary),
        Err(reconciliation) => StateIdPublicationFailure::new(
            StateIdPublicationOutcome::Ambiguous,
            PublicationError::Reconciliation {
                primary: Box::new(primary),
                reconciliation: Box::new(reconciliation),
            },
        ),
    }
}

fn failure_identity(outcome: StateIdPublicationOutcome, source: super::super::Error) -> StateIdPublicationFailure {
    StateIdPublicationFailure::new(outcome, PublicationError::Identity(Box::new(source)))
}

fn failure_io(
    outcome: StateIdPublicationOutcome,
    operation: &'static str,
    path: &Path,
    source: io::Error,
) -> StateIdPublicationFailure {
    StateIdPublicationFailure::new(outcome, io_error(operation, path, source))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PublicationError {
    PublicationError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn require_directory(store: &TreeMarkerStore) -> Result<(), PublicationError> {
    store
        .revalidate_directory()
        .map_err(|source| PublicationError::Identity(Box::new(source.into())))?;
    require_no_default_acl(store.retained_directory(), store.display_path()).map_err(|source| {
        io_error(
            "reject inherited ACL before state-ID publication",
            store.display_path(),
            source,
        )
    })?;
    require_no_xattrs(store.retained_directory(), store.display_path())
        .map_err(|source| io_error("reject candidate-directory attributes", store.display_path(), source))?;
    store
        .revalidate_directory()
        .map_err(|source| PublicationError::Identity(Box::new(source.into())))
}

fn write_all_at(file: &std::fs::File, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0usize;
    let mut attempts = 0usize;
    while offset < bytes.len() {
        attempts += 1;
        if attempts > MAX_IO_ATTEMPTS {
            return Err(io::Error::other("state-ID write exceeded bounded retry limit"));
        }
        match file.write_at(&bytes[offset..], offset as u64) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => offset += written,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(source),
        }
    }
    Ok(())
}

fn require_file(file: &std::fs::File, path: &Path, length: u64, mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let actual_mode = metadata.permissions().mode() & 0o7777;
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.len() != length
        || actual_mode != mode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "unsafe state-ID inode (uid={}, mode={actual_mode:04o}, links={}, length={})",
                metadata.uid(),
                metadata.nlink(),
                metadata.len()
            ),
        ));
    }
    require_no_access_acl(file, path)?;
    require_no_xattrs(file, path)
}

fn require_no_xattrs(file: &std::fs::File, path: &Path) -> io::Result<()> {
    for _ in 0..MAX_IO_ATTEMPTS {
        // SAFETY: the retained readable descriptor is live; the null/zero
        // invocation is the documented size probe and copies no bytes.
        let result = unsafe { nix::libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
        if result == 0 {
            return Ok(());
        }
        if result > 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("inode carries {result} xattr-name bytes: {}", path.display()),
            ));
        }
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) {
            return Ok(());
        }
        return Err(source);
    }
    Err(io::Error::other("xattr probe exceeded bounded retry limit"))
}

fn inode_identity(file: &std::fs::File) -> io::Result<InodeIdentity> {
    let metadata = file.metadata()?;
    Ok(InodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_exact_file(
    file: &std::fs::File,
    witness: StateIdWitness,
    path: &Path,
    expected: &[u8],
    state: state::Id,
) -> Result<(), PublicationError> {
    let actual = state_id_witness(file, path, expected.len() as u64)
        .map_err(|source| PublicationError::Identity(Box::new(source)))?;
    if actual != witness {
        return Err(PublicationError::Namespace {
            canonical: "absent",
            temporary: "retained-changed",
        });
    }
    require_state_id_contents(file, path, expected, state)
        .map_err(|source| PublicationError::Identity(Box::new(source)))?;
    require_file(file, path, expected.len() as u64, STATE_ID_MODE)
        .map_err(|source| io_error("authenticate exact state-ID temporary", path, source))
}

fn probe_name(
    store: &TreeMarkerStore,
    name: &std::ffi::CStr,
    expected: Option<InodeIdentity>,
) -> Result<NameState, PublicationError> {
    let path = store.display_path().join(name.to_string_lossy().as_ref());
    let resolution = controlled_resolution() & !(nix::libc::RESOLVE_NO_SYMLINKS as u64);
    match openat2_file(
        store.retained_directory().as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        resolution,
    ) {
        Ok(file) => {
            let identity =
                inode_identity(&file).map_err(|source| io_error("inspect state-ID publication name", &path, source))?;
            Ok(if expected == Some(identity) {
                NameState::Exact
            } else {
                NameState::Foreign(identity)
            })
        }
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(NameState::Absent),
        Err(source) if matches!(source.raw_os_error(), Some(nix::libc::EXDEV) | Some(nix::libc::ELOOP)) => {
            Ok(NameState::OpaqueForeign(source.raw_os_error().unwrap()))
        }
        Err(source) => Err(io_error("probe state-ID publication name", &path, source)),
    }
}

fn inspect_namespace(store: &TreeMarkerStore, expected: InodeIdentity) -> Result<NamespaceState, PublicationError> {
    store
        .revalidate_directory()
        .map_err(|source| PublicationError::Identity(Box::new(source.into())))?;
    let state = NamespaceState {
        canonical: probe_name(store, STATE_ID_NAME, Some(expected))?,
        temporary: probe_name(store, STATE_ID_TEMPORARY_NAME, Some(expected))?,
    };
    store
        .revalidate_directory()
        .map_err(|source| PublicationError::Identity(Box::new(source.into())))?;
    Ok(state)
}

fn cleanup_failure(
    store: &TreeMarkerStore,
    temporary: &std::fs::File,
    identity: InodeIdentity,
    primary: PublicationError,
) -> StateIdPublicationFailure {
    match retire_temporary(store, temporary, identity) {
        Ok(()) => StateIdPublicationFailure::new(StateIdPublicationOutcome::NotPublished, primary),
        Err(cleanup) => StateIdPublicationFailure::new(
            StateIdPublicationOutcome::Ambiguous,
            PublicationError::Cleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            },
        ),
    }
}

fn retire_temporary(
    store: &TreeMarkerStore,
    temporary: &std::fs::File,
    identity: InodeIdentity,
) -> Result<(), PublicationError> {
    let before = inspect_namespace(store, identity)?;
    if before.canonical == NameState::Exact || before.temporary != NameState::Exact {
        return Err(before.error());
    }
    if inode_identity(temporary).map_err(|source| {
        io_error(
            "inspect retained state-ID temporary before retirement",
            &store.display_path().join(".cast-state-id.tmp"),
            source,
        )
    })? != identity
    {
        return Err(PublicationError::Namespace {
            canonical: before.canonical.as_str(),
            temporary: "retained-changed",
        });
    }

    // SAFETY: the retained directory and fixed C string remain live. This is
    // deliberately one attempt; EINTR is reconciled and never retried.
    let unlink = if unsafe {
        nix::libc::unlinkat(
            store.retained_directory().as_raw_fd(),
            STATE_ID_TEMPORARY_NAME.as_ptr(),
            0,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    let after = inspect_namespace(store, identity)?;
    let links = temporary
        .metadata()
        .map_err(|source| {
            io_error(
                "inspect retired state-ID temporary",
                &store.display_path().join(".cast-state-id.tmp"),
                source,
            )
        })?
        .nlink();
    if after.canonical != before.canonical || after.temporary != NameState::Absent || links != 0 {
        return Err(match unlink {
            Err(source) => io_error(
                "retire exact state-ID temporary",
                &store.display_path().join(".cast-state-id.tmp"),
                source,
            ),
            Ok(()) => after.error(),
        });
    }
    store
        .retained_directory()
        .sync_all()
        .map_err(|source| io_error("sync exact state-ID temporary retirement", store.display_path(), source))?;
    let final_state = inspect_namespace(store, identity)?;
    if final_state.canonical == before.canonical && final_state.temporary == NameState::Absent {
        Ok(())
    } else {
        Err(final_state.error())
    }
}
