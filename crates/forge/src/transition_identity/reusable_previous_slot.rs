//! Discovery of authenticated wrapper slots retained from earlier activation.

use std::{ffi::CString, io};

use super::{
    Error, MAX_PREVIOUS_SLOT_PARKING_CANDIDATES, ROOTS_RELATIVE, RetainedDirectory, RetainedPreviousArchiveAttempt,
    StatefulTreeIdentity,
    archived_candidate::archived_candidate_parking_name,
    state_slot_marker::{Error as StateSlotMarkerError, RetainedStateSlotMarker},
};
use crate::{Installation, state};

pub(super) struct ReusablePreviousStateSlot {
    pub(super) parking_name: CString,
    pub(super) slot: RetainedDirectory,
    pub(super) marker: RetainedStateSlotMarker,
}

impl StatefulTreeIdentity {
    pub(super) fn find_reusable_previous_state_slot(
        &self,
        installation: &Installation,
        roots: &RetainedDirectory,
        staging: &RetainedDirectory,
        state: state::Id,
    ) -> Result<Option<ReusablePreviousStateSlot>, Error> {
        let mut found = None;
        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let parking_name = archived_candidate_parking_name(state, self.previous.marker.token().as_str(), index)
                .map_err(Error::InvalidReusableArchivedCandidateParkingName)?;
            let parking_path = roots.path.join(parking_name.to_string_lossy().as_ref());
            if !roots.child_name_exists(&parking_name, parking_path.clone())? {
                continue;
            }

            // Every non-matching occupant is foreign evidence. Preserve it
            // and continue the bounded scan without following or repairing it.
            let slot = match roots.open_child(&parking_name, parking_path) {
                Ok(slot) => slot,
                Err(source) if skippable_wrapper_occupant(&source) => continue,
                Err(source) => return Err(source),
            };
            let marker = match RetainedStateSlotMarker::open_expected(&slot, state, &self.previous.marker) {
                Ok(marker) => marker,
                Err(source) if skippable_marker_occupant(&source) => continue,
                Err(source) => return Err(source.into()),
            };
            marker.require_named(&slot)?;
            if let Err(source) = slot.require_exact_entries(&[marker.name_bytes()]) {
                if skippable_wrapper_layout(&source) {
                    continue;
                }
                return Err(source);
            }
            if found.is_some() {
                return Err(Error::DuplicateReusableArchivedCandidateSlots {
                    state: i32::from(state),
                });
            }
            found = Some(ReusablePreviousStateSlot {
                parking_name,
                slot,
                marker,
            });
        }

        installation.revalidate_root_directory()?;
        roots.revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)?;
        staging.revalidate_child(roots, c"staging")?;
        self.previous_move_layout_without_slot(staging)?;
        if let Some(reusable) = found.as_ref() {
            reusable.slot.revalidate_child(roots, &reusable.parking_name)?;
            reusable.marker.require_named(&reusable.slot)?;
            reusable.slot.require_exact_entries(&[reusable.marker.name_bytes()])?;
        }
        Ok(found)
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

fn skippable_wrapper_layout(source: &Error) -> bool {
    matches!(source, Error::UnexpectedQuarantineEntries { .. })
}

impl RetainedPreviousArchiveAttempt {
    pub(super) fn require_slot_without_tree(&self) -> Result<(), Error> {
        match self.state_slot_marker.as_ref() {
            Some(marker) => {
                marker.require_named(&self.slot)?;
                self.slot.require_exact_entries(&[marker.name_bytes()])
            }
            None => self.slot.require_exact_entries(&[]),
        }
    }

    pub(super) fn require_slot_with_tree(&self) -> Result<(), Error> {
        match self.state_slot_marker.as_ref() {
            Some(marker) => {
                marker.require_named(&self.slot)?;
                self.slot.require_exact_entries(&[marker.name_bytes(), b"usr"])
            }
            None => self.slot.require_exact_entries(&[b"usr"]),
        }
    }

    pub(super) fn require_reusable_marker(&self) -> Result<(), Error> {
        if let Some(marker) = self.state_slot_marker.as_ref() {
            marker.require_retained()?;
            marker.require_named(&self.slot)?;
        }
        Ok(())
    }

    pub(super) fn sync_reusable_marker(&self) -> Result<(), Error> {
        if let Some(marker) = self.state_slot_marker.as_ref() {
            marker.sync()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn reusable_slot_scan_skips_only_proven_foreign_errors() {
        let path = PathBuf::from("slot");
        let wrapper_shape = Error::Quarantine {
            operation: "pin retained directory",
            path: path.clone(),
            source: io::Error::from_raw_os_error(nix::libc::ENOTDIR),
        };
        assert!(skippable_wrapper_occupant(&wrapper_shape));
        let wrapper_io = Error::Quarantine {
            operation: "pin retained directory",
            path: path.clone(),
            source: io::Error::from_raw_os_error(nix::libc::EIO),
        };
        assert!(!skippable_wrapper_occupant(&wrapper_io));

        let marker_missing = StateSlotMarkerError::Missing { path: path.clone() };
        assert!(skippable_marker_occupant(&marker_missing));
        let marker_io = StateSlotMarkerError::Io {
            operation: "probe state-slot marker",
            path: path.clone(),
            source: io::Error::from_raw_os_error(nix::libc::EIO),
        };
        assert!(!skippable_marker_occupant(&marker_io));
        let marker_changed = StateSlotMarkerError::Changed { path: path.clone() };
        assert!(!skippable_marker_occupant(&marker_changed));

        let foreign_layout = Error::UnexpectedQuarantineEntries {
            path: path.clone(),
            entries: vec!["foreign".to_owned()],
        };
        assert!(skippable_wrapper_layout(&foreign_layout));
        let layout_io = Error::Quarantine {
            operation: "enumerate retained directory",
            path,
            source: io::Error::from_raw_os_error(nix::libc::EIO),
        };
        assert!(!skippable_wrapper_layout(&layout_io));
    }
}
