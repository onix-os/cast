//! Non-destructive replacement of the fixed stateful staging wrapper.
//!
//! Trigger execution may leave arbitrary entries beside `usr`, so securely
//! emptying that wrapper would require the still-pending descriptor-recursive
//! inventory and deletion boundary. Instead, this module creates one exact
//! empty wrapper in the private quarantine namespace and exchanges the two
//! retained directory names. The original wrapper and every byte below it are
//! preserved without traversal.

use std::{ffi::CString, path::PathBuf};

use thiserror::Error;

use super::{
    QUARANTINE_RELATIVE, ROOTS_RELATIVE, RetainedDirectory, StatefulTreeIdentity, canonical_state_name,
    open_optional_retained_tree,
};
use crate::{
    Installation, db,
    linux_fs::{chmod_path_descriptor, renameat2_exchange_once},
    state::{self, State},
    transition_journal::QuarantineName,
};

const STAGING_NAME: &std::ffi::CStr = c"staging";
const MAX_QUARANTINE_NAMES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedStagingWrapperRotationOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
#[error("retained staging-wrapper rotation outcome is {outcome:?}")]
pub(crate) struct RetainedStagingWrapperRotationFailure {
    outcome: RetainedStagingWrapperRotationOutcome,
    #[source]
    source: StagingWrapperRotationError,
}

