use std::io;

use crate::linux_fs::gpt_partition_role::{GptPartitionRole, authenticate_gpt_partition_role_image_until};

use super::support::{
    ESP_TYPE_GUID, ESP_UUID, Fixture, SECOND_UUID, XBOOTLDR_UUID, change_bytes_without_changing_crc, guid_disk_bytes,
    live_deadline,
};

fn invalid_data(error: io::Error) {
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn logical_block_size_and_image_length_are_strict() {
    let fixture = Fixture::esp(512);
    for size in [0, 256, 768, 131_072] {
        let error = authenticate_gpt_partition_role_image_until(
            &fixture.bytes,
            size,
            1,
            ESP_UUID,
            GptPartitionRole::Esp,
            live_deadline(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    let mut bytes = fixture.bytes.clone();
    bytes.push(0);
    invalid_data(
        authenticate_gpt_partition_role_image_until(&bytes, 512, 1, ESP_UUID, GptPartitionRole::Esp, live_deadline())
            .unwrap_err(),
    );
}

#[test]
fn protective_mbr_must_be_exact_and_non_hybrid() {
    for offset in [440, 444, 446, 447, 450, 454, 458, 462, 510] {
        let mut fixture = Fixture::esp(512);
        fixture.bytes[offset] ^= 1;
        invalid_data(fixture.authenticate().unwrap_err());
    }

    let mut padded = Fixture::esp(4_096);
    padded.bytes[512] = 1;
    invalid_data(padded.authenticate().unwrap_err());
}

#[test]
fn both_headers_require_exact_profile_fields_and_crc() {
    for primary in [true, false] {
        for relative in [0, 8, 12, 16, 20, 92] {
            let mut fixture = Fixture::esp(512);
            let base = if primary {
                fixture.primary_header_offset()
            } else {
                fixture.backup_header_offset()
            };
            fixture.bytes[base + relative] ^= 1;
            invalid_data(fixture.authenticate().unwrap_err());
        }
    }
}

#[test]
fn header_locations_and_redundant_semantics_must_match() {
    for relative in [24, 32, 40, 48, 56, 72, 80, 88] {
        let mut fixture = Fixture::esp(512);
        let offset = fixture.backup_header_offset() + relative;
        fixture.bytes[offset] ^= 1;
        fixture.repair_header_crc(fixture.backup_header_lba);
        invalid_data(fixture.authenticate().unwrap_err());
    }
}

#[test]
fn entry_count_size_and_metadata_layout_are_strict() {
    for (relative, value) in [(80, 127_u32), (80, 4_097), (84, 256)] {
        let mut fixture = Fixture::esp(512);
        for lba in [fixture.primary_header_lba, fixture.backup_header_lba] {
            let offset = fixture.block_offset(lba) + relative;
            fixture.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            fixture.repair_header_crc(lba);
        }
        invalid_data(fixture.authenticate().unwrap_err());
    }

    let mut fixture = Fixture::esp(512);
    let offset = fixture.primary_header_offset() + 72;
    fixture.bytes[offset..offset + 8].copy_from_slice(&3_u64.to_le_bytes());
    fixture.repair_header_crc(fixture.primary_header_lba);
    invalid_data(fixture.authenticate().unwrap_err());

    let unaligned = Fixture::with_entry_count(32_768, 128);
    invalid_data(unaligned.authenticate().unwrap_err());
}

#[test]
fn both_entry_arrays_require_crc_and_byte_equality() {
    let mut primary_crc = Fixture::esp(512);
    let primary = primary_crc.primary_array_offset();
    primary_crc.bytes[primary + 60] ^= 1;
    invalid_data(primary_crc.authenticate().unwrap_err());

    let mut backup_crc = Fixture::esp(512);
    let backup = backup_crc.backup_array_offset();
    backup_crc.bytes[backup + 60] ^= 1;
    invalid_data(backup_crc.authenticate().unwrap_err());

    let mut unequal = Fixture::esp(512);
    let backup = unequal.backup_array_offset();
    let count = unequal.array_bytes();
    change_bytes_without_changing_crc(&mut unequal.bytes[backup..backup + count]);
    invalid_data(unequal.authenticate().unwrap_err());
}

#[test]
fn selected_entry_requires_exact_number_partuuid_and_role() {
    let fixture = Fixture::esp(512);
    for (number, uuid, role) in [
        (0, ESP_UUID, GptPartitionRole::Esp),
        (129, ESP_UUID, GptPartitionRole::Esp),
        (1, XBOOTLDR_UUID, GptPartitionRole::Esp),
        (1, ESP_UUID, GptPartitionRole::Xbootldr),
    ] {
        let error =
            authenticate_gpt_partition_role_image_until(&fixture.bytes, 512, number, uuid, role, live_deadline())
                .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
        ));
    }
}

#[test]
fn used_entries_require_nonzero_unique_guids_and_usable_ranges() {
    let mut zero = Fixture::esp(512);
    zero.write_entry(
        1,
        ESP_TYPE_GUID,
        [0; 16],
        zero.selected_start_lba,
        zero.selected_start_lba + zero.selected_size_lba - 1,
    );
    zero.rebuild_arrays_and_headers();
    invalid_data(zero.authenticate().unwrap_err());

    let mut duplicate = Fixture::esp(512);
    duplicate.write_entry(
        2,
        [0x5a; 16],
        guid_disk_bytes(ESP_UUID),
        duplicate.selected_start_lba + 64,
        duplicate.selected_start_lba + 95,
    );
    duplicate.rebuild_arrays_and_headers();
    invalid_data(duplicate.authenticate().unwrap_err());

    for (start, end) in [
        (0, 1),
        (100, 99),
        (duplicate.last_usable_lba, duplicate.last_usable_lba + 1),
    ] {
        let mut range = Fixture::esp(512);
        range.write_entry(2, [0x5a; 16], guid_disk_bytes(SECOND_UUID), start, end);
        range.rebuild_arrays_and_headers();
        invalid_data(range.authenticate().unwrap_err());
    }

    let mut reserved_attribute = Fixture::esp(512);
    let attribute_offset = reserved_attribute.primary_array_offset() + 48;
    reserved_attribute.bytes[attribute_offset] = 1 << 3;
    reserved_attribute.rebuild_arrays_and_headers();
    invalid_data(reserved_attribute.authenticate().unwrap_err());
}

#[test]
fn used_entry_ranges_must_not_overlap() {
    let mut fixture = Fixture::esp(512);
    fixture.write_entry(
        2,
        [0x5a; 16],
        guid_disk_bytes(SECOND_UUID),
        fixture.selected_start_lba + 1,
        fixture.selected_start_lba + fixture.selected_size_lba,
    );
    fixture.rebuild_arrays_and_headers();
    invalid_data(fixture.authenticate().unwrap_err());
}

#[test]
fn unused_entries_must_be_completely_zero() {
    let mut fixture = Fixture::esp(512);
    fixture.clear_entry(3);
    let offset = fixture.primary_array_offset() + 2 * 128 + 16;
    fixture.bytes[offset] = 1;
    fixture.rebuild_arrays_and_headers();
    invalid_data(fixture.authenticate().unwrap_err());
}

#[test]
fn expected_partuuid_text_is_canonical_lowercase_and_nonzero() {
    let fixture = Fixture::esp(512);
    for uuid in [
        "00112233-4455-6677-8899-AABBCCDDEEFF",
        "00112233445566778899aabbccddeeff",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let error = authenticate_gpt_partition_role_image_until(
            &fixture.bytes,
            512,
            1,
            uuid,
            GptPartitionRole::Esp,
            live_deadline(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
