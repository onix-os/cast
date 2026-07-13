// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, io::Write};

use stone::{
    StoneHeaderV1FileType, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriteError,
    StoneWriter, relation::Dependency,
};
use stone_recipe::derivation::DerivationId;

use crate::package::emit::Package;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("write stone manifest")]
    Stone(#[from] StoneWriteError),
}

pub fn write<W: Write>(
    output: &mut W,
    packages: &[&Package<'_>],
    build_deps: &BTreeSet<Dependency>,
    recipe_fingerprint: &str,
    derivation_id: &DerivationId,
) -> Result<(), Error> {
    let mut writer = StoneWriter::new(output, StoneHeaderV1FileType::BuildManifest)?;

    // Add each package
    for package in packages {
        let mut meta = package.meta();
        // deliberately override .stone package metadata and set build_release to zero for binary manifests
        meta.build_release = 0;
        let mut payload =
            Package::with_derivation_provenance(meta.to_stone_payload(), recipe_fingerprint, derivation_id);

        // Add build deps
        for dependency in build_deps {
            payload.push(StonePayloadMetaRecord {
                tag: StonePayloadMetaTag::BuildDepends,
                primitive: StonePayloadMetaPrimitive::Dependency(dependency.kind.into(), dependency.name.clone()),
            });
        }

        writer.add_payload(payload.as_slice())?;
    }

    writer.finalize()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, io::Cursor, num::NonZeroU64};

    use stone::{
        StoneDecodedPayload, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriter,
    };

    use super::*;
    use crate::package::emit::{
        DERIVATION_ID_SOURCE_REF_PREFIX, RECIPE_FINGERPRINT_SOURCE_REF_PREFIX, test_derivation_plan,
    };
    use crate::package::{ResolvedOutput, analysis::Bucket};

    #[test]
    fn package_and_binary_manifest_metadata_record_plan_recipe_and_derivation_provenance() {
        let plan = test_derivation_plan();
        let recipe_fingerprint = &plan.provenance.recipe.sha256;
        let derivation_id = plan.derivation_id();
        let identity = stone_recipe::derivation::PackageIdentity {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            source_release: 1,
            build_release: 1,
            homepage: "https://example.invalid".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            architecture: "x86_64".to_owned(),
        };
        let definition = ResolvedOutput {
            summary: Some("Example".to_owned()),
            description: Some("Example package".to_owned()),
            runtime_inputs: vec![Dependency::package_name("runtime")],
            conflicts: vec![stone::relation::Provider::package_name("incompatible")],
            ..ResolvedOutput::default()
        };
        let package = Package::new_with_architecture(
            "example",
            &identity,
            &definition,
            Bucket::default(),
            NonZeroU64::new(1).unwrap(),
            crate::Architecture::X86_64,
            1,
        );
        let recipe_ref = format!("{RECIPE_FINGERPRINT_SOURCE_REF_PREFIX}{recipe_fingerprint}");
        let derivation_ref = format!("{DERIVATION_ID_SOURCE_REF_PREFIX}{derivation_id}");
        let expected = BTreeSet::from([recipe_ref.as_str(), derivation_ref.as_str()]);

        let mut package_output = Cursor::new(Vec::new());
        let mut package_writer = StoneWriter::new(&mut package_output, StoneHeaderV1FileType::Binary).unwrap();
        package_writer
            .add_payload(package.meta_payload(recipe_fingerprint, &derivation_id).as_slice())
            .unwrap();
        package_writer.finalize().unwrap();
        package_output.set_position(0);
        let package_payloads = forge::util::stone_payloads(&mut package_output).unwrap();
        let package_meta = package_payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();

        assert_eq!(source_refs(&package_meta.body), expected);

        let mut output = Cursor::new(Vec::new());
        write(
            &mut output,
            &[&package],
            &BTreeSet::from([Dependency::package_name("build-tool")]),
            recipe_fingerprint,
            &derivation_id,
        )
        .unwrap();
        output.set_position(0);
        let payloads = forge::util::stone_payloads(&mut output).unwrap();
        let manifest_meta = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();

        assert_eq!(source_refs(&manifest_meta.body), expected);

        let meta = package.meta();
        assert_eq!(meta.summary, "Example");
        assert_eq!(meta.description, "Example package");
        assert_eq!(
            meta.dependencies.iter().map(Dependency::to_name).collect::<Vec<_>>(),
            ["runtime"]
        );
        assert_eq!(
            meta.conflicts
                .iter()
                .map(|provider| provider.to_name())
                .collect::<Vec<_>>(),
            ["incompatible"]
        );
    }

    fn source_refs(payload: &[StonePayloadMetaRecord]) -> BTreeSet<&str> {
        payload
            .iter()
            .filter_map(|record| match (&record.tag, &record.primitive) {
                (StonePayloadMetaTag::SourceRef, StonePayloadMetaPrimitive::String(value)) => Some(value.as_str()),
                _ => None,
            })
            .collect()
    }
}
