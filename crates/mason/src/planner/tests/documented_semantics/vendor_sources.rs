use std::path::Path;

use fs_err as fs;
use stone_recipe::{
    UpstreamSpec,
    derivation::{DerivationPlan, LockedSource, NetworkMode, StepPlan},
    package::{PackageSpec, StepSpec},
};

use super::{
    PackageExample, PackageExampleMatrix, WriteOutcome, assert_x86_64_platform, copy_package_directory,
    dependency_names, plan_for_build, synthesize_source_lock,
};

const APPLICATION_URL: &str = "https://example.invalid/vendor-note-3.2.1.tar.zst";
const APPLICATION_DIGEST: &str = "111122223333444455556666777788889999aaaabbbbccccddddeeeeffff0000";
const APPLICATION_ARCHIVE: &str = "vendor-note-3.2.1.tar.zst";
const VENDOR_URL: &str = "https://example.invalid/vendor-note-cargo-vendor-2026-07-21.tar.zst";
const VENDOR_DIGEST: &str = "0000ffffeeeeddddccccbbbbaaaa999988887777666655554444333322221111";
const VENDOR_ARCHIVE: &str = "vendor-note-cargo-vendor-2026-07-21.tar.zst";

const SETUP_SCRIPT: &str = r#"test -f "${CAST_SOURCE_DIR}/application/Cargo.toml"
test -f "${CAST_SOURCE_DIR}/application/Cargo.lock"
test -d "${CAST_SOURCE_DIR}/vendor"
test ! -e "${CAST_SOURCE_DIR}/application/vendor""#;

const BUILD_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export CARGO_HOME="${CAST_BUILD_ROOT}/cargo-home"
export CARGO_NET_OFFLINE=true
export CARGO_INCREMENTAL=0
cargo build \
    --manifest-path "${CAST_SOURCE_DIR}/application/Cargo.toml" \
    --target-dir "${CAST_BUILDER_DIR}/target" \
    --release --frozen --offline \
    --config 'net.offline=true' \
    --config 'source.crates-io.replace-with="declared-vendor"' \
    --config "source.declared-vendor.directory='${CAST_SOURCE_DIR}/vendor'""#;

const CHECK_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export CARGO_HOME="${CAST_BUILD_ROOT}/cargo-home"
export CARGO_NET_OFFLINE=true
export CARGO_INCREMENTAL=0
cargo test \
    --manifest-path "${CAST_SOURCE_DIR}/application/Cargo.toml" \
    --target-dir "${CAST_BUILDER_DIR}/target" \
    --frozen --offline \
    --config 'net.offline=true' \
    --config 'source.crates-io.replace-with="declared-vendor"' \
    --config "source.declared-vendor.directory='${CAST_SOURCE_DIR}/vendor'""#;

const INSTALL_SCRIPT: &str = r#"install -Dm755 "${CAST_BUILDER_DIR}/target/release/vendor-note" "${CAST_INSTALL_ROOT}${CAST_BINDIR}/vendor-note"
install -Dm644 "${CAST_SOURCE_DIR}/application/README.md" "${CAST_INSTALL_ROOT}${CAST_DATADIR}/doc/vendor-note/README.md""#;

