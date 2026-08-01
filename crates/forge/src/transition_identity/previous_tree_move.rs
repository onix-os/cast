use super::*;
use std::ffi::CString;

use crate::transition_journal::{PreviousArchiveSlot, QuarantineName};
use super::reusable_previous_slot::ReusablePreviousStateSlot;

/// Re-validate a namespace name through the journal model's newtype.
///
/// The producing helpers already build these via `QuarantineName::parse`, so
/// this cannot widen what is accepted; it re-establishes the bound at the
/// journal boundary rather than trusting the caller.
fn quarantine_name_of(name: &CStr) -> Result<QuarantineName, Error> {
    QuarantineName::parse(name.to_string_lossy().as_ref())
        .map_err(Error::InvalidReusableArchivedCandidateParkingName)
}

/// Proof that a physical previous-tree restore is being driven by the startup
/// crash-recovery path, which legitimately runs with the transition journal
/// present (the journal is exactly what selected this recovery). Unlike the
/// forward [`journal_coordinator::PreviousArchiveEffectSeal`] — minted inside
/// the coordinator that owns the durable record — the recovery seal is minted
/// by the `PreviousRestore` rollback dispatcher, which lives in the `client`
/// layer above this module; the guard therefore cannot name that module and the
/// mint is `pub(crate)` rather than module-private. It must only ever be created
/// by that dispatcher after it has proven the exact `PreviousRestoreIntent`
/// record and namespace.
#[derive(Clone, Copy)]
pub(crate) struct PreviousRestoreRecoverySeal {
    _private: (),
}

impl PreviousRestoreRecoverySeal {
    /// Mint the recovery seal. ONLY the `PreviousRestore` rollback dispatcher may
    /// call this, and only after admitting the exact durable record.
    // Forward scaffolding: consumed by the not-yet-built dispatcher (Phase 1);
    // exercised today only by the physical-primitive test.
    #[allow(dead_code)]
    pub(crate) fn for_recovery() -> Self {
        Self { _private: () }
    }
}

/// Journal expectation for a previous-tree move. Legacy entry points require
/// the transition journal to be absent (a present journal signals an
/// unreconciled crash); a journal-coordinated caller instead proves, with an
/// unforgeable seal, that it owns the exact durable record whose journal is
/// intentionally retained across this physical move.
#[derive(Clone, Copy)]
enum ArchiveJournalGuard<'authority> {
    LegacyNoJournal,
    Coordinator(&'authority journal_coordinator::PreviousArchiveEffectSeal),
    // Forward scaffolding: constructed by `restore_previous_with_journal`, whose
    // only caller today is a test; the PreviousRestore dispatcher (Phase 1) will
    // be the production caller.
    #[allow(dead_code)]
    Recovery(&'authority PreviousRestoreRecoverySeal),
}

impl ArchiveJournalGuard<'_> {
    fn require(self, identity: &StatefulTreeIdentity) -> Result<(), Error> {
        match self {
            Self::LegacyNoJournal => identity.require_no_journal(),
            Self::Coordinator(seal) => {
                let _seal = seal;
                Ok(())
            }
            Self::Recovery(seal) => {
                let _seal = seal;
                Ok(())
            }
        }
    }
}

impl StatefulTreeIdentity {
    /// Move the exact staged previous tree into an authenticated state slot.
    ///
    /// The state slot is either freshly created beneath the retained roots
    /// directory or recovered from an exact immutable wrapper marker left by
    /// an earlier activation. The move makes one `RENAME_NOREPLACE` attempt
    /// and reconciles both retained parents before interpreting its result.
    pub(crate) fn archive_previous(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.archive_previous_guarded(installation, state, ArchiveJournalGuard::LegacyNoJournal, None)
    }

    /// Coordinator-only archive. The seal proves the caller owns the exact
    /// durable `PreviousArchiveIntent`; the journal stays retained across the
    /// move rather than blocking it.
    pub(super) fn archive_previous_with_journal(
        &self,
        installation: &Installation,
        state: state::Id,
        seal: &journal_coordinator::PreviousArchiveEffectSeal,
        recorded: &PreviousArchiveSlot,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.archive_previous_guarded(installation, state, ArchiveJournalGuard::Coordinator(seal), Some(recorded))
    }

