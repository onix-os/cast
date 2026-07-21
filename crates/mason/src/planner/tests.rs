#![cfg_attr(
    all(feature = "delegated-fixture-test-support", not(test)),
    allow(dead_code, unused_imports)
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    io::Read,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use forge::{
    Provider,
    package::{Meta, Name},
};
use fs_err as fs;
use sha2::{Digest, Sha256};
use stone::{StoneHeaderV1FileType, StoneWriter, relation::Kind as RelationKind};
use stone_recipe::{
    TuningSpec, UpstreamSpec,
    derivation::{
        DerivationPlan, FilesystemPolicy, InputOrigin, NetworkMode, OutputRelation, PackageInputSelection,
        encode_build_lock,
    },
    package::{DependencySpec, PackageSpec, StepSpec},
};
use tempfile::TempDir;
use url::Url;

use super::{Planned, Request, plan, plan_for_build};
use crate::{
    Env,
    build::{self, Builder, BuilderRequest},
    build_lock::WriteOutcome,
    package::{Packager, Publication},
    profile,
    source_lock::{
        ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, decode_source_lock,
        encode_source_lock, write_source_lock,
    },
};

const PROFILE: &str = "planner-hermetic";
const ALTERNATE_PROFILE: &str = "planner-hermetic-alternate";
const TARGET: &str = "x86_64";
const SOURCE_DATE_EPOCH: i64 = 1_700_000_000;
const RUNTIME_REQUEST: &str = "binary(planner-runtime)";
const EXAMPLE_PROFILE: &str = "planner-example-matrix";
const EXAMPLE_GIT_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const EXAMPLE_GIT_MATERIALIZATION_SHA256: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const PACKAGE_EXAMPLES: [&str; 59] = [
    "autotools",
    "backend-choice-factory",
    "binary-release",
    "cargo",
    "cmake",
    "conditionals",
    "conflicts",
    "custom-steps",
    "dependency-roles",
    "desktop-application",
    "explicit-git-subprojects",
    "explicit-package-scope",
    "explicit-package-set-extension",
    "external-patch-source",
    "external-test-vectors",
    "factory-override",
    "firmware-bundle",
    "font-family",
    "generated-schema-library",
    "gettext-catalogs",
    "go-module",
    "header-only-library",
    "hooks",
    "kernel-module-factory",
    "layered-overrides",
    "locked-template-substitution",
    "manual-compiler-pipeline",
    "maven-application",
    "meson",
    "meta-package",
    "minimal",
    "multiple-sources",
    "native-codegen-target-library",
    "nodejs-vendored-application",
    "optional-component-source-graph",
    "options-tuning",
    "output-policy-factory",
    "output-tool-wrapper",
    "patch-series",
    "pgo-workload",
    "platform-binary-factory",
    "platform-factory",
    "post-install-smoke-test",
    "profiles-emul32",
    "python-module",
    "raw-script-package",
    "realistic-daemon",
    "release-override",
    "release-source-factory",
    "service-family-factory",
    "shared-capability-origins",
    "source-less-generated-config",
    "split-outputs",
    "system-integration-assets",
    "target-profile-specialization",
    "typed-output-routing",
    "userspace-role-factory",
    "variant-matrix-factory",
    "zig-project",
];

fn write_repository_policy_fixture(data_dir: &Path) {
    let policy_dir = data_dir.join("policy");
    fs::create_dir_all(policy_dir.join("tuning")).unwrap();
    fs::write(
        policy_dir.join("policy.glu"),
        include_str!("../../data/policy/policy.glu"),
    )
    .unwrap();
    fs::write(
        policy_dir.join("default.glu"),
        include_str!("../../data/policy/default.glu"),
    )
    .unwrap();
    fs::write(
        policy_dir.join("tuning/flags.glu"),
        include_str!("../../data/policy/tuning/flags.glu"),
    )
    .unwrap();
    fs::write(
        policy_dir.join("tuning/groups.glu"),
        include_str!("../../data/policy/tuning/groups.glu"),
    )
    .unwrap();
}
const EXECUTION_FIXTURES: [&str; 28] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "desktop-integration",
    "external-test-vectors",
    "factory-override",
    "font-family",
    "generated-config",
    "generated-shell",
    "gettext-localization",
    "go-module",
    "header-only-library",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "pgo-workload",
    "plugin-output",
    "post-install-smoke-test",
    "python-module",
    "relation-policy",
    "split",
    "system-integration-assets",
    "userspace-profile",
];

