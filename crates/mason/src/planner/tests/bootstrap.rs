// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use forge::package::Meta;
use fs_err as fs;
use gluon_config::{Evaluator, Source};
use sha2::{Digest, Sha256};
use stone::{StoneDecodeLimits, StoneDecodedPayload, StoneHeader, StoneHeaderV1FileType};
use url::Url;

use super::{
    EXECUTION_FIXTURES, Env, Publication, Request, SOURCE_DATE_EPOCH, TARGET, WriteOutcome,
    container_capability_unavailable, copy_package_directory, encode_build_lock, error_chain, execute_and_publish,
    execution_capability_required, plan_for_build, profile,
};

#[path = "bootstrap/bundle.rs"]
mod bundle;

const BOOTSTRAP_SCHEMA_VERSION: i64 = 1;
const MAX_BOOTSTRAP_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BOOTSTRAP_PACKAGE_COUNT: usize = 512;
const MAX_BOOTSTRAP_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const BOOTSTRAP_PROFILE: &str = "planner-contentful-bootstrap";
const REQUIRED_EXECUTION_FIXTURES: [&str; 6] = ["autotools", "cargo", "cmake", "custom", "meson", "split"];

#[derive(Debug, PartialEq, Eq)]
enum FrozenStepShape {
    Run {
        program: String,
        first_argument: Option<String>,
    },
    Shell {
        interpreter: String,
        declared_programs: Vec<String>,
        script: String,
    },
}

fn step_shape(step: &stone_recipe::derivation::StepPlan) -> FrozenStepShape {
    match step {
        stone_recipe::derivation::StepPlan::Run { program, args, .. } => FrozenStepShape::Run {
            program: program.path.clone(),
            first_argument: args.first().cloned(),
        },
        stone_recipe::derivation::StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            ..
        } => FrozenStepShape::Shell {
            interpreter: interpreter.path.clone(),
            declared_programs: declared_programs.iter().map(|program| program.path.clone()).collect(),
            script: script.clone(),
        },
    }
}

fn run(program: &str, first_argument: &str) -> FrozenStepShape {
    FrozenStepShape::Run {
        program: format!("/usr/bin/{program}"),
        first_argument: Some(first_argument.to_owned()),
    }
}

