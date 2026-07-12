// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Versioned Gluon boundary for package recipes.

use std::fmt::Write;

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

// Standalone recipes cannot depend on an import root, so the canonical encoder
// repeats only the ABI-owned wire types that are needed to type the final
// literal. Keep this in lockstep with `gluon/recipe.glu` and the DTOs below.
const STANDALONE_GLUON_TYPES: &str = r#"// Canonical standalone Boulder recipe.

type Optional a =
    | Unset
    | Set a

type Boolean =
    | False
    | True

type SourceSpec = {
    name: String,
    version: String,
    release: Int,
    homepage: String,
    license: Array String,
}

type BuildSpec = {
    setup: Optional String,
    build: Optional String,
    install: Optional String,
    check: Optional String,
    workload: Optional String,
    environment: Optional String,
    build_deps: Array String,
    check_deps: Array String,
}

type PathSpec =
    | Any { path: String }
    | Exe { path: String }
    | Symlink { path: String }
    | Special { path: String }

type PackageSpec = {
    summary: Optional String,
    description: Optional String,
    provides_exclude: Array String,
    run_deps: Array String,
    run_deps_exclude: Array String,
    paths: Array PathSpec,
    conflicts: Array String,
}

type ToolchainSpec =
    | Llvm
    | Gnu

type OptionsSpec = {
    toolchain: ToolchainSpec,
    cspgo: Boolean,
    samplepgo: Boolean,
    debug: Boolean,
    strip: Boolean,
    networking: Boolean,
    compressman: Boolean,
    lastrip: Boolean,
}

type UpstreamSpec =
    | Archive {
        url: String,
        hash: String,
        rename: Optional String,
        strip_dirs: Optional Int,
        unpack: Boolean,
        unpack_dir: Optional String,
    }
    | Git {
        url: String,
        git_ref: String,
        clone_dir: Optional String,
    }

type TuningSpec =
    | Enable
    | Disable
    | Config { value: String }

type KeyValueSpec a = {
    key: String,
    value: a,
}

type RecipeSpec = {
    source: SourceSpec,
    build: BuildSpec,
    package: PackageSpec,
    options: OptionsSpec,
    profiles: Array (KeyValueSpec BuildSpec),
    sub_packages: Array (KeyValueSpec PackageSpec),
    upstreams: Array UpstreamSpec,
    architectures: Array String,
    tuning: Array (KeyValueSpec TuningSpec),
    emul32: Boolean,
    mold: Boolean,
}

"#;

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

/// Encode a format-neutral recipe as a canonical standalone Gluon value.
///
/// The specification is first converted through the shared domain validation
/// boundary. URLs and other domain values are then converted back to their
/// canonical representation before formatting. Dynamically named entries are
/// ordered by key; entries with duplicate keys retain their relative order.
pub fn encode_gluon_spec(spec: &RecipeSpec) -> Result<String, RecipeConversionError> {
    let recipe = Recipe::try_from(spec.clone())?;
    let canonical_spec = RecipeSpec::try_from(&recipe)?;
    Ok(encode_valid_spec(&canonical_spec))
}

/// Encode an already parsed domain recipe as canonical standalone Gluon.
pub fn encode_gluon(recipe: &Recipe) -> Result<String, RecipeConversionError> {
    let spec = RecipeSpec::try_from(recipe)?;
    Ok(encode_valid_spec(&spec))
}

fn encode_valid_spec(spec: &RecipeSpec) -> String {
    let mut output = String::from(STANDALONE_GLUON_TYPES);
    output.push_str("{\n");

    write_indent(&mut output, 1);
    output.push_str("source = ");
    encode_source(&mut output, &spec.source, 1);
    output.push_str(",\n");

    write_indent(&mut output, 1);
    output.push_str("build = ");
    encode_build(&mut output, &spec.build, 1);
    output.push_str(",\n");

    write_indent(&mut output, 1);
    output.push_str("package = ");
    encode_package(&mut output, &spec.package, 1);
    output.push_str(",\n");

    write_indent(&mut output, 1);
    output.push_str("options = ");
    encode_options(&mut output, &spec.options, 1);
    output.push_str(",\n");

    encode_key_values(&mut output, "profiles", &spec.profiles, encode_build);
    encode_key_values(&mut output, "sub_packages", &spec.sub_packages, encode_package);
    encode_upstreams(&mut output, &spec.upstreams);
    encode_string_array_field(&mut output, 1, "architectures", &spec.architectures);
    encode_key_values(&mut output, "tuning", &spec.tuning, encode_tuning);
    writeln!(output, "    emul32 = {},", gluon_bool(spec.emul32)).unwrap();
    writeln!(output, "    mold = {},", gluon_bool(spec.mold)).unwrap();
    output.push_str("}\n");
    output
}