const EXECUTION_PACKAGE_DIRECTORIES: [&str; 27] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "desktop-integration",
    "external-test-vectors",
    "factory-override",
    "font-family",
    "generated-config",
    "generated-shell",
    "gettext-localization",
    "go-module",
    "header-only-library",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "pgo-workload",
    "plugin-output",
    "post-install-smoke-test",
    "python-module",
    "relation-policy",
    "split",
    "system-integration-assets",
];

fn execution_fixture_package_directory(name: &str) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon");
    if name == "userspace-profile" {
        fixtures.join(name)
    } else {
        fixtures.join("execution/packages").join(name)
    }
}

#[path = "tests/bootstrap.rs"]
mod bootstrap;
#[cfg(feature = "delegated-fixture-test-support")]
pub(crate) use bootstrap::DelegatedExecutionOutcome;
#[path = "tests/execution_session.rs"]
mod execution_session;
use execution_session::execute_and_publish;
#[path = "tests/execution_cleanup_witness_tests.rs"]
mod execution_cleanup_witness_tests;
#[path = "tests/documented_semantics/code_generation.rs"]
mod documented_code_generation;
#[path = "tests/documented_semantics/composition.rs"]
mod documented_composition;
#[path = "tests/documented_semantics/dependencies.rs"]
mod documented_dependencies;
#[path = "tests/documented_semantics/generated.rs"]
mod documented_generated;
#[path = "tests/documented_semantics/git_subprojects.rs"]
mod documented_git_subprojects;
#[path = "tests/documented_semantics/outputs.rs"]
mod documented_outputs;
#[path = "tests/documented_semantics/overrides.rs"]
mod documented_overrides;
#[path = "tests/documented_semantics/profiles.rs"]
mod documented_profiles;
#[path = "tests/documented_semantics/scopes.rs"]
mod documented_scopes;
#[path = "tests/documented_semantics.rs"]
mod documented_semantics;
#[path = "tests/documented_semantics/sources.rs"]
mod documented_sources;
#[path = "tests/documented_semantics/variants.rs"]
mod documented_variants;
include!("tests/execution_archives.rs");
include!("tests/execution_autotools_regeneration.rs");
include!("tests/execution_capability.rs");
include!("tests/execution_cmake_zlib.rs");
include!("tests/execution_desktop_integration.rs");
include!("tests/execution_external_patch.rs");
include!("tests/execution_external_test_vectors.rs");
include!("tests/execution_font_family.rs");
include!("tests/execution_gettext_localization.rs");
include!("tests/execution_go_module.rs");
include!("tests/execution_header_only_library.rs");
include!("tests/execution_meson_dependency_roles.rs");
include!("tests/execution_multiple_sources.rs");
include!("tests/execution_post_install_smoke.rs");
include!("tests/execution_pgo_workload.rs");
include!("tests/execution_python_module.rs");
include!("tests/execution_relation_policy.rs");
include!("tests/execution_system_integration_assets.rs");
include!("tests/frozen_runtime.rs");
include!("tests/package_examples.rs");
include!("tests/planning_identity.rs");

#[cfg(feature = "delegated-fixture-test-support")]
pub(super) fn run_delegated_execution_fixture() -> DelegatedExecutionOutcome {
    bootstrap::run_delegated_execution_fixture()
}

const RECIPE: &str = r#"let b = import! cast.package.v3

let scripts = b.scripts {
    build = b.phase [b.step.shell "printf planner-hermetic > build.log"],
    .. b.defaults.scripts
}

let root = {
    summary = b.optional.set "Hermetic planner fixture",
    description = b.optional.set "Hermetic planner fixture",
    runtime_inputs = [b.dep.binary "planner-runtime"],
    .. b.output "out"
}

{
    builder = b.builder.shell scripts [],
    outputs = b.outputs.with_root "planner-hermetic" root,
    .. b.mk_package (b.meta {
        pname = "planner-hermetic",
        version = "1.0.0",
        release = 1,
        homepage = "https://example.invalid/planner-hermetic",
        license = ["MPL-2.0"],
    })
}
"#;

struct Fixture {
    _root: TempDir,
    cache_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    forge_dir: PathBuf,
    output_dir: PathBuf,
    recipe_path: PathBuf,
    repository_index: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = crate::private_tempdir();
        let cache_dir = root.path().join("cache");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let forge_dir = root.path().join("forge");
        let output_dir = root.path().join("output");
        let recipe_dir = root.path().join("recipe");
        let repository_dir = root.path().join("repository");
        let recipe_path = recipe_dir.join("stone.glu");
        let repository_index = repository_dir.join("stone.index");