    fn archive_previous_guarded(
        &self,
        installation: &Installation,
        state: state::Id,
        guard: ArchiveJournalGuard<'_>,
        recorded: Option<&PreviousArchiveSlot>,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let result = self.move_previous_recorded(
            installation,
            state,
            RetainedPreviousMoveDirection::Archive,
            guard,
            recorded,
        );
        match result {
            Err(failure) if failure.outcome == RetainedPreviousMoveOutcome::NotApplied => {
                match self.finish_not_applied_previous_archive_guarded(installation, state, guard) {
                    Ok(()) => Err(failure),
                    Err(cleanup) => Err(failure.with_abort_cleanup(cleanup)),
                }
            }
            result => result,
        }
    }

    /// Move the exact archived previous tree back into staging for the
    /// compensating exchange.
    pub(crate) fn restore_previous(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.move_previous(
            installation,
            state,
            RetainedPreviousMoveDirection::Restore,
            ArchiveJournalGuard::LegacyNoJournal,
        )
    }

    /// Recovery-only restore. The seal proves the caller is the `PreviousRestore`
    /// rollback dispatcher reconciling a crashed archive; the transition journal
    /// is legitimately present (it selected this recovery) and stays retained
    /// across the compensating move rather than blocking it, exactly mirroring
    /// the forward [`Self::archive_previous_with_journal`].
    // Forward scaffolding: the PreviousRestore rollback dispatcher (Phase 1) will
    // be the production caller; exercised today by the physical-primitive test.
    #[allow(dead_code)]
    pub(crate) fn restore_previous_with_journal(
        &self,
        installation: &Installation,
        state: state::Id,
        seal: &PreviousRestoreRecoverySeal,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.move_previous(
            installation,
            state,
            RetainedPreviousMoveDirection::Restore,
            ArchiveJournalGuard::Recovery(seal),
        )
    }

    /// Resume only the idempotent durability suffix of an archive already
    /// proven to have moved the exact previous tree.
    pub(crate) fn finish_applied_previous_archive(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        self.finish_applied_previous_move(
            installation,
            state,
            RetainedPreviousMoveDirection::Archive,
            ArchiveJournalGuard::LegacyNoJournal,
        )
    }

    /// Resume only the idempotent durability suffix of a compensating restore
    /// already proven to have moved the exact previous tree.
    pub(crate) fn finish_applied_previous_restore(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        self.finish_applied_previous_move(
            installation,
            state,
            RetainedPreviousMoveDirection::Restore,
            ArchiveJournalGuard::LegacyNoJournal,
        )
    }

    /// Resume the durability suffix of a compensating restore while the
    /// transition journal is deliberately retained.
    ///
    /// The legacy sibling refuses whenever a journal is present, which is the
    /// correct crash signal outside recovery. A rollback dispatcher resuming a
    /// restore that already moved the tree owns the journal by construction, so
    /// it presents the unforgeable recovery seal instead.
    // Forward scaffolding: the PreviousRestore rollback dispatcher (Phase 1) will
    // be the production caller; exercised today by the physical-primitive test.
    #[allow(dead_code)]
    pub(crate) fn finish_applied_previous_restore_with_journal(
        &self,
        installation: &Installation,
        state: state::Id,
        seal: &PreviousRestoreRecoverySeal,
    ) -> Result<(), Error> {
        self.finish_applied_previous_move(
            installation,
            state,
            RetainedPreviousMoveDirection::Restore,
            ArchiveJournalGuard::Recovery(seal),
        )
    }

    /// Retire only an exact inert state slot retained by this guard after an
    /// archive attempt which is proven not to have moved the previous tree.
    /// A reusable slot remains marker-only; a fresh slot remains empty.
    /// The slot is moved back to a non-state parking name rather than deleted,
    /// so ambient, replaced, moved, or populated directories are preserved.
    pub(crate) fn finish_not_applied_previous_archive(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), Error> {
        self.finish_not_applied_previous_archive_guarded(installation, state, ArchiveJournalGuard::LegacyNoJournal)
    }

