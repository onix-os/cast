use super::*;

#[test]
fn canonical_path_split_preserves_exact_parent_order_and_leaf() {
    let path = split_bound_replacement_path(
        "EFI/Linux/xxh3-0123456789abcdef-l0000000000000001/vmlinuz",
        7,
    )
    .unwrap();

    assert_eq!(
        path.parents(),
        ["EFI", "Linux", "xxh3-0123456789abcdef-l0000000000000001"],
    );
    assert_eq!(path.leaf, "vmlinuz");
}

#[test]
fn malformed_root_only_and_overdeep_paths_fail_before_effects() {
    assert!(matches!(
        split_bound_replacement_path("loader.conf", 2),
        Err(ActiveReblitBootOwnedLeafReplacementError::MissingPublicationParent {
            plan_index: 2,
        }),
    ));
    for malformed in ["/loader/loader.conf", "loader//loader.conf", "../loader.conf"] {
        assert!(matches!(
            split_bound_replacement_path(malformed, 3),
            Err(ActiveReblitBootOwnedLeafReplacementError::InvalidPathComponent {
                plan_index: 3,
            }),
        ));
    }
    let overdeep = std::iter::repeat_n("parent", 16)
        .chain(std::iter::once("leaf"))
        .collect::<Vec<_>>()
        .join("/");
    assert!(matches!(
        split_bound_replacement_path(&overdeep, 4),
        Err(ActiveReblitBootOwnedLeafReplacementError::PublicationParentDepth {
            plan_index: 4,
        }),
    ));
}