pub(super) fn assert_semantics(
    declaration: &PackageSpec,
    plan: &DerivationPlan,
    matrix: &PackageExampleMatrix,
) {
    assert_eq!(declaration.meta.pname, "vendor-note");
    assert_eq!(declaration.meta.version, "3.2.1");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(cargo)", "binary(install)"]
    );
    assert!(declaration.native_build_inputs.is_empty());
    assert!(declaration.build_inputs.is_empty());
    assert!(declaration.check_inputs.is_empty());
    assert!(!declaration.options.networking);

    let [application, vendor] = declaration.sources.as_slice() else {
        panic!("independent-vendor-source must retain exactly two ordered archive locks");
    };
    assert_archive(
        application,
        APPLICATION_URL,
        APPLICATION_DIGEST,
        APPLICATION_ARCHIVE,
        "application",
    );
    assert_archive(vendor, VENDOR_URL, VENDOR_DIGEST, VENDOR_ARCHIVE, "vendor");
    assert_ne!(APPLICATION_URL, VENDOR_URL);
    assert_ne!(APPLICATION_DIGEST, VENDOR_DIGEST);
    assert_ne!(APPLICATION_ARCHIVE, VENDOR_ARCHIVE);

    let [setup] = declaration.builder.phases.setup.steps.as_slice() else {
        panic!("independent-vendor-source must retain one source-layout check");
    };
    assert_shell(setup, &[], SETUP_SCRIPT);
    let [build] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("independent-vendor-source must retain one offline Cargo build");
    };
    assert_shell(build, &["/usr/bin/cargo"], BUILD_SCRIPT);
    let [check] = declaration.builder.phases.check.steps.as_slice() else {
        panic!("independent-vendor-source must retain one offline Cargo check");
    };
    assert_shell(check, &["/usr/bin/cargo"], CHECK_SCRIPT);
    let [install] = declaration.builder.phases.install.steps.as_slice() else {
        panic!("independent-vendor-source must retain one explicit install step");
    };
    assert_shell(install, &["/usr/bin/install"], INSTALL_SCRIPT);
    assert_offline_vendor_contract(BUILD_SCRIPT);
    assert_offline_vendor_contract(CHECK_SCRIPT);

    assert!(matches!(
        plan.sources.as_slice(),
        [
            LockedSource::Archive {
                order: 0,
                url,
                sha256,
                filename,
            },
            LockedSource::Archive {
                order: 1,
                url: vendor_url,
                sha256: vendor_sha256,
                filename: vendor_filename,
            },
        ] if url == APPLICATION_URL
            && sha256 == APPLICATION_DIGEST
            && filename == APPLICATION_ARCHIVE
            && vendor_url == VENDOR_URL
            && vendor_sha256 == VENDOR_DIGEST
            && vendor_filename == VENDOR_ARCHIVE
    ));
    let prepare = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("prepare"))
        .expect("independent vendor archives lost their prepare phase");
    assert!(matches!(
        prepare.steps.as_slice(),
        [
            StepPlan::ExtractArchive {
                source: 0,
                destination,
                strip_components: 1,
            },
            StepPlan::ExtractArchive {
                source: 1,
                destination: vendor_destination,
                strip_components: 1,
            },
        ] if destination == "application" && vendor_destination == "vendor"
    ));
    for module in ["package.glu", "sources.glu"] {
        assert!(
            plan.provenance
                .recipe
                .modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "independent vendor plan lost imported module {module}"
        );
    }
    for (phase_name, expected_script) in [("build", BUILD_SCRIPT), ("check", CHECK_SCRIPT)] {
        let phase = plan
            .jobs
            .iter()
            .flat_map(|job| &job.phases)
            .find(|phase| phase.name.eq_ignore_ascii_case(phase_name))
            .unwrap_or_else(|| panic!("independent vendor plan lost its {phase_name} phase"));
        assert!(matches!(
            phase.steps.as_slice(),
            [StepPlan::Shell {
                interpreter,
                declared_programs,
                script,
                ..
            }] if interpreter.path == "/usr/bin/bash"
                && matches!(declared_programs.as_slice(), [program] if program.path == "/usr/bin/cargo")
                && script == expected_script
        ));
    }

    assert_independent_identity_variants(declaration, plan, matrix);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_archive(source: &UpstreamSpec, url: &str, digest: &str, filename: &str, destination: &str) {
    assert!(matches!(
        source,
        UpstreamSpec::Archive {
            url: actual_url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(unpack_dir),
        } if actual_url == url && hash == digest && rename == filename && unpack_dir == destination
    ));
}

fn assert_shell(step: &StepSpec, programs: &[&str], expected_script: &str) {
    let StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    } = step
    else {
        panic!("independent vendor phase must remain one typed shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        programs
    );
    assert_eq!(script, expected_script);
}

fn assert_offline_vendor_contract(script: &str) {
    for required in [
        "HOME=\"${CAST_BUILD_ROOT}/home\"",
        "CARGO_HOME=\"${CAST_BUILD_ROOT}/cargo-home\"",
        "CARGO_NET_OFFLINE=true",
        "--frozen --offline",
        "--config 'net.offline=true'",
        "source.crates-io.replace-with=\"declared-vendor\"",
        "source.declared-vendor.directory='${CAST_SOURCE_DIR}/vendor'",
    ] {
        assert!(script.contains(required), "offline vendor build lost {required}");
    }
    for forbidden in ["cargo fetch", "cargo vendor", "git clone", "http://", "https://"] {
        assert!(!script.contains(forbidden), "offline vendor build gained {forbidden}");
    }
}

