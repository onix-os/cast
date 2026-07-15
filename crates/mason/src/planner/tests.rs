#![cfg_attr(
    all(feature = "delegated-fixture-test-support", not(test)),
    allow(dead_code, unused_imports)
)]

use std::{
    collections::BTreeSet,
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
    Env, Timing,
    build::{self, Builder, BuilderRequest},
    build_lock::WriteOutcome,
    executor::Executor,
    package::{self, FrozenPackager, Packager, Publication},
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
const PACKAGE_EXAMPLES: [&str; 47] = [
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
    "external-patch-source",
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
    "manual-compiler-pipeline",
    "maven-application",
    "meson",
    "meta-package",
    "minimal",
    "multiple-sources",
    "nodejs-vendored-application",
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
    "release-source-factory",
    "shared-capability-origins",
    "source-less-generated-config",
    "split-outputs",
    "system-integration-assets",
    "target-profile-specialization",
    "zig-project",
];
const EXECUTION_FIXTURES: [&str; 10] = [
    "autotools",
    "cargo",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "factory-override",
    "hooks-patch",
    "meson",
    "split",
];

#[path = "tests/bootstrap.rs"]
mod bootstrap;
#[path = "tests/documented_semantics/dependencies.rs"]
mod documented_dependencies;
#[path = "tests/documented_semantics/generated.rs"]
mod documented_generated;
#[path = "tests/documented_semantics/profiles.rs"]
mod documented_profiles;
#[path = "tests/documented_semantics.rs"]
mod documented_semantics;
#[path = "tests/documented_semantics/variants.rs"]
mod documented_variants;

#[cfg(feature = "delegated-fixture-test-support")]
pub(super) fn run_delegated_execution_fixture() {
    bootstrap::run_delegated_execution_fixture();
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
    examples
}

#[test]
fn offline_execution_fixture_archives_are_real_locked_and_complete() {
    let temporary = crate::private_tempdir();
    let cache = temporary.path().join("source-cache");
    let shared = temporary.path().join("shared");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution");
    let packages = root.join("packages");
    let archives = root.join("archives");
    let source_trees = root.join("source-trees");

    let discovered = [&packages, &source_trees].map(|directory| {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    });
    assert_eq!(discovered[0], EXECUTION_FIXTURES);
    assert_eq!(
        discovered[1],
        [
            "cast-autotools-fixture-1.0.0",
            "cast-cargo-fixture-1.0.0",
            "cast-cargo-vendored-fixture-1.0.0",
            "cast-cmake-fixture-1.0.0",
            "cast-custom-fixture-1.0.0",
            "cast-daemon-fixture-1.0.0",
            "cast-factory-override-fixture-1.0.0",
            "cast-hooks-fixture-1.0.0",
            "cast-meson-fixture-1.0.0",
            "cast-split-fixture-1.0.0",
        ]
    );

    let mut admitted_archives = BTreeSet::new();
    let mut archive_format_counts = [0_usize; 4];
    for name in EXECUTION_FIXTURES {
        let recipe_path = packages.join(name).join("stone.glu");
        let recipe = crate::Recipe::load_authored(&recipe_path)
            .unwrap_or_else(|error| panic!("{name}: evaluate execution fixture: {error:#}"));
        if name == "factory-override" {
            let factory = recipe
                .fingerprint
                .imported_modules
                .iter()
                .find(|module| module.logical_name == "factory.glu")
                .expect("factory-override: local Gluon factory is absent from recipe provenance");
            assert_eq!(
                factory.sha256,
                hex::encode(Sha256::digest(
                    fs::read(packages.join(name).join("factory.glu")).unwrap()
                )),
                "factory-override: recipe provenance does not bind the exact imported factory"
            );
            assert_eq!(recipe.declaration.architectures, ["x86_64"]);
            let [StepSpec::CMakeConfigure { flags }] = recipe.declaration.builder.phases.setup.steps.as_slice() else {
                panic!("factory-override: package patch did not select the CMake builder");
            };
            assert_eq!(flags.as_slice(), ["-DCAST_FACTORY_VARIANT=stone-override"]);
        }
        let lock_path = recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME);
        let lock_bytes = fs::read(&lock_path).unwrap();
        let lock = decode_source_lock(SOURCE_LOCK_FILE_NAME, &lock_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode source lock: {error:#}"));
        lock.validate_against(&recipe.declaration.sources)
            .unwrap_or_else(|error| panic!("{name}: validate source lock: {error:#}"));
        assert_eq!(
            lock_bytes,
            encode_source_lock(&lock).into_bytes(),
            "{name}: checked-in source lock is not canonical"
        );

        let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
            panic!("{name}: execution fixture must have exactly one archive source");
        };
        let url = Url::parse(&source.url).unwrap();
        assert_eq!(
            url.scheme(),
            "https",
            "{name}: production source policy must remain HTTPS"
        );
        assert_eq!(url.host_str(), Some("fixtures.invalid"));
        let filename = url.path_segments().unwrap().next_back().unwrap();
        let archive_path = archives.join(filename);
        let metadata = fs::symlink_metadata(&archive_path).unwrap();
        assert!(metadata.file_type().is_file(), "{name}: archive must be a regular file");
        let bytes = fs::read(&archive_path).unwrap();
        assert_eq!(metadata.len(), u64::try_from(bytes.len()).unwrap());
        assert!(
            (1..=1024 * 1024).contains(&metadata.len()),
            "{name}: encoded fixture archive must remain small and non-empty"
        );

        let mut decoder: Box<dyn Read + '_> = match name {
            "cargo-vendored" => {
                assert_eq!(filename, "cast-cargo-vendored-fixture-1.0.0.tar.gz");
                assert!(bytes.starts_with(&[0x1f, 0x8b, 0x08]), "{name}: missing gzip magic");
                archive_format_counts[1] += 1;
                Box::new(flate2::read::GzDecoder::new(bytes.as_slice()))
            }
            "hooks-patch" => {
                assert_eq!(filename, "cast-hooks-fixture-1.0.0.tar.xz");
                assert!(
                    bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
                    "{name}: missing XZ magic"
                );
                archive_format_counts[2] += 1;
                Box::new(xz2::read::XzDecoder::new(bytes.as_slice()))
            }
            "daemon-generated" => {
                assert_eq!(filename, "cast-daemon-fixture-1.0.0.tar.zst");
                assert!(
                    bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
                    "{name}: missing Zstandard magic"
                );
                archive_format_counts[3] += 1;
                Box::new(zstd::stream::read::Decoder::new(bytes.as_slice()).unwrap())
            }
            _ => {
                assert!(filename.ends_with(".tar"), "{name}: expected a plain .tar fixture");
                archive_format_counts[0] += 1;
                Box::new(std::io::Cursor::new(bytes.as_slice()))
            }
        };
        let mut tar_bytes = Vec::new();
        decoder
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut tar_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode execution fixture archive: {error}"));
        assert!(
            (10_240..=1024 * 1024).contains(&tar_bytes.len()) && tar_bytes.len() % 512 == 0,
            "{name}: decoded fixture must remain one small block-aligned tar stream"
        );
        assert_eq!(
            &tar_bytes[257..263],
            b"ustar\0",
            "{name}: decoded fixture is not a USTAR archive"
        );
        assert_eq!(hex::encode(Sha256::digest(&bytes)), source.sha256);
        assert!(
            admitted_archives.insert(filename.to_owned()),
            "duplicate execution archive"
        );

        let materialization_name = recipe.declaration.sources[0].materialization_name().unwrap();
        let locked = stone_recipe::derivation::LockedSource::Archive {
            order: 0,
            url: source.url.clone(),
            sha256: source.sha256.clone(),
            filename: materialization_name.clone(),
        };
        crate::upstream::import_locked_archive_fixture(&locked, &cache, &archive_path)
            .unwrap_or_else(|error| panic!("{name}: import locked fixture into source cache: {error:#}"));
        let share = shared.join(name);
        crate::upstream::sync_locked(std::slice::from_ref(&locked), &cache, &share, SOURCE_DATE_EPOCH)
            .unwrap_or_else(|error| panic!("{name}: share imported fixture through frozen source path: {error:#}"));
        let shared_archive = share.join(&materialization_name);
        assert_eq!(fs::read(&shared_archive).unwrap(), bytes);
        let shared_metadata = fs::metadata(&shared_archive).unwrap();
        let fixture_metadata = fs::metadata(&archive_path).unwrap();
        assert_ne!(
            (shared_metadata.dev(), shared_metadata.ino()),
            (fixture_metadata.dev(), fixture_metadata.ino()),
            "{name}: build-visible source must not alias the tracked fixture"
        );

        // Exercise the same structural two-pass extractor and atomic
        // publication path used by a real build. In particular, the three
        // compressed fixtures must not be accepted on filename or magic alone.
        let build = temporary.path().join("extracted").join(name);
        fs::create_dir_all(&build).unwrap();
        let mut archive_session = crate::archive::ArchiveSessionBudget::production();
        crate::archive::extract_locked_tar(
            &share,
            &materialization_name,
            &source.sha256,
            &build,
            "source",
            1,
            SOURCE_DATE_EPOCH,
            &mut archive_session,
        )
        .unwrap_or_else(|error| panic!("{name}: structurally extract and publish locked fixture: {error:#}"));
        let published = build.join("source");
        assert!(published.is_dir(), "{name}: extractor did not publish its destination");
        assert!(
            fs::read_dir(&published).unwrap().next().is_some(),
            "{name}: extractor published an empty source tree"
        );
    }

    let present_archives = fs::read_dir(archives)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        present_archives, admitted_archives,
        "orphaned execution fixture archive"
    );
    assert_eq!(
        archive_format_counts,
        [7, 1, 1, 1],
        "execution fixtures must cover seven plain tar streams plus one each of gzip, XZ, and Zstandard"
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

#[derive(Debug, thiserror::Error)]
enum FrozenExamplePayloadError {
    #[error("execute frozen example")]
    Execute(#[from] crate::executor::Error),
    #[error("package frozen example")]
    Package(#[from] package::Error),
}

fn execute_and_publish(planned: &Planned) -> Result<Publication, Box<dyn StdError + Send + Sync>> {
    let executor = Executor::new(&planned.plan)?;
    let packager = FrozenPackager::from_plan(&planned.runtime.paths, &planned.plan)?;
    let execution_lock = planned.runtime.acquire_execution_lock(&planned.plan)?;
    let mut timing = Timing::default();
    let initialize_timer = timing.begin(crate::timing::Kind::Initialize);
    let prepared = planned
        .runtime
        .setup(&planned.plan, &execution_lock, &mut timing, initialize_timer)?;

    prepared.require_for(&planned.runtime.paths, &planned.plan)?;
    crate::container::exec_frozen::<FrozenExamplePayloadError>(
        &planned.runtime.paths,
        &planned.plan,
        prepared.sandbox(),
        prepared.root_guard(),
        || {
            executor.run(&mut timing)?;
            packager.package(&execution_lock, &mut timing)?;
            Ok(())
        },
    )?;

    Ok(package::publish_artefacts(
        &planned.runtime.paths,
        &planned.plan,
        &execution_lock,
        prepared.artefacts()?,
        package::ManifestVerification::None,
    )?)
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

#[test]
fn planner_and_frozen_runtime_share_one_authenticated_repository_snapshot_namespace() {
    let fixture = Fixture::new();
    let repositories = fixture.builder().repositories().clone();
    let planned = plan(fixture.env(), fixture.request()).unwrap();

    assert_runtime_reopens_planner_repository_snapshot(&fixture.forge_dir, &fixture.output_dir, repositories, &planned);
}

fn capability_errno(error: &(dyn StdError + 'static)) -> bool {
    if let Some(source) = error.downcast_ref::<std::io::Error>()
        && (source.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(
                source.raw_os_error(),
                Some(code)
                    if code == nix::libc::EPERM
                        || code == nix::libc::EACCES
                        || code == nix::libc::ENOSYS
            ))
    {
        return true;
    }
    if let Some(source) = error.downcast_ref::<nix::errno::Errno>()
        && matches!(
            source,
            nix::errno::Errno::EPERM | nix::errno::Errno::EACCES | nix::errno::Errno::ENOSYS
        )
    {
        return true;
    }
    error.source().is_some_and(capability_errno)
}

fn setup_capability_denial(message: &str) -> bool {
    let message = message.strip_prefix("exited with failure: ").unwrap_or(message);
    let setup_failure = [
        "clear inherited supplementary groups",
        "mount ",
        "pivot_root",
        "sethostname",
        "unmount old root",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix));
    // Child setup errors are rendered from a typed `nix::Errno` source. Match
    // only its exact terminal representation: paths and operation labels are
    // diagnostic text and must never be able to smuggle an errno substring
    // into the capability-skip decision.
    let permission_failure = [
        ": EPERM: Operation not permitted",
        ": EACCES: Permission denied",
        ": ENOSYS: Function not implemented",
    ]
    .iter()
    .any(|suffix| message.ends_with(suffix));
    setup_failure && permission_failure
}

fn container_capability_unavailable(error: &(dyn StdError + 'static)) -> bool {
    // The Mason wrapper is transparent, so `source()` skips directly to the
    // wrapped container error's own source (often a bare `Errno`). Inspect the
    // typed wrapper before walking the chain or the operation identity needed
    // to distinguish namespace denial from unrelated EPERM failures is lost.
    if let Some(error) = error.downcast_ref::<crate::container::Error>() {
        return match error {
            crate::container::Error::Container(error) => container_capability_unavailable(error),
            // A normal developer shell is not an explicitly delegated
            // systemd supervisor. The optional fixture lane may report that
            // one precise host deficiency; malformed or partially configured
            // cgroup state remains a hard failure.
            crate::container::Error::FrozenCgroupDelegationRequired { .. } => true,
            _ => false,
        };
    }

    if let Some(error) = error.downcast_ref::<::container::Error>() {
        return match error {
            ::container::Error::CloneNamespaces { source } => matches!(
                source,
                nix::errno::Errno::EPERM | nix::errno::Errno::EACCES | nix::errno::Errno::ENOSYS
            ),
            ::container::Error::CloneIntoCgroup { source } => matches!(
                source.raw_os_error(),
                Some(code)
                    if code == nix::libc::ENOSYS
                        || code == nix::libc::EPERM
                        || code == nix::libc::E2BIG
            ),
            // Once the kernel accepted atomic placement, authentication and
            // teardown failures are lifecycle violations, not evidence that
            // the host merely lacks an optional execution capability.
            ::container::Error::CgroupLifecycle { .. }
            | ::container::Error::ChildCleanup { .. }
            | ::container::Error::ChildCleanupAfterFailure { .. }
            | ::container::Error::CgroupCleanup { .. }
            | ::container::Error::CgroupCleanupAfterFailure { .. }
            | ::container::Error::AtomicCgroupRequiresAnchoredRoot
            | ::container::Error::InspectCgroupFilesystem { .. }
            | ::container::Error::UnsafeCgroupRootFilesystem { .. }
            | ::container::Error::UnsafeCgroupBindSource { .. }
            | ::container::Error::UnsafeCgroupSysPolicy => false,
            // Pipe, signal, and wait failures remain grouped here. An errno
            // alone therefore does not prove namespace capability denial.
            ::container::Error::Nix { .. } => false,
            ::container::Error::Idmap { source } => {
                source
                    .to_string()
                    .contains("needs at least one delegated subordinate GID")
                    || capability_errno(source)
            }
            ::container::Error::Failure { message } => setup_capability_denial(message),
            ::container::Error::Signaled { .. } | ::container::Error::UnknownExit => false,
        };
    }

    // `thiserror` may make a transparent wrapper's concrete inner error
    // unavailable to `downcast_ref`, but its exact display remains in the
    // source chain. Accept only known setup labels, never `run: ...` payload
    // failures. Typed lifecycle errors above take precedence so cleanup text
    // can never turn a post-clone violation into an optional-capability skip.
    if setup_capability_denial(&error.to_string()) {
        return true;
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

fn assert_documented_factory_semantics(name: &str, declaration: &PackageSpec, plan: &DerivationPlan) {
    match name {
        "backend-choice-factory" => documented_variants::assert_semantics(declaration, plan),
        "factory-override" => assert_factory_override_semantics(declaration, plan),
        "gettext-catalogs" => assert_gettext_catalog_semantics(declaration, plan),
        "go-module" => assert_go_module_semantics(declaration, plan),
        "kernel-module-factory" => assert_kernel_module_factory_semantics(declaration, plan),
        "layered-overrides" => assert_layered_override_semantics(declaration, plan),
        "maven-application" => assert_maven_application_semantics(declaration, plan),
        "nodejs-vendored-application" => assert_nodejs_vendored_application_semantics(declaration, plan),
        "output-policy-factory" => assert_output_policy_factory_semantics(declaration, plan),
        "platform-factory" => assert_platform_factory_semantics(declaration, plan),
        "shared-capability-origins" => documented_dependencies::assert_semantics(declaration, plan),
        "source-less-generated-config" => documented_generated::assert_semantics(declaration, plan),
        "target-profile-specialization" => documented_profiles::assert_semantics(declaration, plan),
        "zig-project" => assert_zig_project_semantics(declaration, plan),
        _ => documented_semantics::assert_semantics(name, declaration, plan),
    }
}

fn assert_kernel_module_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "atlas-sensor-module");
    assert_eq!(declaration.architectures, ["x86_64", "aarch64"]);
    assert!(matches!(
        declaration.native_build_inputs.as_slice(),
        [DependencySpec::Output(output)]
            if output.package.name == "linux-lts" && output.output == "devel"
    ));
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(make)", "binary(modinfo)", "binary(bash)", "binary(install)"]
    );
    let [StepSpec::Run { args, .. }] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("kernel-module-factory must retain one structural make step");
    };
    assert_eq!(
        args.as_slice(),
        [
            "KERNEL_RELEASE=6.12.28-onix1",
            "KERNEL_DIR=/usr/lib/modules/6.12.28-onix1/build",
            "modules",
        ]
    );
    let root = declaration.outputs.iter().find(|output| output.name == "out").unwrap();
    assert!(root.paths.iter().any(|path| {
        matches!(path, stone_recipe::PathSpec::Any { path }
            if path == "/usr/lib/modules/6.12.28-onix1/extra/atlas-sensor.ko")
    }));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_layered_override_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "layered-proxy");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(hardening-check)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libarchive)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "docs",
            "devel",
            "dbginfo",
            "libs",
            "32bit",
            "32bit-devel",
            "32bit-dbginfo",
            "demos",
            "tools",
        ]
    );
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| tuning.key.as_str())
            .collect::<Vec<_>>(),
        ["harden", "optimize"]
    );
    assert!(!declaration.options.debug);
    assert!(declaration.options.strip);
    assert!(declaration.options.compressman);
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_maven_application_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "pulse-router");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mvn)", "binary(install)"]
    );
    for phase in [&declaration.builder.phases.build, &declaration.builder.phases.check] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("maven-application build and check phases must remain explicit shell steps");
        };
        for required in ["--offline", "-Dmaven.repo.local=", "-Dproject.build.outputTimestamp="] {
            assert!(
                script.contains(required),
                "maven-application lost offline setting {required}"
            );
        }
    }
    assert!(!declaration.options.networking);
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_nodejs_vendored_application_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "nodejs-nebula-lint");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(node)", "binary(install)", "binary(cp)"]
    );
    for phase in [&declaration.builder.phases.build, &declaration.builder.phases.check] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("nodejs-vendored-application build and check phases must remain explicit shell steps");
        };
        assert!(script.contains("NODE_PATH=\"${CAST_SOURCE_DIR}/vendor/node_modules\""));
        assert!(!script.contains("npm"));
    }
    assert!(!declaration.options.networking);
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_gettext_catalog_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "orbit-catalogs");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mkdir)", "binary(msgfmt)", "binary(install)"]
    );
    assert_eq!(declaration.builder.phases.build.steps.len(), 2);
    assert_eq!(declaration.builder.phases.check.steps.len(), 2);
    assert_eq!(declaration.outputs.len(), 2);
    assert!(declaration.outputs[0].paths.iter().any(|path| {
        matches!(path, stone_recipe::PathSpec::Any { path } if path == "/usr/share/locale/*/LC_MESSAGES/orbit.mo")
    }));
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_go_module_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "go-glyph");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mkdir)", "binary(go)", "binary(install)"]
    );
    let [StepSpec::Shell { script: build, .. }] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("go-module must retain one explicit shell build step");
    };
    for required in [
        "GOPROXY=off",
        "GOSUMDB=off",
        "-mod=vendor",
        "-trimpath",
        "-buildvcs=false",
    ] {
        assert!(
            build.contains(required),
            "go-module build lost offline/reproducible setting {required}"
        );
    }
    let [StepSpec::Shell { script: check, .. }] = declaration.builder.phases.check.steps.as_slice() else {
        panic!("go-module must retain one explicit shell check step");
    };
    for required in ["GOPROXY=off", "GOSUMDB=off", "-mod=vendor", "-trimpath"] {
        assert!(
            check.contains(required),
            "go-module check lost offline/reproducible setting {required}"
        );
    }
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_zig_project_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "zig-vector");
    assert_eq!(dependency_names(&declaration.builder.required_tools), ["binary(zig)"]);
    for phase in [
        &declaration.builder.phases.build,
        &declaration.builder.phases.check,
        &declaration.builder.phases.install,
    ] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("zig-project phases must remain explicit shell steps");
        };
        assert!(script.contains("ZIG_GLOBAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-global-cache\""));
        assert!(script.contains("ZIG_LOCAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-local-cache\""));
    }
    let root = declaration.outputs.iter().find(|output| output.name == "out").unwrap();
    let development = declaration
        .outputs
        .iter()
        .find(|output| output.name == "devel")
        .unwrap();
    for output in [root, development] {
        assert!(matches!(
            output.runtime_inputs.as_slice(),
            [DependencySpec::Output(reference)]
                if reference.package.name == "zig-vector" && reference.output == "libs"
        ));
    }
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_factory_override_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "override-client");
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libressl)"],
        "the explicit TLS argument must replace the factory's OpenSSL default"
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "docs",
            "devel",
            "dbginfo",
            "libs",
            "32bit",
            "32bit-devel",
            "32bit-dbginfo",
            "demos",
            "tools",
        ],
        "the output patch must append tools without disturbing the base output order"
    );
    let tools = declaration.outputs.last().expect("factory override appends tools");
    assert!(
        matches!(
            tools.runtime_inputs.as_slice(),
            [DependencySpec::Output(output)]
                if output.package.name == "override-client" && output.output == "out"
        ),
        "the appended tools output must depend on the package's exact root output"
    );
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DUSE_SYSTEM_LIBRARIES=ON".to_owned()],
        }]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(ninja)", "pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    let frozen_tools = plan
        .outputs
        .iter()
        .find(|output| output.name == "tools")
        .expect("the appended tools output reaches the frozen plan");
    assert!(matches!(
        frozen_tools.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "out"
    ));
    assert_locked_request_origin(
        plan,
        "pkgconfig(zlib)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 0,
        },
    );
    assert_locked_request_origin(
        plan,
        "pkgconfig(libressl)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 1,
        },
    );
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| request.request != "pkgconfig(openssl)"),
        "the replaced OpenSSL default must not leak into the frozen closure"
    );
    assert_x86_64_platform(plan);
}

