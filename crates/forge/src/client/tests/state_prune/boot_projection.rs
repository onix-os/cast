use super::*;

struct ContentfulPruneFixture {
    _temporary: tempfile::TempDir,
    client: Client,
    archived: State,
    active: State,
    wrapper: PathBuf,
    stale_entry: PathBuf,
    stale_contents: Vec<u8>,
    removed_package: package::Id,
    removed_asset: PathBuf,
}

impl ContentfulPruneFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
        let removed_package = package::Id::from("contentful-removed-package");
        let (archived, wrapper, _) = add_archived_wrapper_with_selections(
            &client,
            "production archived",
            &[Selection::explicit(removed_package.clone())],
        );
        let removed_digest = 0xdead_beefu128;
        client
            .layout_db
            .add(
                &removed_package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o644,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(removed_digest, "share/removed/payload".into()),
                },
            )
            .unwrap();
        let removed_asset = cache::asset_path(&client.installation, &format!("{removed_digest:02x}"));
        fs::create_dir_all(removed_asset.parent().unwrap()).unwrap();
        fs::write(&removed_asset, b"unreferenced CAS payload").unwrap();

        let boot_package = package::Id::from("contentful-boot-package");
        let boot_layouts = [
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "lib/kernel/6.1.0/vmlinuz".into()),
            },
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(2, "lib/systemd/boot/efi/systemd-bootx64.efi".into()),
            },
        ];
        client
            .layout_db
            .batch_add(boot_layouts.iter().map(|layout| (&boot_package, layout)))
            .unwrap();
        let active = client
            .state_db
            .add(&[Selection::explicit(boot_package)], Some("active"), None)
            .unwrap();
        client.installation.active_state = Some(active.id);
        record_state_id(&client.installation.root, active.id).unwrap();
        TreeMarkerStore::open_path(client.installation.root.join("usr"))
            .unwrap()
            .adopt_or_create_before_journal()
            .unwrap();
        fs::create_dir_all(client.installation.root.join("usr/lib/kernel/6.1.0")).unwrap();
        fs::write(
            client.installation.root.join("usr/lib/kernel/6.1.0/vmlinuz"),
            b"contentful kernel",
        )
        .unwrap();
        fs::create_dir_all(client.installation.root.join("usr/lib/systemd/boot/efi")).unwrap();
        fs::write(
            client
                .installation
                .root
                .join("usr/lib/systemd/boot/efi/systemd-bootx64.efi"),
            b"contentful bootloader",
        )
        .unwrap();
        fs::write(
            client.installation.root.join("usr/lib/os-release"),
            b"NAME=AerynOS\nID=aerynos\nVERSION_ID=1\n",
        )
        .unwrap();
        let stale_entry = client.installation.root.join("boot/loader/entries/aerynos-stale.conf");
        let stale_contents = format!("options cast.fstx={}\n", archived.id).into_bytes();
        fs::create_dir_all(stale_entry.parent().unwrap()).unwrap();
        fs::write(&stale_entry, &stale_contents).unwrap();

        Self {
            _temporary: temporary,
            client,
            archived,
            active,
            wrapper,
            stale_entry,
            stale_contents,
            removed_package,
            removed_asset,
        }
    }

    fn arm_post_prune_projection(&self) {
        let database = self.client.state_db.clone();
        let layout_database = self.client.layout_db.clone();
        let wrapper = self.wrapper.clone();
        let stale_entry = self.stale_entry.clone();
        let removed_asset = self.removed_asset.clone();
        let removed_package = self.removed_package.clone();
        let archived_id = self.archived.id;
        let active_id = self.active.id;
        boot::arm_boot_projection_sync(move |projected| {
            assert_eq!(projected, [active_id]);
            assert!(database.get(archived_id).is_ok());
            assert!(!wrapper.exists());
            assert!(!layout_database.query([&removed_package]).unwrap().is_empty());
            assert!(removed_asset.exists());
            fs::remove_file(&stale_entry).unwrap();
        });
    }
}

fn fail_active_state_revalidation_at(call: usize, state_id_path: PathBuf) {
    assert!(call > 0);
    for _ in 1..call {
        active_state_snapshot::arm_before_active_state_revalidation(|| {});
    }
    active_state_snapshot::arm_before_active_state_revalidation(move || {
        fs::write(state_id_path, b"2147483647").unwrap();
    });
}

