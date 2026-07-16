use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

use crate::transition_identity::{
    ActiveReblitReplacementRecovery, ActiveReblitReplacementRecoveryError,
    arm_before_active_reblit_replacement_normalization_preflight,
    recover_active_reblit_replacement_residue_for_namespace_test as recover_active_reblit_replacement_residue,
};
use crate::transition_journal::{
    BootRollback, CandidateRollback, Phase, RollbackAction, RollbackPlan, TransitionJournalStore,
};

use super::*;

#[test]
fn candidate_prepared_restrictive_replacement_residues_are_normalized_then_admitted() {
    for residue_mode in [0o000, 0o500] {
        let mut fixture = Fixture::active_reblit();
        let journal = advance_journal(
            &mut fixture,
            &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
        );
        let replacement = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
        create_private_directory(&replacement);
        set_mode(&replacement, residue_mode);

        assert_eq!(
            recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record).unwrap(),
            ActiveReblitReplacementRecovery::Normalized
        );
        assert_eq!(mode(&replacement), 0o700);
        assert_eq!(fixture.assess(), Ok(()));
    }
}

#[test]
fn rollback_from_candidate_prepared_can_finish_the_same_replacement_residue() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let mut rollback = fixture.record.clone();
    rollback.generation += 1;
    rollback.phase = Phase::RollbackDecided;
    rollback.rollback = Some(RollbackPlan {
        source: ForwardPhase::CandidatePrepared,
        previous_archive: RollbackAction::NotRequired,
        usr_exchange: RollbackAction::NotRequired,
        candidate: CandidateRollback {
            action: RollbackAction::Pending,
            disposition: AbortDisposition::Quarantine,
        },
        fresh_db: RollbackAction::NotRequired,
        boot: BootRollback::NotRequired,
        external_effects_may_remain: false,
    });
    journal.advance(&fixture.record, &rollback).unwrap();
    fixture.record = rollback;
    let replacement = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    create_private_directory(&replacement);
    set_mode(&replacement, 0o000);

    assert_eq!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record).unwrap(),
        ActiveReblitReplacementRecovery::Normalized
    );
    assert_eq!(mode(&replacement), 0o700);
    assert_eq!(fixture.assess(), Ok(()));
}

#[test]
fn canonical_populated_rollback_wrapper_is_delegated_to_phase_policy_unchanged() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let mut rollback = candidate_prepared_rollback(&fixture.record);
    journal.advance(&fixture.record, &rollback).unwrap();
    fixture.record = rollback.clone();
    rollback.generation += 1;
    rollback.phase = Phase::CandidatePreserveIntent;
    journal.advance(&fixture.record, &rollback).unwrap();
    fixture.record = rollback;

    let replacement = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    create_private_directory(&replacement);
    fs::rename(fixture.installation.staging_path("usr"), replacement.join("usr")).unwrap();
    let before = fs::metadata(&replacement).unwrap();

    assert_eq!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record).unwrap(),
        ActiveReblitReplacementRecovery::AlreadyCanonical
    );
    let after = fs::metadata(&replacement).unwrap();
    assert_eq!(
        (after.dev(), after.ino(), after.mode()),
        (before.dev(), before.ino(), before.mode())
    );
    assert!(replacement.join("usr").is_dir());
    assert_eq!(fixture.assess(), Ok(()));
}

#[test]
fn replacement_residue_is_never_normalized_before_or_after_candidate_prepared() {
    for phases in [
        vec![Phase::CandidatePrepareStarted],
        vec![
            Phase::CandidatePrepareStarted,
            Phase::CandidatePrepared,
            Phase::TransactionTriggersStarted,
        ],
    ] {
        let mut fixture = Fixture::active_reblit();
        let journal = advance_journal(&mut fixture, &phases);
        let replacement = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
        create_private_directory(&replacement);
        set_mode(&replacement, 0o500);

        assert_eq!(
            recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record).unwrap(),
            ActiveReblitReplacementRecovery::NotApplicable
        );
        assert_eq!(mode(&replacement), 0o500);
        assert!(matches!(fixture.snapshot(), Err(CaptureError::UnsafeDirectory { .. })));
    }
}

