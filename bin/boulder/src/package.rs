// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::{io, num::NonZeroU64};

use fs_err as fs;
use stone::{
    StoneDigestWriterHasher,
    relation::{Dependency, ParseError, Provider},
};
use thiserror::Error;

use moss::util;
use stone_recipe::{
    derivation::{AnalysisPlan, DerivationId, DerivationPlan, OutputRelation, PackageIdentity, PathRuleKind},
    package::OutputSpec,
};

use crate::{Paths, Recipe, Timing, timing};

use self::collect::Collector;

mod analysis;
mod collect;
mod emit;

#[cfg(test)]
pub(crate) use emit::test_derivation_plan;

pub struct Packager {
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
}

/// One emitted package resolved from a direct package-v2 output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedOutput {
    pub(crate) include_in_manifest: bool,
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) provides_exclude: Vec<String>,
    pub(crate) runtime_inputs: Vec<Dependency>,
    pub(crate) runtime_exclude: Vec<String>,
    pub(crate) conflicts: Vec<Provider>,
}

pub struct FrozenPackager<'a> {
    paths: &'a Paths,
    identity: PackageIdentity,
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
    build_release: NonZeroU64,
    recipe_fingerprint: String,
    analysis: AnalysisPlan,
    architecture: crate::Architecture,
    manifest_build_inputs: Vec<Dependency>,
    jobs: u32,
    derivation_id: DerivationId,
}

