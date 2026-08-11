pub(crate) mod ext;
mod header;
mod payload;
mod read;
pub mod relation;
mod write;

pub use self::header::{
    STONE_HEADER_MAGIC, StoneAgnosticHeader, StoneHeader, StoneHeaderDecodeError, StoneHeaderV1,
    StoneHeaderV1DecodeError, StoneHeaderV1FileType, StoneHeaderVersion,
};
pub use self::payload::{
    StonePayload, StonePayloadAttributeRecord, StonePayloadCompression, StonePayloadContent, StonePayloadDecodeError,
    StonePayloadEncodeError, StonePayloadHeader, StonePayloadIndexRecord, StonePayloadKind, StonePayloadLayoutFile,
    StonePayloadLayoutFileType, StonePayloadLayoutRecord, StonePayloadMetaDependency, StonePayloadMetaPrimitive,
    StonePayloadMetaRecord, StonePayloadMetaTag,
};
#[cfg(feature = "ffi")]
pub use self::read::StonePayloadContentReader;
pub use self::read::{
    StoneDecodeLimits, StoneDecodedPayload, StoneReadError, StoneReader, read, read_bytes, read_bytes_with_limits,
    read_with_limits,
};
pub use self::write::{
    StoneContentWriter, StoneDigestWriter, StoneDigestWriterHasher, StoneWriteError, StoneWritePayload, StoneWriter,
};

#[cfg(test)]
mod test {
    use std::{io::Cursor, thread};

    use super::*;

    /// Emits the boot-capable package the crash matrix installs to make
    /// `run_boot_sync` true. Applicability needs a systemd-boot asset and a
    /// kernel in the same candidate; the initrd and cmdline complete the
    /// per-version asset set. `os-info.json` is deliberately absent so the plan
    /// takes its generated-os-release fallback.
    #[test]
    #[ignore = "regeneration helper for the boot-capable crash-matrix fixture"]
    fn regenerate_boot_assets_fixture() {
        use xxhash_rust::xxh3::Xxh3;

        const KERNEL_VERSION: &str = "6.12.0-fixture";
        let files: Vec<(String, &[u8])> = vec![
            (
                "lib/systemd/boot/efi/systemd-bootx64.efi".to_owned(),
                b"crash-matrix fixture systemd-boot payload\n".as_slice(),
            ),
            (
                format!("lib/kernel/{KERNEL_VERSION}/vmlinuz"),
                b"crash-matrix fixture kernel payload\n".as_slice(),
            ),
            (
                format!("lib/kernel/{KERNEL_VERSION}/fixture.initrd"),
                b"crash-matrix fixture initrd payload\n".as_slice(),
            ),
            (
                format!("lib/kernel/{KERNEL_VERSION}/fixture.cmdline"),
                b"quiet\n".as_slice(),
            ),
        ];

        let meta = |tag, primitive| StonePayloadMetaRecord { tag, primitive };
        let meta_records = vec![
            meta(
                StonePayloadMetaTag::Name,
                StonePayloadMetaPrimitive::String("boot-assets".to_owned()),
            ),
            meta(
                StonePayloadMetaTag::Version,
                StonePayloadMetaPrimitive::String("1.0".to_owned()),
            ),
            meta(StonePayloadMetaTag::Release, StonePayloadMetaPrimitive::Uint64(1)),
            meta(StonePayloadMetaTag::BuildRelease, StonePayloadMetaPrimitive::Uint64(1)),
            meta(
                StonePayloadMetaTag::Architecture,
                StonePayloadMetaPrimitive::String("x86_64".to_owned()),
            ),
            meta(
                StonePayloadMetaTag::Summary,
                StonePayloadMetaPrimitive::String("Synthetic boot assets for the crash matrix".to_owned()),
            ),
            meta(
                StonePayloadMetaTag::Description,
                StonePayloadMetaPrimitive::String(
                    "Carries a systemd-boot asset and one kernel version so boot publication applies.".to_owned(),
                ),
            ),
            meta(
                StonePayloadMetaTag::Homepage,
                StonePayloadMetaPrimitive::String("https://example.invalid/boot-assets".to_owned()),
            ),
            meta(
                StonePayloadMetaTag::SourceID,
                StonePayloadMetaPrimitive::String("boot-assets".to_owned()),
            ),
            meta(
                StonePayloadMetaTag::License,
                StonePayloadMetaPrimitive::String("MPL-2.0".to_owned()),
            ),
        ];

        let layouts = files
            .iter()
            .map(|(target, bytes)| {
                let mut hasher = Xxh3::new();
                hasher.update(bytes);
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: 0o100644,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(hasher.digest128(), target.as_str().into()),
                }
            })
            .collect::<Vec<_>>();

