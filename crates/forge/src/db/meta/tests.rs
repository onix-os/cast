use stone::StoneDecodedPayload;

use crate::dependency::Kind;

use super::*;

fn fixture_meta() -> Meta {
    let bytes = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");
    let mut stone = stone::read_bytes(bytes).unwrap();
    let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
    Meta::from_stone_payload(&payload.body).unwrap()
}

fn fixture_snapshot(hash: char) -> Snapshot {
    Snapshot::new(
        format!("https://cdn.example.test/main/history/{hash}/x86_64/stone.index")
            .parse()
            .unwrap(),
        hash.to_string().repeat(64),
        2_432_187,
    )
    .unwrap()
}

fn snapshot_uri_with_length(length: usize) -> Url {
    const PREFIX: &str = "https://example.test/";
    assert!(length >= PREFIX.len());
    let uri = format!("{PREFIX}{}", "a".repeat(length - PREFIX.len()));
    assert_eq!(uri.len(), length);
    uri.parse().unwrap()
}

#[test]
fn create_insert_select() {
    let db = Database::new(":memory:").unwrap();

    let bash_completion = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");

    let mut stone = stone::read_bytes(bash_completion).unwrap();

    let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let meta_payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
    let meta = Meta::from_stone_payload(&meta_payload.body).unwrap();

    let id = package::Id::from("test");

    db.add(id.clone(), meta.clone()).unwrap();

    assert_eq!(&meta.name, &"bash-completion".to_owned().into());

    // Now retrieve by provider.
    let lookup = Filter::Provider(Provider {
        kind: Kind::PackageName,
        name: "bash-completion".to_owned(),
    });
    let fetched = db.query(Some(lookup)).unwrap();
    assert_eq!(fetched.len(), 1);

    db.remove(&id).unwrap();

    let result = db.get(&id);

    assert!(result.is_err());

    // Test wipe
    db.add(id.clone(), meta).unwrap();
    db.wipe().unwrap();
    let result = db.get(&id);
    assert!(result.is_err());
}

#[test]
fn test_conflict_is_recognized() {
    let db = Database::new(":memory:").unwrap();

    // See `tests/fixtures/conflicts/italian-pizza.glu` for the recipe file that produced this stone.
    // It should be obvious that this package conflicts with `name(pineapple)`.
    let italian_pizza = include_bytes!("../../../../../tests/fixtures/conflicts/italian-pizza-1-1-1-x86_64.stone");
    let pineapple_provider = Provider {
        kind: Kind::PackageName,
        name: "pineapple".to_owned(),
    };

    let mut stone = stone::read_bytes(italian_pizza).unwrap();

    let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let meta_payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
    let meta = Meta::from_stone_payload(&meta_payload.body).unwrap();
    db.add(package::Id::from(meta.id()), meta.clone()).unwrap();

    // Ensure we're parsing the correct package!
    assert_eq!(&meta.name, &"italian-pizza".to_owned().into());
    // Ensure that the conflict info already exists in the binary package.
    assert_eq!(
        meta.conflicts.iter().collect::<Vec<&Provider>>(),
        vec![&pineapple_provider]
    );

    // Now retrieve by provider.
    let lookup = Filter::Provider(Provider {
        kind: Kind::PackageName,
        name: "italian-pizza".to_owned(),
    });
    let fetched = db.query(Some(lookup)).unwrap();
    assert_eq!(fetched.len(), 1);

    let (_, retrieved_pkg) = fetched.first().unwrap();
    let retrieved_conflicts: Vec<&Provider> = retrieved_pkg.conflicts.iter().collect();
    // Ensure that the conflicts field is inserted into and can be queried from our database
    // correctly.
    assert_eq!(retrieved_conflicts, vec![&pineapple_provider]);
}

#[test]
fn replace_all_commits_complete_metadata_and_relations() {
    let db = Database::new(":memory:").unwrap();
    let old = package::Id::from("old");
    db.add(old.clone(), fixture_meta()).unwrap();

    let first = package::Id::from("first");
    let second = package::Id::from("second");
    db.replace_all(vec![(first.clone(), fixture_meta()), (second.clone(), fixture_meta())])
        .unwrap();

    assert!(!db.package_ids().unwrap().contains(&old));
    assert!(!db.get(&first).unwrap().providers.is_empty());
    assert!(!db.get(&second).unwrap().licenses.is_empty());
    assert_eq!(db.package_ids().unwrap(), BTreeSet::from([first, second]));
}

