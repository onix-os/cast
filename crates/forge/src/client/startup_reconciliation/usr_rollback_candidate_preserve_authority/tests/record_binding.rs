//! Exact canonical-record identity across candidate-preservation effects.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
        UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation,
        UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation,
        active_reblit_candidate_preserve_exchange_attempt_count, archived_candidate_preserve_move_attempt_count,
        arm_before_active_reblit_candidate_preserve_reconciliation_capture,
        arm_before_archived_candidate_preserve_move_reconciliation_capture,
        arm_before_new_state_candidate_preserve_move_reconciliation_capture,
        arm_before_new_state_target_create_reconciliation_capture,
        arm_before_new_state_target_normalize_reconciliation_capture,
        new_state_candidate_preserve_move_attempt_count, new_state_target_create_attempt_count,
        new_state_target_normalize_attempt_count, reset_active_reblit_candidate_preserve_exchange_attempt_count,
        reset_archived_candidate_preserve_move_attempt_count,
        reset_new_state_candidate_preserve_move_attempt_count, reset_new_state_target_create_attempt_count,
        reset_new_state_target_normalize_attempt_count,
    },
    startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
};
use crate::transition_journal::RollbackActionOutcome;

use super::{
    fixture::{OperationKind, ROOT_ABI, canonical_journal},
    support::{
        CandidateLayout, CandidatePreserveFixture, CandidateSource, reserved_active_reblit_wrapper_path,
        transition_quarantine_path,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectCase {
    CreateTarget,
    NormalizeTarget,
    MoveNewState,
    MoveArchived,
    ExchangeActiveReblit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordSourceCase {
    Candidate(CandidateSource),
    BootSyncStarted,
}

impl EffectCase {
    const ALL: [Self; 5] = [
        Self::CreateTarget,
        Self::NormalizeTarget,
        Self::MoveNewState,
        Self::MoveArchived,
        Self::ExchangeActiveReblit,
    ];

    fn fixture(
        self,
        historical: bool,
        source: RecordSourceCase,
        outcome: RollbackActionOutcome,
    ) -> CandidatePreserveFixture {
        if source == RecordSourceCase::BootSyncStarted {
            assert_eq!(self, Self::ExchangeActiveReblit);
            return CandidatePreserveFixture::active_reblit_boot_sync_started(
                historical,
                outcome,
                CandidateLayout::Staged,
            );
        }
        let RecordSourceCase::Candidate(source) = source else {
            unreachable!("BootSyncStarted was handled above")
        };
        match self {
            Self::CreateTarget => fixture_at_epoch(
                historical,
                OperationKind::NewState,
                source,
                outcome,
                CandidateLayout::Staged,
            ),
            Self::NormalizeTarget => CandidatePreserveFixture::new_state_target_residue_at_epoch(
                historical, source, outcome, 0o500,
            ),
            Self::MoveNewState => CandidatePreserveFixture::new_state_empty_quarantine_prefix_at_epoch(
                historical, source, outcome,
            ),
            Self::MoveArchived => fixture_at_epoch(
                historical,
                OperationKind::Archived,
                source,
                outcome,
                CandidateLayout::Staged,
            ),
            Self::ExchangeActiveReblit => fixture_at_epoch(
                historical,
                OperationKind::ActiveReblit,
                source,
                outcome,
                CandidateLayout::Staged,
            ),
        }
    }

    fn record_sources(self) -> impl Iterator<Item = RecordSourceCase> {
        CandidateSource::ALL
            .into_iter()
            .map(RecordSourceCase::Candidate)
            .chain(
                (self == Self::ExchangeActiveReblit).then_some(RecordSourceCase::BootSyncStarted),
            )
    }

    fn arm_after_physical_effect(self, hook: impl FnOnce() + 'static) {
        match self {
            Self::CreateTarget => arm_before_new_state_target_create_reconciliation_capture(hook),
            Self::NormalizeTarget => arm_before_new_state_target_normalize_reconciliation_capture(hook),
            Self::MoveNewState => arm_before_new_state_candidate_preserve_move_reconciliation_capture(hook),
            Self::MoveArchived => arm_before_archived_candidate_preserve_move_reconciliation_capture(hook),
            Self::ExchangeActiveReblit => {
                arm_before_active_reblit_candidate_preserve_reconciliation_capture(hook)
            }
        }
    }
}

fn fixture_at_epoch(
    historical: bool,
    kind: OperationKind,
    source: CandidateSource,
    outcome: RollbackActionOutcome,
    layout: CandidateLayout,
) -> CandidatePreserveFixture {
    if historical {
        CandidatePreserveFixture::historical(kind, source, outcome, layout)
    } else {
        CandidatePreserveFixture::new(kind, source, outcome, layout)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RootAbiEntry {
    name: &'static str,
    target: Option<PathBuf>,
    identity: Option<(u64, u64, u32, u64)>,
}

fn root_abi_snapshot(root: &Path) -> Vec<RootAbiEntry> {
    ROOT_ABI
        .into_iter()
        .map(|(name, _)| {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => RootAbiEntry {
                    name,
                    target: Some(fs::read_link(path).unwrap()),
                    identity: Some((metadata.dev(), metadata.ino(), metadata.mode(), metadata.nlink())),
                },
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => RootAbiEntry {
                    name,
                    target: None,
                    identity: None,
                },
                Err(source) => panic!("snapshot root ABI link {name}: {source}"),
            }
        })
        .collect()
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_replacement_hook(
    fixture: &CandidatePreserveFixture,
    label: &str,
) -> ((u64, u64), PathBuf, Vec<u8>, impl FnOnce() + 'static) {
    let canonical = canonical_journal(&fixture.fixture.installation.root);
    let retained_identity = inode_identity(&canonical);
    let root = &fixture.fixture.installation.root;
    let temporary_name = root.file_name().unwrap().to_string_lossy();
    let displaced = root
        .parent()
        .unwrap()
        .join(format!("{temporary_name}-candidate-record-binding-{label}"));
    assert!(!displaced.exists());
    let expected_bytes = fixture.fixture.canonical_bytes();
    let hook_bytes = expected_bytes.clone();
    let hook_canonical = canonical;
    let hook_displaced = displaced.clone();
    let hook = move || {
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::write(&hook_canonical, hook_bytes).unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
    };
    (retained_identity, displaced, expected_bytes, hook)
}

fn assert_same_byte_replacement(
    fixture: &CandidatePreserveFixture,
    retained_identity: (u64, u64),
    displaced: &Path,
    expected_bytes: &[u8],
) {
    let canonical = canonical_journal(&fixture.fixture.installation.root);
    assert_eq!(fixture.fixture.canonical_bytes(), expected_bytes);
    assert_eq!(fs::read(displaced).unwrap(), expected_bytes);
    assert_eq!(inode_identity(displaced), retained_identity);
    assert_ne!(inode_identity(&canonical), retained_identity);
}

fn reset_effect_counts() {
    reset_new_state_target_create_attempt_count();
    reset_new_state_target_normalize_attempt_count();
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_archived_candidate_preserve_move_attempt_count();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
}

fn effect_counts() -> [usize; 5] {
    [
        new_state_target_create_attempt_count(),
        new_state_target_normalize_attempt_count(),
        new_state_candidate_preserve_move_attempt_count(),
        archived_candidate_preserve_move_attempt_count(),
        active_reblit_candidate_preserve_exchange_attempt_count(),
    ]
}

fn expected_effect_counts(case: EffectCase) -> [usize; 5] {
    match case {
        EffectCase::CreateTarget => [1, 0, 0, 0, 0],
        EffectCase::NormalizeTarget => [0, 1, 0, 0, 0],
        EffectCase::MoveNewState => [0, 0, 1, 0, 0],
        EffectCase::MoveArchived => [0, 0, 0, 1, 0],
        EffectCase::ExchangeActiveReblit => [0, 0, 0, 0, 1],
    }
}

fn select<'reservation>(
    fixture: &CandidatePreserveFixture,
    reservation: &'reservation ActiveStateReservation,
    journal: &crate::transition_journal::TransitionJournalStore,
) -> UsrRollbackCandidatePreserveApplyEffectSelection<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("staged candidate evidence did not admit Apply")
    };
    authority
        .into_effect_selection(&UsrRollbackCandidatePreserveEffectSeal::new_for_test(), journal)
        .unwrap()
}

fn reconcile_selected(
    case: EffectCase,
    selected: UsrRollbackCandidatePreserveApplyEffectSelection<'_>,
    journal: &crate::transition_journal::TransitionJournalStore,
) -> bool {
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    match (case, selected) {
        (EffectCase::CreateTarget, UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease)) => {
            lease.reconcile(&seal, journal).is_err()
        }
        (
            EffectCase::NormalizeTarget,
            UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease),
        ) => lease.reconcile(&seal, journal).is_err(),
        (EffectCase::MoveNewState, UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease)) => {
            lease.reconcile(&seal, journal).is_err()
        }
        (EffectCase::MoveArchived, UsrRollbackCandidatePreserveApplyEffectSelection::MoveArchived(lease)) => {
            lease.reconcile(&seal, journal).is_err()
        }
        (
            EffectCase::ExchangeActiveReblit,
            UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit(lease),
        ) => lease.reconcile(&seal, journal).is_err(),
        _ => panic!("candidate operation selected the wrong effect family"),
    }
}