        let mut out_stone = vec![];
        let mut temp_content_buffer: Vec<u8> = vec![];
        let plain_size = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        let mut writer = StoneWriter::new(&mut out_stone, StoneHeaderV1FileType::Binary)
            .unwrap()
            .with_content(Cursor::new(&mut temp_content_buffer), Some(plain_size), 1)
            .unwrap();
        writer.add_payload(meta_records.as_slice()).unwrap();
        for (_, bytes) in &files {
            writer.add_content(&mut &bytes[..]).unwrap();
        }
        writer.add_payload(layouts.as_slice()).unwrap();
        writer.finalize().unwrap();

        std::fs::write("../../tests/fixtures/boot-assets-1.0-1-1-x86_64.stone", &out_stone).unwrap();
        println!("wrote {} bytes", out_stone.len());
    }

    #[test]
    fn roundtrip() {
        let in_stone = include_bytes!("../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");

        let mut reader = read_bytes(in_stone).unwrap();

        let payloads = reader.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let meta = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
        let layouts = payloads.iter().find_map(StoneDecodedPayload::layout).unwrap();
        let indices = payloads.iter().find_map(StoneDecodedPayload::index).unwrap();
        let content = payloads.iter().find_map(StoneDecodedPayload::content).unwrap();

        let mut content_buffer = vec![];

        reader.unpack_content(content, &mut content_buffer).unwrap();

        let mut out_stone = vec![];
        let mut temp_content_buffer: Vec<u8> = vec![];
        let mut writer = StoneWriter::new(&mut out_stone, StoneHeaderV1FileType::Binary)
            .unwrap()
            .with_content(
                Cursor::new(&mut temp_content_buffer),
                Some(content_buffer.len() as u64),
                thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as u32,
            )
            .unwrap();

        writer.add_payload(meta.body.as_slice()).unwrap();

        for index in &indices.body {
            let mut bytes = &content_buffer[index.start as usize..index.end as usize];

            writer.add_content(&mut bytes).unwrap();
        }

        // We'd typically add layouts after calling `add_content` since
        // we will determine the layout when processing the file during
        // that iteration
        writer.add_payload(layouts.body.as_slice()).unwrap();

        writer.finalize().unwrap();

        let mut rt_reader = read_bytes(&out_stone).unwrap();
        assert_eq!(rt_reader.header, reader.header);

        let rt_payloads = rt_reader.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let rt_meta = rt_payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
        let rt_layouts = rt_payloads.iter().find_map(StoneDecodedPayload::layout).unwrap();
        let rt_indices = rt_payloads.iter().find_map(StoneDecodedPayload::index).unwrap();
        let rt_content = rt_payloads.iter().find_map(StoneDecodedPayload::content).unwrap();

        // Stored size / digest will be different since compression from Cast
        // isn't identical & we don't add null terminated strings
        assert_eq!(rt_indices.header.plain_size, indices.header.plain_size);
        assert_eq!(rt_content.header.plain_size, content.header.plain_size);
        assert_eq!(rt_meta.body.len(), meta.body.len());
        assert_eq!(rt_layouts.body.len(), layouts.body.len());

        assert!(meta.body.iter().zip(&rt_meta.body).all(|(a, b)| a == b));
        assert!(layouts.body.iter().zip(&rt_layouts.body).all(|(a, b)| a == b));
        assert!(indices.body.iter().zip(&rt_indices.body).all(|(a, b)| a == b));

        let mut rt_content_buffer = vec![];

        rt_reader.unpack_content(rt_content, &mut rt_content_buffer).unwrap();

        assert_eq!(rt_content_buffer, content_buffer);

        println!(
            "reference stone size => {}, stone-rs stone size => {}",
            in_stone.len(),
            out_stone.len()
        );
    }
}
