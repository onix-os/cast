use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    Installation, db,
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_identity::{
        ActiveReblitReplacementRecoveryError, recover_active_reblit_replacement_residue_with_explicit_context_for_test,
    },
    transition_journal::{
        Operation, Phase, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity,
        TransitionJournalStore, TransitionRecord, TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::super::{
    ActiveReblitReplacementMutationAuthorityProvider, MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
    active_state_snapshot::ActiveStateReservation, startup_gate,
};

const RECORD_TRANSITION: &str = "0123456789abcdef0123456789abcdef";
const FOREIGN_TRANSITION: &str = "fedcba9876543210fedcba9876543210";

struct Fixture {
    _temporary: tempfile::TempDir,
    system: MutableSystemCapabilities,
    record: TransitionRecord,
    replacement: PathBuf,
}

#[test]
fn compatible_database_and_active_selection_admit_restrictive_replacement_repair() {
    let fixture = Fixture::new(false, false);
    let reservation = ActiveStateReservation::acquire().unwrap();

    let result = startup_gate::CleanSystemStartup::enter(&fixture.system, &reservation);

    assert!(matches!(result, Err(startup_gate::Error::RecoveryPending(_))));
    assert_eq!(mode(&fixture.replacement), 0o700);
}

#[test]
fn foreign_in_flight_database_ownership_causes_zero_replacement_chmod() {
    let fixture = Fixture::new(false, true);
    write_replacement_payload(&fixture.replacement, b"database-conflict");
    let before = exact_witness(&fixture.replacement);
    let in_flight_before = fixture.state_db().audit_in_flight_transition().unwrap();
    let reservation = ActiveStateReservation::acquire().unwrap();

    let result = startup_gate::CleanSystemStartup::enter(&fixture.system, &reservation);

    assert!(matches!(
        result,
        Err(startup_gate::Error::ActiveReblitReplacementRecovery(
            ActiveReblitReplacementRecoveryError::MutationAuthority { .. }
        ))
    ));
    assert_eq!(exact_witness(&fixture.replacement), before);
    assert_eq!(
        fs::read(fixture.replacement.join("sentinel")).unwrap(),
        b"database-conflict"
    );
    assert_eq!(
        fixture.state_db().audit_in_flight_transition().unwrap(),
        in_flight_before
    );
}

#[test]
fn stale_active_selection_causes_zero_replacement_chmod() {
    let fixture = Fixture::new(true, false);
    write_replacement_payload(&fixture.replacement, b"stale-active");
    let before = exact_witness(&fixture.replacement);
    assert_eq!(fixture.installation().active_state, None);
    let reservation = ActiveStateReservation::acquire().unwrap();

    let result = startup_gate::CleanSystemStartup::enter(&fixture.system, &reservation);

    assert!(matches!(
        result,
        Err(startup_gate::Error::ActiveReblitReplacementRecovery(
            ActiveReblitReplacementRecoveryError::MutationAuthority { .. }
        ))
    ));
    assert_eq!(exact_witness(&fixture.replacement), before);
    assert_eq!(fs::read(fixture.replacement.join("sentinel")).unwrap(), b"stale-active");
    assert_eq!(fixture.state_db().audit_in_flight_transition().unwrap(), None);
}

#[test]
fn mismatched_record_cannot_reuse_replacement_mutation_authority() {
    let fixture = Fixture::new(false, false);
    write_replacement_payload(&fixture.replacement, b"record-context");
    let before = exact_witness(&fixture.replacement);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let journal = retained_journal(&fixture);
    let mut provider = mutation_authority_provider(&fixture, &journal, &reservation);
    let mut mismatched_record = fixture.record.clone();
    mismatched_record.generation += 1;

    let result = recover_active_reblit_replacement_residue_with_explicit_context_for_test(
        fixture.installation(),
        &journal,
        &mismatched_record,
        &mut provider,
    );

    assert!(matches!(
        result,
        Err(ActiveReblitReplacementRecoveryError::MutationAuthority { .. })
    ));
    assert_eq!(exact_witness(&fixture.replacement), before);
    assert_eq!(
        fs::read(fixture.replacement.join("sentinel")).unwrap(),
        b"record-context"
    );
    assert_eq!(journal.load().unwrap(), Some(fixture.record.clone()));
}

#[test]
fn mismatched_installation_cannot_reuse_replacement_mutation_authority() {
    let fixture = Fixture::new(false, false);
    let other = Fixture::new(false, false);
    write_replacement_payload(&fixture.replacement, b"bound-installation");
    write_replacement_payload(&other.replacement, b"other-installation");
    let before = exact_witness(&fixture.replacement);
    let other_before = exact_witness(&other.replacement);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let journal = retained_journal(&fixture);
    let mut provider = mutation_authority_provider(&fixture, &journal, &reservation);

    let result = recover_active_reblit_replacement_residue_with_explicit_context_for_test(
        other.installation(),
        &journal,
        &fixture.record,
        &mut provider,
    );

    assert!(matches!(
        result,
        Err(ActiveReblitReplacementRecoveryError::MutationAuthority { .. })
    ));
    assert_eq!(exact_witness(&fixture.replacement), before);
    assert_eq!(exact_witness(&other.replacement), other_before);
    assert_eq!(
        fs::read(fixture.replacement.join("sentinel")).unwrap(),
        b"bound-installation"
    );
    assert_eq!(
        fs::read(other.replacement.join("sentinel")).unwrap(),
        b"other-installation"
    );
}

impl Fixture {
    fn new(stale_active: bool, foreign_in_flight: bool) -> Self {
        let temporary = private_installation_tempdir();
        let provisioned = Installation::open(temporary.path(), None).unwrap();
        let state_db = db::state::Database::new(provisioned.db_path("state").to_str().unwrap()).unwrap();
        let layout_db = db::layout::Database::new(provisioned.db_path("layout").to_str().unwrap()).unwrap();
        let record_transition = transition_id(RECORD_TRANSITION);
        let candidate = state_db
            .add_with_transition(&record_transition, &[], Some("active-reblit candidate"), None)
            .unwrap();
        let provenance = db::state::MetadataProvenance::from_outputs(
            b"NAME=active-reblit-startup-test\n",
            b"let active_reblit_startup_test = true\n",
        );
        state_db
            .insert_fresh_metadata_provenance_if_transition_matches(candidate.id, &record_transition, &provenance)
            .unwrap();
        state_db
            .clear_transition_if_matches(candidate.id, &record_transition)
            .unwrap();
        if foreign_in_flight {
            state_db
                .add_with_transition(
                    &transition_id(FOREIGN_TRANSITION),
                    &[],
                    Some("foreign in-flight transition"),
                    None,
                )
                .unwrap();
        }

        let usr = temporary.path().join("usr");
        let staging_usr = temporary.path().join(".cast/root/staging/usr");
        let (previous_token, previous_runtime) = create_marked_tree(&usr);
        write_state_id(&usr, i32::from(candidate.id));
        let (candidate_token, candidate_runtime) = create_marked_tree(&staging_usr);
        let installation = if stale_active {
            provisioned
        } else {
            drop(provisioned);
            Installation::open(temporary.path(), None).unwrap()
        };
        let mut record = TransitionRecord::preparing(
            record_transition,
            RuntimeEpoch::capture().unwrap(),
            Operation::ActiveReblit,
            Some(candidate.id.into()),
            candidate_token,
            candidate_runtime,
            Previous {
                id: Some(candidate.id.into()),
                tree_token: previous_token,
                usr_runtime_identity: previous_runtime,
                origin: PreviousOrigin::ActiveReblitCorrupt,
            },
            true,
            true,
            QuarantineName::parse("failed-startup-authority-test").unwrap(),
        )
        .unwrap();
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
        journal.create(&record).unwrap();
        for phase in [Phase::CandidatePrepareStarted, Phase::CandidatePrepared] {
            let mut next = record.clone();
            next.generation += 1;
            next.phase = phase;
            journal.advance(&record, &next).unwrap();
            record = next;
        }
        drop(journal);
        let replacement = installation.state_quarantine_dir().join(format!(
            "replaced-active-reblit-wrapper-{}-{}-0",
            record.previous.id.unwrap(),
            record.previous.tree_token.as_str(),
        ));
        fs::create_dir(&replacement).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o500)).unwrap();

        let system = MutableSystemCapabilities::from_test_parts(
            &MutableSystemCapabilitiesTestSeal::new(),
            installation,
            state_db,
            layout_db,
        );
        Self {
            _temporary: temporary,
            system,
            record,
            replacement,
        }
    }

    fn installation(&self) -> &Installation {
        self.system.installation()
    }

    fn state_db(&self) -> &db::state::Database {
        self.system.state_db()
    }
}

fn retained_journal(fixture: &Fixture) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(fixture.installation().root_directory(), &fixture.installation().root)
        .unwrap()
}

fn mutation_authority_provider<'authority>(
    fixture: &'authority Fixture,
    journal: &'authority TransitionJournalStore,
    reservation: &'authority ActiveStateReservation,
) -> ActiveReblitReplacementMutationAuthorityProvider<'authority> {
    ActiveReblitReplacementMutationAuthorityProvider::new_for_test(
        fixture.installation(),
        journal,
        fixture.state_db(),
        reservation,
        &fixture.record,
        fixture.state_db().audit_in_flight_transition().unwrap(),
    )
}

fn transition_id(value: &str) -> TransitionId {
    TransitionId::parse(value).unwrap()
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

fn write_state_id(usr: &Path, state: i32) {
    let path = usr.join(".stateID");
    fs::write(&path, state.to_string()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn write_replacement_payload(path: &Path, payload: &[u8]) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(path.join("sentinel"), payload).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
}

fn exact_witness(path: &Path) -> (u64, u64, u32, i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (
        metadata.dev(),
        metadata.ino(),
        metadata.permissions().mode() & 0o7777,
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
