use super::*;

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

use crate::test_support::private_installation_tempdir;

#[test]
fn reverse_exchange_parent_rebind_uses_only_value_identity() {
    let root = witness(10);
    let staging = witness(30);
    let identity = ReverseExchangeParentIdentity::from_witnesses(root, staging).unwrap();

    let mut rebound_root = root;
    rebound_root.modified_seconds += 1;
    rebound_root.changed_nanoseconds += 1;
    let mut rebound_staging = staging;
    rebound_staging.modified_nanoseconds += 1;
    rebound_staging.changed_seconds += 1;
    assert_eq!(
        identity.require_rebound(rebound_root, rebound_staging).unwrap(),
        identity
    );

    let mut foreign_root = root;
    foreign_root.inode += 1;
    assert!(matches!(
        identity.require_rebound(foreign_root, staging),
        Err(ReverseExchangeCaptureError::ParentIdentityChanged)
    ));

    let mut foreign_staging = staging;
    foreign_staging.mode ^= 0o100;
    assert!(matches!(
        identity.require_rebound(root, foreign_staging),
        Err(ReverseExchangeCaptureError::ParentIdentityChanged)
    ));
}

#[test]
fn reverse_exchange_parent_rebind_rejects_cross_device_pairs() {
    let root = witness(10);
    let mut staging = witness(30);
    staging.device += 1;
    assert!(matches!(
        ReverseExchangeParentIdentity::from_witnesses(root, staging),
        Err(ReverseExchangeCaptureError::ParentsCrossDevice { .. })
    ));
}

#[test]
fn retained_reverse_exchange_parents_rebind_both_exact_public_names() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let holder = parent_holder(&installation);
    let expected = holder.identity();
    assert_eq!(holder.revalidate_value_identity(&installation).unwrap(), expected);

    let staging = temporary.path().join(".cast/root/staging");
    let detached = temporary.path().join(".cast/root/detached-staging");
    fs::rename(&staging, &detached).unwrap();
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(holder.revalidate_value_identity(&installation).is_err());
    assert_eq!(
        InodeWitness::read(&holder.staging, &detached).unwrap().inode,
        expected.staging.inode
    );
    assert_ne!(fs::metadata(&staging).unwrap().ino(), expected.staging.inode);
}

fn parent_holder(installation: &Installation) -> RetainedReverseExchangeParents {
    let root_path = installation.root.clone();
    let roots_path = root_path.join(".cast/root");
    let staging_path = roots_path.join("staging");
    let mut budget = Budget::new().unwrap();
    let root = open_directory(installation.root_directory(), c".", &root_path, &mut budget).unwrap();
    let roots = open_directory(&root, c".cast/root", &roots_path, &mut budget).unwrap();
    let staging = open_directory(&roots, c"staging", &staging_path, &mut budget).unwrap();
    let root_witness = controlled_directory_witness(&root, &root_path).unwrap();
    let roots_witness = controlled_directory_witness(&roots, &roots_path).unwrap();
    let staging_witness = controlled_directory_witness(&staging, &staging_path).unwrap();
    RetainedReverseExchangeParents {
        root,
        roots,
        staging,
        root_path,
        roots_path,
        staging_path,
        roots_witness,
        identity: ReverseExchangeParentIdentity::from_witnesses(root_witness, staging_witness).unwrap(),
    }
}