fn assert_output_policy_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "telemetry-runtime");
    assert!(
        declaration.native_build_inputs.is_empty(),
        "disabled documentation policy must omit its generator capability"
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "libs", "devel", "tools"],
        "one policy value must select the exact published output graph"
    );
    let [root, libraries, development, tools] = declaration.outputs.as_slice() else {
        panic!("output policy must return exactly four outputs");
    };
    assert!(libraries.runtime_inputs.is_empty());
    for (output, name) in [(root, "out"), (development, "devel")] {
        assert!(
            matches!(
                output.runtime_inputs.as_slice(),
                [DependencySpec::Output(reference)]
                    if reference.package.name == "telemetry-runtime" && reference.output == "libs"
            ),
            "{name} must retain its exact local library-output relation"
        );
    }
    assert!(matches!(
        tools.runtime_inputs.as_slice(),
        [DependencySpec::Output(libraries), DependencySpec::Package(trust_store)]
            if libraries.package.name == "telemetry-runtime"
                && libraries.output == "libs"
                && trust_store.name == "ca-certificates"
    ));
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DBUILD_COMMAND_LINE_TOOLS=ON".to_owned(),
                "-DBUILD_DOCUMENTATION=OFF".to_owned(),
            ],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| (tuning.key.as_str(), &tuning.value))
            .collect::<Vec<_>>(),
        [
            ("harden", &TuningSpec::Enable),
            (
                "optimize",
                &TuningSpec::Config {
                    value: "size".to_owned(),
                },
            ),
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(ninja)", "pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    for output in ["out", "devel"] {
        let frozen = plan
            .outputs
            .iter()
            .find(|candidate| candidate.name == output)
            .unwrap_or_else(|| panic!("missing frozen {output} output"));
        assert!(matches!(
            frozen.runtime_inputs.as_slice(),
            [OutputRelation::Planned { output }] if output == "libs"
        ));
    }
    let frozen_tools = plan
        .outputs
        .iter()
        .find(|output| output.name == "tools")
        .expect("policy-selected tools output reaches the frozen plan");
    assert!(matches!(
        frozen_tools.runtime_inputs.as_slice(),
        [
            OutputRelation::Planned { output },
            OutputRelation::Locked { relation, reference },
        ] if output == "libs"
            && relation.canonical_name() == "ca-certificates"
            && reference.output == "out"
    ));
    for (request, origin) in [
        (
            "pkgconfig(zlib)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(libressl)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 1,
            },
        ),
        (
            "ca-certificates",
            InputOrigin::OutputRuntime {
                output: "tools".to_owned(),
                index: 1,
            },
        ),
    ] {
        assert_locked_request_origin(plan, request, origin);
    }
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| request.request != "binary(doxygen)"),
        "disabled documentation tooling must not leak into the frozen closure"
    );
    assert_x86_64_platform(plan);
}