#[test]
fn production_client_prune_completes_detach_boot_db_delete_and_gc_order() {
    let fixture = ContentfulPruneFixture::new();
    fixture.arm_post_prune_projection();

    fixture
        .client
        .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
        .unwrap();

    assert!(fixture.client.state_db.get(fixture.archived.id).is_err());
    assert_eq!(fixture.client.state_db.get(fixture.active.id).unwrap(), fixture.active);
    assert!(
        fixture
            .client
            .layout_db
            .query([&fixture.removed_package])
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.removed_asset.exists());
    assert!(!fixture.wrapper.exists());
    assert!(!fixture.stale_entry.exists());
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn definitely_not_applied_boot_fault_restores_archives_without_touching_boot() {
    let fixture = ContentfulPruneFixture::new();
    boot::arm_projection_sync_fault(boot::ProjectionSyncFaultPoint::BeforeSideEffects);

    assert!(
        fixture
            .client
            .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
            .is_err()
    );

    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
    assert!(fixture.wrapper.exists());
    assert_eq!(fs::read(&fixture.stale_entry).unwrap(), fixture.stale_contents);
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn active_state_failure_after_prepare_or_detach_restores_archives_before_boot() {
    // Revalidation 1 precedes preparation. Revalidation 2 follows private
    // reservation creation, and revalidation 3 follows wrapper detachment.
    // Both latter windows must compensate every namespace change before
    // returning the active-state proof failure.
    for failed_call in [2, 3] {
        let fixture = ContentfulPruneFixture::new();
        fail_active_state_revalidation_at(failed_call, fixture.client.installation.root.join("usr/.stateID"));

        assert!(
            fixture
                .client
                .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
                .is_err()
        );

        assert_eq!(
            fixture.client.state_db.get(fixture.archived.id).unwrap(),
            fixture.archived
        );
        assert!(fixture.wrapper.exists());
        assert_eq!(fs::read(&fixture.stale_entry).unwrap(), fixture.stale_contents);
        assert!(
            fs::read_dir(fixture.client.installation.state_quarantine_dir())
                .unwrap()
                .next()
                .is_none(),
            "active-state failure at revalidation {failed_call} left prune residue"
        );
    }
}

#[test]
fn active_state_failure_after_boot_restores_prior_projection_before_retiring_reservations() {
    let fixture = ContentfulPruneFixture::new();
    fixture.arm_post_prune_projection();
    let prior_stale_entry = fixture.stale_entry.clone();
    let prior_stale_contents = fixture.stale_contents.clone();
    let restored_wrapper = fixture.wrapper.clone();
    let restored_database = fixture.client.state_db.clone();
    let archived_id = fixture.archived.id;
    let active_id = fixture.active.id;
    boot::arm_boot_projection_sync(move |projected| {
        assert_eq!(projected, [active_id]);
        assert!(restored_wrapper.exists());
        assert!(restored_database.get(archived_id).is_ok());
        fs::write(prior_stale_entry, prior_stale_contents).unwrap();
    });
    fail_active_state_revalidation_at(4, fixture.client.installation.root.join("usr/.stateID"));

    assert!(
        fixture
            .client
            .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
            .is_err()
    );

    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
    assert!(fixture.wrapper.exists());
    assert_eq!(fs::read(&fixture.stale_entry).unwrap(), fixture.stale_contents);
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn ambiguous_post_projection_is_compensated_before_reservations_are_retired() {
    let fixture = ContentfulPruneFixture::new();
    fixture.arm_post_prune_projection();
    let prior_stale_entry = fixture.stale_entry.clone();
    let prior_stale_contents = fixture.stale_contents.clone();
    let restored_wrapper = fixture.wrapper.clone();
    let restored_database = fixture.client.state_db.clone();
    let archived_id = fixture.archived.id;
    let active_id = fixture.active.id;
    boot::arm_boot_projection_sync(move |projected| {
        assert_eq!(projected, [active_id]);
        assert!(restored_wrapper.exists());
        assert!(restored_database.get(archived_id).is_ok());
        fs::write(prior_stale_entry, prior_stale_contents).unwrap();
    });
    boot::arm_projection_sync_fault(boot::ProjectionSyncFaultPoint::AfterSideEffects);

    assert!(
        fixture
            .client
            .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
            .is_err()
    );

    assert_eq!(
        fixture.client.state_db.get(fixture.archived.id).unwrap(),
        fixture.archived
    );
    assert!(fixture.wrapper.exists());
    assert_eq!(fs::read(&fixture.stale_entry).unwrap(), fixture.stale_contents);
    assert!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn failed_ambiguous_boot_compensation_leaves_restart_blocking_residue() {
    let fixture = ContentfulPruneFixture::new();
    fixture.arm_post_prune_projection();
    let prior_stale_entry = fixture.stale_entry.clone();
    let prior_stale_contents = fixture.stale_contents.clone();
    boot::arm_boot_projection_sync(move |_| {
        fs::write(prior_stale_entry, prior_stale_contents).unwrap();
        boot::arm_projection_sync_fault(boot::ProjectionSyncFaultPoint::AfterSideEffects);
    });
    boot::arm_projection_sync_fault(boot::ProjectionSyncFaultPoint::AfterSideEffects);

    assert!(
        fixture
            .client
            .prune_states(prune::Strategy::Remove(&[fixture.archived.id]), true)
            .is_err()
    );
    let residue = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fixture.wrapper.exists());
    assert!(residue.is_dir());
    let root = fixture.client.installation.root.clone();
    drop(fixture.client);

    expect_restart_prune_residue(&root, &residue);
    assert!(residue.is_dir());
}