fn assert_post_effect_layout(case: EffectCase, fixture: &CandidatePreserveFixture) {
    match case {
        EffectCase::CreateTarget => {
            let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
            assert!(target.is_dir());
            assert_eq!(fs::read_dir(target).unwrap().count(), 0);
            assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
        }
        EffectCase::NormalizeTarget => {
            let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
            assert_eq!(fs::metadata(target).unwrap().permissions().mode() & 0o7777, 0o700);
            assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
        }
        EffectCase::MoveNewState => {
            let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
            assert!(target.join("usr").is_dir());
            assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
        }
        EffectCase::MoveArchived => {
            let target = fixture
                .fixture
                .installation
                .root
                .join(".cast/root")
                .join(fixture.fixture.candidate_state.to_string());
            assert!(target.join("usr").is_dir());
            assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
        }
        EffectCase::ExchangeActiveReblit => {
            let target = reserved_active_reblit_wrapper_path(fixture, CandidateLayout::Staged);
            assert!(target.join("usr").is_dir());
            assert_eq!(
                fs::read_dir(fixture.fixture.installation.staging_dir())
                    .unwrap()
                    .count(),
                0,
            );
        }
    }
}

fn assert_fresh_restart_does_not_repeat(case: EffectCase, fixture: &CandidatePreserveFixture) {
    let counts = effect_counts();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    match case {
        EffectCase::CreateTarget | EffectCase::NormalizeTarget => {
            let selected = select(fixture, &reservation, &journal);
            assert!(matches!(
                selected,
                UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(_)
            ));
        }
        EffectCase::MoveNewState | EffectCase::MoveArchived | EffectCase::ExchangeActiveReblit => {
            assert!(matches!(
                fixture.capture(&journal, &reservation),
                UsrRollbackCandidatePreserveAdmission::Finish(_)
            ));
        }
    }
    assert_eq!(effect_counts(), counts);
}

