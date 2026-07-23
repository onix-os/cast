use super::*;

#[test]
fn reverse_exchange_projection_accepts_only_exact_semantic_post_to_pre_movement() {
    let before = fingerprint(UsrExchangeLayout::Post);
    let mut after = fingerprint(UsrExchangeLayout::Pre);
    allow_exchange_timestamps(&mut after);

    let before = project(&before);
    let after = project(&after);
    assert_eq!(before.layout(), UsrExchangeLayout::Post);
    assert_eq!(after.layout(), UsrExchangeLayout::Pre);
    before.require_post_to_pre(&after).unwrap();
}

#[test]
fn reverse_exchange_projection_rejects_every_nonexchange_delta() {
    let before_fingerprint = fingerprint(UsrExchangeLayout::Post);
    let before = project(&before_fingerprint);

    let mut parent = fingerprint(UsrExchangeLayout::Pre);
    parent.root.mode ^= 0o100;
    assert!(matches!(
        before.require_post_to_pre(&project(&parent)),
        Err(ReverseExchangeCaptureError::InvariantChanged)
    ));

    let mut moved_tree = fingerprint(UsrExchangeLayout::Pre);
    moved_tree.live.directory.modified_seconds += 1;
    assert!(matches!(
        before.require_post_to_pre(&project(&moved_tree)),
        Err(ReverseExchangeCaptureError::InvariantChanged)
    ));

    let mut marker = fingerprint(UsrExchangeLayout::Pre);
    marker.live.marker.inode += 1;
    assert!(matches!(
        before.require_post_to_pre(&project(&marker)),
        Err(ReverseExchangeCaptureError::InvariantChanged)
    ));

    let mut ambient = fingerprint(UsrExchangeLayout::Pre);
    ambient.quarantine_entries.push(wrapper(b"added", 42));
    assert!(matches!(
        before.require_post_to_pre(&project(&ambient)),
        Err(ReverseExchangeCaptureError::InvariantChanged)
    ));

    let after = project(&fingerprint(UsrExchangeLayout::Pre));
    assert!(matches!(
        after.require_post_to_pre(&before),
        Err(ReverseExchangeCaptureError::NotPostToPre { .. })
    ));
}

#[test]
fn reverse_exchange_projection_requires_unique_tokens_and_exact_staging_shape() {
    let mut duplicate = fingerprint(UsrExchangeLayout::Post);
    duplicate.quarantine_entries.push(WrapperFingerprint {
        name: b"duplicate".to_vec(),
        witness: witness(42),
        role: TreeLocation::AmbientQuarantine(b"duplicate".to_vec()),
        entries: vec![(
            b"usr".to_vec(),
            usr(
                CANDIDATE_TOKEN,
                TreeLocation::AmbientQuarantine(b"duplicate".to_vec()),
                300,
            )
            .directory,
        )],
        usr: Some(usr(
            CANDIDATE_TOKEN,
            TreeLocation::AmbientQuarantine(b"duplicate".to_vec()),
            300,
        )),
        slot: None,
    });
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&duplicate, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::TreeCount {
            role: "candidate",
            actual: 2
        })
    ));

    let mut malformed = fingerprint(UsrExchangeLayout::Post);
    staging_mut(&mut malformed)
        .entries
        .push((b"extra".to_vec(), witness(301)));
    assert!(matches!(
        ProjectedReverseNamespace::from_fingerprint(&malformed, CANDIDATE_TOKEN, PREVIOUS_TOKEN),
        Err(ReverseExchangeCaptureError::InvalidStagingShape)
    ));

    let post = project(&fingerprint(UsrExchangeLayout::Post));
    assert_eq!(staging(&fingerprint(UsrExchangeLayout::Post)).name, b"staging");
    assert_eq!(post.layout(), UsrExchangeLayout::Post);
}
