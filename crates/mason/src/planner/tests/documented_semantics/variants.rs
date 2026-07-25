use stone_recipe::{
    derivation::{DerivationPlan, InputOrigin, LockedSource, NetworkMode, OutputRelation, PackageInputSelection},
    package::{PackageSpec, StepSpec},
};

use super::{assert_locked_request_origin, assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "typed-backend-client");
    assert_eq!(dependency_names(&declaration.build_inputs), ["pkgconfig(libressl)"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DTLS_BACKEND=libressl".to_owned()],
        }]
    );
    let [output] = declaration.outputs.as_slice() else {
        panic!("typed backend package must publish one output");
    };
    assert_eq!(dependency_names(&output.runtime_inputs), ["soname(libtls.so.28)"]);

    assert!(
        plan.provenance
            .recipe
            .modules
            .iter()
            .any(|module| module.logical_name == "package.glu"),
        "the frozen evaluation provenance must bind the typed factory module"
    );
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive { filename, .. }]
            if filename == "typed-backend-client-4.0.0.tar.xz"
    ));
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(sh)", "binary(ninja)", "pkgconfig(libressl)"]
    );
    assert_locked_request_origin(
        plan,
        "pkgconfig(libressl)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 0,
        },
    );
    assert_locked_request_origin(
        plan,
        "soname(libtls.so.28)",
        InputOrigin::OutputRuntime {
            output: "out".to_owned(),
            index: 0,
        },
    );
    assert!(
        plan.build_lock.requests.iter().all(|request| {
            ![
                "pkgconfig(openssl)",
                "pkgconfig(rustls-ffi)",
                "soname(libssl.so.3)",
                "soname(librustls_ffi.so.0)",
            ]
            .contains(&request.request.as_str())
        }),
        "unselected backend capabilities must not enter the frozen closure"
    );
    let frozen_output = plan.outputs.iter().find(|output| output.name == "out").unwrap();
    assert!(matches!(
        frozen_output.runtime_inputs.as_slice(),
        [OutputRelation::Locked { relation, .. }]
            if relation.canonical_name() == "soname(libtls.so.28)"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
