use stone_recipe::{
    UpstreamSpec,
    derivation::{DerivationPlan, LockedSource, NetworkMode, StepPlan},
    package::PackageSpec,
};

use super::assert_x86_64_platform;

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "override-release");
    assert_eq!(declaration.meta.version, "2.1.0");
    assert_eq!(declaration.meta.release, 4);
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
        }] if url.ends_with("/override-release-2.1.0.tar.xz")
            && hash == "4444444444444444444444444444444444444444444444444444444444444444"
            && rename == "override-release-2.1.0.tar.xz"
            && directory == "override-release-2.1.0"
    ));
    assert!(
        declaration.sources.iter().all(|source| match source {
            UpstreamSpec::Archive { url, hash, .. } =>
                !url.contains("1.0.0") && hash != "3333333333333333333333333333333333333333333333333333333333333333",
            UpstreamSpec::Git { .. } => true,
        }),
        "the replaced release source must not retain old source identity"
    );

    assert_eq!(plan.package.version, "2.1.0");
    assert_eq!(plan.package.source_release, 4);
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive {
            url,
            sha256,
            filename,
            ..
        }] if url.ends_with("/override-release-2.1.0.tar.xz")
            && sha256 == "4444444444444444444444444444444444444444444444444444444444444444"
            && filename == "override-release-2.1.0.tar.xz"
    ));
    assert!(
        plan.provenance
            .recipe
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "package.glu"),
        "the frozen evaluation must bind the overridden base package module"
    );
    let prepare = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("prepare"))
        .expect("replacement archive lost its prepare phase");
    assert!(matches!(
        prepare.steps.as_slice(),
        [StepPlan::ExtractArchive {
            source: 0,
            destination,
            strip_components: 1,
        }] if destination == "override-release-2.1.0"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
