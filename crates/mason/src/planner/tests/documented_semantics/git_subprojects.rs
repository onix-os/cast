use stone_recipe::{
    UpstreamSpec,
    derivation::{DerivationPlan, LockedSource, NetworkMode, StepPlan},
    package::{PackageSpec, StepSpec},
};

use super::{EXAMPLE_GIT_COMMIT, EXAMPLE_GIT_MATERIALIZATION_SHA256, assert_x86_64_platform};

const REPOSITORIES: [(&str, &str, &str); 3] = [
    (
        "https://example.invalid/orbit-console.git",
        "1111111111111111111111111111111111111111",
        "application",
    ),
    (
        "https://example.invalid/orbit-syntax.git",
        "2222222222222222222222222222222222222222",
        "syntax-engine",
    ),
    (
        "https://example.invalid/orbit-wire.git",
        "3333333333333333333333333333333333333333",
        "wire-format",
    ),
];

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "orbit-console");
    assert_eq!(declaration.sources.len(), REPOSITORIES.len());
    for (source, (expected_url, expected_ref, expected_directory)) in declaration.sources.iter().zip(REPOSITORIES) {
        assert!(matches!(
            source,
            UpstreamSpec::Git {
                url,
                git_ref,
                clone_dir: Some(directory),
            } if url == expected_url && git_ref == expected_ref && directory == expected_directory
        ));
    }
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-Sapplication".to_owned(),
                "-DFETCHCONTENT_FULLY_DISCONNECTED=ON".to_owned(),
                "-DUSE_BUNDLED_SUBPROJECTS=ON".to_owned(),
            ],
        }]
    );
    let [mkdir, syntax, protocol] = declaration.hooks.pre_setup.as_slice() else {
        panic!("explicit Git subprojects must retain three structural layout steps");
    };
    assert!(matches!(
        mkdir,
        StepSpec::Run { program, args }
            if program.path == "/usr/bin/mkdir" && args.as_slice() == ["-p", "application/subprojects"]
    ));
    assert!(matches!(
        syntax,
        StepSpec::Run { program, args }
            if program.path == "/usr/bin/ln"
                && args.as_slice() == ["-s", "../../syntax-engine", "application/subprojects/syntax-engine"]
    ));
    assert!(matches!(
        protocol,
        StepSpec::Run { program, args }
            if program.path == "/usr/bin/ln"
                && args.as_slice() == ["-s", "../../wire-format", "application/subprojects/wire-format"]
    ));

    assert_eq!(plan.sources.len(), REPOSITORIES.len());
    for (source, (index, (expected_url, expected_ref, expected_directory))) in
        plan.sources.iter().zip(REPOSITORIES.into_iter().enumerate())
    {
        assert!(matches!(
            source,
            LockedSource::Git {
                order,
                url,
                requested_ref,
                commit,
                materialization_sha256,
                directory,
            } if *order == u32::try_from(index).unwrap()
                && url == expected_url
                && requested_ref == expected_ref
                && commit == EXAMPLE_GIT_COMMIT
                && materialization_sha256 == EXAMPLE_GIT_MATERIALIZATION_SHA256
                && directory == expected_directory
        ));
    }
    for module in ["package.glu", "repositories.glu"] {
        assert!(
            plan.provenance
                .recipe
                .imported_modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "frozen explicit subproject graph lost imported module {module}"
        );
    }
    let setup = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("setup"))
        .expect("explicit Git subprojects lost their setup phase");
    assert!(matches!(
        setup.pre.as_slice(),
        [
            StepPlan::Run { program: mkdir, .. },
            StepPlan::Run { program: syntax, .. },
            StepPlan::Run { program: protocol, .. },
        ] if mkdir.requirement.canonical_name() == "binary(mkdir)"
            && syntax.requirement.canonical_name() == "binary(ln)"
            && protocol.requirement.canonical_name() == "binary(ln)"
    ));
    assert!(
        plan.jobs
            .iter()
            .flat_map(|job| &job.phases)
            .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
            .all(|step| !matches!(step, StepPlan::ExtractArchive { .. })),
        "independent Git trees must not gain archive extraction steps"
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
