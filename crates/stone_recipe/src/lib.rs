// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub use self::macros::{
    ActionSpec, EvaluatedMacros, EvaluatedPolicy, GLUON_MACROS_ABI, GLUON_POLICY_ABI, MACROS_ABI_VERSION, Macros,
    MacrosConversionError, MacrosEvaluationError, MacrosSpec, POLICY_ABI_VERSION, PolicyEvaluationError, PolicyKind,
    PolicyLayer, PolicyModule, PolicyOperation, encode_gluon as encode_macros_gluon,
    encode_gluon_spec as encode_macros_gluon_spec, evaluate_gluon as evaluate_macros_gluon,
    evaluate_gluon_with as evaluate_macros_gluon_with, evaluate_policy_gluon_with, evaluate_policy_gluon_with_inputs,
};
pub use self::script::Script;
pub use self::spec::{KeyValueSpec, OptionsSpec, PathSpec, ToolchainSpec, TuningSpec, UpstreamSpec};
pub use self::tuning::{CompilerFlagsSpec, TuningFlagSpec, TuningGroupSpec, TuningOptionSpec};
pub use self::validation::ValidationError;

pub mod derivation;
pub mod macros;
pub mod package;
pub mod script;
pub mod spec;
pub mod tuning;
pub mod upstream;
pub mod validation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValue<T> {
    pub key: String,
    pub value: T,
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
            evaluated.package.validate().unwrap();
        }
    }
}
