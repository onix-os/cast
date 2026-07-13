// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub use self::spec::{KeyValueSpec, OptionsSpec, PathSpec, ToolchainSpec, TuningSpec, UpstreamSpec};

pub mod build_policy;
pub mod derivation;
pub mod package;
pub mod spec;
pub mod upstream;

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
