// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use stone::{
    StoneDigestWriterHasher,
    relation::{Dependency, ParseError, Provider},
};
use thiserror::Error;

use stone_recipe::{
    derivation::{AnalysisPlan, DerivationId, DerivationPlan, OutputRelation, PackageIdentity, PathRuleKind},
    package::OutputSpec,
};

use crate::{Paths, Recipe, Timing, timing};

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
pub(crate) use emit::test_derivation_plan;

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
            paths,
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

    pub fn package(&self, timing: &mut Timing) -> Result<(), Error> {
        let mut hasher = StoneDigestWriterHasher::new();
        let timer = timing.begin(timing::Kind::Analyze);
        let paths = self
            .collector
            .enumerate_paths(None, &mut hasher)
            .map_err(Error::CollectPaths)?;
        let mut analysis = analysis::Chain::new(self.paths, &self.analysis, &self.collector, &mut hasher);
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
            self.paths,
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
    #[error("frozen output {0} is missing")]
    MissingFrozenOutput(String),
    #[error("analysis produced undeclared output {0}")]
    UnexpectedAnalyzedOutput(String),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use fs_err as fs;

    use super::*;
    use stone_recipe::build_policy::AnalyzerKind;
    use stone_recipe::derivation::{
        AnalysisToolsPlan, CollectionRulePlan, ExecutablePlan, OutputPlan, PathRuleKind, RelationKind, RelationPlan,
    };

    fn frozen_analyzer_tool(name: &str) -> ExecutablePlan {
        ExecutablePlan {
            path: format!("/usr/bin/{name}"),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: name.to_owned(),
            },
        }
    }

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
            rules.last().map(|rule| (rule.package(), rule.pattern())),
            Some(("hello-demos", "/usr/lib/qt*/examples"))
        );
        assert_ne!(
            rules.last().map(|rule| rule.package()),
            packages.keys().last().map(String::as_str),
            "collector precedence must retain composition order rather than package-map order"
        );

        let root = &packages["hello"];
        assert_eq!(root.summary.as_deref(), Some("Minimal Gluon recipe example"));
        assert!(root.include_in_manifest);
        assert_eq!(
            rules
                .iter()
                .filter(|rule| rule.package() == "hello")
                .map(|rule| (rule.kind(), rule.pattern()))
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
                .filter(|rule| rule.package() == "hello-devel")
                .map(|rule| rule.pattern())
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
    fn resolved_outputs_do_not_inherit_root_metadata() {
        let mut recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let root = recipe
            .declaration
            .outputs
            .iter_mut()
            .find(|output| output.name == "out")
            .unwrap();
        root.summary = Some("Root summary only".to_owned());
        root.description = Some("Root description only".to_owned());
        let split = recipe
            .declaration
            .outputs
            .iter_mut()
            .find(|output| output.name == "libs")
            .unwrap();
        split.summary = None;
        split.description = None;

        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());
        let packages = resolve_packages(&recipe, &mut collector).unwrap();

        assert_eq!(packages["hello"].summary.as_deref(), Some("Root summary only"));
        assert_eq!(packages["hello"].description.as_deref(), Some("Root description only"));
        assert_eq!(packages["hello-libs"].summary, None);
        assert_eq!(packages["hello-libs"].description, None);
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
            tools: AnalysisToolsPlan {
                pkg_config: Some(frozen_analyzer_tool("pkg-config")),
                python: Some(frozen_analyzer_tool("python3")),
                objcopy: Some(frozen_analyzer_tool("objcopy")),
                strip: None,
            },
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
        assert_eq!(packager.recipe_fingerprint, plan.provenance.recipe.sha256);
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
                .map(|rule| (rule.package(), rule.kind(), rule.pattern()))
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

    fn publication_fixture() -> (tempfile::TempDir, DerivationPlan, Paths) {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        fs::create_dir(&output).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let plan = test_derivation_plan();
        let mut paths = Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap();
        paths.bind_to_plan(&plan).unwrap();
        fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&paths.artefacts().host, std::fs::Permissions::from_mode(0o700)).unwrap();
        (root, plan, paths)
    }

    fn publish_artefacts(paths: &Paths, plan: &DerivationPlan) -> Result<Publication, PublishError> {
        let execution_lock = paths
            .acquire_execution_lock(plan)
            .map_err(PublishError::InvalidExecutionLock)?;
        super::publish_artefacts(paths, plan, &execution_lock, ManifestVerification::None)
    }

    fn publish_artefacts_with<F>(
        paths: &Paths,
        plan: &DerivationPlan,
        limits: PublishLimits,
        hook: F,
    ) -> Result<Publication, PublishError>
    where
        F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
    {
        let execution_lock = paths
            .acquire_execution_lock(plan)
            .map_err(PublishError::InvalidExecutionLock)?;
        super::publish_artefacts_with(paths, plan, &execution_lock, ManifestVerification::None, limits, hook)
    }

    fn publish_artefacts_verifying(
        paths: &Paths,
        plan: &DerivationPlan,
        expected: &Path,
    ) -> Result<Publication, PublishError> {
        let execution_lock = paths
            .acquire_execution_lock(plan)
            .map_err(PublishError::InvalidExecutionLock)?;
        super::publish_artefacts(
            paths,
            plan,
            &execution_lock,
            ManifestVerification::ExactBinary(expected),
        )
    }

    fn publish_artefacts_verifying_with<F>(
        paths: &Paths,
        plan: &DerivationPlan,
        expected: &Path,
        limits: PublishLimits,
        hook: F,
    ) -> Result<Publication, PublishError>
    where
        F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
    {
        let execution_lock = paths
            .acquire_execution_lock(plan)
            .map_err(PublishError::InvalidExecutionLock)?;
        super::publish_artefacts_with(
            paths,
            plan,
            &execution_lock,
            ManifestVerification::ExactBinary(expected),
            limits,
            hook,
        )
    }

    fn output_entries(paths: &Paths) -> Vec<OsString> {
        let mut entries = fs::read_dir(paths.output_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn stage_expected_bundle(plan: &DerivationPlan, paths: &Paths) -> Vec<OsString> {
        let names = expected_bundle_files(plan).into_iter().collect::<Vec<_>>();
        for name in &names {
            let path = paths.artefacts().host.join(name);
            fs::write(&path, b"frozen artefact bytes").unwrap();
            fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        names
    }

    fn stone_name(names: &[OsString]) -> &OsString {
        names
            .iter()
            .find(|name| name.to_string_lossy().ends_with(".stone"))
            .unwrap()
    }

    fn binary_manifest_name(names: &[OsString]) -> &OsString {
        names
            .iter()
            .find(|name| name.to_string_lossy().ends_with(".bin"))
            .unwrap()
    }

    fn reference_path(root: &Path, label: &str) -> PathBuf {
        let parent = root.join(label);
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        parent.join("expected.bin")
    }

    fn reference_manifest(root: &Path, bytes: &[u8]) -> PathBuf {
        let path = reference_path(root, "verification-reference");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        path
    }

    fn seal_test_bundle_directory(path: &Path, plan: &DerivationPlan) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
        fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_BUNDLE_MODE)).unwrap();
    }

    #[test]
    fn frozen_bundle_contract_names_every_declared_output_and_both_manifests() {
        let mut plan = test_derivation_plan();
        plan.outputs.push(OutputPlan {
            name: "dev".to_owned(),
            package_name: "example-devel".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
        });
        plan.validate().unwrap();

        assert_eq!(
            expected_bundle_files(&plan).into_iter().collect::<Vec<_>>(),
            [
                OsString::from("example-1.2.3-1-1-x86_64.stone"),
                OsString::from("example-devel-1.2.3-1-1-x86_64.stone"),
                OsString::from("manifest.x86_64.bin"),
                OsString::from("manifest.x86_64.jsonc"),
            ]
        );
    }

    #[test]
    fn publication_requires_the_execution_lock_for_its_exact_derivation_workspace() {
        let (_root, plan, paths) = publication_fixture();
        let (_other_root, other_plan, other_paths) = publication_fixture();
        let wrong_lock = other_paths.acquire_execution_lock(&other_plan).unwrap();

        let error = super::publish_artefacts(&paths, &plan, &wrong_lock, ManifestVerification::None).unwrap_err();

        assert!(matches!(error, PublishError::InvalidExecutionLock(_)));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn publication_rejects_group_or_other_writable_roots() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o775)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::WritableRoot {
                role: "output",
                found: 0o775,
                ..
            }
        ));
        assert!(output_entries(&paths).is_empty());

        fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&paths.artefacts().host, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::WritableRoot {
                role: "staged",
                found: 0o777,
                ..
            }
        ));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn publishes_and_reuses_one_complete_derivation_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        let names = stage_expected_bundle(&plan, &paths);
        assert_eq!(
            names,
            [
                OsString::from("example-1.2.3-1-1-x86_64.stone"),
                OsString::from("manifest.x86_64.bin"),
                OsString::from("manifest.x86_64.jsonc"),
            ]
        );
        let package = staged.join(stone_name(&names));

        assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Published);

        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert_eq!(
            fs::metadata(&bundle).unwrap().permissions().mode() & 0o7777,
            PUBLISHED_BUNDLE_MODE
        );
        for name in &names {
            assert_eq!(fs::read(bundle.join(name)).unwrap(), b"frozen artefact bytes");
            let metadata = fs::metadata(bundle.join(name)).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o7777, PUBLISHED_ARTEFACT_MODE);
            assert_eq!(metadata.mtime(), plan.source_date_epoch);
            assert_eq!(metadata.mtime_nsec(), 0);
        }
        assert_ne!(
            fs::metadata(&package).unwrap().ino(),
            fs::metadata(bundle.join(stone_name(&names))).unwrap().ino(),
            "published files must not retain mutable staging inodes"
        );
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
        assert!(!paths.output_dir().join(stone_name(&names)).exists());

        assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Reused);
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn publication_normalizes_authenticated_creation_modes_under_adverse_umask() {
        const CHILD: &str = "MASON_PUBLICATION_UMASK_TEST_CHILD";
        const TEST: &str = "package::tests::publication_normalizes_authenticated_creation_modes_under_adverse_umask";

        // umask is process-global. Isolate the mutation in a child test process
        // so this regression cannot race unrelated tests in the harness.
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "adverse-umask child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        // Removes owner-write at creation: the directory starts as 0500 and
        // files as 0400. Publication must authenticate each descriptor before
        // restoring its private construction mode and sealing it read-only.
        // SAFETY: this is the sole test selected in the isolated child process.
        let previous = unsafe { nix::libc::umask(0o277) };
        let result = publish_artefacts(&paths, &plan);
        // SAFETY: restore the child process mask before assertions can panic.
        unsafe { nix::libc::umask(previous) };

        assert_eq!(result.unwrap(), Publication::Published);
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert_eq!(
            fs::metadata(&bundle).unwrap().permissions().mode() & 0o7777,
            PUBLISHED_BUNDLE_MODE
        );
        assert!(names.iter().all(|name| {
            fs::metadata(bundle.join(name)).unwrap().permissions().mode() & 0o7777 == PUBLISHED_ARTEFACT_MODE
        }));
    }

    #[test]
    fn mismatched_existing_bundle_is_never_modified() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        let names = stage_expected_bundle(&plan, &paths);
        let package_name = stone_name(&names);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());

        let staged_package = staged.join(package_name);
        fs::set_permissions(&staged_package, std::fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&staged_package, b"different").unwrap();
        fs::set_permissions(
            &staged_package,
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::ContentMismatch { .. }));
        assert_eq!(fs::read(bundle.join(package_name)).unwrap(), b"frozen artefact bytes");

        fs::set_permissions(&staged_package, std::fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&staged_package, b"frozen artefact bytes").unwrap();
        fs::set_permissions(
            &staged_package,
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        fs::write(staged.join("extra.stone"), b"extra").unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::FrozenFileSetMismatch { role: "staged", .. }
        ));
        assert!(!bundle.join("extra.stone").exists());
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn missing_or_extra_staged_files_are_rejected_before_publication() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged = paths.artefacts().host;
        let missing = names[0].clone();
        fs::remove_file(staged.join(&missing)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "staged",
                expected,
                found,
                ..
            } if expected.contains(&missing) && !found.contains(&missing)
        ));
        assert!(output_entries(&paths).is_empty());

        fs::write(staged.join(&missing), b"frozen artefact bytes").unwrap();
        fs::set_permissions(
            staged.join(&missing),
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        let extra = OsString::from("undeclared-debug-output.stone");
        fs::write(staged.join(&extra), b"undeclared").unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "staged",
                expected,
                found,
                ..
            } if !expected.contains(&extra) && found.contains(&extra)
        ));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn unsealed_staged_modes_are_rejected_before_publication() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged_path = paths.artefacts().host.join(&names[0]);
        fs::set_permissions(&staged_path, std::fs::Permissions::from_mode(0o6755)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ModeMismatch {
                role: "staged",
                expected: PUBLISHED_ARTEFACT_MODE,
                found: 0o6755,
                ..
            }
        ));
        assert_eq!(fs::metadata(staged_path).unwrap().permissions().mode() & 0o7777, 0o6755);
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn existing_bundle_mode_mismatch_is_never_reused() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        let published = bundle.join(&names[0]);
        fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ModeMismatch {
                role: "published",
                expected: PUBLISHED_ARTEFACT_MODE,
                found: 0o600,
                ..
            }
        ));
        assert_eq!(fs::metadata(published).unwrap().permissions().mode() & 0o7777, 0o600);
    }

    #[test]
    fn existing_bundle_directory_mode_mismatch_is_never_reused() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ModeMismatch {
                role: "published bundle",
                expected: PUBLISHED_BUNDLE_MODE,
                found: 0o700,
                ..
            }
        ));
        assert_eq!(fs::metadata(bundle).unwrap().permissions().mode() & 0o7777, 0o700);
    }

    #[test]
    fn existing_bundle_file_set_must_still_match_the_frozen_plan() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        let missing = names[0].clone();
        fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(bundle.join(&missing)).unwrap();
        seal_test_bundle_directory(&bundle, &plan);

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "published",
                expected,
                found,
                ..
            } if expected.contains(&missing) && !found.contains(&missing)
        ));

        fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(bundle.join(&missing), b"frozen artefact bytes").unwrap();
        fs::set_permissions(
            bundle.join(&missing),
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        let extra = OsString::from("undeclared-published-file");
        fs::write(bundle.join(&extra), b"extra").unwrap();
        seal_test_bundle_directory(&bundle, &plan);

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "published",
                expected,
                found,
                ..
            } if !expected.contains(&extra) && found.contains(&extra)
        ));
    }

    #[test]
    fn rejects_non_regular_or_nested_staged_entries_without_a_final_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        fs::create_dir(staged.join("nested")).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::FrozenFileSetMismatch { role: "staged", .. }
        ));
        assert!(output_entries(&paths).is_empty());

        fs::remove_dir(staged.join("nested")).unwrap();
        let names = stage_expected_bundle(&plan, &paths);
        let replaced = staged.join(&names[0]);
        fs::remove_file(&replaced).unwrap();
        symlink("missing", &replaced).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(output_entries(&paths).is_empty());

        fs::remove_file(&replaced).unwrap();
        nix::unistd::mkfifo(&replaced, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn rejects_unexpected_entries_in_an_existing_final_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let package_name = stone_name(&names);
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        fs::create_dir(&bundle).unwrap();
        symlink("missing", bundle.join(package_name)).unwrap();
        seal_test_bundle_directory(&bundle, &plan);

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::FrozenFileSetMismatch { role: "published", .. }
        ));
        assert!(
            fs::symlink_metadata(bundle.join(package_name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    fn create_competing_bundle(plan: &DerivationPlan, paths: &Paths, mismatched: bool) -> Vec<OsString> {
        let names = expected_bundle_files(plan).into_iter().collect::<Vec<_>>();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        fs::create_dir(&bundle).unwrap();
        for (index, name) in names.iter().enumerate() {
            let source = paths.artefacts().host.join(name);
            let target = bundle.join(name);
            let mut bytes = fs::read(source).unwrap();
            if mismatched && index == 0 {
                bytes.fill(b'X');
            }
            fs::write(&target, bytes).unwrap();
            fs::set_permissions(&target, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
            filetime::set_file_mtime(&target, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
        }
        seal_test_bundle_directory(&bundle, plan);
        names
    }

    #[test]
    fn verified_manifest_publishes_and_reuses_exact_bytes() {
        let (root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");

        assert_eq!(
            publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
            Publication::Published
        );
        assert_eq!(
            publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
            Publication::Reused
        );
        let published_manifest = paths
            .output_dir()
            .join(plan.derivation_id().as_str())
            .join(binary_manifest_name(&names));
        assert_eq!(
            publish_artefacts_verifying(&paths, &plan, &published_manifest).unwrap(),
            Publication::Reused,
            "a published manifest is a useful independent reference for staged bytes"
        );
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn manifest_mismatch_rolls_back_new_output_and_preserves_reused_output() {
        let (root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"different artefact bytes");

        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(error, PublishError::ManifestVerificationMismatch { .. }));
        assert!(output_entries(&paths).is_empty());

        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        let manifest = bundle.join(binary_manifest_name(&names));
        let corrupted = vec![b'X'; b"frozen artefact bytes".len()];
        fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&manifest, &corrupted).unwrap();
        fs::set_permissions(&manifest, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        filetime::set_file_mtime(&manifest, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
        fs::write(&expected, b"frozen artefact bytes").unwrap();
        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(error, PublishError::ManifestVerificationMismatch { .. }));
        assert_eq!(fs::read(manifest).unwrap(), corrupted);
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn manifest_verification_limit_accepts_n_rejects_n_plus_one_and_expires() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let bytes = b"frozen artefact bytes";
        let expected = reference_manifest(root.path(), bytes);
        let limits = PublishLimits::with_manifest_verification(bytes.len() as u64, Duration::from_secs(30));
        assert_eq!(
            publish_artefacts_verifying_with(&paths, &plan, &expected, limits, |_| Ok(())).unwrap(),
            Publication::Published
        );

        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let oversized = reference_manifest(root.path(), b"frozen artefact bytes+");
        let limit = b"frozen artefact bytes".len() as u64;
        let error = publish_artefacts_verifying_with(
            &paths,
            &plan,
            &oversized,
            PublishLimits::with_manifest_verification(limit, Duration::from_secs(30)),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(
            matches!(error, PublishError::ArtifactTooLarge { maximum, found, .. } if maximum == limit && found == limit + 1)
        );
        assert!(output_entries(&paths).is_empty());

        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), bytes);
        let error = publish_artefacts_verifying_with(
            &paths,
            &plan,
            &expected,
            PublishLimits::with_manifest_verification(bytes.len() as u64, Duration::ZERO),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, PublishError::Deadline { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn manifest_reference_rejects_symlink_directory_fifo_and_socket_without_blocking() {
        fn assert_rejected<F>(label: &str, make: F)
        where
            F: FnOnce(&Path),
        {
            let (root, plan, paths) = publication_fixture();
            stage_expected_bundle(&plan, &paths);
            let expected = reference_path(root.path(), label);
            make(&expected);
            let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
            assert!(matches!(
                error,
                PublishError::UnexpectedEntry {
                    role: "expected manifest",
                    ..
                }
            ));
            assert!(output_entries(&paths).is_empty());
        }

        assert_rejected("reference-symlink", |expected| symlink("missing", expected).unwrap());
        assert_rejected("reference-directory", |expected| fs::create_dir(expected).unwrap());
        assert_rejected("reference-fifo", |expected| {
            nix::unistd::mkfifo(expected, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
        });
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_path(root.path(), "reference-socket");
        match std::os::unix::net::UnixListener::bind(&expected) {
            Ok(listener) => {
                drop(listener);
                let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
                assert!(matches!(
                    error,
                    PublishError::UnexpectedEntry {
                        role: "expected manifest",
                        ..
                    }
                ));
                assert!(output_entries(&paths).is_empty());
            }
            // Some CI sandboxes prohibit AF_UNIX creation. The production
            // rejection is still exercised whenever the kernel permits the
            // fixture; FIFO covers the nonblocking special-file path always.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
            Err(error) => panic!("create manifest reference socket: {error}"),
        }
    }

    #[test]
    fn manifest_reference_cannot_alias_the_staged_manifest() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let expected = paths.artefacts().host.join(binary_manifest_name(&names));

        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(error, PublishError::ReferenceAliasesStagedManifest { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn manifest_reference_accepts_protected_hardlinks_and_trusted_owners() {
        assert!(publish::reference_owner_is_trusted(1000, 1000));
        assert!(publish::reference_owner_is_trusted(0, 1000));
        assert!(!publish::reference_owner_is_trusted(1001, 1000));

        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_path(root.path(), "hardlinked-reference");
        let original = expected.parent().unwrap().join("original");
        fs::write(&original, b"frozen artefact bytes").unwrap();
        fs::set_permissions(&original, std::fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&original, &expected).unwrap();
        assert_eq!(fs::metadata(&expected).unwrap().nlink(), 2);

        assert_eq!(
            publish_artefacts_verifying(&paths, &plan, &expected).unwrap(),
            Publication::Published
        );
    }

    #[test]
    fn manifest_reference_rejects_group_or_other_writable_parent_and_file() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");
        fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o666)).unwrap();
        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(
            error,
            PublishError::WritableReferenceManifest { found: 0o666, .. }
        ));
        assert!(output_entries(&paths).is_empty());

        fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(expected.parent().unwrap(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let error = publish_artefacts_verifying(&paths, &plan, &expected).unwrap_err();
        assert!(matches!(
            error,
            PublishError::WritableRoot {
                role: "expected manifest parent",
                found: 0o770,
                ..
            }
        ));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn same_inode_reference_mutation_before_rename_rolls_back() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");
        let mut mutated = false;

        let error =
            publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
                if checkpoint == PublishCheckpoint::BeforeRename {
                    mutated = true;
                    fs::write(&expected, b"changed artefact bytes").unwrap();
                }
                Ok(())
            })
            .unwrap_err();
        assert!(mutated);
        assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn replaced_reference_path_is_rejected_before_publication() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");
        let mut replaced = false;

        let error =
            publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
                if checkpoint == PublishCheckpoint::SourcesPinned {
                    replaced = true;
                    fs::remove_file(&expected).unwrap();
                    fs::write(&expected, b"frozen artefact bytes").unwrap();
                }
                Ok(())
            })
            .unwrap_err();
        assert!(replaced);
        assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn reference_change_after_rename_removes_the_new_final_bundle() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");
        let mut mutated = false;

        let error =
            publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
                if checkpoint == PublishCheckpoint::AfterRename {
                    mutated = true;
                    fs::write(&expected, b"changed artefact bytes").unwrap();
                }
                Ok(())
            })
            .unwrap_err();
        assert!(mutated);
        assert!(matches!(error, PublishError::ReferenceManifestChanged { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn staged_manifest_mutation_before_rename_rolls_back_verified_publication() {
        let (root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged = paths.artefacts().host.join(binary_manifest_name(&names));
        let expected = reference_manifest(root.path(), b"frozen artefact bytes");

        let error =
            publish_artefacts_verifying_with(&paths, &plan, &expected, PublishLimits::default(), |checkpoint| {
                if checkpoint == PublishCheckpoint::BeforeRename {
                    fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
                    fs::write(&staged, b"changed artefact bytes").unwrap();
                    fs::set_permissions(&staged, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
                }
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, PublishError::ArtifactChanged { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn publication_limits_accept_exact_n_and_reject_n_plus_one() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let file_bytes = fs::metadata(paths.artefacts().host.join(&names[0])).unwrap().len();
        let aggregate = file_bytes * names.len() as u64;
        let limits = PublishLimits::with_file_and_bundle_bytes(file_bytes, aggregate);
        assert_eq!(
            publish_artefacts_with(&paths, &plan, limits, |_| Ok(())).unwrap(),
            Publication::Published
        );

        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let error = publish_artefacts_with(
            &paths,
            &plan,
            PublishLimits::with_file_and_bundle_bytes(file_bytes - 1, aggregate),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, PublishError::ArtifactTooLarge { maximum, found, .. } if maximum + 1 == found));

        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let error = publish_artefacts_with(
            &paths,
            &plan,
            PublishLimits::with_file_and_bundle_bytes(file_bytes, aggregate - 1),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, PublishError::BundleTooLarge { maximum, found } if maximum + 1 == found));

        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        assert_eq!(
            publish_artefacts_with(&paths, &plan, PublishLimits::with_max_artefacts(3), |_| Ok(())).unwrap(),
            Publication::Published
        );
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let error =
            publish_artefacts_with(&paths, &plan, PublishLimits::with_max_artefacts(2), |_| Ok(())).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ResourceLimit {
                resource: "published artefact count",
                limit: 2
            }
        ));
    }

    #[test]
    fn same_inode_staged_mutation_before_rename_rolls_back_every_owned_output() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let source = paths.artefacts().host.join(&names[0]);
        let length = fs::metadata(&source).unwrap().len() as usize;
        let mut mutated = false;
        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::BeforeRename {
                mutated = true;
                fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&source, vec![b'X'; length]).unwrap();
                fs::set_permissions(&source, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
            }
            Ok(())
        })
        .unwrap_err();
        assert!(mutated);
        assert!(matches!(error, PublishError::ArtifactChanged { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn replaced_output_root_is_rejected_before_any_publication_write() {
        let (root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let output = paths.output_dir().clone();
        let displaced = root.path().join("displaced-output");
        let mut replaced = false;
        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::SourcesPinned {
                replaced = true;
                fs::rename(&output, &displaced).unwrap();
                fs::create_dir(&output).unwrap();
                fs::write(output.join("sentinel"), b"replacement").unwrap();
            }
            Ok(())
        })
        .unwrap_err();
        assert!(replaced);
        assert!(matches!(error, PublishError::OwnershipChanged { .. }));
        assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"replacement");
        assert!(fs::read_dir(displaced).unwrap().next().is_none());
    }

    #[test]
    fn exact_concurrent_bundle_is_reused_and_private_stage_is_removed() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let mut collided = false;
        let publication = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::BeforeRename {
                collided = true;
                create_competing_bundle(&plan, &paths, false);
            }
            Ok(())
        })
        .unwrap();
        assert!(collided);
        assert_eq!(publication, Publication::Reused);
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn collision_reuse_cannot_forget_the_byte_set_prepared_by_this_build() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged = paths.artefacts().host.join(&names[0]);
        let replacement = vec![b'B'; fs::metadata(&staged).unwrap().len() as usize];
        let mut collided = false;

        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::BeforeRename {
                collided = true;
                fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&staged, &replacement).unwrap();
                fs::set_permissions(&staged, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
                create_competing_bundle(&plan, &paths, false);
            }
            Ok(())
        })
        .unwrap_err();

        assert!(collided);
        assert!(matches!(error, PublishError::ArtifactChanged { .. }));
        let final_bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert_eq!(fs::read(final_bundle.join(&names[0])).unwrap(), replacement);
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn mismatched_concurrent_bundle_is_preserved_and_private_stage_is_removed() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        let mut collided = false;
        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::BeforeRename {
                collided = true;
                create_competing_bundle(&plan, &paths, true);
            }
            Ok(())
        })
        .unwrap_err();
        assert!(collided);
        assert!(matches!(error, PublishError::ContentMismatch { .. }));
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert!(fs::read_dir(bundle).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mason-publish-")
        }));
    }

    #[test]
    fn reuse_rejects_same_size_mutation_between_digest_rounds() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let published = paths.output_dir().join(plan.derivation_id().as_str()).join(&names[0]);
        let length = fs::metadata(&published).unwrap().len() as usize;
        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::BeforeReuseConfirmation {
                fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&published, vec![b'Y'; length]).unwrap();
                fs::set_permissions(&published, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
            }
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(error, PublishError::ArtifactChanged { .. }));
    }

    #[test]
    fn reuse_rechecks_bytes_after_syncing_every_durability_boundary() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let published = paths.output_dir().join(plan.derivation_id().as_str()).join(&names[0]);
        let length = fs::metadata(&published).unwrap().len() as usize;
        let mut reached_post_sync_confirmation = false;

        let error = publish_artefacts_with(&paths, &plan, PublishLimits::default(), |checkpoint| {
            if checkpoint == PublishCheckpoint::AfterReuseDurabilitySync {
                reached_post_sync_confirmation = true;
                fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&published, vec![b'Z'; length]).unwrap();
                fs::set_permissions(&published, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert!(reached_post_sync_confirmation);
        assert!(matches!(error, PublishError::ArtifactChanged { .. }));
    }

    #[test]
    fn reuse_rejects_wrong_bundle_timestamp() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        filetime::set_file_mtime(
            &bundle,
            filetime::FileTime::from_unix_time(plan.source_date_epoch + 1, 0),
        )
        .unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::TimestampMismatch { expected, seconds, .. } if expected + 1 == seconds));
    }

    #[test]
    fn rename_noreplace_does_not_replace_even_an_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("complete"), b"bundle").unwrap();

        let error = test_rename_noreplace(&source, &target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.join("complete").is_file());
        assert!(target.is_dir());
        assert!(fs::read_dir(target).unwrap().next().is_none());
    }
}