impl RetainedStagingWrapperRotationFailure {
    pub(crate) fn outcome(&self) -> RetainedStagingWrapperRotationOutcome {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedStagingWrapperRotationFaultPoint {
    ReplacementPostCreate,
    ReplacementPreparationSync,
    QuarantinePreparationSync,
    FinalPreparationRevalidation,
    OriginalPreSync,
    ReplacementPreSync,
    QuarantinePreSync,
    BeforeExchange,
    AfterExchange,
    OriginalPostSync,
    ReplacementPostSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalRevalidation,
}

#[derive(Debug, Error)]
enum StagingWrapperRotationError {
    #[error("{operation} while rotating the retained staging wrapper: {source}")]
    Identity {
        operation: &'static str,
        #[source]
        source: Box<super::Error>,
    },
    #[error("construct the private staging-wrapper quarantine name")]
    InvalidName(#[source] crate::transition_journal::CodecError),
    #[error("all {limit} private staging-wrapper quarantine names are occupied")]
    DestinationExhausted { limit: usize },
    #[error(
        "staging wrapper and quarantine replacement are on different filesystems: `{}` and `{}`",
        staging.display(),
        quarantine.display()
    )]
    CrossDevice { staging: PathBuf, quarantine: PathBuf },
    #[error("retained staging-wrapper namespace mismatch: staging={staging}, quarantine={quarantine}")]
    NamespaceMismatch {
        staging: &'static str,
        quarantine: &'static str,
    },
    #[error("staging-wrapper exchange reported success without moving either exact wrapper")]
    ReportedSuccessWithoutMove,
    #[error("retained active-reblit staging rotation lock is poisoned")]
    AttemptLockPoisoned,
    #[error("no retained active-reblit staging rotation was reserved")]
    AttemptMissing,
    #[error("an active-reblit staging rotation is already reserved")]
    AttemptAlreadyReserved,
    #[error("park the retained active previous-state slot before the live /usr exchange")]
    ActivePreviousSlotParking(
        #[source] Box<super::active_previous_slot_parking::RetainedActivePreviousSlotParkingFailure>,
    ),
    #[error("exchange retained staging wrapper at `{}`", path.display())]
    Exchange {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("staging-wrapper preflight failed ({primary}) and exact reconciliation failed ({reconciliation})")]
    PreflightReconciliationFailed {
        primary: Box<StagingWrapperRotationError>,
        reconciliation: Box<StagingWrapperRotationError>,
    },
    #[cfg(test)]
    #[error("injected retained staging-wrapper fault at {point:?}")]
    InjectedFault {
        point: RetainedStagingWrapperRotationFaultPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WrapperLayout {
    OriginalStaged,
    OriginalQuarantined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedWrapper {
    Absent,
    Original,
    Replacement,
    Foreign,
}

impl NamedWrapper {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Original => "original",
            Self::Replacement => "replacement",
            Self::Foreign => "foreign",
        }
    }
}

/// Retains both wrapper inodes and both parent namespaces until the exchange
/// and every durability barrier have been proven.
#[derive(Debug)]
pub(super) struct RetainedStagingWrapperRotation {
    roots: RetainedDirectory,
    quarantine: RetainedDirectory,
    original: RetainedDirectory,
    replacement: RetainedDirectory,
    quarantine_name: CString,
    quarantine_path: PathBuf,
}

impl RetainedStagingWrapperRotation {
    /// Reserve and retain one fresh empty replacement. No fallible durability
    /// suffix runs after this returns, so callers can store the attempt before
    /// invoking [`Self::finish_preparation`].
    pub(super) fn reserve(
        installation: &Installation,
        role: &'static str,
        state: state::Id,
        token: &str,
    ) -> Result<Self, RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        installation
            .revalidate_root_directory()
            .map_err(super::Error::from)
            .map_err(|source| not_applied(identity("revalidate installation root", source)))?;
        canonical_state_name(state)
            .map_err(|source| not_applied(identity("validate staging-wrapper state", source)))?;

        let roots_path = installation.root_path("");
        let roots = RetainedDirectory::open_beneath(installation.root_directory(), ROOTS_RELATIVE, roots_path.clone())
            .map_err(|source| not_applied(identity("retain roots directory", source)))?;
        let preliminary = roots
            .open_child(STAGING_NAME, installation.staging_dir())
            .map_err(|source| not_applied(identity("retain original staging wrapper", source)))?;
        chmod_path_descriptor(&preliminary.file, 0o700).map_err(|source| {
            not_applied(identity(
                "normalize original staging wrapper mode",
                super::Error::Quarantine {
                    operation: "normalize original staging wrapper mode",
                    path: preliminary.path.clone(),
                    source,
                },
            ))
        })?;
        drop(preliminary);
        let original = roots
            .open_child(STAGING_NAME, installation.staging_dir())
            .map_err(|source| not_applied(identity("reopen normalized staging wrapper", source)))?;
        if original.witness.mode != 0o700 {
            return Err(not_applied(identity(
                "require normalized staging wrapper mode",
                super::Error::UnsafeQuarantineDirectory {
                    path: original.path.clone(),
                    owner: original.witness.owner,
                    mode: original.witness.mode,
                },
            )));
        }
        original
            .require_exact_entries(&[b"usr"])
            .map_err(|source| not_applied(identity("require exact active-reblit staging wrapper", source)))?;
        let quarantine_path = installation.state_quarantine_dir();
        let quarantine = RetainedDirectory::open_beneath(
            installation.root_directory(),
            QUARANTINE_RELATIVE,
            quarantine_path.clone(),
        )
        .map_err(|source| not_applied(identity("retain quarantine directory", source)))?;
        if roots.witness.device != original.witness.device || roots.witness.device != quarantine.witness.device {
            return Err(not_applied(StagingWrapperRotationError::CrossDevice {
                staging: original.path.clone(),
                quarantine: quarantine.path.clone(),
            }));
        }

        let mut reserved = None;
        for index in 0..MAX_QUARANTINE_NAMES {
            let name = QuarantineName::parse(format!("replaced-{role}-{}-{token}-{index}", i32::from(state)))
                .map_err(|source| not_applied(StagingWrapperRotationError::InvalidName(source)))?;
            let quarantine_name = CString::new(name.as_str()).expect("validated quarantine name contains no NUL");
            let replacement_path = quarantine.path.join(name.as_str());
            if quarantine
                .child_name_exists(&quarantine_name, replacement_path.clone())
                .map_err(|source| not_applied(identity("probe staging-wrapper quarantine name", source)))?
            {
                continue;
            }
            match RetainedDirectory::create_private_child(&quarantine, &quarantine_name, replacement_path.clone()) {
                Ok(replacement) => {
                    reserved = Some((quarantine_name, replacement_path, replacement));
                    break;
                }
                Err(super::Error::QuarantineSlotExists { .. }) => continue,
                Err(source) => {
                    return Err(not_applied(identity("create empty staging replacement", source)));
                }
            }
        }
        let (quarantine_name, replacement_path, replacement) = reserved.ok_or_else(|| {
            not_applied(StagingWrapperRotationError::DestinationExhausted {
                limit: MAX_QUARANTINE_NAMES,
            })
        })?;

        let rotation = Self {
            roots,
            quarantine,
            original,
            replacement,
            quarantine_name,
            quarantine_path: replacement_path,
        };
        Ok(rotation)
    }

    /// Finish every post-mkdir durability and identity check. Callers retain
    /// `self` before entering this suffix, so a fault never loses authority to
    /// the exact empty replacement.
    pub(super) fn finish_preparation(
        &self,
        installation: &Installation,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        checkpoint(RetainedStagingWrapperRotationFaultPoint::ReplacementPostCreate).map_err(not_applied)?;
        self.replacement
            .require_exact_entries(&[])
            .map_err(|source| not_applied(identity("validate empty staging replacement", source)))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::ReplacementPreparationSync).map_err(not_applied)?;
        self.replacement
            .sync("sync empty staging replacement")
            .map_err(|source| not_applied(identity("sync empty staging replacement", source)))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::QuarantinePreparationSync).map_err(not_applied)?;
        self.quarantine
            .sync("sync quarantine after staging replacement creation")
            .map_err(|source| not_applied(identity("sync staging replacement name", source)))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::FinalPreparationRevalidation).map_err(not_applied)?;
        self.revalidate_base(installation)
            .and_then(|()| self.require_layout(WrapperLayout::OriginalStaged))
            .map_err(|source| RetainedStagingWrapperRotationFailure {
                outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
                source,
            })
    }

    pub(super) fn rotate(
        &self,
        installation: &Installation,
        validate_before: &impl Fn() -> Result<(), super::Error>,
        validate_after: &impl Fn() -> Result<(), super::Error>,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
            source,
        };

        self.revalidate_base(installation).map_err(ambiguous)?;
        match self.layout().map_err(ambiguous)? {
            WrapperLayout::OriginalQuarantined => {
                return self.finish(installation, validate_after).map_err(applied);
            }
            WrapperLayout::OriginalStaged => {}
        }

        let preflight = (|| -> Result<(), StagingWrapperRotationError> {
            checkpoint(RetainedStagingWrapperRotationFaultPoint::OriginalPreSync)?;
            self.original
                .sync("sync original staging wrapper before rotation")
                .map_err(|source| identity("sync original staging wrapper", source))?;
            checkpoint(RetainedStagingWrapperRotationFaultPoint::ReplacementPreSync)?;
            self.replacement
                .sync("sync empty staging replacement before rotation")
                .map_err(|source| identity("sync staging replacement", source))?;
            checkpoint(RetainedStagingWrapperRotationFaultPoint::QuarantinePreSync)?;
            self.quarantine
                .sync("sync quarantine before staging-wrapper rotation")
                .map_err(|source| identity("sync quarantine before staging rotation", source))?;
            before_exchange();
            self.revalidate_base(installation)?;
            self.require_layout(WrapperLayout::OriginalStaged)?;
            // Keep the semantic witness last: the hook and every namespace
            // reopen run first, then journal/state/tree identity is rebound
            // immediately before the fault checkpoint and single exchange.
            validate_before().map_err(|source| identity("validate transition before staging rotation", source))?;
            checkpoint(RetainedStagingWrapperRotationFaultPoint::BeforeExchange)
        })();
        if let Err(source) = preflight {
            return self.reconcile_preflight_failure(installation, validate_after, source);
        }

        let syscall_result = renameat2_exchange_once(
            &self.roots.file,
            STAGING_NAME,
            &self.quarantine.file,
            &self.quarantine_name,
        )
        .map_err(|source| StagingWrapperRotationError::Exchange {
            path: self.quarantine_path.clone(),
            source,
        })
        .and_then(|()| checkpoint(RetainedStagingWrapperRotationFaultPoint::AfterExchange));

        match self.layout().map_err(ambiguous)? {
            WrapperLayout::OriginalStaged => {
                let source = syscall_result
                    .err()
                    .unwrap_or(StagingWrapperRotationError::ReportedSuccessWithoutMove);
                Err(not_applied(source))
            }
            WrapperLayout::OriginalQuarantined => self.finish(installation, validate_after).map_err(applied),
        }
    }

    pub(super) fn quarantined_wrapper_path(&self) -> &std::path::Path {
        &self.quarantine_path
    }

    pub(super) fn original_wrapper(&self) -> &RetainedDirectory {
        &self.original
    }

    pub(super) fn require_completed(
        &self,
        installation: &Installation,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        self.revalidate_base(installation)
            .and_then(|()| self.require_layout(WrapperLayout::OriginalQuarantined))
            .map_err(|source| RetainedStagingWrapperRotationFailure {
                outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
                source,
            })
    }

    fn reconcile_preflight_failure(
        &self,
        installation: &Installation,
        validate_transition: &impl Fn() -> Result<(), super::Error>,
        source: StagingWrapperRotationError,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let layout = self.revalidate_base(installation).and_then(|()| self.layout());
        match layout {
            Ok(WrapperLayout::OriginalStaged) => Err(RetainedStagingWrapperRotationFailure {
                outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
                source,
            }),
            Ok(WrapperLayout::OriginalQuarantined) => {
                self.finish(installation, validate_transition)
                    .map_err(|finish| RetainedStagingWrapperRotationFailure {
                        outcome: RetainedStagingWrapperRotationOutcome::Applied,
                        source: StagingWrapperRotationError::PreflightReconciliationFailed {
                            primary: Box::new(source),
                            reconciliation: Box::new(finish),
                        },
                    })
            }
            Err(reconciliation) => Err(RetainedStagingWrapperRotationFailure {
                outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
                source: StagingWrapperRotationError::PreflightReconciliationFailed {
                    primary: Box::new(source),
                    reconciliation: Box::new(reconciliation),
                },
            }),
        }
    }

    fn finish(
        &self,
        installation: &Installation,
        validate_transition: &impl Fn() -> Result<(), super::Error>,
    ) -> Result<(), StagingWrapperRotationError> {
        checkpoint(RetainedStagingWrapperRotationFaultPoint::OriginalPostSync)?;
        self.original
            .sync("sync quarantined original staging wrapper")
            .map_err(|source| identity("sync quarantined original staging wrapper", source))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::ReplacementPostSync)?;
        self.replacement
            .sync("sync fixed empty staging replacement")
            .map_err(|source| identity("sync fixed empty staging replacement", source))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::RootsParentSync)?;
        self.roots
            .sync("sync roots after staging-wrapper rotation")
            .map_err(|source| identity("sync roots after staging-wrapper rotation", source))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::QuarantineParentSync)?;
        self.quarantine
            .sync("sync quarantine after staging-wrapper rotation")
            .map_err(|source| identity("sync quarantine after staging-wrapper rotation", source))?;
        checkpoint(RetainedStagingWrapperRotationFaultPoint::FinalRevalidation)?;
        validate_transition().map_err(|source| identity("validate transition after staging rotation", source))?;
        self.revalidate_base(installation)?;
        self.require_layout(WrapperLayout::OriginalQuarantined)
    }

    fn revalidate_base(&self, installation: &Installation) -> Result<(), StagingWrapperRotationError> {
        installation
            .revalidate_root_directory()
            .map_err(super::Error::from)
            .map_err(|source| identity("revalidate installation root", source))?;
        self.roots
            .revalidate_beneath(installation.root_directory(), ROOTS_RELATIVE)
            .map_err(|source| identity("revalidate roots directory", source))?;
        self.quarantine
            .revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)
            .map_err(|source| identity("revalidate quarantine directory", source))?;
        self.original
            .require_retained()
            .map_err(|source| identity("revalidate original staging wrapper", source))?;
        self.replacement
            .require_retained()
            .map_err(|source| identity("revalidate staging replacement", source))?;
        self.replacement
            .require_exact_entries(&[])
            .map_err(|source| identity("require empty staging replacement", source))?;
        Ok(())
    }

    fn layout(&self) -> Result<WrapperLayout, StagingWrapperRotationError> {
        let staged = self
            .roots
            .open_optional_child(STAGING_NAME, self.original.path.clone())
            .map_err(|source| identity("open fixed staging wrapper", source))?;
        let quarantined = self
            .quarantine
            .open_optional_child(&self.quarantine_name, self.quarantine_path.clone())
            .map_err(|source| identity("open staging-wrapper quarantine name", source))?;
        let role = |named: Option<&RetainedDirectory>| match named {
            None => NamedWrapper::Absent,
            Some(named) if named.witness == self.original.witness => NamedWrapper::Original,
            Some(named) if named.witness == self.replacement.witness => NamedWrapper::Replacement,
            Some(_) => NamedWrapper::Foreign,
        };
        let staged = role(staged.as_ref());
        let quarantined = role(quarantined.as_ref());
        match (staged, quarantined) {
            (NamedWrapper::Original, NamedWrapper::Replacement) => Ok(WrapperLayout::OriginalStaged),
            (NamedWrapper::Replacement, NamedWrapper::Original) => Ok(WrapperLayout::OriginalQuarantined),
            _ => Err(StagingWrapperRotationError::NamespaceMismatch {
                staging: staged.as_str(),
                quarantine: quarantined.as_str(),
            }),
        }
    }

    fn require_layout(&self, expected: WrapperLayout) -> Result<(), StagingWrapperRotationError> {
        let actual = self.layout()?;
        if actual == expected {
            Ok(())
        } else {
            Err(StagingWrapperRotationError::NamespaceMismatch {
                staging: match actual {
                    WrapperLayout::OriginalStaged => "original",
                    WrapperLayout::OriginalQuarantined => "replacement",
                },
                quarantine: match actual {
                    WrapperLayout::OriginalStaged => "replacement",
                    WrapperLayout::OriginalQuarantined => "original",
                },
            })
        }
    }
}

