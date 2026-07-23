use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    Installation, Registry, State,
    test_support::private_installation_tempdir,
    transition_identity::{LegacyBootRepairAuthorityError, StatefulTreeIdentity},
    transition_journal::{StorageError, TransitionJournalStore},
};

use super::{Error, arm_before_worker, synchronize};
use crate::client::{Client, boot, record_state_id};

struct Fixture {
    _temporary: tempfile::TempDir,
    client: Client,
    identity: StatefulTreeIdentity,
    restored: State,
}

impl Fixture {
    fn new() -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let mut client = Client::mocked(installation, Registry::default()).unwrap();
        let restored = client
            .state_db
            .add(&[], Some("legacy boot repair restored state"), None)
            .unwrap();

        let live_usr = client.installation.root.join("usr");
        fs::create_dir(&live_usr).unwrap();
        fs::set_permissions(&live_usr, fs::Permissions::from_mode(0o755)).unwrap();
        record_state_id(&client.installation.root, restored.id).unwrap();
        client.installation.active_state = Some(restored.id);

        let candidate = client.installation.staging_path("usr");
        fs::create_dir(&candidate).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        let identity = StatefulTreeIdentity::prepare_active_reblit_candidate(
            &client.installation,
            &client.state_db,
            &candidate,
            restored.id,
        )
        .unwrap();

        Self {
            _temporary: temporary,
            client,
            identity,
            restored,
        }
    }

    fn authorize(&self) -> crate::transition_identity::LegacyBootRepairAuthority<'_> {
        self.identity
            .authorize_legacy_boot_repair(&self.client.installation, &self.client.state_db)
            .unwrap()
    }
}

#[test]
fn legacy_worker_rejects_a_client_with_a_different_state_database_capability() {
    let fixture = Fixture::new();
    let authority = fixture.authorize();
    let other = Client::mocked(fixture.client.installation.clone(), Registry::default()).unwrap();
    boot::reset_boot_synchronize_attempt_count();

    let error = synchronize(&other, &fixture.restored, authority).unwrap_err();

    assert!(matches!(
        error,
        Error::Authority(LegacyBootRepairAuthorityError::StateDatabaseMismatch)
    ));
    assert_eq!(boot::boot_synchronize_attempt_count(), 0);
}

#[test]
fn legacy_worker_rejects_public_journal_replacement_during_boot() {
    let fixture = Fixture::new();
    let authority = fixture.authorize();
    let root = fixture.client.installation.root.clone();
    arm_before_worker(move || {
        let journal = root.join(".cast/journal");
        let displaced = root.join(".cast/journal.displaced");
        fs::rename(&journal, &displaced).unwrap();
        fs::create_dir(&journal).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    });
    boot::reset_boot_synchronize_attempt_count();

    let error = synchronize(&fixture.client, &fixture.restored, authority).unwrap_err();

    assert!(matches!(
        error,
        Error::Authority(LegacyBootRepairAuthorityError::Journal(
            StorageError::JournalDirectoryBindingChanged
        ))
    ));
    assert_eq!(boot::boot_synchronize_attempt_count(), 1);
}

#[test]
fn legacy_worker_retains_the_exact_journal_lock_through_boot() {
    let fixture = Fixture::new();
    let authority = fixture.authorize();
    let installation = fixture.client.installation.clone();
    arm_before_worker(move || {
        let cast = installation.retained_mutable_cast_directory().unwrap();
        let competing = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap_err();
        assert!(matches!(competing, StorageError::AcquireLock { .. }), "{competing:?}");
    });
    boot::reset_boot_synchronize_attempt_count();

    synchronize(&fixture.client, &fixture.restored, authority).unwrap();

    assert_eq!(boot::boot_synchronize_attempt_count(), 1);
}

#[test]
fn legacy_authorization_rechecks_orphan_transition_ownership() {
    let fixture = Fixture::new();
    let transition = crate::state::TransitionId::parse("f".repeat(crate::state::TransitionId::TEXT_LENGTH)).unwrap();
    fixture
        .client
        .state_db
        .add_with_transition(&transition, &[], Some("legacy repair orphan"), None)
        .unwrap();

    let error = match fixture
        .identity
        .authorize_legacy_boot_repair(&fixture.client.installation, &fixture.client.state_db)
    {
        Ok(_) => panic!("orphan transition row unexpectedly authorized legacy boot repair"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        LegacyBootRepairAuthorityError::OrphanTransitionRow { .. }
    ));
}
