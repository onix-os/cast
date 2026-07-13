// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Restricted Gluon boundary and canonical encoder for macro policy modules.

use std::{error::Error, fmt, fmt::Write as _};

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};

use super::{Action, ActionSpec, Macros, MacrosSpec, PolicyKind, PolicyLayer, PolicyModule, PolicyOperation};
use crate::{
    Package, PackageSpec, PathKind, PathSpec, ValidationError,
    spec::KeyValueSpec,
    tuning::{CompilerFlagsSpec, TuningFlagSpec, TuningGroupSpec, TuningOptionSpec},
    validation,
};

/// Version of the embedded macro-policy language boundary.
pub const MACROS_ABI_VERSION: u32 = 1;

/// Version of the explicit repository-policy composition boundary.
pub const POLICY_ABI_VERSION: u32 = 1;

/// Pure helpers imported by policy modules as `boulder.macros.v1`.
pub const GLUON_MACROS_ABI: &str = include_str!("../../gluon/macros.glu");

/// Pure helpers imported by the repository policy root as `boulder.policy.v1`.
pub const GLUON_POLICY_ABI: &str = include_str!("../../gluon/policy.glu");

const STANDALONE_GLUON_TYPES: &str = r#"type Optional a =
    | None
    | Some a

type ActionSpec = {
    description : String,
    example : Optional String,
    command : String,
    dependencies : Array String,
}

type ActionEntry = {
    key : String,
    value : ActionSpec,
}

type CompilerFlagsSpec = {
    c : Optional String,
    cxx : Optional String,
    f : Optional String,
    d : Optional String,
    rust : Optional String,
    vala : Optional String,
    go : Optional String,
    ld : Optional String,
}

type TuningFlagSpec = {
    root : CompilerFlagsSpec,
    gnu : CompilerFlagsSpec,
    llvm : CompilerFlagsSpec,
}

type TuningOptionSpec = {
    enabled : Array String,
    disabled : Array String,
}

type TuningChoice = {
    key : String,
    value : TuningOptionSpec,
}

type TuningGroupSpec = {
    root : TuningOptionSpec,
    default : Optional String,
    choices : Array TuningChoice,
}

type PathSpec =
    | Any { path : String }
    | Exe { path : String }
    | Symlink { path : String }
    | Special { path : String }

type PackageSpec = {
    summary : Optional String,
    description : Optional String,
    provides_exclude : Array String,
    run_deps : Array String,
    run_deps_exclude : Array String,
    paths : Array PathSpec,
    conflicts : Array String,
}

type DefinitionEntry = {
    key : String,
    value : String,
}

type FlagEntry = {
    key : String,
    value : TuningFlagSpec,
}

type TuningEntry = {
    key : String,
    value : TuningGroupSpec,
}

type PackageEntry = {
    key : String,
    value : PackageSpec,
}

type MacrosSpec = {
    actions : Array ActionEntry,
    definitions : Array DefinitionEntry,
    flags : Array FlagEntry,
    tuning : Array TuningEntry,
    packages : Array PackageEntry,
    default_tuning_groups : Array String,
}

"#;

/// Evaluated domain macros and their complete source provenance.
#[derive(Debug, Clone)]
pub struct EvaluatedMacros {
    pub macros: Macros,
    pub fingerprint: EvaluationFingerprint,
}

/// Ordered policy operations and the complete provenance of their root.
#[derive(Debug, Clone)]
pub struct EvaluatedPolicy {
    pub layers: Vec<PolicyLayer>,
    pub fingerprint: EvaluationFingerprint,
}

/// Semantic macro conversion error with a stable field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacrosConversionError {
    EmptyKey { field: String },
    UnknownTuningDefault { field: String, value: String },
    EmptyPackagePath { field: String },
    InvalidRelation(ValidationError),
}

impl MacrosConversionError {
    pub fn field(&self) -> &str {
        match self {
            Self::EmptyKey { field } | Self::UnknownTuningDefault { field, .. } | Self::EmptyPackagePath { field } => {
                field
            }
            Self::InvalidRelation(error) => error.field(),
        }
    }
}

impl fmt::Display for MacrosConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyKey { field } => write!(formatter, "{field}: dynamic key must not be empty"),
            Self::UnknownTuningDefault { field, value } => {
                write!(formatter, "{field}: default `{value}` is not one of the tuning choices")
            }
            Self::EmptyPackagePath { field } => write!(formatter, "{field}: package path must not be empty"),
            Self::InvalidRelation(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for MacrosConversionError {}

/// Failure to evaluate or semantically convert a macro module.
#[derive(Debug)]
pub enum MacrosEvaluationError {
    Evaluation(Diagnostic),
    Conversion(MacrosConversionError),
}

impl fmt::Display for MacrosEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evaluation(error) => write!(formatter, "evaluate macro Gluon: {error}"),
            Self::Conversion(error) => write!(formatter, "convert macro Gluon: {error}"),
        }
    }
}

impl Error for MacrosEvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Evaluation(error) => Some(error),
            Self::Conversion(error) => Some(error),
        }
    }
}