#[test]
fn startup_candidate_preserve_same_byte_predecessor_replacement_before_effect_never_authorizes_any_operation() {
    let mut cases = 0;
    for historical in [false, true] {
        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for case in EffectCase::ALL {
                for source in case.record_sources() {
                    cases += 1;
                    let fixture = case.fixture(historical, source, outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let UsrRollbackCandidatePreserveAdmission::Apply(authority) =
                        fixture.capture(&journal, &reservation)
                    else {
                        panic!("{case:?} did not admit Apply")
                    };
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                    let label = format!("pre-{case:?}-{historical}-{source:?}-{outcome:?}");
                    let (identity, displaced, bytes, replace) = same_byte_replacement_hook(&fixture, &label);
                    replace();
                    reset_effect_counts();

                    assert!(
                        authority
                            .into_effect_selection(&UsrRollbackCandidatePreserveEffectSeal::new_for_test(), &journal)
                            .is_err(),
                        "{label}",
                    );

                    assert_eq!(effect_counts(), [0; 5], "{label}");
                    assert_eq!(fixture.fixture.database_snapshot(), database_before, "{label}");
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before, "{label}");
                    assert_eq!(root_abi_snapshot(&fixture.fixture.installation.root), root_abi_before, "{label}");
                    assert_same_byte_replacement(&fixture, identity, &displaced, &bytes);
                    drop(journal);
                    fs::remove_file(displaced).unwrap();
                }
            }
        }
    }
    assert_eq!(cases, 44, "pre-effect record-binding matrix drifted");
}

