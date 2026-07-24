use super::*;

#[test]
fn direct_file_refresh_publishes_and_uses_one_verified_immutable_snapshot() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("stone.index");
    let bytes = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]);
    fs::write(&source_path, &bytes).unwrap();
    let index_uri = Url::from_file_path(&source_path).unwrap();
    let (_root, installation) = test_installation();
    let (id, manager) = explicit_manager("direct-file", direct_repository(index_uri.clone()), installation);

    runtime::block_on(manager.refresh(&id)).unwrap();

    let state = manager.repositories.get(&id).unwrap();
    let snapshot = verified_active_snapshot(state).unwrap();
    let expected = index_identity(&bytes);
    assert_eq!(snapshot.index_uri(), &index_uri);
    assert_eq!(snapshot.sha256(), expected.sha256);
    assert_eq!(snapshot.byte_size(), expected.byte_size);

    let immutable = immutable_index_path(state, snapshot.sha256());
    assert_eq!(fs::read(&immutable).unwrap(), bytes);
    assert_eq!(fs::metadata(&immutable).unwrap().permissions().mode() & 0o222, 0);
    assert!(!state.cache_dir.join("stone.index").exists());
    assert!(!state.cache_dir.join("index-uri").exists());

    let exported = manager.index_snapshots().unwrap();
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].index_uri, index_uri);
    assert_eq!(exported[0].sha256, snapshot.sha256());
    assert_eq!(exported[0].byte_size, snapshot.byte_size());

    let package = manager
        .resolve_exact_package(&package::Id::from("a".repeat(64)))
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        package.meta.uri,
        Some(
            Url::from_file_path(source.path().join("package-a.stone"))
                .unwrap()
                .to_string()
        )
    );
}

#[test]
fn downloads_use_distinct_private_candidates_without_creating_active_state() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("candidate.index");
    fs::write(&source_path, b"candidate bytes").unwrap();
    let (_root, installation) = test_installation();
    let (id, manager) = explicit_manager(
        "private-candidates",
        direct_repository(Url::from_file_path(&source_path).unwrap()),
        installation,
    );
    let state = manager.repositories.get(&id).unwrap();

    let mutation = RepositoryMutationLock::acquire(state).unwrap();
    let first = runtime::block_on(fetch_index(&manager.source, state, &mutation.cache_directory)).unwrap();
    let second = runtime::block_on(fetch_index(&manager.source, state, &mutation.cache_directory)).unwrap();
    assert_ne!(first.path, second.path);
    for candidate in [&first, &second] {
        assert_eq!(
            fs::metadata(candidate._directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(&candidate.path).unwrap(), b"candidate bytes");
    }
    assert_eq!(state.db.active_snapshot().unwrap(), None);
    assert!(!state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY).exists());
}

#[test]
fn root_file_source_ignores_legacy_sidecars_and_initializes_from_db_snapshot() {
    let source = tempfile::tempdir().unwrap();
    let history_dir = source.path().join("main/history/1/x86_64");
    fs::create_dir_all(&history_dir).unwrap();
    let bytes = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('b', "package-b.stone")]);
    fs::write(history_dir.join("stone.index"), &bytes).unwrap();
    fs::create_dir_all(source.path().join("main")).unwrap();
    fs::write(
        source.path().join("main").join(repository::ROOT_INDEX_WIRE_FILENAME),
        r#"{
  "formats": { "v0": {} },
  "streams": { "unstable": { "format": "v0", "history": "1" } },
  "tags": {},
  "history": { "1": { "format": "v0" } }
}"#,
    )
    .unwrap();

    let repository = Repository {
        description: "root test".to_owned(),
        source: repository::Source::RootIndex(repository::RootIndexSource {
            base_uri: Url::from_directory_path(source.path()).unwrap(),
            channel: "main".try_into().unwrap(),
            version: "stream/unstable".parse().unwrap(),
            arch: "x86_64".to_owned(),
        }),
        priority: repository::Priority::new(0),
        active: true,
    };
    let identifier = "root-file";
    let (_root, installation) = test_installation();
    let legacy_cache = cache_dir(identifier, &repository::Id::new("test"), &repository, &installation);
    fs::create_dir_all(&legacy_cache).unwrap();
    fs::set_permissions(&legacy_cache, std::fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(legacy_cache.join("stone.index"), b"legacy mutable cache").unwrap();
    fs::write(legacy_cache.join("index-uri"), b"not even a URL").unwrap();

    let (id, mut manager) = explicit_manager(identifier, repository, installation);
    assert!(matches!(
        manager.index_snapshots(),
        Err(Error::MissingActiveSnapshot(missing)) if missing == id
    ));

    assert_eq!(runtime::block_on(manager.ensure_all_initialized()).unwrap(), 1);
    assert_eq!(
        fs::read(legacy_cache.join("stone.index")).unwrap(),
        b"legacy mutable cache"
    );
    assert_eq!(fs::read(legacy_cache.join("index-uri")).unwrap(), b"not even a URL");

    let state = manager.repositories.get(&id).unwrap();
    let snapshot = verified_active_snapshot(state).unwrap();
    assert_eq!(
        snapshot.index_uri(),
        &Url::from_file_path(history_dir.join("stone.index")).unwrap()
    );
    assert!(state.db.get(&package::Id::from("b".repeat(64))).is_ok());
}