fn encode_source(output: &mut String, source: &SourceSpec, indent: usize) {
    output.push_str("{\n");
    write_string_field(output, indent + 1, "name", &source.name);
    write_string_field(output, indent + 1, "version", &source.version);
    write_indent(output, indent + 1);
    writeln!(output, "release = {},", source.release).unwrap();
    write_string_field(output, indent + 1, "homepage", &source.homepage);
    encode_string_array_field(output, indent + 1, "license", &source.license);
    write_indent(output, indent);
    output.push('}');
}

fn encode_build(output: &mut String, build: &BuildSpec, indent: usize) {
    output.push_str("{\n");
    for (field, value) in [
        ("setup", build.setup.as_deref()),
        ("build", build.build.as_deref()),
        ("install", build.install.as_deref()),
        ("check", build.check.as_deref()),
        ("workload", build.workload.as_deref()),
        ("environment", build.environment.as_deref()),
    ] {
        write_indent(output, indent + 1);
        writeln!(output, "{field} = {},", gluon_optional_text(value)).unwrap();
    }
    encode_string_array_field(output, indent + 1, "build_deps", &build.build_deps);
    encode_string_array_field(output, indent + 1, "check_deps", &build.check_deps);
    write_indent(output, indent);
    output.push('}');
}

fn encode_package(output: &mut String, package: &PackageSpec, indent: usize) {
    output.push_str("{\n");
    for (field, value) in [
        ("summary", package.summary.as_deref()),
        ("description", package.description.as_deref()),
    ] {
        write_indent(output, indent + 1);
        writeln!(output, "{field} = {},", gluon_optional_text(value)).unwrap();
    }
    encode_string_array_field(output, indent + 1, "provides_exclude", &package.provides_exclude);
    encode_string_array_field(output, indent + 1, "run_deps", &package.run_deps);
    encode_string_array_field(output, indent + 1, "run_deps_exclude", &package.run_deps_exclude);

    write_indent(output, indent + 1);
    output.push_str("paths = [\n");
    for path in &package.paths {
        let (variant, path) = match path {
            PathSpec::Any { path } => ("Any", path),
            PathSpec::Exe { path } => ("Exe", path),
            PathSpec::Symlink { path } => ("Symlink", path),
            PathSpec::Special { path } => ("Special", path),
        };
        write_indent(output, indent + 2);
        writeln!(output, "{variant} {{ path = {} }},", gluon_text(path)).unwrap();
    }
    write_indent(output, indent + 1);
    output.push_str("],\n");

    encode_string_array_field(output, indent + 1, "conflicts", &package.conflicts);
    write_indent(output, indent);
    output.push('}');
}

fn encode_options(output: &mut String, options: &OptionsSpec, indent: usize) {
    output.push_str("{\n");
    write_indent(output, indent + 1);
    writeln!(
        output,
        "toolchain = {},",
        match options.toolchain {
            ToolchainSpec::Llvm => "Llvm",
            ToolchainSpec::Gnu => "Gnu",
        }
    )
    .unwrap();
    for (field, value) in [
        ("cspgo", options.cspgo),
        ("samplepgo", options.samplepgo),
        ("debug", options.debug),
        ("strip", options.strip),
        ("networking", options.networking),
        ("compressman", options.compressman),
        ("lastrip", options.lastrip),
    ] {
        write_indent(output, indent + 1);
        writeln!(output, "{field} = {},", gluon_bool(value)).unwrap();
    }
    write_indent(output, indent);
    output.push('}');
}

