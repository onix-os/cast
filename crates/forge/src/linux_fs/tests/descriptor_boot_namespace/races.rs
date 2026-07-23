use super::{support::*, *};

#[test]
fn absent_to_present_lookup_is_rejected_as_unstable_absence() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let root = FixtureDirectory::stable(ROOT, Vec::new()).with_lookup(FixtureLookup::changing(
        b"loader.conf".to_vec(),
        BootNamespaceLookup::Absent,
        present(FILE_A, BootNamespaceNodeKind::Regular),
    ));
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::UnstableAbsence { .. }));
}

#[test]
fn lookup_absence_cannot_disagree_with_raw_inventory() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let root = FixtureDirectory::stable(
        ROOT,
        vec![entry(b"loader.conf".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
    )
    .with_lookup(FixtureLookup::stable(
        b"loader.conf".to_vec(),
        BootNamespaceLookup::Absent,
    ));
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::LookupAbsenceInventoryConflict { .. }
    ));
}

#[test]
fn changing_complete_inventory_is_rejected() {
    let expected = b"entry";
    let requests = [request("target", expected)];
    let root = FixtureDirectory::changing(
        ROOT,
        vec![entry(b"old".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
        vec![entry(b"new".to_vec(), FILE_B, BootNamespaceNodeKind::Regular)],
    );
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::InventoryRace));
}

#[test]
fn changing_present_lookup_identity_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let root = FixtureDirectory::stable(
        ROOT,
        vec![entry(b"loader.conf".to_vec(), FILE_A, BootNamespaceNodeKind::Regular)],
    )
    .with_lookup(FixtureLookup::changing(
        b"loader.conf".to_vec(),
        present(FILE_A, BootNamespaceNodeKind::Regular),
        present(FILE_B, BootNamespaceNodeKind::Regular),
    ));
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![root],
        vec![FixtureRegularFile::stable(FILE_A, witness(FILE_A, expected), expected)],
        vec![FixtureExpectedStream::new(expected)],
        std::time::Instant::now(),
    );

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::LookupRace { .. }));
}

#[test]
fn changing_regular_witness_is_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", expected, expected);
    let mut closing = witness(FILE_A, expected);
    closing.version += 1;
    fixture.regular_files[0] = fixture.regular_files[0].clone().with_closing_witness(closing);

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::RegularContentRace { .. }));
}

#[test]
fn regular_witness_must_match_lookup_identity() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", expected, expected);
    fixture.regular_files[0].opening_witness.identity = FILE_B;

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::RegularWitnessIdentityMismatch { .. }
    ));
}

#[test]
fn actual_stream_must_match_stable_witness_digest() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", expected, expected);
    fixture.regular_files[0].opening_witness.digest = digest(b"other");
    fixture.regular_files[0].closing_witness.digest = digest(b"other");

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::ActualContentProtocolViolation { .. }
    ));
}

#[test]
fn expected_stream_must_match_declared_digest() {
    let declared = b"entry-A";
    let requests = [request("loader.conf", declared)];
    let mut fixture = one_file_fixture(b"loader.conf", declared, b"entry-B");

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::ExpectedContentProtocolViolation { .. }
    ));
}

#[test]
fn stalled_actual_and_expected_streams_are_rejected() {
    let expected = b"entry";
    let requests = [request("loader.conf", expected)];
    let mut actual_stall = one_file_fixture(b"loader.conf", expected, expected);
    actual_stall.regular_files[0] = actual_stall.regular_files[0].clone().with_stall_at(0);
    let actual_error = assess(&requests, &mut actual_stall).unwrap_err();
    assert!(matches!(
        actual_error,
        BootNamespaceAssessmentError::StreamStalled { stream: "actual", .. }
    ));

    let mut expected_stall = one_file_fixture(b"loader.conf", expected, expected);
    expected_stall.expected_streams[0] = expected_stall.expected_streams[0].clone().with_stall_at(0);
    let expected_error = assess(&requests, &mut expected_stall).unwrap_err();
    assert!(matches!(
        expected_error,
        BootNamespaceAssessmentError::StreamStalled { stream: "expected", .. }
    ));
}
