use std::{
    ffi::CString,
    fs::Permissions,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use super::*;
use crate::{
    transition_identity::{
        ArchivedStatePruneError, ArchivedStatePruneFaultPoint, ArchivedStatePruneLimits,
        ArchivedStatePruneResidueError, MAX_ARCHIVED_STATE_PRUNE_BATCH, RetainedArchivedStatePrune,
        archived_state_prune_quarantine_name, arm_archived_state_prune_fault,
        arm_before_archived_state_prune_child_unlink, arm_before_archived_state_prune_wrapper_move,
    },
    tree_marker::TreeMarkerStore,
};

mod boot_projection;
mod startup_residue;

struct Fixture {
    _temporary: tempfile::TempDir,
    client: Client,
    archived: State,
    wrapper: PathBuf,
    marker_token: String,
}

struct MountGuard {
    targets: Vec<PathBuf>,
    armed: bool,
}

impl MountGuard {
    fn new(targets: Vec<PathBuf>) -> Self {
        Self { targets, armed: true }
    }

    fn unmount(&mut self) -> io::Result<()> {
        for target in &self.targets {
            let target = CString::new(target.as_os_str().as_bytes()).unwrap();
            // SAFETY: the path is a live NUL-terminated string. MNT_DETACH
            // makes cleanup independent of descriptors retained by the test.
            if unsafe { nix::libc::umount2(target.as_ptr(), nix::libc::MNT_DETACH) } == 0 {
                self.armed = false;
                return Ok(());
            }
            let source = io::Error::last_os_error();
            if !matches!(source.raw_os_error(), Some(nix::libc::EINVAL | nix::libc::ENOENT)) {
                return Err(source);
            }
        }
        Err(io::Error::other(
            "mounted prune fixture was not found at any retained location",
        ))
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for target in &self.targets {
            let Ok(target) = CString::new(target.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: best-effort panic cleanup uses a live NUL-terminated path.
            if unsafe { nix::libc::umount2(target.as_ptr(), nix::libc::MNT_DETACH) } == 0 {
                self.armed = false;
                break;
            }
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let archived = client.state_db.add(&[], Some("archived prune fixture"), None).unwrap();
        let wrapper = client.installation.root_path(archived.id.to_string());
        record_state_id(&wrapper, archived.id).unwrap();
        let store = TreeMarkerStore::open_path(wrapper.join("usr")).unwrap();
        let marker = store.adopt_or_create_before_journal().unwrap();
        let marker_token = marker.token().as_str().to_owned();
        Self {
            _temporary: temporary,
            client,
            archived,
            wrapper,
            marker_token,
        }
    }

    fn prepare(&self) -> RetainedArchivedStatePrune {
        RetainedArchivedStatePrune::prepare(
            &self.client.installation,
            &self.client.state_db,
            &[self.archived.clone()],
        )
        .unwrap()
    }

    fn prepare_with_limits(&self, limits: ArchivedStatePruneLimits) -> RetainedArchivedStatePrune {
        RetainedArchivedStatePrune::prepare_for_test(
            &self.client.installation,
            &self.client.state_db,
            &[self.archived.clone()],
            limits,
        )
        .unwrap()
    }

    fn add_file(&self, relative: impl AsRef<Path>, contents: &[u8]) {
        let path = self.wrapper.join("usr").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

fn add_archived_wrapper(client: &Client, summary: &str) -> (State, PathBuf, String) {
    add_archived_wrapper_with_selections(client, summary, &[])
}

fn add_archived_wrapper_with_selections(
    client: &Client,
    summary: &str,
    selections: &[Selection],
) -> (State, PathBuf, String) {
    let archived = client.state_db.add(selections, Some(summary), None).unwrap();
    let wrapper = client.installation.root_path(archived.id.to_string());
    record_state_id(&wrapper, archived.id).unwrap();
    let marker = TreeMarkerStore::open_path(wrapper.join("usr"))
        .unwrap()
        .adopt_or_create_before_journal()
        .unwrap();
    (archived, wrapper, marker.token().as_str().to_owned())
}

fn detach_and_remove_rows(fixture: &Fixture, prune: &mut RetainedArchivedStatePrune) -> PathBuf {
    let detached = prune
        .detach_all(&fixture.client.installation, &fixture.client.state_db)
        .unwrap();
    let quarantine = detached[0].quarantine.clone();
    prune
        .remove_database_rows(&fixture.client.installation, &fixture.client.state_db)
        .unwrap();
    assert!(fixture.client.state_db.get(fixture.archived.id).is_err());
    quarantine
}

fn expect_restart_prune_residue(root: &Path, residue: &Path) {
    let installation = Installation::open(root, None).unwrap();
    let source = match Client::new("archived-state-prune-restart-gate", installation) {
        Err(Error::SystemStartupGate { source }) => source,
        Err(other) => panic!("expected startup-gate prune residue error, got {other:?}"),
        Ok(_) => panic!("client startup unexpectedly accepted archived-state prune residue"),
    };
    let source = source
        .downcast::<startup_gate::Error>()
        .unwrap_or_else(|source| panic!("unexpected startup-gate source: {source}"));
    assert!(matches!(
        source.as_ref(),
        startup_gate::Error::ArchivedStatePruneResidue(ArchivedStatePruneResidueError::Residue { path })
            if path == residue
    ));
}

#[test]
fn fresh_exact_wrapper_is_detached_committed_and_deleted() {
    let fixture = Fixture::new();
    fixture.add_file("lib/data", b"retained payload");
    let mut prune = fixture.prepare();

    let quarantine = detach_and_remove_rows(&fixture, &mut prune);
    assert!(!fixture.wrapper.exists());
    assert!(quarantine.exists());

    prune.delete_detached(&fixture.client.installation).unwrap();
    assert!(!quarantine.exists());
    assert!(!quarantine.parent().unwrap().exists());
}

#[test]
fn authorized_two_link_state_slot_wrapper_is_pruned_exactly() {
    let fixture = Fixture::new();
    let marker = fixture.wrapper.join("usr/.cast-tree-id");
    let slot = fixture.wrapper.join(format!(
        ".cast-state-slot-{}-{}",
        fixture.archived.id, fixture.marker_token
    ));
    fs::hard_link(&marker, &slot).unwrap();
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 2);
    let mut prune = fixture.prepare();

    detach_and_remove_rows(&fixture, &mut prune);
    prune.delete_detached(&fixture.client.installation).unwrap();

    assert!(!fixture.wrapper.exists());
}

#[test]
fn deletion_batches_more_than_256_entries_without_retaining_one_fd_per_entry() {
    let fixture = Fixture::new();
    for index in 0..600 {
        fixture.add_file(format!("batch/entry-{index:04}"), b"bounded batching");
    }
    let mut prune = fixture.prepare();

    detach_and_remove_rows(&fixture, &mut prune);
    prune.delete_detached(&fixture.client.installation).unwrap();

    assert!(!fixture.wrapper.exists());
}

#[test]
fn empty_batch_is_rejected_before_any_reservation() {
    let fixture = Fixture::new();
    assert!(matches!(
        RetainedArchivedStatePrune::prepare(&fixture.client.installation, &fixture.client.state_db, &[]),
        Err(ArchivedStatePruneError::EmptyBatch)
    ));
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn oversized_batch_is_rejected_before_any_reservation_or_wrapper_open() {
    let fixture = Fixture::new();
    let oversized = vec![fixture.archived.clone(); MAX_ARCHIVED_STATE_PRUNE_BATCH + 1];

    assert!(matches!(
        RetainedArchivedStatePrune::prepare(&fixture.client.installation, &fixture.client.state_db, &oversized),
        Err(ArchivedStatePruneError::BatchTooLarge { actual, limit })
            if actual == MAX_ARCHIVED_STATE_PRUNE_BATCH + 1 && limit == MAX_ARCHIVED_STATE_PRUNE_BATCH
    ));
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn client_rejects_oversized_selection_before_loading_or_printing_snapshots() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let removal_ids = (0..=MAX_ARCHIVED_STATE_PRUNE_BATCH)
        .map(|_| client.state_db.add(&[], Some("unopened archive"), None).unwrap().id)
        .collect::<Vec<_>>();
    let active = client.state_db.add(&[], Some("active"), None).unwrap();
    client.installation.active_state = Some(active.id);
    record_state_id(&client.installation.root, active.id).unwrap();
    TreeMarkerStore::open_path(client.installation.root.join("usr"))
        .unwrap()
        .adopt_or_create_before_journal()
        .unwrap();

    assert!(matches!(
        client.prune_states(prune::Strategy::Remove(&removal_ids), true),
        Err(Error::Prune(prune::Error::PruneBatchTooLarge { actual, limit }))
            if actual == MAX_ARCHIVED_STATE_PRUNE_BATCH + 1 && limit == MAX_ARCHIVED_STATE_PRUNE_BATCH
    ));
    assert!(
        fs::read_dir(client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn applied_detach_fault_keeps_database_evidence_and_retries_suffix() {
    let fixture = Fixture::new();
    let mut prune = fixture.prepare();
    arm_archived_state_prune_fault(ArchivedStatePruneFaultPoint::AfterQuarantineMove);

    assert!(matches!(
        prune.detach_all(&fixture.client.installation, &fixture.client.state_db),
        Err(ArchivedStatePruneError::InjectedFault {
            point: ArchivedStatePruneFaultPoint::AfterQuarantineMove
        })
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
    assert!(!fixture.wrapper.exists());

    prune
        .detach_all(&fixture.client.installation, &fixture.client.state_db)
        .unwrap();
    prune.restore_all(&fixture.client.installation).unwrap();
    assert!(fixture.wrapper.exists());
}

#[test]
fn multi_state_partial_detach_restores_every_wrapper_then_retires_reservations() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let (first, first_wrapper, first_token) = add_archived_wrapper(&client, "first archived");
    let (second, second_wrapper, _) = add_archived_wrapper(&client, "second archived");
    let expected = [first.clone(), second.clone()];
    let mut prune = RetainedArchivedStatePrune::prepare(&client.installation, &client.state_db, &expected).unwrap();

    let first_slot = client.installation.state_quarantine_dir().join(
        archived_state_prune_quarantine_name(first.id, &first_token)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    );
    fs::rename(&first_wrapper, first_slot.join("wrapper")).unwrap();
    arm_archived_state_prune_fault(ArchivedStatePruneFaultPoint::AfterQuarantineMove);
    assert!(prune.detach_all(&client.installation, &client.state_db).is_err());
    assert!(!first_wrapper.exists());
    assert!(!second_wrapper.exists());
    assert_eq!(client.state_db.get(first.id).unwrap(), first);
    assert_eq!(client.state_db.get(second.id).unwrap(), second);

    prune.restore_all(&client.installation).unwrap();
    assert!(first_wrapper.exists());
    assert!(second_wrapper.exists());
    assert!(
        fs::read_dir(client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn preexisting_prune_residue_blocks_the_batch_before_its_first_reservation() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let (first, _, first_token) = add_archived_wrapper(&client, "first archived");
    let (second, _, second_token) = add_archived_wrapper(&client, "second archived");
    let first_slot = client.installation.state_quarantine_dir().join(
        archived_state_prune_quarantine_name(first.id, &first_token)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    );
    let collision = client.installation.state_quarantine_dir().join(
        archived_state_prune_quarantine_name(second.id, &second_token)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    );
    fs::create_dir(&collision).unwrap();
    fs::write(collision.join("foreign"), b"preserve").unwrap();

    assert!(matches!(
        RetainedArchivedStatePrune::prepare(&client.installation, &client.state_db, &[first, second]),
        Err(ArchivedStatePruneError::PruneResidue(
            ArchivedStatePruneResidueError::Residue { path }
        )) if path == collision
    ));
    assert!(!first_slot.exists());
    assert_eq!(fs::read(collision.join("foreign")).unwrap(), b"preserve");
}

#[test]
fn restored_phase_retries_a_partially_applied_reservation_unlink() {
    let fixture = Fixture::new();
    let mut prune = fixture.prepare();
    prune
        .detach_all(&fixture.client.installation, &fixture.client.state_db)
        .unwrap();
    arm_archived_state_prune_fault(ArchivedStatePruneFaultPoint::AfterPrivateDirectoryUnlink);

    assert!(prune.restore_all(&fixture.client.installation).is_err());
    assert!(fixture.wrapper.exists());
    let (first_operations, first_deadline) = prune.retirement_budget_usage_for_test().unwrap();
    prune.restore_all(&fixture.client.installation).unwrap();
    let (second_operations, second_deadline) = prune.retirement_budget_usage_for_test().unwrap();
    assert!(second_operations > first_operations);
    assert_eq!(second_deadline, first_deadline);
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn canonical_wrapper_substitution_before_move_is_never_adopted() {
    let fixture = Fixture::new();
    let mut prune = fixture.prepare();
    let canonical = fixture.wrapper.clone();
    let quarantine_wrapper = fixture
        .client
        .installation
        .state_quarantine_dir()
        .join(
            archived_state_prune_quarantine_name(fixture.archived.id, &fixture.marker_token)
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        )
        .join("wrapper");
    let exact_saved = fixture.client.installation.root_path("exact-saved-by-test");
    let hook_canonical = canonical.clone();
    let hook_saved = exact_saved.clone();
    arm_before_archived_state_prune_wrapper_move(move || {
        fs::rename(&hook_canonical, &hook_saved).unwrap();
        fs::create_dir(&hook_canonical).unwrap();
        fs::write(hook_canonical.join("foreign"), b"do not move").unwrap();
    });

    assert!(matches!(
        prune.detach_all(&fixture.client.installation, &fixture.client.state_db),
        Err(ArchivedStatePruneError::AmbiguousLayout { .. } | ArchivedStatePruneError::WrapperLayoutChanged { .. })
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
    assert!(!canonical.exists());
    assert_eq!(fs::read(quarantine_wrapper.join("foreign")).unwrap(), b"do not move");
    assert!(exact_saved.join("usr/.cast-tree-id").exists());
}

#[test]
fn foreign_quarantine_wrapper_occupant_is_preserved_without_moving_canonical() {
    let fixture = Fixture::new();
    let mut prune = fixture.prepare();
    let slot = fixture.client.installation.state_quarantine_dir().join(
        archived_state_prune_quarantine_name(fixture.archived.id, &fixture.marker_token)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    );
    let foreign = slot.join("wrapper");
    let hook_foreign = foreign.clone();
    arm_before_archived_state_prune_wrapper_move(move || {
        fs::create_dir(&hook_foreign).unwrap();
        fs::write(hook_foreign.join("foreign"), b"preserve quarantine occupant").unwrap();
    });

    assert!(
        prune
            .detach_all(&fixture.client.installation, &fixture.client.state_db)
            .is_err()
    );
    assert!(fixture.wrapper.join("usr/.cast-tree-id").exists());
    assert_eq!(
        fs::read(foreign.join("foreign")).unwrap(),
        b"preserve quarantine occupant"
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
}

#[test]
fn deterministic_quarantine_residue_is_preserved_and_rejected() {
    let fixture = Fixture::new();
    let name = archived_state_prune_quarantine_name(fixture.archived.id, &fixture.marker_token).unwrap();
    let collision = fixture
        .client
        .installation
        .state_quarantine_dir()
        .join(name.to_string_lossy().as_ref());
    fs::create_dir(&collision).unwrap();
    fs::write(collision.join("foreign"), b"keep this evidence").unwrap();

    assert!(matches!(
        fixture.prepare_result(),
        Err(ArchivedStatePruneError::PruneResidue(
            ArchivedStatePruneResidueError::Residue { path }
        )) if path == collision
    ));
    assert_eq!(fs::read(collision.join("foreign")).unwrap(), b"keep this evidence");
}

#[test]
fn metadata_corruption_is_rejected_without_reservation() {
    let missing_state_id = Fixture::new();
    fs::remove_file(missing_state_id.wrapper.join("usr/.stateID")).unwrap();
    assert!(missing_state_id.prepare_result().is_err());

    let missing_marker = Fixture::new();
    fs::remove_file(missing_marker.wrapper.join("usr/.cast-tree-id")).unwrap();
    assert!(missing_marker.prepare_result().is_err());
}

#[test]
fn phase_api_forbids_delete_before_database_and_restore_after_database() {
    let fixture = Fixture::new();
    let mut prune = fixture.prepare();
    assert!(matches!(
        prune.delete_detached(&fixture.client.installation),
        Err(ArchivedStatePruneError::InvalidPhase { .. })
    ));
    detach_and_remove_rows(&fixture, &mut prune);
    assert!(matches!(
        prune.restore_all(&fixture.client.installation),
        Err(ArchivedStatePruneError::InvalidPhase { .. })
    ));
    prune.delete_detached(&fixture.client.installation).unwrap();
}

#[test]
fn mode_zero_directories_and_symlinks_are_deleted_without_following_targets() {
    let fixture = Fixture::new();
    fixture.add_file("locked/nested/data", b"mode zero payload");
    fs::set_permissions(fixture.wrapper.join("usr/locked"), Permissions::from_mode(0o000)).unwrap();
    let outside = fixture._temporary.path().join("outside-sentinel");
    fs::write(&outside, b"outside remains").unwrap();
    symlink(&outside, fixture.wrapper.join("usr/outside-link")).unwrap();
    let mut prune = fixture.prepare();

    detach_and_remove_rows(&fixture, &mut prune);
    prune.delete_detached(&fixture.client.installation).unwrap();
    assert_eq!(fs::read(outside).unwrap(), b"outside remains");
}

#[test]
fn child_substitution_in_final_check_syscall_window_preserves_foreign_entry() {
    let fixture = Fixture::new();
    fixture.add_file("payload", b"exact tree");
    let outside = fixture._temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside remains").unwrap();
    let exact_saved = fixture._temporary.path().join("exact-saved");
    let mut prune = fixture.prepare();
    let quarantine = detach_and_remove_rows(&fixture, &mut prune);
    let quarantined_usr = quarantine.join("usr");
    let hook_usr = quarantined_usr.clone();
    let hook_saved = exact_saved.clone();
    let hook_outside = outside.clone();
    arm_before_archived_state_prune_child_unlink(move || {
        fs::rename(&hook_usr, &hook_saved).unwrap();
        symlink(&hook_outside, &hook_usr).unwrap();
    });

    assert!(matches!(
        prune.delete_detached(&fixture.client.installation),
        Err(ArchivedStatePruneError::EntryChanged { .. })
    ));
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside remains");
    assert_eq!(fs::read(exact_saved.join("payload")).unwrap(), b"exact tree");
    assert!(!quarantined_usr.exists());
    let private_entries = fs::read_dir(quarantine.parent().unwrap().join("delete"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(private_entries.len(), 1);
    assert!(
        fs::symlink_metadata(private_entries[0].path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn partial_private_unlink_and_sync_faults_retry_in_process() {
    let fixture = Fixture::new();
    fixture.add_file("payload", b"retry me");
    let mut prune = fixture.prepare();
    let quarantine = detach_and_remove_rows(&fixture, &mut prune);

    arm_archived_state_prune_fault(ArchivedStatePruneFaultPoint::AfterChildUnlink);
    assert!(prune.delete_detached(&fixture.client.installation).is_err());
    assert!(quarantine.parent().unwrap().exists());
    let (first_operations, first_deadline) = prune.delete_budget_usage_for_test().unwrap();

    arm_archived_state_prune_fault(ArchivedStatePruneFaultPoint::BeforeChangedParentSync);
    assert!(prune.delete_detached(&fixture.client.installation).is_err());
    let (second_operations, second_deadline) = prune.delete_budget_usage_for_test().unwrap();
    assert!(second_operations >= first_operations);
    assert_eq!(second_deadline, first_deadline);
    prune.delete_detached(&fixture.client.installation).unwrap();
    let (final_operations, final_deadline) = prune.delete_budget_usage_for_test().unwrap();
    assert!(final_operations > second_operations);
    assert_eq!(final_deadline, first_deadline);
    assert!(!quarantine.parent().unwrap().exists());
}

#[test]
fn aggregate_delete_boundaries_fail_closed() {
    let cases: [(&str, Box<dyn Fn(&mut ArchivedStatePruneLimits)>); 6] = [
        ("depth", Box::new(|limits| limits.depth = 0)),
        ("entries", Box::new(|limits| limits.entries = 0)),
        ("name-bytes", Box::new(|limits| limits.name_bytes = 0)),
        ("operations", Box::new(|limits| limits.operations = 0)),
        ("retained-nodes", Box::new(|limits| limits.retained_nodes = 0)),
        ("deadline", Box::new(|limits| limits.time = Duration::ZERO)),
    ];

    for (label, configure) in cases {
        let fixture = Fixture::new();
        fixture.add_file("directory/payload", b"bounded");
        let mut limits = ArchivedStatePruneLimits::default();
        configure(&mut limits);
        let mut prune = fixture.prepare_with_limits(limits);
        let quarantine = detach_and_remove_rows(&fixture, &mut prune);
        assert!(prune.delete_detached(&fixture.client.installation).is_err(), "{label}");
        assert!(quarantine.parent().unwrap().exists(), "{label}");
    }
}

#[test]
fn mounted_descendant_is_rejected_or_test_is_skipped_only_for_unavailable_mounting() {
    let fixture = Fixture::new();
    let mountpoint = fixture.wrapper.join("usr/mounted");
    fs::create_dir(&mountpoint).unwrap();
    let target = CString::new(mountpoint.as_os_str().as_bytes()).unwrap();
    let filesystem = c"tmpfs";
    // SAFETY: all pointers are live NUL-terminated strings for this syscall.
    let mounted = unsafe {
        nix::libc::mount(
            filesystem.as_ptr(),
            target.as_ptr(),
            filesystem.as_ptr(),
            nix::libc::MS_NODEV | nix::libc::MS_NOSUID | nix::libc::MS_NOEXEC,
            std::ptr::null(),
        )
    };
    if mounted != 0 {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(code) if matches!(code, nix::libc::EPERM | nix::libc::EACCES | nix::libc::ENOSYS)
        ) {
            return;
        }
        panic!("unexpected mount failure: {source}");
    }

    let slot_name = archived_state_prune_quarantine_name(fixture.archived.id, &fixture.marker_token).unwrap();
    let predicted_moved_mountpoint = fixture
        .client
        .installation
        .state_quarantine_dir()
        .join(slot_name.to_string_lossy().as_ref())
        .join("wrapper/usr/mounted");
    let mut mount_guard = MountGuard::new(vec![mountpoint, predicted_moved_mountpoint]);

    let mut prune = fixture.prepare();
    let quarantine = prune
        .detach_all(&fixture.client.installation, &fixture.client.state_db)
        .unwrap()[0]
        .quarantine
        .clone();
    let moved_mountpoint = quarantine.join("usr/mounted");
    prune
        .remove_database_rows(&fixture.client.installation, &fixture.client.state_db)
        .unwrap();
    assert!(matches!(
        prune.delete_detached(&fixture.client.installation),
        Err(ArchivedStatePruneError::MountedEntry { .. })
    ));
    assert_eq!(moved_mountpoint, mount_guard.targets[1]);
    mount_guard.unmount().unwrap();
}

impl Fixture {
    fn prepare_result(&self) -> Result<RetainedArchivedStatePrune, ArchivedStatePruneError> {
        RetainedArchivedStatePrune::prepare(
            &self.client.installation,
            &self.client.state_db,
            &[self.archived.clone()],
        )
    }
}
