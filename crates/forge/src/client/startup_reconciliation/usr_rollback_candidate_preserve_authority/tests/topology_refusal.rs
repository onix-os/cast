//! Exact refusal contracts for lookalike or unauthorized preservation shapes.

use std::{
    ffi::CString,
    fs,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
            UsrRollbackCandidatePreserveTopology, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
    tree_marker::TreeMarkerStore,
};

use super::{
    fixture::{OperationKind, create_private_directory},
    support::{
        CandidateLayout, CandidatePreserveFixture, CandidateSource, active_reblit_wrapper_path, archived_slot_path,
        transition_quarantine_path,
    },
};

#[test]
fn startup_candidate_preserve_refuses_an_occupied_new_state_target() {
    let fixture = staged(OperationKind::NewState);
    let destination = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    create_private_directory(&destination);
    create_private_directory(&destination.join("usr"));
    require_deferred(&fixture);
}

#[test]
fn startup_candidate_preserve_refuses_every_controlled_non_private_new_state_target_mode() {
    const CONTROLLED_NON_PRIVATE_MODES: [u32; 15] = [
        0o701, 0o704, 0o705, 0o710, 0o711, 0o714, 0o715, 0o740, 0o741, 0o744, 0o745, 0o750, 0o751, 0o754, 0o755,
    ];

    for mode in CONTROLLED_NON_PRIVATE_MODES {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture = match layout {
                CandidateLayout::Staged => CandidatePreserveFixture::new_state_empty_quarantine_prefix(
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                ),
                CandidateLayout::Preserved => CandidatePreserveFixture::new(
                    OperationKind::NewState,
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                    CandidateLayout::Preserved,
                ),
            };
            let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
            fs::set_permissions(target, fs::Permissions::from_mode(mode)).unwrap();

            require_deferred(&fixture);
        }
    }
}

#[test]
fn startup_candidate_preserve_models_every_restrictive_new_state_target_residue_without_mutation() {
    for mode in [0o000, 0o100, 0o200, 0o300, 0o400, 0o500, 0o600] {
        let fixture = new_state_target_residue(mode);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let target_before = target_witness(&target);
        let journal_before = fixture.fixture.canonical_bytes();
        let database_before = fixture.fixture.database_snapshot();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();

        let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
            panic!("restrictive target residue {mode:04o} did not admit Apply authority");
        };
        assert_eq!(
            authority.topology(),
            UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue
        );
        authority.revalidate(&journal).unwrap();

        assert_eq!(target_witness(&target), target_before, "mode {mode:04o}");
        assert_eq!(fixture.fixture.canonical_bytes(), journal_before, "mode {mode:04o}");
        assert_eq!(fixture.fixture.database_snapshot(), database_before, "mode {mode:04o}");
        assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
    }
}