fn assert_independent_identity_variants(
    declaration: &PackageSpec,
    plan: &DerivationPlan,
    matrix: &PackageExampleMatrix,
) {
    let (application_changed, application_plan) =
        freeze_variant(matrix, "application-identity", APPLICATION_VARIANT);
    let (vendor_changed, vendor_plan) = freeze_variant(matrix, "vendor-identity", VENDOR_VARIANT);

    assert_ne!(application_changed.sources[0], declaration.sources[0]);
    assert_eq!(application_changed.sources[1], declaration.sources[1]);
    assert_eq!(vendor_changed.sources[0], declaration.sources[0]);
    assert_ne!(vendor_changed.sources[1], declaration.sources[1]);

    assert_ne!(application_plan.sources[0], plan.sources[0]);
    assert_eq!(application_plan.sources[1], plan.sources[1]);
    assert_eq!(vendor_plan.sources[0], plan.sources[0]);
    assert_ne!(vendor_plan.sources[1], plan.sources[1]);
    assert_ne!(application_plan.derivation_id(), plan.derivation_id());
    assert_ne!(vendor_plan.derivation_id(), plan.derivation_id());
    assert_ne!(application_plan.derivation_id(), vendor_plan.derivation_id());
    assert_eq!(application_plan.jobs, plan.jobs);
    assert_eq!(vendor_plan.jobs, plan.jobs);
    assert_eq!(application_plan.execution, plan.execution);
    assert_eq!(vendor_plan.execution, plan.execution);

    for variant in [&application_changed, &vendor_changed] {
        let mut without_sources = variant.clone();
        without_sources.sources.clear();
        let mut baseline_without_sources = declaration.clone();
        baseline_without_sources.sources.clear();
        assert_eq!(
            without_sources, baseline_without_sources,
            "changing one source identity must not change package semantics outside the source graph"
        );
    }
}

fn freeze_variant(
    matrix: &PackageExampleMatrix,
    name: &str,
    recipe_source: &str,
) -> (PackageSpec, DerivationPlan) {
    let authored = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/gluon/packages/independent-vendor-source");
    let recipe_dir = matrix
        ._root
        .path()
        .join("independent-vendor-identity-variants")
        .join(name);
    copy_package_directory(&authored, &recipe_dir);
    let recipe_path = recipe_dir.join("stone.glu");
    fs::write(&recipe_path, recipe_source).expect("write independent vendor identity variant");
    let (source_lock_bytes, source_count) = synthesize_source_lock(&recipe_path);
    assert_eq!(source_count, 2);
    assert!(source_lock_bytes.is_some());
    let example = PackageExample {
        name: format!("independent-vendor-source-{name}"),
        recipe_path,
        source_lock_bytes,
        source_count,
    };
    let evaluated = matrix.builder(&example);
    let planned = plan_for_build(matrix.env(), matrix.request(&example, true), &matrix.output_dir)
        .unwrap_or_else(|error| panic!("freeze independent vendor {name} variant: {error:#}"));
    planned
        .plan
        .validate()
        .unwrap_or_else(|error| panic!("validate independent vendor {name} variant: {error:#}"));
    assert_eq!(planned.lock_outcome, Some(WriteOutcome::Written));
    assert_eq!(planned.plan.provenance.recipe, evaluated.recipe.fingerprint);
    (evaluated.recipe.declaration.clone(), planned.plan)
}

const APPLICATION_VARIANT: &str = r#"let b = import! cast.package.v3
let make_package = import! "./package.glu"
let sources = import! "./sources.glu"

make_package {
    application = {
        url = "https://example.invalid/vendor-note-next.tar.zst",
        digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        unpack_dir = "application",
        lock = b.source.archive_with {
            url = "https://example.invalid/vendor-note-next.tar.zst",
            hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            rename = b.optional.set "vendor-note-next.tar.zst",
            strip_dirs = b.optional.set 1,
            unpack = b.boolean.true,
            unpack_dir = b.optional.set "application",
        },
    },
    .. sources
}
"#;

const VENDOR_VARIANT: &str = r#"let b = import! cast.package.v3
let make_package = import! "./package.glu"
let sources = import! "./sources.glu"

make_package {
    vendor = {
        url = "https://example.invalid/vendor-note-cargo-vendor-next.tar.zst",
        digest = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        unpack_dir = "vendor",
        lock = b.source.archive_with {
            url = "https://example.invalid/vendor-note-cargo-vendor-next.tar.zst",
            hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            rename = b.optional.set "vendor-note-cargo-vendor-next.tar.zst",
            strip_dirs = b.optional.set 1,
            unpack = b.boolean.true,
            unpack_dir = b.optional.set "vendor",
        },
    },
    .. sources
}
"#;
