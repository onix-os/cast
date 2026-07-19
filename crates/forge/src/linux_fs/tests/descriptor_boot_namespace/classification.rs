use super::{support::*, *};

#[test]
fn stable_missing_leaf_is_absent() {
    let expected = b"not installed";
    let requests = [request("loader.conf", expected)];
    let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);

    let (assessment, _) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Absent]);
}

#[test]
fn stable_missing_ancestor_marks_nested_request_absent() {
    let expected = b"nested";
    let requests = [request("loader/entries/a.conf", expected)];
    let mut fixture = empty_fixture(vec![FixtureExpectedStream::new(expected)]);

    let (assessment, _) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Absent]);
}

#[test]
fn stable_regular_bytes_are_exact_across_short_reads() {
    let expected = b"title Aeryn\nlinux /EFI/Linux/a.efi\n";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", expected, expected);
    fixture.regular_files[0] = fixture.regular_files[0].clone().with_max_chunk(3);
    fixture.expected_streams[0] = fixture.expected_streams[0].clone().with_max_chunk(2);

    let (assessment, _) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Exact]);
}

#[test]
fn stable_nested_regular_bytes_are_exact() {
    let expected = b"title nested\n";
    let requests = [request("loader/entry.conf", expected)];
    let mut fixture = nested_file_fixture(expected, expected);

    let (assessment, usage) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Exact]);
    assert_eq!(usage.peak_descriptors, 3);
}

#[test]
fn stable_large_regular_crosses_fixed_stream_buffers() {
    let expected = vec![b'x'; 8 * 1024 + 17];
    let requests = [request("uki.efi", &expected)];
    let mut fixture = one_file_fixture(b"uki.efi", &expected, &expected);

    let (assessment, usage) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Exact]);
    assert_eq!(usage.read_bytes, (expected.len() * 2 + 2) as u64);
}

#[test]
fn stable_length_mismatch_is_different() {
    let expected = b"new generation";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", b"old", expected);

    let (assessment, usage) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Different]);
    assert_eq!(usage.read_bytes, 0);
}

#[test]
fn stable_equal_length_byte_mismatch_is_different() {
    let expected = b"generation-B";
    let requests = [request("loader.conf", expected)];
    let mut fixture = one_file_fixture(b"loader.conf", b"generation-A", expected);

    let (assessment, _) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Different]);
}

#[test]
fn shared_trie_preserves_original_request_order() {
    let expected_a = b"alpha";
    let expected_b = b"beta";
    let requests = [request("b.conf", expected_b), request("a.conf", expected_a)];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![
                entry(b"a.conf".to_vec(), FILE_A, BootNamespaceNodeKind::Regular),
                entry(b"b.conf".to_vec(), FILE_B, BootNamespaceNodeKind::Regular),
            ],
        )],
        vec![
            FixtureRegularFile::stable(FILE_A, witness(FILE_A, b"old-a"), b"old-a"),
            FixtureRegularFile::stable(FILE_B, witness(FILE_B, expected_b), expected_b),
        ],
        vec![
            FixtureExpectedStream::new(expected_b),
            FixtureExpectedStream::new(expected_a),
        ],
        std::time::Instant::now(),
    );

    let (assessment, _) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(
        assessment.states(),
        &[
            BootNamespaceDestinationState::Exact,
            BootNamespaceDestinationState::Different,
        ]
    );
}
