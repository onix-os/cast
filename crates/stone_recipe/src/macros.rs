// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use crate::{
    KeyValue, Package, PackageSpec,
    spec::KeyValueSpec,
    tuning::{TuningFlag, TuningFlagSpec, TuningGroup, TuningGroupSpec},
};

mod gluon;

pub use self::gluon::{
    EvaluatedMacros, GLUON_MACROS_ABI, MACROS_ABI_VERSION, MacrosConversionError, MacrosEvaluationError, encode_gluon,
    encode_gluon_spec, evaluate_gluon, evaluate_gluon_with,
};

#[derive(Debug, Clone)]
pub struct Macros {
    pub actions: Vec<KeyValue<Action>>,
    pub definitions: Vec<KeyValue<String>>,
    pub flags: Vec<KeyValue<TuningFlag>>,
    pub tuning: Vec<KeyValue<TuningGroup>>,
    pub packages: Vec<KeyValue<Package>>,
    pub default_tuning_groups: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub description: String,
    pub example: Option<String>,
    pub command: String,
    pub dependencies: Vec<String>,
}

/// Format-neutral macro/policy module value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacrosSpec {
    pub actions: Vec<KeyValueSpec<ActionSpec>>,
    pub definitions: Vec<KeyValueSpec<String>>,
    pub flags: Vec<KeyValueSpec<TuningFlagSpec>>,
    pub tuning: Vec<KeyValueSpec<TuningGroupSpec>>,
    pub packages: Vec<KeyValueSpec<PackageSpec>>,
    pub default_tuning_groups: Vec<String>,
}

/// Format-neutral build action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpec {
    pub description: String,
    pub example: Option<String>,
    pub command: String,
    pub dependencies: Vec<String>,
}

impl From<MacrosSpec> for Macros {
    fn from(spec: MacrosSpec) -> Self {
        Self {
            actions: spec.actions.into_iter().map(Into::into).collect(),
            definitions: spec.definitions.into_iter().map(Into::into).collect(),
            flags: spec.flags.into_iter().map(Into::into).collect(),
            tuning: spec.tuning.into_iter().map(Into::into).collect(),
            packages: spec.packages.into_iter().map(Into::into).collect(),
            default_tuning_groups: spec.default_tuning_groups,
        }
    }
}

impl From<ActionSpec> for Action {
    fn from(spec: ActionSpec) -> Self {
        Self {
            description: spec.description,
            example: spec.example,
            command: spec.command,
            dependencies: spec.dependencies,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::tuning::{CompilerFlag, Toolchain, TuningOptionSpec};

    #[test]
    fn format_neutral_spec_converts_to_macro_domain() {
        let macros = Macros::from(MacrosSpec {
            actions: vec![KeyValueSpec {
                key: "build".to_owned(),
                value: ActionSpec {
                    description: "Build the project".to_owned(),
                    example: None,
                    command: "make".to_owned(),
                    dependencies: vec!["binary(make)".to_owned()],
                },
            }],
            definitions: vec![KeyValueSpec {
                key: "prefix".to_owned(),
                value: "/usr".to_owned(),
            }],
            flags: vec![KeyValueSpec {
                key: "optimize".to_owned(),
                value: TuningFlagSpec {
                    root: crate::tuning::CompilerFlagsSpec {
                        c: Some("-O2".to_owned()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }],
            tuning: vec![KeyValueSpec {
                key: "release".to_owned(),
                value: TuningGroupSpec {
                    root: TuningOptionSpec {
                        enabled: vec!["optimize".to_owned()],
                        disabled: Vec::new(),
                    },
                    default: None,
                    choices: Vec::new(),
                },
            }],
            packages: vec![KeyValueSpec {
                key: "main".to_owned(),
                value: PackageSpec {
                    summary: Some("Main package".to_owned()),
                    ..Default::default()
                },
            }],
            default_tuning_groups: vec!["release".to_owned()],
        });

        assert_eq!(macros.actions[0].value.command, "make");
        assert_eq!(macros.definitions[0].value, "/usr");
        assert_eq!(macros.flags[0].value.get(CompilerFlag::C, Toolchain::Llvm), Some("-O2"));
        assert_eq!(macros.tuning[0].value.root.enabled, ["optimize"]);
        assert_eq!(macros.packages[0].value.summary.as_deref(), Some("Main package"));
        assert_eq!(macros.default_tuning_groups, ["release"]);
    }
}
