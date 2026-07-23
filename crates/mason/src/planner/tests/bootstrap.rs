use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    time::Duration,
};

use forge::package::Meta;
use fs_err as fs;
use gluon_config::{DiagnosticCategory, GluonEngine, ImportPolicy, LimitKind, Limits, SourceRoot};
use sha2::{Digest, Sha256};
use stone::{StoneDecodeLimits, StoneDecodedPayload, StoneHeader, StoneHeaderV1FileType};
use url::Url;

use super::{
    EXECUTION_FIXTURES, Env, HEADER_ONLY_CHECK_SCRIPT, HEADER_ONLY_INSTALL_SCRIPT, Planned, Publication, Request,
    SOURCE_DATE_EPOCH, TARGET, WriteOutcome, canonical_build_lock,
    container_capability_unavailable, copy_package_directory, error_chain,
    execute_and_publish, execution_capability_required, plan_for_build, profile,
};

#[path = "bootstrap/bundle.rs"]
mod bundle;
#[path = "bootstrap/execution_evidence.rs"]
mod execution_evidence;

pub(crate) use execution_evidence::DelegatedExecutionOutcome;

const BOOTSTRAP_SCHEMA_VERSION: i64 = 2;
const MAX_BOOTSTRAP_GLUON_MODULE_BYTES: usize = 64 * 1024;
const MAX_BOOTSTRAP_GLUON_IMPORT_GRAPH_BYTES: usize = 4 * MAX_BOOTSTRAP_GLUON_MODULE_BYTES;
const MAX_BOOTSTRAP_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BOOTSTRAP_PACKAGE_COUNT: usize = 512;
const MAX_BOOTSTRAP_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const BOOTSTRAP_PROFILE: &str = "planner-contentful-bootstrap";
include!("bootstrap/execution_topology.rs");
include!("bootstrap/hooks_patch.rs");
include!("bootstrap/autotools_regeneration.rs");
include!("bootstrap/cmake_zlib.rs");
include!("bootstrap/desktop_integration.rs");
include!("bootstrap/external_test_vectors.rs");
include!("bootstrap/font_family.rs");
include!("bootstrap/gettext_localization.rs");
include!("bootstrap/go_module.rs");
include!("bootstrap/meson_dependency_roles.rs");
include!("bootstrap/pgo_workload.rs");
include!("bootstrap/relation_policy.rs");
include!("bootstrap/python_module.rs");
include!("bootstrap/system_integration_assets.rs");
include!("bootstrap/temp_root.rs");
include!("bootstrap/execution_cleanup.rs");

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

fn bootstrap_closure_evaluator(source_root: SourceRoot) -> GluonEngine {
    let mut import_policy = ImportPolicy::new();
    import_policy.enable_array_primitives();
    let limits = Limits {
        max_source_bytes: MAX_BOOTSTRAP_GLUON_MODULE_BYTES,
        max_imported_file_bytes: MAX_BOOTSTRAP_GLUON_MODULE_BYTES,
        max_imports: 16,
        max_import_graph_bytes: MAX_BOOTSTRAP_GLUON_IMPORT_GRAPH_BYTES,
        ..Limits::default()
    };
    GluonEngine::new(limits)
        .with_source_root(source_root)
        .with_import_policy(import_policy)
}

fn evaluate_bootstrap_closure() -> gluon_config::Evaluation<BootstrapClosure> {
    let source_root = SourceRoot::new(bootstrap_root()).unwrap();
    bootstrap_closure_evaluator(source_root)
        .evaluate_file::<BootstrapClosure>("closure.glu")
        .unwrap()
}

fn load_bootstrap_closure() -> BootstrapClosure {
    evaluate_bootstrap_closure().value
}

#[test]
fn bootstrap_closure_fingerprints_every_functional_data_module() {
    let evaluation = evaluate_bootstrap_closure();
    evaluation.identity.validate().unwrap();
    let mut imported = evaluation
        .identity
        .modules
        .iter()
        .map(|module| module.logical_name.as_str())
        .collect::<Vec<_>>();
    // v2 identity orders modules by their canonical graph identity; assert
    // membership independent of that ordering.
    imported.sort_unstable();
    assert_eq!(
        imported,
        [
            "aggregate_package_ids.glu",
            "build_system_package_sets.glu",
            "package_sets.glu",
            "specialized_package_sets.glu",
            "std.array.prim",
            "system_integration_package_set.glu",
            "tooling_package_sets.glu",
        ]
    );
}

