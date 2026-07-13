// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Typed package declarations for the `boulder.package.v2` Gluon ABI.
//!
//! A package factory is evaluated completely inside Gluon and produces one
//! concrete [`PackageSpec`]. This module deliberately contains values only:
//! Rust never receives or retains a Gluon closure. The specification still
//! lowers into the existing [`crate::Recipe`] domain model while Boulder is
//! migrated to consume package declarations directly.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{
    BuildSpec, KeyValueSpec, OptionsSpec, PackageSpec as LegacyPackageSpec, PathSpec, Recipe, RecipeConversionError,
    RecipeSpec, SourceSpec, TuningSpec, UpstreamSpec,
};

pub use self::gluon::{
    EvaluatedPackage, GLUON_PACKAGE_ABI, PACKAGE_ABI_VERSION, PackageEvaluationError, evaluate_gluon,
    evaluate_gluon_with, evaluate_gluon_with_inputs,
};

mod gluon;

/// One pure, concrete package declaration returned by a Gluon package factory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    pub meta: MetaSpec,
    pub scripts: ScriptsSpec,
    pub native_build_inputs: Vec<DependencySpec>,
    pub build_inputs: Vec<DependencySpec>,
    pub check_inputs: Vec<DependencySpec>,
    pub outputs: Vec<OutputSpec>,
    pub options: OptionsSpec,
    pub profiles: Vec<ProfileSpec>,
    pub sources: Vec<UpstreamSpec>,
    pub architectures: Vec<String>,
    pub tuning: Vec<KeyValueSpec<TuningSpec>>,
    pub emul32: bool,
    pub mold: bool,
}

/// Package identity and user-facing source metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaSpec {
    pub pname: String,
    pub version: String,
    pub release: i64,
    pub homepage: String,
    pub license: Vec<String>,
}

/// Transitional shell phases.
///
/// Structured builders will replace these fields. Keeping them in v2 for now
/// permits deterministic lowering without changing Boulder's executor in the
/// same slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptsSpec {
    pub setup: Option<String>,
    pub build: Option<String>,
    pub install: Option<String>,
    pub check: Option<String>,
    pub workload: Option<String>,
    pub environment: Option<String>,
}

/// A target-specific package build profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSpec {
    pub name: String,
    pub scripts: ScriptsSpec,
    pub native_build_inputs: Vec<DependencySpec>,
    pub build_inputs: Vec<DependencySpec>,
    pub check_inputs: Vec<DependencySpec>,
}

/// One explicit Stone output.
///
/// `out` is the root package. Other local names lower temporarily to
/// `<pname>-<name>` subpackages until Boulder and Stone consume output names
/// directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    pub name: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_inputs: Vec<DependencySpec>,
    pub runtime_exclude: Vec<String>,
    pub paths: Vec<PathSpec>,
    pub conflicts: Vec<DependencySpec>,
}

/// A symbolic package name supplied to a package factory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageRef {
    pub name: String,
}

/// A named output of a symbolic package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutputRef {
    pub package: PackageRef,
    pub output: String,
}

/// A typed package relationship.
///
/// The current recipe domain still stores Moss provider expressions as
/// strings. [`DependencySpec::provider`] is the one transitional lowering
/// point; authored v2 packages never construct those strings themselves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencySpec {
    Package(PackageRef),
    Output(OutputRef),
    Binary(String),
    SystemBinary(String),
    PkgConfig(String),
    PkgConfig32(String),
    Soname(String),
    CMake(String),
    Python(String),
    Interpreter(String),
}

impl DependencySpec {
    /// Lower a typed dependency into the provider syntax consumed by Moss.
    pub fn provider(&self) -> String {
        match self {
            Self::Package(package) => package.name.clone(),
            Self::Output(output) if output.output == "out" => output.package.name.clone(),
            Self::Output(output) => format!("{}-{}", output.package.name, output.output),
            Self::Binary(target) => format!("binary({target})"),
            Self::SystemBinary(target) => format!("sysbinary({target})"),
            Self::PkgConfig(target) => format!("pkgconfig({target})"),
            Self::PkgConfig32(target) => format!("pkgconfig32({target})"),
            Self::Soname(target) => format!("soname({target})"),
            Self::CMake(target) => format!("cmake({target})"),
            Self::Python(target) => format!("python({target})"),
            Self::Interpreter(target) => format!("interpreter({target})"),
        }
    }