fn assert_execution_fixture_topology(name: &str, plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(EXECUTION_FIXTURES, REQUIRED_EXECUTION_FIXTURES);
    let [job] = plan.jobs.as_slice() else {
        panic!("{name}: execution fixture must freeze exactly one non-PGO job");
    };
    assert_eq!(job.pgo_stage, None, "{name}: unexpected PGO stage");
    assert_eq!(job.pgo_dir, None, "{name}: unexpected PGO directory");

    let prepare = vec![run("mkdir", "-p"), run("bsdtar-static", "xf")];
    let expected = match name {
        "cmake" => vec![
            ("Prepare", prepare),
            ("Setup", vec![run("cmake", "-G")]),
            ("Build", vec![run("cmake", "--build")]),
            ("Install", vec![run("cmake", "--install")]),
            ("Check", vec![run("ctest", "--test-dir")]),
        ],
        "split" => vec![
            ("Prepare", prepare),
            ("Setup", vec![run("cmake", "-G")]),
            ("Build", vec![run("cmake", "--build")]),
            ("Install", vec![run("cmake", "--install")]),
            ("Check", vec![run("ctest", "--test-dir")]),
        ],
        "meson" => vec![
            ("Prepare", prepare),
            ("Setup", vec![run("meson", "setup")]),
            ("Build", vec![run("meson", "compile")]),
            ("Install", vec![run("meson", "install")]),
            ("Check", vec![run("meson", "test")]),
        ],
        "cargo" => vec![
            ("Prepare", prepare),
            ("Build", vec![run("cargo", "build")]),
            ("Install", vec![run("install", "-Dm00755")]),
            ("Check", vec![run("cargo", "test")]),
        ],
        "autotools" => vec![
            ("Prepare", prepare),
            ("Setup", vec![run("dash", "./configure")]),
            ("Build", vec![run("make", "VERBOSE=1")]),
            ("Install", vec![run("make", "install")]),
            ("Check", vec![run("make", "check")]),
        ],
        "custom" => vec![
            ("Prepare", prepare),
            ("Setup", vec![run("mkdir", "-p")]),
            ("Build", vec![run("cp", "payload.txt")]),
            (
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: r#"install -Dm644 build/payload.txt "${CAST_INSTALL_ROOT}${CAST_DATADIR}/cast-custom-fixture/payload.txt""#
                        .to_owned(),
                }],
            ),
            ("Check", vec![run("cmp", "payload.txt")]),
        ],
        other => panic!("unexpected execution fixture {other:?}"),
    };

    let actual = job
        .phases
        .iter()
        .map(|phase| {
            assert!(phase.pre.is_empty(), "{name}/{}: unexpected pre-hook", phase.name);
            assert!(phase.post.is_empty(), "{name}/{}: unexpected post-hook", phase.name);
            assert!(!phase.steps.is_empty(), "{name}/{}: empty frozen phase", phase.name);
            (
                phase.name.as_str(),
                phase.steps.iter().map(step_shape).collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{name}: frozen builder phase topology drifted");
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct BootstrapClosure {
    schema_version: i64,
    repository: RepositoryPin,
    fixtures: Vec<String>,
    packages: PackageSet,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct RepositoryPin {
    base_uri: String,
    channel: String,
    version: String,
    architecture: String,
    index: IndexPin,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct IndexPin {
    sha256: String,
    size: i64,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct PackageSet {
    total_download_bytes: i64,
    sha256: Vec<String>,
}

fn bootstrap_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution/bootstrap")
}

fn package_store() -> PathBuf {
    std::env::var_os("CAST_BOOTSTRAP_PACKAGE_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bootstrap-fixtures/packages"))
}

fn load_bootstrap_closure() -> BootstrapClosure {
    let path = bootstrap_root().join("closure.glu");
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(
        metadata.file_type().is_file(),
        "bootstrap closure must be a regular file"
    );
    assert!(
        metadata.len() <= 64 * 1024,
        "bootstrap closure exceeds its test boundary"
    );
    let source = std::fs::read_to_string(&path).unwrap();
    Evaluator::default()
        .evaluate::<BootstrapClosure>(&Source::new("bootstrap/closure.glu", &source))
        .unwrap()
        .value
}

fn validate_sha256(value: &str, field: &str) {
    assert_eq!(value.len(), 64, "{field} must contain one SHA-256 digest");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{field} must be lowercase hexadecimal"
    );
}

fn index_url(repository: &RepositoryPin) -> Url {
    let base = Url::parse(&repository.base_uri).unwrap();
    assert_eq!(
        base.scheme(),
        "https",
        "bootstrap repository transport must remain HTTPS"
    );
    assert!(base.username().is_empty() && base.password().is_none());
    assert!(base.query().is_none() && base.fragment().is_none());
    assert!(
        !repository.channel.is_empty()
            && repository
                .channel
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "bootstrap repository channel is not one path component"
    );
    let history = repository
        .version
        .strip_prefix("history/")
        .expect("bootstrap repository version must pin one immutable history");
    assert!(
        !history.is_empty() && history.bytes().all(|byte| byte.is_ascii_digit()),
        "bootstrap history identifier must be decimal"
    );
    assert_eq!(repository.architecture, "x86_64");

    base.join(&format!(
        "{}/history/{history}/{}/stone.index",
        repository.channel, repository.architecture
    ))
    .unwrap()
}

fn package_url(repository: &RepositoryPin, raw_uri: &str) -> Url {
    assert!(!raw_uri.is_empty(), "bootstrap package URI is empty");
    assert!(!raw_uri.starts_with('/'), "bootstrap package URI must be relative");
    assert!(
        !raw_uri.bytes().any(|byte| matches!(byte, b'?' | b'#' | b'\\')),
        "bootstrap package URI contains controls"
    );
    assert!(
        !raw_uri.as_bytes().windows(3).any(|window| {
            if window[0] != b'%' {
                return false;
            }
            let Some(high) = (window[1] as char).to_digit(16) else {
                return false;
            };
            let Some(low) = (window[2] as char).to_digit(16) else {
                return false;
            };
            matches!(((high << 4) | low) as u8, b'.' | b'/' | b'\\' | 0)
        }),
        "bootstrap package URI contains encoded path controls"
    );
    let index = index_url(repository);
    let resolved = index.join(raw_uri).unwrap();
    let base = Url::parse(&repository.base_uri).unwrap();
    assert_eq!(resolved.scheme(), "https");
    assert_eq!(resolved.host_str(), base.host_str());
    assert_eq!(resolved.port_or_known_default(), base.port_or_known_default());
    assert!(resolved.username().is_empty() && resolved.password().is_none());
    assert!(resolved.query().is_none() && resolved.fragment().is_none());
    assert!(
        resolved.path().starts_with(&format!("/{}/", repository.channel)),
        "bootstrap package URI escaped the repository channel capability"
    );
    resolved
}

fn repository_index_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 8_192,
        max_records_per_payload: 512,
        max_record_bytes: 64 * 1024,
        max_stored_payload_bytes: 64 * 1024,
        max_plain_payload_bytes: 256 * 1024,
        max_total_records: 262_144,
        max_total_record_bytes: 16 * 1024 * 1024,
        max_total_stored_bytes: 8 * 1024 * 1024,
        max_total_plain_bytes: 16 * 1024 * 1024,
        max_zstd_window_log: 20,
    }
}

fn indexed_packages(index_bytes: &[u8]) -> BTreeMap<String, Meta> {
    let mut reader = stone::read_bytes_with_limits(index_bytes, repository_index_limits()).unwrap();
    assert!(matches!(
        reader.header,
        StoneHeader::V1(header) if header.file_type == StoneHeaderV1FileType::Repository
    ));

    let mut packages = BTreeMap::new();
    for (index, payload) in reader.payloads().unwrap().enumerate() {
        let payload = payload.unwrap();
        let StoneDecodedPayload::Meta(payload) = payload else {
            panic!("bootstrap index payload {index} is not metadata");
        };
        let meta = Meta::from_stone_payload(&payload.body).unwrap();
        let hash = meta.hash.clone().expect("repository package has no SHA-256");
        validate_sha256(&hash, "repository package hash");
        assert!(
            packages.insert(hash.clone(), meta).is_none(),
            "bootstrap index repeats package identity {hash}"
        );
    }
    packages
}

fn package_file_matches(path: &Path, expected_hash: &str, expected_size: u64) -> bool {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => panic!("inspect cached bootstrap package {path:?}: {error}"),
    };
    if !metadata.file_type().is_file() || metadata.len() != expected_size {
        return false;
    }
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return false,
    };
    let probe = expected_size.saturating_add(1);
    let mut limited = file.by_ref().take(probe);
    let actual_hash = forge::util::sha256_hash(&mut limited).unwrap();
    let copied = probe.saturating_sub(limited.limit());
    copied == expected_size && actual_hash == expected_hash
}

