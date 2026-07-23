//! Exact retirement of an unused preparation reservation.
//!
//! The final-component `unlinkat` guarantee is scoped to the installation's
//! cooperating coordinator/lock discipline. Linux cannot condition rmdir on
//! an inode number, so an uncooperative same-UID writer could substitute the
//! final name after preflight. Exact absence plus retained `nlink=0`
//! distinguishes a moved-away retained inode from an unlinked one; it cannot
//! make the syscall conditional or guarantee foreign preservation against
//! such a writer. No error path retries or recursively removes either tree.

use std::os::{fd::AsRawFd as _, unix::fs::MetadataExt as _};

use super::{
    ArchivedStateRepairError, ArchivedStateRepairIdentity,
    error::identity,
    fault_injection::{ArchivedStateRepairFaultPoint, checkpoint},
};
use crate::Installation;

const UNLINKED_DIRECTORY_LINKS: u64 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReservationNameState {
    Absent,
    Exact,
    Foreign,
}

impl ArchivedStateRepairIdentity {
    /// Retire only the fresh, retained, still-empty Q directory after guard
    /// preparation fails. The descriptor-relative rmdir is issued once: an
    /// interrupted syscall is reconciled from the retained inode and name,
    /// never retried or followed by recursive cleanup.
    pub(super) fn retire_failed_preparation_reservation(
        &self,
        installation: &Installation,
    ) -> Result<(), ArchivedStateRepairError> {
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| {
                identity(
                    "revalidate installation root before preparation-reservation retirement",
                    source,
                )
            })?;
        self.quarantine
            .revalidate_beneath(installation.root_directory(), super::super::QUARANTINE_RELATIVE)
            .map_err(|source| {
                identity(
                    "revalidate quarantine before preparation-reservation retirement",
                    source,
                )
            })?;
        self.require_exact_named_reservation()?;
        checkpoint(ArchivedStateRepairFaultPoint::BeforePreparationReservationRetirement)?;

        // SAFETY: quarantine and its single validated C-string child name
        // remain retained. This is deliberately one-shot: EINTR may describe
        // an already-applied rmdir and is reconciled below.
        let syscall = if unsafe {
            nix::libc::unlinkat(
                self.quarantine.file.as_raw_fd(),
                self.quarantine_name.as_ptr(),
                nix::libc::AT_REMOVEDIR,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(ArchivedStateRepairError::Io {
                operation: "retire exact empty archived-repair preparation reservation",
                path: self.quarantine_path.clone(),
                source: std::io::Error::last_os_error(),
            })
        };

        match self.reservation_name_state()? {
            ReservationNameState::Absent => {
                self.replacement
                    .require_retained()
                    .map_err(|source| identity("revalidate retired preparation reservation inode", source))?;
                self.replacement
                    .require_exact_entries(&[])
                    .map_err(|source| identity("require retired preparation reservation to remain empty", source))?;
                self.require_replacement_link_count(UNLINKED_DIRECTORY_LINKS)?;
            }
            ReservationNameState::Exact => {
                self.require_exact_named_reservation()?;
                return Err(match syscall {
                    Err(source) => source,
                    Ok(()) => ArchivedStateRepairError::ReportedSuccessWithoutMove {
                        operation: "retire exact empty archived-repair preparation reservation",
                    },
                });
            }
            ReservationNameState::Foreign => {
                return Err(ArchivedStateRepairError::PreparationReservationNamespaceChanged {
                    path: self.quarantine_path.clone(),
                });
            }
        }

        // Exact absence plus nlink=0 proves the retained directory, rather
        // than a copied empty occupant, was removed. A syscall error is an
        // applied result in this layout and must not cause a second rmdir.
        self.quarantine
            .sync("sync quarantine after preparation-reservation retirement")
            .map_err(|source| identity("sync retired preparation-reservation name", source))?;
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| {
                identity(
                    "revalidate installation root after preparation-reservation retirement",
                    source,
                )
            })?;
        self.quarantine
            .revalidate_beneath(installation.root_directory(), super::super::QUARANTINE_RELATIVE)
            .map_err(|source| identity("revalidate quarantine after preparation-reservation retirement", source))?;
        self.quarantine
            .require_child_absent(&self.quarantine_name)
            .map_err(|source| identity("finish preparation-reservation absence proof", source))
    }

    fn require_exact_named_reservation(&self) -> Result<(), ArchivedStateRepairError> {
        self.replacement
            .require_retained()
            .map_err(|source| identity("revalidate retained preparation reservation", source))?;
        self.replacement
            .require_exact_entries(&[])
            .map_err(|source| identity("require empty retained preparation reservation", source))?;
        match self.reservation_name_state()? {
            ReservationNameState::Exact => Ok(()),
            ReservationNameState::Absent | ReservationNameState::Foreign => {
                Err(ArchivedStateRepairError::PreparationReservationNamespaceChanged {
                    path: self.quarantine_path.clone(),
                })
            }
        }
    }

    fn reservation_name_state(&self) -> Result<ReservationNameState, ArchivedStateRepairError> {
        let named = self
            .quarantine
            .open_optional_child(&self.quarantine_name, self.quarantine_path.clone())
            .map_err(|source| identity("reopen preparation-reservation name", source))?;
        let Some(named) = named else {
            return Ok(ReservationNameState::Absent);
        };
        if named.witness == self.replacement.witness {
            named
                .require_exact_entries(&[])
                .map_err(|source| identity("require named preparation reservation to remain empty", source))?;
            self.replacement
                .require_same(&named)
                .map_err(|source| identity("authenticate named preparation reservation", source))?;
            Ok(ReservationNameState::Exact)
        } else {
            Ok(ReservationNameState::Foreign)
        }
    }

    fn require_replacement_link_count(&self, expected: u64) -> Result<(), ArchivedStateRepairError> {
        let actual = self
            .replacement
            .file
            .metadata()
            .map_err(|source| ArchivedStateRepairError::Io {
                operation: "inspect preparation-reservation link count",
                path: self.quarantine_path.clone(),
                source,
            })?
            .nlink();
        if actual == expected {
            Ok(())
        } else {
            Err(ArchivedStateRepairError::PreparationReservationLinkCount {
                path: self.quarantine_path.clone(),
                expected,
                actual,
            })
        }
    }
}
