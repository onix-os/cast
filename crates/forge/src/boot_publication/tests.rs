use super::*;

#[test]
fn receipt_fingerprint_uses_one_canonical_lowercase_hex_encoding() {
    let mut bytes = [0_u8; FINGERPRINT_BYTES];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from(index).unwrap();
    }
    let fingerprint = BootPublicationReceiptFingerprint::from_bytes(bytes);
    let encoded = serde_json::to_string(&fingerprint).unwrap();
    assert_eq!(
        encoded,
        "\"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\""
    );
    assert_eq!(serde_json::from_str::<BootPublicationReceiptFingerprint>(&encoded).unwrap(), fingerprint);
}

#[test]
fn receipt_fingerprint_accepts_zero_and_rejects_every_noncanonical_shape() {
    let zero = BootPublicationReceiptFingerprint::from_bytes([0_u8; FINGERPRINT_BYTES]);
    assert_eq!(
        serde_json::from_str::<BootPublicationReceiptFingerprint>(&serde_json::to_string(&zero).unwrap()).unwrap(),
        zero
    );

    for invalid in [
        "",
        "00",
        "000000000000000000000000000000000000000000000000000000000000000",
        "00000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000g",
        "000000000000000000000000000000000000000000000000000000000000000A",
    ] {
        assert!(serde_json::from_str::<BootPublicationReceiptFingerprint>(&format!("\"{invalid}\"")).is_err());
    }
    assert!(serde_json::from_str::<BootPublicationReceiptFingerprint>("[0,0]").is_err());
}

#[test]
fn receipt_pair_is_strict_and_round_trips_both_committed_states() {
    let pending = BootPublicationReceiptFingerprint::from_bytes([0x11; FINGERPRINT_BYTES]);
    let committed = BootPublicationReceiptFingerprint::from_bytes([0x22; FINGERPRINT_BYTES]);
    for pair in [
        BootPublicationReceiptPair {
            committed: None,
            pending,
        },
        BootPublicationReceiptPair {
            committed: Some(committed),
            pending,
        },
    ] {
        let encoded = serde_json::to_vec(&pair).unwrap();
        assert_eq!(serde_json::from_slice::<BootPublicationReceiptPair>(&encoded).unwrap(), pair);
    }

    let extra = format!(
        "{{\"committed\":null,\"pending\":{},\"unexpected\":true}}",
        serde_json::to_string(&pending).unwrap()
    );
    assert!(serde_json::from_str::<BootPublicationReceiptPair>(&extra).is_err());
}

#[test]
fn binary_receipt_fingerprint_requires_exactly_thirty_two_bytes() {
    assert_eq!(
        BootPublicationReceiptFingerprint::from_slice(&[0_u8; FINGERPRINT_BYTES]).unwrap(),
        BootPublicationReceiptFingerprint::from_bytes([0_u8; FINGERPRINT_BYTES])
    );
    for length in [0, FINGERPRINT_BYTES - 1, FINGERPRINT_BYTES + 1] {
        assert_eq!(
            BootPublicationReceiptFingerprint::from_slice(&vec![0_u8; length]),
            Err(BootPublicationReceiptFingerprintError::InvalidBinaryLength(length))
        );
    }
}