#[test]
fn active_snapshot_validates_uri_digest_and_exact_size_boundaries() {
    let exact = Snapshot::new(
        snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES),
        "a".repeat(64),
        MAX_SNAPSHOT_BYTE_SIZE,
    )
    .unwrap();
    assert_eq!(exact.index_uri().as_str().len(), MAX_SNAPSHOT_INDEX_URI_BYTES);
    assert_eq!(exact.byte_size(), MAX_SNAPSHOT_BYTE_SIZE);

    assert!(matches!(
        Snapshot::new(
            snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES + 1),
            "a".repeat(64),
            0,
        ),
        Err(Error::SnapshotIndexUriTooLong {
            limit: MAX_SNAPSHOT_INDEX_URI_BYTES,
            actual,
        }) if actual == MAX_SNAPSHOT_INDEX_URI_BYTES + 1
    ));
    assert!(matches!(
        Snapshot::new("ftp://example.test/stone.index".parse().unwrap(), "a".repeat(64), 0,),
        Err(Error::SnapshotIndexUriPolicy { .. })
    ));
    assert!(matches!(
        Snapshot::new("http://example.test/stone.index".parse().unwrap(), "a".repeat(64), 0,),
        Err(Error::SnapshotIndexUriPolicy { .. })
    ));
    assert!(matches!(
        Snapshot::new("https://example.test/stone.index".parse().unwrap(), "A".repeat(64), 0,),
        Err(Error::InvalidSnapshotSha256)
    ));
    assert!(matches!(
        Snapshot::new("https://example.test/stone.index".parse().unwrap(), "a".repeat(63), 0,),
        Err(Error::InvalidSnapshotSha256)
    ));
    assert!(matches!(
        Snapshot::new(
            "https://example.test/stone.index".parse().unwrap(),
            "a".repeat(64),
            MAX_SNAPSHOT_BYTE_SIZE + 1,
        ),
        Err(Error::SnapshotByteSizeOutOfRange {
            limit: MAX_SNAPSHOT_BYTE_SIZE,
            actual,
        }) if actual == MAX_SNAPSHOT_BYTE_SIZE + 1
    ));
}

#[test]
fn active_snapshot_migration_enforces_singleton_hash_uri_and_size_bounds() {
    use diesel::sql_types::{BigInt, Integer, Text};

    let db = Database::new(":memory:").unwrap();
    let insert = |singleton: i32, index_uri: &str, sha256: &str, byte_size: i64| {
        db.conn.exec(|conn| {
            diesel::sql_query(
                "INSERT INTO active_repository_snapshot \
                 (singleton, index_uri, sha256, byte_size) VALUES (?, ?, ?, ?)",
            )
            .bind::<Integer, _>(singleton)
            .bind::<Text, _>(index_uri)
            .bind::<Text, _>(sha256)
            .bind::<BigInt, _>(byte_size)
            .execute(conn)
        })
    };

    assert!(insert(2, "https://example.test/stone.index", &"a".repeat(64), 0).is_err());
    assert!(insert(1, "https://example.test/stone.index", &"A".repeat(64), 0).is_err());
    assert!(
        insert(
            1,
            snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES + 1).as_str(),
            &"a".repeat(64),
            0,
        )
        .is_err()
    );
    assert!(
        insert(
            1,
            "https://example.test/stone.index",
            &"a".repeat(64),
            i64::try_from(MAX_SNAPSHOT_BYTE_SIZE + 1).unwrap(),
        )
        .is_err()
    );
    assert!(insert(1, "https://example.test/stone.index", &"a".repeat(64), -1).is_err());
    assert_eq!(db.active_snapshot().unwrap(), None);
}

#[test]
fn replace_all_with_snapshot_round_trips_complete_active_state() {
    let db = Database::new(":memory:").unwrap();
    let first = package::Id::from("first");
    let second = package::Id::from("second");
    let snapshot = fixture_snapshot('a');

    db.replace_all_with_snapshot(
        vec![(first.clone(), fixture_meta()), (second.clone(), fixture_meta())],
        snapshot.clone(),
    )
    .unwrap();

    assert_eq!(db.package_ids().unwrap(), BTreeSet::from([first, second]));
    assert_eq!(db.active_snapshot().unwrap(), Some(snapshot));
}

