//! Alias-isolation and crash-residue proofs for archived-repair materialization.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use fs_err as fs;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::client::{archived_repair_materialization::arm_before_staging_baseline_revalidation, cache};

const CACHED_BYTES: &[u8] = b"persistent archived-repair asset bytes";

#[test]
fn the_public_low_level_blitter_rejects_fixed_staging_unchanged() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    let before = directory_identity(&staging);
    let tree = crate::client::vfs(Vec::new()).unwrap();

    let result = crate::client::blit_root(&fixture.client.installation, &tree, &staging);

    assert!(matches!(result, Err(Error::EphemeralInstallationRoot)));
    assert_eq!(directory_identity(&staging), before);
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn archived_repair_materialization_uses_a_private_inode_and_never_chmods_the_asset_pool() {
    let fixture = Fixture::new(false);
    let package = package::Id::from("archived-independent-copy");
    let (asset, candidate_path) = add_cached_regular(&fixture, &package, "bin/private-tool", 0o755);
    let live_alias = fixture.client.installation.root.join("usr/live-asset-alias");
    fs::hard_link(&asset, &live_alias).unwrap();
    let asset_before = regular_witness(&asset);
    let live_before = regular_witness(&live_alias);

    let candidate = fixture.client.materialize_archived_repair_root([&package]).unwrap();
    let copied = regular_witness(&candidate_path);

    assert_eq!(copied.links, 1);
    assert_ne!((copied.device, copied.inode), (asset_before.device, asset_before.inode));
    assert_eq!(copied.mode, 0o755);
    assert_eq!(fs::read(&candidate_path).unwrap(), CACHED_BYTES);
    assert_eq!(regular_witness(&asset), asset_before);
    assert_eq!(regular_witness(&live_alias), live_before);
    assert_eq!(fs::read(&asset).unwrap(), CACHED_BYTES);
    assert_eq!(fs::read(&live_alias).unwrap(), CACHED_BYTES);

    drop(candidate);
}

#[test]
fn a_write_at_the_transaction_trigger_boundary_cannot_mutate_cache_or_live_aliases() {
    let fixture = Fixture::new(false);
    let package = package::Id::from("archived-trigger-write-isolation");
    let (asset, candidate_path) = add_cached_regular(&fixture, &package, "share/repair/payload", 0o644);
    let live_alias = fixture.client.installation.root.join("usr/live-trigger-alias");
    fs::hard_link(&asset, &live_alias).unwrap();
    let asset_before = regular_witness(&asset);
    let live_before = regular_witness(&live_alias);
    let archived_payload = fixture.archived_root.join("usr/share/repair/payload");

    fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.client.materialize_archived_repair_root([&package]).unwrap(),
            &fixture.repaired,
            fixture.snapshot("archived-trigger-write-isolation"),
            |point| {
                if point == ArchivedRepairCheckpoint::BeforeTransactionTriggers {
                    // This checkpoint has the same writable candidate inode
                    // that retained transaction triggers receive. Mutate both
                    // bytes and mode to prove neither operation reaches an
                    // asset-pool/live hardlink alias.
                    fs::write(&candidate_path, b"transaction-trigger mutation").unwrap();
                    fs::set_permissions(&candidate_path, std::fs::Permissions::from_mode(0o600)).unwrap();
                }
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(fs::read(&archived_payload).unwrap(), b"transaction-trigger mutation");
    assert_eq!(regular_witness(&archived_payload).mode, 0o600);
    assert_eq!(regular_witness(&asset), asset_before);
    assert_eq!(regular_witness(&live_alias), live_before);
    assert_eq!(fs::read(&asset).unwrap(), CACHED_BYTES);
    assert_eq!(fs::read(&live_alias).unwrap(), CACHED_BYTES);
}

#[test]
fn identical_digest_outputs_keep_distinct_package_modes_without_chmodding_aliases() {
    let fixture = Fixture::new(false);
    let library_package = package::Id::from("archived-shared-digest-library");
    let executable_package = package::Id::from("archived-shared-digest-executable");
    let (asset, library_path) =
        add_cached_regular(&fixture, &library_package, "share/repair/shared-library-mode", 0o644);
    let (_, executable_path) = add_cached_regular(&fixture, &executable_package, "bin/shared-executable-mode", 0o755);
    let live_alias = fixture.client.installation.root.join("usr/live-shared-digest-alias");
    fs::hard_link(&asset, &live_alias).unwrap();
    let asset_before = regular_witness(&asset);
    let live_before = regular_witness(&live_alias);

    let candidate = fixture
        .client
        .materialize_archived_repair_root([&library_package, &executable_package])
        .unwrap();
    let library = regular_witness(&library_path);
    let executable = regular_witness(&executable_path);

    assert_eq!(library.links, 1);
    assert_eq!(executable.links, 1);
    assert_eq!(library.mode, 0o644);
    assert_eq!(executable.mode, 0o755);
    assert_ne!((library.device, library.inode), (executable.device, executable.inode));
    assert_ne!(
        (library.device, library.inode),
        (asset_before.device, asset_before.inode)
    );
    assert_ne!(
        (executable.device, executable.inode),
        (asset_before.device, asset_before.inode)
    );
    assert_eq!(fs::read(&library_path).unwrap(), CACHED_BYTES);
    assert_eq!(fs::read(&executable_path).unwrap(), CACHED_BYTES);
    assert_eq!(regular_witness(&asset), asset_before);
    assert_eq!(regular_witness(&live_alias), live_before);
    assert_eq!(fs::read(&asset).unwrap(), CACHED_BYTES);
    assert_eq!(fs::read(&live_alias).unwrap(), CACHED_BYTES);

    drop(candidate);
}

#[test]
fn nonempty_fixed_staging_crash_residue_is_refused_without_traversal_or_deletion() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    assert_exact_empty_private_staging(&staging);
    let residue = staging.join("retained-crash-residue");
    fs::write(&residue, b"must survive failed preflight").unwrap();
    let staging_before = directory_identity(&staging);
    let residue_before = regular_witness(&residue);

    let result = fixture
        .client
        .materialize_archived_repair_root(std::iter::empty::<&package::Id>());
    assert!(matches!(result, Err(Error::ArchivedRepairMaterialization { .. })));

    assert_eq!(directory_identity(&staging), staging_before);
    assert_eq!(regular_witness(&residue), residue_before);
    assert_eq!(fs::read(&residue).unwrap(), b"must survive failed preflight");
    assert_eq!(read_entry_names(&staging), [OsString::from("retained-crash-residue")]);
}