impl From<Diagnostic> for MacrosEvaluationError {
    fn from(error: Diagnostic) -> Self {
        Self::Evaluation(error)
    }
}

impl From<MacrosConversionError> for MacrosEvaluationError {
    fn from(error: MacrosConversionError) -> Self {
        Self::Conversion(error)
    }
}

/// Failure to evaluate or semantically convert the explicit policy root.
#[derive(Debug)]
pub enum PolicyEvaluationError {
    Evaluation(Diagnostic),
}

impl fmt::Display for PolicyEvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evaluation(error) => write!(formatter, "evaluate policy Gluon: {error}"),
        }
    }
}

impl Error for PolicyEvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Evaluation(error) => Some(error),
        }
    }
}

impl From<Diagnostic> for PolicyEvaluationError {
    fn from(error: Diagnostic) -> Self {
        Self::Evaluation(error)
    }
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    None,
    Some(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonKeyValueSpec<T> {
    key: String,
    value: T,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonMacrosSpec {
    actions: Vec<GluonKeyValueSpec<GluonActionSpec>>,
    definitions: Vec<GluonKeyValueSpec<String>>,
    flags: Vec<GluonKeyValueSpec<GluonTuningFlagSpec>>,
    tuning: Vec<GluonKeyValueSpec<GluonTuningGroupSpec>>,
    packages: Vec<GluonKeyValueSpec<GluonPackageSpec>>,
    default_tuning_groups: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonPolicyKind {
    Actions,
    Architecture,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonPolicyOperation {
    Add,
    Replace,
    Modify,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPolicyModule {
    operation: GluonPolicyOperation,
    kind: GluonPolicyKind,
    key: String,
    origin: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPolicyLayer {
    name: String,
    entries: Vec<GluonPolicyModule>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonActionSpec {
    description: String,
    example: GluonOptional<String>,
    command: String,
    dependencies: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonCompilerFlagsSpec {
    c: GluonOptional<String>,
    cxx: GluonOptional<String>,
    f: GluonOptional<String>,
    d: GluonOptional<String>,
    rust: GluonOptional<String>,
    vala: GluonOptional<String>,
    go: GluonOptional<String>,
    ld: GluonOptional<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningFlagSpec {
    root: GluonCompilerFlagsSpec,
    gnu: GluonCompilerFlagsSpec,
    llvm: GluonCompilerFlagsSpec,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningOptionSpec {
    enabled: Vec<String>,
    disabled: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonTuningGroupSpec {
    root: GluonTuningOptionSpec,
    default: GluonOptional<String>,
    choices: Vec<GluonKeyValueSpec<GluonTuningOptionSpec>>,
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
enum GluonPathSpec {
    Any { path: String },
    Exe { path: String },
    Symlink { path: String },
    Special { path: String },
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::None => None,
            GluonOptional::Some(value) => Some(value),
        }
    }
}

impl<T, U> From<GluonKeyValueSpec<T>> for KeyValueSpec<U>
where
    U: From<T>,
{
    fn from(value: GluonKeyValueSpec<T>) -> Self {
        Self {
            key: value.key,
            value: value.value.into(),
        }
    }
}

impl From<GluonMacrosSpec> for MacrosSpec {
    fn from(value: GluonMacrosSpec) -> Self {
        Self {
            actions: value.actions.into_iter().map(Into::into).collect(),
            definitions: value.definitions.into_iter().map(Into::into).collect(),
            flags: value.flags.into_iter().map(Into::into).collect(),
            tuning: value.tuning.into_iter().map(Into::into).collect(),
            packages: value.packages.into_iter().map(Into::into).collect(),
            default_tuning_groups: value.default_tuning_groups,
        }
    }
}

impl From<GluonPolicyKind> for PolicyKind {
    fn from(value: GluonPolicyKind) -> Self {
        match value {
            GluonPolicyKind::Actions => Self::Actions,
            GluonPolicyKind::Architecture => Self::Architecture,
        }
    }
}

impl From<GluonPolicyOperation> for PolicyOperation {
    fn from(value: GluonPolicyOperation) -> Self {
        match value {
            GluonPolicyOperation::Add => Self::Add,
            GluonPolicyOperation::Replace => Self::Replace,
            GluonPolicyOperation::Modify => Self::Modify,
        }
    }
}

impl From<GluonActionSpec> for ActionSpec {
    fn from(value: GluonActionSpec) -> Self {
        Self {
            description: value.description,
            example: value.example.into(),
            command: value.command,
            dependencies: value.dependencies,
        }
    }
}

impl From<GluonCompilerFlagsSpec> for CompilerFlagsSpec {
    fn from(value: GluonCompilerFlagsSpec) -> Self {
        Self {
            c: value.c.into(),
            cxx: value.cxx.into(),
            f: value.f.into(),
            d: value.d.into(),
            rust: value.rust.into(),
            vala: value.vala.into(),
            go: value.go.into(),
            ld: value.ld.into(),
        }
    }
}

impl From<GluonTuningFlagSpec> for TuningFlagSpec {
    fn from(value: GluonTuningFlagSpec) -> Self {
        Self {
            root: value.root.into(),
            gnu: value.gnu.into(),
            llvm: value.llvm.into(),
        }
    }
}

impl From<GluonTuningOptionSpec> for TuningOptionSpec {
    fn from(value: GluonTuningOptionSpec) -> Self {
        Self {
            enabled: value.enabled,
            disabled: value.disabled,
        }
    }
}

impl From<GluonTuningGroupSpec> for TuningGroupSpec {
    fn from(value: GluonTuningGroupSpec) -> Self {
        Self {
            root: value.root.into(),
            default: value.default.into(),
            choices: value.choices.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonPackageSpec> for PackageSpec {
    fn from(value: GluonPackageSpec) -> Self {
        Self {
            summary: value.summary.into(),
            description: value.description.into(),
            provides_exclude: value.provides_exclude,
            run_deps: value.run_deps,
            run_deps_exclude: value.run_deps_exclude,
            paths: value.paths.into_iter().map(Into::into).collect(),
            conflicts: value.conflicts,
        }
    }
}

impl From<GluonPathSpec> for PathSpec {
    fn from(value: GluonPathSpec) -> Self {
        match value {
            GluonPathSpec::Any { path } => Self::Any { path },
            GluonPathSpec::Exe { path } => Self::Exe { path },
            GluonPathSpec::Symlink { path } => Self::Symlink { path },
            GluonPathSpec::Special { path } => Self::Special { path },
        }
    }
}

impl MacrosSpec {
    pub fn validate(&self) -> Result<(), MacrosConversionError> {
        validate_keys("actions", &self.actions)?;
        validate_keys("definitions", &self.definitions)?;
        validate_keys("flags", &self.flags)?;
        validate_keys("tuning", &self.tuning)?;
        validate_keys("packages", &self.packages)?;

        for (index, action) in self.actions.iter().enumerate() {
            validation::validate_dependencies(
                &action.value.dependencies,
                &format!("actions[{index}].value.dependencies"),
            )
            .map_err(MacrosConversionError::InvalidRelation)?;
        }
        for (index, tuning) in self.tuning.iter().enumerate() {
            if let Some(default) = tuning.value.default.as_ref()
                && !tuning.value.choices.iter().any(|choice| choice.key == *default)
            {
                return Err(MacrosConversionError::UnknownTuningDefault {
                    field: format!("tuning[{index}].value.default"),
                    value: default.clone(),
                });
            }
        }
        for (package_index, package) in self.packages.iter().enumerate() {
            validation::validate_package_templates(
                &Package::from(package.value.clone()),
                &format!("packages[{package_index}].value"),
            )
            .map_err(MacrosConversionError::InvalidRelation)?;
            for (path_index, path) in package.value.paths.iter().enumerate() {
                let path = match path {
                    PathSpec::Any { path }
                    | PathSpec::Exe { path }
                    | PathSpec::Symlink { path }
                    | PathSpec::Special { path } => path,
                };
                if path.trim().is_empty() {
                    return Err(MacrosConversionError::EmptyPackagePath {
                        field: format!("packages[{package_index}].value.paths[{path_index}].path"),
                    });
                }
            }
        }
        for (index, group) in self.default_tuning_groups.iter().enumerate() {
            if group.is_empty() {
                return Err(MacrosConversionError::EmptyKey {
                    field: format!("default_tuning_groups[{index}]"),
                });
            }
        }
        Ok(())
    }
}

fn validate_keys<T>(field: &str, values: &[KeyValueSpec<T>]) -> Result<(), MacrosConversionError> {
    for (index, value) in values.iter().enumerate() {
        if value.key.is_empty() {
            return Err(MacrosConversionError::EmptyKey {
                field: format!("{field}[{index}].key"),
            });
        }
    }
    Ok(())
}

impl From<&Macros> for MacrosSpec {
    fn from(macros: &Macros) -> Self {
        Self {
            actions: macros
                .actions
                .iter()
                .map(|value| KeyValueSpec {
                    key: value.key.clone(),
                    value: (&value.value).into(),
                })
                .collect(),
            definitions: macros
                .definitions
                .iter()
                .map(|value| KeyValueSpec {
                    key: value.key.clone(),
                    value: value.value.clone(),
                })
                .collect(),
            flags: macros
                .flags
                .iter()
                .map(|value| KeyValueSpec {
                    key: value.key.clone(),
                    value: (&value.value).into(),
                })
                .collect(),
            tuning: macros
                .tuning
                .iter()
                .map(|value| KeyValueSpec {
                    key: value.key.clone(),
                    value: (&value.value).into(),
                })
                .collect(),
            packages: macros
                .packages
                .iter()
                .map(|value| KeyValueSpec {
                    key: value.key.clone(),
                    value: package_to_spec(&value.value),
                })
                .collect(),
            default_tuning_groups: macros.default_tuning_groups.clone(),
        }
    }
}

impl From<&Action> for ActionSpec {
    fn from(action: &Action) -> Self {
        Self {
            description: action.description.clone(),
            example: action.example.clone(),
            command: action.command.clone(),
            dependencies: action.dependencies.clone(),
        }
    }
}

fn package_to_spec(package: &Package) -> PackageSpec {
    PackageSpec {
        summary: package.summary.clone(),
        description: package.description.clone(),
        provides_exclude: package.provides_exclude.clone(),
        run_deps: package.run_deps.clone(),
        run_deps_exclude: package.run_deps_exclude.clone(),
        paths: package
            .paths
            .iter()
            .map(|value| match value.kind {
                PathKind::Any => PathSpec::Any {
                    path: value.path.clone(),
                },
                PathKind::Exe => PathSpec::Exe {
                    path: value.path.clone(),
                },
                PathKind::Symlink => PathSpec::Symlink {
                    path: value.path.clone(),
                },
                PathKind::Special => PathSpec::Special {
                    path: value.path.clone(),
                },
            })
            .collect(),
        conflicts: package.conflicts.clone(),
    }
}

/// Evaluate a macro module with the restricted default evaluator.
pub fn evaluate_gluon(source: &Source) -> Result<EvaluatedMacros, MacrosEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

/// Evaluate a macro module using caller-selected resource and import policy.
pub fn evaluate_gluon_with(evaluator: &Evaluator, source: &Source) -> Result<EvaluatedMacros, MacrosEvaluationError> {
    let mut policy = evaluator.import_policy().clone();
    policy.insert_embedded_module("boulder.macros.v1", GLUON_MACROS_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(policy);
    let evaluation = evaluator.evaluate::<GluonMacrosSpec>(source)?;
    let spec = MacrosSpec::from(evaluation.value);
    spec.validate()?;

    Ok(EvaluatedMacros {
        macros: spec.into(),
        fingerprint: evaluation.fingerprint,
    })
}

/// Evaluate the single explicit repository-policy root.
///
/// The root returns ordered module references rather than forcing all policy
/// modules into one enormous structural Gluon value.
pub fn evaluate_policy_gluon_with(
    evaluator: &Evaluator,
    source: &Source,
) -> Result<EvaluatedPolicy, PolicyEvaluationError> {
    evaluate_policy_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate the policy root and bind the exact bytes of its host-resolved
/// modules into the otherwise pure evaluation fingerprint.
pub fn evaluate_policy_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedPolicy, PolicyEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.insert_embedded_module("boulder.macros.v1", GLUON_MACROS_ABI)?;
    import_policy.insert_embedded_module("boulder.policy.v1", GLUON_POLICY_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<Vec<GluonPolicyLayer>>(source, explicit_inputs)?;
    let layers = evaluation
        .value
        .into_iter()
        .map(|layer| PolicyLayer {
            name: layer.name,
            entries: layer
                .entries
                .into_iter()
                .map(|module| PolicyModule {
                    operation: module.operation.into(),
                    kind: module.kind.into(),
                    key: module.key,
                    origin: module.origin,
                })
                .collect(),
        })
        .collect();

    Ok(EvaluatedPolicy {
        layers,
        fingerprint: evaluation.fingerprint,
    })
}

/// Encode a format-neutral macro module as a canonical standalone Gluon value.
pub fn encode_gluon_spec(spec: &MacrosSpec) -> Result<String, MacrosConversionError> {
    spec.validate()?;
    Ok(encode_valid_spec(spec))
}

/// Encode domain macros through their format-neutral representation.
pub fn encode_gluon(macros: &Macros) -> Result<String, MacrosConversionError> {
    encode_gluon_spec(&MacrosSpec::from(macros))
}

fn encode_valid_spec(spec: &MacrosSpec) -> String {
    let mut output = String::from(STANDALONE_GLUON_TYPES);
    output.push_str("{\n");
    encode_key_values(&mut output, "actions", &spec.actions, encode_action);
    encode_key_values(&mut output, "definitions", &spec.definitions, |output, value| {
        output.push_str(&gluon_text(value));
    });
    encode_key_values(&mut output, "flags", &spec.flags, encode_tuning_flag);
    encode_key_values(&mut output, "tuning", &spec.tuning, encode_tuning_group);
    encode_key_values(&mut output, "packages", &spec.packages, encode_package);
    encode_string_array_field(&mut output, 1, "default_tuning_groups", &spec.default_tuning_groups);
    output.push_str("}\n");
    output
}

fn encode_key_values<T>(
    output: &mut String,
    field: &str,
    values: &[KeyValueSpec<T>],
    encode_value: impl Fn(&mut String, &T),
) {
    writeln!(output, "    {field} = [").unwrap();
    for value in values {
        output.push_str("        {\n");
        writeln!(output, "            key = {},", gluon_string(&value.key)).unwrap();
        output.push_str("            value = ");
        encode_value(output, &value.value);
        output.push_str(",\n");
        output.push_str("        },\n");
    }
    output.push_str("    ],\n");
}

fn encode_action(output: &mut String, action: &ActionSpec) {
    output.push_str("{\n");
    writeln!(
        output,
        "                description = {},",
        gluon_text(&action.description)
    )
    .unwrap();
    writeln!(
        output,
        "                example = {},",
        gluon_optional_text(action.example.as_deref())
    )
    .unwrap();
    writeln!(output, "                command = {},", gluon_text(&action.command)).unwrap();
    encode_string_array_field(output, 4, "dependencies", &action.dependencies);
    output.push_str("            }");
}

fn encode_tuning_flag(output: &mut String, flag: &TuningFlagSpec) {
    output.push_str("{\n");
    encode_compiler_flags_field(output, "root", &flag.root);
    encode_compiler_flags_field(output, "gnu", &flag.gnu);
    encode_compiler_flags_field(output, "llvm", &flag.llvm);
    output.push_str("            }");
}

fn encode_compiler_flags_field(output: &mut String, field: &str, flags: &CompilerFlagsSpec) {
    writeln!(output, "                {field} = {{").unwrap();
    for (name, value) in [
        ("c", flags.c.as_deref()),
        ("cxx", flags.cxx.as_deref()),
        ("f", flags.f.as_deref()),
        ("d", flags.d.as_deref()),
        ("rust", flags.rust.as_deref()),
        ("vala", flags.vala.as_deref()),
        ("go", flags.go.as_deref()),
        ("ld", flags.ld.as_deref()),
    ] {
        writeln!(output, "                    {name} = {},", gluon_optional_string(value)).unwrap();
    }
    output.push_str("                },\n");
}

fn encode_tuning_group(output: &mut String, group: &TuningGroupSpec) {
    output.push_str("{\n");
    output.push_str("                root = ");
    encode_tuning_option(output, &group.root, 4);
    output.push_str(",\n");
    writeln!(
        output,
        "                default = {},",
        gluon_optional_string(group.default.as_deref())
    )
    .unwrap();
    output.push_str("                choices = [\n");
    for choice in &group.choices {
        output.push_str("                    {\n");
        writeln!(output, "                        key = {},", gluon_string(&choice.key)).unwrap();
        output.push_str("                        value = ");
        encode_tuning_option(output, &choice.value, 6);
        output.push_str(",\n");
        output.push_str("                    },\n");
    }
    output.push_str("                ],\n");
    output.push_str("            }");
}

fn encode_tuning_option(output: &mut String, option: &TuningOptionSpec, indent: usize) {
    output.push_str("{\n");
    encode_string_array_field(output, indent + 1, "enabled", &option.enabled);
    encode_string_array_field(output, indent + 1, "disabled", &option.disabled);
    write_indent(output, indent);
    output.push('}');
}

fn encode_package(output: &mut String, package: &PackageSpec) {
    output.push_str("{\n");
    writeln!(
        output,
        "                summary = {},",
        gluon_optional_text(package.summary.as_deref())
    )
    .unwrap();
    writeln!(
        output,
        "                description = {},",
        gluon_optional_text(package.description.as_deref())
    )
    .unwrap();
    encode_string_array_field(output, 4, "provides_exclude", &package.provides_exclude);
    encode_string_array_field(output, 4, "run_deps", &package.run_deps);
    encode_string_array_field(output, 4, "run_deps_exclude", &package.run_deps_exclude);
    output.push_str("                paths = [\n");
    for path in &package.paths {
        let (variant, path) = match path {
            PathSpec::Any { path } => ("Any", path),
            PathSpec::Exe { path } => ("Exe", path),
            PathSpec::Symlink { path } => ("Symlink", path),
            PathSpec::Special { path } => ("Special", path),
        };
        writeln!(
            output,
            "                    {variant} {{ path = {} }},",
            gluon_string(path)
        )
        .unwrap();
    }
    output.push_str("                ],\n");
    encode_string_array_field(output, 4, "conflicts", &package.conflicts);
    output.push_str("            }");
}

fn encode_string_array_field(output: &mut String, indent: usize, field: &str, values: &[String]) {
    write_indent(output, indent);
    writeln!(output, "{field} = [").unwrap();
    for value in values {
        write_indent(output, indent + 1);
        writeln!(output, "{},", gluon_string(value)).unwrap();
    }
    write_indent(output, indent);
    output.push_str("],\n");
}

fn write_indent(output: &mut String, indent: usize) {
    for _ in 0..indent {
        output.push_str("    ");
    }
}

fn gluon_optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {}", gluon_string(value)))
}

fn gluon_optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {}", gluon_text(value)))
}

fn gluon_text(value: &str) -> String {
    if value.contains('\n') || (value.len() > 80 && (value.contains('"') || value.contains('\\'))) {
        gluon_raw_string(value)
    } else {
        gluon_string(value)
    }
}

fn gluon_raw_string(value: &str) -> String {
    for delimiter_count in 0.. {
        let hashes = "#".repeat(delimiter_count);
        if !value.contains(&format!("\"{hashes}")) {
            return format!("r{hashes}\"{value}\"{hashes}");
        }
    }
    unreachable!("an unbounded raw-string delimiter always has a free length")
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
mod tests {
    use gluon_config::{DiagnosticCategory, LimitKind, Limits};

    use super::*;
    use crate::tuning::{CompilerFlag, Toolchain};

    fn authored(body: &str) -> Source {
        Source::new("macros.glu", format!("let boulder = import! boulder.macros.v1\n{body}"))
    }

    #[test]
    fn evaluates_empty_and_maximal_abi_shapes() {
        let empty = evaluate_gluon(&authored("boulder.macros")).unwrap();
        assert_eq!(MacrosSpec::from(&empty.macros), MacrosSpec::default());
        assert_eq!(empty.fingerprint.imported_modules[0].logical_name, "boulder.macros.v1");

        let evaluated = evaluate_gluon(&authored(maximal_gluon())).unwrap();
        let expected = maximal_spec();

        assert_eq!(MacrosSpec::from(&evaluated.macros), expected);
        assert_eq!(evaluated.macros.actions[0].value.dependencies, ["cmake", "ninja"]);
        let flag = &evaluated.macros.flags[0].value;
        assert_eq!(flag.get(CompilerFlag::C, Toolchain::Llvm), Some("-O2"));
        assert_eq!(flag.get(CompilerFlag::C, Toolchain::Gnu), Some("-O3"));
        assert_eq!(flag.get(CompilerFlag::Cxx, Toolchain::Llvm), Some("-stdlib=libc++"));
        assert_eq!(evaluated.macros.tuning[0].value.default.as_deref(), Some("fast"));
        assert_eq!(evaluated.macros.packages[0].value.paths.len(), 4);
        assert_eq!(evaluated.macros.default_tuning_groups, ["optimize"]);
    }

    #[test]
    fn action_dependencies_are_preserved_by_the_constructor() {
        let evaluated = evaluate_gluon(&authored(
            r#"{
    actions = [boulder.named "configure" (boulder.action.with_dependencies ["cmake", "ninja"]
        (boulder.action.new "Configure" "cmake -B build"))],
    .. boulder.macros
}"#,
        ))
        .unwrap();

        assert_eq!(evaluated.macros.actions[0].value.command, "cmake -B build");
        assert_eq!(evaluated.macros.actions[0].value.dependencies, ["cmake", "ninja"]);
    }

    #[test]
    fn policy_abi_preserves_named_layer_and_entry_order() {
        let source = Source::new(
            "policy.glu",
            r#"let policy = import! boulder.policy.v1
policy.policy [
    policy.layer "foundation" [
        policy.add (policy.actions "build" "actions/build.glu"),
    ],
    policy.layer "overrides" [
        policy.modify (policy.actions "build" "actions/override.glu"),
        policy.add (policy.architecture "x86_64" "arch/x86_64.glu"),
    ],
]
"#,
        );

        let evaluated = evaluate_policy_gluon_with(&Evaluator::default(), &source).unwrap();

        assert_eq!(evaluated.layers.len(), 2);
        assert_eq!(evaluated.layers[0].name, "foundation");
        assert_eq!(evaluated.layers[0].entries[0].origin, "actions/build.glu");
        assert_eq!(evaluated.layers[1].name, "overrides");
        assert_eq!(evaluated.layers[1].entries.len(), 2);
        assert_eq!(evaluated.layers[1].entries[0].operation, PolicyOperation::Modify);
        assert_eq!(evaluated.layers[1].entries[1].kind, PolicyKind::Architecture);
    }

    #[test]
    fn immutable_record_setters_compose_policy_fragments() {
        let evaluated = evaluate_gluon(&authored(
            r#"let definitions = boulder.set.definitions [boulder.definition "prefix" "/usr"] boulder.macros
let actions = [boulder.named "build" (boulder.action.new "Build" "ninja")]
boulder.set.actions actions definitions"#,
        ))
        .unwrap();

        assert_eq!(evaluated.macros.definitions[0].key, "prefix");
        assert_eq!(evaluated.macros.actions[0].value.command, "ninja");
    }

    #[test]
    fn flag_tuning_and_package_conversion_matches_the_format_neutral_spec() {
        let evaluated = evaluate_gluon(&authored(maximal_gluon())).unwrap();

        assert_eq!(MacrosSpec::from(&evaluated.macros), maximal_spec());
    }

    #[test]
    fn flag_tuning_and_package_defaults_are_explicit() {
        let evaluated = evaluate_gluon(&authored(
            r#"{
    flags = [boulder.named "empty" boulder.defaults.flag],
    tuning = [boulder.named "empty" boulder.defaults.tuning_group],
    packages = [boulder.named "empty" boulder.package.default],
    .. boulder.macros
}"#,
        ))
        .unwrap();
        let spec = MacrosSpec::from(&evaluated.macros);

        assert_eq!(spec.flags[0].value, TuningFlagSpec::default());
        assert_eq!(spec.tuning[0].value, TuningGroupSpec::default());
        assert_eq!(spec.packages[0].value, PackageSpec::default());
    }

    #[test]
    fn invalid_types_option_ranges_and_paths_are_structured() {
        let wrong_type = evaluate_gluon(&authored("{ actions = 1, .. boulder.macros }")).unwrap_err();
        assert!(matches!(
            wrong_type,
            MacrosEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type
        ));

        let unknown_default = evaluate_gluon(&authored(
            r#"{
    tuning = [boulder.named "mode" (boulder.tuning.group_with {
        root = boulder.defaults.tuning_option,
        default = boulder.optional.some "missing",
        choices = [],
    })],
    .. boulder.macros
}"#,
        ))
        .unwrap_err();
        assert!(matches!(
            unknown_default,
            MacrosEvaluationError::Conversion(ref error)
                if error.field() == "tuning[0].value.default"
        ));

        let empty_path = evaluate_gluon(&authored(
            r#"{
    packages = [boulder.named "bad" (boulder.package.from_record {
        paths = [boulder.package.path.any ""],
        .. boulder.package.default
    })],
    .. boulder.macros
}"#,
        ))
        .unwrap_err();
        assert!(matches!(
            empty_path,
            MacrosEvaluationError::Conversion(ref error)
                if error.field() == "packages[0].value.paths[0].path"
        ));

        let limits = Limits {
            max_source_bytes: 8,
            ..Limits::default()
        };
        let out_of_range = evaluate_gluon_with(&Evaluator::new(limits), &authored("boulder.macros")).unwrap_err();
        assert!(matches!(
            out_of_range,
            MacrosEvaluationError::Evaluation(ref error)
                if error.limit == Some(LimitKind::SourceSize)
        ));
    }

    #[test]
    fn macro_relations_distinguish_strict_actions_from_deferred_packages() {
        let deferred_package = evaluate_gluon(&authored(
            r#"{
    packages = [boulder.named "devel" (boulder.package.from_record {
        run_deps = ["%(name)", "binary(%(tool))"],
        .. boulder.package.default
    })],
    .. boulder.macros
}"#,
        ))
        .unwrap();
        assert_eq!(
            deferred_package.macros.packages[0].value.run_deps,
            ["%(name)", "binary(%(tool))"]
        );

        let invalid_action = evaluate_gluon(&authored(
            r#"{
    actions = [boulder.named "build" (boulder.action.with_dependencies
        ["valid", "unknown(target)"]
        (boulder.action.new "Build" "make"))],
    .. boulder.macros
}"#,
        ))
        .unwrap_err();
        assert!(matches!(
            invalid_action,
            MacrosEvaluationError::Conversion(MacrosConversionError::InvalidRelation(
                ValidationError::InvalidDependency { ref field, .. }
            )) if field == "actions[0].value.dependencies[1]"
        ));

        let invalid_package = evaluate_gluon(&authored(
            r#"{
    packages = [boulder.named "devel" (boulder.package.from_record {
        conflicts = ["valid", "binary(unclosed"],
        .. boulder.package.default
    })],
    .. boulder.macros
}"#,
        ))
        .unwrap_err();
        assert!(matches!(
            invalid_package,
            MacrosEvaluationError::Conversion(MacrosConversionError::InvalidRelation(
                ValidationError::InvalidProvider { ref field, .. }
            )) if field == "packages[0].value.conflicts[1]"
        ));
    }

    #[test]
    fn forbidden_imports_fail_and_fingerprints_are_deterministic() {
        let forbidden = evaluate_gluon(&authored("let _ = import! std.process\nboulder.macros")).unwrap_err();
        assert!(matches!(
            forbidden,
            MacrosEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Import
        ));

        let source = authored(maximal_gluon());
        let first = evaluate_gluon(&source).unwrap();
        let repeated = evaluate_gluon(&source).unwrap();
        assert_eq!(first.fingerprint, repeated.fingerprint);
    }

    #[test]
    fn canonical_encoder_round_trips_and_uses_safe_multiline_raw_strings() {
        let expected = maximal_spec();
        let encoded = encode_gluon_spec(&expected).unwrap();

        assert!(encoded.starts_with("type Optional a ="));
        assert!(encoded.contains("r##\"echo \"# boundary\nnext\"##"));
        assert!(!encoded.contains("import!"));

        let evaluated = evaluate_gluon(&Source::new("generated-macros.glu", &encoded)).unwrap();
        assert_eq!(MacrosSpec::from(&evaluated.macros), expected);
        assert_eq!(evaluated.macros.actions[0].value.command, "echo \"# boundary\nnext");

        let from_domain = encode_gluon(&evaluated.macros).unwrap();
        assert_eq!(from_domain, encoded);
    }

    fn maximal_gluon() -> &'static str {
        r##"boulder.module {
    actions = [boulder.named "build" (boulder.action.with_dependencies ["cmake", "ninja"]
        (boulder.action.from_record {
            description = "Build the project",
            example = boulder.optional.some "boulder build",
            command = "echo \"# boundary\nnext",
            dependencies = [],
        }))],
    definitions = [boulder.definition "prefix" "/usr"],
    flags = [boulder.named "optimize" (boulder.flag.from_record {
        root = boulder.flag.compiler_flags {
            c = boulder.optional.some "-O2",
            cxx = boulder.optional.some "-O2-cxx",
            f = boulder.optional.some "-O2-f",
            d = boulder.optional.some "-O2-d",
            rust = boulder.optional.some "-Copt-level=2",
            vala = boulder.optional.some "--target-glib=2.80",
            go = boulder.optional.some "-trimpath",
            ld = boulder.optional.some "-Wl,-O2",
        },
        gnu = boulder.flag.compiler_flags {
            c = boulder.optional.some "-O3",
            .. boulder.defaults.compiler_flags
        },
        llvm = boulder.flag.compiler_flags {
            cxx = boulder.optional.some "-stdlib=libc++",
            .. boulder.defaults.compiler_flags
        },
    })],
    tuning = [boulder.named "optimize" (boulder.tuning.group_with {
        root = boulder.tuning.option ["optimize"] ["debug"],
        default = boulder.optional.some "fast",
        choices = [boulder.tuning.choice "fast" (boulder.tuning.option ["optimize-fast"] [])],
    })],
    packages = [boulder.named "main" (boulder.package.from_record {
        summary = boulder.optional.some "Main package",
        description = boulder.optional.some "All runtime files",
        provides_exclude = ["provided(*)"],
        run_deps = ["runtime"],
        run_deps_exclude = ["excluded(*)"],
        paths = [
            boulder.package.path.any "/usr/share/example",
            boulder.package.path.exe "/usr/bin/example",
            boulder.package.path.symlink "/usr/bin/example-link",
            boulder.package.path.special "/usr/lib/example.special",
        ],
        conflicts = ["other"],
    })],
    default_tuning_groups = ["optimize"],
}"##
    }

    fn maximal_spec() -> MacrosSpec {
        MacrosSpec {
            actions: vec![KeyValueSpec {
                key: "build".to_owned(),
                value: ActionSpec {
                    description: "Build the project".to_owned(),
                    example: Some("boulder build".to_owned()),
                    command: "echo \"# boundary\nnext".to_owned(),
                    dependencies: vec!["cmake".to_owned(), "ninja".to_owned()],
                },
            }],
            definitions: vec![KeyValueSpec {
                key: "prefix".to_owned(),
                value: "/usr".to_owned(),
            }],
            flags: vec![KeyValueSpec {
                key: "optimize".to_owned(),
                value: TuningFlagSpec {
                    root: CompilerFlagsSpec {
                        c: Some("-O2".to_owned()),
                        cxx: Some("-O2-cxx".to_owned()),
                        f: Some("-O2-f".to_owned()),
                        d: Some("-O2-d".to_owned()),
                        rust: Some("-Copt-level=2".to_owned()),
                        vala: Some("--target-glib=2.80".to_owned()),
                        go: Some("-trimpath".to_owned()),
                        ld: Some("-Wl,-O2".to_owned()),
                    },
                    gnu: CompilerFlagsSpec {
                        c: Some("-O3".to_owned()),
                        ..CompilerFlagsSpec::default()
                    },
                    llvm: CompilerFlagsSpec {
                        cxx: Some("-stdlib=libc++".to_owned()),
                        ..CompilerFlagsSpec::default()
                    },
                },
            }],
            tuning: vec![KeyValueSpec {
                key: "optimize".to_owned(),
                value: TuningGroupSpec {
                    root: TuningOptionSpec {
                        enabled: vec!["optimize".to_owned()],
                        disabled: vec!["debug".to_owned()],
                    },
                    default: Some("fast".to_owned()),
                    choices: vec![KeyValueSpec {
                        key: "fast".to_owned(),
                        value: TuningOptionSpec {
                            enabled: vec!["optimize-fast".to_owned()],
                            disabled: Vec::new(),
                        },
                    }],
                },
            }],
            packages: vec![KeyValueSpec {
                key: "main".to_owned(),
                value: PackageSpec {
                    summary: Some("Main package".to_owned()),
                    description: Some("All runtime files".to_owned()),
                    provides_exclude: vec!["provided(*)".to_owned()],
                    run_deps: vec!["runtime".to_owned()],
                    run_deps_exclude: vec!["excluded(*)".to_owned()],
                    paths: vec![
                        PathSpec::Any {
                            path: "/usr/share/example".to_owned(),
                        },
                        PathSpec::Exe {
                            path: "/usr/bin/example".to_owned(),
                        },
                        PathSpec::Symlink {
                            path: "/usr/bin/example-link".to_owned(),
                        },
                        PathSpec::Special {
                            path: "/usr/lib/example.special".to_owned(),
                        },
                    ],
                    conflicts: vec!["other".to_owned()],
                },
            }],
            default_tuning_groups: vec!["optimize".to_owned()],
        }
    }
}
