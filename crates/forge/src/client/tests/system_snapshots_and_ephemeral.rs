#[test]
fn state_creation_records_and_exports_the_generated_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let intent_path = system_model::intent_path(temporary.path());
    let intent_directory = intent_path.parent().unwrap();
    fs::create_dir_all(intent_directory).unwrap();
    fs::set_permissions(temporary.path().join("etc"), Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(intent_directory, Permissions::from_mode(0o755)).unwrap();
    fs::write(
        &intent_path,
        r#"// Authored intent must remain unchanged.
let cast = import! cast.system.v1
cast.system
"#,
    )
    .unwrap();
    fs::set_permissions(&intent_path, Permissions::from_mode(0o644)).unwrap();
    let authored = fs::read_to_string(&intent_path).unwrap();

    let client = stateful_test_client(temporary.path());
    fs::create_dir_all(client.installation.assets_path("v2")).unwrap();
    let authored_fingerprint = client
        .installation
        .system_model
        .as_ref()
        .unwrap()
        .fingerprint()
        .sha256
        .clone();

    let created = client.new_state(&[], "Gluon state creation").unwrap().unwrap();
    let snapshot_path = system_model::snapshot_path(temporary.path());
    let recorded = fs::read_to_string(&snapshot_path).unwrap();
    assert!(recorded.starts_with(system_model::spec::GENERATED_GLUON_MARKER));
    assert!(recorded.contains(&format!("// Authored source fingerprint: {authored_fingerprint}")));
    assert_eq!(fs::read_to_string(&intent_path).unwrap(), authored);

    drop(client);
    let reopened = stateful_test_client(temporary.path());
    assert_eq!(reopened.installation.active_state, Some(created.id));
    let exported = reopened.export_state(created.id).unwrap();

    assert_eq!(exported.encoded(), recorded);
    assert_eq!(exported.source_fingerprint(), Some(authored_fingerprint.as_str()));
    assert_eq!(fs::read_to_string(intent_path).unwrap(), authored);
}

#[test]
fn ephemeral_import_evaluates_intent_and_records_only_a_generated_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(temporary.path());
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("ephemeral-root");
    let intent_path = temporary.path().join("import.glu");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();
    prepare_private_installation_root(&blit_root);

    let authored = r#"// This authored source must never be copied into state.
let cast = import! cast.system.v1
{
packages = ["alpha"],
.. cast.system
}
"#;
    fs::write(&intent_path, authored).unwrap();

    let installation = test_installation(&installation_root);
    let client = Client::builder("ephemeral-import-test", installation)
        .system_intent_path(&intent_path)
        .ephemeral(&blit_root)
        .build()
        .unwrap();
    let imported = client.installation.system_model.as_ref().unwrap();

    assert!(client.is_ephemeral());
    assert_eq!(imported.authored_source(), authored);
    assert!(imported.packages.contains(&Provider::package_name("alpha")));

    let imported_fingerprint = imported.fingerprint().sha256.clone();
    record_system_snapshot(&blit_root, SystemModel::try_from(imported.clone()).unwrap()).unwrap();
    let snapshot_path = system_model::snapshot_path(&blit_root);
    let snapshot = fs::read_to_string(&snapshot_path).unwrap();
    let evaluated =
        system_model::gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", snapshot.clone())).unwrap();
    let loaded_snapshot = system_model::load(&snapshot_path).unwrap().unwrap();
    let round_trip = SystemModel::try_from(loaded_snapshot).unwrap();

    assert!(snapshot.starts_with(system_model::spec::GENERATED_GLUON_MARKER));
    assert!(snapshot.contains(&format!("// Authored source fingerprint: {imported_fingerprint}")));
    assert!(!snapshot.contains("This authored source must never be copied into state"));
    assert!(evaluated.packages.contains(&Provider::package_name("alpha")));
    assert_eq!(round_trip.encoded(), snapshot);
    assert_eq!(fs::read_to_string(intent_path).unwrap(), authored);
}

#[test]
fn ephemeral_blit_isolates_cached_asset_bytes_and_mode() {
    let temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(temporary.path());
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("ephemeral-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();
    prepare_private_installation_root(&blit_root);

    let installation = test_installation(&installation_root);
    let client = Client::builder("ephemeral-asset-isolation-test", installation)
        .repositories(repository::Map::default())
        .ephemeral(&blit_root)
        .build()
        .unwrap();

    let asset_id = xxhash_rust::xxh3::xxh3_128(b"persistent cached bytes");
    let asset_path = cache::asset_path(&client.installation, &format!("{asset_id:02x}"));
    fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
    fs::write(&asset_path, b"persistent cached bytes").unwrap();
    fs::set_permissions(&asset_path, Permissions::from_mode(0o640)).unwrap();

    let package = package::Id::from("ephemeral-asset-isolation-package");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(asset_id, "bin/cached-tool".into()),
            },
        )
        .unwrap();

    client.blit_root([&package]).unwrap();

    let materialized_path = blit_root.join("usr/bin/cached-tool");
    let cached_metadata = fs::metadata(&asset_path).unwrap();
    let materialized_metadata = fs::metadata(&materialized_path).unwrap();
    assert_ne!(
        (cached_metadata.dev(), cached_metadata.ino()),
        (materialized_metadata.dev(), materialized_metadata.ino())
    );
    assert_eq!(cached_metadata.permissions().mode() & 0o7777, 0o640);
    assert_eq!(materialized_metadata.permissions().mode() & 0o7777, 0o755);

    fs::write(&materialized_path, b"build-mutated bytes").unwrap();
    fs::set_permissions(&materialized_path, Permissions::from_mode(0o600)).unwrap();

    assert_eq!(fs::read(&asset_path).unwrap(), b"persistent cached bytes");
    assert_eq!(fs::metadata(&asset_path).unwrap().permissions().mode() & 0o7777, 0o640);
}