#[test]
fn refresh_failure_missing_and_corrupt_active_files_fail_closed_without_losing_snapshot() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("stone.index");
    let valid = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('c', "package-c.stone")]);
    fs::write(&source_path, &valid).unwrap();
    let (_root, installation) = test_installation();
    let (id, mut manager) = explicit_manager(
        "failure-preservation",
        direct_repository(Url::from_file_path(&source_path).unwrap()),
        installation,
    );
    runtime::block_on(manager.refresh(&id)).unwrap();

    let state = manager.repositories.get(&id).unwrap().clone();
    let old_snapshot = state.db.active_snapshot().unwrap().unwrap();
    let immutable = immutable_index_path(&state, old_snapshot.sha256());
    let old_bytes = fs::read(&immutable).unwrap();

    fs::write(&source_path, layout_index()).unwrap();
    assert!(matches!(
        runtime::block_on(manager.refresh(&id)),
        Err(Error::UnexpectedIndexPayload { .. })
    ));
    assert_eq!(state.db.active_snapshot().unwrap(), Some(old_snapshot.clone()));
    assert_eq!(fs::read(&immutable).unwrap(), old_bytes);
    assert!(state.db.get(&package::Id::from("c".repeat(64))).is_ok());

    fs::remove_file(&immutable).unwrap();
    assert!(manager.index_snapshots().is_err());
    assert!(
        manager
            .resolve_exact_package(&package::Id::from("c".repeat(64)))
            .is_err()
    );
    fs::write(&source_path, &valid).unwrap();
    assert_eq!(runtime::block_on(manager.ensure_all_initialized()).unwrap(), 1);
    assert_eq!(verified_active_snapshot(&state).unwrap(), old_snapshot);
    let registry_repository = crate::registry::plugin::Repository::new(state.clone());
    assert!(
        registry_repository
            .package(&package::Id::from("c".repeat(64)))
            .unwrap()
            .is_some()
    );

    fs::set_permissions(&immutable, std::fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&immutable, b"corrupt immutable index").unwrap();
    let corrupt = fs::read(&immutable).unwrap();
    let error = manager.index_snapshots().unwrap_err();
    assert!(
        matches!(
            error,
            Error::IndexSizeMismatch { .. } | Error::IndexMetadataPolicy { .. } | Error::IndexChanged(_)
        ),
        "{error:?}"
    );
    assert!(
        manager
            .resolve_exact_package(&package::Id::from("c".repeat(64)))
            .is_err()
    );
    assert!(registry_repository.package(&package::Id::from("c".repeat(64))).is_err());
    assert!(runtime::block_on(manager.ensure_all_initialized()).is_err());
    assert_eq!(fs::read(&immutable).unwrap(), corrupt);
    assert_eq!(state.db.active_snapshot().unwrap(), Some(old_snapshot));
}

#[test]
fn concurrent_refreshes_converge_on_one_no_replace_content_address() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("stone.index");
    fs::write(
        &source_path,
        meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('d', "package-d.stone")]),
    )
    .unwrap();
    let (_root, installation) = test_installation();
    let (id, manager) = explicit_manager(
        "concurrent-refresh",
        direct_repository(Url::from_file_path(&source_path).unwrap()),
        installation,
    );

    runtime::block_on(async { futures_util::future::try_join(manager.refresh(&id), manager.refresh(&id)).await })
        .unwrap();

    let state = manager.repositories.get(&id).unwrap();
    verified_active_snapshot(state).unwrap();
    assert_eq!(
        fs::read_dir(state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY))
            .unwrap()
            .count(),
        1
    );
    assert!(
        fs::read_dir(&state.cache_dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".index-candidate-"))
    );
}