    fn package_and_output(&self) -> Option<(&str, &str)> {
        match self {
            Self::Package(package) => Some((&package.name, "out")),
            Self::Output(output) => Some((&output.package.name, &output.output)),
            _ => None,
        }
    }

    fn target(&self) -> &str {
        match self {
            Self::Package(package) => &package.name,
            Self::Output(output) => &output.output,
            Self::Binary(target)
            | Self::SystemBinary(target)
            | Self::PkgConfig(target)
            | Self::PkgConfig32(target)
            | Self::Soname(target)
            | Self::CMake(target)
            | Self::Python(target)
            | Self::Interpreter(target) => target,
        }
    }
}

/// Failure to lower a v2 package declaration into the current recipe domain.
#[derive(Debug, Error)]
pub enum PackageConversionError {
    #[error("{field}: package or relation target must not be empty")]
    EmptyDependencyTarget { field: String },
    #[error("outputs: package must declare exactly one `out` output")]
    MissingRootOutput,
    #[error("outputs[{index}].name: duplicate output name `{name}`")]
    DuplicateOutput { index: usize, name: String },
    #[error("outputs[{index}].name: invalid output name `{name}`")]
    InvalidOutputName { index: usize, name: String },
    #[error("{field}: output `{output}` does not exist in package `{package}`")]
    MissingOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: package output dependency cycle: {cycle}")]
    OutputDependencyCycle { field: String, cycle: String },
    #[error(transparent)]
    Recipe(#[from] RecipeConversionError),
}

impl PackageConversionError {
    pub fn field(&self) -> &str {
        match self {
            Self::EmptyDependencyTarget { field }
            | Self::MissingOutputReference { field, .. }
            | Self::OutputDependencyCycle { field, .. } => field,
            Self::MissingRootOutput => "outputs",
            Self::DuplicateOutput { .. } | Self::InvalidOutputName { .. } => "outputs",
            Self::Recipe(error) => error.field(),
        }
    }
}

impl TryFrom<PackageSpec> for Recipe {
    type Error = PackageConversionError;

    fn try_from(package: PackageSpec) -> Result<Self, Self::Error> {
        package.validate_relations()?;

        let PackageSpec {
            meta,
            scripts,
            native_build_inputs,
            build_inputs,
            check_inputs,
            outputs,
            options,
            profiles,
            sources,
            architectures,
            tuning,
            emul32,
            mold,
        } = package;

        let root_index = outputs
            .iter()
            .position(|output| output.name == "out")
            .ok_or(PackageConversionError::MissingRootOutput)?;
        let root = outputs[root_index].clone();

        let build_deps = native_build_inputs
            .into_iter()
            .chain(build_inputs)
            .map(|dependency| dependency.provider())
            .collect();
        let check_deps = check_inputs
            .into_iter()
            .map(|dependency| dependency.provider())
            .collect();

        let profiles = profiles
            .into_iter()
            .map(|profile| {
                let key = profile.name.clone();
                KeyValueSpec {
                    key,
                    value: profile.into_build_spec(),
                }
            })
            .collect();

        let sub_packages = outputs
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index != root_index)
            .map(|(_, output)| KeyValueSpec {
                key: format!("{}-{}", meta.pname, output.name),
                value: output.into_legacy(),
            })
            .collect();

        let recipe = RecipeSpec {
            source: SourceSpec {
                name: meta.pname,
                version: meta.version,
                release: meta.release,
                homepage: meta.homepage,
                license: meta.license,
            },
            build: scripts.into_build_spec(build_deps, check_deps),
            package: root.into_legacy(),
            options,
            profiles,
            sub_packages,
            upstreams: sources,
            architectures,
            tuning,
            emul32,
            mold,
        };

        Ok(Recipe::try_from(recipe)?)
    }
}

