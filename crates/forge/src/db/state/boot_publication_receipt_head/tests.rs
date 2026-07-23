use super::*;

fn fingerprint(byte: u8) -> BootPublicationReceiptFingerprint {
    BootPublicationReceiptFingerprint::from_bytes([byte; 32])
}

fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
}

fn empty_raw_head() -> BootPublicationReceiptHeadRawForTest {
    BootPublicationReceiptHeadRawForTest {
        committed_receipt_sha256: None,
        pending_transition_id: None,
        pending_receipt_sha256: None,
    }
}

#[path = "tests/api.rs"]
mod api;
#[path = "tests/corruption.rs"]
mod corruption;
#[path = "tests/migration.rs"]
mod migration;
