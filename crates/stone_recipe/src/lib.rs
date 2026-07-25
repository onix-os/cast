pub use self::spec::{NamedTuningSpec, OptionsSpec, PathSpec, ToolchainSpec, TuningSpec, UpstreamSpec};

pub mod build_policy;
pub mod derivation;
pub mod package;
pub mod spec;
pub mod upstream;

#[cfg(test)]
mod test {
    use super::*;
    use declarative_config::{DeclarationEvaluator, Source};

    #[test]
    fn evaluate_repository_gluon_fixtures() {
        let inputs = [
            (
                "tests/fixtures/llvm-stone.glu",
                include_str!("../../../tests/fixtures/llvm-stone.glu"),
            ),
            (
                "tests/fixtures/cast-stone.glu",
                include_str!("../../../tests/fixtures/cast-stone.glu"),
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
                "tests/fixtures/cast-concurrency-test.glu",
                include_str!("../../../tests/fixtures/cast-concurrency-test.glu"),
            ),
        ];

        for (logical_name, input) in inputs {
            let evaluated = DeclarationEvaluator::<package::PackageSpec>::evaluate(
                &package::GluonPackageEvaluator::default(),
                &Source::new(logical_name, input),
            )
            .unwrap();
            evaluated.value.validate().unwrap();
            if logical_name == "tests/fixtures/llvm-stone.glu" {
                assert_eq!(
                    evaluated
                        .value
                        .builder
                        .required_tools()
                        .iter()
                        .map(|dependency| dependency.dependency().unwrap().to_name())
                        .collect::<Vec<_>>(),
                    [
                        "binary(cmake)",
                        "binary(sh)",
                        "binary(ninja)",
                        "binary(perl)",
                        "binary(python3)",
                    ]
                );
            }
        }
    }
}