#[test]
fn foreign_replacement_residue_is_untouched_and_rejected() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let foreign = fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-43-{}-0",
        fixture.record.previous.tree_token.as_str()
    ));
    create_private_directory(&foreign);
    set_mode(&foreign, 0o500);

    assert_eq!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record).unwrap(),
        ActiveReblitReplacementRecovery::Absent
    );
    assert_eq!(mode(&foreign), 0o500);
    assert!(matches!(fixture.snapshot(), Err(CaptureError::UnsafeDirectory { .. })));
}

#[test]
fn ambiguous_current_transition_replacements_are_both_untouched() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let first = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    let second = fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-1",
        fixture.record.previous.id.unwrap(),
        fixture.record.previous.tree_token.as_str()
    ));
    for (path, payload) in [(&first, b"first".as_slice()), (&second, b"second".as_slice())] {
        create_private_directory(path);
        fs::write(path.join("sentinel"), payload).unwrap();
        set_mode(path, 0o500);
    }
    let first_before = identity_and_mode(&first);
    let second_before = identity_and_mode(&second);

    assert!(matches!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record),
        Err(ActiveReblitReplacementRecoveryError::Ambiguous { count: 2, .. })
    ));
    assert_eq!(identity_and_mode(&first), first_before);
    assert_eq!(identity_and_mode(&second), second_before);
    assert_eq!(fs::read(first.join("sentinel")).unwrap(), b"first");
    assert_eq!(fs::read(second.join("sentinel")).unwrap(), b"second");
}

#[test]
fn second_current_transition_replacement_inserted_before_chmod_leaves_both_inodes_untouched() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let first = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    let staged_second = fixture
        .installation
        .state_quarantine_dir()
        .join("replacement-race-staged-second");
    let second = fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-1",
        fixture.record.previous.id.unwrap(),
        fixture.record.previous.tree_token.as_str()
    ));
    for (path, payload) in [(&first, b"first".as_slice()), (&staged_second, b"second".as_slice())] {
        create_private_directory(path);
        fs::write(path.join("sentinel"), payload).unwrap();
        set_mode(path, 0o500);
    }
    let first_before = identity_and_mode(&first);
    let second_before = identity_and_mode(&staged_second);
    let staged_second_path = staged_second.clone();
    let second_path = second.clone();
    arm_before_active_reblit_replacement_normalization_preflight(move |_| {
        fs::rename(&staged_second_path, &second_path).unwrap();
    });

    assert!(matches!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record),
        Err(ActiveReblitReplacementRecoveryError::Ambiguous { count: 2, .. })
    ));
    assert_eq!(identity_and_mode(&first), first_before);
    assert_eq!(identity_and_mode(&second), second_before);
    assert!(!staged_second.exists());
    assert_eq!(fs::read(first.join("sentinel")).unwrap(), b"first");
    assert_eq!(fs::read(second.join("sentinel")).unwrap(), b"second");
}

#[test]
fn journal_advance_before_chmod_preserves_both_replacement_inodes_and_names() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let original = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    let alternate = fixture
        .installation
        .state_quarantine_dir()
        .join("journal-race-alternate");
    let displaced = fixture
        .installation
        .state_quarantine_dir()
        .join("journal-race-displaced");
    for (path, payload) in [
        (&original, b"original".as_slice()),
        (&alternate, b"alternate".as_slice()),
    ] {
        create_private_directory(path);
        fs::write(path.join("sentinel"), payload).unwrap();
        set_mode(path, 0o500);
    }
    let original_before = identity_and_mode(&original);
    let alternate_before = identity_and_mode(&alternate);
    let expected = fixture.record.clone();
    let mut next = expected.clone();
    next.generation += 1;
    next.phase = Phase::TransactionTriggersStarted;
    let advanced = next.clone();
    let original_path = original.clone();
    let alternate_path = alternate.clone();
    let displaced_path = displaced.clone();
    arm_before_active_reblit_replacement_normalization_preflight(move |store| {
        store.advance(&expected, &advanced).unwrap();
        fs::rename(&original_path, &displaced_path).unwrap();
        fs::rename(&alternate_path, &original_path).unwrap();
    });

    assert!(matches!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record),
        Err(ActiveReblitReplacementRecoveryError::JournalChanged {
            actual_generation: Some(generation),
            ..
        }) if generation == next.generation
    ));
    assert_eq!(journal.load().unwrap(), Some(next));
    assert_eq!(identity_and_mode(&displaced), original_before);
    assert_eq!(identity_and_mode(&original), alternate_before);
    assert!(!alternate.exists());
    assert_eq!(fs::read(displaced.join("sentinel")).unwrap(), b"original");
    assert_eq!(fs::read(original.join("sentinel")).unwrap(), b"alternate");
}

