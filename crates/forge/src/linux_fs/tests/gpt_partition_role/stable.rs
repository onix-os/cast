use crate::linux_fs::gpt_partition_role::GptPartitionRole;

use super::support::{ESP_TYPE_GUID, ESP_UUID, Fixture, XBOOTLDR_TYPE_GUID, XBOOTLDR_UUID, guid_disk_bytes};

#[test]
fn esp_guid_constant_uses_uefi_mixed_endian_disk_bytes() {
    assert_eq!(guid_disk_bytes("c12a7328-f81f-11d2-ba4b-00a0c93ec93b"), ESP_TYPE_GUID);
}

#[test]
fn xbootldr_guid_constant_uses_uefi_mixed_endian_disk_bytes() {
    assert_eq!(
        guid_disk_bytes("bc13c2ff-59e6-4262-a352-b275fd6f7172"),
        XBOOTLDR_TYPE_GUID
    );
}

#[test]
fn stable_512_byte_esp_image_returns_selected_semantics_and_table_identity() {
    let fixture = Fixture::esp(512);
    let selected = fixture.authenticate().unwrap();
    assert_eq!(selected.role(), GptPartitionRole::Esp);
    assert_eq!(selected.partition_number(), 1);
    assert_eq!(selected.partition_uuid(), ESP_UUID);
    assert_eq!(selected.start_lba(), fixture.selected_start_lba);
    assert_eq!(selected.size_lba(), fixture.selected_size_lba);
    assert_eq!(selected.logical_block_size(), fixture.block_size);
    assert_eq!(selected.image_bytes(), fixture.bytes.len() as u64);
    assert_ne!(selected.table_sha256(), &[0; 32]);
}

#[test]
fn stable_4096_byte_xbootldr_image_returns_selected_semantics_and_table_identity() {
    let fixture = Fixture::xbootldr(4_096);
    let selected = fixture.authenticate().unwrap();
    assert_eq!(selected.role(), GptPartitionRole::Xbootldr);
    assert_eq!(selected.partition_number(), 1);
    assert_eq!(selected.partition_uuid(), XBOOTLDR_UUID);
    assert_eq!(selected.start_lba(), fixture.selected_start_lba);
    assert_eq!(selected.size_lba(), fixture.selected_size_lba);
    assert_eq!(selected.logical_block_size(), fixture.block_size);
    assert_eq!(selected.image_bytes(), fixture.bytes.len() as u64);
    assert_ne!(selected.table_sha256(), &[0; 32]);
}

#[test]
fn logical_block_size_endpoints_are_both_admitted() {
    Fixture::esp(512).authenticate().unwrap();
    Fixture::xbootldr(65_536).authenticate().unwrap();
}

#[test]
fn unselected_used_entries_are_validated_without_changing_the_selected_result() {
    let mut fixture = Fixture::esp(512);
    fixture.clear_entry(2);
    fixture.rebuild_arrays_and_headers();
    let selected = fixture.authenticate().unwrap();
    assert_eq!(selected.partition_uuid(), ESP_UUID);
    assert_eq!(selected.size_lba(), fixture.selected_size_lba);
}