fn assert_platform_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "relay-engine");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(protocol-compiler)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(openssl)", "pkgconfig(liburing)"],
        "the selected platform must supply liburing after the reusable dependencies"
    );
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DENABLE_PORTABLE_DISPATCH=ON".to_owned(),
                "-DENABLE_SERVER=OFF".to_owned(),
                "-DUSE_IO_URING=ON".to_owned(),
            ],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| (tuning.key.as_str(), &tuning.value))
            .collect::<Vec<_>>(),
        [
            ("harden", &TuningSpec::Enable),
            (
                "lto",
                &TuningSpec::Config {
                    value: "thin".to_owned(),
                },
            ),
            (
                "optimize",
                &TuningSpec::Config {
                    value: "speed".to_owned(),
                },
            ),
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(ninja)",
            "binary(protocol-compiler)",
            "pkgconfig(zlib)",
            "pkgconfig(openssl)",
            "pkgconfig(liburing)",
        ]
    );
    for (request, origin) in [
        (
            "binary(protocol-compiler)",
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(zlib)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(openssl)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 1,
            },
        ),
        (
            "pkgconfig(liburing)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 2,
            },
        ),
    ] {
        assert_locked_request_origin(plan, request, origin);
    }
    assert_x86_64_platform(plan);
}

fn assert_factory_override_changes_frozen_identity(matrix: &PackageExampleMatrix) {
    let example = matrix
        .examples
        .iter()
        .find(|example| example.name == "factory-override")
        .expect("the explicit example inventory contains factory-override");
    let original = plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir)
        .expect("reuse the original factory-override build lock");
    let original_source = fs::read_to_string(&example.recipe_path).unwrap();
    const OVERRIDE: &str = "b.dep.pkgconfig \"libressl\"";
    const CHANGED_OVERRIDE: &str = "b.dep.pkgconfig \"openssl\"";
    assert_eq!(
        original_source.matches(OVERRIDE).count(),
        1,
        "the fingerprint proof must mutate exactly one explicit factory argument"
    );
    let changed_source = original_source.replacen(OVERRIDE, CHANGED_OVERRIDE, 1);
    fs::write(&example.recipe_path, changed_source).unwrap();

    let changed_evaluation = matrix.builder(example);
    assert_eq!(
        dependency_names(&changed_evaluation.recipe.declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(openssl)"]
    );
    let changed = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("freeze the changed factory override");

    assert_eq!(changed.lock_outcome, Some(WriteOutcome::Written));
    assert_eq!(changed.plan.provenance.recipe, changed_evaluation.recipe.fingerprint);
    assert_ne!(
        original.plan.provenance.recipe.sha256, changed.plan.provenance.recipe.sha256,
        "changing a factory argument must invalidate the complete evaluation fingerprint"
    );
    assert_ne!(
        original.plan.canonical_bytes(),
        changed.plan.canonical_bytes(),
        "changing a factory argument must change the frozen plan"
    );
    assert_ne!(
        original.plan.derivation_id(),
        changed.plan.derivation_id(),
        "changing a factory argument must change derivation identity"
    );
    assert_locked_request_origin(
        &changed.plan,
        "pkgconfig(openssl)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 1,
        },
    );
    assert!(
        changed
            .plan
            .build_lock
            .requests
            .iter()
            .all(|request| request.request != "pkgconfig(libressl)"),
        "the old override must not survive in the changed frozen closure"
    );
}

