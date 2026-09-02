use std::{
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    },
    path::Path,
};

use stone::{
    StoneHeader, StoneHeaderV1, StoneHeaderV1FileType, StonePayloadCompression, StonePayloadHeader, StonePayloadKind,
    StonePayloadLayoutRecord, StonePayloadMetaDependency, StonePayloadMetaPrimitive, StonePayloadMetaRecord,
    StonePayloadMetaTag, StoneReadError, StoneWriter, read_bytes_with_limits,
};

use crate::repository::format;

use super::*;

fn string(tag: StonePayloadMetaTag, value: impl Into<String>) -> StonePayloadMetaRecord {
    StonePayloadMetaRecord {
        tag,
        primitive: StonePayloadMetaPrimitive::String(value.into()),
    }
}

fn integer(tag: StonePayloadMetaTag, value: u64) -> StonePayloadMetaRecord {
    StonePayloadMetaRecord {
        tag,
        primitive: StonePayloadMetaPrimitive::Uint64(value),
    }
}

fn valid_meta(hash: char, uri: &str) -> Vec<StonePayloadMetaRecord> {
    vec![
        string(StonePayloadMetaTag::Name, format!("package-{hash}")),
        string(StonePayloadMetaTag::Architecture, "x86_64"),
        string(StonePayloadMetaTag::Version, "1.0"),
        string(StonePayloadMetaTag::Summary, "A package"),
        string(StonePayloadMetaTag::Description, "A package for repository tests"),
        string(StonePayloadMetaTag::Homepage, "https://example.test"),
        string(StonePayloadMetaTag::SourceID, format!("package-{hash}")),
        integer(StonePayloadMetaTag::Release, 1),
        integer(StonePayloadMetaTag::BuildRelease, 1),
        string(StonePayloadMetaTag::PackageURI, uri),
        string(StonePayloadMetaTag::PackageHash, hash.to_string().repeat(64)),
        integer(StonePayloadMetaTag::PackageSize, 4_096),
        string(StonePayloadMetaTag::License, "MPL-2.0"),
        StonePayloadMetaRecord {
            tag: StonePayloadMetaTag::Depends,
            primitive: StonePayloadMetaPrimitive::Dependency(
                StonePayloadMetaDependency::PackageName,
                "runtime".to_owned(),
            ),
        },
    ]
}

fn meta_index(file_type: StoneHeaderV1FileType, payloads: &[Vec<StonePayloadMetaRecord>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StoneWriter::new(&mut bytes, file_type).unwrap();
    for payload in payloads {
        writer.add_payload(payload.as_slice()).unwrap();
    }
    writer.finalize().unwrap();
    bytes
}

fn layout_index() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StoneWriter::new(&mut bytes, StoneHeaderV1FileType::Repository).unwrap();
    let layout = Vec::<StonePayloadLayoutRecord>::new();
    writer.add_payload(layout.as_slice()).unwrap();
    writer.finalize().unwrap();
    bytes
}

fn test_index_uri() -> Url {
    "https://cdn.example.test/main/history/1783706384/x86_64/stone.index"
        .parse()
        .unwrap()
}

fn cached(db: meta::Database) -> (tempfile::TempDir, repository::Cached) {
    let index_uri = test_index_uri();
    let cache = tempfile::tempdir().unwrap();
    fs::set_permissions(cache.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = repository::Cached::new(
        repository::Id::new("test"),
        Repository {
            description: "test".to_owned(),
            source: repository::Source::DirectIndex(index_uri),
            priority: repository::Priority::new(0),
            active: true,
        },
        db,
        None,
        cache.path().to_owned(),
    );
    (cache, state)
}

fn sentinel_meta() -> package::Meta {
    let mut meta = package::Meta::from_repository_index_payload(&valid_meta(
        'f',
        "../../../pool/v0/f/foo/foo-1.0-1-1-x86_64.stone",
    ))
    .unwrap();
    meta.uri = Some("https://cdn.example.test/main/pool/v0/f/foo/foo-1.0-1-1-x86_64.stone".to_owned());
    meta
}

fn write_index(bytes: &[u8]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), bytes).unwrap();
    file
}

