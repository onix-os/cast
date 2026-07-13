// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, btree_map};
use std::{io, num::NonZeroU64};

use fs_err as fs;
use itertools::Itertools;
use stone::StoneDigestWriterHasher;
use thiserror::Error;

use moss::util;
use stone_recipe::{Package, PathKind, derivation::DerivationId, script};

use crate::{Macros, Paths, Recipe, Timing, build, container, timing};

use self::collect::Collector;
use self::emit::emit;

mod analysis;
mod collect;
mod emit;

pub struct Packager<'a> {
    paths: &'a Paths,
    recipe: &'a Recipe,
    packages: BTreeMap<String, Package>,
    collector: Collector,
    build_release: NonZeroU64,
}

impl<'a> Packager<'a> {
    pub fn new(
        paths: &'a Paths,
        recipe: &'a Recipe,
        macros: &'a Macros,
        targets: &'a [build::Target],
        build_release: NonZeroU64,
    ) -> Result<Self, Error> {
        let mut collector = Collector::new(paths.install().guest);

        // Arch names used to parse [`Macros`] for package templates
        //
        // We always use "base" plus whatever build targets we've built
        let arches = Some("base".to_owned())
            .into_iter()
            .chain(targets.iter().map(|target| target.build_target.to_string()));

        // Resolves all package templates from arch macros + recipe file. Also adds
        // package paths to [`Collector`]
        let packages = resolve_packages(arches, macros, recipe, &mut collector)?;

        Ok(Self {
            paths,
            recipe,
            collector,
            packages,
            build_release,
        })
    }

    pub fn package(&self, timing: &mut Timing, derivation_id: &DerivationId) -> Result<(), Error> {
        // Hasher used for calculating file digests
        let mut hasher = StoneDigestWriterHasher::new();

        let timer = timing.begin(timing::Kind::Analyze);

        // Collect all paths under install root
        let paths = self
            .collector
            .enumerate_paths(None, &mut hasher)
            .map_err(Error::CollectPaths)?;

        // Process all paths with the analysis chain
        // This will determine which files get included
        // and what deps / provides they produce
        let mut analysis = analysis::Chain::new(self.paths, self.recipe, &self.collector, &mut hasher);
        analysis.process(paths).map_err(Error::Analysis)?;

        timing.finish(timer);

        let timer = timing.begin(timing::Kind::Emit);

        // Combine the package definition with the analysis results
        // for that package. We will use this to emit the package stones & manifests.
        //
        // If no bucket exists, that means no paths matched this package so we can
        // safely filter it out
        let packages = self
            .packages
            .iter()
            .filter_map(|(name, package)| {
                let bucket = analysis.buckets.remove(name)?;

                Some(emit::Package::new(
                    name,
                    &self.recipe.parsed.source,
                    package,
                    bucket,
                    self.build_release,
                    &self.recipe.fingerprint.sha256,
                    derivation_id,
                ))
            })
            .collect::<Vec<_>>();

        // Emit package stones and manifest files to artefact directory
        emit(self.paths, self.recipe, &packages, derivation_id).map_err(Error::Emit)?;

        timing.finish(timer);

        Ok(())
    }

    pub(crate) fn resolved_packages(&self) -> &BTreeMap<String, Package> {
        &self.packages
    }

    pub(crate) fn collection_rules(&self) -> impl Iterator<Item = (&str, PathKind, &str)> {
        self.collector
            .rules()
            .iter()
            .map(|rule| (rule.package.as_str(), rule.kind, rule.pattern.as_str()))
    }
}

/// Resolve all package templates from the arch macros and
/// incoming recipe. Package templates may have variables so
/// they are fully expanded before returned.
fn resolve_packages(
    arches: impl IntoIterator<Item = String>,
    macros: &Macros,
    recipe: &Recipe,
    collector: &mut Collector,
) -> Result<BTreeMap<String, Package>, Error> {
    let mut parser = script::Parser::new();
    parser.add_definition("name", &recipe.parsed.source.name);
    parser.add_definition("version", &recipe.parsed.source.version);
    parser.add_definition("release", recipe.parsed.source.release);

    let mut packages = BTreeMap::new();

    // Add a package, ensuring it's fully expanded
    //
    // If a name collision occurs, merge the incoming and stored
    // packages
    let mut add_package = |mut name: String, mut package: Package| {
        name = parser.parse_content(&name)?;

        package.summary = package
            .summary
            .as_ref()
            .or(recipe.parsed.package.summary.as_ref())
            .map(|summary| parser.parse_content(summary))
            .transpose()?;
        package.description = package
            .description
            .as_ref()
            .or(recipe.parsed.package.description.as_ref())
            .map(|description| parser.parse_content(description))
            .transpose()?;
        package.provides_exclude = package.provides_exclude.into_iter().collect();
        package.run_deps = package
            .run_deps
            .into_iter()
            .map(|dep| parser.parse_content(&dep))
            .collect::<Result<_, _>>()?;
        package.run_deps_exclude = package.run_deps_exclude.into_iter().collect();
        package.conflicts = package
            .conflicts
            .into_iter()
            .map(|provider| parser.parse_content(&provider))
            .collect::<Result<_, _>>()?;
        package.paths = package
            .paths
            .into_iter()
            .map(|mut path| {
                path.path = parser.parse_content(&path.path)?;
                Ok(path)
            })
            .collect::<Result<_, Error>>()?;

        stone_recipe::validation::validate_package(&package, &format!("packages[{name}]"))
            .map_err(Error::InvalidPackageRelation)?;

        // Add each path to collector
        for path in &package.paths {
            collector.add_rule(collect::Rule {
                pattern: path.path.clone(),
                package: name.clone(),
                kind: path.kind,
            });
        }

        match packages.entry(name.clone()) {
            btree_map::Entry::Vacant(entry) => {
                entry.insert(package);
            }
            btree_map::Entry::Occupied(entry) => {
                let prev = entry.remove();

                package.run_deps = package.run_deps.into_iter().chain(prev.run_deps).sorted().collect();
                package.run_deps_exclude = package
                    .run_deps_exclude
                    .into_iter()
                    .chain(prev.run_deps_exclude)
                    .sorted()
                    .collect();
                package.paths = package
                    .paths
                    .into_iter()
                    .chain(prev.paths)
                    .sorted_by_key(|p| p.path.clone())
                    .collect();

                packages.insert(name, package);
            }
        }

        Result::<_, Error>::Ok(())
    };

    // Add packages templates from each architecture
    for arch in arches.into_iter() {
        if let Some(macros) = macros.arch.get(&arch) {
            for entry in macros.packages.clone().into_iter() {
                add_package(entry.key, entry.value)?;
            }
        }
    }

    // Add the root recipe package
    add_package(recipe.parsed.source.name.clone(), recipe.parsed.package.clone())?;

    // Add the recipe sub-packages
    recipe
        .parsed
        .sub_packages
        .iter()
        .try_for_each(|entry| add_package(entry.key.clone(), entry.value.clone()))?;

    Ok(packages)
}

