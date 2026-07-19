use std::{
    cell::Cell,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::fs::{FileExt as _, PermissionsExt as _, symlink},
    },
    time::{Duration, Instant},
};

use fs_err as fs;
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, FdFlag, SealFlag, fcntl},
    sys::stat::{Mode, fstat},
    unistd::{ftruncate, mkfifo, write as descriptor_write},
};

use super::*;
use crate::test_support::private_installation_tempdir;

struct SnapshotFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    source_path: std::path::PathBuf,
    digest: u128,
    length: u64,
}

fn installation_fixture() -> (tempfile::TempDir, Installation) {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    (temporary, installation)
}

fn write_asset(installation: &Installation, digest: u128, bytes: &[u8]) -> std::path::PathBuf {
    let path = crate::client::cache::asset_path(installation, &format!("{digest:02x}"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    path
}

fn snapshot_fixture(bytes: &[u8]) -> SnapshotFixture {
    let (temporary, installation) = installation_fixture();
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    let source_path = write_asset(&installation, digest, bytes);
    SnapshotFixture {
        _temporary: temporary,
        installation,
        source_path,
        digest,
        length: bytes.len() as u64,
    }
}

fn test_policy(
    max_declarations: usize,
    max_asset_bytes: u64,
    max_total_bytes: u64,
    max_descriptors: usize,
) -> BootAssetSnapshotPolicy {
    BootAssetSnapshotPolicy {
        max_declarations,
        max_asset_bytes,
        max_total_bytes,
        max_descriptors,
        timeout: Duration::from_secs(5),
    }
}

fn test_deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn snapshot_bytes(snapshot: &SealedBootAssetSnapshot) -> Vec<u8> {
    let duplicate = fcntl(snapshot.descriptor().as_raw_fd(), FcntlArg::F_DUPFD_CLOEXEC(0)).unwrap();
    // SAFETY: F_DUPFD_CLOEXEC returned one new descriptor and transfers its
    // ownership exactly once into OwnedFd.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    let file = std::fs::File::from(duplicate);
    let mut bytes = vec![0; snapshot.length() as usize];
    file.read_exact_at(&mut bytes, 0).unwrap();
    bytes
}

#[test]
fn sealed_snapshot_has_exact_bytes_digest_length_metadata_and_seals() {
    let bytes = b"exact boot projection bytes";
    let fixture = snapshot_fixture(bytes);
    let prepared = prepare_digests(&fixture.installation, [fixture.digest]).unwrap();

    assert_eq!(prepared.len(), 1);
    assert!(!prepared.is_empty());
    let snapshot = prepared.snapshots().next().unwrap();
    assert_eq!(snapshot.digest(), fixture.digest);
    assert_eq!(snapshot.length(), fixture.length);
    assert_eq!(snapshot_bytes(snapshot), bytes);
    assert_eq!(
        hex::encode(snapshot.content_identity().as_bytes()),
        "be80b2b6b0fc63358be22d68565a55f12345d0ada9d3ee0e094d10a0b8856b16"
    );

    let descriptor = snapshot.descriptor().as_raw_fd();
    let stat = fstat(descriptor).unwrap();
    assert_eq!(stat.st_mode & nix::libc::S_IFMT, nix::libc::S_IFREG);
    assert_eq!(stat.st_mode & 0o777, SNAPSHOT_MODE);
    assert_eq!(stat.st_size, fixture.length as i64);
    let flags = fcntl(descriptor, FcntlArg::F_GETFD).unwrap();
    assert_ne!(flags & FdFlag::FD_CLOEXEC.bits(), 0);
    let seals = fcntl(descriptor, FcntlArg::F_GET_SEALS).unwrap();
    let required = SealFlag::F_SEAL_WRITE | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_SEAL;
    assert_eq!(seals & required.bits(), required.bits());
}

#[test]
fn sealed_snapshot_rejects_write_shrink_grow_and_additional_seals() {
    let bytes = b"immutable boot input";
    let fixture = snapshot_fixture(bytes);
    let prepared = prepare_digests(&fixture.installation, [fixture.digest]).unwrap();
    let snapshot = prepared.snapshots().next().unwrap();
    let descriptor = snapshot.descriptor();

    assert_eq!(descriptor_write(descriptor.as_raw_fd(), b"x"), Err(Errno::EPERM));
    assert_eq!(ftruncate(descriptor, fixture.length as i64 - 1), Err(Errno::EPERM));
    assert_eq!(ftruncate(descriptor, fixture.length as i64 + 1), Err(Errno::EPERM));
    assert_eq!(
        fcntl(descriptor.as_raw_fd(), FcntlArg::F_ADD_SEALS(SealFlag::F_SEAL_WRITE)),
        Err(Errno::EPERM)
    );
    assert_eq!(snapshot_bytes(snapshot), bytes);
}

#[test]
fn wrong_digest_fails_without_publishing_a_snapshot() {
    let bytes = b"bytes stored under the wrong CAS name";
    let fixture = snapshot_fixture(bytes);
    let wrong_digest = fixture.digest ^ 1;
    fs::rename(
        &fixture.source_path,
        crate::client::cache::asset_path(&fixture.installation, &format!("{wrong_digest:02x}")),
    )
    .unwrap();

    let error = prepare_digests(&fixture.installation, [wrong_digest])
        .err()
        .expect("digest mismatch must reject the complete snapshot set");
    assert!(matches!(
        error,
        BootAssetSnapshotError::CopyAssetSource { digest, .. } if digest == wrong_digest
    ));
}

#[test]
fn count_and_aggregate_byte_limits_admit_n_and_reject_n_plus_one() {
    let (temporary, installation) = installation_fixture();
    let assets =
        [b"aa".as_slice(), b"bbb".as_slice(), b"x".as_slice()].map(|bytes| (xxhash_rust::xxh3::xxh3_128(bytes), bytes));
    for (digest, bytes) in assets {
        write_asset(&installation, digest, bytes);
    }
    let digests = assets.map(|(digest, _)| digest);

    let count_policy = test_policy(2, 16, 64, 32);
    let admitted = prepare_with_policy_until(
        &installation,
        digests[..2].iter().copied(),
        count_policy,
        test_deadline(),
    )
    .unwrap();
    assert_eq!(admitted.len(), 2);
    assert!(matches!(
        prepare_with_policy_until(&installation, digests, count_policy, test_deadline()),
        Err(BootAssetSnapshotError::AssetCountLimit { limit: 2, actual: 3 })
    ));

    let byte_policy = test_policy(3, 16, 5, 32);
    let admitted = prepare_with_policy_until(
        &installation,
        digests[..2].iter().copied(),
        byte_policy,
        test_deadline(),
    )
    .unwrap();
    assert_eq!(admitted.len(), 2);
    assert!(matches!(
        prepare_with_policy_until(&installation, digests, byte_policy, test_deadline()),
        Err(BootAssetSnapshotError::AggregateByteLimit { limit: 5, actual: 6 })
    ));
    drop(temporary);
}

#[test]
fn per_asset_and_descriptor_budgets_fail_before_memfd_allocation() {
    let fixture = snapshot_fixture(b"four");
    assert!(matches!(
        prepare_with_policy_until(
            &fixture.installation,
            [fixture.digest],
            test_policy(1, 3, 64, 32),
            test_deadline(),
        ),
        Err(BootAssetSnapshotError::AssetByteLimit {
            limit: 3,
            actual: 4,
            ..
        })
    ));

    let (_temporary, installation) = installation_fixture();
    assert!(matches!(
        prepare_with_policy_until(
            &installation,
            [fixture.digest],
            test_policy(1, 8, 64, ASSET_POOL_TRANSIENT_DESCRIPTORS + 2),
            test_deadline(),
        ),
        Err(BootAssetSnapshotError::DescriptorLimit { limit, actual })
            if limit == ASSET_POOL_TRANSIENT_DESCRIPTORS + 2
                && actual == ASSET_POOL_TRANSIENT_DESCRIPTORS + 3
    ));
    assert!(!installation.assets_path("v2").exists());
}

#[test]
fn expired_deadline_fails_before_opening_the_asset_pool() {
    let (_temporary, installation) = installation_fixture();
    let digest = xxhash_rust::xxh3::xxh3_128(b"absent source must not be opened");
    let policy = test_policy(1, 64, 64, 32);
    let error = prepare_with_policy_until(
        &installation,
        [digest],
        policy,
        Instant::now() - Duration::from_millis(1),
    )
    .err()
    .expect("expired deadline must win before the absent asset pool is opened");

    assert!(matches!(
        error,
        BootAssetSnapshotError::DeadlineExceeded { timeout } if timeout == policy.timeout
    ));
    assert!(!installation.assets_path("v2").exists());
}

#[test]
fn canonical_empty_asset_is_sealed_without_an_asset_pool() {
    let (_temporary, installation) = installation_fixture();
    assert!(!installation.assets_path("v2").exists());

    let prepared = prepare_digests(&installation, [EMPTY_FILE_DIGEST]).unwrap();
    let snapshot = prepared.snapshots().next().unwrap();
    assert_eq!(snapshot.digest(), EMPTY_FILE_DIGEST);
    assert_eq!(snapshot.length(), 0);
    assert!(snapshot_bytes(snapshot).is_empty());
    assert_eq!(
        hex::encode(snapshot.content_identity().as_bytes()),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(!installation.assets_path("v2").exists());
}

#[test]
fn short_digest_uses_the_descriptor_rooted_flat_asset_path() {
    let (_temporary, installation) = installation_fixture();
    let digest = 1u128;
    let source = write_asset(&installation, digest, b"flat asset path");
    assert_eq!(source.parent().unwrap(), installation.assets_path("v2"));
    assert_eq!(frozen_asset_path(digest), std::path::PathBuf::from("01"));

    let pool = AssetPool::open(&installation).unwrap();
    let asset = pool.open_asset(&frozen_asset_path(digest)).unwrap();
    assert_eq!(asset.witness.length, b"flat asset path".len() as u64);
    require_asset_unchanged_until(&pool, &asset, test_deadline()).unwrap();
}

#[test]
fn fifo_and_symlink_sources_fail_closed_without_blocking() {
    let fixture = snapshot_fixture(b"non-regular source placeholder");
    fs::remove_file(&fixture.source_path).unwrap();
    mkfifo(&fixture.source_path, Mode::from_bits_truncate(0o600)).unwrap();
    let started = Instant::now();
    assert!(matches!(
        prepare_digests(&fixture.installation, [fixture.digest]),
        Err(BootAssetSnapshotError::OpenAssetSource { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(2));

    fs::remove_file(&fixture.source_path).unwrap();
    symlink("missing", &fixture.source_path).unwrap();
    assert!(matches!(
        prepare_digests(&fixture.installation, [fixture.digest]),
        Err(BootAssetSnapshotError::OpenAssetSource { .. })
    ));
}

#[test]
fn source_replacement_after_open_fails_closed() {
    let fixture = snapshot_fixture(b"retained original source bytes");
    let detached = fixture.source_path.with_extension("detached");
    let replaced = Cell::new(false);
    let error = prepare_with_policy_until_and_checkpoint(
        &fixture.installation,
        [fixture.digest],
        test_policy(1, 64, 64, 32),
        test_deadline(),
        |checkpoint| {
            if matches!(checkpoint, AssetSnapshotCheckpoint::SourceOpened { .. }) && !replaced.replace(true) {
                fs::rename(&fixture.source_path, &detached).unwrap();
                fs::write(&fixture.source_path, vec![0x44; fixture.length as usize]).unwrap();
            }
        },
    )
    .err()
    .expect("replacing the public CAS name must reject the snapshot set");

    assert!(matches!(error, BootAssetSnapshotError::RevalidateAssetSource { .. }));
}

#[test]
fn source_mutation_after_copy_fails_closed() {
    let fixture = snapshot_fixture(b"original stable source bytes");
    let mutated = Cell::new(false);
    let error = prepare_with_policy_until_and_checkpoint(
        &fixture.installation,
        [fixture.digest],
        test_policy(1, 64, 64, 32),
        test_deadline(),
        |checkpoint| {
            if matches!(checkpoint, AssetSnapshotCheckpoint::BytesCopied { .. }) && !mutated.replace(true) {
                fs::write(&fixture.source_path, vec![0x55; fixture.length as usize]).unwrap();
            }
        },
    )
    .err()
    .expect("mutating the source after hashing must reject the snapshot set");

    assert!(matches!(error, BootAssetSnapshotError::RevalidateAssetSource { .. }));
}

#[test]
fn failed_batch_drops_prior_snapshots_and_leaves_policy_reusable() {
    let (temporary, installation) = installation_fixture();
    let correct_bytes = b"first valid asset";
    let correct_digest = xxhash_rust::xxh3::xxh3_128(correct_bytes);
    let wrong_digest = u128::MAX;
    write_asset(&installation, correct_digest, correct_bytes);
    write_asset(&installation, wrong_digest, b"content does not match its name");
    let digests = [correct_digest, wrong_digest];
    let policy = test_policy(2, 64, 128, 32);
    let sealed_descriptor = Cell::new(None::<RawFd>);

    let error =
        prepare_with_policy_until_and_checkpoint(&installation, digests, policy, test_deadline(), |checkpoint| {
            if let AssetSnapshotCheckpoint::SnapshotSealed { digest, descriptor } = checkpoint
                && digest == correct_digest
            {
                sealed_descriptor.set(Some(descriptor));
            }
        })
        .err()
        .expect("the second invalid asset must reject the whole batch");
    assert!(matches!(
        error,
        BootAssetSnapshotError::CopyAssetSource { digest, .. } if digest == wrong_digest
    ));
    let dropped = sealed_descriptor
        .get()
        .expect("the first snapshot must have been sealed");
    assert_eq!(fcntl(dropped, FcntlArg::F_GETFD), Err(Errno::EBADF));

    let retried =
        prepare_with_policy_until(&installation, digests[..1].iter().copied(), policy, test_deadline()).unwrap();
    assert_eq!(
        retried.len(),
        1,
        "a failed batch must not consume a later attempt's budget"
    );
    drop(temporary);
}

#[test]
fn duplicate_digest_is_canonicalized_without_a_duplicate_snapshot() {
    let fixture = snapshot_fixture(b"shared boot asset");
    let prepared = prepare_with_policy_until(
        &fixture.installation,
        [fixture.digest, fixture.digest],
        test_policy(2, 64, 64, 32),
        test_deadline(),
    )
    .unwrap();
    assert_eq!(prepared.len(), 1);
}

#[test]
fn digest_lookup_is_sorted_deduplicated_and_independent_of_descriptor_offsets() {
    let (_temporary, installation) = installation_fixture();
    let first_bytes = b"first lookup bytes";
    let second_bytes = b"second lookup bytes";
    let first = xxhash_rust::xxh3::xxh3_128(first_bytes);
    let second = xxhash_rust::xxh3::xxh3_128(second_bytes);
    write_asset(&installation, first, first_bytes);
    write_asset(&installation, second, second_bytes);

    let prepared = prepare_with_policy_until(
        &installation,
        [second, first, second],
        test_policy(3, 64, 128, 32),
        test_deadline(),
    )
    .unwrap();

    assert_eq!(prepared.len(), 2);
    assert_eq!(snapshot_bytes(prepared.snapshot_for(first).unwrap()), first_bytes);
    assert_eq!(snapshot_bytes(prepared.snapshot_for(second).unwrap()), second_bytes);
    assert!(prepared.snapshot_for(first ^ second).is_none());
    assert_eq!(snapshot_bytes(prepared.snapshot_for(first).unwrap()), first_bytes);
}

#[test]
fn source_growth_after_length_preflight_fails_before_snapshot_publication() {
    let fixture = snapshot_fixture(b"authenticated length witness");
    let sealed = Cell::new(false);
    let error = prepare_with_policy_until_and_checkpoint(
        &fixture.installation,
        [fixture.digest],
        test_policy(1, 64, 64, 32),
        test_deadline(),
        |checkpoint| {
            if matches!(checkpoint, AssetSnapshotCheckpoint::SourceOpened { .. }) {
                let mut longer = fs::read(&fixture.source_path).unwrap();
                longer.push(0xff);
                fs::write(&fixture.source_path, longer).unwrap();
            }
            if matches!(checkpoint, AssetSnapshotCheckpoint::SnapshotSealed { .. }) {
                sealed.set(true);
            }
        },
    )
    .err()
    .expect("growth after the authenticated length witness must reject the snapshot set");

    assert!(matches!(
        error,
        BootAssetSnapshotError::CopyAssetSource { digest, .. } if digest == fixture.digest
    ));
    assert!(!sealed.get());
}

#[test]
fn materialization_timeout_maps_to_the_boot_snapshot_deadline() {
    let timeout = Duration::from_secs(7);
    let error = map_copy_error(
        1,
        ClientError::FrozenMaterializationTimeout { seconds: 600 },
        timeout,
        test_deadline(),
    );
    assert!(matches!(
        error,
        BootAssetSnapshotError::DeadlineExceeded { timeout: actual } if actual == timeout
    ));
}
