use std::{fs, os::unix::fs::PermissionsExt as _};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::{
    Installation, package, repository,
    state::{Selection, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        BootId, MountNamespaceIdentity, Operation, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
        RuntimeTreeIdentity, StorageError, TransitionJournalStore, TransitionRecord, TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::super::{Client, Error, boot, record_state_id};
use super::{arm_before_final_journal_revalidation, arm_before_post_revalidation};

const TRANSITION_ID: &str = "0123456789abcdef0123456789abcdef";

struct Fixture {
    _temporary: tempfile::TempDir,
    client: Client,
}

impl Fixture {
    fn new(with_bootloader_layout: bool) -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let mut client = Client::builder("clean-boot-synchronization-test", installation)
            .repositories(repository::Map::default())
            .build()
            .unwrap();

        let package = package::Id::from("clean-boot-synchronization-package");
        let selections = if with_bootloader_layout {
            client
                .layout_db
                .add(
                    &package,
                    &StonePayloadLayoutRecord {
                        uid: 0,
                        gid: 0,
                        mode: nix::libc::S_IFREG | 0o644,
                        tag: 0,
                        file: StonePayloadLayoutFile::Regular(1, "lib/systemd/boot/efi/systemd-bootx64.efi".into()),
                    },
                )
                .unwrap();
            vec![Selection::explicit(package)]
        } else {
            Vec::new()
        };
        let state = client
            .state_db
            .add(&selections, Some("clean boot synchronization"), None)
            .unwrap();
        record_state_id(&client.installation.root, state.id).unwrap();
        TreeMarkerStore::open_path(client.installation.root.join("usr"))
            .unwrap()
            .adopt_or_create_before_journal()
            .unwrap();
        client.installation.active_state = Some(state.id);

        Self {
            _temporary: temporary,
            client,
        }
    }

    fn journal(&self) -> TransitionJournalStore {
        let cast = self.client.installation.retained_mutable_cast_directory().unwrap();
        TransitionJournalStore::open_in_retained_cast(cast, &self.client.installation.root).unwrap()
    }
}

fn transition_id() -> TransitionId {
    TransitionId::parse(TRANSITION_ID).unwrap()
}

fn creation_record() -> TransitionRecord {
    TransitionRecord::preparing(
        transition_id(),
        RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
            mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
        },
        Operation::NewState,
        None,
        TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 10,
            mount_id: 12,
        },
        Previous {
            id: Some(41),
            tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            usr_runtime_identity: RuntimeTreeIdentity {
                st_dev: 10,
                inode: 20,
                mount_id: 12,
            },
            origin: PreviousOrigin::ActiveState,
        },
        true,
        true,
        QuarantineName::parse("clean-boot-synchronization-test").unwrap(),
    )
    .unwrap()
}

fn assert_authority_error(result: Result<(), Error>, expected: &str) {
    let Err(Error::BootSynchronizationAuthority { source }) = result else {
        panic!("expected standalone boot-synchronization authority rejection");
    };
    assert!(
        source.to_string().contains(expected),
        "unexpected authority error: {source}"
    );
}

#[test]
fn clean_standalone_boot_synchronization_retains_authority_through_one_worker_attempt() {
    let fixture = Fixture::new(false);
    let installation = fixture.client.installation.clone();
    arm_before_post_revalidation(move |_| {
        let cast = installation.retained_mutable_cast_directory().unwrap();
        let competing = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap_err();
        assert!(matches!(competing, StorageError::AcquireLock { .. }), "{competing:?}");
    });
    boot::reset_boot_synchronize_attempt_count();

    fixture.client.synchronize_boot().unwrap();

    assert_eq!(boot::boot_synchronize_attempt_count(), 1);
    assert_eq!(fixture.journal().load().unwrap(), None);
}

#[test]
fn final_public_journal_binding_rejects_replacement_after_the_leading_check() {
    let fixture = Fixture::new(false);
    let root = fixture.client.installation.root.clone();
    arm_before_post_revalidation(move |_| {
        arm_before_final_journal_revalidation(move || {
            let journal = root.join(".cast/journal");
            let displaced = root.join(".cast/journal.displaced");
            fs::rename(&journal, &displaced).unwrap();
            fs::create_dir(&journal).unwrap();
            fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
        });
    });
    boot::reset_boot_synchronize_attempt_count();

    assert_authority_error(
        fixture.client.synchronize_boot(),
        "open, bind, or load the canonical transition journal",
    );

    assert_eq!(boot::boot_synchronize_attempt_count(), 1);
}

#[test]
fn unresolved_journal_blocks_standalone_boot_before_the_worker() {
    let fixture = Fixture::new(false);
    let expected = creation_record();
    let journal = fixture.journal();
    journal.create(&expected).unwrap();
    drop(journal);
    boot::reset_boot_synchronize_attempt_count();

    assert_authority_error(fixture.client.synchronize_boot(), "unresolved transition");

    assert_eq!(boot::boot_synchronize_attempt_count(), 0);
    assert_eq!(fixture.journal().load().unwrap(), Some(expected));
}

#[test]
fn orphan_transition_row_blocks_standalone_boot_before_the_worker() {
    let fixture = Fixture::new(false);
    fixture
        .client
        .state_db
        .add_with_transition(&transition_id(), &[], Some("orphan"), None)
        .unwrap();
    boot::reset_boot_synchronize_attempt_count();

    assert_authority_error(fixture.client.synchronize_boot(), "orphan transition");

    assert_eq!(boot::boot_synchronize_attempt_count(), 0);
    assert_eq!(fixture.journal().load().unwrap(), None);
}

#[test]
fn post_authority_failure_supersedes_the_boot_backend_error() {
    let fixture = Fixture::new(true);
    let expected = creation_record();
    let injected = expected.clone();
    arm_before_post_revalidation(move |journal| journal.create(&injected).unwrap());
    boot::reset_boot_synchronize_attempt_count();

    // The bootloader layout reaches os-release discovery, which deliberately
    // fails in this fixture. The injected journal record must still select the
    // post-authority error rather than that simultaneous backend error.
    assert_authority_error(fixture.client.synchronize_boot(), "unresolved transition");

    assert_eq!(boot::boot_synchronize_attempt_count(), 1);
    assert_eq!(fixture.journal().load().unwrap(), Some(expected));
}