#[test]
fn startup_candidate_preserve_keeps_payload_residue_distinct_from_empty_move_ready_evidence() {
    let fixture = staged(OperationKind::NewState);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    create_private_directory(&target);
    fs::write(target.join("foreign-payload"), b"retained residue payload").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
    let target_before = target_witness(&target);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();

    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("payload-bearing restrictive residue did not admit Apply authority");
    };
    assert_eq!(
        authority.topology(),
        UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue
    );
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    reset_new_state_candidate_preserve_move_attempt_count();
    let UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease) =
        authority.into_effect_selection(&seal, &journal).unwrap()
    else {
        panic!("opaque payload-bearing residue did not select normalization-only evidence");
    };
    drop(lease);

    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert_eq!(target_witness(&target), target_before);
    assert_eq!(
        fs::read(target.join("foreign-payload")).unwrap(),
        b"retained residue payload"
    );
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_candidate_preserve_refuses_unsafe_modes_and_wrong_target_types_without_mutation() {
    for mode in [0o001, 0o070, 0o702, 0o720, 0o770, 0o1700] {
        let fixture = staged(OperationKind::NewState);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        create_private_directory(&target);
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        require_deferred_preserving_target(&fixture, &target);
    }

    let fixture = staged(OperationKind::NewState);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    fs::write(&target, b"regular target occupant").unwrap();
    require_deferred_preserving_target(&fixture, &target);

    let fixture = staged(OperationKind::NewState);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let external = fixture
        .fixture
        .installation
        .root
        .join("candidate-target-symlink-sentinel");
    fs::write(&external, b"outside sentinel").unwrap();
    symlink(&external, &target).unwrap();
    require_deferred_preserving_target(&fixture, &target);
    assert_eq!(fs::read(external).unwrap(), b"outside sentinel");

    let fixture = staged(OperationKind::NewState);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let encoded = CString::new(target.as_os_str().as_bytes()).unwrap();
    // SAFETY: the encoded absolute fixture path remains live and names an
    // absent entry under the private quarantine directory.
    assert_eq!(unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
    require_deferred_preserving_target(&fixture, &target);
}

#[test]
fn startup_candidate_preserve_residue_revalidation_rejects_name_inode_mode_and_content_changes() {
    let fixture = new_state_target_residue(0o500);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let journal_before = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("restrictive target residue did not admit Apply authority");
    };
    let displaced = target.with_file_name("candidate-target-displaced-residue");
    fs::rename(&target, &displaced).unwrap();
    create_private_directory(&target);
    fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
    let displaced_after_race = target_witness(&displaced);
    let replacement_after_race = target_witness(&target);

    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(target_witness(&displaced), displaced_after_race);
    assert_eq!(target_witness(&target), replacement_after_race);
    assert_eq!(fixture.fixture.canonical_bytes(), journal_before);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    drop(reservation);
    drop(journal);

    let fixture = new_state_target_residue(0o500);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("restrictive target residue did not admit Apply authority");
    };
    fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
    let changed = target_witness(&target);
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(target_witness(&target), changed);
    drop(reservation);
    drop(journal);

    let fixture = new_state_target_residue(0o300);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("restrictive target residue did not admit Apply authority");
    };
    fs::write(target.join("raced-payload"), b"content race").unwrap();
    let changed = target_witness(&target);
    assert!(authority.revalidate(&journal).is_err());
    assert_eq!(target_witness(&target), changed);
    assert_eq!(fs::read(target.join("raced-payload")).unwrap(), b"content race");
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_candidate_preserve_refuses_missing_wrong_extra_and_transferred_archived_slots() {
    let fixture = staged(OperationKind::Archived);
    fs::remove_file(archived_slot_path(&fixture.fixture, &fixture.candidate_intent)).unwrap();
    require_deferred(&fixture);

    let fixture = staged(OperationKind::Archived);
    let slot = archived_slot_path(&fixture.fixture, &fixture.candidate_intent);
    let wrong = slot.with_file_name(format!(
        ".cast-state-slot-{}-{}",
        fixture.fixture.candidate_state,
        fixture.candidate_intent.previous.tree_token.as_str()
    ));
    fs::rename(slot, wrong).unwrap();
    require_deferred(&fixture);

    let fixture = staged(OperationKind::Archived);
    fs::hard_link(
        fixture.fixture.installation.staging_dir().join("usr/.cast-tree-id"),
        fixture
            .fixture
            .installation
            .root
            .join(".candidate-preserve-extra-marker-link"),
    )
    .unwrap();
    require_deferred(&fixture);

    let fixture = staged(OperationKind::Archived);
    fs::rename(
        archived_slot_path(&fixture.fixture, &fixture.candidate_intent),
        fixture.fixture.installation.staging_dir().join(format!(
            ".cast-state-slot-{}-{}",
            fixture.fixture.candidate_state,
            fixture.candidate_intent.candidate.tree_token.as_str()
        )),
    )
    .unwrap();
    require_deferred(&fixture);
}

#[test]
fn startup_candidate_preserve_refuses_missing_duplicate_and_wrong_active_reblit_reservations() {
    let fixture = staged(OperationKind::ActiveReblit);
    fs::remove_dir(fixture.fixture.active_reblit_reservation.as_ref().unwrap()).unwrap();
    require_deferred(&fixture);

    let fixture = staged(OperationKind::ActiveReblit);
    create_private_directory(&active_reblit_wrapper_path(
        &fixture.fixture,
        &fixture.candidate_intent,
        1,
    ));
    require_deferred(&fixture);

    let fixture = staged(OperationKind::ActiveReblit);
    let current = fixture.fixture.active_reblit_reservation.as_ref().unwrap();
    let wrong = fixture.fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-0",
        i32::from(fixture.fixture.previous_state) + 1,
        fixture.candidate_intent.previous.tree_token.as_str()
    ));
    fs::rename(current, wrong).unwrap();
    require_deferred(&fixture);
}

#[test]
fn startup_candidate_preserve_refuses_generic_quarantine_for_active_reblit() {
    let fixture = staged(OperationKind::ActiveReblit);
    let destination = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    create_private_directory(&destination);
    fs::rename(
        fixture.fixture.installation.staging_dir().join("usr"),
        destination.join("usr"),
    )
    .unwrap();
    require_deferred(&fixture);
}

#[test]
fn startup_candidate_preserve_refuses_empty_and_foreign_current_state_wrappers() {
    for kind in OperationKind::ALL {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture =
                CandidatePreserveFixture::new(kind, CandidateSource::Exchanged, RollbackActionOutcome::Applied, layout);
            create_private_directory(&state_wrapper(&fixture, i32::from(fixture.fixture.previous_state)));
            require_deferred(&fixture);

            let fixture = CandidatePreserveFixture::new(
                kind,
                CandidateSource::Exchanged,
                RollbackActionOutcome::AlreadySatisfied,
                layout,
            );
            create_foreign_state_wrapper(&fixture, i32::from(fixture.fixture.previous_state));
            require_deferred(&fixture);

            if kind == OperationKind::NewState {
                let fixture = CandidatePreserveFixture::new(
                    kind,
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                    layout,
                );
                create_private_directory(&state_wrapper(&fixture, i32::from(fixture.fixture.candidate_state)));
                require_deferred(&fixture);

                let fixture = CandidatePreserveFixture::new(
                    kind,
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::AlreadySatisfied,
                    layout,
                );
                create_foreign_state_wrapper(&fixture, i32::from(fixture.fixture.candidate_state));
                require_deferred(&fixture);
            }
        }
    }
}

