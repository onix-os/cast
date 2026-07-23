use super::*;
use crate::{
    boot_publication::receipt_body::{
        BootPublicationDestination, BootPublicationDestinations, BootPublicationHistoricalRuntimeWitness,
        BootPublicationOutput, BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
        BootPublicationPublicationPhase, BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
    },
    state::TransitionId,
};

const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
const ALTERNATE_PARTUUID: &str = "99999999-8888-7777-6666-555555555555";
const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

#[derive(Clone)]
struct Fixture {
    transition_id: &'static str,
    committed: Option<u8>,
    journal_hash: u8,
    inventory_hash: u8,
    esp_partuuid: &'static str,
    esp_partition: u32,
    esp_inode: u64,
    distinct: bool,
    root: BootPublicationRoot,
    phase: BootPublicationPublicationPhase,
    role: BootPublicationOutputRole,
    path: &'static str,
    mode: u32,
    xxh3: u128,
    length: u64,
    content_hash: u8,
    claim: BootPublicationOutputProvenanceClaim,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            transition_id: "00112233445566778899aabbccddeeff",
            committed: None,
            journal_hash: 0x11,
            inventory_hash: 0x22,
            esp_partuuid: ESP_PARTUUID,
            esp_partition: 1,
            esp_inode: 101,
            distinct: false,
            root: BootPublicationRoot::Boot,
            phase: BootPublicationPublicationPhase::Payload,
            role: BootPublicationOutputRole::Payload,
            path: "EFI/cast/vmlinuz",
            mode: 0o644,
            xxh3: 0x00112233445566778899aabbccddeeff,
            length: 12,
            content_hash: 0x33,
            claim: BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
        }
    }
}

fn destination(partuuid: &str, partition_number: u32, inode: u64, minor: u32) -> BootPublicationDestination {
    BootPublicationDestination::new(
        partuuid,
        partition_number,
        BootPublicationHistoricalRuntimeWitness::new(
            u64::try_from(nix::libc::makedev(8, minor)).unwrap(),
            inode,
            10 + u64::from(minor),
            8,
            minor,
            Some(77),
        ),
    )
}

fn body(fixture: &Fixture) -> BootPublicationReceiptBody {
    let esp = destination(fixture.esp_partuuid, fixture.esp_partition, fixture.esp_inode, 1);
    let destinations = if fixture.distinct {
        BootPublicationDestinations::distinct_xbootldr(
            esp,
            destination(XBOOTLDR_PARTUUID, 2, 202, 2),
        )
    } else {
        BootPublicationDestinations::boot_aliases_esp(esp)
    };
    BootPublicationReceiptBody::new(
        TransitionId::parse(fixture.transition_id).unwrap(),
        fixture
            .committed
            .map(|byte| BootPublicationReceiptFingerprint::from_bytes([byte; 32])),
        BootPublicationSha256::from_bytes([fixture.journal_hash; 32]),
        BootPublicationSha256::from_bytes([fixture.inventory_hash; 32]),
        destinations,
        vec![BootPublicationOutput::new(
            fixture.root,
            fixture.phase,
            fixture.role,
            fixture.path,
            fixture.mode,
            BootPublicationXxh3::from_u128(fixture.xxh3),
            fixture.length,
            BootPublicationSha256::from_bytes([fixture.content_hash; 32]),
            fixture.claim,
        )],
    )
    .unwrap()
}

fn prepared(fixture: &Fixture) -> CanonicalBootPublicationReceipt {
    prepare_boot_publication_receipt(body(fixture)).unwrap()
}

#[test]
fn canonical_receipt_round_trips_exact_body_bytes_and_identity() {
    let prepared = prepared(&Fixture::default());
    let decoded = decode_boot_publication_receipt(prepared.canonical_body()).unwrap();
    assert_eq!(decoded, prepared);
    assert_eq!(decoded.body().transition_id().as_str(), "00112233445566778899aabbccddeeff");
    assert_eq!(decoded.body().outputs()[0].relative_path(), "EFI/cast/vmlinuz");
}