#[test]
fn bootstrap_closure_imports_cannot_escape_the_descriptor_root() {
    let root = crate::private_tempdir();
    let source_root = root.path().join("bootstrap");
    fs::create_dir(&source_root).unwrap();
    fs::write(root.path().join("outside.glu"), "42").unwrap();
    fs::write(source_root.join("closure.glu"), "import! \"../outside.glu\"").unwrap();

    let error = bootstrap_closure_evaluator(SourceRoot::new(&source_root).unwrap())
        .evaluate_file::<i64>("closure.glu")
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert!(error.message.contains("parent traversal"));
}

#[test]
fn bootstrap_closure_imports_reject_symlinks_and_oversized_modules() {
    let root = crate::private_tempdir();
    let source_root = root.path().join("bootstrap");
    fs::create_dir(&source_root).unwrap();
    fs::write(source_root.join("closure.glu"), "import! \"./package_sets.glu\"").unwrap();
    fs::write(root.path().join("outside.glu"), "42").unwrap();
    symlink("../outside.glu", source_root.join("package_sets.glu")).unwrap();
    let evaluator = bootstrap_closure_evaluator(SourceRoot::new(&source_root).unwrap());

    let symlink_error = evaluator.evaluate_file::<i64>("closure.glu").unwrap_err();
    assert_eq!(symlink_error.category, DiagnosticCategory::Import);
    assert!(symlink_error.message.contains("symbolic links"));

    fs::remove_file(source_root.join("package_sets.glu")).unwrap();
    fs::write(
        source_root.join("package_sets.glu"),
        vec![b' '; MAX_BOOTSTRAP_GLUON_MODULE_BYTES + 1],
    )
    .unwrap();
    let size_error = evaluator.evaluate_file::<i64>("closure.glu").unwrap_err();
    assert_eq!(size_error.category, DiagnosticCategory::Limit);
    assert_eq!(size_error.limit, Some(LimitKind::ImportedFileSize));
}

#[test]
fn bootstrap_closure_import_graph_has_an_explicit_total_byte_boundary() {
    let root = crate::private_tempdir();
    let source_root = root.path().join("bootstrap");
    fs::create_dir(&source_root).unwrap();
    let module_bytes = MAX_BOOTSTRAP_GLUON_MODULE_BYTES - 1024;
    let module_source = format!("\"{}\"", "x".repeat(module_bytes - 2));
    for name in ["one", "two", "three", "four", "five"] {
        fs::write(source_root.join(format!("{name}.glu")), &module_source).unwrap();
    }
    fs::write(
        source_root.join("closure.glu"),
        concat!(
            "let _ = import! \"./one.glu\"\n",
            "let _ = import! \"./two.glu\"\n",
            "let _ = import! \"./three.glu\"\n",
            "let _ = import! \"./four.glu\"\n",
            "let _ = import! \"./five.glu\"\n",
            "0\n",
        ),
    )
    .unwrap();

    let error = bootstrap_closure_evaluator(SourceRoot::new(&source_root).unwrap())
        .evaluate_file::<i64>("closure.glu")
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportGraphSize));
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
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
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
    assert_autotools_regeneration_bootstrap_contract(&closure, &indexed);
    assert_cmake_zlib_bootstrap_contract(&closure, &indexed);
    assert_desktop_integration_bootstrap_contract(&closure, &indexed);
    assert_external_test_vectors_bootstrap_contract(&closure, &indexed);
    assert_font_family_bootstrap_contract(&closure, &indexed);
    assert_gettext_localization_bootstrap_contract(&closure, &indexed);
    assert_go_module_bootstrap_contract(&closure, &indexed);
    assert_meson_dependency_role_bootstrap_contract(&closure, &indexed);
    assert_pgo_workload_bootstrap_contract(&closure, &indexed);
    assert_relation_policy_bootstrap_contract(&closure, &indexed);
    assert_python_module_bootstrap_contract(&closure, &indexed);
    assert_system_integration_assets_bootstrap_contract(&closure, &indexed);

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
        "autoconf", "automake", "clang", "cmake", "dash", "golang", "make", "meson", "ninja", "patch", "pkgconf",
        "python", "rust",
    ] {
        assert!(
            names.contains(required),
            "bootstrap closure is missing real package `{required}`"
        );
    }

    (closure, indexed)
}