#[test]
fn startup_candidate_preserve_refuses_empty_transition_wrapper_for_archived_and_active_reblit() {
    for kind in [OperationKind::Archived, OperationKind::ActiveReblit] {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture =
                CandidatePreserveFixture::new(kind, CandidateSource::Exchanged, RollbackActionOutcome::Applied, layout);
            create_private_directory(&transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent));
            require_deferred(&fixture);
        }
    }
}

#[test]
fn startup_candidate_preserve_allows_fingerprint_bound_unrelated_state_wrappers() {
    for kind in OperationKind::ALL {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture =
                CandidatePreserveFixture::new(kind, CandidateSource::Exchanged, RollbackActionOutcome::Applied, layout);
            create_foreign_state_wrapper(&fixture, 999);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            match (layout, fixture.capture(&journal, &reservation)) {
                (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                _ => panic!("unrelated state wrapper changed the admitted typestate"),
            }
        }
    }
}

#[test]
fn startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states() {
    for kind in [OperationKind::NewState, OperationKind::Archived] {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture =
                CandidatePreserveFixture::new(kind, CandidateSource::Exchanged, RollbackActionOutcome::Applied, layout);
            create_private_directory(&fixture.fixture.installation.root.join(format!(
                ".cast/root/.archived-candidate-slot-{}-{}-0",
                fixture.fixture.candidate_state,
                fixture.candidate_intent.candidate.tree_token.as_str()
            )));
            require_deferred(&fixture);

            let fixture = CandidatePreserveFixture::new(
                kind,
                CandidateSource::Exchanged,
                RollbackActionOutcome::AlreadySatisfied,
                layout,
            );
            create_private_directory(&fixture.fixture.installation.root.join(format!(
                ".cast/root/.previous-slot-{}-{}-0",
                fixture.fixture.previous_state,
                fixture.candidate_intent.previous.tree_token.as_str()
            )));
            require_deferred(&fixture);
        }
    }
}

#[test]
fn startup_candidate_preserve_retains_a_nonzero_active_reblit_reservation_index() {
    for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            layout,
        )
        .with_active_reblit_wrapper_index(7);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        match (layout, fixture.capture(&journal, &reservation)) {
            (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                assert_eq!(
                    authority.topology(),
                    UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index: 7 }
                );
                authority.revalidate(&journal).unwrap();
            }
            (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                assert_eq!(
                    authority.topology(),
                    UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index: 7 }
                );
                authority.revalidate(&journal).unwrap();
            }
            _ => panic!("nonzero active-reblit wrapper index selected the wrong typestate"),
        }
    }
}

fn staged(kind: OperationKind) -> CandidatePreserveFixture {
    CandidatePreserveFixture::new(
        kind,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    )
}

fn new_state_target_residue(mode: u32) -> CandidatePreserveFixture {
    let fixture = staged(OperationKind::NewState);
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    create_private_directory(&target);
    fs::set_permissions(target, fs::Permissions::from_mode(mode)).unwrap();
    fixture
}

fn require_deferred(fixture: &CandidatePreserveFixture) {
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackCandidatePreserveAdmission::Deferred
    ));
    fixture.assert_evidence_unchanged(&before);
}

fn require_deferred_preserving_target(fixture: &CandidatePreserveFixture, target: &Path) {
    let target_before = target_witness(target);
    let journal_before = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation),
        UsrRollbackCandidatePreserveAdmission::Deferred
    ));
    assert_eq!(target_witness(target), target_before);
    assert_eq!(fixture.fixture.canonical_bytes(), journal_before);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
}

#[derive(Debug, Eq, PartialEq)]
struct TargetWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    link_target: Option<PathBuf>,
    regular_payload: Option<Vec<u8>>,
}

fn target_witness(path: &Path) -> TargetWitness {
    let metadata = fs::symlink_metadata(path).unwrap();
    let file_type = metadata.file_type();
    TargetWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        link_target: file_type.is_symlink().then(|| fs::read_link(path).unwrap()),
        regular_payload: file_type.is_file().then(|| fs::read(path).unwrap()),
    }
}

fn state_wrapper(fixture: &CandidatePreserveFixture, state: i32) -> PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/root")
        .join(state.to_string())
}

fn create_foreign_state_wrapper(fixture: &CandidatePreserveFixture, state: i32) {
    let wrapper = state_wrapper(fixture, state);
    create_private_directory(&wrapper);
    let usr = wrapper.join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, fs::Permissions::from_mode(0o755)).unwrap();
    let state_id = usr.join(".stateID");
    fs::write(&state_id, state.to_string().as_bytes()).unwrap();
    fs::set_permissions(&state_id, fs::Permissions::from_mode(0o644)).unwrap();
    TreeMarkerStore::open_path(&usr)
        .unwrap()
        .adopt_or_create_before_journal()
        .unwrap();
}
