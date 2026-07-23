use super::{support::*, *};

#[test]
fn raw_ascii_case_alias_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"LOADER.CONF", expected, expected);

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::AsciiFoldAlias { .. }));
}

#[test]
fn kernel_short_alias_cannot_hide_a_different_raw_name() {
    let expected = b"entry";
    let requests = [request("VERYLON~1", expected)];
    let root = FixtureDirectory::stable(
        ROOT,
        vec![entry(
            b"very-long-name.conf".to_vec(),
            FILE_A,
            BootNamespaceNodeKind::Regular,
        )],
    )
    .with_lookup(FixtureLookup::stable(
        b"VERYLON~1".to_vec(),
        present(FILE_A, BootNamespaceNodeKind::Regular),
    ));
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        vec![FixtureRegularFile::stable(FILE_A, witness(FILE_A, expected), expected)],
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::LookupRawNameMismatch { .. }
    ));
}

#[test]
fn duplicate_raw_inventory_name_is_rejected() {
    let expected = b"entry";
    let requests = [request("target", expected)];
    let entries = vec![
        entry(b"same".to_vec(), FILE_A, BootNamespaceNodeKind::Regular),
        entry(b"same".to_vec(), FILE_B, BootNamespaceNodeKind::Regular),
    ];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries)],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::DuplicateRawName));
}

#[test]
fn duplicate_inventory_identity_is_rejected() {
    let expected = b"entry";
    let requests = [request("target", expected)];
    let entries = vec![
        entry(b"first".to_vec(), FILE_A, BootNamespaceNodeKind::Regular),
        entry(b"second".to_vec(), FILE_A, BootNamespaceNodeKind::Regular),
    ];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, entries)],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::DuplicateIdentityMapping));
}

#[test]
fn duplicate_requested_identity_across_directories_is_rejected() {
    let expected = b"entry";
    let requests = [request("one/file", expected), request("two/file", expected)];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![
            FixtureDirectory::stable(
                ROOT,
                vec![
                    entry(b"one".to_vec(), DIRECTORY_A, BootNamespaceNodeKind::Directory),
                    entry(b"two".to_vec(), DIRECTORY_B, BootNamespaceNodeKind::Directory),
                ],
            ),
            FixtureDirectory::stable(
                DIRECTORY_A,
                vec![entry(b"file".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
            ),
            FixtureDirectory::stable(
                DIRECTORY_B,
                vec![entry(b"file".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
            ),
        ],
        vec![FixtureRegularFile::stable(FILE_A, witness(FILE_A, expected), expected)],
        vec![
            FixtureExpectedStream::new(expected),
            FixtureExpectedStream::new(expected),
        ],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::DuplicateIdentityMapping));
}

#[test]
fn symlink_destination_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![entry(b"loader.conf".to_vec(), FILE_A, BootNamespaceNodeKind::Symlink)],
        )],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::Symlink { .. }));
}

#[test]
fn wrong_ancestor_node_kind_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader/entry.conf", expected)];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![entry(b"loader".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
        )],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::WrongNodeKind { .. }));
}

#[test]
fn cross_mount_lookup_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let foreign = BootNamespaceNodeIdentity::new(7, 201, ROOT.mount_id + 1);
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![entry(b"loader.conf".to_vec(), foreign, BootNamespaceNodeKind::Regular)],
        )],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::CrossMount { .. }));
}

#[test]
fn lookup_kind_must_match_raw_inventory_kind() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let root = FixtureDirectory::stable(
        ROOT,
        vec![entry(b"loader.conf".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
    )
    .with_lookup(FixtureLookup::stable(
        b"loader.conf".to_vec(),
        present(FILE_A, BootNamespaceNodeKind::Directory),
    ));
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::LookupKindMismatch { .. }));
}

#[test]
fn invalid_inventory_identity_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let invalid = BootNamespaceNodeIdentity::new(0, 1, ROOT.mount_id);
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![entry(b"loader.conf".to_vec(), invalid, BootNamespaceNodeKind::Regular)],
        )],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::InvalidObservedIdentity));
}
