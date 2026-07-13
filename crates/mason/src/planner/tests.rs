// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use forge::{
    Provider,
    package::{Meta, Name},
};
use fs_err as fs;
use stone::{StoneHeaderV1FileType, StoneWriter, relation::Kind as RelationKind};
use stone_recipe::{
    UpstreamSpec,
    derivation::{FilesystemPolicy, InputOrigin, NetworkMode, encode_build_lock},
};
use tempfile::TempDir;
use url::Url;

use super::{Request, plan, plan_for_build};
use crate::{
    Env,
    build::{self, Builder, BuilderRequest},
    build_lock::WriteOutcome,
    package::Packager,
    profile,
    source_lock::{
        ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, encode_source_lock,
        write_source_lock,
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
const PACKAGE_EXAMPLES: [&str; 17] = [
    "autotools",
    "cargo",
    "cmake",
    "conditionals",
    "conflicts",
    "custom-steps",
    "dependency-roles",
    "factory-override",
    "hooks",
    "meson",
    "meta-package",
    "minimal",
    "multiple-sources",
    "options-tuning",
    "profiles-emul32",
    "realistic-daemon",
    "split-outputs",
];

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
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let forge_dir = root.path().join("forge");
        let output_dir = root.path().join("output");
        let recipe_dir = root.path().join("recipe");
        let repository_dir = root.path().join("repository");
        let recipe_path = recipe_dir.join("stone.glu");
        let repository_index = repository_dir.join("stone.index");

        fs::create_dir_all(data_dir.join("policy")).unwrap();
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipe_dir).unwrap();
        fs::create_dir_all(&repository_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();

        fs::write(
            data_dir.join("policy/policy.glu"),
            include_str!("../../data/policy/policy.glu"),
        )
        .unwrap();
        fs::write(
            data_dir.join("policy/default.glu"),
            include_str!("../../data/policy/default.glu"),
        )
        .unwrap();
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

    fn requested_packages(&self) -> Vec<String> {
        let builder = Builder::new(BuilderRequest {
            recipe_path: self.recipe_path.clone(),
            env: self.env(),
            profile: profile::Id::new(PROFILE),
            compiler_cache: false,
            output_dir: self.output_dir.clone(),
            jobs: NonZeroUsize::new(1).unwrap(),
            source_date_epoch: Some(SOURCE_DATE_EPOCH),
            requested_target: TARGET.to_owned(),
        })
        .unwrap();
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
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let forge_dir = root.path().join("forge");
        let output_dir = root.path().join("output");
        let recipes_dir = root.path().join("recipes");
        let repository_dir = root.path().join("repository");
        let repository_index = repository_dir.join("stone.index");

        fs::create_dir_all(data_dir.join("policy")).unwrap();
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipes_dir).unwrap();
        fs::create_dir_all(&repository_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(
            data_dir.join("policy/policy.glu"),
            include_str!("../../data/policy/policy.glu"),
        )
        .unwrap();
        fs::write(
            data_dir.join("policy/default.glu"),
            include_str!("../../data/policy/default.glu"),
        )
        .unwrap();

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

    fn requested_packages(&self) -> Vec<String> {
        let mut requested = Vec::new();
        for example in &self.examples {
            let builder = Builder::new(BuilderRequest {
                recipe_path: example.recipe_path.clone(),
                env: self.env(),
                profile: profile::Id::new(EXAMPLE_PROFILE),
                compiler_cache: false,
                output_dir: self.output_dir.clone(),
                jobs: NonZeroUsize::new(1).unwrap(),
                source_date_epoch: Some(SOURCE_DATE_EPOCH),
                requested_target: TARGET.to_owned(),
            })
            .unwrap_or_else(|error| panic!("{}: create matrix builder: {error:#}", example.name));
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
    examples
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
    let mut file = fs::File::create(path).unwrap();
    let mut writer = StoneWriter::new(&mut file, StoneHeaderV1FileType::Repository).unwrap();

    for (index, request) in requested.iter().enumerate() {
        let provider = Provider::from_name(request).unwrap();
        let name = if provider.kind == RelationKind::PackageName {
            provider.name.clone()
        } else {
            format!("planner-provider-{index}")
        };
        let hash = format!("{:064x}", index + 1);
        let meta = Meta {
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
            uri: Some(format!("packages/{index}.stone")),
            hash: Some(hash),
            download_size: Some(1),
        };
        let payload = meta.to_stone_payload();
        writer.add_payload(payload.as_slice()).unwrap();
    }

    writer.finalize().unwrap();
}

#[test]
fn identical_explicit_inputs_produce_identical_plans_and_locks() {
    let fixture = Fixture::new();

    let first = plan(fixture.env(), fixture.request()).unwrap();
    let first_plan = first.plan.canonical_bytes();
    let first_id = first.plan.derivation_id();
    let first_lock = fs::read(&first.lock_path).unwrap();

    assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
    assert_eq!(first.plan.execution.executor.name, super::EXECUTOR_ABI);
    assert_eq!(first.plan.build_lock.builder.name, "custom");
    assert_ne!(
        first.plan.build_lock.builder.name, first.plan.execution.executor.name,
        "authored structural builder and executor identities must remain separate"
    );
    assert_eq!(first.plan.execution.network, NetworkMode::Disabled);
    assert_eq!(first.plan.execution.filesystems, FilesystemPolicy::default());
    assert_eq!(
        first.plan.environment.get("SOURCE_DATE_EPOCH").map(String::as_str),
        Some("1700000000")
    );
    assert!(!first.plan.build_lock.requests.is_empty());
    let runtime_request = first
        .plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == RUNTIME_REQUEST)
        .expect("the external output runtime input must be resolved");
    assert_eq!(
        runtime_request.origins,
        [InputOrigin::OutputRuntime {
            output: "out".to_owned(),
            index: 0,
        }]
    );
    assert!(
        first
            .plan
            .build_lock
            .repositories
            .iter()
            .all(|repository| { Url::parse(&repository.index_uri).is_ok_and(|uri| uri.scheme() == "file") })
    );

    let repeated = plan(fixture.env(), fixture.request()).unwrap();

    assert_eq!(repeated.lock_outcome, Some(WriteOutcome::Unchanged));
    assert_eq!(
        repeated.plan.build_lock.request_fingerprint,
        first.plan.build_lock.request_fingerprint
    );
    assert_eq!(repeated.plan.build_lock.requests, first.plan.build_lock.requests);
    assert_eq!(repeated.plan.provenance, first.plan.provenance);
    assert_eq!(repeated.plan.canonical_bytes(), first_plan);
    assert_eq!(repeated.plan.derivation_id(), first_id);
    assert_eq!(fs::read(&repeated.lock_path).unwrap(), first_lock);
}

#[test]
fn checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks() {
    let matrix = PackageExampleMatrix::new();
    let repository_uri = Url::from_file_path(&matrix.repository_index).unwrap().to_string();

    for example in &matrix.examples {
        let first = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{}: freeze example plan: {error:#}", example.name));
        first
            .plan
            .validate()
            .unwrap_or_else(|error| panic!("{}: validate frozen example plan: {error:#}", example.name));
        assert_eq!(
            first.lock_outcome,
            Some(WriteOutcome::Written),
            "{}: first freeze must create a fresh build lock",
            example.name
        );
        assert_eq!(
            first.plan.sources.len(),
            example.source_count,
            "{}: every authored source must reach the derivation plan",
            example.name
        );
        assert!(
            !first.plan.build_lock.repositories.is_empty()
                && first
                    .plan
                    .build_lock
                    .repositories
                    .iter()
                    .all(|repository| repository.index_uri == repository_uri),
            "{}: dependency resolution must use only the temporary local file repository",
            example.name
        );
        assert!(
            first
                .plan
                .build_lock
                .repositories
                .iter()
                .all(|repository| Url::parse(&repository.index_uri)
                    .is_ok_and(|uri| uri.scheme() == "file" && uri.to_file_path().is_ok())),
            "{}: the temporary repository must remain a valid file URL",
            example.name
        );

        let first_plan_bytes = first.plan.canonical_bytes();
        let first_derivation_id = first.plan.derivation_id();
        let first_lock_bytes = fs::read(&first.lock_path).unwrap();
        assert_eq!(
            first_lock_bytes,
            encode_build_lock(&first.plan.build_lock).into_bytes(),
            "{}: the on-disk build lock must be the canonical encoding of the frozen lock",
            example.name
        );
        match &example.source_lock_bytes {
            Some(expected) => assert_eq!(
                fs::read(example.recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME)).unwrap(),
                *expected,
                "{}: planning must not rewrite the synthetic canonical source lock",
                example.name
            ),
            None => assert!(
                !example.recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME).exists(),
                "{}: source-less examples must not gain a synthetic source lock",
                example.name
            ),
        }

        let locked = plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{}: plan from written build lock: {error:#}", example.name));
        assert_eq!(
            locked.lock_outcome, None,
            "{}: the second plan must consume, not regenerate, build.lock.glu",
            example.name
        );
        assert_eq!(
            locked.plan.canonical_bytes(),
            first_plan_bytes,
            "{}: canonical plan bytes changed when reusing the build lock",
            example.name
        );
        assert_eq!(
            locked.plan.derivation_id(),
            first_derivation_id,
            "{}: derivation identity changed when reusing the build lock",
            example.name
        );
        assert_eq!(
            fs::read(&locked.lock_path).unwrap(),
            first_lock_bytes,
            "{}: consuming the build lock changed its canonical bytes",
            example.name
        );
    }
}

#[test]
fn selected_profile_name_participates_in_the_request_fingerprint() {
    let fixture = Fixture::new();
    let first = plan(fixture.env(), fixture.request()).unwrap();
    let mut alternate_request = fixture.request();
    alternate_request.profile = profile::Id::new(ALTERNATE_PROFILE);
    let alternate = plan(fixture.env(), alternate_request).unwrap();

    assert_eq!(
        first.plan.build_lock.profile.fingerprint, alternate.plan.build_lock.profile.fingerprint,
        "both selections intentionally share the same ordered fragment aggregate"
    );
    assert_ne!(
        first.plan.build_lock.profile.name,
        alternate.plan.build_lock.profile.name
    );
    assert_ne!(
        first.plan.build_lock.request_fingerprint,
        alternate.plan.build_lock.request_fingerprint
    );
}
