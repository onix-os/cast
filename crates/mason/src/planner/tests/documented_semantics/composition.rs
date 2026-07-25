use stone_recipe::{
    derivation::{DerivationPlan, NetworkMode},
    package::{PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

fn assert_imports(plan: &DerivationPlan, expected: &[&str]) {
    for logical_name in expected {
        assert!(
            plan.provenance
                .recipe
                .modules
                .iter()
                .any(|module| module.logical_name == *logical_name),
            "frozen composition lost imported module {logical_name}"
        );
    }
}

pub(super) fn assert_userspace_role(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "userspace-server");
    let [root] = declaration.outputs.as_slice() else {
        panic!("userspace role must have exactly one closure output");
    };
    assert_eq!(dependency_names(&root.runtime_inputs), ["openssh", "podman", "systemd"]);
    assert!(declaration.sources.is_empty());
    assert_imports(plan, &["package.glu", "roles.glu"]);
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| !["foot", "helix", "wayland"].contains(&request.request.as_str())),
        "unselected userspace roles must not enter the frozen closure"
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

pub(super) fn assert_package_set_extension(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "observability-workstation");
    let [root] = declaration.outputs.as_slice() else {
        panic!("extended package set must have exactly one closure output");
    };
    assert_eq!(
        dependency_names(&root.runtime_inputs),
        ["bash", "coreutils", "prometheus-node-exporter", "vector"]
    );
    assert!(declaration.sources.is_empty());
    assert_imports(plan, &["package.glu", "scope.glu"]);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

pub(super) fn assert_service_family(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "relay-daemon");
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_MEMBER=daemon".to_owned()],
        }]
    );
    let [root] = declaration.outputs.as_slice() else {
        panic!("selected service-family member must have one output");
    };
    assert_eq!(
        dependency_names(&root.runtime_inputs),
        ["systemd", "soname(libssl.so.3)"]
    );
    assert_eq!(root.paths.len(), 3);
    assert_imports(plan, &["family.glu", "package.glu", "release.glu"]);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

pub(super) fn assert_variant_matrix(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "matrix-service");
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DSTORAGE=postgresql".to_owned(),
                "-DTELEMETRY=opentelemetry".to_owned(),
            ],
        }]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(libpq)", "pkgconfig(opentelemetry-c)"]
    );
    let [root] = declaration.outputs.as_slice() else {
        panic!("variant matrix package must have one output");
    };
    assert_eq!(
        dependency_names(&root.runtime_inputs),
        ["soname(libpq.so.5)", "soname(libopentelemetry.so.1)"]
    );
    assert_imports(plan, &["matrix.glu", "package.glu"]);
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| !["pkgconfig(sqlite3)", "soname(libsqlite3.so.0)"].contains(&request.request.as_str())),
        "unselected matrix cells must not enter the frozen closure"
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
