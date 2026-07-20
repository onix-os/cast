const ROOT_ABI_MASK_LINKS: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

// This is the production publisher order, intentionally distinct from the
// stable mask order above.
const ROOT_ABI_PUBLICATION_LINKS: [(&str, &str); 5] = [
    ("sbin", "usr/sbin"),
    ("bin", "usr/bin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib"),
    ("lib32", "usr/lib32"),
];

fn install_root_abi_subset(root: &Path, mask: u8) {
    assert_eq!(mask & !0b1_1111, 0, "root ABI mask must contain exactly five bits");
    for (index, (name, target)) in ROOT_ABI_MASK_LINKS.into_iter().enumerate() {
        if mask & (1 << index) != 0 {
            std::os::unix::fs::symlink(target, root.join(name)).unwrap();
        }
    }
}

fn root_abi_link_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_symlink());
    (metadata.dev(), metadata.ino())
}

fn root_abi_identities(root: &Path) -> Vec<Option<(u64, u64)>> {
    ROOT_ABI_MASK_LINKS
        .into_iter()
        .map(|(name, _)| match fs::symlink_metadata(root.join(name)) {
            Ok(metadata) => {
                assert!(metadata.file_type().is_symlink());
                Some((metadata.dev(), metadata.ino()))
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => panic!("inspect root ABI link: {source}"),
        })
        .collect()
}

fn assert_initial_root_abi_inodes_preserved(root: &Path, before: &[Option<(u64, u64)>]) {
    for ((name, target), expected) in ROOT_ABI_MASK_LINKS.into_iter().zip(before) {
        assert_eq!(fs::read_link(root.join(name)).unwrap(), Path::new(target));
        if let Some(expected) = expected {
            assert_eq!(root_abi_link_identity(&root.join(name)), *expected);
        }
    }
}

fn fixture_with_exchange_authority_and_root_abi_mask(
    candidate_kind: CandidateKind,
    previous_kind: PreviousKind,
    mask: u8,
) -> (CoordinatorFixture, StatefulTreeIdentity, JournalUsrExchangeAuthority) {
    let (fixture, identity, authority) =
        fixture_parts_with_root_abi_mask(candidate_kind, previous_kind, true, false, mask);
    (
        fixture,
        identity,
        authority.expect("root ABI fixture requested pre-journal client authority"),
    )
}

fn coordinator_ready_for_root_abi_publication(
    candidate_kind: CandidateKind,
    mask: u8,
) -> (CoordinatorFixture, UsrExchangedCoordinator) {
    coordinator_ready_for_root_abi_publication_with_previous(candidate_kind, PreviousKind::Active, mask)
}

fn coordinator_ready_for_root_abi_publication_with_previous(
    candidate_kind: CandidateKind,
    previous_kind: PreviousKind,
    mask: u8,
) -> (CoordinatorFixture, UsrExchangedCoordinator) {
    let (fixture, identity, authority) =
        fixture_with_exchange_authority_and_root_abi_mask(candidate_kind, previous_kind, mask);
    let (fixture, intent, authority) =
        coordinator_from_exchange_fixture(candidate_kind, fixture, identity, authority);
    let exchanged = intent.execute_usr_exchange(authority).unwrap();
    (fixture, exchanged)
}

fn expected_root_links_complete_generation(candidate_kind: CandidateKind) -> u64 {
    match candidate_kind {
        CandidateKind::NewState => 10,
        CandidateKind::Archived => 6,
        CandidateKind::ActiveReblit => 8,
    }
}

fn assert_usr_exchanged_source(fixture: &CoordinatorFixture, source: &TransitionRecord) {
    assert_eq!(source.phase, Phase::UsrExchanged);
    assert_eq!(read_canonical(&fixture.installation.root), *source);
}

fn replace_symlink_with_same_target(path: &Path, displaced: &Path) -> ((u64, u64), (u64, u64)) {
    let target = fs::read_link(path).unwrap();
    let original = root_abi_link_identity(path);
    fs::rename(path, displaced).unwrap();
    std::os::unix::fs::symlink(&target, path).unwrap();
    let replacement = root_abi_link_identity(path);
    assert_ne!(original, replacement);
    (original, replacement)
}

fn replace_regular_file_with_same_bytes_at(path: &Path, displaced: &Path) {
    let bytes = fs::read(path).unwrap();
    let mode = fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777;
    fs::rename(path, displaced).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn assert_root_abi_publication_failure(failure: &RootAbiPublicationFailure) {
    assert!(
        matches!(
            failure,
            RootAbiPublicationFailure::Publication { .. }
                | RootAbiPublicationFailure::PostEffectEvidence { .. }
                | RootAbiPublicationFailure::CompletionPersistence { .. }
                | RootAbiPublicationFailure::FinalEvidence { .. }
        ),
        "unexpected root ABI publication failure: {failure:?}"
    );
}