#[test]
fn an_exact_empty_private_staging_baseline_is_reused_without_replacement() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    assert_exact_empty_private_staging(&staging);
    let before = directory_identity(&staging);

    let candidate = fixture
        .client
        .materialize_archived_repair_root(std::iter::empty::<&package::Id>())
        .unwrap();

    assert_eq!(directory_identity(&staging), before);
    assert_eq!(staging.metadata().unwrap().permissions().mode() & 0o7777, 0o700);
    assert_eq!(read_entry_names(&staging), [OsString::from("usr")]);
    assert_eq!(
        staging.join("usr").metadata().unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert!(read_entry_names(&staging.join("usr")).is_empty());
    drop(candidate);
}

#[test]
fn an_empty_staging_name_substitution_is_refused_without_removing_either_inode() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    let detached = fixture.client.installation.root_path("detached-empty-staging");
    let retained = directory_identity(&staging);
    let hook_staging = staging.clone();
    let hook_detached = detached.clone();
    arm_before_staging_baseline_revalidation(move || {
        fs::rename(&hook_staging, &hook_detached).unwrap();
        fs::create_dir(&hook_staging).unwrap();
        fs::set_permissions(&hook_staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    });

    let result = fixture
        .client
        .materialize_archived_repair_root(std::iter::empty::<&package::Id>());
    assert!(matches!(result, Err(Error::ArchivedRepairMaterialization { .. })));

    assert_eq!(directory_identity(&detached), retained);
    assert_ne!(directory_identity(&staging), retained);
    assert_exact_empty_private_staging(&staging);
    assert!(read_entry_names(&detached).is_empty());
}

fn add_cached_regular(fixture: &Fixture, package: &package::Id, path: &str, mode: u32) -> (PathBuf, PathBuf) {
    let digest = xxhash_rust::xxh3::xxh3_128(CACHED_BYTES);
    let asset = cache::asset_path(&fixture.client.installation, &format!("{digest:02x}"));
    fs::create_dir_all(asset.parent().unwrap()).unwrap();
    fs::write(&asset, CACHED_BYTES).unwrap();
    fs::set_permissions(&asset, std::fs::Permissions::from_mode(0o640)).unwrap();
    fixture
        .client
        .layout_db
        .add(
            package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | mode,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(digest, path.into()),
            },
        )
        .unwrap();

    (asset, fixture.client.installation.staging_path("usr").join(path))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegularWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
}

fn regular_witness(path: &Path) -> RegularWitness {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_file());
    RegularWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
    }
}
