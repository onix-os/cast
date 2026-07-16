use std::{
    ffi::CString,
    fs,
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::{
    Installation, db,
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_journal::{
        AbortDisposition, BootRollback, CandidateRollback, ForwardPhase, Operation, Phase, Previous, PreviousOrigin,
        QuarantineName, RollbackAction, RollbackPlan, RuntimeEpoch, RuntimeTreeIdentity, TransitionJournalStore,
        TransitionRecord, TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::super::{PendingSystemTransition, RecoveryBlocker};
use super::{
    ActivationNamespaceInspection, ActivationNamespaceStability, arm_before_final_namespace_revalidation,
    capture::{CaptureError, TreeLocation, capture_snapshot},
    policy::{
        CandidatePlace, LayoutAlternative, NamespacePolicyConflict, PreviousPlace, StateIdExpectation, assess_snapshot,
        candidate_destination, candidate_state_id_expectation, forward_layouts, isolation_abi_must_be_complete,
        rollback_layouts, root_abi_must_be_complete,
    },
};

mod slot_links;

const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

struct Fixture {
    installation: Installation,
    record: TransitionRecord,
    temporary: tempfile::TempDir,
}

impl Fixture {
    fn new_state(previous_id: Option<i32>, previous_origin: PreviousOrigin) -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let (previous_token, previous_runtime) = create_marked_tree(&installation.root.join("usr"));
        if let Some(previous_id) = previous_id {
            write_state_id(&installation.root.join("usr"), previous_id.to_string().as_bytes());
        }
        let (candidate_token, candidate_runtime) = create_marked_tree(&installation.staging_path("usr"));
        let record = TransitionRecord::preparing(
            transition_id(),
            RuntimeEpoch::capture().unwrap(),
            Operation::NewState,
            None,
            candidate_token,
            candidate_runtime,
            Previous {
                id: previous_id,
                tree_token: previous_token,
                usr_runtime_identity: previous_runtime,
                origin: previous_origin,
            },
            true,
            true,
            QuarantineName::parse("failed-startup-inventory").unwrap(),
        )
        .unwrap();
        Self {
            installation,
            record,
            temporary,
        }
    }

    fn active_reblit() -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let (previous_token, previous_runtime) = create_marked_tree(&installation.root.join("usr"));
        let (candidate_token, candidate_runtime) = create_marked_tree(&installation.staging_path("usr"));
        let record = TransitionRecord::preparing(
            transition_id(),
            RuntimeEpoch::capture().unwrap(),
            Operation::ActiveReblit,
            Some(42),
            candidate_token,
            candidate_runtime,
            Previous {
                id: Some(42),
                tree_token: previous_token,
                usr_runtime_identity: previous_runtime,
                origin: PreviousOrigin::ActiveReblitCorrupt,
            },
            true,
            true,
            QuarantineName::parse("failed-startup-reblit").unwrap(),
        )
        .unwrap();
        Self {
            installation,
            record,
            temporary,
        }
    }

    fn archived_activation() -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let (previous_token, previous_runtime) = create_marked_tree(&installation.root.join("usr"));
        write_state_id(&installation.root.join("usr"), b"41");
        let (candidate_token, candidate_runtime) = create_marked_tree(&installation.staging_path("usr"));
        let record = TransitionRecord::preparing(
            transition_id(),
            RuntimeEpoch::capture().unwrap(),
            Operation::ActivateArchived,
            Some(42),
            candidate_token,
            candidate_runtime,
            Previous {
                id: Some(41),
                tree_token: previous_token,
                usr_runtime_identity: previous_runtime,
                origin: PreviousOrigin::ActiveState,
            },
            true,
            true,
            QuarantineName::parse("failed-startup-archived").unwrap(),
        )
        .unwrap();
        Self {
            installation,
            record,
            temporary,
        }
    }

    fn snapshot(&self) -> Result<super::capture::NamespaceSnapshot, CaptureError> {
        capture_snapshot(&self.installation, &self.record)
    }

    fn assess(&self) -> Result<(), NamespacePolicyConflict> {
        assess_snapshot(&self.record, &self.snapshot().unwrap())
    }
}

