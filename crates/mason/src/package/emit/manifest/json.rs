// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};

use itertools::Itertools;
use serde::Serialize;
use snafu::ResultExt;
use stone::relation::Dependency;
use stone_recipe::derivation::{DerivationId, PackageIdentity};

use super::{Error, IoSnafu, JsonSnafu};
use crate::package::emit;

pub fn write<W: Write>(
    output: &mut W,
    identity: &PackageIdentity,
    recipe_fingerprint: &str,
    packages: &[&emit::Package<'_>],
    build_deps: &BTreeSet<Dependency>,
    derivation_id: &DerivationId,
) -> Result<(), Error> {
    let packages = packages
        .iter()
        .map(|package| {
            let name = package.name.to_owned();

            let build_depends = build_deps.iter().map(Dependency::to_name).collect();
            let mut depends = package
                .dependencies()
                .iter()
                .map(Dependency::to_name)
                .collect::<Vec<_>>();
            depends.sort();
            depends.dedup();

            let provides = package.providers().iter().map(|provider| provider.to_name()).collect();

            let files = package
                .analysis
                .paths
                .iter()
                .map(|p| format!("/usr/{}", p.layout.file.target()))
                .sorted()
                .collect();

            let package = Package {
                build_depends,
                depends,
                files,
                name: name.clone(),
                provides,
            };

            (name, package)
        })
        .collect();

    let content = Content {
        manifest_version: "0.2".to_owned(),
        packages,
        derivation_id: derivation_id.as_str().to_owned(),
        recipe_fingerprint: recipe_fingerprint.to_owned(),
        source_name: identity.name.clone(),
        source_release: identity.source_release.to_string(),
        source_version: identity.version.clone(),
    };

    writeln!(output, "/** Human readable report. This is not consumed by Cast */").context(IoSnafu)?;

    let mut serializer =
        serde_json::Serializer::with_formatter(&mut *output, serde_json::ser::PrettyFormatter::with_indent(b"\t"));
    content.serialize(&mut serializer).context(JsonSnafu)?;

    writeln!(output).context(IoSnafu)?;

    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Content {
    manifest_version: String,
    packages: BTreeMap<String, Package>,
    derivation_id: String,
    recipe_fingerprint: String,
    source_name: String,
    source_release: String,
    source_version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Package {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    build_depends: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
    name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    provides: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fs_err as fs;

    use super::*;
    use crate::Recipe;
    use crate::source_lock::{SOURCE_LOCK_FILE_NAME, SourceLock, encode_source_lock};

    const RECIPE_SOURCE: &str = r#"let cast = import! cast.package.v3
cast.mk_package (cast.meta {
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.invalid",
    license = ["MPL-2.0"],
})
"#;

    #[test]
    fn emitted_recipe_aggregate_and_derivation_id_follow_plan_provenance() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stone.glu"), RECIPE_SOURCE).unwrap();
        let lock = encode_source_lock(&SourceLock::default());
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        fs::write(&lock_path, &lock).unwrap();

        let first_recipe = Recipe::load(root.path()).unwrap();
        let first_plan = plan_with_recipe(&first_recipe);
        let first_derivation_id = first_plan.derivation_id();
        let first_identity = identity(&first_recipe);
        let mut first_output = Vec::new();
        write(
            &mut first_output,
            &first_identity,
            &first_plan.provenance.recipe.sha256,
            &[],
            &BTreeSet::new(),
            &first_derivation_id,
        )
        .unwrap();
        let first_manifest = read_jsonc(&first_output);

        fs::write(&lock_path, format!("{lock}// semantically inert provenance change\n")).unwrap();
        let changed_recipe = Recipe::load(root.path()).unwrap();
        let changed_plan = plan_with_recipe(&changed_recipe);
        let changed_derivation_id = changed_plan.derivation_id();
        let changed_identity = identity(&changed_recipe);
        let mut changed_output = Vec::new();
        write(
            &mut changed_output,
            &changed_identity,
            &changed_plan.provenance.recipe.sha256,
            &[],
            &BTreeSet::new(),
            &changed_derivation_id,
        )
        .unwrap();
        let changed_manifest = read_jsonc(&changed_output);

        assert_eq!(
            first_manifest["recipe-fingerprint"],
            first_plan.provenance.recipe.sha256
        );
        assert_eq!(first_manifest["derivation-id"], first_derivation_id.as_str());
        assert_eq!(
            changed_manifest["recipe-fingerprint"],
            changed_plan.provenance.recipe.sha256
        );
        assert_eq!(changed_manifest["derivation-id"], changed_derivation_id.as_str());
        assert_ne!(
            first_manifest["recipe-fingerprint"],
            changed_manifest["recipe-fingerprint"]
        );
        assert_ne!(first_manifest["derivation-id"], changed_manifest["derivation-id"]);
    }

    fn plan_with_recipe(recipe: &Recipe) -> stone_recipe::derivation::DerivationPlan {
        let mut plan = emit::test_derivation_plan();
        plan.provenance.recipe = recipe.fingerprint.clone();
        plan.source_lock_digest = recipe.fingerprint.explicit_inputs_sha256.clone();
        plan.validate().unwrap();
        plan
    }

    fn identity(recipe: &Recipe) -> PackageIdentity {
        PackageIdentity {
            name: recipe.declaration.meta.pname.clone(),
            version: recipe.declaration.meta.version.clone(),
            source_release: u64::try_from(recipe.declaration.meta.release).unwrap(),
            build_release: 1,
            homepage: recipe.declaration.meta.homepage.clone(),
            licenses: recipe.declaration.meta.license.clone(),
            architecture: "x86_64".to_owned(),
        }
    }

    fn read_jsonc(content: &[u8]) -> serde_json::Value {
        let content = std::str::from_utf8(content).unwrap();
        let (_, json) = content.split_once('\n').unwrap();
        serde_json::from_str(json).unwrap()
    }
}