#[test]
fn stable_snapshot_view_blocks_refresh_across_multiple_queries() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("stone.index");
    let first = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]);
    fs::write(&source_path, &first).unwrap();
    let (_root, installation) = test_installation();
    let (id, manager) = explicit_manager(
        "stable-view",
        direct_repository(Url::from_file_path(&source_path).unwrap()),
        installation,
    );
    runtime::block_on(manager.refresh(&id)).unwrap();
    let manager = Arc::new(manager);
    let stable = manager.stable_snapshot_view().unwrap();
    assert_eq!(stable.snapshots()[0].sha256, index_identity(&first).sha256);

    let second = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('b', "package-b.stone")]);
    fs::write(&source_path, &second).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = manager.clone();
    let writer_id = id.clone();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx.send(runtime::block_on(writer.refresh(&writer_id))).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());

    assert!(
        manager
            .resolve_exact_package(&package::Id::from("a".repeat(64)))
            .unwrap()
            .is_some()
    );
    assert!(
        manager
            .resolve_exact_package(&package::Id::from("b".repeat(64)))
            .unwrap()
            .is_none()
    );
    assert_eq!(stable.snapshots()[0].sha256, index_identity(&first).sha256);

    drop(stable);
    done_rx.recv_timeout(Duration::from_secs(120)).unwrap().unwrap();
    thread.join().unwrap();
    assert!(
        manager
            .resolve_exact_package(&package::Id::from("b".repeat(64)))
            .unwrap()
            .is_some()
    );
}

#[test]
fn stable_snapshot_view_blocks_repository_removal() {
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("stone.index");
    fs::write(
        &source_path,
        meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]),
    )
    .unwrap();
    let config_directory = tempfile::tempdir().unwrap();
    fs::set_permissions(config_directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = config::Manager::custom(config_directory.path());
    let (_root, installation) = test_installation();
    let id = repository::Id::new("removable");
    let repository = direct_repository(Url::from_file_path(&source_path).unwrap());
    let mut writer = Manager::with_config_manager(config.clone(), installation.clone()).unwrap();
    writer.add_repository(id.clone(), repository).unwrap();
    runtime::block_on(writer.refresh(&id)).unwrap();
    let reader = Manager::with_config_manager(config, installation).unwrap();
    let cache_path = reader.repositories.get(&id).unwrap().cache_dir.clone();
    let stable = reader.stable_snapshot_view().unwrap();

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let remove_id = id.clone();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx.send(writer.remove(remove_id)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(cache_path.exists());
    assert!(
        reader
            .resolve_exact_package(&package::Id::from("a".repeat(64)))
            .unwrap()
            .is_some()
    );

    drop(stable);
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_secs(120)).unwrap().unwrap(),
        Removal::ConfigDeleted(true)
    ));
    thread.join().unwrap();
    assert!(!cache_path.exists());
}

#[test]
fn immutable_generation_budget_accepts_n_and_rejects_n_plus_one() {
    let (_cache, state) = cached(meta::Database::new(":memory:").unwrap());
    let cache_directory = open_cache_directory(&state).unwrap();
    let owner = directory_owner(&cache_directory, &state.cache_dir).unwrap();
    let indexes = open_indexes_directory(&state, &cache_directory, true).unwrap();
    let indexes_path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);

    for generation in 0..(MAX_INDEX_GENERATIONS - 1) {
        let path = indexes_path.join(format!("{generation:064x}.stone"));
        fs::write(&path, [generation as u8]).unwrap();
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let candidate = IndexIdentity {
        sha256: "f".repeat(64),
        byte_size: 1,
    };
    let candidate_name = immutable_index_name(&candidate.sha256).unwrap();
    enforce_index_generation_budget(&state, &indexes, owner, &candidate_name, &candidate).unwrap();

    let final_existing_path = indexes_path.join(format!("{:064x}.stone", MAX_INDEX_GENERATIONS - 1));
    fs::write(&final_existing_path, [0_u8]).unwrap();
    fs::set_permissions(&final_existing_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(matches!(
        enforce_index_generation_budget(&state, &indexes, owner, &candidate_name, &candidate),
        Err(Error::IndexGenerationLimit {
            limit: MAX_INDEX_GENERATIONS
        })
    ));

    let existing = IndexIdentity {
        sha256: format!("{:064x}", MAX_INDEX_GENERATIONS - 1),
        byte_size: 1,
    };
    let existing_name = immutable_index_name(&existing.sha256).unwrap();
    enforce_index_generation_budget(&state, &indexes, owner, &existing_name, &existing).unwrap();
}

#[test]
fn bounded_index_identity_accepts_n_and_rejects_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let limit = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS.max_bytes;
    let exact = temporary.path().join("exact");
    fs::File::create(&exact).unwrap().set_len(limit).unwrap();
    let exact_file = fs::File::open(&exact).unwrap();
    assert_eq!(read_index_bytes(&exact_file, &exact).unwrap().0.len() as u64, limit);

    let too_large = temporary.path().join("too-large");
    fs::File::create(&too_large).unwrap().set_len(limit + 1).unwrap();
    assert!(matches!(
        read_index_bytes(&fs::File::open(&too_large).unwrap(), &too_large),
        Err(Error::IndexTooLarge { limit: actual, .. }) if actual == limit
    ));
}