fn transition_id() -> TransitionId {
    TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap()
}

fn create_private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn create_marked_tree(path: &Path) -> (TreeToken, RuntimeTreeIdentity) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    let store = TreeMarkerStore::open_path(path).unwrap();
    let marker = store.adopt_or_create_before_journal().unwrap();
    let token = marker.token().clone();
    let runtime = RuntimeTreeIdentity::capture_directory(store.retained_directory()).unwrap();
    (token, runtime)
}

fn write_state_id(usr: &Path, bytes: &[u8]) {
    let path = usr.join(".stateID");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

fn active_reblit_wrapper_path(installation: &Installation, record: &TransitionRecord) -> PathBuf {
    installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-42-{}-0",
        record.previous.tree_token.as_str()
    ))
}

fn archived_rearchive_fixture(phase: Phase, rearchived: bool, state_id: Option<&[u8]>) -> Fixture {
    let mut fixture = Fixture::archived_activation();
    if let Some(state_id) = state_id {
        write_state_id(&fixture.installation.staging_path("usr"), state_id);
    }
    let candidate_action = if phase == Phase::CandidatePreserveIntent {
        RollbackAction::Pending
    } else {
        RollbackAction::AlreadySatisfied
    };
    let mut rollback = rollback_plan(
        ForwardPhase::CandidatePrepared,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        candidate_action,
    );
    rollback.candidate.disposition = AbortDisposition::Rearchive;
    rollback.fresh_db = RollbackAction::NotRequired;
    fixture.record.phase = phase;
    fixture.record.rollback = Some(rollback);
    if rearchived {
        let wrapper = fixture.installation.root_path("42");
        create_private_directory(&wrapper);
        fs::rename(fixture.installation.staging_path("usr"), wrapper.join("usr")).unwrap();
        fs::hard_link(
            wrapper.join("usr/.cast-tree-id"),
            wrapper.join(format!(
                ".cast-state-slot-42-{}",
                fixture.record.candidate.tree_token.as_str()
            )),
        )
        .unwrap();
    }
    fixture
}

fn install_root_abi(fixture: &Fixture) {
    for (name, target) in ROOT_ABI {
        symlink(target, fixture.installation.root.join(name)).unwrap();
        symlink(target, fixture.installation.isolation_path(name)).unwrap();
    }
}

fn rollback_plan(
    source: ForwardPhase,
    previous_archive: RollbackAction,
    usr_exchange: RollbackAction,
    candidate: RollbackAction,
) -> RollbackPlan {
    RollbackPlan {
        source,
        previous_archive,
        usr_exchange,
        candidate: CandidateRollback {
            action: candidate,
            disposition: AbortDisposition::Quarantine,
        },
        fresh_db: RollbackAction::Pending,
        boot: BootRollback::NotRequired,
        external_effects_may_remain: false,
    }
}

#[test]
fn startup_activation_inventory_accepts_exact_preparing_layout_without_mutation() {
    let fixture = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let paths = [
        fixture.installation.root.join("usr/.cast-tree-id"),
        fixture.installation.root.join("usr/.stateID"),
        fixture.installation.root.join("usr"),
        fixture.installation.root_path(""),
    ];
    let before = paths
        .iter()
        .map(|path| (path.clone(), force_old_atime(path)))
        .collect::<Vec<_>>();

    let snapshot = fixture.snapshot().unwrap();
    assert_eq!(assess_snapshot(&fixture.record, &snapshot), Ok(()));
    snapshot.revalidate_retained().unwrap();

    for (path, expected) in before {
        let after = fs::metadata(&path).unwrap();
        assert_eq!((after.atime(), after.atime_nsec()), expected, "{}", path.display());
    }
    assert!(fixture.temporary.path().exists());
}

fn force_old_atime(path: &Path) -> (i64, i64) {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let file = fs::File::open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_accessed(old)).unwrap();
    let metadata = fs::metadata(path).unwrap();
    (metadata.atime(), metadata.atime_nsec())
}