fn fetched_test_index(
    state: &repository::Cached,
    source: &Path,
    index_uri: Url,
    mutation: &RepositoryMutationLock,
) -> FetchedIndex {
    let cache_clone = mutation.cache_directory.try_clone().unwrap();
    let directory = tempfile::Builder::new()
        .prefix(".index-candidate-test-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(proc_fd_path(&cache_clone))
        .unwrap();
    let name = directory.path().file_name().unwrap();
    let directory_display = state.cache_dir.join(name);
    let directory_file = openat2_file(
        mutation.cache_directory.as_raw_fd(),
        name.as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &directory_display,
    )
    .unwrap();
    let path = directory_display.join(INDEX_CANDIDATE_NAME);
    fs::write(
        proc_fd_path(&directory_file).join(INDEX_CANDIDATE_NAME),
        fs::read(source).unwrap(),
    )
    .unwrap();
    let file = openat2_file(
        directory_file.as_raw_fd(),
        INDEX_CANDIDATE_NAME.as_bytes(),
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &path,
    )
    .unwrap();
    FetchedIndex {
        _directory: directory,
        _cache_directory: cache_clone,
        directory: directory_file,
        file,
        path,
        index_uri,
    }
}

fn activate_test_index(state: &repository::Cached, source: &Path, index_uri: &Url) -> Result<(), Error> {
    let mutation = RepositoryMutationLock::acquire(state)?;
    let candidate = fetched_test_index(state, source, index_uri.clone(), &mutation);
    activate_index_candidate(state, candidate, mutation)
}

fn test_installation() -> (tempfile::TempDir, Installation) {
    let root = crate::test_support::private_installation_tempdir();
    let installation = Installation::open(root.path(), None).unwrap();
    (root, installation)
}

fn direct_repository(index_uri: Url) -> Repository {
    Repository {
        description: "test".to_owned(),
        source: repository::Source::DirectIndex(index_uri),
        priority: repository::Priority::new(0),
        active: true,
    }
}

fn explicit_manager(identifier: &str, repository: Repository, installation: Installation) -> (repository::Id, Manager) {
    let id = repository::Id::new("test");
    let manager = Manager::with_explicit(
        identifier,
        repository::Map::with([(id.clone(), repository)]),
        installation,
    )
    .unwrap();
    (id, manager)
}

fn empty_payload_archive(payloads: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    StoneHeader::V1(StoneHeaderV1 {
        num_payloads: payloads,
        file_type: StoneHeaderV1FileType::Repository,
    })
    .encode(&mut bytes)
    .unwrap();
    let header = StonePayloadHeader {
        stored_size: 0,
        plain_size: 0,
        checksum: xxh3_64(&[]).to_be_bytes(),
        num_records: 0,
        version: 1,
        kind: StonePayloadKind::Unknown,
        compression: StonePayloadCompression::None,
    };
    for _ in 0..payloads {
        header.encode(&mut bytes).unwrap();
    }
    bytes
}

#[test]
fn observed_pinned_aerynos_index_shape_fits_repository_limits() {
    // Audited by fully decoding the official immutable snapshot at
    // https://cdn.aerynos.dev/main/history/1783706384/x86_64/stone.index
    // with SHA-256 0f986f19f4e88f74ed5ae3452fbd9cc34ab53915f391555952d68ac90b202efc.
    let limits = repository_index_decode_limits();
    let download_limits = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS;
    assert_eq!(download_limits.max_bytes, 16 * 1024 * 1024);
    assert_eq!(download_limits.total_timeout, Duration::from_secs(120));
    assert_eq!(limits.max_payloads, 8_192);
    assert!(5_463 <= limits.max_payloads);
    assert!(334 <= limits.max_records_per_payload);
    assert!(18_812 <= limits.max_plain_payload_bytes);
    assert!(18_812 <= limits.max_record_bytes);
    assert!(2_715 <= limits.max_stored_payload_bytes);
    assert!(114_447 <= limits.max_total_records);
    assert!(4_113_121 <= limits.max_total_plain_bytes);
    assert!(4_113_121 <= limits.max_total_record_bytes);
    assert!(2_257_339 <= limits.max_total_stored_bytes);
    assert!(2_432_187 <= download_limits.max_bytes);
}

#[test]
fn repository_payload_limit_accepts_n_and_rejects_n_plus_one() {
    let limits = repository_index_decode_limits();
    let accepted = empty_payload_archive(8_192);
    let mut reader = read_bytes_with_limits(&accepted, limits).unwrap();
    let decoded = reader
        .payloads()
        .unwrap()
        .try_fold(0, |count, payload| payload.map(|_| count + 1))
        .unwrap();
    assert_eq!(decoded, 8_192);

    let rejected = empty_payload_archive(8_193);
    assert!(matches!(
        read_bytes_with_limits(&rejected, limits),
        Err(StoneReadError::LimitExceeded {
            resource: "payload count",
            limit: 8_192,
            actual: 8_193,
        })
    ));
}

#[test]
fn read_only_installation_rejects_repository_cache_without_weakening_ownership() {
    let (_root, mut installation) = test_installation();
    installation.mutability = crate::installation::Mutability::ReadOnly;
    let index_uri = test_index_uri();
    assert!(matches!(
        Manager::with_explicit(
            "read-only",
            repository::Map::with([(repository::Id::new("test"), direct_repository(index_uri))]),
            installation,
        ),
        Err(Error::ReadOnlyRepositoryCacheUnsupported(_))
    ));
}

#[test]
fn config_manager_preserves_repository_fragment_precedence() {
    let config_directory = tempfile::tempdir().unwrap();
    fs::set_permissions(config_directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let fragments = config_directory.path().join("repo.d");
    fs::create_dir_all(&fragments).unwrap();
    fs::write(
        fragments.join("a.lua"),
        r#"return {
    {
        id = "selected",
        description = { kind = "none" },
        source = { kind = "direct_index", uri = "file:///a.index" },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
}
"#,
    )
    .unwrap();
    fs::write(
        fragments.join("z.lua"),
        r#"return {
    {
        id = "selected",
        description = { kind = "none" },
        source = { kind = "direct_index", uri = "file:///z.index" },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
}
"#,
    )
    .unwrap();

    let (_root, installation) = test_installation();
    let manager = Manager::with_config_manager(config::Manager::custom(config_directory.path()), installation).unwrap();
    let selected = manager.repositories.get(&repository::Id::new("selected")).unwrap();
    let repository::Source::DirectIndex(uri) = &selected.repository.source else {
        panic!("expected direct repository source");
    };
    assert_eq!(uri.as_str(), "file:///z.index");
    assert!(selected.config_path.as_ref().unwrap().ends_with("repo.d/z.lua"));
}

#[test]
fn config_manager_loads_a_lua_repository_fragment_by_extension() {
    let config_directory = tempfile::tempdir().unwrap();
    fs::set_permissions(config_directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let fragments = config_directory.path().join("repo.d");
    fs::create_dir_all(&fragments).unwrap();
    fs::write(
        fragments.join("main.lua"),
        r#"
return {
    {
        id = "lua-main",
        description = { kind = "none" },
        source = { kind = "direct_index", uri = "file:///lua.index" },
        priority = { kind = "none" },
        enabled = { kind = "none" },
    },
}
"#,
    )
    .unwrap();

    let (_root, installation) = test_installation();
    let manager = Manager::with_config_manager(config::Manager::custom(config_directory.path()), installation).unwrap();
    let selected = manager.repositories.get(&repository::Id::new("lua-main")).unwrap();
    let repository::Source::DirectIndex(uri) = &selected.repository.source else {
        panic!("expected direct repository source");
    };
    assert_eq!(uri.as_str(), "file:///lua.index");
    assert!(selected.config_path.as_ref().unwrap().ends_with("repo.d/main.lua"));
}

#[test]
fn config_manager_admits_only_registered_declaration_languages() {
    // Only the registered language set (`lua`) is dispatched. Serialized
    // configuration formats — YAML, KDL, JSON — are never an authored surface;
    // a fragment with an unregistered extension is not loaded as a repository.
    let config_directory = tempfile::tempdir().unwrap();
    fs::set_permissions(config_directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let fragments = config_directory.path().join("repo.d");
    fs::create_dir_all(&fragments).unwrap();
    for (name, body) in [
        (
            "main.yaml",
            "- id: yaml-repo\n  source: {direct_index: {uri: 'file:///y.index'}}\n",
        ),
        (
            "main.kdl",
            "repository \"kdl-repo\" { direct_index \"file:///k.index\" }\n",
        ),
        ("main.json", "[{\"id\":\"json-repo\"}]\n"),
    ] {
        fs::write(fragments.join(name), body).unwrap();
    }

    let (_root, installation) = test_installation();
    let manager = Manager::with_config_manager(config::Manager::custom(config_directory.path()), installation).unwrap();

    // None of the unregistered-extension fragments produced a repository.
    assert_eq!(manager.repositories.iter().count(), 0);
    for id in ["yaml-repo", "kdl-repo", "json-repo"] {
        assert!(manager.repositories.get(&repository::Id::new(id)).is_none());
    }
}

#[test]
fn repository_transport_errors_never_render_credentials() {
    let uri = Url::parse("https://user:secret@example.test/stone.index").unwrap();
    let error = validate_repository_transport(&uri).unwrap_err().to_string();
    assert!(!error.contains("user"));
    assert!(!error.contains("secret"));
    assert!(!error.contains(uri.as_str()));
}

#[test]
fn repository_package_uri_accepts_official_parent_paths_and_rejects_origin_changes() {
    let index = test_index_uri();
    let source = repository::Source::DirectIndex(index.clone());
    let resolved = normalize_repository_package_uri(
        &source,
        &index,
        "../../../pool/v0/e/example/example-1.0-1-1-x86_64.stone",
    )
    .unwrap();
    assert_eq!(
        resolved.as_str(),
        "https://cdn.example.test/main/pool/v0/e/example/example-1.0-1-1-x86_64.stone"
    );

    assert!(matches!(
        normalize_repository_package_uri(&source, &index, "https://cdn.example.test/package.stone"),
        Err(PackageUriError::AbsoluteReference)
    ));
    assert!(matches!(
        normalize_repository_package_uri(&source, &index, "//other.example.test/package.stone"),
        Err(PackageUriError::CrossOrigin)
    ));
    assert!(matches!(
        normalize_repository_package_uri(&source, &index, "../../../pool/package.stone#fragment"),
        Err(PackageUriError::Fragment)
    ));
}

#[test]
fn root_package_capability_preserves_trailing_and_non_trailing_base_paths() {
    let history = format::Identifier::new("1783706384").unwrap();
    for base in [
        "https://cdn.example.test/repositories",
        "https://cdn.example.test/repositories/",
    ] {
        let root = repository::RootIndexSource {
            base_uri: base.parse().unwrap(),
            channel: "main".try_into().unwrap(),
            version: "stream/unstable".try_into().unwrap(),
            arch: "x86_64".to_owned(),
        };
        let index = root.history_index_uri(&history);
        let source = repository::Source::RootIndex(root);
        let package = normalize_repository_package_uri(&source, &index, "../../../pool/package.stone").unwrap();
        assert_eq!(
            package.as_str(),
            "https://cdn.example.test/repositories/main/pool/package.stone"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let repository_path = temp.path().join("repositories");
    for trailing_slash in [false, true] {
        let mut base_uri = Url::from_file_path(&repository_path).unwrap();
        if trailing_slash {
            let mut path = base_uri.path().to_owned();
            path.push('/');
            base_uri.set_path(&path);
        }
        let root = repository::RootIndexSource {
            base_uri,
            channel: "main".try_into().unwrap(),
            version: "stream/unstable".try_into().unwrap(),
            arch: "x86_64".to_owned(),
        };
        let index = root.history_index_uri(&history);
        let source = repository::Source::RootIndex(root);
        let package = normalize_repository_package_uri(&source, &index, "../../../pool/package.stone").unwrap();
        assert_eq!(
            package.to_file_path().unwrap(),
            repository_path.join("main/pool/package.stone")
        );
    }
}

#[test]
fn update_rejects_wrong_container_or_payload_without_changing_database() {
    let db = meta::Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    db.add(sentinel.clone(), sentinel_meta()).unwrap();
    let (_cache, state) = cached(db);

    let wrong_container = write_index(&meta_index(
        StoneHeaderV1FileType::Binary,
        &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
    ));
    let error = activate_test_index(&state, wrong_container.path(), &test_index_uri()).unwrap_err();
    assert!(
        matches!(error, Error::UnexpectedIndexFileType(StoneHeaderV1FileType::Binary)),
        "{error:?}"
    );
    assert!(state.db.get(&sentinel).is_ok());

    let wrong_payload = write_index(&layout_index());
    assert!(matches!(
        activate_test_index(&state, wrong_payload.path(), &test_index_uri()),
        Err(Error::UnexpectedIndexPayload {
            index: 0,
            kind: StonePayloadKind::Layout,
        })
    ));
    assert!(state.db.get(&sentinel).is_ok());
}

#[test]
fn invalid_late_entry_and_duplicate_identity_preserve_existing_database() {
    let db = meta::Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    db.add(sentinel.clone(), sentinel_meta()).unwrap();
    let (_cache, state) = cached(db);

    let first = valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone");
    let mut invalid = valid_meta('b', "../../../pool/v0/b/b/b-1.0-1-1-x86_64.stone");
    invalid.retain(|record| record.tag != StonePayloadMetaTag::PackageSize);
    let invalid_index = write_index(&meta_index(
        StoneHeaderV1FileType::Repository,
        &[first.clone(), invalid],
    ));
    assert!(matches!(
        activate_test_index(&state, invalid_index.path(), &test_index_uri()),
        Err(Error::InvalidRepositoryMeta { index: 1, .. })
    ));
    assert!(state.db.get(&sentinel).is_ok());
    assert!(state.db.get(&package::Id::from("a".repeat(64))).is_err());

    let duplicate_index = write_index(&meta_index(StoneHeaderV1FileType::Repository, &[first.clone(), first]));
    assert!(matches!(
        activate_test_index(&state, duplicate_index.path(), &test_index_uri()),
        Err(Error::DuplicateIndexPackage { index: 1 })
    ));
    assert!(state.db.get(&sentinel).is_ok());
}

#[test]
fn valid_repository_index_replaces_database_and_normalizes_package_uri() {
    let db = meta::Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    db.add(sentinel.clone(), sentinel_meta()).unwrap();
    let (_cache, state) = cached(db);
    let hash = "a".repeat(64);
    let index = write_index(&meta_index(
        StoneHeaderV1FileType::Repository,
        &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
    ));

    activate_test_index(&state, index.path(), &test_index_uri()).unwrap();

    assert!(state.db.get(&sentinel).is_err());
    let stored = state.db.get(&package::Id::from(hash)).unwrap();
    assert_eq!(
        stored.uri.as_deref(),
        Some("https://cdn.example.test/main/pool/v0/a/a/a-1.0-1-1-x86_64.stone")
    );
}

#[test]
fn unrepresentable_metadata_never_consumes_generation_budget() {
    let (_cache, state) = cached(meta::Database::new(":memory:").unwrap());
    for attempt in 0..=MAX_INDEX_GENERATIONS {
        let mut invalid = valid_meta('a', "package-a.stone");
        for record in &mut invalid {
            if record.tag == StonePayloadMetaTag::Release {
                record.primitive = StonePayloadMetaPrimitive::Uint64(i32::MAX as u64 + 1);
            }
            if record.tag == StonePayloadMetaTag::Summary {
                record.primitive = StonePayloadMetaPrimitive::String(format!("attempt-{attempt}"));
            }
        }
        let index = write_index(&meta_index(StoneHeaderV1FileType::Repository, &[invalid]));
        let error = activate_test_index(&state, index.path(), &test_index_uri()).unwrap_err();
        assert!(
            matches!(
                error,
                Error::InvalidRepositoryMeta {
                    source: package::RepositoryMetaError::IntegerOutOfRange {
                        tag: StonePayloadMetaTag::Release,
                        ..
                    },
                    ..
                } | Error::Database(meta::Error::MetaIntegerOutOfRange {
                    field: "source_release",
                    ..
                })
            ),
            "{error:?}"
        );
    }
    assert!(state.db.active_snapshot().unwrap().is_none());
    let indexes = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
    assert!(!indexes.exists() || fs::read_dir(indexes).unwrap().next().is_none());
}

mod refresh;