fn encode_key_values<T>(
    output: &mut String,
    field: &str,
    values: &[KeyValueSpec<T>],
    encode_value: fn(&mut String, &T, usize),
) {
    let mut sorted = values.iter().enumerate().collect::<Vec<_>>();
    sorted.sort_by(|(left_index, left), (right_index, right)| {
        left.key.cmp(&right.key).then_with(|| left_index.cmp(right_index))
    });

    write_indent(output, 1);
    writeln!(output, "{field} = [").unwrap();
    for (_, value) in sorted {
        output.push_str("        {\n");
        writeln!(output, "            key = {},", gluon_text(&value.key)).unwrap();
        output.push_str("            value = ");
        encode_value(output, &value.value, 3);
        output.push_str(",\n");
        output.push_str("        },\n");
    }
    output.push_str("    ],\n");
}

fn encode_upstreams(output: &mut String, upstreams: &[UpstreamSpec]) {
    output.push_str("    upstreams = [\n");
    for upstream in upstreams {
        output.push_str("        ");
        match upstream {
            UpstreamSpec::Archive {
                url,
                hash,
                rename,
                strip_dirs,
                unpack,
                unpack_dir,
            } => {
                output.push_str("Archive {\n");
                write_string_field(output, 3, "url", url);
                write_string_field(output, 3, "hash", hash);
                writeln!(
                    output,
                    "            rename = {},",
                    gluon_optional_text(rename.as_deref())
                )
                .unwrap();
                writeln!(
                    output,
                    "            strip_dirs = {},",
                    strip_dirs.map_or_else(|| "Unset".to_owned(), |value| format!("Set {value}"))
                )
                .unwrap();
                writeln!(output, "            unpack = {},", gluon_bool(*unpack)).unwrap();
                writeln!(
                    output,
                    "            unpack_dir = {},",
                    gluon_optional_text(unpack_dir.as_deref())
                )
                .unwrap();
                output.push_str("        },\n");
            }
            UpstreamSpec::Git {
                url,
                git_ref,
                clone_dir,
            } => {
                output.push_str("Git {\n");
                write_string_field(output, 3, "url", url);
                write_string_field(output, 3, "git_ref", git_ref);
                writeln!(
                    output,
                    "            clone_dir = {},",
                    gluon_optional_text(clone_dir.as_deref())
                )
                .unwrap();
                output.push_str("        },\n");
            }
        }
    }
    output.push_str("    ],\n");
}

fn encode_tuning(output: &mut String, tuning: &TuningSpec, indent: usize) {
    match tuning {
        TuningSpec::Enable => output.push_str("Enable"),
        TuningSpec::Disable => output.push_str("Disable"),
        TuningSpec::Config { value } => {
            writeln!(output, "Config {{").unwrap();
            write_string_field(output, indent + 1, "value", value);
            write_indent(output, indent);
            output.push('}');
        }
    }
}

fn encode_string_array_field(output: &mut String, indent: usize, field: &str, values: &[String]) {
    write_indent(output, indent);
    writeln!(output, "{field} = [").unwrap();
    for value in values {
        write_indent(output, indent + 1);
        writeln!(output, "{},", gluon_text(value)).unwrap();
    }
    write_indent(output, indent);
    output.push_str("],\n");
}

fn write_string_field(output: &mut String, indent: usize, field: &str, value: &str) {
    write_indent(output, indent);
    writeln!(output, "{field} = {},", gluon_text(value)).unwrap();
}

fn write_indent(output: &mut String, indent: usize) {
    for _ in 0..indent {
        output.push_str("    ");
    }
}

fn gluon_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn gluon_optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "Unset".to_owned(), |value| format!("Set {}", gluon_text(value)))
}

fn gluon_text(value: &str) -> String {
    if value.contains('\n') || (value.len() > 80 && (value.contains('"') || value.contains('\\'))) {
        gluon_raw_string(value)
    } else {
        gluon_string(value)
    }
}