impl<'a> FrozenPackager<'a> {
    pub fn from_plan(paths: &'a Paths, plan: &DerivationPlan) -> Result<Self, Error> {
        plan.validate().map_err(Error::InvalidFrozenPlan)?;
        if paths.layout() != &plan.layout {
            return Err(Error::FrozenLayoutMismatch);
        }
        let output_packages = plan
            .outputs
            .iter()
            .map(|output| (output.name.as_str(), output.package_name.as_str()))
            .collect::<BTreeMap<_, _>>();
        let packages = plan
            .outputs
            .iter()
            .map(|output| {
                let run_deps = output
                    .runtime_inputs
                    .iter()
                    .map(|relation| match relation {
                        OutputRelation::Locked { relation, .. } => Ok(relation.to_dependency()),
                        OutputRelation::Planned { output } => output_packages
                            .get(output.as_str())
                            .map(|package| Dependency::package_name(*package))
                            .ok_or_else(|| Error::MissingFrozenOutput(output.clone())),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    output.package_name.clone(),
                    ResolvedOutput {
                        include_in_manifest: output.include_in_manifest,
                        summary: output.summary.clone(),
                        description: output.description.clone(),
                        provides_exclude: output.provides_exclude.clone(),
                        runtime_inputs: run_deps,
                        runtime_exclude: output.runtime_exclude.clone(),
                        conflicts: output.conflicts.iter().map(|relation| relation.to_provider()).collect(),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, Error>>()?;

        let mut collector = Collector::new(&plan.layout.install_dir);
        for rule in &plan.collection_rules {
            let package = output_packages
                .get(rule.output.as_str())
                .ok_or_else(|| Error::MissingFrozenOutput(rule.output.clone()))?;
            collector.add_rule(collect::Rule {
                pattern: rule.pattern.clone(),
                package: (*package).to_owned(),
                kind: rule.kind,
            });
        }

        let manifest_build_inputs = plan
            .manifest_build_inputs
            .iter()
            .map(|relation| relation.to_dependency())
            .collect();

        Ok(Self {
            paths,
            identity: plan.package.clone(),
            packages,
            collector,
            build_release: NonZeroU64::new(plan.package.build_release).expect("validated build release"),
            recipe_fingerprint: plan.recipe_fingerprint.clone(),
            analysis: plan.analysis.clone(),
            architecture: frozen_architecture(&plan.package.architecture),
            manifest_build_inputs,
            jobs: plan.execution.jobs,
            derivation_id: plan.derivation_id(),
        })
    }

    pub fn package(&self, timing: &mut Timing) -> Result<(), Error> {
        let mut hasher = StoneDigestWriterHasher::new();
        let timer = timing.begin(timing::Kind::Analyze);
        let paths = self
            .collector
            .enumerate_paths(None, &mut hasher)
            .map_err(Error::CollectPaths)?;
        let mut analysis = analysis::Chain::new(self.paths, &self.analysis, &self.collector, &mut hasher);
        analysis.process(paths).map_err(Error::Analysis)?;
        timing.finish(timer);

        let timer = timing.begin(timing::Kind::Emit);
        let packages = self
            .packages
            .iter()
            .map(|(name, package)| {
                let bucket = analysis.buckets.remove(name).unwrap_or_default();
                emit::Package::new_with_architecture(
                    name,
                    &self.identity,
                    package,
                    bucket,
                    self.build_release,
                    self.architecture,
                    self.jobs,
                )
            })
            .collect::<Vec<_>>();
        if let Some(name) = analysis.buckets.keys().next() {
            return Err(Error::UnexpectedAnalyzedOutput(name.clone()));
        }
        emit::emit_frozen(
            self.paths,
            &self.identity,
            &self.recipe_fingerprint,
            self.manifest_build_inputs.clone(),
            self.architecture,
            &packages,
            &self.derivation_id,
        )
        .map_err(Error::Emit)?;
        timing.finish(timer);
        Ok(())
    }
}

fn frozen_architecture(value: &str) -> crate::Architecture {
    match value {
        "x86_64" => crate::Architecture::X86_64,
        "x86" => crate::Architecture::X86,
        "aarch64" => crate::Architecture::Aarch64,
        "riscv64" => crate::Architecture::Riscv64,
        _ => unreachable!("artifact architecture was validated before freeze"),
    }
}

impl Packager {
    pub fn new(paths: &Paths, recipe: &Recipe) -> Result<Self, Error> {
        let mut collector = Collector::new(paths.install().guest);
        let packages = resolve_packages(recipe, &mut collector)?;

        Ok(Self { collector, packages })
    }

    pub(crate) fn resolved_packages(&self) -> &BTreeMap<String, ResolvedOutput> {
        &self.packages
    }

    pub(crate) fn collection_rules(&self) -> impl Iterator<Item = (&str, PathRuleKind, &str)> {
        self.collector
            .rules()
            .iter()
            .map(|rule| (rule.package.as_str(), rule.kind, rule.pattern.as_str()))
    }
}

/// Resolve the concrete typed outputs returned by the Gluon package factory.
fn resolve_packages(recipe: &Recipe, collector: &mut Collector) -> Result<BTreeMap<String, ResolvedOutput>, Error> {
    let root_output = recipe
        .declaration
        .outputs
        .iter()
        .find(|output| output.name == "out")
        .expect("validated package has one root output");

    let mut packages = BTreeMap::new();
    for (index, output) in recipe.declaration.outputs.iter().enumerate() {
        let name = emitted_output_name(&recipe.declaration.meta.pname, &output.name);
        let package = resolved_output(output, root_output, index)?;
        for path in &output.paths {
            let (kind, pattern) = collection_rule(path);
            collector.add_rule(collect::Rule {
                pattern: pattern.to_owned(),
                package: name.clone(),
                kind,
            });
        }
        packages.insert(name, package);
    }

    Ok(packages)
}

fn emitted_output_name(pname: &str, output: &str) -> String {
    if output == "out" {
        pname.to_owned()
    } else {
        format!("{pname}-{output}")
    }
}

fn resolved_output(output: &OutputSpec, root: &OutputSpec, output_index: usize) -> Result<ResolvedOutput, Error> {
    Ok(ResolvedOutput {
        include_in_manifest: output.include_in_manifest,
        summary: output.summary.clone().or_else(|| root.summary.clone()),
        description: output.description.clone().or_else(|| root.description.clone()),
        provides_exclude: output.provides_exclude.clone(),
        runtime_inputs: output
            .runtime_inputs
            .iter()
            .enumerate()
            .map(|(index, dependency)| {
                dependency.dependency().map_err(|source| Error::InvalidDependency {
                    field: format!("outputs[{output_index}].runtime_inputs[{index}]"),
                    value: format!("{dependency:?}"),
                    source,
                })
            })
            .collect::<Result<_, _>>()?,
        runtime_exclude: output.runtime_exclude.clone(),
        conflicts: output
            .conflicts
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                provider.provider().map_err(|source| Error::InvalidProvider {
                    field: format!("outputs[{output_index}].conflicts[{index}]"),
                    value: format!("{provider:?}"),
                    source,
                })
            })
            .collect::<Result<_, _>>()?,
    })
}

fn collection_rule(path: &stone_recipe::PathSpec) -> (PathRuleKind, &str) {
    match path {
        stone_recipe::PathSpec::Any { path } => (PathRuleKind::Any, path),
        stone_recipe::PathSpec::Exe { path } => (PathRuleKind::Executable, path),
        stone_recipe::PathSpec::Symlink { path } => (PathRuleKind::Symlink, path),
        stone_recipe::PathSpec::Special { path } => (PathRuleKind::Special, path),
    }
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
    #[error("collect install paths")]
    CollectPaths(#[source] collect::Error),
    #[error("analyzing paths")]
    Analysis(#[source] analysis::BoxError),
    #[error("emit packages")]
    Emit(#[from] emit::Error),
    #[error("{field}: invalid dependency `{value}`")]
    InvalidDependency {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
    #[error("{field}: invalid provider `{value}`")]
    InvalidProvider {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
    #[error("invalid frozen derivation plan")]
    InvalidFrozenPlan(#[source] stone_recipe::derivation::DerivationValidationError),
    #[error("frozen output {0} is missing")]
    MissingFrozenOutput(String),
    #[error("analysis produced undeclared output {0}")]
    UnexpectedAnalyzedOutput(String),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use stone_recipe::build_policy::AnalyzerKind;
    use stone_recipe::derivation::{
        AnalysisToolchain, CollectionRulePlan, OutputPlan, PathRuleKind, RelationKind, RelationPlan,
    };

    #[test]
    fn package_factory_defaults_resolve_directly() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());

        let packages = resolve_packages(&recipe, &mut collector).unwrap();

        // Golden split policy is now returned as typed values by mk_package.
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
        assert!(root.include_in_manifest);
        assert_eq!(
            rules
                .iter()
                .filter(|rule| rule.package == "hello")
                .map(|rule| (rule.kind, rule.pattern.as_str()))
                .collect::<Vec<_>>(),
            [(PathRuleKind::Any, "*")]
        );
        assert!(!packages["hello-dbginfo"].include_in_manifest);
        assert!(!packages["hello-32bit-dbginfo"].include_in_manifest);

        let devel = &packages["hello-devel"];
        assert_eq!(devel.summary.as_deref(), Some("Development files for hello"));
        assert_eq!(
            devel.description.as_deref(),
            Some("Install this package if you intend to build software against\nthe hello package.")
        );
        assert_eq!(
            devel.runtime_inputs.iter().map(Dependency::to_name).collect::<Vec<_>>(),
            ["hello"]
        );
        assert_eq!(
            rules
                .iter()
                .filter(|rule| rule.package == "hello-devel")
                .map(|rule| rule.pattern.as_str())
                .collect::<Vec<_>>(),
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
    fn frozen_packager_uses_only_plan_outputs_rules_analysis_and_identity() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.package.name = "frozen".to_owned();
        plan.package.homepage = "https://frozen.invalid".to_owned();
        plan.package.architecture = "x86".to_owned();
        plan.build_lock.target_platform.architecture = "x86".to_owned();
        plan.package.licenses = vec!["MIT".to_owned()];
        plan.analysis = AnalysisPlan {
            handlers: vec![
                AnalyzerKind::IgnoreBlocked,
                AnalyzerKind::Binary,
                AnalyzerKind::Elf,
                AnalyzerKind::PkgConfig,
                AnalyzerKind::Python,
                AnalyzerKind::CMake,
                AnalyzerKind::CompressMan,
                AnalyzerKind::IncludeAny,
            ],
            toolchain: AnalysisToolchain::Gnu,
            debug: true,
            strip: false,
            compress_man: false,
            remove_libtool: false,
        };
        plan.manifest_build_inputs = vec![RelationPlan {
            kind: RelationKind::Binary,
            name: "frozen-build-input".to_owned(),
        }];
        plan.outputs = vec![OutputPlan {
            name: "out".to_owned(),
            package_name: "frozen".to_owned(),
            include_in_manifest: true,
            summary: Some("Frozen output".to_owned()),
            description: Some("Only plan data".to_owned()),
            provides_exclude: vec!["excluded-provider".to_owned()],
            runtime_exclude: vec!["excluded-runtime".to_owned()],
            runtime_inputs: Vec::new(),
            conflicts: vec![RelationPlan {
                kind: RelationKind::PkgConfig,
                name: "conflict".to_owned(),
            }],
        }];
        plan.collection_rules = vec![
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            },
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/*".to_owned(),
            },
        ];
        plan.validate().unwrap();
        let expected_id = plan.derivation_id();

        let packager = FrozenPackager::from_plan(&paths, &plan).unwrap();
        assert_eq!(packager.identity.name, "frozen");
        assert_eq!(packager.identity.homepage, "https://frozen.invalid");
        assert_eq!(packager.architecture, crate::Architecture::X86);
        assert_eq!(packager.analysis, plan.analysis);
        assert_eq!(
            packager
                .manifest_build_inputs
                .iter()
                .map(Dependency::to_name)
                .collect::<Vec<_>>(),
            ["binary(frozen-build-input)"]
        );
        assert_eq!(packager.derivation_id, expected_id);
        assert_eq!(
            packager
                .collector
                .rules()
                .iter()
                .map(|rule| (rule.package.as_str(), rule.kind, rule.pattern.as_str()))
                .collect::<Vec<_>>(),
            [
                ("frozen", PathRuleKind::Any, "*"),
                ("frozen", PathRuleKind::Executable, "/usr/bin/*"),
            ]
        );
        let output = &packager.packages["frozen"];
        assert_eq!(output.summary.as_deref(), Some("Frozen output"));
        assert_eq!(
            output.conflicts.iter().map(Provider::to_name).collect::<Vec<_>>(),
            ["pkgconfig(conflict)"]
        );
    }

    #[test]
    fn frozen_packager_rejects_runtime_and_plan_layout_mismatch() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.layout.hostname = "different-builder".to_owned();
        plan.validate().unwrap();

        assert!(matches!(
            FrozenPackager::from_plan(&paths, &plan),
            Err(Error::FrozenLayoutMismatch)
        ));
    }
}