        write_repository_policy_fixture(&data_dir);
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipe_dir).unwrap();
        fs::create_dir_all(&repository_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();

        fs::write(&recipe_path, RECIPE).unwrap();

        let index_uri = Url::from_file_path(&repository_index).unwrap();
        fs::write(
            config_dir.join("profile.d/planner-hermetic.glu"),
            format!(
                r#"let cast = import! cast.profile.v1

cast.profiles [
    cast.profile "{PROFILE}" [
        cast.repository.direct "fixture" "{index_uri}",
    ],
    cast.profile "{ALTERNATE_PROFILE}" [
        cast.repository.direct "fixture" "{index_uri}",
    ],
]
"#,
            ),
        )
        .unwrap();

        let fixture = Self {
            _root: root,
            cache_dir,
            config_dir,
            data_dir,
            forge_dir,
            output_dir,
            recipe_path,
            repository_index,
        };
        let requested = fixture.requested_packages();
        write_repository_index(&fixture.repository_index, &requested);
        fixture
    }

    fn env(&self) -> Env {
        Env::new(
            Some(self.cache_dir.clone()),
            Some(self.config_dir.clone()),
            Some(self.data_dir.clone()),
            Some(self.forge_dir.clone()),
        )
        .unwrap()
    }

    fn request(&self) -> Request {
        Request {
            recipe: self.recipe_path.clone(),
            profile: profile::Id::new(PROFILE),
            target: TARGET.to_owned(),
            source_date_epoch: SOURCE_DATE_EPOCH,
            build_release: NonZeroU64::new(1).unwrap(),
            jobs: NonZeroU32::new(1).unwrap(),
            compiler_cache: false,
            update_lock: true,
            refresh_repositories: true,
        }
    }

    fn builder(&self) -> Builder {
        Builder::new(BuilderRequest {
            recipe_path: self.recipe_path.clone(),
            env: self.env(),
            profile: profile::Id::new(PROFILE),
            compiler_cache: false,
            output_dir: self.output_dir.clone(),
            jobs: NonZeroUsize::new(1).unwrap(),
            source_date_epoch: Some(SOURCE_DATE_EPOCH),
            requested_target: TARGET.to_owned(),
        })
        .unwrap()
    }

    fn requested_packages(&self) -> Vec<String> {
        let builder = self.builder();
        let mut requested = build::root::inputs(&builder)
            .unwrap()
            .into_iter()
            .map(|input| input.request)
            .collect::<Vec<_>>();
        requested.push(RUNTIME_REQUEST.to_owned());
        requested.sort();
        requested.dedup();
        requested
    }
}

struct PackageExample {
    name: String,
    recipe_path: PathBuf,
    source_lock_bytes: Option<Vec<u8>>,
    source_count: usize,
}

struct PackageExampleMatrix {
    _root: TempDir,
    cache_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    forge_dir: PathBuf,
    output_dir: PathBuf,
    repository_index: PathBuf,
    examples: Vec<PackageExample>,
}

impl PackageExampleMatrix {
    fn new() -> Self {
        let root = crate::private_tempdir();
        let cache_dir = root.path().join("cache");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let forge_dir = root.path().join("forge");
        let output_dir = root.path().join("output");
        let recipes_dir = root.path().join("recipes");
        let repository_dir = root.path().join("repository");
        let repository_index = repository_dir.join("stone.index");

        write_repository_policy_fixture(&data_dir);
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipes_dir).unwrap();
        fs::create_dir_all(&repository_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();
        let index_uri = Url::from_file_path(&repository_index).unwrap();
        fs::write(
            config_dir.join("profile.d/planner-example-matrix.glu"),
            format!(
                r#"let cast = import! cast.profile.v1

cast.profiles [
    cast.profile "{EXAMPLE_PROFILE}" [
        cast.repository.direct "fixture" "{index_uri}",
    ],
]
"#,
            ),
        )
        .unwrap();

        let examples = package_example_roots()
            .into_iter()
            .map(|(name, authored_dir)| {
                let recipe_dir = recipes_dir.join(&name);
                copy_package_directory(&authored_dir, &recipe_dir);
                let recipe_path = recipe_dir.join("stone.glu");
                let build_lock_path = crate::build_lock::path_for_recipe(&recipe_path);
                if build_lock_path.exists() {
                    fs::remove_file(build_lock_path).unwrap();
                }
                let (source_lock_bytes, source_count) = synthesize_source_lock(&recipe_path);
                PackageExample {
                    name,
                    recipe_path,
                    source_lock_bytes,
                    source_count,
                }
            })
            .collect();

        let matrix = Self {
            _root: root,
            cache_dir,
            config_dir,
            data_dir,
            forge_dir,
            output_dir,
            repository_index,
            examples,
        };
        let requested = matrix.requested_packages();
        write_repository_index(&matrix.repository_index, &requested);
        matrix
    }