pub fn sync_artefacts(paths: &Paths) -> io::Result<()> {
    for path in util::enumerate_files(&paths.artefacts().host, |_| true)? {
        let filename = path.file_name().and_then(|p| p.to_str()).unwrap_or_default();

        let target = paths.output_dir().join(filename);

        if target.exists() {
            fs::remove_file(&target)?;
        }

        util::hardlink_or_copy(&path, &target)?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("script")]
    Script(#[from] script::Error),
    #[error("collect install paths")]
    CollectPaths(#[source] collect::Error),
    #[error("analyzing paths")]
    Analysis(#[source] analysis::BoxError),
    #[error("emit packages")]
    Emit(#[from] emit::Error),
    #[error("container")]
    Container(#[from] container::Error),
    #[error("invalid fully expanded package relation")]
    InvalidPackageRelation(#[source] stone_recipe::ValidationError),
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn repository_package_policy_expands_and_merges_for_x86_64() {
        let macros = Macros::repository_for_tests();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());

        let packages = resolve_packages(
            ["base".to_owned(), "x86_64".to_owned()],
            &macros,
            &recipe,
            &mut collector,
        )
        .unwrap();

        // Golden inherited base-policy package templates captured at 80d7ac5, expanded
        // and merged through the same boundary used by the packager.
        assert_eq!(
            packages.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "hello",
                "hello-32bit",
                "hello-32bit-dbginfo",
                "hello-32bit-devel",
                "hello-dbginfo",
                "hello-demos",
                "hello-devel",
                "hello-docs",
                "hello-libs",
            ]
        );
        let rules = collector.rules();
        assert_eq!(
            rules.last().map(|rule| (rule.package.as_str(), rule.pattern.as_str())),
            Some(("hello-demos", "/usr/lib/qt*/examples"))
        );
        assert_ne!(
            rules.last().map(|rule| rule.package.as_str()),
            packages.keys().last().map(String::as_str),
            "collector precedence must retain composition order rather than package-map order"
        );

        let root = &packages["hello"];
        assert_eq!(root.summary.as_deref(), Some("Minimal Gluon recipe example"));
        assert_eq!(
            root.paths.iter().map(|path| path.path.as_str()).collect::<Vec<_>>(),
            ["*"]
        );

        let devel = &packages["hello-devel"];
        assert_eq!(devel.summary.as_deref(), Some("Development files for hello"));
        assert_eq!(
            devel.description.as_deref(),
            Some("Install this package if you intend to build software against\nthe hello package.")
        );
        assert_eq!(devel.run_deps, ["hello"]);
        assert_eq!(
            devel.paths.iter().map(|path| path.path.as_str()).collect::<Vec<_>>(),
            [
                "/usr/include",
                "/usr/lib/*.a",
                "/usr/lib/cmake",
                "/usr/lib/lib*.so",
                "/usr/lib/pkgconfig",
                "/usr/share/aclocal",
                "/usr/share/cmake",
                "/usr/share/man/man2",
                "/usr/share/man/man3",
                "/usr/share/man/man9",
                "/usr/share/pkgconfig",
                "/usr/share/gir-1.0/*.gir",
                "/usr/share/vala/vapi/*.deps",
                "/usr/share/vala/vapi/*.vapi",
                "/usr/lib/*.prl",
                "/usr/lib/metatypes",
                "/usr/lib/qt*/metatypes/qt*.json",
                "/usr/lib/qt*/mkspecs",
                "/usr/lib/qt*/modules/*.json",
                "/usr/lib/qt*/sbom",
                "/usr/lib/qt*/plugins/designer/*.so",
                "/usr/share/doc/qt5/*.qch",
                "/usr/share/doc/qt5/*.tags",
                "/usr/share/doc/qt6/*.qch",
                "/usr/share/doc/qt6/*.tags",
            ]
        );
    }

    #[test]
    fn invalid_relation_after_macro_expansion_is_a_structured_error() {
        let macros = Macros::repository_for_tests();
        let mut recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        recipe.parsed.source.name = "unknown(target)".to_owned();
        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());

        let error = resolve_packages(["base".to_owned()], &macros, &recipe, &mut collector).unwrap_err();

        assert!(matches!(
            error,
            Error::InvalidPackageRelation(stone_recipe::ValidationError::InvalidDependency { ref field, .. })
                if field == "packages[unknown(target)-devel].run_deps[0]"
        ));
    }
}
