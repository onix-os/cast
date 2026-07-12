// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, io::Write};

use moss::Dependency;
use stone::{
    StoneHeaderV1FileType, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriteError,
    StoneWriter,
};

use crate::package::emit::Package;

pub fn write<W: Write>(
    output: &mut W,
    packages: &BTreeSet<&Package<'_>>,
    build_deps: &BTreeSet<String>,
) -> Result<(), StoneWriteError> {
    let mut writer = StoneWriter::new(output, StoneHeaderV1FileType::BuildManifest)?;

    // Add each package
    for package in packages {
        let mut meta = package.meta();
        // deliberately override .stone package metadata and set build_release to zero for binary manifests
        meta.build_release = 0;
        let mut payload = package.with_recipe_provenance(meta.to_stone_payload());

        // Add build deps
        for name in build_deps {
            if let Ok(dep) = Dependency::from_name(name) {
                payload.push(StonePayloadMetaRecord {
                    tag: StonePayloadMetaTag::BuildDepends,
                    primitive: StonePayloadMetaPrimitive::Dependency(dep.kind.into(), dep.name),
                });
            }
        }

        writer.add_payload(payload.as_slice())?;
    }

    writer.finalize()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, io::Cursor, num::NonZeroU64};

    use stone::{StoneDecodedPayload, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag};

    use super::*;
    use crate::package::analysis::Bucket;
    use crate::package::emit::RECIPE_FINGERPRINT_SOURCE_REF_PREFIX;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn package_and_binary_manifest_metadata_record_recipe_provenance() {
        let source = stone_recipe::Source {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            release: 1,
            homepage: "https://example.invalid".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        };
        let definition = stone_recipe::Package {
            summary: Some("Example".to_owned()),
            description: Some("Example package".to_owned()),
            provides_exclude: Vec::new(),
            run_deps: Vec::new(),
            run_deps_exclude: Vec::new(),
            paths: Vec::new(),
            conflicts: Vec::new(),
        };
        let package = Package::new(
            "example",
            &source,
            &definition,
            Bucket::default(),
            NonZeroU64::new(1).unwrap(),
            FINGERPRINT,
        );
        let expected = format!("{RECIPE_FINGERPRINT_SOURCE_REF_PREFIX}{FINGERPRINT}");

        assert_eq!(source_ref(&package.meta_payload()), Some(expected.as_str()));

        let mut output = Cursor::new(Vec::new());
        write(&mut output, &BTreeSet::from([&package]), &BTreeSet::new()).unwrap();
        output.set_position(0);
        let payloads = moss::util::stone_payloads(&mut output).unwrap();
        let manifest_meta = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();

        assert_eq!(source_ref(&manifest_meta.body), Some(expected.as_str()));
    }

    fn source_ref(payload: &[StonePayloadMetaRecord]) -> Option<&str> {
        payload
            .iter()
            .find_map(|record| match (&record.tag, &record.primitive) {
                (StonePayloadMetaTag::SourceRef, StonePayloadMetaPrimitive::String(value)) => Some(value.as_str()),
                _ => None,
            })
    }
}