fn validated_bootstrap() -> (BootstrapClosure, BTreeMap<String, Meta>) {
    let closure = load_bootstrap_closure();
    assert_eq!(closure.schema_version, BOOTSTRAP_SCHEMA_VERSION);
    assert_eq!(
        closure.fixtures.iter().map(String::as_str).collect::<Vec<_>>(),
        EXECUTION_FIXTURES
    );
    assert_eq!(
        index_url(&closure.repository).as_str(),
        "https://cdn.aerynos.dev/main/history/1783706384/x86_64/stone.index"
    );

    let index_size = u64::try_from(closure.repository.index.size).unwrap();
    assert!(index_size <= MAX_BOOTSTRAP_INDEX_BYTES);
    validate_sha256(&closure.repository.index.sha256, "repository index hash");

    let index_path = bootstrap_root().join("stone.index");
    let metadata = std::fs::symlink_metadata(&index_path).unwrap();
    assert!(metadata.file_type().is_file(), "pinned index must be a regular file");
    assert_eq!(metadata.len(), index_size);
    let index_bytes = std::fs::read(index_path).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&index_bytes)),
        closure.repository.index.sha256
    );
    let indexed = indexed_packages(&index_bytes);

    assert!(!closure.packages.sha256.is_empty());
    assert!(closure.packages.sha256.len() <= MAX_BOOTSTRAP_PACKAGE_COUNT);
    assert!(
        closure.packages.sha256.is_sorted(),
        "bootstrap package identities must be canonical"
    );
    assert_eq!(
        closure.packages.sha256.iter().collect::<BTreeSet<_>>().len(),
        closure.packages.sha256.len(),
        "bootstrap package identities must be unique"
    );

    let mut names = BTreeSet::new();
    let total_download_bytes = closure
        .packages
        .sha256
        .iter()
        .map(|hash| {
            validate_sha256(hash, "bootstrap package hash");
            let meta = indexed
                .get(hash)
                .unwrap_or_else(|| panic!("bootstrap package {hash} is absent from the pinned index"));
            assert_eq!(meta.architecture, closure.repository.architecture);
            let uri = meta.uri.as_deref().expect("bootstrap package has no URI");
            package_url(&closure.repository, uri);
            names.insert(meta.name.to_string());
            meta.download_size.expect("bootstrap package has no declared size")
        })
        .try_fold(0u64, |total, size| total.checked_add(size))
        .expect("bootstrap package byte total overflowed");
    assert!(total_download_bytes <= MAX_BOOTSTRAP_DOWNLOAD_BYTES);
    assert_eq!(
        total_download_bytes,
        u64::try_from(closure.packages.total_download_bytes).unwrap()
    );

    for required in [
        "autoconf",
        "automake",
        "bsdtar-static",
        "clang",
        "cmake",
        "dash",
        "make",
        "meson",
        "ninja",
        "pkgconf",
        "python",
        "rust",
    ] {
        assert!(
            names.contains(required),
            "bootstrap closure is missing real package `{required}`"
        );
    }

    (closure, indexed)
}