impl StatefulTreeIdentity {
    pub(crate) fn has_active_reblit_staging_rotation(&self) -> Result<bool, super::Error> {
        self.active_reblit_rotation
            .lock()
            .map(|retained| retained.is_some())
            .map_err(|_| super::Error::LiveUsr {
                operation: "inspect retained active-reblit staging rotation",
                path: self.candidate.store.display_path().to_owned(),
                source: std::io::Error::other("active-reblit staging rotation lock is poisoned"),
            })
    }

    /// Reserve the exact empty replacement before a verification reblit can
    /// exchange the candidate into live `/usr`. Exhaustion or collision is
    /// therefore discovered while the old live tree is still untouched.
    pub(crate) fn prepare_active_reblit_staging_rotation(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &State,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        let state = expected.id;
        super::require_clean_baseline(&self.journal, state_db).map_err(|source| {
            not_applied(identity(
                "check transition baseline before active-reblit reservation",
                source,
            ))
        })?;
        self.candidate
            .verify_named_read_only(&installation.staging_path("usr"))
            .map_err(|source| not_applied(identity("authenticate active-reblit candidate", source)))?;
        self.previous
            .verify_named_read_only(&installation.root.join("usr"))
            .map_err(|source| not_applied(identity("authenticate live tree before active reblit", source)))?;

        let mut retained = self
            .active_reblit_rotation
            .lock()
            .map_err(|_| not_applied(StagingWrapperRotationError::AttemptLockPoisoned))?;
        if retained.is_some() {
            return Err(not_applied(StagingWrapperRotationError::AttemptAlreadyReserved));
        }
        let rotation = RetainedStagingWrapperRotation::reserve(
            installation,
            "active-reblit-wrapper",
            state,
            self.previous.marker.token().as_str(),
        )?;
        *retained = Some(rotation);

        let rotation = retained.as_ref().expect("active-reblit rotation was stored");
        let mut retried = false;
        loop {
            match rotation.finish_preparation(installation) {
                Ok(()) => break,
                Err(failure) if failure.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied && !retried => {
                    retried = true;
                }
                Err(failure) => return Err(failure),
            }
        }
        self.prepare_active_previous_slot_parking(installation, state)
            .map_err(|failure| {
                not_applied(StagingWrapperRotationError::ActivePreviousSlotParking(Box::new(
                    failure,
                )))
            })?;
        self.require_active_reblit_snapshot(installation, state_db, expected, ActiveReblitTreeLocation::Staged)
            .map_err(|source| not_applied(identity("revalidate active-reblit snapshot after reservation", source)))?;
        self.previous
            .verify_named_read_only(&installation.root.join("usr"))
            .map_err(|source| not_applied(identity("revalidate live tree after active-reblit reservation", source)))
    }

