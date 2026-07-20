use stone_recipe::{
    UpstreamSpec,
    derivation::{DerivationPlan, InputOrigin, LockedSource, NetworkMode, PackageInputSelection},
    package::{PackageSpec, StepSpec},
};

use super::{assert_locked_request_origin, assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "scoped-client");
    assert!(declaration.native_build_inputs.is_empty());
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_SERVER=OFF".to_owned()],
        }]
    );
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive { url, .. }] if url.ends_with("/scoped-client-1.2.0.tar.xz")
    ));

    for module in ["packages.glu", "scope.glu"] {
        assert!(
            plan.provenance
                .recipe
                .imported_modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "frozen package scope lost imported module {module}"
        );
    }
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive { filename, .. }] if filename == "scoped-client-1.2.0.tar.xz"
    ));
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(sh)",
            "binary(ninja)",
            "pkgconfig(zlib)",
            "pkgconfig(libressl)",
        ]
    );
    for (request, index) in [("pkgconfig(zlib)", 0), ("pkgconfig(libressl)", 1)] {
        assert_locked_request_origin(
            plan,
            request,
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index,
            },
        );
    }
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| { !["binary(doxygen)", "pkgconfig(sqlite3)"].contains(&request.request.as_str()) }),
        "unselected server-scope dependencies must not enter the client closure"
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