struct BootstrapPlanningMatrix {
    _root: tempfile::TempDir,
    cache_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    forge_dir: PathBuf,
    output_dir: PathBuf,
    mirror_dir: PathBuf,
    index_uri: String,
    recipes: Vec<(String, PathBuf)>,
}

impl BootstrapPlanningMatrix {
    fn new(closure: &BootstrapClosure) -> Self {
        let root = crate::private_tempdir();
        let cache_dir = root.path().join("cache");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let forge_dir = root.path().join("forge");
        let output_dir = root.path().join("output");
        let recipes_dir = root.path().join("recipes");
        let mirror = root.path().join("mirror");
        let history = closure
            .repository
            .version
            .strip_prefix("history/")
            .expect("validated bootstrap version is an immutable history");
        let history_dir = mirror
            .join(&closure.repository.channel)
            .join("history")
            .join(history)
            .join(&closure.repository.architecture);

        fs::create_dir_all(data_dir.join("policy")).unwrap();
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipes_dir).unwrap();
        fs::create_dir_all(&history_dir).unwrap();
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(
            data_dir.join("policy/policy.glu"),
            include_str!("../../../data/policy/policy.glu"),
        )
        .unwrap();
        fs::write(
            data_dir.join("policy/default.glu"),
            include_str!("../../../data/policy/default.glu"),
        )
        .unwrap();

        let index_path = history_dir.join("stone.index");
        fs::copy(bootstrap_root().join("stone.index"), &index_path).unwrap();
        fs::write(
            mirror
                .join(&closure.repository.channel)
                .join(forge::repository::ROOT_INDEX_WIRE_FILENAME),
            format!(
                concat!(
                    "{{\n",
                    "  \"formats\": {{ \"v0\": {{}} }},\n",
                    "  \"streams\": {{}},\n",
                    "  \"tags\": {{}},\n",
                    "  \"history\": {{ \"{}\": {{ \"format\": \"v0\" }} }}\n",
                    "}}\n"
                ),
                history
            ),
        )
        .unwrap();