impl PackageSpec {
    fn validate_relations(&self) -> Result<(), PackageConversionError> {
        let mut outputs = BTreeMap::new();
        for (index, output) in self.outputs.iter().enumerate() {
            if !valid_output_name(&output.name) {
                return Err(PackageConversionError::InvalidOutputName {
                    index,
                    name: output.name.clone(),
                });
            }
            if outputs.insert(output.name.as_str(), index).is_some() {
                return Err(PackageConversionError::DuplicateOutput {
                    index,
                    name: output.name.clone(),
                });
            }
        }
        if !outputs.contains_key("out") {
            return Err(PackageConversionError::MissingRootOutput);
        }

        self.validate_dependency_list(&self.native_build_inputs, "native_build_inputs", &outputs)?;
        self.validate_dependency_list(&self.build_inputs, "build_inputs", &outputs)?;
        self.validate_dependency_list(&self.check_inputs, "check_inputs", &outputs)?;

        for (index, profile) in self.profiles.iter().enumerate() {
            let parent = format!("profiles[{index}]");
            self.validate_dependency_list(
                &profile.native_build_inputs,
                &format!("{parent}.native_build_inputs"),
                &outputs,
            )?;
            self.validate_dependency_list(&profile.build_inputs, &format!("{parent}.build_inputs"), &outputs)?;
            self.validate_dependency_list(&profile.check_inputs, &format!("{parent}.check_inputs"), &outputs)?;
        }

        for (index, output) in self.outputs.iter().enumerate() {
            self.validate_dependency_list(
                &output.runtime_inputs,
                &format!("outputs[{index}].runtime_inputs"),
                &outputs,
            )?;
            self.validate_dependency_list(&output.conflicts, &format!("outputs[{index}].conflicts"), &outputs)?;
        }

        self.validate_output_cycles(&outputs)
    }