    /// Revalidate one active-reblit candidate at a trigger or exchange
    /// boundary. The marker and `.stateID` are both sandwiched around a single
    /// retained candidate directory; this method never creates or repairs
    /// either identity file.
    pub(crate) fn verify_active_reblit_candidate_snapshot(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &State,
        live: bool,
    ) -> Result<(), super::Error> {
        let retained = self.active_reblit_rotation.lock().map_err(|_| super::Error::LiveUsr {
            operation: "lock retained active-reblit staging rotation",
            path: installation.staging_dir(),
            source: std::io::Error::other("active-reblit staging rotation lock is poisoned"),
        })?;
        let _rotation = retained.as_ref().ok_or_else(|| super::Error::LiveUsr {
            operation: "load retained active-reblit staging rotation",
            path: installation.staging_dir(),
            source: std::io::Error::other("active-reblit staging rotation was not reserved"),
        })?;
        self.require_active_reblit_snapshot(
            installation,
            state_db,
            expected,
            if live {
                ActiveReblitTreeLocation::Live
            } else {
                ActiveReblitTreeLocation::Staged
            },
        )
    }

    /// Preserve the corrupt tree displaced by a successful active-state
    /// verification reblit without deleting or traversing the staging wrapper.
    ///
    /// This runs only after triggers and boot synchronization have succeeded,
    /// while the transition guard still retains the previous tree marker and
    /// journal lock. Once wrapper rotation applies, a failure is cleanup-only:
    /// callers must not try to reverse `/usr` through the now-empty fixed
    /// staging wrapper.
    pub(crate) fn rotate_active_reblit_staging(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &State,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        super::require_clean_baseline(&self.journal, state_db).map_err(|source| {
            not_applied(identity(
                "check transition baseline before active-reblit cleanup",
                source,
            ))
        })?;
        let staged_usr = installation.staging_path("usr");
        self.previous
            .verify_named_read_only(&staged_usr)
            .map_err(|source| not_applied(identity("authenticate displaced active tree", source)))?;
        self.previous
            .store
            .sync_retained_tree()
            .map_err(super::Error::from)
            .map_err(|source| not_applied(identity("sync displaced active tree", source)))?;

        let retained = self
            .active_reblit_rotation
            .lock()
            .map_err(|_| not_applied(StagingWrapperRotationError::AttemptLockPoisoned))?;
        let rotation = retained
            .as_ref()
            .ok_or_else(|| not_applied(StagingWrapperRotationError::AttemptMissing))?;
        let retained_tree = open_optional_retained_tree(rotation.original_wrapper(), &staged_usr)
            .map_err(|source| not_applied(identity("retain displaced active tree under staging", source)))?
            .ok_or_else(|| {
                not_applied(identity(
                    "require displaced active tree under staging",
                    super::Error::PreviousMoveTreeMissing {
                        staged: staged_usr.clone(),
                        archived: rotation.quarantined_wrapper_path().join("usr"),
                    },
                ))
            })?;
        self.previous
            .verify_store_read_only(&retained_tree)
            .map_err(|source| not_applied(identity("bind displaced active tree to staging wrapper", source)))?;

        let mut retried = false;
        let validate =
            || self.require_active_reblit_snapshot(installation, state_db, expected, ActiveReblitTreeLocation::Live);
        loop {
            match rotation.rotate(installation, &validate, &validate) {
                Ok(()) => break,
                Err(failure) if failure.outcome() != RetainedStagingWrapperRotationOutcome::Ambiguous && !retried => {
                    retried = true;
                }
                Err(failure) => return Err(failure),
            }
        }

        let applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::Applied,
            source,
        };
        super::require_clean_baseline(&self.journal, state_db).map_err(|source| {
            applied(identity(
                "recheck transition baseline after active-reblit cleanup",
                source,
            ))
        })?;
        let quarantined_usr = rotation.quarantined_wrapper_path().join("usr");
        self.previous
            .verify_named_read_only(&quarantined_usr)
            .map_err(|source| applied(identity("authenticate quarantined displaced active tree", source)))?;
        let quarantined_tree = rotation
            .original
            .open_child(c"usr", quarantined_usr.clone())
            .map_err(|source| applied(identity("retain quarantined displaced active tree", source)))?;
        let quarantined_store = open_optional_retained_tree(&rotation.original, &quarantined_usr)
            .map_err(|source| applied(identity("open quarantined displaced active marker store", source)))?
            .ok_or_else(|| {
                applied(identity(
                    "require quarantined displaced active tree",
                    super::Error::PreviousMoveTreeMissing {
                        staged: installation.staging_path("usr"),
                        archived: quarantined_usr.clone(),
                    },
                ))
            })?;
        self.previous
            .verify_store_read_only(&quarantined_store)
            .map_err(|source| applied(identity("bind quarantined displaced active tree", source)))?;
        quarantined_tree
            .require_retained()
            .map_err(|source| applied(identity("revalidate quarantined displaced active directory", source)))?;
        self.previous
            .verify_named_read_only(&quarantined_usr)
            .map_err(|source| applied(identity("revalidate quarantined displaced active tree", source)))?;
        self.require_active_reblit_snapshot(installation, state_db, expected, ActiveReblitTreeLocation::Live)
            .map_err(|source| applied(identity("prove final live active-reblit snapshot", source)))?;
        // This must remain the final namespace proof. It sandwiches the exact
        // quarantined original wrapper and exact empty fixed replacement after
        // both child-tree identities have been revalidated.
        rotation.require_completed(installation)
    }

    /// Preserve a failed active-reblit wrapper with the replacement reserved
    /// before triggers. The wrapper is the evidence boundary, so a corrupted
    /// candidate `.stateID` remains opaque payload and does not block recovery.
    pub(crate) fn preserve_failed_active_reblit_wrapper(
        &self,
        installation: &Installation,
        state: state::Id,
    ) -> Result<(), RetainedStagingWrapperRotationFailure> {
        let not_applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::NotApplied,
            source,
        };
        self.require_no_journal().map_err(|source| {
            not_applied(identity(
                "check journal before failed active-reblit preservation",
                source,
            ))
        })?;
        let retained = self
            .active_reblit_rotation
            .lock()
            .map_err(|_| not_applied(StagingWrapperRotationError::AttemptLockPoisoned))?;
        let rotation = retained
            .as_ref()
            .ok_or_else(|| not_applied(StagingWrapperRotationError::AttemptMissing))?;
        let layout = rotation
            .layout()
            .map_err(|source| RetainedStagingWrapperRotationFailure {
                outcome: RetainedStagingWrapperRotationOutcome::Ambiguous,
                source,
            })?;
        let candidate_path = match layout {
            WrapperLayout::OriginalStaged => installation.staging_path("usr"),
            WrapperLayout::OriginalQuarantined => rotation.quarantined_wrapper_path().join("usr"),
        };
        self.candidate
            .verify_named_read_only(&candidate_path)
            .map_err(|source| not_applied(identity("authenticate failed active-reblit candidate", source)))?;
        self.candidate
            .store
            .sync_retained_tree()
            .map_err(super::Error::from)
            .map_err(|source| not_applied(identity("sync failed active-reblit candidate", source)))?;

        if layout == WrapperLayout::OriginalStaged {
            let mut preparation_retried = false;
            loop {
                match rotation.finish_preparation(installation) {
                    Ok(()) => break,
                    Err(failure)
                        if failure.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied
                            && !preparation_retried =>
                    {
                        preparation_retried = true;
                    }
                    Err(failure) => return Err(failure),
                }
            }
        }

        let mut rotation_retried = false;
        let validate_before = || self.require_failed_active_reblit_wrapper(installation, rotation, false);
        let validate_after = || self.require_failed_active_reblit_wrapper(installation, rotation, true);
        loop {
            match rotation.rotate(installation, &validate_before, &validate_after) {
                Ok(()) => break,
                Err(failure)
                    if failure.outcome() != RetainedStagingWrapperRotationOutcome::Ambiguous && !rotation_retried =>
                {
                    rotation_retried = true;
                }
                Err(failure) => return Err(failure),
            }
        }

        let applied = |source| RetainedStagingWrapperRotationFailure {
            outcome: RetainedStagingWrapperRotationOutcome::Applied,
            source,
        };
        self.candidate
            .verify_named_read_only(&rotation.quarantined_wrapper_path().join("usr"))
            .map_err(|source| applied(identity("authenticate quarantined failed active reblit", source)))?;
        rotation.require_completed(installation)?;
        self.require_active_previous_slot_parked(installation, state)
            .map_err(|source| {
                applied(identity(
                    "prove parked active previous-state slot after failed-wrapper preservation",
                    super::Error::ActivePreviousSlotParking {
                        source: Box::new(source),
                    },
                ))
            })
    }

    fn require_failed_active_reblit_wrapper(
        &self,
        installation: &Installation,
        rotation: &RetainedStagingWrapperRotation,
        quarantined: bool,
    ) -> Result<(), super::Error> {
        self.require_no_journal()?;
        rotation.original.require_retained()?;
        let path = if quarantined {
            rotation.quarantined_wrapper_path().join("usr")
        } else {
            installation.staging_path("usr")
        };
        self.candidate.verify_named_read_only(&path)?;
        let store = open_optional_retained_tree(&rotation.original, &path)?.ok_or_else(|| {
            super::Error::PreviousMoveTreeMissing {
                staged: installation.staging_path("usr"),
                archived: rotation.quarantined_wrapper_path().join("usr"),
            }
        })?;
        self.candidate.verify_store_read_only(&store)?;
        self.candidate.verify_named_read_only(&path)
    }

    fn require_active_reblit_snapshot(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &State,
        location: ActiveReblitTreeLocation,
    ) -> Result<(), super::Error> {
        super::require_clean_baseline(&self.journal, state_db)?;
        let actual = state_db
            .get(expected.id)
            .map_err(|source| super::Error::ActiveReblitStateLookup {
                state: i32::from(expected.id),
                source,
            })?;
        if !same_state_snapshot(expected, &actual) {
            return Err(super::Error::ActiveReblitStateChanged {
                state: i32::from(expected.id),
            });
        }
        if installation.active_state != Some(expected.id) {
            return Err(super::Error::ActiveReblitSelectionChanged {
                expected: i32::from(expected.id),
                actual: installation.active_state.map(i32::from),
            });
        }

        let path = location.path(installation);
        self.require_active_previous_slot_parked(installation, expected.id)
            .map_err(|source| super::Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })?;
        self.candidate.verify_named_with_state_id(&path)?;
        self.require_active_previous_slot_parked(installation, expected.id)
            .map_err(|source| super::Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })
    }
}