        let mirror_uri = Url::from_directory_path(&mirror).unwrap();
        fs::write(
            config_dir.join("profile.d/bootstrap.glu"),
            format!(
                r#"let cast = import! cast.profile.v1

cast.profiles [
    cast.profile "{BOOTSTRAP_PROFILE}" [
        cast.repository.root_index_with {{
            id = "bootstrap",
            description = cast.optional.some "Pinned contentful execution bootstrap",
            base_uri = "{mirror_uri}",
            channel = cast.optional.some "{}",
            version = "{}",
            arch = cast.optional.some "{}",
            priority = cast.optional.some 0,
            enabled = cast.optional.some cast.boolean.true,
        }},
    ],
]
"#,
                closure.repository.channel, closure.repository.version, closure.repository.architecture
            ),
        )
        .unwrap();

        let authored_packages = bootstrap_root().parent().unwrap().join("packages");
        let recipes = EXECUTION_FIXTURES
            .iter()
            .map(|name| {
                let recipe_dir = recipes_dir.join(name);
                copy_package_directory(&authored_packages.join(name), &recipe_dir);
                ((*name).to_owned(), recipe_dir.join("stone.glu"))
            })
            .collect();

        Self {
            _root: root,
            cache_dir,
            config_dir,
            data_dir,
            forge_dir,
            output_dir,
            mirror_dir: mirror,
            index_uri: Url::from_file_path(index_path).unwrap().to_string(),
            recipes,
        }
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

    fn request(&self, recipe: &Path, update_lock: bool) -> Request {
        Request {
            recipe: recipe.to_owned(),
            profile: profile::Id::new(BOOTSTRAP_PROFILE),
            target: TARGET.to_owned(),
            source_date_epoch: SOURCE_DATE_EPOCH,
            build_release: std::num::NonZeroU64::new(1).unwrap(),
            jobs: std::num::NonZeroU32::new(1).unwrap(),
            compiler_cache: false,
            update_lock,
            refresh_repositories: update_lock,
        }
    }

    fn materialize_package_pool(&self, closure: &BootstrapClosure, indexed: &BTreeMap<String, Meta>) {
        let store = package_store();
        let channel_prefix = format!("/{}/", closure.repository.channel);

        for hash in &closure.packages.sha256 {
            let meta = indexed
                .get(hash)
                .unwrap_or_else(|| panic!("bootstrap package {hash} is absent from the pinned index"));
            let size = meta.download_size.expect("bootstrap package has no declared size");
            let source = store.join(format!("{hash}.stone"));
            assert!(
                package_file_matches(&source, hash, size),
                "run `make bootstrap-fixtures`: contentful package {} is absent or corrupt",
                meta.name
            );
            let url = package_url(
                &closure.repository,
                meta.uri.as_deref().expect("bootstrap package has no URI"),
            );
            let relative = url
                .path()
                .strip_prefix(&channel_prefix)
                .expect("validated package URL is beneath the repository channel");
            let destination = self.mirror_dir.join(&closure.repository.channel).join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            // Use the std syscall wrapper here: fs_err decorates the error and
            // can hide EXDEV from raw_os_error(), preventing the bounded copy
            // fallback when the package store and test root are on different
            // filesystems.
            match std::fs::hard_link(&source, &destination) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(nix::libc::EXDEV) => {
                    fs::copy(&source, &destination).unwrap();
                }
                Err(error) => panic!("publish bootstrap mirror package {}: {error}", meta.name),
            }
            assert!(
                package_file_matches(&destination, hash, size),
                "materialized mirror package {} failed exact re-verification",
                meta.name
            );
        }
    }

    fn import_sources(&self, sources: &[stone_recipe::derivation::LockedSource]) {
        let archives = bootstrap_root().parent().unwrap().join("archives");
        for source in sources {
            let stone_recipe::derivation::LockedSource::Archive { url, .. } = source else {
                panic!("execution fixtures must remain archive-only");
            };
            let source_url = Url::parse(url).unwrap();
            let filename = source_url
                .path_segments()
                .and_then(Iterator::last)
                .expect("locked execution archive URL has a basename")
                .to_owned();
            crate::upstream::import_locked_archive_fixture(source, &self.cache_dir, &archives.join(&filename))
                .unwrap_or_else(|error| panic!("import offline execution source {filename}: {error:#}"));
        }
    }
}