#[test]
fn legacy_package_mutation_invalidates_the_active_snapshot() {
    let db = Database::new(":memory:").unwrap();
    db.replace_all_with_snapshot(vec![(package::Id::from("old"), fixture_meta())], fixture_snapshot('a'))
        .unwrap();

    db.replace_all(vec![(package::Id::from("new"), fixture_meta())])
        .unwrap();

    assert_eq!(db.active_snapshot().unwrap(), None);
}

#[test]
fn replace_all_validates_complete_batch_before_deleting_existing_metadata() {
    let db = Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    db.add(sentinel.clone(), fixture_meta()).unwrap();

    let duplicate = package::Id::from("duplicate");
    let error = db
        .replace_all(vec![(duplicate.clone(), fixture_meta()), (duplicate, fixture_meta())])
        .unwrap_err();
    assert!(matches!(error, Error::DuplicatePackageId));
    assert!(db.get(&sentinel).is_ok());

    let mut overflowing = fixture_meta();
    overflowing.source_release = u64::MAX;
    let error = db
        .replace_all(vec![(package::Id::from("overflowing"), overflowing)])
        .unwrap_err();
    assert!(matches!(
        error,
        Error::MetaIntegerOutOfRange {
            field: "source_release",
            ..
        }
    ));
    assert!(db.get(&sentinel).is_ok());
}

#[test]
fn replace_all_rolls_back_delete_and_partial_insert_on_sqlite_failure() {
    let db = Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    let sentinel_meta = fixture_meta();
    db.add(sentinel.clone(), sentinel_meta.clone()).unwrap();

    db.conn.exec(|conn| {
        diesel::sql_query(
            "CREATE TRIGGER reject_broken_package \
             BEFORE INSERT ON meta \
             WHEN NEW.package = 'broken' \
             BEGIN SELECT RAISE(ABORT, 'injected replacement failure'); END",
        )
        .execute(conn)
        .unwrap();
    });

    let replacement_meta = fixture_meta();
    let mut candidates = (0..PACKAGE_INSERT_CHUNK_SIZE)
        .map(|index| {
            (
                package::Id::from(format!("candidate-{index:03}")),
                replacement_meta.clone(),
            )
        })
        .collect::<Vec<_>>();
    candidates.push((package::Id::from("broken"), replacement_meta));

    let error = db.replace_all(candidates).unwrap_err();
    assert!(matches!(error, Error::Diesel(_)));

    assert_eq!(db.get(&sentinel).unwrap(), sentinel_meta);
    assert_eq!(db.package_ids().unwrap(), BTreeSet::from([sentinel]));
}

#[test]
fn snapshot_replacement_failure_in_package_chunk_129_preserves_complete_old_state() {
    let db = Database::new(":memory:").unwrap();
    let sentinel = package::Id::from("sentinel");
    let sentinel_meta = fixture_meta();
    let old_snapshot = fixture_snapshot('a');
    db.replace_all_with_snapshot(vec![(sentinel.clone(), sentinel_meta.clone())], old_snapshot.clone())
        .unwrap();

    db.conn.exec(|conn| {
        diesel::sql_query(
            "CREATE TRIGGER reject_broken_snapshot_package \
             BEFORE INSERT ON meta \
             WHEN NEW.package = 'broken' \
             BEGIN SELECT RAISE(ABORT, 'injected snapshot replacement failure'); END",
        )
        .execute(conn)
        .unwrap();
    });

    let replacement_meta = fixture_meta();
    let mut candidates = (0..PACKAGE_INSERT_CHUNK_SIZE)
        .map(|index| {
            (
                package::Id::from(format!("candidate-{index:03}")),
                replacement_meta.clone(),
            )
        })
        .collect::<Vec<_>>();
    candidates.push((package::Id::from("broken"), replacement_meta));

    let error = db
        .replace_all_with_snapshot(candidates, fixture_snapshot('b'))
        .unwrap_err();
    assert!(matches!(error, Error::Diesel(_)));

    assert_eq!(db.get(&sentinel).unwrap(), sentinel_meta);
    assert_eq!(db.package_ids().unwrap(), BTreeSet::from([sentinel]));
    assert_eq!(db.active_snapshot().unwrap(), Some(old_snapshot));
}
