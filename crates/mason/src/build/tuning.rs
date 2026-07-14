
//! Resolve authored tuning selections against the typed repository catalog.

use std::collections::{BTreeMap, BTreeSet};

use stone_recipe::{
    NamedTuningSpec, ToolchainSpec, TuningSpec,
    build_policy::{
        CompilerFlagsSpec, NamedTuningFlagSpec, TargetPolicySpec, TextSpec, ToolchainFlagsSpec, TuningPolicySpec,
    },
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Stable descriptions of the selected group state for derivation
    /// provenance.
    pub groups: Vec<String>,
    /// Stable names of every concrete flag record contributing values.
    pub flag_names: Vec<String>,
    /// Structural compiler flags; finite context values remain unresolved until
    /// the complete build context is available.
    pub flags: CompilerFlagsSpec,
}

pub fn resolve(
    tuning: &TuningPolicySpec,
    target: &TargetPolicySpec,
    toolchain: ToolchainSpec,
    authored: &[NamedTuningSpec],
) -> Result<Selection, Error> {
    let groups = tuning
        .groups
        .iter()
        .map(|group| (group.name.as_str(), &group.value))
        .collect::<BTreeMap<_, _>>();
    let flags = tuning
        .flags
        .iter()
        .map(|flag| (flag.name.as_str(), flag))
        .collect::<BTreeMap<_, _>>();

    let authored_names = authored.iter().map(|entry| entry.key.as_str()).collect::<BTreeSet<_>>();
    let mut enabled = BTreeMap::<String, Option<String>>::new();
    let mut disabled = BTreeSet::<String>::new();

    // Architecture is a mandatory policy selection, not an authored default.
    enabled.insert("architecture".to_owned(), None);
    for group in &tuning.default_groups {
        if !authored_names.contains(group.as_str()) {
            enabled.insert(group.clone(), None);
        }
    }
    for entry in authored {
        require_group(&groups, &entry.key)?;
        match &entry.value {
            TuningSpec::Enable => {
                enabled.insert(entry.key.clone(), None);
                disabled.remove(&entry.key);
            }
            TuningSpec::Disable => {
                enabled.remove(&entry.key);
                disabled.insert(entry.key.clone());
            }
            TuningSpec::Config { value } => {
                enabled.insert(entry.key.clone(), Some(value.clone()));
                disabled.remove(&entry.key);
            }
        }
    }

    let mut selected_flags = BTreeSet::<String>::new();
    let mut selected_groups = Vec::with_capacity(enabled.len() + disabled.len());
    for (name, configured) in &enabled {
        let group = require_group(&groups, name)?;
        let selected_choice = configured.as_deref().or(group.default.as_deref());
        let option = if let Some(choice_name) = selected_choice {
            &group
                .choices
                .iter()
                .find(|choice| choice.name == choice_name)
                .ok_or_else(|| Error::InvalidChoice {
                    group: name.clone(),
                    choice: choice_name.to_owned(),
                })?
                .value
        } else {
            &group.base
        };
        selected_flags.extend(option.enabled.iter().cloned());
        selected_groups.push(match selected_choice {
            Some(choice) => format!("{name}={choice}"),
            None => format!("{name}=enabled"),
        });
    }
    for name in &disabled {
        let group = require_group(&groups, name)?;
        selected_flags.extend(group.base.disabled.iter().cloned());
        selected_groups.push(format!("{name}=disabled"));
    }
    selected_groups.sort();

    let mut resolved = CompilerFlagsSpec::default();
    for name in &selected_flags {
        let flag = flags
            .get(name.as_str())
            .ok_or_else(|| Error::MissingFlag { name: name.clone() })?;
        extend_flag(&mut resolved, flag, toolchain);
    }
    extend_toolchain_flags(&mut resolved, &target.architecture_flags, toolchain);

    Ok(Selection {
        groups: selected_groups,
        flag_names: selected_flags.into_iter().collect(),
        flags: resolved,
    })
}

fn require_group<'a>(
    groups: &BTreeMap<&str, &'a stone_recipe::build_policy::TuningGroupSpec>,
    name: &str,
) -> Result<&'a stone_recipe::build_policy::TuningGroupSpec, Error> {
    groups
        .get(name)
        .copied()
        .ok_or_else(|| Error::MissingGroup { name: name.to_owned() })
}

fn extend_flag(output: &mut CompilerFlagsSpec, flag: &NamedTuningFlagSpec, toolchain: ToolchainSpec) {
    let selected = match toolchain {
        ToolchainSpec::Llvm => &flag.value.llvm,
        ToolchainSpec::Gnu => &flag.value.gnu,
    };
    extend_with_fallback(output, &flag.value, selected);
}

pub fn extend_toolchain_flags(output: &mut CompilerFlagsSpec, flag: &ToolchainFlagsSpec, toolchain: ToolchainSpec) {
    let selected = match toolchain {
        ToolchainSpec::Llvm => &flag.llvm,
        ToolchainSpec::Gnu => &flag.gnu,
    };
    extend_with_fallback(output, flag, selected);
}