#[derive(Clone, Copy)]
enum ActiveReblitTreeLocation {
    Staged,
    Live,
}

impl ActiveReblitTreeLocation {
    fn path(self, installation: &Installation) -> PathBuf {
        match self {
            Self::Staged => installation.staging_path("usr"),
            Self::Live => installation.root.join("usr"),
        }
    }
}

fn same_state_snapshot(expected: &State, actual: &State) -> bool {
    let mut expected_selections = expected.selections.clone();
    let mut actual_selections = actual.selections.clone();
    let sort = |selections: &mut Vec<state::Selection>| {
        selections.sort_by(|left, right| {
            left.package
                .cmp(&right.package)
                .then(left.explicit.cmp(&right.explicit))
                .then(left.reason.cmp(&right.reason))
        });
    };
    sort(&mut expected_selections);
    sort(&mut actual_selections);
    expected.id == actual.id
        && expected.summary == actual.summary
        && expected.description == actual.description
        && expected.created == actual.created
        && expected.kind == actual.kind
        && expected_selections == actual_selections
}

fn identity(operation: &'static str, source: super::Error) -> StagingWrapperRotationError {
    StagingWrapperRotationError::Identity {
        operation,
        source: Box::new(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::RefCell<Vec<RetainedStagingWrapperRotationFaultPoint>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_staging_wrapper_rotation_faults(
    points: impl IntoIterator<Item = RetainedStagingWrapperRotationFaultPoint>,
) {
    let mut points = points.into_iter().collect::<Vec<_>>();
    points.reverse();
    FAULT.with(|fault| *fault.borrow_mut() = points);
}

#[cfg(test)]
pub(crate) fn arm_before_staging_wrapper_exchange(hook: impl FnOnce() + 'static) {
    BEFORE_EXCHANGE.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

fn before_exchange() {
    #[cfg(test)]
    BEFORE_EXCHANGE.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

fn checkpoint(point: RetainedStagingWrapperRotationFaultPoint) -> Result<(), StagingWrapperRotationError> {
    #[cfg(test)]
    if FAULT.with(|fault| fault.borrow_mut().last().copied()) == Some(point) {
        FAULT.with(|fault| {
            fault.borrow_mut().pop();
        });
        return Err(StagingWrapperRotationError::InjectedFault { point });
    }
    let _ = point;
    Ok(())
}