    fn validate_dependency_list(
        &self,
        dependencies: &[DependencySpec],
        field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        for (index, dependency) in dependencies.iter().enumerate() {
            let field = format!("{field}[{index}]");
            if dependency.target().is_empty() {
                return Err(PackageConversionError::EmptyDependencyTarget { field });
            }
            if let Some((package, output)) = dependency.package_and_output()
                && package == self.meta.pname
                && !outputs.contains_key(output)
            {
                return Err(PackageConversionError::MissingOutputReference {
                    field,
                    package: package.to_owned(),
                    output: output.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn validate_output_cycles(&self, outputs: &BTreeMap<&str, usize>) -> Result<(), PackageConversionError> {
        let mut edges = BTreeMap::<&str, Vec<(&str, String)>>::new();
        for (index, output) in self.outputs.iter().enumerate() {
            let dependencies = output
                .runtime_inputs
                .iter()
                .enumerate()
                .filter_map(|(dependency_index, dependency)| {
                    let (package, target) = dependency.package_and_output()?;
                    (package == self.meta.pname && outputs.contains_key(target))
                        .then(|| (target, format!("outputs[{index}].runtime_inputs[{dependency_index}]")))
                })
                .collect();
            edges.insert(&output.name, dependencies);
        }

        for output in &self.outputs {
            let mut visiting = BTreeSet::new();
            let mut visited = BTreeSet::new();
            let mut path = Vec::new();
            if let Some((field, cycle)) = find_cycle(&output.name, &edges, &mut visiting, &mut visited, &mut path) {
                return Err(PackageConversionError::OutputDependencyCycle { field, cycle });
            }
        }
        Ok(())
    }
}

fn find_cycle<'a>(
    node: &'a str,
    edges: &BTreeMap<&'a str, Vec<(&'a str, String)>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<(String, String)> {
    if visited.contains(node) {
        return None;
    }
    if !visiting.insert(node) {
        let start = path.iter().position(|entry| *entry == node).unwrap_or(0);
        let mut cycle = path[start..].to_vec();
        cycle.push(node);
        return Some(("outputs".to_owned(), cycle.join(" -> ")));
    }

    path.push(node);
    for (target, field) in edges.get(node).into_iter().flatten() {
        if visiting.contains(target) {
            let start = path.iter().position(|entry| entry == target).unwrap_or(0);
            let mut cycle = path[start..].to_vec();
            cycle.push(target);
            return Some((field.clone(), cycle.join(" -> ")));
        }
        if let Some(cycle) = find_cycle(target, edges, visiting, visited, path) {
            return Some(cycle);
        }
    }
    path.pop();
    visiting.remove(node);
    visited.insert(node);
    None
}

fn valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_'))
}

impl ScriptsSpec {
    fn into_build_spec(self, build_deps: Vec<String>, check_deps: Vec<String>) -> BuildSpec {
        BuildSpec {
            setup: self.setup,
            build: self.build,
            install: self.install,
            check: self.check,
            workload: self.workload,
            environment: self.environment,
            build_deps,
            check_deps,
        }
    }
}

impl ProfileSpec {
    fn into_build_spec(self) -> BuildSpec {
        let build_deps = self
            .native_build_inputs
            .into_iter()
            .chain(self.build_inputs)
            .map(|dependency| dependency.provider())
            .collect();
        let check_deps = self
            .check_inputs
            .into_iter()
            .map(|dependency| dependency.provider())
            .collect();
        self.scripts.into_build_spec(build_deps, check_deps)
    }
}

impl OutputSpec {
    fn into_legacy(self) -> LegacyPackageSpec {
        LegacyPackageSpec {
            summary: self.summary,
            description: self.description,
            provides_exclude: self.provides_exclude,
            run_deps: self
                .runtime_inputs
                .into_iter()
                .map(|dependency| dependency.provider())
                .collect(),
            run_deps_exclude: self.runtime_exclude,
            paths: self.paths,
            conflicts: self
                .conflicts
                .into_iter()
                .map(|dependency| dependency.provider())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(name: &str) -> DependencySpec {
        DependencySpec::Package(PackageRef { name: name.to_owned() })
    }

    fn package() -> PackageSpec {
        PackageSpec {
            meta: MetaSpec {
                pname: "example".to_owned(),
                version: "1.0.0".to_owned(),
                release: 1,
                homepage: "https://example.com".to_owned(),
                license: vec!["MPL-2.0".to_owned()],
            },
            scripts: ScriptsSpec::default(),
            native_build_inputs: vec![DependencySpec::Binary("cmake".to_owned())],
            build_inputs: vec![dependency("zlib")],
            check_inputs: Vec::new(),
            outputs: vec![OutputSpec {
                name: "out".to_owned(),
                summary: None,
                description: None,
                provides_exclude: Vec::new(),
                runtime_inputs: Vec::new(),
                runtime_exclude: Vec::new(),
                paths: Vec::new(),
                conflicts: Vec::new(),
            }],
            options: OptionsSpec::default(),
            profiles: Vec::new(),
            sources: Vec::new(),
            architectures: Vec::new(),
            tuning: Vec::new(),
            emul32: false,
            mold: false,
        }
    }

    #[test]
    fn typed_dependencies_lower_at_one_boundary() {
        let recipe = Recipe::try_from(package()).unwrap();
        assert_eq!(recipe.build.build_deps, ["binary(cmake)", "zlib"]);
    }

    #[test]
    fn duplicate_and_missing_outputs_are_rejected() {
        let mut missing = package();
        missing.outputs[0].name = "dev".to_owned();
        assert!(matches!(
            Recipe::try_from(missing),
            Err(PackageConversionError::MissingRootOutput)
        ));

        let mut duplicate = package();
        duplicate.outputs.push(duplicate.outputs[0].clone());
        assert!(matches!(
            Recipe::try_from(duplicate),
            Err(PackageConversionError::DuplicateOutput { .. })
        ));
    }

    #[test]
    fn local_output_references_are_checked_for_missing_values_and_cycles() {
        let mut missing = package();
        missing.outputs[0]
            .runtime_inputs
            .push(DependencySpec::Output(OutputRef {
                package: PackageRef {
                    name: "example".to_owned(),
                },
                output: "dev".to_owned(),
            }));
        assert!(matches!(
            Recipe::try_from(missing),
            Err(PackageConversionError::MissingOutputReference { .. })
        ));

        let mut cyclic = package();
        cyclic.outputs[0].runtime_inputs.push(DependencySpec::Output(OutputRef {
            package: PackageRef {
                name: "example".to_owned(),
            },
            output: "out".to_owned(),
        }));
        assert!(matches!(
            Recipe::try_from(cyclic),
            Err(PackageConversionError::OutputDependencyCycle { .. })
        ));
    }
}
