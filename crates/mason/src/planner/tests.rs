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
use stone_recipe::derivation::{FilesystemPolicy, InputOrigin, NetworkMode};
use tempfile::TempDir;
use url::Url;

use super::{Request, plan};
use crate::{
    Env,
    build::{self, Builder, BuilderRequest},
    build_lock::WriteOutcome,
    profile,
};

const PROFILE: &str = "planner-hermetic";
const ALTERNATE_PROFILE: &str = "planner-hermetic-alternate";
const TARGET: &str = "x86_64";
const SOURCE_DATE_EPOCH: i64 = 1_700_000_000;
const RUNTIME_REQUEST: &str = "binary(planner-runtime)";

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
