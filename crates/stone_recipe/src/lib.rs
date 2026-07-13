// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub use self::macros::{
    ActionSpec, EvaluatedMacros, EvaluatedPolicy, GLUON_MACROS_ABI, GLUON_POLICY_ABI, MACROS_ABI_VERSION, Macros,
    MacrosConversionError, MacrosEvaluationError, MacrosSpec, POLICY_ABI_VERSION, PolicyEvaluationError, PolicyKind,
    PolicyModule, PolicyOperation, encode_gluon as encode_macros_gluon, encode_gluon_spec as encode_macros_gluon_spec,
    evaluate_gluon as evaluate_macros_gluon, evaluate_gluon_with as evaluate_macros_gluon_with,
    evaluate_policy_gluon_with, evaluate_policy_gluon_with_inputs,
};
pub use self::script::Script;
pub use self::spec::{
    BuildSpec, KeyValueSpec, OptionsSpec, PackageSpec, PathSpec, RecipeConversionError, RecipeSpec, SourceSpec,
    ToolchainSpec, TuningSpec, UpstreamSpec,
};
pub use self::tuning::{CompilerFlagsSpec, Tuning, TuningFlagSpec, TuningGroupSpec, TuningOptionSpec};
pub use self::upstream::Upstream;
pub use self::validation::ValidationError;

pub mod derivation;
pub mod macros;
pub mod package;
pub mod script;
pub mod spec;
pub mod tuning;
pub mod upstream;
pub mod validation;

#[derive(Debug, Clone)]
pub struct Recipe {
    pub source: Source,
    pub build: Build,
    pub package: Package,
    pub options: Options,
    pub profiles: Vec<KeyValue<Build>>,
    pub sub_packages: Vec<KeyValue<Package>>,
    pub upstreams: Vec<Upstream>,
    pub architectures: Vec<String>,
    pub tuning: Vec<KeyValue<Tuning>>,
    pub emul32: bool,
    pub mold: bool,
}

impl Recipe {
    /// Validate the format-independent invariants of a recipe.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validation::validate(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValue<T> {
    pub key: String,
    pub value: T,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub version: String,
    pub release: u64,
    pub homepage: String,
    pub license: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Build {
    pub setup: Option<String>,
    pub build: Option<String>,
    pub install: Option<String>,
    pub check: Option<String>,
    pub workload: Option<String>,
    pub environment: Option<String>,
    pub build_deps: Vec<String>,
    pub check_deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub toolchain: tuning::Toolchain,
    pub cspgo: bool,
    pub samplepgo: bool,
    pub debug: bool,
    pub strip: bool,
    pub networking: bool,
    pub compressman: bool,
    pub lastrip: bool,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub run_deps: Vec<String>,
    pub run_deps_exclude: Vec<String>,
    pub paths: Vec<Path>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    pub path: String,
    pub kind: PathKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, Default)]
#[strum(serialize_all = "lowercase")]
pub enum PathKind {
    #[default]
    Any,
    Exe,
    Symlink,
    Special,
}

#[cfg(test)]
mod test {
    use super::*;
    use gluon_config::Source as GluonSource;

    #[test]
    fn evaluate_repository_gluon_fixtures() {
        let inputs = [
            (
                "tests/fixtures/llvm-stone.glu",
                include_str!("../../../tests/fixtures/llvm-stone.glu"),
            ),
            (
                "tests/fixtures/boulder-stone.glu",
                include_str!("../../../tests/fixtures/boulder-stone.glu"),
            ),
            (
                "tests/fixtures/conflicts/italian-pizza.glu",
                include_str!("../../../tests/fixtures/conflicts/italian-pizza.glu"),
            ),
            (
                "tests/fixtures/conflicts/pineapple.glu",
                include_str!("../../../tests/fixtures/conflicts/pineapple.glu"),
            ),
            (
                "tests/fixtures/boulder-concurrency-test.glu",
                include_str!("../../../tests/fixtures/boulder-concurrency-test.glu"),
            ),
        ];

        for (logical_name, input) in inputs {
            let evaluated = package::evaluate_gluon(&GluonSource::new(logical_name, input)).unwrap();
            evaluated.recipe.validate().unwrap();
        }
    }
}