fn extend_with_fallback(output: &mut CompilerFlagsSpec, flag: &ToolchainFlagsSpec, selected: &CompilerFlagsSpec) {
    macro_rules! extend {
        ($field:ident) => {
            extend_unique(
                &mut output.$field,
                if selected.$field.is_empty() {
                    &flag.common.$field
                } else {
                    &selected.$field
                },
            );
        };
    }
    extend!(c);
    extend!(cxx);
    extend!(f);
    extend!(d);
    extend!(rust);
    extend!(vala);
    extend!(go);
    extend!(ld);
}

fn extend_unique(output: &mut Vec<TextSpec>, values: &[TextSpec]) {
    for value in values {
        if !output.contains(value) {
            output.push(value.clone());
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("unknown tuning group `{name}`")]
    MissingGroup { name: String },
    #[error("unknown choice `{choice}` for tuning group `{group}`")]
    InvalidChoice { group: String, choice: String },
    #[error("tuning policy references unknown flag `{name}`")]
    MissingFlag { name: String },
}

#[cfg(test)]
mod tests {
    use stone_recipe::build_policy::ContextValue;

    use super::*;
    use crate::BuildPolicy;

    fn selected(authored: Vec<NamedTuningSpec>, toolchain: ToolchainSpec) -> Selection {
        let policy = BuildPolicy::repository_for_tests();
        resolve(
            &policy.spec.tuning,
            policy.target("x86_64").unwrap(),
            toolchain,
            &authored,
        )
        .unwrap()
    }

    #[test]
    fn repository_defaults_and_architecture_are_selected_structurally() {
        let selected = selected(Vec::new(), ToolchainSpec::Llvm);

        assert!(selected.groups.contains(&"optimize=generic".to_owned()));
        assert!(selected.groups.contains(&"architecture=enabled".to_owned()));
        assert!(selected.flag_names.contains(&"optimize-generic".to_owned()));
        assert!(selected.flags.c.contains(&TextSpec::Literal("-O2".to_owned())));
        assert!(
            selected
                .flags
                .c
                .contains(&TextSpec::Literal("-march=x86-64-v2".to_owned()))
        );
        assert!(
            selected
                .flags
                .rust
                .contains(&TextSpec::Literal("-Ctarget-cpu=x86-64-v2".to_owned()))
        );
    }

    #[test]
    fn authored_values_replace_defaults_and_disabled_groups_select_fallback_flags() {
        let selected = selected(
            vec![
                NamedTuningSpec {
                    key: "optimize".to_owned(),
                    value: TuningSpec::Config {
                        value: "speed".to_owned(),
                    },
                },
                NamedTuningSpec {
                    key: "harden".to_owned(),
                    value: TuningSpec::Disable,
                },
                NamedTuningSpec {
                    key: "lto".to_owned(),
                    value: TuningSpec::Disable,
                },
            ],
            ToolchainSpec::Gnu,
        );

        assert!(selected.groups.contains(&"optimize=speed".to_owned()));
        assert!(selected.groups.contains(&"harden=disabled".to_owned()));
        assert!(selected.groups.contains(&"lto=disabled".to_owned()));
        assert!(selected.flag_names.contains(&"optimize-speed".to_owned()));
        assert!(selected.flag_names.contains(&"harden-none".to_owned()));
        assert!(!selected.flag_names.contains(&"lto-thin".to_owned()));
        assert!(selected.flags.c.contains(&TextSpec::Literal("-O3".to_owned())));
        assert!(
            selected
                .flags
                .c
                .contains(&TextSpec::Literal("-fno-stack-protector".to_owned()))
        );
    }

    #[test]
    fn lto_concurrency_remains_a_finite_context_value() {
        let selected = selected(
            vec![NamedTuningSpec {
                key: "lto".to_owned(),
                value: TuningSpec::Config {
                    value: "full".to_owned(),
                },
            }],
            ToolchainSpec::Gnu,
        );

        assert!(selected.flags.c.contains(&TextSpec::Concat(vec![
            TextSpec::Literal("-flto=".to_owned()),
            TextSpec::Context(ContextValue::Jobs),
        ])));
    }

    #[test]
    fn unknown_authored_groups_and_choices_are_actionable() {
        let policy = BuildPolicy::repository_for_tests();
        assert_eq!(
            resolve(
                &policy.spec.tuning,
                policy.target("x86_64").unwrap(),
                ToolchainSpec::Llvm,
                &[NamedTuningSpec {
                    key: "missing".to_owned(),
                    value: TuningSpec::Enable,
                }],
            ),
            Err(Error::MissingGroup {
                name: "missing".to_owned()
            })
        );
        assert_eq!(
            resolve(
                &policy.spec.tuning,
                policy.target("x86_64").unwrap(),
                ToolchainSpec::Llvm,
                &[NamedTuningSpec {
                    key: "optimize".to_owned(),
                    value: TuningSpec::Config {
                        value: "impossible".to_owned(),
                    },
                }],
            ),
            Err(Error::InvalidChoice {
                group: "optimize".to_owned(),
                choice: "impossible".to_owned(),
            })
        );
    }
}
