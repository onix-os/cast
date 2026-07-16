//! Authentication of a persistent tree-marker hardlink after process restart.

use std::{ffi::CString, io};

use super::{
    Error, MAX_PREVIOUS_SLOT_PARKING_CANDIDATES, ROOTS_RELATIVE, RetainedDirectory, RetainedIdentity,
    archived_candidate::archived_candidate_parking_name,
    canonical_state_name, open_optional_retained_tree,
    state_slot_marker::{Error as StateSlotMarkerError, RetainedStateSlotMarker},
};
use crate::{Installation, state};

#[derive(Clone, Copy)]
pub(super) enum WrapperKind {
    Canonical,
    Parked,
}

pub(super) struct RecoveredSlotLink {
    pub(super) roots: RetainedDirectory,
    pub(super) state: state::Id,
    pub(super) name: CString,
    pub(super) wrapper: RetainedDirectory,
    pub(super) marker: RetainedStateSlotMarker,
    pub(super) kind: WrapperKind,
}

impl RetainedIdentity {
    pub(super) fn authorize_recovered_slot_link(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<Option<RecoveredSlotLink>, Error> {
        if !self.marker.needs_slot_link_authorization() {
            return Ok(None);
        }

        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path)?;
        let mut found = None;

        let canonical_name = canonical_state_name(state)?;
        self.inspect_recovery_wrapper(&roots, canonical_name, WrapperKind::Canonical, state, &mut found)?;

        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let name = archived_candidate_parking_name(state, self.marker.token().as_str(), index)
                .map_err(Error::InvalidReusableArchivedCandidateParkingName)?;
            self.inspect_recovery_wrapper(&roots, name, WrapperKind::Parked, state, &mut found)?;
        }

        let recovered = found.ok_or(Error::MissingAuthorizedStateSlotLink {
            state: i32::from(state),
        })?;
        installation.revalidate_root_directory()?;
        roots.revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)?;
        recovered.wrapper.revalidate_child(&roots, &recovered.name)?;
        recovered.marker.require_named(&recovered.wrapper)?;
        self.require_recovered_wrapper_layout(&recovered.wrapper, &recovered.marker, recovered.kind)?;
        // A prior in-process publication may have linked the marker before a
        // wrapper fsync reported failure. Reopening proves the exact sole
        // hardlink; repeat both inode and containing-directory durability
        // before authorizing nlink=2 for a new pre-journal baseline.
        recovered.marker.sync()?;
        recovered
            .wrapper
            .sync("sync recovered state-slot wrapper before link authorization")?;
        recovered.wrapper.revalidate_child(&roots, &recovered.name)?;
        recovered.marker.require_named(&recovered.wrapper)?;
        self.require_recovered_wrapper_layout(&recovered.wrapper, &recovered.marker, recovered.kind)?;
        self.marker.authorize_recovered_slot_link()?;
        recovered.marker.require_named(&recovered.wrapper)?;
        self.require_recovered_wrapper_layout(&recovered.wrapper, &recovered.marker, recovered.kind)?;
        self.revalidate_retained()?;
        Ok(Some(recovered))
    }

    fn inspect_recovery_wrapper(
        &self,
        roots: &RetainedDirectory,
        name: CString,
        kind: WrapperKind,
        state: state::Id,
        found: &mut Option<RecoveredSlotLink>,
    ) -> Result<(), Error> {
        let path = roots.path.join(name.to_string_lossy().as_ref());
        if !roots.child_name_exists(&name, path.clone())? {
            return Ok(());
        }
        let wrapper = match roots.open_child(&name, path) {
            Ok(wrapper) => wrapper,
            Err(source) if matches!(kind, WrapperKind::Parked) && skippable_wrapper_occupant(&source) => return Ok(()),
            Err(source) => return Err(source),
        };
        let marker = match RetainedStateSlotMarker::open_recovery_candidate(&wrapper, state, &self.marker) {
            Ok(marker) => marker,
            Err(source) if skippable_marker_occupant(&source) => return Ok(()),
            Err(source) => return Err(source.into()),
        };
        self.require_recovered_wrapper_layout(&wrapper, &marker, kind)?;
        if found.is_some() {
            return Err(Error::DuplicateAuthorizedStateSlotLinks {
                state: i32::from(state),
            });
        }
        *found = Some(RecoveredSlotLink {
            roots: roots.clone_retained()?,
            state,
            name,
            wrapper,
            marker,
            kind,
        });
        Ok(())
    }

    fn require_recovered_wrapper_layout(
        &self,
        wrapper: &RetainedDirectory,
        marker: &RetainedStateSlotMarker,
        kind: WrapperKind,
    ) -> Result<(), Error> {
        if matches!(kind, WrapperKind::Parked) {
            return wrapper.require_exact_entries(&[marker.name_bytes()]);
        }

        match wrapper.require_exact_entries(&[marker.name_bytes()]) {
            Ok(()) => Ok(()),
            Err(Error::UnexpectedQuarantineEntries { .. }) => {
                wrapper.require_exact_entries(&[marker.name_bytes(), b"usr"])?;
                let path = wrapper.path.join("usr");
                let tree = open_optional_retained_tree(wrapper, &path)?.ok_or(Error::PreviousMoveTreeMissing {
                    staged: path.clone(),
                    archived: path,
                })?;
                self.store.require_same_directory(&tree).map_err(Error::from)
            }
            Err(source) => Err(source),
        }
    }
}

fn skippable_wrapper_occupant(source: &Error) -> bool {
    match source {
        Error::UnsafeQuarantineDirectory { .. } => true,
        Error::Quarantine {
            operation: "pin retained directory",
            source,
            ..
        } => matches!(
            source.raw_os_error(),
            Some(nix::libc::ENOTDIR) | Some(nix::libc::ELOOP) | Some(nix::libc::EXDEV)
        ),
        Error::Quarantine {
            operation: "reject access ACL on retained directory" | "reject default ACL on retained directory",
            source,
            ..
        } => source.kind() == io::ErrorKind::PermissionDenied && source.raw_os_error().is_none(),
        _ => false,
    }
}

fn skippable_marker_occupant(source: &StateSlotMarkerError) -> bool {
    match source {
        StateSlotMarkerError::Missing { .. } => true,
        StateSlotMarkerError::Io {
            operation: "probe state-slot marker",
            source,
            ..
        } => matches!(source.raw_os_error(), Some(nix::libc::ELOOP) | Some(nix::libc::EXDEV)),
        _ => false,
    }
}
