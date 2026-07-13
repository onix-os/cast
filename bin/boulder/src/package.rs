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

pub struct Packager {
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
}

/// One path selection rule after pure package-factory composition. The
/// derivation vocabulary is retained so planning can copy it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPath {
    pub(crate) pattern: String,
    pub(crate) kind: PathRuleKind,
}

/// One emitted package resolved from a direct package-v2 output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedOutput {
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) provides_exclude: Vec<String>,
    pub(crate) runtime_inputs: Vec<Dependency>,
    pub(crate) runtime_exclude: Vec<String>,
    pub(crate) paths: Vec<ResolvedPath>,
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
                        OutputRelation::Locked { request, .. } => Ok(request.clone()),
                        OutputRelation::Planned { output } => output_packages
                            .get(output.as_str())
                            .map(|package| (*package).to_owned())
                            .ok_or_else(|| Error::MissingFrozenOutput(output.clone())),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    output.package_name.clone(),
                    ResolvedOutput {
                        summary: output.summary.clone(),
                        description: output.description.clone(),
                        provides_exclude: output.provides_exclude.clone(),
                        runtime_inputs: run_deps
                            .into_iter()
                            .enumerate()
                            .map(|(index, value)| {
                                parse_dependency(format!("outputs[{}].runtime_inputs[{index}]", output.name), &value)
                            })
                            .collect::<Result<_, _>>()?,
                        runtime_exclude: output.runtime_exclude.clone(),
                        paths: output
                            .paths
                            .iter()
                            .map(|path| ResolvedPath {
                                pattern: path.pattern.clone(),
                                kind: path.kind,
                            })
                            .collect(),
                        conflicts: output
                            .conflicts
                            .iter()
                            .enumerate()
                            .map(|(index, value)| {
                                parse_provider(format!("outputs[{}].conflicts[{index}]", output.name), value)
                            })
                            .collect::<Result<_, _>>()?,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, Error>>()?;

        let mut collector = Collector::new(paths.install().guest);
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
            .enumerate()
            .map(|(index, value)| parse_dependency(format!("manifest_build_inputs[{index}]"), value))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            paths,
            identity: plan.package.clone(),
            packages,
            collector,
            build_release: NonZeroU64::new(plan.package.build_release).expect("validated build release"),
            recipe_fingerprint: plan.recipe_fingerprint.clone(),
            analysis: plan.analysis.clone(),
            architecture: parse_frozen_architecture(&plan.package.architecture)?,
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
            .filter_map(|(name, package)| {
                let bucket = analysis.buckets.remove(name)?;
                Some(emit::Package::new_with_architecture(
                    name,
                    &self.identity,
                    package,
                    bucket,
                    self.build_release,
                    self.architecture,
                    self.jobs,
                ))
            })
            .collect::<Vec<_>>();
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

fn parse_frozen_architecture(value: &str) -> Result<crate::Architecture, Error> {
    match value {
        "x86_64" => Ok(crate::Architecture::X86_64),
        "x86" => Ok(crate::Architecture::X86),
        "aarch64" => Ok(crate::Architecture::Aarch64),
        "riscv64" => Ok(crate::Architecture::Riscv64),
        _ => Err(Error::UnsupportedFrozenArchitecture(value.to_owned())),
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
        for path in &package.paths {
            collector.add_rule(collect::Rule {
                pattern: path.pattern.clone(),
                package: name.clone(),
                kind: path.kind,
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
        paths: output.paths.iter().map(resolved_path).collect(),
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

fn resolved_path(path: &stone_recipe::PathSpec) -> ResolvedPath {
    match path {
        stone_recipe::PathSpec::Any { path } => ResolvedPath {
            pattern: path.clone(),
            kind: PathRuleKind::Any,
        },
        stone_recipe::PathSpec::Exe { path } => ResolvedPath {
            pattern: path.clone(),
            kind: PathRuleKind::Executable,
        },
        stone_recipe::PathSpec::Symlink { path } => ResolvedPath {
            pattern: path.clone(),
            kind: PathRuleKind::Symlink,
        },
        stone_recipe::PathSpec::Special { path } => ResolvedPath {
            pattern: path.clone(),
            kind: PathRuleKind::Special,
        },
    }
}

fn parse_dependency(field: String, value: &str) -> Result<Dependency, Error> {
    Dependency::from_name(value).map_err(|source| Error::InvalidDependency {
        field,
        value: value.to_owned(),
        source,
    })
}

fn parse_provider(field: String, value: &str) -> Result<Provider, Error> {
    Provider::from_name(value).map_err(|source| Error::InvalidProvider {
        field,
        value: value.to_owned(),
        source,
    })
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
    #[error("unsupported frozen artifact architecture {0}")]
    UnsupportedFrozenArchitecture(String),
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use stone_recipe::derivation::{AnalysisToolchain, CollectionRulePlan, OutputPlan, PathRuleKind, PathRulePlan};

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
        assert_eq!(
            root.paths.iter().map(|path| path.pattern.as_str()).collect::<Vec<_>>(),
            ["*"]
        );

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
            devel.paths.iter().map(|path| path.pattern.as_str()).collect::<Vec<_>>(),
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
        let paths = Paths::new(&recipe, None, runtime.path(), "/mason", output.path()).unwrap();
        let mut plan = emit::test_derivation_plan();
        plan.package.name = "frozen".to_owned();
        plan.package.homepage = "https://frozen.invalid".to_owned();
        plan.package.architecture = "x86".to_owned();
        plan.build_lock.target_platform.architecture = "x86".to_owned();
        plan.package.licenses = vec!["MIT".to_owned()];
        plan.analysis = AnalysisPlan {
            toolchain: AnalysisToolchain::Gnu,
            debug: true,
            strip: false,
            compress_man: false,
            remove_libtool: false,
        };
        plan.manifest_build_inputs = vec!["frozen-build-input".to_owned()];
        plan.outputs = vec![OutputPlan {
            name: "out".to_owned(),
            package_name: "frozen".to_owned(),
            summary: Some("Frozen output".to_owned()),
            description: Some("Only plan data".to_owned()),
            provides_exclude: vec!["excluded-provider".to_owned()],
            runtime_exclude: vec!["excluded-runtime".to_owned()],
            paths: vec![PathRulePlan {
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            }],
            runtime_inputs: Vec::new(),
            conflicts: vec!["conflict".to_owned()],
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
            ["frozen-build-input"]
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
            ["conflict"]
        );
    }
}
