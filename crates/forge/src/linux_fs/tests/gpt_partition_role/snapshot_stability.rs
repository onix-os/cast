use std::io;

use crate::linux_fs::gpt_partition_role::{
    FixtureGptPartitionRoleLimits, GptPartitionRole, authenticate_gpt_partition_role_image_until,
    authenticate_gpt_partition_role_two_image_fixture_until,
};

use super::support::{
    ESP_UUID, Fixture, SECOND_UUID, XBOOTLDR_TYPE_GUID, XBOOTLDR_UUID, guid_disk_bytes, live_deadline,
};

#[test]
fn stable_512_and_4096_tables_retain_deterministic_nonzero_fingerprints() {
    for fixture in [Fixture::esp(512), Fixture::xbootldr(4_096)] {
        let first = fixture.authenticate().unwrap();
        let second = fixture.authenticate().unwrap();
        assert_eq!(first.table_sha256(), second.table_sha256());
        assert_ne!(first.table_sha256(), &[0; 32]);
    }
}

#[test]
fn stable_512_esp_table_fingerprint_v1_is_pinned() {
    let fixture = Fixture::esp(512);
    assert_eq!(
        fixture.authenticate().unwrap().table_sha256(),
        &[
            0x02, 0x59, 0x9b, 0xb8, 0x5a, 0x00, 0x76, 0xf4, 0x57, 0xea, 0x14, 0xbb, 0x0c, 0xa1, 0x41, 0xb3, 0x83, 0x1a,
            0x2d, 0x2b, 0xee, 0xf2, 0xf1, 0xb9, 0x76, 0x65, 0x4d, 0x0a, 0x8b, 0x0a, 0x09, 0x04,
        ]
    );
}

#[test]
fn a_valid_unselected_entry_change_is_rejected_between_passes() {
    let first = Fixture::esp(512);
    let mut second = Fixture::esp(512);
    second.write_entry(
        2,
        [0x6a; 16],
        guid_disk_bytes(SECOND_UUID),
        second.selected_start_lba + 64,
        second.selected_start_lba + 95,
    );
    second.rebuild_arrays_and_headers();

    let first_alone = first.authenticate().unwrap();
    let second_alone = second.authenticate().unwrap();
    assert_eq!(first_alone.partition_uuid(), second_alone.partition_uuid());
    assert_eq!(first_alone.start_lba(), second_alone.start_lba());
    assert_ne!(first_alone.table_sha256(), second_alone.table_sha256());

    let error = authenticate_gpt_partition_role_two_image_fixture_until(
        &first.bytes,
        &second.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        live_deadline(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_valid_disk_guid_change_is_rejected_between_4096_byte_passes() {
    let first = Fixture::esp(4_096);
    let mut second = Fixture::esp(4_096);
    second.set_disk_guid([0x4d; 16]);

    let first_alone = first.authenticate().unwrap();
    let second_alone = second.authenticate().unwrap();
    assert_eq!(first_alone.partition_uuid(), second_alone.partition_uuid());
    assert_ne!(first_alone.table_sha256(), second_alone.table_sha256());

    let error = authenticate_gpt_partition_role_two_image_fixture_until(
        &first.bytes,
        &second.bytes,
        4_096,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        live_deadline(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn one_exact_table_has_one_fingerprint_independent_of_selected_role() {
    let mut fixture = Fixture::esp(512);
    fixture.write_entry(
        2,
        XBOOTLDR_TYPE_GUID,
        guid_disk_bytes(XBOOTLDR_UUID),
        fixture.selected_start_lba + 64,
        fixture.selected_start_lba + 95,
    );
    fixture.rebuild_arrays_and_headers();

    let esp = fixture.authenticate().unwrap();
    let xbootldr = authenticate_gpt_partition_role_image_until(
        &fixture.bytes,
        512,
        2,
        XBOOTLDR_UUID,
        GptPartitionRole::Xbootldr,
        live_deadline(),
    )
    .unwrap();
    assert_eq!(esp.table_sha256(), xbootldr.table_sha256());
}
