// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use fs_err as fs;
use moss::{
    Provider,
    package::{Meta, Name},
};
use stone::{StoneHeaderV1FileType, StoneWriter, relation::Kind as RelationKind};
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
const TARGET: &str = "x86_64";
const SOURCE_DATE_EPOCH: i64 = 1_700_000_000;

const RECIPE: &str = r#"let b = import! boulder.package.v2

let scripts = b.scripts {
    build = b.phase [b.step.shell "printf planner-hermetic > build.log"],
    .. b.defaults.scripts
}

let root = {
    summary = b.optional.set "Hermetic planner fixture",
    description = b.optional.set "Hermetic planner fixture",
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
    moss_dir: PathBuf,
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
        let moss_dir = root.path().join("moss");
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
                r#"let boulder = import! boulder.profile.v1

boulder.profiles [
    boulder.profile "{PROFILE}" [
        boulder.repository.direct "fixture" "{index_uri}",
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
            moss_dir,
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
            Some(self.moss_dir.clone()),
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
        let mut requested = build::root::packages(&builder).unwrap();
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
    assert!(!first.requested_packages.is_empty());
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
    assert_eq!(repeated.request_fingerprint, first.request_fingerprint);
    assert_eq!(repeated.requested_packages, first.requested_packages);
    assert_eq!(repeated.plan.canonical_bytes(), first_plan);
    assert_eq!(repeated.plan.derivation_id(), first_id);
    assert_eq!(fs::read(&repeated.lock_path).unwrap(), first_lock);
}
