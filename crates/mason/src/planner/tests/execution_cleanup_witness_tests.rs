use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
};

use fs_err as fs;

use super::bootstrap::AssetInventory;

#[test]
fn complete_asset_inventory_is_stable_and_detects_a_nonfirst_file_mutation() {
    let temporary = crate::private_tempdir();
    let root = temporary.path().join("assets/v2");
    fs::create_dir_all(root.join("00/00/00")).unwrap();
    fs::create_dir_all(root.join("ff/ff/ff")).unwrap();
    fs::write(root.join("00/00/00/first"), b"first").unwrap();
    fs::write(root.join("ff/ff/ff/last"), b"last").unwrap();

    let before = AssetInventory::capture("inventory-test", &root);
    assert_eq!(AssetInventory::capture("inventory-test", &root), before);

    fs::write(root.join("ff/ff/ff/last"), b"changed-last").unwrap();
    assert_ne!(AssetInventory::capture("inventory-test", &root), before);
}

#[test]
fn complete_asset_inventory_rejects_symlinks_without_touching_their_targets() {
    let temporary = crate::private_tempdir();
    let outside = crate::private_tempdir();
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"outside").unwrap();
    let root = temporary.path().join("assets/v2");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("asset"), b"asset").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("foreign")).unwrap();

    let result = catch_unwind(AssertUnwindSafe(|| {
        AssetInventory::capture("inventory-test", &root)
    }));

    assert!(result.is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"outside");
}

#[test]
fn complete_asset_inventory_enforces_its_depth_boundary() {
    let temporary = crate::private_tempdir();
    let root = temporary.path().join("assets/v2");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("asset"), b"asset").unwrap();
    let mut deepest = PathBuf::new();
    for _ in 0..129 {
        deepest.push("d");
    }
    fs::create_dir_all(root.join(deepest)).unwrap();

    let result = catch_unwind(AssertUnwindSafe(|| {
        AssetInventory::capture("inventory-test", &root)
    }));

    assert!(result.is_err());
}