#[test]
fn public_name_substitution_before_normalization_chmods_neither_inode() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let original = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    let displaced = fixture
        .installation
        .state_quarantine_dir()
        .join("failed-replacement-race");
    create_private_directory(&original);
    fs::write(original.join("sentinel"), b"original").unwrap();
    set_mode(&original, 0o500);
    let original_before = identity_and_mode(&original);
    let replacement_path = original.clone();
    let displaced_path = displaced.clone();
    arm_before_active_reblit_replacement_normalization_preflight(move |_| {
        fs::rename(&replacement_path, &displaced_path).unwrap();
        create_private_directory(&replacement_path);
        fs::write(replacement_path.join("sentinel"), b"replacement").unwrap();
        set_mode(&replacement_path, 0o500);
    });

    assert!(matches!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record),
        Err(ActiveReblitReplacementRecoveryError::Changed { .. })
    ));
    assert_eq!(identity_and_mode(&displaced), original_before);
    assert_eq!(mode(&original), 0o500);
    assert_ne!(identity_and_mode(&original).0, original_before.0);
    assert_eq!(fs::read(displaced.join("sentinel")).unwrap(), b"original");
    assert_eq!(fs::read(original.join("sentinel")).unwrap(), b"replacement");
}

#[test]
fn populated_replacement_is_normalized_durably_but_never_admitted() {
    let mut fixture = Fixture::active_reblit();
    let journal = advance_journal(
        &mut fixture,
        &[Phase::CandidatePrepareStarted, Phase::CandidatePrepared],
    );
    let replacement = active_reblit_wrapper_path(&fixture.installation, &fixture.record);
    create_private_directory(&replacement);
    fs::write(replacement.join("foreign"), b"payload").unwrap();
    set_mode(&replacement, 0o500);

    assert!(matches!(
        recover_active_reblit_replacement_residue(&fixture.installation, &journal, &fixture.record),
        Err(ActiveReblitReplacementRecoveryError::Namespace(_))
    ));
    assert_eq!(mode(&replacement), 0o700);
    assert_eq!(fs::read(replacement.join("foreign")).unwrap(), b"payload");
    assert!(matches!(
        fixture.snapshot(),
        Err(CaptureError::UnexpectedWrapperEntry { .. })
    ));
}

fn advance_journal(fixture: &mut Fixture, phases: &[Phase]) -> TransitionJournalStore {
    let journal =
        TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root)
            .unwrap();
    journal.create(&fixture.record).unwrap();
    for phase in phases {
        let mut next = fixture.record.clone();
        next.generation += 1;
        next.phase = *phase;
        journal.advance(&fixture.record, &next).unwrap();
        fixture.record = next;
    }
    journal
}

fn candidate_prepared_rollback(record: &TransitionRecord) -> TransitionRecord {
    let mut rollback = record.clone();
    rollback.generation += 1;
    rollback.phase = Phase::RollbackDecided;
    rollback.rollback = Some(RollbackPlan {
        source: ForwardPhase::CandidatePrepared,
        previous_archive: RollbackAction::NotRequired,
        usr_exchange: RollbackAction::NotRequired,
        candidate: CandidateRollback {
            action: RollbackAction::Pending,
            disposition: AbortDisposition::Quarantine,
        },
        fresh_db: RollbackAction::NotRequired,
        boot: BootRollback::NotRequired,
        external_effects_may_remain: false,
    });
    rollback
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn identity_and_mode(path: &Path) -> ((u64, u64), u32) {
    let metadata = fs::metadata(path).unwrap();
    ((metadata.dev(), metadata.ino()), metadata.permissions().mode() & 0o7777)
}
