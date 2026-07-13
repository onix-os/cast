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

    #[test]
    fn selected_profile_workload_preserves_llvm_pgo_stages() {
        let policy = crate::BuildPolicy::repository_for_tests();
        let target = policy.target("x86_64").unwrap();
        let target_name = &target.name;
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            format!(
                r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {{
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.invalid", license = ["MPL-2.0"],
}})
let scripts = cast.scripts {{
    workload = cast.phase [cast.step.run (cast.program.binary "run-workload") []],
    .. cast.defaults.scripts
}}
let profile = cast.profile_with {{
    name = {target_name:?},
    builder = cast.builder.shell scripts [],
    hooks = cast.defaults.hooks,
    native_build_inputs = [], build_inputs = [], check_inputs = [],
}}
{{
    options = cast.options {{
        cspgo = cast.boolean.true,
        .. cast.defaults.options
    }},
    profiles = [profile],
    .. base
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