#[test]
fn startup_candidate_preserve_same_byte_predecessor_replacement_after_physical_effect_never_becomes_success() {
    let mut cases = 0;
    for historical in [false, true] {
        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for case in EffectCase::ALL {
                for source in case.record_sources() {
                    cases += 1;
                    let fixture = case.fixture(historical, source, outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let selected = select(&fixture, &reservation, &journal);
                    let database_before = fixture.fixture.database_snapshot();
                    let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                    let label = format!("post-{case:?}-{historical}-{source:?}-{outcome:?}");
                    let (identity, displaced, bytes, replace) = same_byte_replacement_hook(&fixture, &label);
                    case.arm_after_physical_effect(replace);
                    reset_effect_counts();

                    assert!(reconcile_selected(case, selected, &journal), "{label}");

                    assert_eq!(effect_counts(), expected_effect_counts(case), "{label}");
                    assert_eq!(fixture.fixture.database_snapshot(), database_before, "{label}");
                    assert_eq!(root_abi_snapshot(&fixture.fixture.installation.root), root_abi_before, "{label}");
                    assert_same_byte_replacement(&fixture, identity, &displaced, &bytes);
                    assert_post_effect_layout(case, &fixture);
                    drop(journal);
                    drop(reservation);
                    fs::remove_file(displaced).unwrap();
                    assert_fresh_restart_does_not_repeat(case, &fixture);
                }
            }
        }
    }
    assert_eq!(cases, 44, "post-effect record-binding matrix drifted");
}

#[test]
fn startup_candidate_preparation_restart_authority_rejects_same_bytes_at_a_successor_inode() {
    let mut cases = 0;
    for historical in [false, true] {
        for source in CandidateSource::ALL {
            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for case in [EffectCase::CreateTarget, EffectCase::NormalizeTarget] {
                    cases += 1;
                    let fixture = case.fixture(historical, RecordSourceCase::Candidate(source), outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let selected = select(&fixture, &reservation, &journal);
                    reset_effect_counts();
                    let restart = match (case, selected) {
                        (
                            EffectCase::CreateTarget,
                            UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease),
                        ) => match lease
                            .reconcile(&UsrRollbackCandidatePreserveEffectSeal::new_for_test(), &journal)
                            .unwrap()
                        {
                            UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired(
                                authority,
                            ) => authority,
                            _ => panic!("create-target preparation did not require restart"),
                        },
                        (
                            EffectCase::NormalizeTarget,
                            UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease),
                        ) => match lease
                            .reconcile(&UsrRollbackCandidatePreserveEffectSeal::new_for_test(), &journal)
                            .unwrap()
                        {
                            UsrRollbackNewStateCandidatePreserveNormalizeTargetReconciliation::RestartRequired(
                                authority,
                            ) => authority,
                            _ => panic!("normalize-target preparation did not require restart"),
                        },
                        _ => panic!("preparation selected the wrong effect family"),
                    };
                    let database_before = fixture.fixture.database_snapshot();
                    let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                    let label = format!("restart-{case:?}-{historical}-{source:?}-{outcome:?}");
                    let (identity, displaced, bytes, replace) = same_byte_replacement_hook(&fixture, &label);
                    replace();

                    assert!(restart.into_exact_source_record(&journal).is_err(), "{label}");

                    assert_eq!(effect_counts(), expected_effect_counts(case), "{label}");
                    assert_eq!(fixture.fixture.database_snapshot(), database_before, "{label}");
                    assert_eq!(root_abi_snapshot(&fixture.fixture.installation.root), root_abi_before, "{label}");
                    assert_same_byte_replacement(&fixture, identity, &displaced, &bytes);
                    assert_post_effect_layout(case, &fixture);
                    drop(journal);
                    drop(reservation);
                    fs::remove_file(displaced).unwrap();
                    assert_fresh_restart_does_not_repeat(case, &fixture);
                }
            }
        }
    }
    assert_eq!(cases, 16, "preparation restart matrix drifted");
}