#[test]
#[ignore = "requires the package store prepared by make bootstrap-fixtures"]
fn contentful_bootstrap_materializes_a_complete_offline_root_mirror() {
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
}

#[test]
#[ignore = "requires make bootstrap-fixtures and unprivileged user/mount namespaces"]
fn all_execution_fixtures_build_package_and_reproduce_from_the_contentful_closure() {
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
    let mut executed = 0usize;

    for (name, recipe) in &matrix.recipes {
        let first = plan_for_build(matrix.env(), matrix.request(recipe, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: plan contentful execution: {error:#}"));
        assert_execution_fixture_topology(name, &first.plan);
        matrix.import_sources(&first.plan.sources);
        let canonical_plan = first.plan.canonical_bytes();
        let derivation_id = first.plan.derivation_id();

        let first_publication = match execute_and_publish(&first) {
            Ok(publication) => publication,
            Err(error)
                if container_capability_unavailable(error.as_ref())
                    && !execution_capability_required()
                    && executed == 0 =>
            {
                eprintln!(
                    "skipping all contentful execution fixtures: this host cannot create the required user/mount namespaces: {}",
                    error_chain(error.as_ref())
                );
                return;
            }
            Err(error) => panic!(
                "{name}: contentful execution failed after successful planning: {}",
                error_chain(error.as_ref())
            ),
        };
        assert_eq!(first_publication, Publication::Published, "{name}: first publication");
        executed += 1;

        let published_root = matrix.output_dir.join(derivation_id.as_str());
        let published = bundle::assert_fixture_bundle(name, &first, &published_root, bundle::BundleRootRole::Published);

        let locked = plan_for_build(matrix.env(), matrix.request(recipe, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: reuse contentful build lock: {error:#}"));
        assert_eq!(
            locked.lock_outcome, None,
            "{name}: reuse must not rewrite build.lock.glu"
        );
        assert_eq!(
            locked.plan.canonical_bytes(),
            canonical_plan,
            "{name}: canonical plan drift"
        );
        assert_eq!(
            locked.plan.derivation_id(),
            derivation_id,
            "{name}: derivation ID drift"
        );

        let second_publication = execute_and_publish(&locked).unwrap_or_else(|error| {
            panic!(
                "{name}: repeated contentful execution failed: {}",
                error_chain(error.as_ref())
            )
        });
        assert_eq!(second_publication, Publication::Reused, "{name}: repeated publication");
        let repeated = bundle::assert_fixture_bundle(
            name,
            &locked,
            &locked.runtime.paths.artefacts().host,
            bundle::BundleRootRole::Staged,
        );
        assert_eq!(repeated, published, "{name}: repeated build changed emitted bytes");
        let preserved =
            bundle::assert_fixture_bundle(name, &locked, &published_root, bundle::BundleRootRole::Published);
        assert_eq!(preserved, published, "{name}: published generation changed");
    }

    assert_eq!(executed, REQUIRED_EXECUTION_FIXTURES.len());
}

#[test]
fn all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure() {
    let (closure, _) = validated_bootstrap();
    let expected_packages = closure.packages.sha256.iter().cloned().collect::<BTreeSet<_>>();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    let mut resolved_packages = BTreeSet::new();

    for (name, recipe) in &matrix.recipes {
        let first = plan_for_build(matrix.env(), matrix.request(recipe, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: plan with pinned contentful bootstrap: {error:#}"));
        first
            .plan
            .validate()
            .unwrap_or_else(|error| panic!("{name}: validate contentful plan: {error:#}"));
        assert_execution_fixture_topology(name, &first.plan);
        assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
        assert_eq!(first.plan.build_lock.repositories.len(), 1);
        let repository = &first.plan.build_lock.repositories[0];
        assert_eq!(repository.id, "bootstrap");
        assert_eq!(repository.index_uri, matrix.index_uri);
        assert_eq!(repository.snapshot, closure.repository.index.sha256);
        assert!(
            first
                .plan
                .build_lock
                .packages
                .iter()
                .all(|package| package.repository == "bootstrap" && !package.name.starts_with("planner-provider-")),
            "{name}: a synthetic metadata-only provider entered the real closure"
        );
        resolved_packages.extend(
            first
                .plan
                .build_lock
                .packages
                .iter()
                .map(|package| package.package_id.clone()),
        );

        let canonical_plan = first.plan.canonical_bytes();
        let derivation_id = first.plan.derivation_id();
        let lock_bytes = fs::read(&first.lock_path).unwrap();
        assert_eq!(lock_bytes, encode_build_lock(&first.plan.build_lock).into_bytes());
        let locked = plan_for_build(matrix.env(), matrix.request(recipe, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: reuse contentful build lock: {error:#}"));
        assert_eq!(locked.lock_outcome, None);
        assert_eq!(locked.plan.canonical_bytes(), canonical_plan);
        assert_eq!(locked.plan.derivation_id(), derivation_id);
        assert_eq!(fs::read(&locked.lock_path).unwrap(), lock_bytes);
    }

    if resolved_packages != expected_packages {
        let missing_from_manifest = resolved_packages
            .difference(&expected_packages)
            .cloned()
            .collect::<Vec<_>>();
        let unused_in_manifest = expected_packages
            .difference(&resolved_packages)
            .cloned()
            .collect::<Vec<_>>();
        panic!(
            "the six real execution plans differ from the declarative bootstrap closure; \
             missing_from_manifest={missing_from_manifest:?}, unused_in_manifest={unused_in_manifest:?}"
        );
    }
}

#[test]
#[ignore = "explicit network preparation for the offline bootstrap package store"]
fn fetch_pinned_bootstrap_package_files() {
    let (closure, indexed) = validated_bootstrap();
    let store = package_store();
    std::fs::create_dir_all(&store).unwrap();

    for (position, hash) in closure.packages.sha256.iter().enumerate() {
        let meta = indexed
            .get(hash)
            .unwrap_or_else(|| panic!("bootstrap package {hash} is absent from the pinned index"));
        let size = meta.download_size.expect("bootstrap package has no declared size");
        let destination = store.join(format!("{hash}.stone"));
        if package_file_matches(&destination, hash, size) {
            eprintln!(
                "bootstrap package {}/{} is already verified",
                position + 1,
                closure.packages.sha256.len()
            );
            continue;
        }
        let url = package_url(
            &closure.repository,
            meta.uri.as_deref().expect("bootstrap package has no URI"),
        );
        eprintln!(
            "fetching bootstrap package {}/{}: {} ({size} bytes)",
            position + 1,
            closure.packages.sha256.len(),
            meta.name
        );
        forge::runtime::block_on(forge::request::download_with_progress_and_expected_sha256_and_limits(
            url,
            &destination,
            hash,
            forge::request::DownloadLimits::new(size, Duration::from_secs(5 * 60)),
            |_| {},
        ))
        .unwrap_or_else(|error| panic!("fetch bootstrap package {}: {error:#}", meta.name));
        assert!(
            package_file_matches(&destination, hash, size),
            "downloaded bootstrap package {} did not survive exact re-verification",
            meta.name
        );
    }
}

#[test]
fn pinned_bootstrap_manifest_is_bounded_and_index_authoritative() {
    let _ = validated_bootstrap();
}
