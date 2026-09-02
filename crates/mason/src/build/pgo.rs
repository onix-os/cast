// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use stone_recipe::{ToolchainSpec, build_policy::TargetPolicySpec};

use crate::recipe::Recipe;

pub fn stages(recipe: &Recipe, target: &TargetPolicySpec) -> Option<Vec<Stage>> {
    let phases = recipe.build_target_phases(target);

    (!phases.workload.is_empty()).then(|| {
        let mut stages = vec![Stage::One];

        if matches!(recipe.declaration.options.toolchain, ToolchainSpec::Llvm) && recipe.declaration.options.cspgo {
            stages.push(Stage::Two);
        }

        stages.push(Stage::Use);

        stages
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, strum::Display)]
pub enum Stage {
    #[strum(serialize = "stage1")]
    One,
    #[strum(serialize = "stage2")]
    Two,
    #[strum(serialize = "use")]
    Use,
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::*;

    /// The ten hook slots a profile must name, all empty.
    const EMPTY_HOOKS: &str = "{ pre_setup = {}, post_setup = {}, pre_build = {}, post_build = {}, \
         pre_check = {}, post_check = {}, pre_install = {}, post_install = {}, \
         pre_workload = {}, post_workload = {} }";

    /// Every builder phase accepts hooks.
    const ALL_HOOKS: &str =
        "{ setup = true, build = true, check = true, install = true, workload = true }";

    /// The five builder phases, all empty but `workload`.
    const WORKLOAD_ONLY_PHASES: &str = r#"{
        setup = { steps = {} },
        build = { steps = {} },
        install = { steps = {} },
        check = { steps = {} },
        workload = { steps = { {
            kind = "run",
            program = { path = "/usr/bin/run-workload", requirement = { kind = "binary", value = "run-workload" } },
            args = {},
        } } },
    }"#;

    #[test]
    fn selected_profile_workload_preserves_llvm_pgo_stages() {
        let policy = crate::BuildPolicy::repository_for_tests();
        let target = policy.target("x86_64").unwrap();
        let target_name = &target.name;
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.lua"),
            format!(
                r#"return {{
    meta = {{
        pname = "example", version = "1.0.0", release = 1,
        homepage = "https://example.invalid", license = {{ "MPL-2.0" }},
    }},
    builder = {{ kind = "custom", spec = {{
        required_tools = {{}},
        environment = {{}},
        phases = {{
            setup = {{ steps = {{}} }},
            build = {{ steps = {{}} }},
            install = {{ steps = {{}} }},
            check = {{ steps = {{}} }},
            workload = {{ steps = {{}} }},
        }},
        supported_hooks = {ALL_HOOKS},
    }} }},
    hooks = {EMPTY_HOOKS},
    native_build_inputs = {{}}, build_inputs = {{}}, check_inputs = {{}},
    options = {{
        toolchain = "llvm",
        cspgo = true,
        samplepgo = false,
        debug = true,
        strip = true,
        networking = false,
        compressman = false,
        lastrip = true,
    }},
    profiles = {{ {{
        name = {target_name:?},
        builder = {{
            required_tools = {{}},
            environment = {{}},
            phases = {WORKLOAD_ONLY_PHASES},
            supported_hooks = {ALL_HOOKS},
        }},
        hooks = {EMPTY_HOOKS},
        native_build_inputs = {{}}, build_inputs = {{}}, check_inputs = {{}},
    }} }},
    sources = {{}},
    architectures = {{}},
    tuning = {{}},
    emul32 = false,
    mold = false,
}}
"#
            ),
        )
        .unwrap();
        let recipe = Recipe::load(root.path()).unwrap();

        assert_eq!(recipe.build_target_profile_key(target), Some(target_name.as_str()));
        assert_eq!(stages(&recipe, target), Some(vec![Stage::One, Stage::Two, Stage::Use]));
    }

    #[test]
    fn stage_names_are_distinct_and_stable() {
        assert_eq!(Stage::One.to_string(), "stage1");
        assert_eq!(Stage::Two.to_string(), "stage2");
        assert_eq!(Stage::Use.to_string(), "use");
    }
}