    fn env(&self) -> Env {
        Env::new(
            Some(self.cache_dir.clone()),
            Some(self.config_dir.clone()),
            Some(self.data_dir.clone()),
            Some(self.forge_dir.clone()),
        )
        .unwrap()
    }

    fn request(&self, example: &PackageExample, update_lock: bool) -> Request {
        Request {
            recipe: example.recipe_path.clone(),
            profile: profile::Id::new(EXAMPLE_PROFILE),
            target: TARGET.to_owned(),
            source_date_epoch: SOURCE_DATE_EPOCH,
            build_release: NonZeroU64::new(1).unwrap(),
            jobs: NonZeroU32::new(1).unwrap(),
            compiler_cache: false,
            update_lock,
            refresh_repositories: update_lock,
        }
    }

    fn builder(&self, example: &PackageExample) -> Builder {
        Builder::new(BuilderRequest {
            recipe_path: example.recipe_path.clone(),
            env: self.env(),
            profile: profile::Id::new(EXAMPLE_PROFILE),
            compiler_cache: false,
            output_dir: self.output_dir.clone(),
            jobs: NonZeroUsize::new(1).unwrap(),
            source_date_epoch: Some(SOURCE_DATE_EPOCH),
            requested_target: TARGET.to_owned(),
        })
        .unwrap_or_else(|error| panic!("{}: create matrix builder: {error:#}", example.name))
    }

    fn requested_packages(&self) -> Vec<String> {
        let mut requested = Vec::new();
        for example in &self.examples {
            let builder = self.builder(example);
            requested.extend(
                build::root::inputs(&builder)
                    .unwrap_or_else(|error| panic!("{}: collect build inputs: {error:#}", example.name))
                    .into_iter()
                    .map(|input| input.request),
            );

            let packager = Packager::new(&builder.paths, &builder.recipe)
                .unwrap_or_else(|error| panic!("{}: resolve package outputs: {error:#}", example.name));
            let package_names = packager.resolved_packages().keys().cloned().collect::<BTreeSet<_>>();
            requested.extend(
                packager
                    .resolved_packages()
                    .values()
                    .flat_map(|package| &package.runtime_inputs)
                    .map(|dependency| dependency.to_name())
                    .filter(|request| !package_names.contains(request)),
            );
        }
        requested.sort();
        requested.dedup();
        requested
    }
}

fn package_example_roots() -> Vec<(String, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/packages");
    let mut examples = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| {
            let name = entry.file_name().into_string().unwrap();
            let path = entry.path();
            assert!(
                path.join("stone.glu").is_file(),
                "package example directory {path:?} has no stone.glu root"
            );
            (name, path)
        })
        .collect::<Vec<_>>();
    examples.sort_by(|left, right| left.0.cmp(&right.0));

    let found = examples.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>();
    assert_eq!(
        found, PACKAGE_EXAMPLES,
        "the planner matrix must explicitly cover every checked-in package example"
    );
    assert_package_example_readme_index(&root, &examples);
    examples
}

fn assert_package_example_readme_index(root: &Path, examples: &[(String, PathBuf)]) {
    let readme_path = root
        .parent()
        .expect("package example root has a parent")
        .join("README.md");
    let readme = fs::read_to_string(&readme_path)
        .unwrap_or_else(|error| panic!("read package example index {readme_path:?}: {error}"));
    let mut linked = Vec::new();
    for line in readme.lines() {
        let Some(row) = line.strip_prefix("| [`") else {
            continue;
        };
        let Some((label, target)) = row.split_once("`](packages/") else {
            continue;
        };
        let Some((directory, _description)) = target.split_once("/stone.glu) |") else {
            panic!("malformed package example README row: {line}");
        };
        assert_eq!(label, directory, "package example README label and target disagree");
        linked.push(directory);
    }

    let linked_set = linked.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        linked.len(),
        linked_set.len(),
        "package example README contains a duplicate package row"
    );
    let expected = examples.iter().map(|(name, _)| name.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(
        linked_set, expected,
        "package example README must index exactly every planner-covered package"
    );
}