#[test]
fn startup_activation_inventory_rejects_raw_names_bounds_acls_and_isolation_foreign_entries() {
    let raw = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let raw_name = std::ffi::OsString::from_vec(vec![0xff, b'x']);
    create_private_directory(&raw.installation.root_path(&raw_name));
    assert!(matches!(
        raw.snapshot(),
        Err(CaptureError::UnexpectedRootName { name }) if name == vec![0xff, b'x']
    ));

    let isolation = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    fs::write(isolation.installation.isolation_path("foreign"), b"foreign").unwrap();
    assert!(matches!(
        isolation.snapshot(),
        Err(CaptureError::UnexpectedIsolationEntry { name }) if name == b"foreign"
    ));

    let bounded = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    for index in 0..33 {
        fs::write(bounded.installation.isolation_path(format!("entry-{index:02}")), b"x").unwrap();
    }
    assert!(matches!(
        bounded.snapshot(),
        Err(CaptureError::EntryLimit { limit: 32, .. })
    ));

    let acl = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    if set_extended_access_acl(&acl.installation.root.join("usr")).unwrap_or(false) {
        assert!(matches!(
            acl.snapshot(),
            Err(CaptureError::Io {
                operation: "reject access ACL on retained /usr tree",
                ..
            })
        ));
    }
}

#[test]
fn startup_activation_inventory_final_revalidation_detects_public_namespace_substitution() {
    let fixture = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let journal =
        TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root)
            .unwrap();
    journal.create(&fixture.record).unwrap();
    let inspection = ActivationNamespaceInspection::begin(&fixture.installation, &journal, &fixture.record);

    let roots = fixture.installation.root_path("");
    let displaced = fixture.installation.root.join(".cast/root-displaced");
    arm_before_final_namespace_revalidation(move || {
        fs::rename(&roots, &displaced).unwrap();
        create_private_directory(&roots);
        create_private_directory(&roots.join("staging"));
        create_private_directory(&roots.join("isolation"));
    });
    let evidence = inspection.finish(&fixture.installation, &journal, &fixture.record);

    assert_eq!(evidence.stability(), ActivationNamespaceStability::Changed);
    assert!(evidence.journal_is_exact());
    assert!(!evidence.phase_layout_is_exact());
    assert!(!evidence.policy_was_assessed());
}

#[test]
fn startup_activation_inventory_binds_slot_links_to_transition_role_and_state() {
    slot_links::run();
}