#[test]
fn canonical_fixture_has_pinned_bytes_and_domain_separated_fingerprint() {
    const CANONICAL: &str = concat!(
        "{\"format\":\"cast-boot-publication-receipt\",\"version\":1,",
        "\"transition_id\":\"00112233445566778899aabbccddeeff\",\"committed_predecessor\":null,",
        "\"predecessor_journal_sha256\":\"1111111111111111111111111111111111111111111111111111111111111111\",",
        "\"desired_inventory_sha256\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
        "\"destinations\":{\"layout\":\"boot-aliases-esp\",\"esp\":{",
        "\"partuuid\":\"11111111-2222-3333-4444-555555555555\",\"partition_number\":1,",
        "\"historical_runtime_witness\":{\"destination_device\":2049,\"destination_inode\":101,",
        "\"mount_id\":11,\"partition_device_major\":8,\"partition_device_minor\":1,\"disk_sequence\":77}}},",
        "\"outputs\":[{\"root\":\"boot\",\"phase\":\"payload\",\"role\":\"payload\",",
        "\"relative_path\":\"EFI/cast/vmlinuz\",\"mode\":420,",
        "\"xxh3\":\"00112233445566778899aabbccddeeff\",\"length\":12,",
        "\"content_sha256\":\"3333333333333333333333333333333333333333333333333333333333333333\",",
        "\"provenance_claim\":\"borrowed-first-adoption\"}]}"
    );
    let prepared = prepared(&Fixture::default());
    assert_eq!(prepared.canonical_body(), CANONICAL.as_bytes());
    assert_eq!(
        serde_json::to_string(&prepared.fingerprint()).unwrap(),
        "\"4bd5bee5f0ffc9caa3510ad89e5b85d2bb4a0f085dc248422967851dfb2ef193\""
    );
}

#[test]
fn fingerprint_changes_when_any_receipt_identity_domain_changes() {
    let base = Fixture::default();
    let expected = prepared(&base).fingerprint();
    let mut variants = Vec::new();

    let mut changed = base.clone();
    changed.transition_id = "10112233445566778899aabbccddeeff";
    variants.push(changed);
    let mut changed = base.clone();
    changed.committed = Some(0x44);
    variants.push(changed);
    let mut changed = base.clone();
    changed.journal_hash = 0x12;
    variants.push(changed);
    let mut changed = base.clone();
    changed.inventory_hash = 0x23;
    variants.push(changed);
    let mut changed = base.clone();
    changed.esp_partuuid = ALTERNATE_PARTUUID;
    variants.push(changed);
    let mut changed = base.clone();
    changed.esp_partition = 3;
    variants.push(changed);
    let mut changed = base.clone();
    changed.esp_inode = 102;
    variants.push(changed);
    let mut changed = base.clone();
    changed.distinct = true;
    variants.push(changed);
    let mut changed = base.clone();
    changed.root = BootPublicationRoot::Esp;
    changed.phase = BootPublicationPublicationPhase::Bootloader;
    changed.role = BootPublicationOutputRole::FallbackBootloader;
    changed.path = "EFI/Boot/BOOTX64.EFI";
    variants.push(changed);
    let mut changed = base.clone();
    changed.path = "EFI/cast/other-vmlinuz";
    variants.push(changed);
    let mut changed = base.clone();
    changed.xxh3 += 1;
    variants.push(changed);
    let mut changed = base.clone();
    changed.length += 1;
    variants.push(changed);
    let mut changed = base.clone();
    changed.content_hash = 0x34;
    variants.push(changed);
    let mut changed = base;
    changed.claim = BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast;
    variants.push(changed);

    for variant in variants {
        assert_ne!(prepared(&variant).fingerprint(), expected);
    }
}

#[test]
fn malformed_noncanonical_and_oversized_bodies_fail_closed() {
    assert!(matches!(
        decode_boot_publication_receipt(b"{"),
        Err(BootPublicationReceiptCodecError::Json(_))
    ));

    let prepared = prepared(&Fixture::default());
    let mut whitespace = Vec::with_capacity(prepared.canonical_body().len() + 1);
    whitespace.push(b' ');
    whitespace.extend_from_slice(prepared.canonical_body());
    assert!(matches!(
        decode_boot_publication_receipt(&whitespace),
        Err(BootPublicationReceiptCodecError::NonCanonicalBody)
    ));

    let canonical = std::str::from_utf8(prepared.canonical_body()).unwrap();
    let unknown = canonical.replacen("\"version\":1", "\"version\":1,\"unknown\":true", 1);
    assert!(matches!(
        decode_boot_publication_receipt(unknown.as_bytes()),
        Err(BootPublicationReceiptCodecError::Json(_))
    ));

    let oversized = vec![b' '; MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES + 1];
    assert!(matches!(
        decode_boot_publication_receipt(&oversized),
        Err(BootPublicationReceiptCodecError::BodyTooLarge { .. })
    ));
}