fn copy_package_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let destination = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_package_directory(&entry.path(), &destination);
        } else if file_type.is_file() {
            fs::copy(entry.path(), destination).unwrap();
        } else {
            panic!(
                "package example contains unsupported filesystem entry: {:?}",
                entry.path()
            );
        }
    }
}

fn synthesize_source_lock(recipe_path: &Path) -> (Option<Vec<u8>>, usize) {
    let authored = crate::Recipe::load_authored(recipe_path).unwrap();
    if authored.declaration.sources.is_empty() {
        return (None, 0);
    }

    let sources = authored
        .declaration
        .sources
        .iter()
        .enumerate()
        .map(|(order, source)| {
            let order = u32::try_from(order).unwrap();
            match source {
                UpstreamSpec::Archive { url, hash, .. } => SourceResolution::Archive(ArchiveResolution {
                    order,
                    url: url.clone(),
                    sha256: hash.clone(),
                }),
                UpstreamSpec::Git { url, git_ref, .. } => SourceResolution::Git(GitResolution {
                    order,
                    url: url.clone(),
                    requested_ref: git_ref.clone(),
                    commit: EXAMPLE_GIT_COMMIT.to_owned(),
                    materialization_sha256: EXAMPLE_GIT_MATERIALIZATION_SHA256.to_owned(),
                }),
            }
        })
        .collect::<Vec<_>>();
    let source_count = sources.len();
    let lock = SourceLock::new(sources);
    lock.validate_against(&authored.declaration.sources).unwrap();
    let path = recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME);
    write_source_lock(&path, &lock).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes, encode_source_lock(&lock).into_bytes());
    (Some(bytes), source_count)
}

fn write_repository_index(path: &Path, requested: &[String]) {
    let repository_dir = path.parent().expect("repository index has a parent");
    let package_dir = repository_dir.join("packages");
    fs::create_dir_all(&package_dir).unwrap();
    let mut file = fs::File::create(path).unwrap();
    let mut writer = StoneWriter::new(&mut file, StoneHeaderV1FileType::Repository).unwrap();

    for (index, request) in requested.iter().enumerate() {
        let provider = Provider::from_name(request).unwrap();
        let name = if provider.kind == RelationKind::PackageName {
            provider.name.clone()
        } else {
            format!("planner-provider-{index}")
        };
        let package_name = format!("{index}.stone");
        let package_path = package_dir.join(&package_name);
        let mut meta = Meta {
            name: Name::from(name.clone()),
            version_identifier: "1.0.0".to_owned(),
            source_release: 1,
            build_release: 1,
            architecture: TARGET.to_owned(),
            summary: format!("Hermetic provider for {request}"),
            description: format!("Hermetic provider for {request}"),
            source_id: name,
            homepage: "https://example.invalid/planner-provider".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            dependencies: BTreeSet::new(),
            providers: BTreeSet::from([provider]),
            conflicts: BTreeSet::new(),
            uri: None,
            hash: None,
            download_size: None,
        };

        let mut package_file = fs::File::create(&package_path).unwrap();
        let mut package_writer = StoneWriter::new(&mut package_file, StoneHeaderV1FileType::Binary).unwrap();
        let payload = meta.clone().to_stone_payload();
        package_writer.add_payload(payload.as_slice()).unwrap();
        package_writer.finalize().unwrap();
        drop(package_file);

        let package_bytes = fs::read(&package_path).unwrap();
        meta.uri = Some(format!("packages/{package_name}"));
        meta.hash = Some(format!("{:x}", Sha256::digest(&package_bytes)));
        meta.download_size = Some(u64::try_from(package_bytes.len()).unwrap());
        let payload = meta.to_stone_payload();
        writer.add_payload(payload.as_slice()).unwrap();
    }

    writer.finalize().unwrap();
}

