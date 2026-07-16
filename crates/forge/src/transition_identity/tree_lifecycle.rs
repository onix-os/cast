use super::candidate_state_authority::CandidateStatePreparation;
use super::*;

#[derive(Clone, Copy)]
enum ExchangeJournalGuard<'authority> {
    LegacyNoJournal,
    Coordinator(&'authority journal_coordinator::UsrExchangeEffectSeal),
}

impl ExchangeJournalGuard<'_> {
    fn require(self, identity: &StatefulTreeIdentity) -> Result<(), Error> {
        match self {
            Self::LegacyNoJournal => identity.require_no_journal(),
            Self::Coordinator(seal) => {
                let _seal = seal;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy)]
enum JournalAcquisition<'authority> {
    LegacyBlocking,
    CoordinatorNonblocking(&'authority crate::client::JournalUsrExchangePreparationSeal),
}

impl JournalAcquisition<'_> {
    fn open(
        self,
        cast: &std::fs::File,
        root: &Path,
    ) -> Result<TransitionJournalStore, crate::transition_journal::StorageError> {
        match self {
            Self::LegacyBlocking => TransitionJournalStore::open_in_retained_cast(cast, root),
            Self::CoordinatorNonblocking(seal) => {
                let _seal = seal;
                TransitionJournalStore::try_open_in_retained_cast(cast, root)
            }
        }
    }
}

impl StatefulTreeIdentity {
    /// Exact candidate `/usr` capability retained before metadata decoration.
    ///
    /// The path is diagnostic only. Callers must perform every traversal from
    /// the descriptor and sandwich their work between strict guard proofs.
    pub(crate) fn retained_candidate_usr(&self) -> (&std::fs::File, &Path) {
        (
            self.candidate.store.retained_directory(),
            self.candidate.store.display_path(),
        )
    }