fn gluon_raw_string(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut saw_quote = false;
    let mut longest_hash_run = 0;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'"' {
            continue;
        }
        saw_quote = true;
        let hash_run = bytes[index + 1..].iter().take_while(|byte| **byte == b'#').count();
        longest_hash_run = longest_hash_run.max(hash_run);
    }

    let delimiter_count = if saw_quote { longest_hash_run + 1 } else { 0 };
    let hashes = "#".repeat(delimiter_count);
    format!("r{hashes}\"{value}\"{hashes}")
}

fn gluon_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod encoder_tests {
    use super::*;

    fn minimal_spec() -> RecipeSpec {
        RecipeSpec::new(SourceSpec {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            release: 1,
            homepage: "https://example.com/".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        })
    }

    fn maximal_spec() -> RecipeSpec {
        RecipeSpec {
            source: SourceSpec {
                name: "example".to_owned(),
                version: "1.2.3".to_owned(),
                release: 7,
                homepage: "https://example.com/project".to_owned(),
                license: vec!["MPL-2.0".to_owned(), "Apache-2.0".to_owned()],
            },
            build: BuildSpec {
                setup: Some("prepare\nsource tree".to_owned()),
                build: Some("build".to_owned()),
                install: Some("install".to_owned()),
                check: Some("check".to_owned()),
                workload: Some("workload".to_owned()),
                environment: Some("ENV=value".to_owned()),
                build_deps: vec!["cmake".to_owned(), "ninja".to_owned()],
                check_deps: vec!["check-framework".to_owned()],
            },
            package: maximal_package("root"),
            options: OptionsSpec {
                toolchain: ToolchainSpec::Gnu,
                cspgo: true,
                samplepgo: true,
                debug: false,
                strip: false,
                networking: true,
                compressman: true,
                lastrip: false,
            },
            profiles: vec![
                KeyValueSpec {
                    key: "x86_64".to_owned(),
                    value: BuildSpec {
                        build: Some("native build".to_owned()),
                        ..BuildSpec::default()
                    },
                },
                KeyValueSpec {
                    key: "aarch64".to_owned(),
                    value: BuildSpec {
                        build: Some("cross build".to_owned()),
                        ..BuildSpec::default()
                    },
                },
            ],
            sub_packages: vec![
                KeyValueSpec {
                    key: "example-docs".to_owned(),
                    value: maximal_package("docs"),
                },
                KeyValueSpec {
                    key: "example-devel".to_owned(),
                    value: maximal_package("devel"),
                },
            ],
            upstreams: vec![
                UpstreamSpec::Archive {
                    url: "https://example.com/source.tar.xz".to_owned(),
                    hash: "sha256-hash".to_owned(),
                    rename: Some("renamed.tar.xz".to_owned()),
                    strip_dirs: Some(2),
                    unpack: false,
                    unpack_dir: Some("archive".to_owned()),
                },
                UpstreamSpec::Git {
                    url: "https://example.com/source.git".to_owned(),
                    git_ref: "v1.2.3".to_owned(),
                    clone_dir: Some("git".to_owned()),
                },
            ],
            architectures: vec!["x86_64".to_owned(), "aarch64".to_owned()],
            tuning: vec![
                KeyValueSpec {
                    key: "optimize".to_owned(),
                    value: TuningSpec::Config {
                        value: "speed".to_owned(),
                    },
                },
                KeyValueSpec {
                    key: "harden".to_owned(),
                    value: TuningSpec::Enable,
                },
                KeyValueSpec {
                    key: "debug".to_owned(),
                    value: TuningSpec::Disable,
                },
            ],
            emul32: true,
            mold: true,
        }
    }

    fn maximal_package(label: &str) -> PackageSpec {
        PackageSpec {
            summary: Some(format!("{label} summary")),
            description: Some(format!("{label} description")),
            provides_exclude: vec![format!("{label}-provided(*)")],
            run_deps: vec![format!("{label}-runtime")],
            run_deps_exclude: vec![format!("{label}-excluded(*)")],
            paths: vec![
                PathSpec::Any {
                    path: format!("/usr/share/{label}"),
                },
                PathSpec::Exe {
                    path: format!("/usr/bin/{label}"),
                },
                PathSpec::Symlink {
                    path: format!("/usr/bin/{label}-link"),
                },
                PathSpec::Special {
                    path: format!("/usr/lib/{label}.special"),
                },
            ],
            conflicts: vec![format!("other-{label}")],
        }
    }

    fn sorted_dynamic_entries(mut spec: RecipeSpec) -> RecipeSpec {
        spec.profiles.sort_by(|left, right| left.key.cmp(&right.key));
        spec.sub_packages.sort_by(|left, right| left.key.cmp(&right.key));
        spec.tuning.sort_by(|left, right| left.key.cmp(&right.key));
        spec
    }

    fn evaluate_encoded(encoded: &str) -> RecipeSpec {
        let evaluated = evaluate_gluon(&GluonSource::new("stone.glu", encoded)).unwrap();
        RecipeSpec::try_from(&evaluated.recipe).unwrap()
    }

    #[test]
    fn canonical_encoder_round_trips_minimal_recipe() {
        let expected = minimal_spec();
        let encoded = encode_gluon_spec(&expected).unwrap();

        assert!(encoded.starts_with("// Canonical standalone Boulder recipe.\n"));
        assert!(!encoded.contains("import!"));
        assert_eq!(evaluate_encoded(&encoded), expected);

        let domain = Recipe::try_from(expected).unwrap();
        assert_eq!(encode_gluon(&domain).unwrap(), encoded);
    }

    #[test]
    fn canonical_encoder_round_trips_every_field_and_variant() {
        let input = maximal_spec();
        let encoded = encode_gluon_spec(&input).unwrap();
        let expected = sorted_dynamic_entries(input);

        assert_eq!(evaluate_encoded(&encoded), expected);
        assert!(encoded.contains("Archive {"));
        assert!(encoded.contains("Git {"));
        assert!(encoded.contains("Any {"));
        assert!(encoded.contains("Exe {"));
        assert!(encoded.contains("Symlink {"));
        assert!(encoded.contains("Special {"));
        assert!(encoded.contains("value = Enable"));
        assert!(encoded.contains("value = Disable"));
        assert!(encoded.contains("value = Config {"));
    }

    #[test]
    fn multiline_raw_strings_and_escaped_strings_are_lossless() {
        let mut expected = minimal_spec();
        let script = "echo \" plain\nthen \"# one\nthen \"## two\nbackslash \\ and tab\t";
        let summary = "quote \" slash \\ tab\t carriage\r";
        expected.build.build = Some(script.to_owned());
        expected.package.summary = Some(summary.to_owned());

        let encoded = encode_gluon_spec(&expected).unwrap();

        assert!(encoded.contains(&format!("Set r###\"{script}\"###")));
        assert!(encoded.contains("Set \"quote \\\" slash \\\\ tab\\t carriage\\r\""));
        assert_eq!(evaluate_encoded(&encoded), expected);
    }

    #[test]
    fn dynamic_entry_order_is_canonical_and_duplicate_order_is_stable() {
        let first = maximal_spec();
        let mut reordered = first.clone();
        reordered.profiles.reverse();
        reordered.sub_packages.reverse();
        reordered.tuning.reverse();

        assert_eq!(
            encode_gluon_spec(&first).unwrap(),
            encode_gluon_spec(&reordered).unwrap()
        );

        let mut duplicates = minimal_spec();
        duplicates.profiles = vec![
            KeyValueSpec {
                key: "same".to_owned(),
                value: BuildSpec {
                    build: Some("first".to_owned()),
                    ..BuildSpec::default()
                },
            },
            KeyValueSpec {
                key: "same".to_owned(),
                value: BuildSpec {
                    build: Some("second".to_owned()),
                    ..BuildSpec::default()
                },
            },
        ];
        let decoded = evaluate_encoded(&encode_gluon_spec(&duplicates).unwrap());
        assert_eq!(decoded.profiles[0].value.build.as_deref(), Some("first"));
        assert_eq!(decoded.profiles[1].value.build.as_deref(), Some("second"));
    }
}