fn assert_runtime_reopens_planner_repository_snapshot(
    forge_dir: &Path,
    output_dir: &Path,
    repositories: forge::repository::Map,
    planned: &Planned,
) {
    let expected = planned
        .plan
        .build_lock
        .repositories
        .iter()
        .map(|snapshot| {
            (
                snapshot.id.clone(),
                snapshot.index_uri.clone(),
                snapshot.snapshot.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !expected.is_empty(),
        "the snapshot regression must exercise a repository used by the locked closure"
    );

    let installation = forge::Installation::open_frozen(forge_dir, None).unwrap();
    let client = forge::Client::frozen(
        build::BUILD_REPOSITORY_CACHE_IDENTITY,
        installation,
        repositories.clone(),
        output_dir.join("repository-snapshot-proof"),
    )
    .unwrap();
    let actual = client
        .repository_index_snapshots()
        .unwrap()
        .into_iter()
        .map(|snapshot| (snapshot.id.to_string(), snapshot.index_uri.to_string(), snapshot.sha256))
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "frozen execution must reopen the exact index generation authenticated by planning"
    );
    drop(client);

    // A different client identity is a deliberately independent cache
    // namespace. It must fail closed instead of borrowing the planner's DB or
    // immutable index generation merely because the repository URI matches.
    const ISOLATED_CACHE_IDENTITY: &str = "cast-plan-isolated-regression";
    assert_ne!(
        ISOLATED_CACHE_IDENTITY,
        build::BUILD_REPOSITORY_CACHE_IDENTITY,
        "the adversarial client must use an independent repository namespace"
    );
    let installation = forge::Installation::open_frozen(forge_dir, None).unwrap();
    let isolated = forge::Client::frozen(
        ISOLATED_CACHE_IDENTITY,
        installation,
        repositories,
        output_dir.join("isolated-repository-snapshot-proof"),
    )
    .unwrap();
    assert!(
        matches!(
            isolated.repository_index_snapshots(),
            Err(forge::client::Error::Repository(
                forge::repository::manager::Error::MissingActiveSnapshot(repository)
            )) if repository == forge::repository::Id::new("fixture")
        ),
        "an unrelated cache identity must not inherit the planner's active snapshot"
    );
}

fn container_capability_unavailable(error: &(dyn StdError + 'static)) -> bool {
    // Cleanup is part of the fixture's lifecycle proof. Even when its primary
    // operation is a skippable namespace denial, losing the exact prepared
    // workspace is a hard failure and must not be hidden by source recursion.
    if execution_session::cleanup_failed(error) {
        return false;
    }

    // The Mason wrapper is transparent, so `source()` skips directly to the
    // wrapped container error's own source (often a bare `Errno`). Inspect the
    // typed wrapper before walking the chain or the operation identity needed
    // to distinguish namespace denial from unrelated EPERM failures is lost.
    if let Some(error) = error.downcast_ref::<crate::container::Error>() {
        return match error {
            crate::container::Error::Container(error) => error.execution_capability_unavailable(),
            // A normal developer shell is not an explicitly delegated
            // systemd supervisor. The optional fixture lane may report that
            // one precise host deficiency; malformed or partially configured
            // cgroup state remains a hard failure.
            crate::container::Error::FrozenCgroupDelegationRequired { .. } => true,
            _ => false,
        };
    }

    if let Some(error) = error.downcast_ref::<::container::Error>() {
        return error.execution_capability_unavailable();
    }

    error.source().is_some_and(container_capability_unavailable)
}

fn error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        messages.push(error.to_string());
        source = error.source();
    }
    messages.join(": ")
}

fn execution_capability_required() -> bool {
    match std::env::var("CAST_REQUIRE_EXECUTION") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "0" => false,
        Ok(value) if value == "1" => true,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("CAST_REQUIRE_EXECUTION must be the UTF-8 value 0 or 1")
        }
        Ok(value) => panic!("CAST_REQUIRE_EXECUTION must be 0 or 1, found {value:?}"),
    }
}

fn dependency_names(dependencies: &[DependencySpec]) -> Vec<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.dependency().unwrap().to_name())
        .collect()
}

fn assert_locked_request_origin(plan: &DerivationPlan, request: &str, expected: InputOrigin) {
    let locked = plan
        .build_lock
        .requests
        .iter()
        .find(|locked| locked.request == request)
        .unwrap_or_else(|| panic!("{}: missing frozen request {request}", plan.package.name));
    assert_eq!(
        locked.origins,
        [expected],
        "{}: {request} reached the frozen closure through an unexpected semantic role",
        plan.package.name
    );
}

fn assert_x86_64_platform(plan: &DerivationPlan) {
    let expected = ("x86_64", "aerynos", "linux", "gnu");
    for (name, platform) in [
        ("build", &plan.build_lock.build_platform),
        ("host", &plan.build_lock.host_platform),
        ("target", &plan.build_lock.target_platform),
    ] {
        assert_eq!(
            (
                platform.architecture.as_str(),
                platform.vendor.as_str(),
                platform.operating_system.as_str(),
                platform.abi.as_str(),
            ),
            expected,
            "{}: {name} platform changed",
            plan.package.name
        );
    }
    assert_eq!(plan.package.architecture, "x86_64");
}
