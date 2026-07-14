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
    EXECUTION_FIXTURES, Env, Planned, Publication, Request, SOURCE_DATE_EPOCH, TARGET, WriteOutcome,
    container_capability_unavailable, copy_package_directory, encode_build_lock, error_chain, execute_and_publish,
    execution_capability_required, plan_for_build, profile,
};

#[path = "bootstrap/bundle.rs"]
mod bundle;

const BOOTSTRAP_SCHEMA_VERSION: i64 = 2;
const MAX_BOOTSTRAP_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BOOTSTRAP_PACKAGE_COUNT: usize = 512;
const MAX_BOOTSTRAP_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const BOOTSTRAP_PROFILE: &str = "planner-contentful-bootstrap";
const REQUIRED_EXECUTION_FIXTURES: [&str; 10] = [
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
const EXECUTION_FIXTURE_SELECTOR_ENV: &str = "CAST_EXECUTION_FIXTURE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionFixtureSelection {
    All,
    One(&'static str),
}

impl ExecutionFixtureSelection {
    fn includes(self, fixture: &str) -> bool {
        match self {
            Self::All => true,
            Self::One(selected) => selected == fixture,
        }
    }

    fn expected_count(self) -> usize {
        match self {
            Self::All => REQUIRED_EXECUTION_FIXTURES.len(),
            Self::One(_) => 1,
        }
    }
}

fn parse_execution_fixture_selection(value: Option<&str>) -> Result<ExecutionFixtureSelection, String> {
    let value = value.unwrap_or("all");
    if value == "all" {
        return Ok(ExecutionFixtureSelection::All);
    }
    if let Some(fixture) = REQUIRED_EXECUTION_FIXTURES
        .iter()
        .copied()
        .find(|fixture| *fixture == value)
    {
        return Ok(ExecutionFixtureSelection::One(fixture));
    }
    Err(format!(
        "{EXECUTION_FIXTURE_SELECTOR_ENV} must be `all` or exactly one of {}; got {value:?}",
        REQUIRED_EXECUTION_FIXTURES.join(", ")
    ))
}

fn execution_fixture_selection_from_env() -> Result<ExecutionFixtureSelection, String> {
    let Some(value) = std::env::var_os(EXECUTION_FIXTURE_SELECTOR_ENV) else {
        return parse_execution_fixture_selection(None);
    };
    let value = value.to_str().ok_or_else(|| {
        format!("{EXECUTION_FIXTURE_SELECTOR_ENV} must contain valid UTF-8 and name exactly one fixture or `all`")
    })?;
    parse_execution_fixture_selection(Some(value))
}

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
    ExtractArchive {
        source: u32,
        destination: String,
        strip_components: u32,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct FrozenPhaseShape {
    name: String,
    pre: Vec<FrozenStepShape>,
    steps: Vec<FrozenStepShape>,
    post: Vec<FrozenStepShape>,
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
        stone_recipe::derivation::StepPlan::ExtractArchive {
            source,
            destination,
            strip_components,
        } => FrozenStepShape::ExtractArchive {
            source: *source,
            destination: destination.clone(),
            strip_components: *strip_components,
        },
    }
}

fn extract(destination: &str) -> FrozenStepShape {
    FrozenStepShape::ExtractArchive {
        source: 0,
        destination: destination.to_owned(),
        strip_components: 1,
    }
}

fn phase(name: &str, steps: Vec<FrozenStepShape>) -> FrozenPhaseShape {
    FrozenPhaseShape {
        name: name.to_owned(),
        pre: Vec::new(),
        steps,
        post: Vec::new(),
    }
}

fn phase_with_pre(name: &str, pre: Vec<FrozenStepShape>, steps: Vec<FrozenStepShape>) -> FrozenPhaseShape {
    FrozenPhaseShape {
        name: name.to_owned(),
        pre,
        steps,
        post: Vec::new(),
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

    let prepare = |destination: &str| phase("Prepare", vec![extract(destination)]);
    let expected = match name {
        "cmake" | "factory-override" => vec![
            prepare(if name == "cmake" {
                "cast-cmake-fixture"
            } else {
                "cast-factory-override-fixture"
            }),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "split" => vec![
            prepare("cast-split-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "meson" => vec![
            prepare("cast-meson-fixture"),
            phase("Setup", vec![run("meson", "setup")]),
            phase("Build", vec![run("meson", "compile")]),
            phase("Install", vec![run("meson", "install")]),
            phase("Check", vec![run("meson", "test")]),
        ],
        "cargo" | "cargo-vendored" => vec![
            prepare(if name == "cargo" {
                "cast-cargo-fixture"
            } else {
                "cast-cargo-vendored-fixture"
            }),
            phase("Build", vec![run("cargo", "build")]),
            phase("Install", vec![run("install", "-Dm00755")]),
            phase("Check", vec![run("cargo", "test")]),
        ],
        "autotools" => vec![
            prepare("cast-autotools-fixture"),
            phase("Setup", vec![run("dash", "./configure")]),
            phase("Build", vec![run("make", "VERBOSE=1")]),
            phase("Install", vec![run("make", "install")]),
            phase("Check", vec![run("make", "check")]),
        ],
        "custom" => vec![
            prepare("cast-custom-fixture"),
            phase("Setup", vec![run("mkdir", "-p")]),
            phase("Build", vec![run("cp", "payload.txt")]),
            phase(
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: r#"install -Dm644 build/payload.txt "${CAST_INSTALL_ROOT}${CAST_DATADIR}/cast-custom-fixture/payload.txt""#
                        .to_owned(),
                }],
            ),
            phase("Check", vec![run("cmp", "payload.txt")]),
        ],
        "daemon-generated" => vec![
            prepare("cast-daemon-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "hooks-patch" => vec![
            prepare("cast-hooks-fixture"),
            phase_with_pre(
                "Setup",
                vec![run("patch", "-p1")],
                vec![run("cmake", "-G")],
            ),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        other => panic!("unexpected execution fixture {other:?}"),
    };

    let actual = job
        .phases
        .iter()
        .map(|phase| {
            assert!(!phase.steps.is_empty(), "{name}/{}: empty frozen phase", phase.name);
            FrozenPhaseShape {
                name: phase.name.clone(),
                pre: phase.pre.iter().map(step_shape).collect(),
                steps: phase.steps.iter().map(step_shape).collect(),
                post: phase.post.iter().map(step_shape).collect(),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{name}: frozen builder phase topology drifted");
    if name == "factory-override" {
        let setup = job
            .phases
            .iter()
            .find(|phase| phase.name == "Setup")
            .expect("factory-override: frozen CMake Setup phase is missing");
        let [stone_recipe::derivation::StepPlan::Run { args, .. }] = setup.steps.as_slice() else {
            panic!("factory-override: frozen CMake Setup phase has unexpected steps");
        };
        assert!(
            args.iter()
                .any(|argument| argument == "-DCAST_FACTORY_VARIANT=stone-override"),
            "factory-override: frozen Setup command omits the explicit package patch"
        );
        assert!(
            args.iter()
                .all(|argument| argument != "-DCAST_FACTORY_VARIANT=factory-default"),
            "factory-override: frozen Setup command retained the factory default"
        );
    }
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct BootstrapClosure {
    schema_version: i64,
    repository: RepositoryPin,
    fixtures: Vec<FixtureClosure>,
    packages: PackageSet,
}

#[derive(Debug, Clone, gluon_codegen::Getable, gluon_codegen::VmType)]
struct FixtureClosure {
    name: String,
    package_ids: Vec<String>,
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

fn validate_fixture_closure_coverage(fixtures: &[FixtureClosure], package_ids: &[String]) -> Result<(), String> {
    let names = fixtures.iter().map(|fixture| fixture.name.as_str()).collect::<Vec<_>>();
    if names != REQUIRED_EXECUTION_FIXTURES {
        return Err(format!(
            "fixture closures must cover the canonical execution matrix exactly once and in order; got {names:?}"
        ));
    }

    let available = package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut covered = BTreeSet::new();
    for fixture in fixtures {
        if fixture.package_ids.is_empty() {
            return Err(format!("{}: package closure is empty", fixture.name));
        }
        if !fixture.package_ids.is_sorted() {
            return Err(format!("{}: package closure is not sorted", fixture.name));
        }
        let exact = fixture.package_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if exact.len() != fixture.package_ids.len() {
            return Err(format!("{}: package closure repeats a package ID", fixture.name));
        }
        if let Some(unknown) = exact.difference(&available).next() {
            return Err(format!(
                "{}: package closure references {unknown}, which is absent from the pinned aggregate closure",
                fixture.name
            ));
        }
        covered.extend(exact);
    }
    if covered != available {
        let uncovered = available.difference(&covered).copied().collect::<Vec<_>>();
        return Err(format!(
            "aggregate closure contains package IDs unused by every execution fixture: {uncovered:?}"
        ));
    }
    Ok(())
}

fn assert_fixture_package_closure(
    fixture: &str,
    plan: &stone_recipe::derivation::DerivationPlan,
    closure: &BootstrapClosure,
) {
    let expected = closure
        .fixtures
        .iter()
        .find(|candidate| candidate.name == fixture)
        .unwrap_or_else(|| panic!("{fixture}: no exact package closure is pinned"));
    let actual = plan
        .build_lock
        .packages
        .iter()
        .map(|package| package.package_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        expected.package_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        "{fixture}: resolved package closure drifted from its exact declarative pin"
    );
}

#[derive(Debug)]
struct ExecutionInputSnapshot {
    authored_files: BTreeMap<PathBuf, Vec<u8>>,
    build_lock: Vec<u8>,
}

impl ExecutionInputSnapshot {
    fn capture(recipe: &Path, build_lock: &Path) -> Self {
        Self {
            authored_files: snapshot_authored_inputs(recipe, build_lock),
            build_lock: fs::read(build_lock)
                .unwrap_or_else(|error| panic!("read generated execution fixture build lock {build_lock:?}: {error}")),
        }
    }

    fn assert_unchanged(&self, fixture: &str, checkpoint: &str, recipe: &Path, build_lock: &Path) {
        let actual = snapshot_authored_inputs(recipe, build_lock);
        assert_eq!(
            actual.keys().collect::<Vec<_>>(),
            self.authored_files.keys().collect::<Vec<_>>(),
            "{fixture}: authored package input set changed {checkpoint}"
        );
        let root = recipe.parent().expect("execution fixture recipe has no parent");
        for (relative, expected) in &self.authored_files {
            assert_file_bytes_unchanged(
                fixture,
                checkpoint,
                relative.to_string_lossy().as_ref(),
                &root.join(relative),
                expected,
            );
        }
        assert_file_bytes_unchanged(fixture, checkpoint, "build.lock.glu", build_lock, &self.build_lock);
    }
}

fn snapshot_authored_inputs(recipe: &Path, build_lock: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let root = recipe.parent().expect("execution fixture recipe has no parent");
    let mut snapshot = BTreeMap::new();
    snapshot_authored_directory(root, root, build_lock, &mut snapshot);
    snapshot
}

fn snapshot_authored_directory(
    root: &Path,
    directory: &Path,
    build_lock: &Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            snapshot_authored_directory(root, &path, build_lock, snapshot);
        } else if file_type.is_file() {
            if path != build_lock {
                let relative = path.strip_prefix(root).unwrap().to_owned();
                assert!(snapshot.insert(relative, fs::read(&path).unwrap()).is_none());
            }
        } else {
            panic!("execution fixture contains unsupported authored input: {path:?}");
        }
    }
}

fn assert_file_bytes_unchanged(fixture: &str, checkpoint: &str, label: &str, path: &Path, expected: &[u8]) {
    let actual =
        fs::read(path).unwrap_or_else(|error| panic!("{fixture}: read {label} at {checkpoint} from {path:?}: {error}"));
    assert!(
        actual == expected,
        "{fixture}: {label} changed {checkpoint}; expected_sha256={}, actual_sha256={}",
        hex::encode(Sha256::digest(expected)),
        hex::encode(Sha256::digest(&actual)),
    );
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
    validate_fixture_closure_coverage(&closure.fixtures, &closure.packages.sha256)
        .unwrap_or_else(|error| panic!("invalid per-fixture bootstrap closure: {error}"));
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
    for fixture in &closure.fixtures {
        for package_id in &fixture.package_ids {
            validate_sha256(package_id, &format!("{} package ID", fixture.name));
        }
    }

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
        "autoconf", "automake", "clang", "cmake", "dash", "make", "meson", "ninja", "patch", "pkgconf", "python",
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

    fn import_sources(&self, planned: &Planned) {
        let archives = bootstrap_root().parent().unwrap().join("archives");
        // Frozen setup reads from the upstream mapping owned by this exact
        // runtime, not from the workspace root itself. Keep fixture admission
        // on that authoritative path so a contentful test never falls through
        // to the deliberately unreachable HTTPS fixture URL.
        let storage_dir = planned.runtime.paths.upstreams().host;
        for source in &planned.plan.sources {
            let stone_recipe::derivation::LockedSource::Archive { url, .. } = source else {
                panic!("execution fixtures must remain archive-only");
            };
            let source_url = Url::parse(url).unwrap();
            let filename = source_url
                .path_segments()
                .and_then(Iterator::last)
                .expect("locked execution archive URL has a basename")
                .to_owned();
            crate::upstream::import_locked_archive_fixture(source, &storage_dir, &archives.join(&filename))
                .unwrap_or_else(|error| panic!("import offline execution source {filename}: {error:#}"));
        }
        assert!(
            !self.cache_dir.join("fetched").exists(),
            "offline fixtures must not be admitted beside the runtime upstream cache"
        );
    }
}

#[test]
#[ignore = "requires the package store prepared by make bootstrap-fixtures"]
fn contentful_bootstrap_materializes_a_complete_offline_root_mirror() {
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
}

#[cfg(feature = "delegated-fixture-test-support")]
pub(super) fn run_delegated_execution_fixture() {
    run_execution_fixtures_from_contentful_closure();
}

// Keep the existing implementation type-checked by ordinary unit-test builds
// without registering it with libtest. Only the feature-gated harness-free
// entry point above is allowed to execute it.
#[cfg(test)]
const _: fn() = run_execution_fixtures_from_contentful_closure;

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
fn run_execution_fixtures_from_contentful_closure() {
    let selection = execution_fixture_selection_from_env()
        .unwrap_or_else(|error| panic!("invalid execution-fixture selector: {error}"));
    let (closure, indexed) = validated_bootstrap();
    let matrix = BootstrapPlanningMatrix::new(&closure);
    matrix.materialize_package_pool(&closure, &indexed);
    let mut executed = 0usize;

    for (name, recipe) in matrix.recipes.iter().filter(|(name, _)| selection.includes(name)) {
        let first = plan_for_build(matrix.env(), matrix.request(recipe, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{name}: plan contentful execution: {error:#}"));
        assert_fixture_package_closure(name, &first.plan, &closure);
        assert_execution_fixture_topology(name, &first.plan);
        matrix.import_sources(&first);
        let canonical_plan = first.plan.canonical_bytes();
        let derivation_id = first.plan.derivation_id();
        let input_snapshot = ExecutionInputSnapshot::capture(recipe, &first.lock_path);

        let first_publication = match execute_and_publish(&first) {
            Ok(publication) => publication,
            Err(error)
                if container_capability_unavailable(error.as_ref())
                    && !execution_capability_required()
                    && executed == 0 =>
            {
                eprintln!(
                    "skipping selected contentful execution fixture(s): this host cannot create the required user/mount namespaces: {}",
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
        input_snapshot.assert_unchanged(
            name,
            "after the first execution and publication",
            recipe,
            &first.lock_path,
        );

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
        assert_eq!(
            locked.lock_path, first.lock_path,
            "{name}: repeated planning selected a different build.lock.glu path"
        );
        input_snapshot.assert_unchanged(name, "after locked replanning", recipe, &locked.lock_path);

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
        input_snapshot.assert_unchanged(
            name,
            "after the repeated execution and publication",
            recipe,
            &locked.lock_path,
        );
    }

    assert_eq!(executed, selection.expected_count());
}

#[test]
fn all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure() {
    let (closure, indexed) = validated_bootstrap();
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
        matrix.import_sources(&first);
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
        assert_fixture_package_closure(name, &first.plan, &closure);
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
        let resolved_total_download_bytes = resolved_packages
            .iter()
            .map(|hash| {
                indexed[hash]
                    .download_size
                    .unwrap_or_else(|| panic!("resolved bootstrap package {hash} has no declared size"))
            })
            .try_fold(0u64, |total, size| total.checked_add(size))
            .expect("resolved bootstrap package byte sum overflowed");
        panic!(
            "the real execution plans differ from the declarative bootstrap closure; \
             missing_from_manifest={missing_from_manifest:?}, unused_in_manifest={unused_in_manifest:?}, \
             resolved_total_download_bytes={resolved_total_download_bytes}, \
             resolved_packages={resolved_packages:?}"
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

#[test]
fn execution_fixture_selector_accepts_all_and_defaults_to_all() {
    assert_eq!(
        parse_execution_fixture_selection(None),
        Ok(ExecutionFixtureSelection::All)
    );
    assert_eq!(
        parse_execution_fixture_selection(Some("all")),
        Ok(ExecutionFixtureSelection::All)
    );
}

#[test]
fn execution_fixture_selector_accepts_each_single_fixture_exactly() {
    for selected in REQUIRED_EXECUTION_FIXTURES {
        let selection = parse_execution_fixture_selection(Some(selected)).unwrap();
        assert_eq!(selection, ExecutionFixtureSelection::One(selected));
        assert_eq!(selection.expected_count(), 1);
        for fixture in REQUIRED_EXECUTION_FIXTURES {
            assert_eq!(selection.includes(fixture), fixture == selected);
        }
    }
}

#[test]
fn execution_fixture_selector_rejects_every_noncanonical_value() {
    for invalid in ["", "ALL", "cmake ", "not-a-fixture", "autotools,cargo"] {
        let error = parse_execution_fixture_selection(Some(invalid)).unwrap_err();
        assert!(error.contains(EXECUTION_FIXTURE_SELECTOR_ENV));
        assert!(error.contains(&format!("{invalid:?}")));
    }
}

#[test]
fn fixture_closure_coverage_is_exact_and_fail_closed() {
    let package_ids = vec!["00".repeat(32), "11".repeat(32)];
    let fixtures = REQUIRED_EXECUTION_FIXTURES
        .iter()
        .map(|name| FixtureClosure {
            name: (*name).to_owned(),
            package_ids: package_ids.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(validate_fixture_closure_coverage(&fixtures, &package_ids), Ok(()));

    let mut missing = fixtures.clone();
    missing.pop();
    assert!(validate_fixture_closure_coverage(&missing, &package_ids).is_err());

    let mut duplicate = fixtures.clone();
    duplicate[0].package_ids.push(package_ids[1].clone());
    assert!(validate_fixture_closure_coverage(&duplicate, &package_ids).is_err());

    let mut unknown = fixtures;
    unknown[0].package_ids.push("ff".repeat(32));
    assert!(validate_fixture_closure_coverage(&unknown, &package_ids).is_err());
}