struct BootstrapPlanningMatrix {
    _root: BootstrapTempRoot,
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
        let root = BootstrapTempRoot::new(crate::private_tempdir());
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

        super::write_repository_policy_fixture(&data_dir);
        fs::create_dir_all(config_dir.join("profile.d")).unwrap();
        fs::create_dir_all(&recipes_dir).unwrap();
        fs::create_dir_all(&history_dir).unwrap();
        root.create_private_directory(Path::new("output"), &output_dir);
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

        let recipes = EXECUTION_FIXTURES
            .iter()
            .map(|name| {
                let recipe_dir = recipes_dir.join(name);
                copy_package_directory(&super::execution_fixture_package_directory(name), &recipe_dir);
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
        let fixture_root = bootstrap_root().parent().unwrap().to_owned();
        let archives = fixture_root.join("archives");
        let git_bundles = fixture_root.join("git-bundles");
        // Frozen setup reads from the upstream mapping owned by this exact
        // runtime, not from the workspace root itself. Keep fixture admission
        // on that authoritative path so a contentful test never falls through
        // to the deliberately unreachable HTTPS fixture URL.
        let storage_dir = planned.runtime.paths.upstreams().host;
        let expected_sources = match planned.plan.package.name.as_str() {
            "cast-generated-config-fixture"
            | "cast-generated-shell-fixture"
            | "cast-relation-policy-fixture"
            | "cast-userspace-profile-fixture" => 0,
            "cast-external-test-vectors-fixture" | "cast-hooks-fixture" => 2,
            "cast-multiple-sources-fixture" => 3,
            _ => 1,
        };
        assert_eq!(
            planned.plan.sources.len(),
            expected_sources,
            "execution fixture locked-source cardinality drift"
        );
        for source in &planned.plan.sources {
            match source {
                stone_recipe::derivation::LockedSource::Archive { url, .. } => {
                    let source_url = Url::parse(url).unwrap();
                    let filename = source_url
                        .path_segments()
                        .and_then(Iterator::last)
                        .expect("locked execution archive URL has a basename")
                        .to_owned();
                    crate::upstream::import_locked_archive_fixture(source, &storage_dir, &archives.join(&filename))
                        .unwrap_or_else(|error| panic!("import offline execution source {filename}: {error:#}"));
                }
                stone_recipe::derivation::LockedSource::Git { url, commit, .. } => {
                    assert_eq!(planned.plan.package.name, "cast-multiple-sources-fixture");
                    assert_eq!(url, super::MULTIPLE_SOURCES_GIT_URL);
                    assert_eq!(commit, super::MULTIPLE_SOURCES_GIT_COMMIT);
                    let bundle = git_bundles.join(super::MULTIPLE_SOURCES_GIT_BUNDLE);
                    let metadata = fs::symlink_metadata(&bundle).unwrap();
                    assert!(metadata.file_type().is_file() && metadata.nlink() == 1);
                    assert_eq!(
                        hex::encode(Sha256::digest(fs::read(&bundle).unwrap())),
                        super::MULTIPLE_SOURCES_GIT_BUNDLE_SHA256,
                        "delegated Git transport bundle drifted before offline admission"
                    );
                    crate::upstream::import_locked_git_fixture(source, &storage_dir, &bundle, SOURCE_DATE_EPOCH)
                        .unwrap_or_else(|error| panic!("import offline execution Git bundle: {error:?}"));
                }
            }
        }
        assert!(
            !self.cache_dir.join("fetched").exists(),
            "offline fixtures must not be admitted beside the runtime upstream cache"
        );
    }
}

include!("bootstrap/execution_matrix.rs");
include!("bootstrap/fixture_selection.rs");
include!("bootstrap/package_store.rs");
