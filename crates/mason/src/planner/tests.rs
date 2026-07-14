// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

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
    UpstreamSpec,
    derivation::{FilesystemPolicy, InputOrigin, NetworkMode, encode_build_lock},
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
const EXECUTION_FIXTURES: [&str; 9] = [
    "autotools",
    "cargo",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "hooks-patch",
    "meson",
    "split",
];

#[path = "tests/bootstrap.rs"]
mod bootstrap;

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
        [6, 1, 1, 1],
        "execution fixtures must cover six plain tar streams plus one each of gzip, XZ, and Zstandard"
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
        "drop payload mount-administration capability",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix));
    let permission_failure = message.contains("Operation not permitted")
        || message.contains("Permission denied")
        || message.contains("Function not implemented")
        || message.contains("EPERM")
        || message.contains("EACCES")
        || message.contains("ENOSYS");
    setup_failure && permission_failure
}

fn container_capability_unavailable(error: &(dyn StdError + 'static)) -> bool {
    // `thiserror` may make a transparent wrapper's concrete inner error
    // unavailable to `downcast_ref`, but its exact display remains in the
    // source chain. Accept only known setup labels, never `run: ...` payload
    // failures.
    if setup_capability_denial(&error.to_string()) {
        return true;
    }
    if let Some(error) = error.downcast_ref::<::container::Error>() {
        return match error {
            // The container crate currently groups clone, pipe, signal, and
            // wait errors into this variant. An errno alone therefore does
            // not prove namespace capability unavailability.
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
    let setup = crate::container::Error::Container(::container::Error::Failure {
        message: "clear inherited supplementary groups: EPERM: Operation not permitted".to_owned(),
    });
    assert!(container_capability_unavailable(&setup));

    let payload = crate::container::Error::Container(::container::Error::Failure {
        message: "run: package frozen example: permission denied".to_owned(),
    });
    assert!(!container_capability_unavailable(&payload));

    let ambiguous = crate::container::Error::Container(::container::Error::Nix {
        source: nix::errno::Errno::EPERM,
    });
    assert!(!container_capability_unavailable(&ambiguous));
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
