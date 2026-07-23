// SPDX-FileCopyrightText: 2024 AerynOS Developers
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::PathBuf;

use stone::{
    StoneDigestWriterHasher,
    relation::{Dependency, ParseError, Provider},
};
use thiserror::Error;

use stone_recipe::{
    derivation::{AnalysisPlan, DerivationId, DerivationPlan, OutputRelation, PackageIdentity, PathRuleKind},
    package::OutputSpec,
};

use crate::{
    Paths, Recipe, Timing,
    paths::{FrozenPackagingBinding, FrozenPackagingPermit},
    timing,
};

use self::collect::Collector;

mod analysis;
mod collect;
mod emit;
mod publish;

#[allow(unused_imports)]
pub use publish::{ManifestVerification, Publication, PublishError, publish_artefacts};

#[cfg(test)]
use publish::{
    PUBLISHED_ARTEFACT_MODE, PUBLISHED_BUNDLE_MODE, PublishCheckpoint, PublishLimits, expected_bundle_files,
    publish_artefacts_with, test_rename_noreplace,
};

#[cfg(test)]
pub(crate) use emit::{set_test_compiler_cache, test_derivation_plan};

pub struct Packager {
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
}

/// One emitted package resolved from a direct package-v3 output.
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

/// Child-side packaging state prepared before frozen activation.
///
/// This value must remain descriptor-free: payload setup deliberately closes
/// every inherited host descriptor before this state is used. Host workspace
/// authentication belongs to the supervising process and crosses the boundary
/// only as [`FrozenPackagingPermit`].
pub struct FrozenPackager {
    packaging_binding: FrozenPackagingBinding,
    install_root: PathBuf,
    artifact_root: PathBuf,
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

impl FrozenPackager {
    pub fn from_plan(paths: &Paths, plan: &DerivationPlan) -> Result<Self, Error> {
        plan.validate().map_err(Error::InvalidFrozenPlan)?;
        if paths.layout() != &plan.layout {
            return Err(Error::FrozenLayoutMismatch);
        }
        let packaging_binding = paths
            .frozen_packaging_binding(plan)
            .map_err(Error::InvalidFrozenPaths)?;
        let install_root = PathBuf::from(&plan.layout.install_dir);
        let artifact_root = PathBuf::from(&plan.layout.artifacts_dir);
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

        let mut collector = Collector::new(&install_root);
        for rule in &plan.collection_rules {
            let package = output_packages
                .get(rule.output.as_str())
                .ok_or_else(|| Error::MissingFrozenOutput(rule.output.clone()))?;
            collector
                .add_rule(&rule.pattern, package, rule.kind)
                .map_err(Error::CollectPaths)?;
        }

        let manifest_build_inputs = plan
            .manifest_build_inputs
            .iter()
            .map(|relation| relation.to_dependency())
            .collect();

        Ok(Self {
            packaging_binding,
            install_root,
            artifact_root,
            identity: plan.package.clone(),
            packages,
            collector,
            build_release: NonZeroU64::new(plan.package.build_release).expect("validated build release"),
            recipe_fingerprint: plan.provenance.recipe.sha256.clone(),
            analysis: plan.analysis.clone(),
            architecture: frozen_architecture(&plan.package.architecture),
            manifest_build_inputs,
            jobs: plan.execution.jobs,
            derivation_id: plan.derivation_id(),
        })
    }

    /// Analyze and emit under the supervisor-authorized frozen packaging binding.
    pub(crate) fn package(&self, permit: &FrozenPackagingPermit<'_>, timing: &mut Timing) -> Result<(), Error> {
        // This must remain the first fallible operation. Collection, analyzers,
        // and emission all interact with derivation-owned filesystem state.
        permit
            .require_for(&self.packaging_binding)
            .map_err(Error::InvalidPackagingPermit)?;
        let mut hasher = StoneDigestWriterHasher::new();
        let timer = timing.begin(timing::Kind::Analyze);
        let paths = self
            .collector
            .enumerate_paths(None, &mut hasher)
            .map_err(Error::CollectPaths)?;
        analysis::preflight_elf_debug_routes(&self.analysis, &self.collector, &paths).map_err(Error::Analysis)?;
        let mut analysis = analysis::Chain::new(&self.install_root, &self.analysis, &self.collector, &mut hasher);
        let sealed = analysis.process(paths).map_err(Error::Analysis)?;
        timing.finish(timer);

        let timer = timing.begin(timing::Kind::Emit);
        let packages = self
            .packages
            .iter()
            .map(|(name, package)| {
                let bucket = analysis.buckets.remove(name.as_str()).unwrap_or_default();
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
            return Err(Error::UnexpectedAnalyzedOutput(name.to_string()));
        }
        emit::emit_frozen(
            &self.artifact_root,
            &self.identity,
            &self.recipe_fingerprint,
            self.manifest_build_inputs.clone(),
            self.architecture,
            &packages,
            &self.derivation_id,
            &sealed,
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

fn stone_artefact_filename(
    package_name: &str,
    version: &str,
    source_release: u64,
    build_release: u64,
    architecture: crate::Architecture,
) -> String {
    format!("{package_name}-{version}-{source_release}-{build_release}-{architecture}.stone")
}

fn binary_manifest_filename(architecture: crate::Architecture) -> String {
    format!("manifest.{architecture}.bin")
}

fn jsonc_manifest_filename(architecture: crate::Architecture) -> String {
    format!("manifest.{architecture}.jsonc")
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
            .map(|rule| (rule.package(), rule.kind(), rule.pattern()))
    }
}

/// Resolve the concrete typed outputs returned by the Gluon package factory.
fn resolve_packages(recipe: &Recipe, collector: &mut Collector) -> Result<BTreeMap<String, ResolvedOutput>, Error> {
    let mut packages = BTreeMap::new();
    for (index, output) in recipe.declaration.outputs.iter().enumerate() {
        let name = emitted_output_name(&recipe.declaration.meta.pname, &output.name);
        let package = resolved_output(output, index)?;
        for path in &output.paths {
            let (kind, pattern) = collection_rule(path);
            collector.add_rule(pattern, &name, kind).map_err(Error::CollectPaths)?;
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

fn resolved_output(output: &OutputSpec, output_index: usize) -> Result<ResolvedOutput, Error> {
    Ok(ResolvedOutput {
        include_in_manifest: output.include_in_manifest,
        summary: output.summary.clone(),
        description: output.description.clone(),
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
    #[error("invalid frozen runtime paths")]
    InvalidFrozenPaths(#[source] std::io::Error),
    #[error("packaging is not authorized for the frozen packaging binding")]
    InvalidPackagingPermit(#[source] std::io::Error),
    #[error("frozen output {0} is missing")]
    MissingFrozenOutput(String),
    #[error("analysis produced undeclared output {0}")]
    UnexpectedAnalyzedOutput(String),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
}

#[cfg(test)]
mod tests;
