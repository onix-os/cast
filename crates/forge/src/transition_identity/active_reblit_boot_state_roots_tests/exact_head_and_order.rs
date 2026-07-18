use std::fs;

use super::*;

#[test]
fn mandatory_head_descriptor_is_exactly_bound_to_the_named_live_usr() {
    let fixture = Fixture::new();
    let foreign = fixture.installation.root.join("foreign-head");
    create_exact_tree(&foreign, head_state());
    let foreign_usr = fs::File::open(&foreign).unwrap();

    assert!(matches!(
        PreparedActiveReblitBootStateRoots::prepare(
            &fixture.installation,
            &foreign_usr,
            head_state(),
            &[head_state()],
        ),
        Err(ActiveReblitBootStateRootsError::HeadIdentity { state, .. })
            if state == i32::from(head_state())
    ));
}

#[test]
fn mandatory_head_missing_marker_is_a_hard_failure() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.installation.root.join("usr/.cast-tree-id")).unwrap();

    assert!(matches!(
        fixture.prepare(&[head_state()]),
        Err(ActiveReblitBootStateRootsError::HeadIdentity { state, .. })
            if state == i32::from(head_state())
    ));
}

#[test]
fn eligible_roots_preserve_projected_archive_order_and_kind() {
    let fixture = Fixture::new();
    let first = state(91);
    let second = state(92);
    fixture.create_archive(first);
    fixture.create_archive(second);

    let prepared = fixture.prepare(&[head_state(), second, first]).unwrap();
    assert_eq!(prepared.eligible_state_ids(), &[head_state(), second, first]);
    assert!(prepared.exclusions().is_empty());
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(
        revalidated
            .roots()
            .map(|root| (root.state_id(), root.kind()))
            .collect::<Vec<_>>(),
        vec![
            (head_state(), ActiveReblitBootStateRootKind::LiveHead),
            (second, ActiveReblitBootStateRootKind::Archived),
            (first, ActiveReblitBootStateRootKind::Archived),
        ]
    );
    assert_eq!(revalidated.eligible_state_ids(), &[head_state(), second, first]);
    assert!(revalidated.exclusions().is_empty());
    assert_eq!(revalidated.head().state_id(), head_state());
    assert_eq!(revalidated.head().kind(), ActiveReblitBootStateRootKind::LiveHead);
}
