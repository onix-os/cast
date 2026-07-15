use std::collections::BTreeSet;

use crate::build_policy::{TuningOptionSpec, TuningPolicySpec};

use super::BuildPolicyConversionError;
use super::builder_checks::{require_string, validate_toolchain_flags};

pub(super) fn validate_tuning(tuning: &TuningPolicySpec) -> Result<(), BuildPolicyConversionError> {
    let mut flag_names = BTreeSet::new();
    for (index, flag) in tuning.flags.iter().enumerate() {
        require_string(&format!("tuning.flags[{index}].name"), &flag.name)?;
        if !flag_names.insert(flag.name.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.flags".to_owned(),
                value: flag.name.clone(),
            });
        }
        validate_toolchain_flags(&format!("tuning.flags[{index}].value"), &flag.value)?;
    }

    let mut group_names = BTreeSet::new();
    for (index, group) in tuning.groups.iter().enumerate() {
        let field = format!("tuning.groups[{index}]");
        require_string(&format!("{field}.name"), &group.name)?;
        if !group_names.insert(group.name.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.groups".to_owned(),
                value: group.name.clone(),
            });
        }
        validate_tuning_option(&format!("{field}.value.base"), &group.value.base, &flag_names)?;

        let mut choice_names = BTreeSet::new();
        for (choice_index, choice) in group.value.choices.iter().enumerate() {
            let choice_field = format!("{field}.value.choices[{choice_index}]");
            require_string(&format!("{choice_field}.name"), &choice.name)?;
            if !choice_names.insert(choice.name.as_str()) {
                return Err(BuildPolicyConversionError::Duplicate {
                    field: format!("{field}.value.choices"),
                    value: choice.name.clone(),
                });
            }
            validate_tuning_option(&format!("{choice_field}.value"), &choice.value, &flag_names)?;
        }

        if let Some(default) = &group.value.default {
            require_string(&format!("{field}.value.default"), default)?;
            if !choice_names.contains(default.as_str()) {
                return Err(BuildPolicyConversionError::InvalidDefault {
                    field: format!("{field}.value.default"),
                    value: default.clone(),
                });
            }
        }
    }

    let mut default_groups = BTreeSet::new();
    for (index, group) in tuning.default_groups.iter().enumerate() {
        let field = format!("tuning.default_groups[{index}]");
        require_string(&field, group)?;
        if !default_groups.insert(group.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: "tuning.default_groups".to_owned(),
                value: group.clone(),
            });
        }
        if !group_names.contains(group.as_str()) {
            return Err(BuildPolicyConversionError::UnknownReference {
                field,
                value: group.clone(),
            });
        }
    }

    Ok(())
}

fn validate_tuning_option(
    field: &str,
    option: &TuningOptionSpec,
    flag_names: &BTreeSet<&str>,
) -> Result<(), BuildPolicyConversionError> {
    let mut enabled = BTreeSet::new();
    for (index, flag) in option.enabled.iter().enumerate() {
        validate_tuning_flag_reference(&format!("{field}.enabled[{index}]"), flag, flag_names)?;
        if !enabled.insert(flag.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: format!("{field}.enabled"),
                value: flag.clone(),
            });
        }
    }

    let mut disabled = BTreeSet::new();
    for (index, flag) in option.disabled.iter().enumerate() {
        validate_tuning_flag_reference(&format!("{field}.disabled[{index}]"), flag, flag_names)?;
        if !disabled.insert(flag.as_str()) {
            return Err(BuildPolicyConversionError::Duplicate {
                field: format!("{field}.disabled"),
                value: flag.clone(),
            });
        }
        if enabled.contains(flag.as_str()) {
            return Err(BuildPolicyConversionError::ConflictingTuningFlag {
                field: field.to_owned(),
                value: flag.clone(),
            });
        }
    }
    Ok(())
}

fn validate_tuning_flag_reference(
    field: &str,
    flag: &str,
    flag_names: &BTreeSet<&str>,
) -> Result<(), BuildPolicyConversionError> {
    require_string(field, flag)?;
    if flag_names.contains(flag) {
        Ok(())
    } else {
        Err(BuildPolicyConversionError::UnknownReference {
            field: field.to_owned(),
            value: flag.to_owned(),
        })
    }
}
