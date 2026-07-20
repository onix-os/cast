fn assert_hooks_patch_working_directory(name: &str, job: &stone_recipe::derivation::JobPlan) {
    if name != "hooks-patch" {
        return;
    }
    let setup = job
        .phases
        .iter()
        .find(|phase| phase.name == "Setup")
        .expect("hooks-patch: frozen Setup phase is missing");
    let [stone_recipe::derivation::StepPlan::Shell { working_dir, .. }] = setup.pre.as_slice() else {
        panic!("hooks-patch: frozen Setup hook has unexpected steps");
    };
    assert_eq!(working_dir, &job.work_dir);
    assert_eq!(job.work_dir, "/mason/build/x86_64/cast-hooks-fixture");
}