    fn finish_not_applied_previous_archive_guarded(
        &self,
        installation: &Installation,
        state: state::Id,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), Error> {
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| Error::PreviousArchiveAttemptLockPoisoned)?;
        let Some(attempt) = retained.as_ref() else {
            return Ok(());
        };
        let name = canonical_state_name(state)?;
        require_previous_attempt_name(attempt, state, &name)?;
        guard.require(self)?;
        self.revalidate_previous_move_base(installation, attempt)?;
        self.finish_previous_slot_retirement(installation, attempt, guard)?;
        *retained = None;
        Ok(())
    }

    fn move_previous(
        &self,
        installation: &Installation,
        state: state::Id,
        direction: RetainedPreviousMoveDirection,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        self.move_previous_recorded(installation, state, direction, guard, None)
    }

    fn move_previous_recorded(
        &self,
        installation: &Installation,
        state: state::Id,
        direction: RetainedPreviousMoveDirection,
        guard: ArchiveJournalGuard<'_>,
        recorded: Option<&PreviousArchiveSlot>,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let not_applied = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedPreviousMoveFailure {
            outcome: RetainedPreviousMoveOutcome::Ambiguous,
            source,
        };

        guard.require(self).map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;
        let name = canonical_state_name(state).map_err(not_applied)?;
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| not_applied(Error::PreviousArchiveAttemptLockPoisoned))?;

        let mut created_now = false;
        if retained.is_none() {
            if direction == RetainedPreviousMoveDirection::Restore {
                return Err(not_applied(Error::PreviousArchiveAttemptMissing {
                    state: i32::from(state),
                }));
            }
            *retained = Some(
                self.create_previous_archive_attempt(installation, state, &name, recorded)
                    .map_err(not_applied)?,
            );
            created_now = true;
        }
        let attempt = retained.as_ref().expect("previous archive attempt was established");
        let preflight = (|| -> Result<(), Error> {
            require_previous_attempt_name(attempt, state, &name)?;
            if direction == RetainedPreviousMoveDirection::Archive {
                self.finish_previous_archive_slot_creation(installation, attempt)?;
            }
            self.revalidate_previous_move_namespace(installation, attempt)?;
            self.require_previous_move_layout(attempt, direction.before())?;

            // A newly created archive attempt was already pre-synced
            // immediately before parking-slot creation. A resumed archive
            // attempt and every restore perform exactly one fresh pre-sync.
            if !created_now || direction == RetainedPreviousMoveDirection::Restore {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::PreviousPreSync)?;
                self.previous.store.sync_retained_tree()?;
                self.require_previous_move_layout(attempt, direction.before())?;
            }

            before_retained_previous_move_rename();
            guard.require(self)?;
            installation.revalidate_root_directory()?;
            self.revalidate_previous_move_namespace(installation, attempt)?;
            self.require_previous_move_layout(attempt, direction.before())?;
            retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeRename)
        })();
        if let Err(source) = preflight {
            let reconciled = self.reconcile_previous_pre_move_failure(installation, attempt, direction, source, guard);
            if reconciled.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
                *retained = None;
            }
            return reconciled;
        }

        let (source, destination) = match direction {
            RetainedPreviousMoveDirection::Archive => (&attempt.staging, &attempt.slot),
            RetainedPreviousMoveDirection::Restore => (&attempt.slot, &attempt.staging),
        };
        // Never retry this syscall. An error, including EINTR, may describe a
        // move which the kernel already completed. Reconcile both names first.
        let syscall_result = renameat2_noreplace_once(&source.file, LIVE_USR_NAME, &destination.file, LIVE_USR_NAME)
            .map_err(|source| previous_move_io("move exact previous /usr", &destination.path.join("usr"), source))
            .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterRename));

        let observed = self.previous_move_layout(attempt).map_err(ambiguous)?;
        if observed == direction.before() {
            let source = match syscall_result {
                Err(source) => source,
                Ok(()) => Error::PreviousMoveReportedSuccessWithoutMove {
                    direction: direction.as_str(),
                },
            };
            return Err(not_applied(source));
        }
        if observed != direction.after() {
            return Err(ambiguous(Error::PreviousMoveUnexpectedLayout {
                direction: direction.as_str(),
                expected: direction.after().as_str(),
                actual: observed.as_str(),
            }));
        }

        // A syscall error is superseded by exact post-move identity evidence.
        // Durability faults remain Applied so callers can resume this suffix
        // without issuing a second rename.
        let finish = self.finish_previous_move(installation, attempt, direction, guard);
        if finish.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
            *retained = None;
        }
        finish.map_err(applied)
    }

    fn reconcile_previous_pre_move_failure(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        direction: RetainedPreviousMoveDirection,
        source: Error,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), RetainedPreviousMoveFailure> {
        let layout = self
            .revalidate_previous_move_base(installation, attempt)
            .and_then(|()| self.previous_move_layout(attempt));
        match layout {
            Ok(layout) if layout == direction.before() => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::NotApplied,
                source,
            }),
            Ok(layout) if layout == direction.after() => self
                .finish_previous_move(installation, attempt, direction, guard)
                .map_err(|finish| RetainedPreviousMoveFailure {
                    outcome: RetainedPreviousMoveOutcome::Applied,
                    source: Error::PreviousMoveAppliedAfterPreflightFailure {
                        direction: direction.as_str(),
                        primary: Box::new(source),
                        finish: Box::new(finish),
                    },
                }),
            Ok(layout) => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::Ambiguous,
                source: Error::PreviousMoveUnexpectedLayout {
                    direction: direction.as_str(),
                    expected: direction.before().as_str(),
                    actual: layout.as_str(),
                },
            }),
            Err(reconciliation) => Err(RetainedPreviousMoveFailure {
                outcome: RetainedPreviousMoveOutcome::Ambiguous,
                source: Error::PreviousMovePreflightReconciliationFailed {
                    direction: direction.as_str(),
                    primary: Box::new(source),
                    reconciliation: Box::new(reconciliation),
                },
            }),
        }
    }

    /// Choose the parking name a completed archive can be reversed to, without
    /// mutating anything.
    ///
    /// The archive itself consumes this evidence: publishing renames the slot
    /// from its parking name into the canonical decimal state name, after which
    /// the namespace no longer records where it came from. So the name has to be
    /// durable in the journal *before* the archive runs, which means choosing it
    /// has to be separable from creating the slot — that is all this does
    /// (`plans/previous-restore-recovery-identity.md`, D-PR1).
    ///
    /// Read-only by construction: the reuse scan only authenticates an existing
    /// marker-only wrapper, and the fresh branch only probes for a free name.
    pub(super) fn select_previous_archive_slot(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<PreviousArchiveSlot, Error> {
        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())?;
        let staging = roots.open_child(c"staging", installation.staging_dir())?;

        if let Some(reusable) = self.find_reusable_previous_state_slot(installation, &roots, &staging, state)? {
            return Ok(PreviousArchiveSlot {
                parking_name: quarantine_name_of(&reusable.parking_name)?,
                reused_wrapper: true,
            });
        }

        for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
            let parking_name = previous_slot_parking_name(state, self.previous.marker.token().as_str(), index)?;
            let parking_path = roots_path.join(parking_name.to_string_lossy().as_ref());
            if roots.child_name_exists(&parking_name, parking_path)? {
                continue;
            }
            return Ok(PreviousArchiveSlot {
                parking_name: quarantine_name_of(&parking_name)?,
                reused_wrapper: false,
            });
        }
        Err(Error::PreviousArchiveParkingExhausted {
            state: i32::from(state),
            limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
        })
    }

    /// Rebuild the archive attempt from disk so a completed archive can be
    /// reversed after a restart.
    ///
    /// `move_previous` refuses a `Restore` when no attempt is retained, and the
    /// attempt is per-process in-memory state that dies with the transition that
    /// made it. That is the second of the two walls this plan documents: even
    /// given an identity, restore could not proceed
    /// (`plans/previous-restore-recovery-identity.md`).
    ///
    /// Four fields come straight from the namespace. The two that drive slot
    /// retirement — `parking_name` and `state_slot_marker` — come from the
    /// **record**, because a successful archive consumed the on-disk evidence
    /// when it renamed the slot into its canonical name. That is exactly what
    /// D-PR1 recorded them for.
    #[cfg(test)]
    pub(crate) fn adopt_previous_archive_attempt_for_test(
        &self,
        installation: &Installation,
        state: state::Id,
        recorded: &PreviousArchiveSlot,
    ) -> Result<(), Error> {
        self.adopt_previous_archive_attempt(installation, state, recorded)
    }

    /// Rebuild the archive attempt from the parking name the journal recorded.
    ///
    /// `pub(crate)` because its production caller is the `PreviousRestore`
    /// rollback dispatcher in the `client` layer above this module, exactly as
    /// [`PreviousRestoreRecoverySeal`] documents.
    pub(crate) fn adopt_previous_archive_attempt(
        &self,
        installation: &Installation,
        state: state::Id,
        recorded: &PreviousArchiveSlot,
    ) -> Result<(), Error> {
        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())?;
        let staging = roots.open_child(c"staging", installation.staging_dir())?;

        // The published slot must already exist: adoption reverses a *completed*
        // archive, so a missing canonical name means the namespace disagrees
        // with the record.
        let name = canonical_state_name(state)?;
        let slot_path = roots_path.join(name.to_string_lossy().as_ref());
        let slot = roots
            .open_optional_child(&name, slot_path.clone())?
            .ok_or(Error::PreviousArchiveSlotRecordUnusable {
                state: i32::from(state),
            })?;

        let parking_name = CString::new(recorded.parking_name.as_str()).map_err(|_| {
            Error::PreviousArchiveSlotRecordUnusable {
                state: i32::from(state),
            }
        })?;
        // The parking name must be free — publication consumed it. If something
        // occupies it, retirement would collide and this is not a namespace the
        // record describes.
        if roots.child_name_exists(&parking_name, roots_path.join(recorded.parking_name.as_str()))? {
            return Err(Error::PreviousArchiveSlotRecordUnusable {
                state: i32::from(state),
            });
        }

        let state_slot_marker = if recorded.reused_wrapper {
            Some(state_slot_marker::RetainedStateSlotMarker::open_expected(&slot, state, &self.previous.marker)?)
        } else {
            None
        };

        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| Error::PreviousArchiveAttemptLockPoisoned)?;
        *retained = Some(RetainedPreviousArchiveAttempt {
            name,
            parking_name,
            roots,
            staging,
            slot,
            state_slot_marker,
        });
        Ok(())
    }

    fn create_previous_archive_attempt(
        &self,
        installation: &Installation,
        state: state::Id,
        name: &CStr,
        recorded: Option<&PreviousArchiveSlot>,
    ) -> Result<RetainedPreviousArchiveAttempt, Error> {
        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())?;
        let staging = roots.open_child(c"staging", installation.staging_dir())?;
        let canonical_path = roots_path.join(name.to_string_lossy().as_ref());
        if roots.open_optional_child(name, canonical_path.clone())?.is_some() {
            return Err(Error::PreviousArchiveSlotExists {
                state: i32::from(state),
                path: canonical_path,
            });
        }
        self.previous_move_layout_without_slot(&staging)?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::PreviousPreSync)?;
        self.previous.store.sync_retained_tree()?;
        self.previous_move_layout_without_slot(&staging)?;

        // Prefer the exact marker-only wrapper retained when this state was
        // activated. Reusing it returns its deterministic parking name to the
        // free pool after a later restore instead of leaking one wrapper per
        // successful activation.
        let reusable = self.find_reusable_previous_state_slot(installation, &roots, &staging, state)?;

        // A journal-coordinated archive already recorded which name it will use,
        // before that evidence could be consumed by the publishing rename. Honour
        // it rather than re-deciding: a second, independent choice could disagree
        // with the record, and the record is what a later rollback trusts
        // (`plans/previous-restore-recovery-identity.md`, D-PR1).
        if let Some(recorded) = recorded {
            let expected = CString::new(recorded.parking_name.as_str())
                .map_err(|_| Error::PreviousArchiveSlotRecordUnusable {
                    state: i32::from(state),
                })?;
            return self.create_recorded_previous_archive_attempt(
                &roots,
                &roots_path,
                name,
                state,
                &expected,
                recorded.reused_wrapper,
                reusable,
            );
        }

        let (parking_name, slot, state_slot_marker) = match reusable {
            Some(reusable) => (reusable.parking_name, reusable.slot, Some(reusable.marker)),
            None => {
                // Prepare a fresh slot at a bounded, non-state parking name.
                // Publication into the decimal state name remains a separate
                // reconciled no-replace rename.
                let mut created = None;
                for index in 0..MAX_PREVIOUS_SLOT_PARKING_CANDIDATES {
                    let parking_name = previous_slot_parking_name(state, self.previous.marker.token().as_str(), index)?;
                    let parking_path = roots_path.join(parking_name.to_string_lossy().as_ref());
                    if roots.child_name_exists(&parking_name, parking_path.clone())? {
                        continue;
                    }
                    match RetainedDirectory::create_private_previous_slot(&roots, &parking_name, parking_path) {
                        Ok(slot) => {
                            created = Some((parking_name, slot));
                            break;
                        }
                        Err(Error::QuarantineSlotExists { .. }) => continue,
                        Err(source) => return Err(source),
                    }
                }
                let (parking_name, slot) = created.ok_or_else(|| Error::PreviousArchiveParkingExhausted {
                    state: i32::from(state),
                    limit: MAX_PREVIOUS_SLOT_PARKING_CANDIDATES,
                })?;
                (parking_name, slot, None)
            }
        };
        let attempt = RetainedPreviousArchiveAttempt {
            name: name.to_owned(),
            parking_name,
            roots,
            staging,
            slot,
            state_slot_marker,
        };
        Ok(attempt)
    }

    /// Build the archive attempt at the name the journal already recorded.
    ///
    /// Deliberately does *not* fall back to another index if the recorded name
    /// is taken. The un-recorded path renumbers because any free name will do;
    /// here the name is durable evidence a later rollback depends on, so a
    /// collision means the namespace disagrees with the record and the honest
    /// answer is to stop. A transition holds the journal, so no other transition
    /// can be competing — the only racer is a hostile same-UID writer, which
    /// this codebase already treats as adversarial.
    #[allow(clippy::too_many_arguments)]
    fn create_recorded_previous_archive_attempt(
        &self,
        roots: &RetainedDirectory,
        roots_path: &Path,
        name: &CStr,
        state: state::Id,
        parking_name: &CStr,
        reused_wrapper: bool,
        reusable: Option<ReusablePreviousStateSlot>,
    ) -> Result<RetainedPreviousArchiveAttempt, Error> {
        let staging = roots.open_child(c"staging", roots_path.join("staging"))?;
        let parking_path = roots_path.join(parking_name.to_string_lossy().as_ref());

        let (slot, state_slot_marker) = if reused_wrapper {
            // The record says a wrapper was reused, so one must still be there
            // under exactly that name.
            let reusable = reusable.filter(|found| found.parking_name.as_c_str() == parking_name).ok_or(
                Error::PreviousArchiveSlotRecordUnusable {
                    state: i32::from(state),
                },
            )?;
            (reusable.slot, Some(reusable.marker))
        } else {
            if roots.child_name_exists(parking_name, parking_path.clone())? {
                return Err(Error::PreviousArchiveSlotRecordUnusable {
                    state: i32::from(state),
                });
            }
            let slot = RetainedDirectory::create_private_previous_slot(roots, parking_name, parking_path)?;
            (slot, None)
        };

        Ok(RetainedPreviousArchiveAttempt {
            name: name.to_owned(),
            parking_name: parking_name.to_owned(),
            roots: roots.clone_retained()?,
            staging,
            slot,
            state_slot_marker,
        })
    }

    fn finish_previous_archive_slot_creation(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        attempt.require_slot_without_tree()?;
        match self.previous_slot_location(attempt)? {
            RetainedPreviousSlotLocation::Parked => {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeSlotPublish)?;
                let syscall_result = renameat2_noreplace_once(
                    &attempt.roots.file,
                    &attempt.parking_name,
                    &attempt.roots.file,
                    &attempt.name,
                )
                .map_err(|source| {
                    previous_move_io(
                        "publish exact previous-state slot",
                        &previous_slot_canonical_path(attempt),
                        source,
                    )
                })
                .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterSlotPublish));

                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Canonical => {}
                    RetainedPreviousSlotLocation::Parked => {
                        return Err(match syscall_result {
                            Err(source) => source,
                            Ok(()) => Error::PreviousArchiveSlotPublishReportedSuccessWithoutMove {
                                canonical: previous_slot_canonical_path(attempt),
                                parking: previous_slot_parking_path(attempt),
                            },
                        });
                    }
                }
            }
            RetainedPreviousSlotLocation::Canonical => {}
        }

        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::SlotSync)?;
        attempt.sync_reusable_marker()?;
        attempt.slot.sync("sync previous-state archive slot")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::RootsParentSync)?;
        attempt
            .roots
            .sync("sync roots directory after previous-state slot creation")?;
        self.revalidate_previous_move_namespace(installation, attempt)?;
        self.require_previous_move_layout(attempt, RetainedPreviousMoveLayout::Staged)?;
        Ok(())
    }

    fn finish_applied_previous_move(
        &self,
        installation: &Installation,
        state: state::Id,
        direction: RetainedPreviousMoveDirection,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), Error> {
        let name = canonical_state_name(state)?;
        let mut retained = self
            .previous_archive_attempt
            .lock()
            .map_err(|_| Error::PreviousArchiveAttemptLockPoisoned)?;
        let attempt = retained.as_ref().ok_or(Error::PreviousArchiveAttemptMissing {
            state: i32::from(state),
        })?;
        require_previous_attempt_name(attempt, state, &name)?;
        self.revalidate_previous_move_base(installation, attempt)?;
        if direction == RetainedPreviousMoveDirection::Archive {
            self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
        }
        self.require_previous_move_layout(attempt, direction.after())?;
        let finish = self.finish_previous_move(installation, attempt, direction, guard);
        if finish.is_ok() && direction == RetainedPreviousMoveDirection::Restore {
            *retained = None;
        }
        finish
    }

    fn finish_previous_move(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        direction: RetainedPreviousMoveDirection,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), Error> {
        let (source, destination) = match direction {
            RetainedPreviousMoveDirection::Archive => (&attempt.staging, &attempt.slot),
            RetainedPreviousMoveDirection::Restore => (&attempt.slot, &attempt.staging),
        };
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::SourceParentSync)?;
        source.sync("sync previous-tree source parent after move")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::DestinationParentSync)?;
        destination.sync("sync previous-tree destination parent after move")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::FinalRevalidation)?;
        guard.require(self)?;
        installation.revalidate_root_directory()?;
        self.revalidate_previous_move_base(installation, attempt)?;
        if direction == RetainedPreviousMoveDirection::Archive {
            self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
        }
        self.require_previous_move_layout(attempt, direction.after())?;
        if direction == RetainedPreviousMoveDirection::Restore {
            self.finish_previous_slot_retirement(installation, attempt, guard)?;
        }
        Ok(())
    }

    fn revalidate_previous_move_namespace(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)
    }

    fn revalidate_previous_move_base(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<(), Error> {
        installation.revalidate_root_directory()?;
        attempt
            .roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)?;
        attempt.staging.revalidate_child(&attempt.roots, c"staging")?;
        attempt.require_reusable_marker()?;
        if attempt.roots.witness.device != attempt.staging.witness.device
            || attempt.roots.witness.device != attempt.slot.witness.device
        {
            return Err(Error::PreviousMoveCrossDevice {
                staging: attempt.staging.path.clone(),
                archive: attempt.slot.path.clone(),
            });
        }
        Ok(())
    }

    fn previous_slot_location(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<RetainedPreviousSlotLocation, Error> {
        attempt.slot.require_retained()?;
        let canonical_path = previous_slot_canonical_path(attempt);
        let parking_path = previous_slot_parking_path(attempt);
        let canonical = attempt
            .roots
            .open_optional_child(&attempt.name, canonical_path.clone())?;
        let parking = attempt
            .roots
            .open_optional_child(&attempt.parking_name, parking_path.clone())?;
        let state = |named: Option<&RetainedDirectory>| match named {
            None => RetainedPreviousSlotNameState::Absent,
            Some(named) if named.witness == attempt.slot.witness => RetainedPreviousSlotNameState::Exact,
            Some(_) => RetainedPreviousSlotNameState::Foreign,
        };
        let canonical_state = state(canonical.as_ref());
        let parking_state = state(parking.as_ref());
        match (canonical_state, parking_state) {
            (RetainedPreviousSlotNameState::Exact, RetainedPreviousSlotNameState::Absent) => {
                Ok(RetainedPreviousSlotLocation::Canonical)
            }
            (RetainedPreviousSlotNameState::Absent, RetainedPreviousSlotNameState::Exact) => {
                Ok(RetainedPreviousSlotLocation::Parked)
            }
            _ => Err(Error::PreviousArchiveSlotNamespaceMismatch {
                canonical: canonical_path,
                canonical_state: canonical_state.as_str(),
                parking: parking_path,
                parking_state: parking_state.as_str(),
            }),
        }
    }

    fn require_previous_slot_location(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
        expected: RetainedPreviousSlotLocation,
    ) -> Result<(), Error> {
        let actual = self.previous_slot_location(attempt)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::PreviousArchiveSlotLocationMismatch {
                canonical: previous_slot_canonical_path(attempt),
                parking: previous_slot_parking_path(attempt),
                expected: match expected {
                    RetainedPreviousSlotLocation::Canonical => "canonical",
                    RetainedPreviousSlotLocation::Parked => "parked",
                },
                actual: match actual {
                    RetainedPreviousSlotLocation::Canonical => "canonical",
                    RetainedPreviousSlotLocation::Parked => "parked",
                },
            })
        }
    }

    /// Move an exact inert canonical slot back to its private parking name.
    ///
    /// This is intentionally non-destructive. A same-UID writer can replace a
    /// final pathname after it is checked, so `unlinkat` cannot safely remove
    /// the retained inode. A no-replace rename preserves every racing inode,
    /// and exact post-syscall reconciliation makes the durability suffix
    /// resumable without issuing a second rename.
    fn finish_previous_slot_retirement(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), Error> {
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_move_layout(attempt, RetainedPreviousMoveLayout::Staged)?;
        attempt.require_slot_without_tree()?;
        match self.previous_slot_location(attempt)? {
            RetainedPreviousSlotLocation::Canonical => {
                retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::BeforeSlotRetire)?;
                before_previous_slot_retirement_rename();
                let syscall_result = renameat2_noreplace_once(
                    &attempt.roots.file,
                    &attempt.name,
                    &attempt.roots.file,
                    &attempt.parking_name,
                )
                .map_err(|source| {
                    previous_move_io(
                        "retire exact previous-state slot",
                        &previous_slot_parking_path(attempt),
                        source,
                    )
                })
                .and_then(|()| retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::AfterSlotRetire));

                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Parked => {}
                    RetainedPreviousSlotLocation::Canonical => {
                        return Err(match syscall_result {
                            Err(source) => source,
                            Ok(()) => Error::PreviousArchiveSlotRetireReportedSuccessWithoutMove {
                                canonical: previous_slot_canonical_path(attempt),
                                parking: previous_slot_parking_path(attempt),
                            },
                        });
                    }
                }
            }
            RetainedPreviousSlotLocation::Parked => {}
        }

        self.finish_parked_previous_slot_retirement(installation, attempt, guard)
    }

    fn finish_parked_previous_slot_retirement(
        &self,
        installation: &Installation,
        attempt: &RetainedPreviousArchiveAttempt,
        guard: ArchiveJournalGuard<'_>,
    ) -> Result<(), Error> {
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Parked)?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync)?;
        attempt
            .roots
            .sync("sync roots directory after previous-state slot retirement")?;
        retained_previous_move_checkpoint(RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation)?;
        guard.require(self)?;
        self.revalidate_previous_move_base(installation, attempt)?;
        self.require_previous_move_layout(attempt, RetainedPreviousMoveLayout::Staged)?;
        self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Parked)
    }

    pub(super) fn previous_move_layout_without_slot(
        &self,
        staging: &RetainedDirectory,
    ) -> Result<RetainedPreviousMoveLayout, Error> {
        let staged = open_optional_retained_tree(staging, &staging.path.join("usr"))?.ok_or_else(|| {
            Error::PreviousMoveTreeMissing {
                staged: staging.path.join("usr"),
                archived: PathBuf::from("<uncreated-state-slot>/usr"),
            }
        })?;
        self.previous.verify_store_read_only(&staged)?;
        Ok(RetainedPreviousMoveLayout::Staged)
    }

    fn previous_move_layout(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
    ) -> Result<RetainedPreviousMoveLayout, Error> {
        let staged_path = attempt.staging.path.join("usr");
        let archived_path = attempt.slot.path.join("usr");
        let staged = open_optional_retained_tree(&attempt.staging, &staged_path)?;
        let archived = open_optional_retained_tree(&attempt.slot, &archived_path)?;
        match (staged, archived) {
            (Some(staged), None) => {
                self.previous.verify_store_read_only(&staged)?;
                match self.previous_slot_location(attempt)? {
                    RetainedPreviousSlotLocation::Canonical | RetainedPreviousSlotLocation::Parked => {}
                }
                attempt.require_slot_without_tree()?;
                Ok(RetainedPreviousMoveLayout::Staged)
            }
            (None, Some(archived)) => {
                self.require_previous_slot_location(attempt, RetainedPreviousSlotLocation::Canonical)?;
                self.previous.verify_store_read_only(&archived)?;
                attempt.require_slot_with_tree()?;
                Ok(RetainedPreviousMoveLayout::Archived)
            }
            (Some(_), Some(_)) => Err(Error::PreviousMoveBothNamesOccupied {
                staged: staged_path,
                archived: archived_path,
            }),
            (None, None) => Err(Error::PreviousMoveTreeMissing {
                staged: staged_path,
                archived: archived_path,
            }),
        }
    }

    fn require_previous_move_layout(
        &self,
        attempt: &RetainedPreviousArchiveAttempt,
        expected: RetainedPreviousMoveLayout,
    ) -> Result<(), Error> {
        let actual = self.previous_move_layout(attempt)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::PreviousMoveUnexpectedLayout {
                direction: "preflight",
                expected: expected.as_str(),
                actual: actual.as_str(),
            })
        }
    }
}
