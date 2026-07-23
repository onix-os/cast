use std::time::{Duration, Instant};

use super::super::super::gpt_partition_role::GptPartitionRole;
use super::support::{
    FixtureInput, FixtureObserver, ObservationFields, PRODUCTION_LIMITS, TABLE_HASH, UUID, authenticate,
};

#[test]
fn stable_read_only_parent_retains_only_exact_closed_scalars() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    let authenticated = authenticate(&mut observer, FixtureInput::standard(), PRODUCTION_LIMITS, deadline).unwrap();

    assert_eq!(observer.calls(), 2);
    assert_eq!(authenticated.containing_device(), 41);
    assert_eq!(authenticated.inode(), 52);
    assert_eq!(authenticated.mount_id(), 63);
    assert_eq!((authenticated.parent_major(), authenticated.parent_minor()), (8, 0));
    assert_eq!(authenticated.logical_block_size(), 512);
    assert_eq!(authenticated.device_byte_length(), 64 * 1024 * 1024);
    assert_eq!(authenticated.partition_number(), 1);
    assert_eq!(authenticated.partition_uuid(), UUID);
    assert_eq!(authenticated.partition_start_bytes(), 2_048 * 512);
    assert_eq!(authenticated.partition_size_bytes(), 4_096 * 512);
    assert_eq!(authenticated.role(), GptPartitionRole::Esp);
    assert_eq!(authenticated.table_sha256(), &TABLE_HASH);
}

#[test]
fn four_kib_logical_blocks_reconcile_with_fixed_512_sector_sysfs_units() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut fields = ObservationFields::standard();
    fields.logical_block_size = 4_096;
    let mut input = FixtureInput::standard();
    input.start_lba = 256;
    input.size_lba = 512;
    input.validated_logical_block_size = 4_096;
    input.role = GptPartitionRole::Xbootldr;
    let mut observer = FixtureObserver::stable(fields);

    let authenticated = authenticate(&mut observer, input, PRODUCTION_LIMITS, deadline).unwrap();
    assert_eq!(authenticated.logical_block_size(), 4_096);
    assert_eq!(authenticated.partition_start_bytes(), 1_048_576);
    assert_eq!(authenticated.partition_size_bytes(), 2_097_152);
    assert_eq!(authenticated.role(), GptPartitionRole::Xbootldr);
}
