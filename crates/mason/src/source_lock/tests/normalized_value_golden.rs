use super::*;

#[test]
fn generated_source_lock_has_exact_normalized_owned_value() {
    let decoded = decode_source_lock(
        SOURCE_LOCK_FILE_NAME,
        include_bytes!("../../../../../tests/fixtures/gluon/execution/packages/daemon-generated/sources.lock.glu"),
    )
    .unwrap();
    let expected = SourceLock {
        schema_version: 2,
        sources: vec![SourceResolution::Archive(ArchiveResolution {
            order: 0,
            url: "https://fixtures.invalid/sources/cast-daemon-fixture-1.0.0.tar.zst".to_owned(),
            sha256: "7d01ab16acc6b96925e8f996e8fbaea4d11b448bccc44d40090bbdc7963e617b".to_owned(),
        })],
    };

    assert_eq!(decoded, expected);
}
