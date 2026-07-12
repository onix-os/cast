// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::Path, path::PathBuf};

use itertools::Itertools;
use licenses::match_licences;
use moss::{Dependency, util};
use stone_recipe::{BuildSpec, PackageSpec, RecipeSpec, SourceSpec, encode_recipe_gluon_spec};
use thiserror::Error;
use tui::Styled;
use url::Url;

use crate::Env;

use self::metadata::Metadata;
use self::upstream::Upstream;

mod build;
mod licenses;
mod metadata;
pub mod upstream;

pub struct Drafter {
    env: Env,
    upstreams: Vec<Url>,
}

pub struct Draft {
    pub stone: String,
}

impl Drafter {
    pub fn new(env: Env, upstreams: Vec<Url>) -> Self {
        Self { env, upstreams }
    }

    pub fn run(&self) -> Result<Draft, Error> {
        let temp_dir = tempfile::tempdir()?;
        let extract_root = temp_dir.as_ref();

        // Fetch and extract all upstreams
        let extracted = upstream::fetch_and_extract(&self.env, &self.upstreams, extract_root)?;

        // Build metadata from extracted upstreams
        let metadata = Metadata::new(extracted);

        // Enumerate all extracted files
        let files = util::enumerate_files(extract_root, |_| true)?
            .into_iter()
            .map(|path| File { path, extract_root })
            .collect::<Vec<_>>();

        // Analyze files to determine build system / collect deps
        let build = build::analyze(&files).map_err(Error::AnalyzeBuildSystem)?;

        let licences_dir = &self.env.data_dir.join("licenses");

        let licenses = format_licenses(match_licences(extract_root, licences_dir).unwrap_or_default());

        // Remove temp extract dir
        drop(temp_dir);

        let build_system = build.detected_system.unwrap_or_else(|| {
            println!(
                "{} | Unhandled build system! - Defaulting to autotools",
                "Warning".yellow()
            );
            build::System::Autotools
        });

        let spec = draft_recipe_spec(&metadata, build_system, build.dependencies, licenses);
        let stone = encode_recipe_gluon_spec(&spec)?;

        Ok(Draft { stone })
    }
}

fn draft_recipe_spec(
    metadata: &Metadata,
    build_system: build::System,
    dependencies: impl IntoIterator<Item = Dependency>,
    licenses: Vec<String>,
) -> RecipeSpec {
    let source = SourceSpec {
        name: placeholder(&metadata.source.name, "UPDATE-NAME"),
        version: placeholder(&metadata.source.version, "0.0.0"),
        release: 1,
        homepage: placeholder(&metadata.source.homepage, "https://example.invalid/UPDATE-HOMEPAGE"),
        license: licenses,
    };
    let phases = build_system.phases();
    let mut recipe = RecipeSpec::new(source);
    recipe.upstreams = metadata.upstream_specs();
    recipe.build = BuildSpec {
        setup: phases.setup.map(str::to_owned),
        build: phases.build.map(str::to_owned),
        install: phases.install.map(str::to_owned),
        check: phases.check.map(str::to_owned),
        workload: None,
        environment: build_system.environment().map(str::to_owned),
        build_deps: builddeps(dependencies),
        check_deps: Vec::new(),
    };
    recipe.package = PackageSpec {
        summary: Some("UPDATE SUMMARY".to_owned()),
        description: Some("UPDATE DESCRIPTION".to_owned()),
        ..PackageSpec::default()
    };
    recipe.options.networking = build_system.options().networking;
    recipe
}

fn placeholder(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn builddeps(deps: impl IntoIterator<Item = Dependency>) -> Vec<String> {
    deps.into_iter().map(|dep| dep.to_string()).sorted().collect()
}

fn format_licenses(licenses: Vec<String>) -> Vec<String> {
    let mut formatted = licenses
        .into_iter()
        .sorted_by(|a, b| {
            // HACK: Ensure -or-later for GNU licenses comes before -only
            //       to match 90% of cases. We need to read the standard license
            //       header to figure out the actual variant.
            if a.contains("-only") {
                std::cmp::Ordering::Greater
            } else if b.contains("-only") {
                std::cmp::Ordering::Less
            } else {
                a.cmp(b)
            }
        })
        .collect::<Vec<_>>();
    if formatted.is_empty() {
        formatted.push("UPDATE LICENSE".to_owned());
    }
    formatted
}

pub struct File<'a> {
    pub path: PathBuf,
    pub extract_root: &'a Path,
}

impl File<'_> {
    // The depth of a file relative to it's extracted archive
    pub fn depth(&self) -> usize {
        let relative = self.path.strip_prefix(self.extract_root).unwrap_or(&self.path);

        // Subtract 2 so root of archive folder == depth 0
        relative.iter().count().saturating_sub(2)
    }

    pub fn file_name(&self) -> &str {
        self.path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("analyzing build system")]
    AnalyzeBuildSystem(#[source] build::Error),
    #[error("upstream")]
    Upstream(#[from] upstream::Error),
    #[error("encode canonical Gluon recipe")]
    EncodeRecipe(#[from] stone_recipe::RecipeConversionError),
    #[error("licensing")]
    Licenses(#[from] licenses::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("walkdir")]
    WalkDir(#[from] walkdir::Error),
}

#[cfg(test)]
mod test {
    use std::{collections::BTreeSet, path::Path};

    use gluon_config::Source as GluonSource;
    use stone_recipe::evaluate_gluon;

    use super::*;

    #[test]
    fn test_file_depth() {
        let extract_root = Path::new("/tmp/test");

        let file = File {
            path: PathBuf::from("/tmp/test/some_archive/meson.build"),
            extract_root,
        };

        assert_eq!(file.depth(), 0);
    }

    #[test]
    fn generated_draft_is_a_valid_standalone_gluon_recipe() {
        let metadata = Metadata::new(vec![Upstream {
            uri: Url::parse("https://example.com/example-1.2.3.tar.xz").unwrap(),
            hash: "0123456789abcdef".to_owned(),
        }]);
        let spec = draft_recipe_spec(
            &metadata,
            build::System::Cargo,
            BTreeSet::<Dependency>::new(),
            vec!["MPL-2.0".to_owned()],
        );

        let source = encode_recipe_gluon_spec(&spec).unwrap();
        let evaluated = evaluate_gluon(&GluonSource::new("stone.glu", source.clone())).unwrap();

        assert!(source.contains("Canonical standalone Boulder recipe"));
        assert!(source.contains("UPDATE SUMMARY"));
        assert_eq!(evaluated.recipe.source.name, "example");
        assert_eq!(evaluated.recipe.source.version, "1.2.3");
        assert_eq!(evaluated.recipe.upstreams.len(), 1);
        assert!(evaluated.recipe.options.networking);
    }
}
