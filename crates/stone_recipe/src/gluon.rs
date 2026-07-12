// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Versioned Gluon boundary for package recipes.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source as GluonSource};
use thiserror::Error;

use crate::{
    BuildSpec, KeyValueSpec, OptionsSpec, PackageSpec, PathSpec, Recipe, RecipeConversionError, RecipeSpec, SourceSpec,
    ToolchainSpec, TuningSpec, UpstreamSpec,
};

/// Version of the authored recipe API exposed through [`GLUON_RECIPE_ABI`].
pub const RECIPE_ABI_VERSION: u32 = 1;

/// Pure Gluon definitions exposed as the embedded `boulder.recipe.v1` module.
///
/// The prelude exposes a `boulder` record containing the ABI version,
/// constructors, defaults and explicit variants. Keeping it checked in makes
/// an ABI change reviewable; the restricted importer records it independently
/// from the authored root in every evaluation fingerprint.
pub const GLUON_RECIPE_ABI: &str = include_str!("../gluon/recipe.glu");

/// A validated domain recipe together with its reproducibility metadata.
#[derive(Debug, Clone)]
pub struct EvaluatedRecipe {
    pub recipe: Recipe,
    pub fingerprint: EvaluationFingerprint,
}

/// Failure to evaluate or convert an authored Gluon recipe.
#[derive(Debug, Error)]
pub enum RecipeEvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] RecipeConversionError),
}