    /// Establish both permanent identities before the coordinator performs a
    /// trigger, exchange, archive, quarantine, or other transition effect.
    pub(crate) fn prepare(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_state: state::Id,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::ExistingId(candidate_state),
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Prepare the state-ID-unallocated candidate required by a fresh durable
    /// transition. Its payload and exact marker are retained, while both the
    /// canonical `.stateID` and fixed temporary remain absent until
    /// `CandidatePrepareStarted` authorizes publication.
    #[allow(dead_code)] // consumed only after the unwired journal coordinator is integrated
    pub(crate) fn prepare_unallocated_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::UnknownIdAbsent,
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Prepare a newly materialized active-reblit candidate whose database
    /// state is already known while both state-ID names remain absent. The
    /// retained classification cannot later be reinterpreted as NewState or
    /// archived activation by a coordinator request.
    #[allow(dead_code)] // consumed only after live coordinator integration
    pub(crate) fn prepare_active_reblit_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_state: state::Id,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::KnownIdAbsent(candidate_state),
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Prepare from an already-retained candidate `/usr` descriptor. The
    /// public name is compared before marker creation, while every write is
    /// descriptor-relative to the caller's exact inode.
    pub(crate) fn prepare_retained_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_usr: &std::fs::File,
        candidate_state: state::Id,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            Some(candidate_usr),
            CandidateStatePreparation::ExistingId(candidate_state),
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Retained-descriptor counterpart to [`Self::prepare_unallocated_candidate`].
    #[allow(dead_code)] // retained-capability entry point for future live coordinator wiring
    pub(crate) fn prepare_retained_unallocated_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_usr: &std::fs::File,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            Some(candidate_usr),
            CandidateStatePreparation::UnknownIdAbsent,
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Retained-descriptor counterpart to
    /// [`Self::prepare_active_reblit_candidate`].
    #[allow(dead_code)] // retained-capability entry point for live coordinator wiring
    pub(crate) fn prepare_retained_active_reblit_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_usr: &std::fs::File,
        candidate_state: state::Id,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            Some(candidate_usr),
            CandidateStatePreparation::KnownIdAbsent(candidate_state),
            JournalAcquisition::LegacyBlocking,
        )
    }

    /// Coordinator-only archived-candidate preparation.  Writer authority is
    /// already held, so journal acquisition must fail rather than wait if a
    /// contender wins the handoff gap.
    pub(crate) fn prepare_usr_exchange_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_state: state::Id,
        seal: &crate::client::JournalUsrExchangePreparationSeal,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::ExistingId(candidate_state),
            JournalAcquisition::CoordinatorNonblocking(seal),
        )
    }

    /// Coordinator-only fresh-state preparation with nonblocking journal
    /// acquisition under writer-first authority.
    pub(crate) fn prepare_usr_exchange_unallocated_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        seal: &crate::client::JournalUsrExchangePreparationSeal,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::UnknownIdAbsent,
            JournalAcquisition::CoordinatorNonblocking(seal),
        )
    }

    /// Coordinator-only active-reblit preparation with nonblocking journal
    /// acquisition under writer-first authority.
    pub(crate) fn prepare_usr_exchange_active_reblit_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        candidate_state: state::Id,
        seal: &crate::client::JournalUsrExchangePreparationSeal,
    ) -> Result<Self, Error> {
        Self::prepare_candidate(
            installation,
            state_db,
            candidate_path,
            None,
            CandidateStatePreparation::KnownIdAbsent(candidate_state),
            JournalAcquisition::CoordinatorNonblocking(seal),
        )
    }

    fn prepare_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        candidate_path: &Path,
        retained_candidate_usr: Option<&std::fs::File>,
        candidate_state: CandidateStatePreparation,
        journal_acquisition: JournalAcquisition<'_>,
    ) -> Result<Self, Error> {
        let root = &installation.root;
        let previous_path = root.join("usr");
        // Lock ordering is installation lock (owned by Installation), state
        // database (already opened), then journal lock. Do not invent a second
        // lock for marker publication.
        installation.revalidate_mutable_namespace()?;
        let cast = installation.retained_mutable_cast_directory()?;
        after_candidate_mutable_namespace_preflight();
        let journal = journal_acquisition.open(cast, root);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let journal = journal?;
        let baseline = require_clean_baseline(&journal, state_db);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        baseline?;

        // Authenticate the materialized candidate and establish a strictly
        // empty, same-mount previous tree only when the retained root proves
        // that `usr` is genuinely absent.
        let candidate_store = if let Some(candidate_usr) = retained_candidate_usr {
            let retained = TreeMarkerStore::open(candidate_usr, candidate_path)?;
            let named = TreeMarkerStore::open_path(candidate_path)?;
            retained.require_same_directory(&named)?;
            retained
        } else {
            TreeMarkerStore::open_path(candidate_path)?
        };
        let previous_store = open_or_synthesize_live_usr(installation)?;
        let previous_classification = installation.active_state.map_or(
            RetainedPreviousClassification::SynthesizedEmpty,
            RetainedPreviousClassification::Active,
        );
        if candidate_state.requires_absent_id() {
            state_tree_metadata::RetainedTreeStateId::require_absent(&candidate_store)?;
        }
        let candidate_durability = RetainedCandidateDurabilitySeal::seal_before_marker(
            candidate_store.retained_directory(),
            candidate_path,
            CandidateInventoryLimits::default(),
        );
        // A recursive durability proof can be long-running. Re-establish that
        // its retained candidate is still reachable through both its exact
        // child name and the complete mutable installation namespace before
        // publishing any marker. Authority errors supersede any simultaneous
        // inventory failure.
        let candidate_name =
            TreeMarkerStore::open_path(candidate_path).and_then(|named| candidate_store.require_same_directory(&named));
        let previous_name = require_named_live_usr(installation, previous_store.retained_directory(), &previous_path);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        candidate_name?;
        previous_name?;
        let candidate_durability = candidate_durability?;
        let mut candidate = match candidate_state {
            CandidateStatePreparation::ExistingId(state) => RetainedIdentity::prepare(candidate_store, Some(state))?,
            CandidateStatePreparation::UnknownIdAbsent | CandidateStatePreparation::KnownIdAbsent(_) => {
                RetainedIdentity::prepare_unallocated(candidate_store)?
            }
        };
        let _candidate_slot_link = candidate_state
            .existing_id()
            .map(|state| candidate.authorize_recovered_slot_link(installation, state))
            .transpose()?
            .flatten();
        let candidate_state_id = candidate_state.retain(&mut candidate, candidate_path)?;
        // Marker publication is the only admitted inventory delta. Bind that
        // delta to the exact retained marker and state-ID inode on both sides
        // of the post-publication inventory and durability pass.
        candidate.verify_named_read_only(candidate_path)?;
        candidate_state_id.verify_initial(&candidate)?;
        let durability = candidate_durability.validate_after_marker();
        let identity = candidate
            .verify_named_read_only(candidate_path)
            .and_then(|()| candidate_state_id.verify_initial(&candidate));
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        identity?;
        durability?;
        // The previous active tree is deliberately opaque during a repair:
        // only its already-retained marker is required. Its `.stateID` may be
        // the corrupt evidence this reblit is replacing.
        let previous = RetainedIdentity::prepare(previous_store, None)?;
        let previous_slot_link = if previous.marker.needs_slot_link_authorization() {
            let previous_state = installation
                .active_state
                .ok_or(Error::AuthorizedStateSlotLinkWithoutState)?;
            previous.authorize_recovered_slot_link(installation, previous_state)?
        } else {
            None
        };
        if candidate.marker.token() == previous.marker.token() {
            return Err(Error::DuplicateTreeToken {
                candidate: candidate_path.to_owned(),
                previous: previous_path,
                token: candidate.marker.token().as_str().to_owned(),
            });
        }

        candidate.revalidate_retained()?;
        candidate_state_id.verify_initial(&candidate)?;
        previous.revalidate_retained()?;
        // A cooperating writer cannot pass either held flock. Repeating the
        // evidence audit after marker publication also makes the ordering an
        // executable invariant rather than a comment.
        let baseline = require_clean_baseline(&journal, state_db);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        baseline?;

        Ok(Self {
            journal,
            state_database: state_db.clone(),
            candidate,
            candidate_state_id,
            previous,
            previous_classification,
            quarantine_attempt: Mutex::new(None),
            previous_archive_attempt: Mutex::new(None),
            archived_candidate_attempt: Mutex::new(None),
            active_reblit_rotation: Mutex::new(None),
            active_previous_slot_parking: Mutex::new(
                previous_slot_link
                    .map(active_previous_slot_parking::RetainedActivePreviousSlotParking::from_recovered)
                    .transpose()?,
            ),
        })
    }

    /// Revalidate both retained inodes and their current pre-exchange names.
    pub(crate) fn verify_pre_exchange(&self, candidate_path: &Path, previous_path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(candidate_path)?;
        self.previous.verify_named_read_only(previous_path)?;
        Ok(())
    }

    /// Exchange the authenticated staged candidate with the authenticated live
    /// previous tree beneath retained parent descriptors.
    #[cfg(test)]
    pub(crate) fn exchange_forward(&self, installation: &Installation) -> Result<(), RetainedExchangeFailure> {
        self.exchange_live_and_staged(
            installation,
            RetainedExchangeDirection::Forward,
            ExchangeJournalGuard::LegacyNoJournal,
            &|| Ok(()),
        )
    }

    /// Forward exchange with one final read-only validation executed inside
    /// the descriptor-bound preflight immediately before the single syscall.
    pub(crate) fn exchange_forward_validated(
        &self,
        installation: &Installation,
        validate: &impl Fn() -> Result<(), Error>,
    ) -> Result<(), RetainedExchangeFailure> {
        self.exchange_live_and_staged(
            installation,
            RetainedExchangeDirection::Forward,
            ExchangeJournalGuard::LegacyNoJournal,
            validate,
        )
    }

    /// Coordinator-only forward exchange.  The seal proves that the caller
    /// owns the exact durable `UsrExchangeIntent`; every legacy entry point
    /// continues to require journal absence.
    pub(super) fn exchange_forward_with_journal(
        &self,
        installation: &Installation,
        seal: &journal_coordinator::UsrExchangeEffectSeal,
        validate: &impl Fn() -> Result<(), StatefulTransitionCoordinatorError>,
    ) -> Result<(), RetainedExchangeFailure> {
        let validate = || validate().map_err(|source| Error::RetainedExchangeCoordinatorEvidence(Box::new(source)));
        self.exchange_live_and_staged(
            installation,
            RetainedExchangeDirection::Forward,
            ExchangeJournalGuard::Coordinator(seal),
            &validate,
        )
    }

    /// Reverse an earlier forward exchange through the same retained
    /// capability namespace.
    pub(crate) fn exchange_reverse(&self, installation: &Installation) -> Result<(), RetainedExchangeFailure> {
        self.exchange_live_and_staged(
            installation,
            RetainedExchangeDirection::Reverse,
            ExchangeJournalGuard::LegacyNoJournal,
            &|| Ok(()),
        )
    }

    /// Finish durability after a reverse exchange which is already proven to
    /// have moved both exact trees.
    ///
    /// This path deliberately performs no rename. Retrying an exchange after
    /// an applied-but-not-yet-durable result would put the failed candidate
    /// back in the live namespace.
    pub(crate) fn finish_applied_reverse(&self, installation: &Installation) -> Result<(), Error> {
        ExchangeJournalGuard::LegacyNoJournal.require(self)?;
        installation.revalidate_root_directory()?;
        let staging = self.open_exchange_staging(installation)?;
        staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            RetainedExchangeDirection::Reverse.after(),
        )?;
        self.finish_exchange(
            installation,
            &staging,
            RetainedExchangeDirection::Reverse.after(),
            ExchangeJournalGuard::LegacyNoJournal,
        )
    }

    fn exchange_live_and_staged(
        &self,
        installation: &Installation,
        direction: RetainedExchangeDirection,
        journal: ExchangeJournalGuard<'_>,
        validate: &impl Fn() -> Result<(), Error>,
    ) -> Result<(), RetainedExchangeFailure> {
        let not_applied = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::NotApplied,
            source,
        };
        let applied = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::Applied,
            source,
        };
        let ambiguous = |source| RetainedExchangeFailure {
            outcome: RetainedExchangeOutcome::Ambiguous,
            source,
        };

        journal.require(self).map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;

        let staging = self.open_exchange_staging(installation).map_err(not_applied)?;

        staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)
            .map_err(not_applied)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            direction.before(),
        )
        .map_err(not_applied)?;

        before_retained_exchange_rename();
        journal.require(self).map_err(not_applied)?;
        installation
            .revalidate_root_directory()
            .map_err(Error::from)
            .map_err(not_applied)?;
        staging
            .revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)
            .map_err(not_applied)?;
        self.require_exchange_layout(
            installation.root_directory(),
            &installation.root,
            &staging,
            direction.before(),
        )
        .map_err(not_applied)?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::BeforeRename).map_err(not_applied)?;
        validate().map_err(not_applied)?;

        let syscall_result = exchange_retained_usr_once(
            &staging.file,
            installation.root_directory(),
            &installation.root.join("usr"),
        );
        after_retained_exchange_rename();
        let syscall_result =
            syscall_result.and_then(|()| retained_exchange_checkpoint(RetainedExchangeFaultPoint::AfterRename));

        let observed = self
            .exchange_layout(installation.root_directory(), &installation.root, &staging)
            .map_err(ambiguous)?;
        if observed == direction.before() {
            let source = match syscall_result {
                Err(source) => source,
                Ok(()) => Error::RetainedExchangeReportedSuccessWithoutMove {
                    direction: direction.as_str(),
                },
            };
            return Err(not_applied(source));
        }
        if observed != direction.after() {
            return Err(ambiguous(Error::RetainedExchangeUnexpectedLayout {
                direction: direction.as_str(),
                expected: direction.after().as_str(),
                actual: observed.as_str(),
            }));
        }

        // Once both exact trees prove the post-exchange layout, a raw syscall
        // error is merely an error-after-apply report.  Complete durability
        // through both retained parents instead of exchanging a second time.
        self.finish_exchange(installation, &staging, direction.after(), journal)
            .map_err(applied)
    }

    fn open_exchange_staging(&self, installation: &Installation) -> Result<RetainedDirectory, Error> {
        let staging_path = installation.staging_dir();
        let staging =
            RetainedDirectory::open_beneath(installation.root_directory(), STAGING_RELATIVE, staging_path.clone())?;
        let root_device = installation
            .root_directory()
            .metadata()
            .map_err(|source| retained_exchange_io("inspect retained installation root", &installation.root, source))?
            .dev();
        if root_device != staging.witness.device {
            return Err(Error::RetainedExchangeCrossDevice {
                live_parent: installation.root.clone(),
                staged_parent: staging_path,
            });
        }
        Ok(staging)
    }

    fn finish_exchange(
        &self,
        installation: &Installation,
        staging: &RetainedDirectory,
        expected: RetainedExchangeLayout,
        journal: ExchangeJournalGuard<'_>,
    ) -> Result<(), Error> {
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::StagingParentSync)?;
        staging.sync("sync retained staging parent after /usr exchange")?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::InstallationRootSync)?;
        installation.root_directory().sync_all().map_err(|source| {
            retained_exchange_io(
                "sync retained installation root after /usr exchange",
                &installation.root,
                source,
            )
        })?;
        retained_exchange_checkpoint(RetainedExchangeFaultPoint::FinalRevalidation)?;
        journal.require(self)?;
        installation.revalidate_root_directory()?;
        staging.revalidate_beneath(installation.root_directory(), STAGING_RELATIVE)?;
        self.require_exchange_layout(installation.root_directory(), &installation.root, staging, expected)
    }

    fn require_exchange_layout(
        &self,
        root: &std::fs::File,
        root_path: &Path,
        staging: &RetainedDirectory,
        expected: RetainedExchangeLayout,
    ) -> Result<(), Error> {
        let actual = self.exchange_layout(root, root_path, staging)?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::RetainedExchangeUnexpectedLayout {
                direction: "preflight",
                expected: expected.as_str(),
                actual: actual.as_str(),
            })
        }
    }

    fn exchange_layout(
        &self,
        root: &std::fs::File,
        root_path: &Path,
        staging: &RetainedDirectory,
    ) -> Result<RetainedExchangeLayout, Error> {
        let live_path = root_path.join("usr");
        let staged_path = staging.path.join("usr");
        let live = open_retained_exchange_tree(root, &live_path)?;
        let staged = open_retained_exchange_tree(&staging.file, &staged_path)?;
        let live_role = self.retained_tree_role(&live)?;
        let staged_role = self.retained_tree_role(&staged)?;
        match (live_role, staged_role) {
            (RetainedTreeRole::Previous, RetainedTreeRole::Candidate) => Ok(RetainedExchangeLayout::CandidateStaged),
            (RetainedTreeRole::Candidate, RetainedTreeRole::Previous) => Ok(RetainedExchangeLayout::CandidateLive),
            (live, staged) => Err(Error::RetainedExchangeNamespaceMismatch {
                live: live.as_str(),
                staged: staged.as_str(),
            }),
        }
    }

    fn retained_tree_role(&self, named: &TreeMarkerStore) -> Result<RetainedTreeRole, Error> {
        match self.candidate.matches_store_read_only(named) {
            Ok(true) => return Ok(RetainedTreeRole::Candidate),
            Ok(false) => {}
            Err(source) => return Err(source),
        }
        match self.previous.matches_store_read_only(named) {
            Ok(true) => Ok(RetainedTreeRole::Previous),
            Ok(false) => Err(Error::RetainedExchangeUnknownTree),
            Err(source) => Err(source),
        }
    }

    /// Verify the forward layout after the atomic exchange.
    pub(crate) fn verify_forward_exchange(&self, live_path: &Path, previous_path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(live_path)?;
        self.previous.verify_named_read_only(previous_path)?;
        Ok(())
    }

    /// Verify the previous tree at staging or its archive using only the
    /// recovery reader.
    pub(crate) fn verify_previous_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.previous.verify_named_read_only(path)
    }

    /// Verify the candidate tree at live, staging, archive, or quarantine
    /// using only the recovery reader.
    pub(crate) fn verify_candidate_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(path)
    }

    /// Strictly authenticate the named candidate before activation work.
    ///
    /// Unlike recovery verification, this also proves the exact retained
    /// `.stateID`. Recovery must remain marker-only so a damaged state ID can
    /// still be moved out of the live namespace and preserved as evidence.
    pub(crate) fn verify_candidate_for_activation(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.verify_candidate_named_with_state_id(path)?;
        self.require_no_journal()
    }

    /// Flush the filesystem containing the retained candidate and persist its
    /// authenticated root at the current name. The later crash coordinator
    /// still owns bounded descriptor-recursive inventory authentication; this
    /// barrier proves durability, not a stable descendant namespace.
    pub(crate) fn sync_candidate_for_recovery(&self, path: &Path) -> Result<(), Error> {
        self.require_no_journal()?;
        self.candidate.verify_named_read_only(path)?;
        self.candidate.store.sync_retained_tree()?;
        self.candidate.verify_named_read_only(path)
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_CANDIDATE_MUTABLE_NAMESPACE_PREFLIGHT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_after_candidate_mutable_namespace_preflight(hook: impl FnOnce() + 'static) {
    AFTER_CANDIDATE_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_candidate_mutable_namespace_preflight() {
    AFTER_CANDIDATE_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_candidate_mutable_namespace_preflight() {}