#[test]
fn startup_activation_inventory_active_reblit_previous_state_id_is_typed() {
    let mut fixture = Fixture::active_reblit();
    assert_eq!(fixture.assess(), Ok(()));

    write_state_id(&fixture.installation.root.join("usr"), b"corrupt");
    assert_eq!(fixture.assess(), Ok(()));

    write_state_id(&fixture.installation.staging_path("usr"), b"candidate-corrupt");
    fixture.record.phase = Phase::CandidatePrepared;
    assert_eq!(
        candidate_state_id_expectation(&fixture.record),
        StateIdExpectation::MarkerOnly
    );
    assert_eq!(fixture.assess(), Ok(()));
    fixture.record.phase = Phase::BootSyncComplete;
    assert_eq!(
        candidate_state_id_expectation(&fixture.record),
        StateIdExpectation::Present(42)
    );
    fixture.record.phase = Phase::RollbackDecided;
    fixture.record.rollback = Some(rollback_plan(
        ForwardPhase::CandidatePrepared,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    assert_eq!(fixture.assess(), Ok(()));

    let wrong_previous = Fixture::active_reblit();
    write_state_id(&wrong_previous.installation.root.join("usr"), b"43");
    assert!(matches!(
        wrong_previous.assess(),
        Err(NamespacePolicyConflict::PreviousStateId { .. })
    ));

    let early_reservation = Fixture::active_reblit();
    create_private_directory(&active_reblit_wrapper_path(
        &early_reservation.installation,
        &early_reservation.record,
    ));
    assert_eq!(
        early_reservation.assess(),
        Err(NamespacePolicyConflict::ActiveReblitWrapper)
    );

    for state_id in [None, Some(&b"corrupt"[..])] {
        for (phase, rearchived) in [
            (Phase::CandidatePreserveIntent, false),
            (Phase::CandidatePreserveIntent, true),
            (Phase::CandidatePreserved, true),
            (Phase::RollbackComplete, true),
        ] {
            let archived = archived_rearchive_fixture(phase, rearchived, state_id);
            assert_eq!(
                archived.assess(),
                Ok(()),
                "archived marker-only recovery at {phase:?}, rearchived={rearchived}, state_id={state_id:?}"
            );
        }
    }

    let ambient = archived_rearchive_fixture(Phase::CandidatePreserved, true, None);
    let ambient_wrapper = ambient.installation.root_path("43");
    create_private_directory(&ambient_wrapper);
    create_marked_tree(&ambient_wrapper.join("usr"));
    assert!(matches!(
        ambient.snapshot(),
        Err(CaptureError::StateWrapperMismatch { expected: 43, .. })
    ));

    let mut quarantined = archived_rearchive_fixture(Phase::CandidatePreserved, true, None);
    quarantined.record.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Quarantine;
    assert!(matches!(
        quarantined.snapshot(),
        Err(CaptureError::StateWrapperMismatch { expected: 42, .. })
    ));
}

#[test]
fn startup_activation_inventory_active_reblit_preserve_accepts_only_paired_destinations() {
    let mut early = Fixture::active_reblit();
    early.record.phase = Phase::CandidatePreserved;
    early.record.rollback = Some(rollback_plan(
        ForwardPhase::Preparing,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::AlreadySatisfied,
    ));
    let early_reserved = active_reblit_wrapper_path(&early.installation, &early.record);
    create_private_directory(&early_reserved);
    fs::rename(early.installation.staging_path("usr"), early_reserved.join("usr")).unwrap();
    assert_eq!(early.assess(), Err(NamespacePolicyConflict::ActiveReblitWrapper));
    fs::rename(early_reserved.join("usr"), early.installation.staging_path("usr")).unwrap();
    fs::remove_dir(early_reserved).unwrap();
    let early_transition = early
        .installation
        .state_quarantine_dir()
        .join(early.record.quarantine_name.as_str());
    create_private_directory(&early_transition);
    fs::rename(early.installation.staging_path("usr"), early_transition.join("usr")).unwrap();
    assert_eq!(early.assess(), Ok(()));

    let mut required = Fixture::active_reblit();
    write_state_id(&required.installation.staging_path("usr"), b"42");
    required.record.phase = Phase::TransactionTriggersStarted;
    install_root_abi(&required);
    assert_eq!(required.assess(), Err(NamespacePolicyConflict::ActiveReblitWrapper));
    let required_reserved = active_reblit_wrapper_path(&required.installation, &required.record);
    create_private_directory(&required_reserved);
    assert_eq!(required.assess(), Ok(()));

    fs::remove_dir(&required_reserved).unwrap();
    required.record.phase = Phase::CandidatePreserved;
    required.record.rollback = Some(rollback_plan(
        ForwardPhase::TransactionTriggersStarted,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::AlreadySatisfied,
    ));
    let required_transition = required
        .installation
        .state_quarantine_dir()
        .join(required.record.quarantine_name.as_str());
    create_private_directory(&required_transition);
    fs::rename(
        required.installation.staging_path("usr"),
        required_transition.join("usr"),
    )
    .unwrap();
    assert_eq!(required.assess(), Err(NamespacePolicyConflict::ActiveReblitWrapper));

    create_private_directory(&required_reserved);
    assert_eq!(required.assess(), Err(NamespacePolicyConflict::ActiveReblitWrapper));
    fs::rename(required_transition.join("usr"), required_reserved.join("usr")).unwrap();
    fs::remove_dir(required_transition).unwrap();
    assert_eq!(required.assess(), Ok(()));

    let mut fixture = Fixture::active_reblit();
    write_state_id(&fixture.installation.staging_path("usr"), b"42");
    fixture.record.phase = Phase::CandidatePreserveIntent;
    fixture.record.rollback = Some(rollback_plan(
        ForwardPhase::CandidatePrepared,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    assert_eq!(fixture.assess(), Ok(()));

    let reserved = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    create_private_directory(&reserved);
    fs::rename(fixture.installation.staging_path("usr"), reserved.join("usr")).unwrap();
    assert_eq!(fixture.assess(), Ok(()));

    fs::rename(reserved.join("usr"), fixture.installation.staging_path("usr")).unwrap();
    fs::remove_dir(&reserved).unwrap();
    let transition = fixture
        .installation
        .state_quarantine_dir()
        .join(fixture.record.quarantine_name.as_str());
    create_private_directory(&transition);
    fs::rename(fixture.installation.staging_path("usr"), transition.join("usr")).unwrap();
    assert_eq!(fixture.assess(), Ok(()));

    create_private_directory(&reserved);
    assert_eq!(fixture.assess(), Err(NamespacePolicyConflict::ActiveReblitWrapper));
}

#[test]
fn startup_activation_policy_forward_layout_matrix_is_exact() {
    let mut fixture = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let pre = LayoutAlternative {
        candidate: CandidatePlace::Staging,
        previous: PreviousPlace::Live,
    };
    let post = LayoutAlternative {
        candidate: CandidatePlace::Live,
        previous: PreviousPlace::Staging,
    };
    let archived = LayoutAlternative {
        candidate: CandidatePlace::Live,
        previous: PreviousPlace::Archived,
    };
    for phase in [
        Phase::Preparing,
        Phase::FreshStateAllocating,
        Phase::FreshStateAllocated,
        Phase::CandidatePrepareStarted,
        Phase::CandidatePrepared,
        Phase::TransactionTriggersStarted,
        Phase::TransactionTriggersComplete,
    ] {
        fixture.record.phase = phase;
        assert_eq!(forward_layouts(&fixture.record), vec![pre], "{phase:?}");
    }
    fixture.record.phase = Phase::UsrExchangeIntent;
    assert_eq!(forward_layouts(&fixture.record), vec![pre, post]);
    for phase in [
        Phase::UsrExchanged,
        Phase::RootLinksComplete,
        Phase::SystemTriggersStarted,
        Phase::SystemTriggersComplete,
    ] {
        fixture.record.phase = phase;
        assert_eq!(forward_layouts(&fixture.record), vec![post], "{phase:?}");
    }
    fixture.record.phase = Phase::PreviousArchiveIntent;
    assert_eq!(forward_layouts(&fixture.record), vec![post, archived]);
    for phase in [
        Phase::PreviousArchived,
        Phase::BootSyncStarted,
        Phase::BootSyncComplete,
        Phase::CommitDecided,
        Phase::CommitCleanupComplete,
        Phase::Complete,
    ] {
        fixture.record.phase = phase;
        assert_eq!(forward_layouts(&fixture.record), vec![archived], "{phase:?}");
    }

    let mut synthesized = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let mut reblit = Fixture::active_reblit();
    for phase in [Phase::BootSyncStarted, Phase::BootSyncComplete] {
        synthesized.record.phase = phase;
        reblit.record.phase = phase;
        assert_eq!(
            forward_layouts(&synthesized.record),
            vec![post],
            "synthesized {phase:?}"
        );
        assert_eq!(forward_layouts(&reblit.record), vec![post], "active-reblit {phase:?}");
    }
}

#[test]
fn startup_activation_policy_rollback_actions_override_source_ordinal() {
    let mut fixture = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    fixture.record.phase = Phase::RollbackDecided;
    let source = ForwardPhase::PreviousArchived;
    let cases = [
        (
            RollbackAction::Pending,
            RollbackAction::Pending,
            RollbackAction::Pending,
            LayoutAlternative {
                candidate: CandidatePlace::Live,
                previous: PreviousPlace::Archived,
            },
        ),
        (
            RollbackAction::AlreadySatisfied,
            RollbackAction::Pending,
            RollbackAction::Pending,
            LayoutAlternative {
                candidate: CandidatePlace::Live,
                previous: PreviousPlace::Staging,
            },
        ),
        (
            RollbackAction::AlreadySatisfied,
            RollbackAction::AlreadySatisfied,
            RollbackAction::Pending,
            LayoutAlternative {
                candidate: CandidatePlace::Staging,
                previous: PreviousPlace::Live,
            },
        ),
        (
            RollbackAction::AlreadySatisfied,
            RollbackAction::AlreadySatisfied,
            RollbackAction::AlreadySatisfied,
            LayoutAlternative {
                candidate: CandidatePlace::Destination,
                previous: PreviousPlace::Live,
            },
        ),
    ];
    for (previous, usr, candidate, expected) in cases {
        fixture.record.rollback = Some(rollback_plan(source, previous, usr, candidate));
        assert_eq!(
            rollback_layouts(&fixture.record, fixture.record.rollback.as_ref().unwrap()).unwrap(),
            vec![expected]
        );
    }

    fixture.record.rollback = Some(rollback_plan(
        source,
        RollbackAction::Pending,
        RollbackAction::AlreadySatisfied,
        RollbackAction::Pending,
    ));
    assert_eq!(
        rollback_layouts(&fixture.record, fixture.record.rollback.as_ref().unwrap()),
        Err(NamespacePolicyConflict::RollbackActions)
    );

    let mut archived = fixture.record.clone();
    archived.operation = Operation::ActivateArchived;
    archived.candidate.id = Some(42);
    archived.rollback = Some(rollback_plan(
        ForwardPhase::SystemTriggersStarted,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
        RollbackAction::Pending,
    ));
    assert!(candidate_destination(&archived, &TreeLocation::TransitionQuarantine));
    assert!(!candidate_destination(&archived, &TreeLocation::State(42)));
    archived.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Rearchive;
    assert!(candidate_destination(&archived, &TreeLocation::State(42)));
    assert!(!candidate_destination(&archived, &TreeLocation::TransitionQuarantine));
}

#[test]
fn startup_activation_policy_cleanup_and_abi_matrix_is_exact() {
    let mut synthesized = Fixture::new_state(None, PreviousOrigin::SynthesizedEmpty);
    let post = LayoutAlternative {
        candidate: CandidatePlace::Live,
        previous: PreviousPlace::Staging,
    };
    let discarded = LayoutAlternative {
        candidate: CandidatePlace::Live,
        previous: PreviousPlace::Absent,
    };
    synthesized.record.phase = Phase::CommitDecided;
    assert_eq!(forward_layouts(&synthesized.record), vec![post, discarded]);
    synthesized.record.phase = Phase::CommitCleanupComplete;
    assert_eq!(forward_layouts(&synthesized.record), vec![discarded]);
    synthesized.record.phase = Phase::Complete;
    assert_eq!(forward_layouts(&synthesized.record), vec![discarded]);

    synthesized.record.candidate.id = Some(42);
    write_state_id(&synthesized.installation.staging_path("usr"), b"42");
    install_root_abi(&synthesized);
    let displaced = synthesized.installation.root.join("usr.displaced-for-cleanup-test");
    fs::rename(synthesized.installation.root.join("usr"), &displaced).unwrap();
    fs::rename(
        synthesized.installation.staging_path("usr"),
        synthesized.installation.root.join("usr"),
    )
    .unwrap();
    fs::rename(&displaced, synthesized.installation.staging_path("usr")).unwrap();
    synthesized.record.phase = Phase::CommitDecided;
    assert_eq!(synthesized.assess(), Ok(()));
    synthesized.record.phase = Phase::CommitCleanupComplete;
    assert!(matches!(
        synthesized.assess(),
        Err(NamespacePolicyConflict::PhaseLayout { .. })
    ));
    synthesized.record.phase = Phase::CommitDecided;
    fs::remove_dir_all(synthesized.installation.staging_path("usr")).unwrap();
    assert_eq!(synthesized.assess(), Ok(()));
    synthesized.record.phase = Phase::CommitCleanupComplete;
    assert_eq!(synthesized.assess(), Ok(()));
    synthesized.record.phase = Phase::Complete;
    assert_eq!(synthesized.assess(), Ok(()));

    for phase in [
        Phase::Preparing,
        Phase::CandidatePrepared,
        Phase::TransactionTriggersStarted,
        Phase::UsrExchangeIntent,
        Phase::UsrExchanged,
        Phase::RootLinksComplete,
    ] {
        synthesized.record.phase = phase;
        assert_eq!(
            isolation_abi_must_be_complete(&synthesized.record),
            matches!(
                phase,
                Phase::TransactionTriggersStarted
                    | Phase::UsrExchangeIntent
                    | Phase::UsrExchanged
                    | Phase::RootLinksComplete
            ),
            "isolation at {phase:?}"
        );
        assert_eq!(
            root_abi_must_be_complete(&synthesized.record),
            phase == Phase::RootLinksComplete,
            "live root at {phase:?}"
        );
    }

    let archived = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let mut archived_record = archived.record.clone();
    archived_record.operation = Operation::ActivateArchived;
    archived_record.phase = Phase::CandidatePrepared;
    assert!(!isolation_abi_must_be_complete(&archived_record));
    for phase in [Phase::UsrExchangeIntent, Phase::UsrExchanged, Phase::RootLinksComplete] {
        archived_record.phase = phase;
        assert!(!isolation_abi_must_be_complete(&archived_record), "{phase:?}");
    }
    for phase in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
        archived_record.phase = phase;
        assert!(isolation_abi_must_be_complete(&archived_record), "{phase:?}");
    }
    archived_record.options.run_system_triggers = false;
    for phase in [
        Phase::RootLinksComplete,
        Phase::PreviousArchiveIntent,
        Phase::PreviousArchived,
        Phase::CommitDecided,
    ] {
        archived_record.phase = phase;
        assert!(
            !isolation_abi_must_be_complete(&archived_record),
            "disabled at {phase:?}"
        );
    }

    let mut root_rollback = synthesized.record.clone();
    root_rollback.phase = Phase::RollbackDecided;
    root_rollback.rollback = Some(rollback_plan(
        ForwardPhase::UsrExchanged,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
        RollbackAction::Pending,
    ));
    for phase in [
        Phase::RollbackDecided,
        Phase::ReverseExchangeIntent,
        Phase::UsrRestored,
        Phase::CandidatePreserveIntent,
        Phase::CandidatePreserved,
    ] {
        root_rollback.phase = phase;
        assert!(!root_abi_must_be_complete(&root_rollback), "rollback at {phase:?}");
    }
    root_rollback.phase = Phase::RollbackComplete;
    assert!(root_abi_must_be_complete(&root_rollback));

    root_rollback.rollback.as_mut().unwrap().source = ForwardPhase::UsrExchangeIntent;
    assert!(!root_abi_must_be_complete(&root_rollback));
    root_rollback.rollback.as_mut().unwrap().source = ForwardPhase::RootLinksComplete;
    root_rollback.phase = Phase::RollbackDecided;
    assert!(root_abi_must_be_complete(&root_rollback));
}

fn set_extended_access_acl(path: &Path) -> std::io::Result<bool> {
    const ACL_XATTR_VERSION: u32 = 0x0002;
    const ACL_USER_OBJ: u16 = 0x01;
    const ACL_USER: u16 = 0x02;
    const ACL_GROUP_OBJ: u16 = 0x04;
    const ACL_MASK: u16 = 0x10;
    const ACL_OTHER: u16 = 0x20;
    const ACL_UNDEFINED_ID: u32 = u32::MAX;

    let mut value = ACL_XATTR_VERSION.to_le_bytes().to_vec();
    let mut entry = |tag: u16, permissions: u16, id: u32| {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&permissions.to_le_bytes());
        value.extend_from_slice(&id.to_le_bytes());
    };
    entry(ACL_USER_OBJ, 0o7, ACL_UNDEFINED_ID);
    entry(ACL_USER, 0o5, unsafe { nix::libc::geteuid() }.saturating_add(1));
    entry(ACL_GROUP_OBJ, 0o5, ACL_UNDEFINED_ID);
    entry(ACL_MASK, 0o5, ACL_UNDEFINED_ID);
    entry(ACL_OTHER, 0o5, ACL_UNDEFINED_ID);

    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = c"system.posix_acl_access";
    // SAFETY: both C strings and the byte slice remain live for this call.
    let result = unsafe { nix::libc::setxattr(encoded.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
    if result == 0 {
        return Ok(true);
    }
    let source = std::io::Error::last_os_error();
    if source
        .raw_os_error()
        .is_some_and(|code| matches!(code, nix::libc::ENOTSUP | nix::libc::EPERM))
    {
        Ok(false)
    } else {
        Err(source)
    }
}
