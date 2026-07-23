use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs_err as fs;

use super::*;
use stone_recipe::build_policy::AnalyzerKind;
use stone_recipe::derivation::{
    AnalysisToolsPlan, CollectionRulePlan, ExecutablePlan, OutputPlan, PathRuleKind, RelationKind, RelationPlan,
};

fn frozen_analyzer_tool(name: &str) -> ExecutablePlan {
    ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: name.to_owned(),
        },
    }
}

include!("factory_resolution.rs");

include!("execution_lock_validation.rs");

fn publication_fixture() -> (tempfile::TempDir, DerivationPlan, Paths) {
    let root = crate::private_tempdir();
    let output = root.path().join("output");
    fs::create_dir(&output).unwrap();
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let plan = test_derivation_plan();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap();
    paths.bind_to_plan(&plan).unwrap();
    fs::set_permissions(paths.output_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let staged_anchor = paths.prepare_private_host_directory(&paths.artefacts().host).unwrap();
    assert_eq!(staged_anchor.metadata().unwrap().permissions().mode() & 0o7777, 0o700);
    (root, plan, paths)
}

fn publish_artefacts(paths: &Paths, plan: &DerivationPlan) -> Result<Publication, PublishError> {
    let execution_lock = paths
        .acquire_execution_lock(plan)
        .map_err(PublishError::InvalidExecutionLock)?;
    let staged_anchor = paths
        .prepare_private_host_directory(&paths.artefacts().host)
        .map_err(PublishError::InvalidFrozenPaths)?;
    super::publish_artefacts(paths, plan, &execution_lock, &staged_anchor, ManifestVerification::None)
}

fn publish_artefacts_with<F>(
    paths: &Paths,
    plan: &DerivationPlan,
    limits: PublishLimits,
    hook: F,
) -> Result<Publication, PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    let execution_lock = paths
        .acquire_execution_lock(plan)
        .map_err(PublishError::InvalidExecutionLock)?;
    super::publish_artefacts_with(paths, plan, &execution_lock, ManifestVerification::None, limits, hook)
}

fn publish_artefacts_verifying(
    paths: &Paths,
    plan: &DerivationPlan,
    expected: &Path,
) -> Result<Publication, PublishError> {
    let execution_lock = paths
        .acquire_execution_lock(plan)
        .map_err(PublishError::InvalidExecutionLock)?;
    let staged_anchor = paths
        .prepare_private_host_directory(&paths.artefacts().host)
        .map_err(PublishError::InvalidFrozenPaths)?;
    super::publish_artefacts(
        paths,
        plan,
        &execution_lock,
        &staged_anchor,
        ManifestVerification::ExactBinary(expected),
    )
}

fn publish_artefacts_verifying_with<F>(
    paths: &Paths,
    plan: &DerivationPlan,
    expected: &Path,
    limits: PublishLimits,
    hook: F,
) -> Result<Publication, PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    let execution_lock = paths
        .acquire_execution_lock(plan)
        .map_err(PublishError::InvalidExecutionLock)?;
    super::publish_artefacts_with(
        paths,
        plan,
        &execution_lock,
        ManifestVerification::ExactBinary(expected),
        limits,
        hook,
    )
}

fn output_entries(paths: &Paths) -> Vec<OsString> {
    let mut entries = fs::read_dir(paths.output_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn stage_expected_bundle(plan: &DerivationPlan, paths: &Paths) -> Vec<OsString> {
    let names = expected_bundle_files(plan).into_iter().collect::<Vec<_>>();
    for name in &names {
        let path = paths.artefacts().host.join(name);
        fs::write(&path, b"frozen artefact bytes").unwrap();
        fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
    }
    names
}

fn stone_name(names: &[OsString]) -> &OsString {
    names
        .iter()
        .find(|name| name.to_string_lossy().ends_with(".stone"))
        .unwrap()
}

fn binary_manifest_name(names: &[OsString]) -> &OsString {
    names
        .iter()
        .find(|name| name.to_string_lossy().ends_with(".bin"))
        .unwrap()
}

fn reference_path(root: &Path, label: &str) -> PathBuf {
    let parent = root.join(label);
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    parent.join("expected.bin")
}

fn reference_manifest(root: &Path, bytes: &[u8]) -> PathBuf {
    let path = reference_path(root, "verification-reference");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    path
}

fn seal_test_bundle_directory(path: &Path, plan: &DerivationPlan) {
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
    fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_BUNDLE_MODE)).unwrap();
}

include!("publication_contract.rs");

fn create_competing_bundle(plan: &DerivationPlan, paths: &Paths, mismatched: bool) -> Vec<OsString> {
    let names = expected_bundle_files(plan).into_iter().collect::<Vec<_>>();
    let bundle = paths.output_dir().join(plan.derivation_id().as_str());
    fs::create_dir(&bundle).unwrap();
    for (index, name) in names.iter().enumerate() {
        let source = paths.artefacts().host.join(name);
        let target = bundle.join(name);
        let mut bytes = fs::read(source).unwrap();
        if mismatched && index == 0 {
            bytes.fill(b'X');
        }
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        filetime::set_file_mtime(&target, filetime::FileTime::from_unix_time(plan.source_date_epoch, 0)).unwrap();
    }
    seal_test_bundle_directory(&bundle, plan);
    names
}

include!("publication_integrity.rs");
