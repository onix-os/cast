use super::*;

#[test]
fn reverse_exchange_projection_rejects_token_and_staging_substitution() {
    let mut candidate = fingerprint(UsrExchangeLayout::Post);
    candidate.live.token = "substituted-candidate".to_owned();
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&candidate, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::TreeCount {
            role: "candidate",
            actual: 0
        })
    ));

    let mut previous = fingerprint(UsrExchangeLayout::Post);
    staging_mut(&mut previous).usr.as_mut().unwrap().token = "substituted-previous".to_owned();
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&previous, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::TreeCount {
            role: "previous",
            actual: 0
        })
    ));

    let mut staging_role = fingerprint(UsrExchangeLayout::Post);
    staging_mut(&mut staging_role).role = TreeLocation::AmbientQuarantine(b"staging".to_vec());
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&staging_role, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::InvalidStagingRole)
    ));

    let mut staging_tree = fingerprint(UsrExchangeLayout::Post);
    staging_mut(&mut staging_tree).usr.as_mut().unwrap().location = TreeLocation::Live;
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&staging_tree, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::InvalidFixedLocations)
    ));
}

#[test]
fn reverse_exchange_projection_rejects_tree_metadata_and_state_changes() {
    let before = project(&fingerprint(UsrExchangeLayout::Post));

    let mut runtime = fingerprint(UsrExchangeLayout::Pre);
    tree_for_token_mut(&mut runtime, CANDIDATE_TOKEN).runtime.mount_id += 1;
    assert_invariant_changed(&before, &runtime);

    let mut state_id = fingerprint(UsrExchangeLayout::Pre);
    tree_for_token_mut(&mut state_id, CANDIDATE_TOKEN).state_id = StateIdFingerprint::Canonical {
        state: 42,
        witness: witness(500),
        bytes: b"42".to_vec(),
    };
    assert_invariant_changed(&before, &state_id);

    let mut marker = fingerprint(UsrExchangeLayout::Pre);
    tree_for_token_mut(&mut marker, PREVIOUS_TOKEN)
        .marker
        .changed_nanoseconds += 1;
    assert_invariant_changed(&before, &marker);

    let mut directory = fingerprint(UsrExchangeLayout::Pre);
    tree_for_token_mut(&mut directory, PREVIOUS_TOKEN).directory.links += 1;
    let staged = staging_mut(&mut directory);
    if staged.usr.as_ref().unwrap().token == PREVIOUS_TOKEN {
        staged.entries[0].1 = staged.usr.as_ref().unwrap().directory;
    }
    assert_invariant_changed(&before, &directory);
}

#[test]
fn reverse_exchange_projection_rejects_abi_wrapper_epoch_and_anchor_changes() {
    let before = project(&fingerprint(UsrExchangeLayout::Post));

    let mut roots = fingerprint(UsrExchangeLayout::Pre);
    roots.roots.changed_seconds += 1;
    assert_invariant_changed(&before, &roots);

    let mut quarantine = fingerprint(UsrExchangeLayout::Pre);
    quarantine.quarantine.changed_nanoseconds += 1;
    assert_invariant_changed(&before, &quarantine);

    let mut staging_parent = fingerprint(UsrExchangeLayout::Pre);
    staging_mut(&mut staging_parent).witness.owner += 1;
    assert_invariant_changed(&before, &staging_parent);

    let mut root_abi = fingerprint(UsrExchangeLayout::Pre);
    root_abi.root_abi.links.push(Some(RootAbiLinkFingerprint {
        name: b"bin".to_vec(),
        target: b"usr/bin".to_vec(),
        witness: witness(600),
    }));
    assert_invariant_changed(&before, &root_abi);

    let mut isolation_abi = fingerprint(UsrExchangeLayout::Pre);
    isolation_abi.isolation_abi.links.push(Some(RootAbiLinkFingerprint {
        name: b"lib".to_vec(),
        target: b"usr/lib".to_vec(),
        witness: witness(601),
    }));
    assert_invariant_changed(&before, &isolation_abi);

    let mut other_wrapper = fingerprint(UsrExchangeLayout::Pre);
    other_wrapper
        .roots_entries
        .iter_mut()
        .find(|wrapper| wrapper.name == b"isolation")
        .unwrap()
        .witness
        .mode ^= 0o100;
    assert_invariant_changed(&before, &other_wrapper);

    let mut epoch = fingerprint(UsrExchangeLayout::Pre);
    epoch.epoch.mount_namespace.inode += 1;
    assert_invariant_changed(&before, &epoch);
}

fn assert_invariant_changed(before: &ProjectedReverseNamespace, after: &NamespaceFingerprint) {
    assert!(matches!(
        before.require_post_to_pre(&project(after)),
        Err(ReverseExchangeCaptureError::InvariantChanged)
    ));
}