#[test]
fn checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks() {
    let matrix = PackageExampleMatrix::new();
    let repository_uri = Url::from_file_path(&matrix.repository_index).unwrap().to_string();

    for example in &matrix.examples {
        let evaluated = matrix.builder(example);
        let first = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{}: freeze example plan: {error:#}", example.name));
        first
            .plan
            .validate()
            .unwrap_or_else(|error| panic!("{}: validate frozen example plan: {error:#}", example.name));
        assert_eq!(
            first.plan.provenance.recipe, evaluated.recipe.fingerprint,
            "{}: the frozen plan must retain the exact public recipe evaluation fingerprint",
            example.name
        );
        assert_documented_factory_semantics(&example.name, &evaluated.recipe.declaration, &first.plan);
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

    assert_factory_override_changes_frozen_identity(&matrix);
}

#[test]
fn checked_in_metadata_only_example_fails_closed_before_execution() {
    let matrix = PackageExampleMatrix::new();
    let example = matrix
        .examples
        .iter()
        .find(|example| example.name == "minimal")
        .expect("the explicitly inventoried example matrix contains minimal");

    let first = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir).unwrap();
    assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
    assert!(
        first.plan.sources.is_empty(),
        "minimal must remain a source-less execution fixture"
    );
    assert!(
        first
            .plan
            .jobs
            .iter()
            .flat_map(|job| &job.phases)
            .all(|phase| { phase.pre.is_empty() && phase.steps.is_empty() && phase.post.is_empty() }),
        "minimal must isolate frozen-root verification without invoking package build steps"
    );
    assert!(
        first
            .plan
            .build_lock
            .packages
            .iter()
            .all(|package| package.package_id.len() == 64
                && package
                    .package_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
        "the frozen runtime closure must use the real SHA-256 identities of local Stone artifacts"
    );
    let derivation_id = first.plan.derivation_id();

    assert_runtime_reopens_planner_repository_snapshot(
        &matrix.forge_dir,
        &matrix.output_dir,
        matrix.builder(example).repositories().clone(),
        &first,
    );

    let error = execute_and_publish(&first)
        .expect_err("metadata-only providers must never satisfy a frozen executable binding");
    let error = error_chain(error.as_ref());
    assert!(
        error.contains("frozen executable provider") && error.contains("has no regular layout entry"),
        "metadata-only closure must fail at the exact executable boundary, got: {error}"
    );
    let published_root = matrix.output_dir.join(derivation_id.as_str());
    assert!(
        !published_root.exists(),
        "an unauthenticated metadata-only closure must not publish a derivation"
    );
}

#[test]
fn frozen_execution_capability_skip_never_hides_payload_or_ambiguous_nix_failures() {
    let missing_delegation = crate::container::Error::FrozenCgroupDelegationRequired {
        current: PathBuf::from("/user.slice/session.scope"),
    };
    assert!(container_capability_unavailable(&missing_delegation));
    let malformed_delegation = crate::container::Error::MalformedCurrentCgroup {
        reason: "duplicate unified entry",
    };
    assert!(!container_capability_unavailable(&malformed_delegation));

    for source in [
        nix::errno::Errno::EPERM,
        nix::errno::Errno::EACCES,
        nix::errno::Errno::ENOSYS,
    ] {
        let namespace = crate::container::Error::Container(::container::Error::CloneNamespaces { source });
        assert!(container_capability_unavailable(&namespace));
    }
    let namespace_resource_exhaustion = crate::container::Error::Container(::container::Error::CloneNamespaces {
        source: nix::errno::Errno::EAGAIN,
    });
    assert!(!container_capability_unavailable(&namespace_resource_exhaustion));

    for terminal in [
        "EPERM: Operation not permitted",
        "EACCES: Permission denied",
        "ENOSYS: Function not implemented",
    ] {
        let setup = crate::container::Error::Container(::container::Error::Failure {
            message: format!("mount /work: {terminal}"),
        });
        assert!(container_capability_unavailable(&setup));
    }

    for message in [
        "mount /work/EPERM: EIO: Input/output error",
        "mount /work/EACCES: unrelated failure",
        "mount /work/ENOSYS: operation failed",
        "mount /work: Operation not permitted",
        "clear inherited supplementary groups: permission denied by payload text",
        "restrict payload scheduler to the fair class: EPERM: Operation not permitted",
        "drop all payload capabilities: EPERM: Operation not permitted",
        "install mandatory payload seccomp policy: EACCES: Permission denied",
    ] {
        let injected = crate::container::Error::Container(::container::Error::Failure {
            message: message.to_owned(),
        });
        assert!(
            !container_capability_unavailable(&injected),
            "diagnostic text must not classify {message:?} as a host capability denial"
        );
    }

    let payload = crate::container::Error::Container(::container::Error::Failure {
        message: "run: package frozen example: permission denied".to_owned(),
    });
    assert!(!container_capability_unavailable(&payload));

    let ambiguous = crate::container::Error::Container(::container::Error::Nix {
        source: nix::errno::Errno::EPERM,
    });
    assert!(!container_capability_unavailable(&ambiguous));

    let child_cleanup = crate::container::Error::Container(::container::Error::ChildCleanup {
        cleanup: std::io::Error::other("EPERM: Operation not permitted"),
        pidfd: None,
    });
    assert!(!container_capability_unavailable(&child_cleanup));

    // Even a setup-shaped primary plus a permission-shaped cleanup diagnostic
    // remains a typed post-clone lifecycle violation. The display-string
    // fallback must never override that typed classification.
    let child_cleanup_after_setup = crate::container::Error::Container(::container::Error::ChildCleanupAfterFailure {
        primary: Box::new(::container::Error::Failure {
            message: "mount /work: EIO: Input/output error".to_owned(),
        }),
        cleanup: std::io::Error::other("EPERM: Operation not permitted"),
        pidfd: None,
    });
    assert!(!container_capability_unavailable(&child_cleanup_after_setup));
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