// Gluon's standard `Option` type is intentionally unavailable in the
// restricted VM because the standard library is not loaded. These wire DTOs
// use an ABI-owned explicit option instead. They are kept separate from
// `RecipeSpec` so no VM-specific representation leaks into the format-neutral
// conversion boundary.
#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    Unset,
    Set(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBool {
    False,
    True,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRecipeSpec {
    source: GluonSourceSpec,
    build: GluonBuildSpec,
    package: GluonPackageSpec,
    options: GluonOptionsSpec,
    profiles: Vec<GluonKeyValueSpec<GluonBuildSpec>>,
    sub_packages: Vec<GluonKeyValueSpec<GluonPackageSpec>>,
    upstreams: Vec<GluonUpstreamSpec>,
    architectures: Vec<String>,
    tuning: Vec<GluonKeyValueSpec<GluonTuningSpec>>,
    emul32: GluonBool,
    mold: GluonBool,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSourceSpec {
    name: String,
    version: String,
    release: i64,
    homepage: String,
    license: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildSpec {
    setup: GluonOptional<String>,
    build: GluonOptional<String>,
    install: GluonOptional<String>,
    check: GluonOptional<String>,
    workload: GluonOptional<String>,
    environment: GluonOptional<String>,
    build_deps: Vec<String>,
    check_deps: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPackageSpec {
    summary: GluonOptional<String>,
    description: GluonOptional<String>,
    provides_exclude: Vec<String>,
    run_deps: Vec<String>,
    run_deps_exclude: Vec<String>,
    paths: Vec<GluonPathSpec>,
    conflicts: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonOptionsSpec {
    toolchain: GluonToolchainSpec,
    cspgo: GluonBool,
    samplepgo: GluonBool,
    debug: GluonBool,
    strip: GluonBool,
    networking: GluonBool,
    compressman: GluonBool,
    lastrip: GluonBool,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonKeyValueSpec<T> {
    key: String,
    value: T,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonUpstreamSpec {
    Archive {
        url: String,
        hash: String,
        rename: GluonOptional<String>,
        strip_dirs: GluonOptional<i64>,
        unpack: GluonBool,
        unpack_dir: GluonOptional<String>,
    },
    Git {
        url: String,
        git_ref: String,
        clone_dir: GluonOptional<String>,
    },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonPathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonTuningSpec {
    Enable,
    Disable,
    Config { value: String },
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonToolchainSpec {
    Llvm,
    Gnu,
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::Unset => None,
            GluonOptional::Set(value) => Some(value),
        }
    }
}

impl From<GluonBool> for bool {
    fn from(value: GluonBool) -> Self {
        match value {
            GluonBool::False => false,
            GluonBool::True => true,
        }
    }
}

impl From<GluonRecipeSpec> for RecipeSpec {
    fn from(spec: GluonRecipeSpec) -> Self {
        Self {
            source: spec.source.into(),
            build: spec.build.into(),
            package: spec.package.into(),
            options: spec.options.into(),
            profiles: spec.profiles.into_iter().map(Into::into).collect(),
            sub_packages: spec.sub_packages.into_iter().map(Into::into).collect(),
            upstreams: spec.upstreams.into_iter().map(Into::into).collect(),
            architectures: spec.architectures,
            tuning: spec.tuning.into_iter().map(Into::into).collect(),
            emul32: spec.emul32.into(),
            mold: spec.mold.into(),
        }
    }
}

impl From<GluonSourceSpec> for SourceSpec {
    fn from(spec: GluonSourceSpec) -> Self {
        Self {
            name: spec.name,
            version: spec.version,
            release: spec.release,
            homepage: spec.homepage,
            license: spec.license,
        }
    }
}

impl From<GluonBuildSpec> for BuildSpec {
    fn from(spec: GluonBuildSpec) -> Self {
        Self {
            setup: spec.setup.into(),
            build: spec.build.into(),
            install: spec.install.into(),
            check: spec.check.into(),
            workload: spec.workload.into(),
            environment: spec.environment.into(),
            build_deps: spec.build_deps,
            check_deps: spec.check_deps,
        }
    }
}

impl From<GluonPackageSpec> for PackageSpec {
    fn from(spec: GluonPackageSpec) -> Self {
        Self {
            summary: spec.summary.into(),
            description: spec.description.into(),
            provides_exclude: spec.provides_exclude,
            run_deps: spec.run_deps,
            run_deps_exclude: spec.run_deps_exclude,
            paths: spec.paths.into_iter().map(Into::into).collect(),
            conflicts: spec.conflicts,
        }
    }
}

impl From<GluonOptionsSpec> for OptionsSpec {
    fn from(spec: GluonOptionsSpec) -> Self {
        Self {
            toolchain: spec.toolchain.into(),
            cspgo: spec.cspgo.into(),
            samplepgo: spec.samplepgo.into(),
            debug: spec.debug.into(),
            strip: spec.strip.into(),
            networking: spec.networking.into(),
            compressman: spec.compressman.into(),
            lastrip: spec.lastrip.into(),
        }
    }
}

impl<T, U> From<GluonKeyValueSpec<T>> for KeyValueSpec<U>
where
    U: From<T>,
{
    fn from(spec: GluonKeyValueSpec<T>) -> Self {
        Self {
            key: spec.key,
            value: spec.value.into(),
        }
    }
}

impl From<GluonUpstreamSpec> for UpstreamSpec {
    fn from(spec: GluonUpstreamSpec) -> Self {
        match spec {
            GluonUpstreamSpec::Archive {
                url,
                hash,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
            } => Self::Archive {
                url,
                hash,
                rename: rename.into(),
                strip_dirs: strip_dirs.into(),
                unpack: unpack.into(),
                unpack_dir: unpack_dir.into(),
            },
            GluonUpstreamSpec::Git {
                url,
                git_ref,
                clone_dir,
            } => Self::Git {
                url,
                git_ref,
                clone_dir: clone_dir.into(),
            },
        }
    }
}

impl From<GluonPathSpec> for PathSpec {
    fn from(spec: GluonPathSpec) -> Self {
        match spec {
            GluonPathSpec::Any { path } => Self::Any { path },
            GluonPathSpec::Exe { path } => Self::Exe { path },
            GluonPathSpec::Symlink { path } => Self::Symlink { path },
            GluonPathSpec::Special { path } => Self::Special { path },
        }
    }
}

impl From<GluonTuningSpec> for TuningSpec {
    fn from(spec: GluonTuningSpec) -> Self {
        match spec {
            GluonTuningSpec::Enable => Self::Enable,
            GluonTuningSpec::Disable => Self::Disable,
            GluonTuningSpec::Config { value } => Self::Config { value },
        }
    }
}

impl From<GluonToolchainSpec> for ToolchainSpec {
    fn from(spec: GluonToolchainSpec) -> Self {
        match spec {
            GluonToolchainSpec::Llvm => Self::Llvm,
            GluonToolchainSpec::Gnu => Self::Gnu,
        }
    }
}

/// Evaluate an authored recipe with the restricted default evaluator.
pub fn evaluate_gluon(source: &GluonSource) -> Result<EvaluatedRecipe, RecipeEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

/// Evaluate an authored recipe with caller-selected evaluator limits/root.
pub fn evaluate_gluon_with(
    evaluator: &Evaluator,
    source: &GluonSource,
) -> Result<EvaluatedRecipe, RecipeEvaluationError> {
    evaluate_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate an authored recipe and bind explicit lock/input bytes into its
/// fingerprint without exposing those bytes to the Gluon program.
pub fn evaluate_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &GluonSource,
    explicit_inputs: &[u8],
) -> Result<EvaluatedRecipe, RecipeEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.insert_embedded_module("boulder.recipe.v1", GLUON_RECIPE_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonRecipeSpec>(source, explicit_inputs)?;
    let recipe = Recipe::try_from(RecipeSpec::from(evaluation.value))?;

    Ok(EvaluatedRecipe {
        recipe,
        fingerprint: evaluation.fingerprint,
    })
}
