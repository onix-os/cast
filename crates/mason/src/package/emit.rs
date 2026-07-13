// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::{
    io::{self, Write},
    num::NonZeroU64,
    path::PathBuf,
    time::Duration,
};

use forge::package::Meta;
use fs_err::{self as fs, File};
use itertools::Itertools;
use regex::Regex;
use snafu::{ResultExt, Snafu};
use stone::{
    StoneHeaderV1FileType, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriteError,
    StoneWriter,
    relation::{Dependency, Provider},
};
use stone_recipe::derivation::{DerivationId, PackageIdentity};
use tempfile::NamedTempFile;
use tui::{ProgressBar, ProgressStyle, Styled};

use self::manifest::Manifest;
use super::{ResolvedOutput, analysis, collect};
use crate::{Architecture, Paths};

mod manifest;

const RECIPE_FINGERPRINT_SOURCE_REF_PREFIX: &str = "gluon-evaluation-sha256:";
const DERIVATION_ID_SOURCE_REF_PREFIX: &str = "derivation-sha256:";

#[derive(Debug)]
pub struct Package<'a> {
    pub name: &'a str,
    pub build_release: NonZeroU64,
    pub architecture: Architecture,
    pub identity: &'a PackageIdentity,
    pub definition: &'a ResolvedOutput,
    pub analysis: analysis::Bucket,
    provides_exclude: Vec<Regex>,
    runtime_exclude: Vec<Regex>,
    jobs: u32,
}

impl<'a> Package<'a> {
    pub fn new_with_architecture(
        name: &'a str,
        identity: &'a PackageIdentity,
        definition: &'a ResolvedOutput,
        analysis: analysis::Bucket,
        build_release: NonZeroU64,
        architecture: Architecture,
        jobs: u32,
    ) -> Self {
        let provides_exclude = compile_exclusions(&definition.provides_exclude);
        let runtime_exclude = compile_exclusions(&definition.runtime_exclude);
        Self {
            name,
            architecture,
            identity,
            definition,
            analysis,
            provides_exclude,
            runtime_exclude,
            build_release,
            jobs,
        }
    }

    pub fn filename(&self) -> String {
        super::stone_artefact_filename(
            self.name,
            &self.identity.version,
            self.identity.source_release,
            self.build_release.get(),
            self.architecture,
        )
    }

    pub fn dependencies(&self) -> Vec<Dependency> {
        self.analysis
            .dependencies()
            .cloned()
            .chain(self.definition.runtime_inputs.iter().cloned())
            .filter(|dependency| {
                self.runtime_exclude
                    .iter()
                    .all(|filter| !filter.is_match(&dependency.to_string()))
            })
            .collect()
    }

    pub fn providers(&self) -> Vec<Provider> {
        self.analysis
            .providers()
            .filter(|provider| {
                self.provides_exclude
                    .iter()
                    .all(|filter| !filter.is_match(&provider.to_string()))
            })
            .cloned()
            .collect()
    }

    pub fn meta(&self) -> Meta {
        Meta {
            name: self.name.to_owned().into(),
            version_identifier: self.identity.version.clone(),
            source_release: self.identity.source_release,
            build_release: self.build_release.get(),
            architecture: self.architecture.to_string(),
            summary: self.definition.summary.clone().unwrap_or_default(),
            description: self.definition.description.clone().unwrap_or_default(),
            source_id: self.identity.name.clone(),
            homepage: self.identity.homepage.clone(),
            licenses: self.identity.licenses.clone().into_iter().sorted().collect(),
            dependencies: self.dependencies().into_iter().collect(),
            providers: self.providers().into_iter().collect(),
            conflicts: self.definition.conflicts.iter().cloned().collect(),
            uri: None,
            hash: None,
            download_size: None,
        }
    }

    fn meta_payload(&self, recipe_fingerprint: &str, derivation_id: &DerivationId) -> Vec<StonePayloadMetaRecord> {
        Self::with_derivation_provenance(self.meta().to_stone_payload(), recipe_fingerprint, derivation_id)
    }

    fn with_derivation_provenance(
        mut payload: Vec<StonePayloadMetaRecord>,
        recipe_fingerprint: &str,
        derivation_id: &DerivationId,
    ) -> Vec<StonePayloadMetaRecord> {
        // SourceRef is an existing, optional stone metadata extension point. The
        // namespaced value is ignored by older package readers but retained in
        // package and build-manifest payloads for provenance-aware tooling.
        payload.push(StonePayloadMetaRecord {
            tag: StonePayloadMetaTag::SourceRef,
            primitive: StonePayloadMetaPrimitive::String(format!(
                "{RECIPE_FINGERPRINT_SOURCE_REF_PREFIX}{recipe_fingerprint}"
            )),
        });
        payload.push(StonePayloadMetaRecord {
            tag: StonePayloadMetaTag::SourceRef,
            primitive: StonePayloadMetaPrimitive::String(format!("{DERIVATION_ID_SOURCE_REF_PREFIX}{derivation_id}")),
        });
        payload
    }
}

fn compile_exclusions(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).expect("output exclusions were validated before package emission"))
        .collect()
}

impl PartialEq for Package<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(other.name)
    }
}

impl Eq for Package<'_> {}

impl PartialOrd for Package<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Package<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(other.name)
    }
}

pub fn emit_frozen(
    paths: &Paths,
    identity: &PackageIdentity,
    recipe_fingerprint: &str,
    build_deps: impl IntoIterator<Item = Dependency>,
    architecture: Architecture,
    packages: &[Package<'_>],
    derivation_id: &DerivationId,
) -> Result<(), Error> {
    let mut manifest = Manifest::new(
        paths,
        identity,
        recipe_fingerprint,
        build_deps,
        architecture,
        derivation_id,
    );
    for package in packages {
        if package.definition.include_in_manifest {
            manifest.add_package(package);
        }
    }

    println!("Packaging");

    for package in packages {
        emit_package(paths, package, recipe_fingerprint, derivation_id)?;
    }

    manifest.write_binary().context(ManifestSnafu)?;
    manifest.write_json().context(ManifestSnafu)?;
    for package in packages {
        verify_paths(&package.analysis.paths)?;
    }

    println!();

    Ok(())
}

fn emit_package(
    paths: &Paths,
    package: &Package<'_>,
    recipe_fingerprint: &str,
    derivation_id: &DerivationId,
) -> Result<(), Error> {
    let filename = package.filename();

    verify_paths(&package.analysis.paths)?;

    // Choose a deterministic representative for each content hash, then emit
    // larger blobs first. Collection identities, rather than host paths, are
    // retained for every selected file.
    let mut hashed_files = Vec::new();
    try_reserve(
        &mut hashed_files,
        package.analysis.paths.len(),
        "package content references",
    )?;
    for info in &package.analysis.paths {
        if let Some(hash) = info.file_hash() {
            hashed_files.push((hash, info));
        }
    }
    hashed_files.sort_unstable_by(|(left_hash, left), (right_hash, right)| {
        left_hash
            .cmp(right_hash)
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    hashed_files.dedup_by(|left, right| left.0 == right.0);
    hashed_files.sort_unstable_by(|(left_hash, left), (right_hash, right)| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left_hash.cmp(right_hash))
            .then_with(|| left.target_path.cmp(&right.target_path))
    });

    let mut total_file_size = 0u64;
    for (_, info) in &hashed_files {
        total_file_size = total_file_size.checked_add(info.size).ok_or(Error::SizeOverflow {
            resource: "package content bytes",
        })?;
    }

    let pb = ProgressBar::new(total_file_size)
        .with_message(format!("Generating {filename}"))
        .with_style(
            ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {binary_bytes_per_sec:>.dim} ")
                .unwrap()
                .tick_chars("--=≡■≡=--"),
        );
    pb.enable_steady_tick(Duration::from_millis(150));

    // Output file to artefacts directory
    let out_path = paths.artefacts().guest.join(&filename);
    if out_path.exists() {
        fs::remove_file(&out_path).context(IoSnafu)?;
    }
    let mut out_file = File::create(out_path).context(IoSnafu)?;

    // Create stone binary writer
    let mut writer = StoneWriter::new(&mut out_file, StoneHeaderV1FileType::Binary).context(StoneBinaryWriterSnafu)?;

    // Add metadata
    {
        writer
            .add_payload(package.meta_payload(recipe_fingerprint, derivation_id).as_slice())
            .context(StoneBinaryWriterSnafu)?;
    }

    // Add layouts
    {
        let mut layouts = Vec::new();
        try_reserve(&mut layouts, package.analysis.paths.len(), "package layout records")?;
        layouts.extend(package.analysis.paths.iter().map(|path| path.layout.clone()));
        if !layouts.is_empty() {
            writer.add_payload(layouts.as_slice()).context(StoneBinaryWriterSnafu)?;
        }
    }

    // Only add content payload if we have some files
    if !hashed_files.is_empty() {
        // Unique plan-runtime-local scratch avoids collisions between
        // concurrent builds of the same output.
        let mut temp_content = NamedTempFile::new_in(&paths.artefacts().guest).context(IoSnafu)?;

        // Convert to content writer using pledged size = total size of all files
        let mut writer = writer
            .with_content(temp_content.as_file_mut(), Some(total_file_size), package.jobs)
            .context(StoneBinaryWriterSnafu)?;

        for (_, info) in hashed_files {
            let mut file = info.open_verified().map_err(|source| Error::VerifiedInput {
                path: info.path.clone(),
                source,
            })?;
            let write_result = {
                let mut progress = pb.wrap_read(&mut file);
                writer.add_content(&mut progress)
            };
            let verify_result = file.finish();
            if let Err(source) = verify_result {
                return Err(Error::VerifiedInput {
                    path: info.path.clone(),
                    source,
                });
            }
            write_result.context(StoneBinaryWriterSnafu)?;
        }

        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
        verify_paths(&package.analysis.paths)?;
    } else {
        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
        verify_paths(&package.analysis.paths)?;
    }

    pb.suspend(|| println!("{} {filename}", "Emitted".green()));
    pb.finish_and_clear();

    Ok(())
}

fn verify_paths(paths: &[collect::PathInfo]) -> Result<(), Error> {
    for info in paths {
        info.verify_unchanged().map_err(|source| Error::VerifiedInput {
            path: info.path.clone(),
            source,
        })?;
    }
    Ok(())
}

fn try_reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        detail: source.to_string(),
    })
}

#[cfg(test)]
mod verification_tests {
    use super::*;

    #[test]
    fn emitter_rejects_a_path_replaced_after_collection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"payload").unwrap();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let info = collector
            .path(&path, &mut stone::StoneDigestWriterHasher::new())
            .unwrap();
        std::fs::rename(&path, root.path().join("old")).unwrap();
        std::fs::write(&path, b"payload").unwrap();

        assert!(matches!(verify_paths(&[info]), Err(Error::VerifiedInput { .. })));
    }
}

#[cfg(test)]
pub(crate) fn test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    static PLAN: std::sync::OnceLock<stone_recipe::derivation::DerivationPlan> = std::sync::OnceLock::new();

    PLAN.get_or_init(build_test_derivation_plan).clone()
}

#[cfg(test)]
fn test_evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> gluon_config::EvaluationFingerprint {
    gluon_config::Evaluator::default()
        .evaluate_with_inputs::<i64>(&gluon_config::Source::new(logical_name, source), explicit_inputs)
        .expect("test provenance must be a real restricted evaluation")
        .fingerprint
}

#[cfg(test)]
fn build_test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    use stone_recipe::build_policy::{AnalyzerKind, layers::BuildPolicyOperation};
    use stone_recipe::derivation::{
        BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, DerivationProvenance, ExecutablePlan,
        ExecutionCredentials, InputOrigin, LockedIdentity, LockedOutput, LockedPackage, LockedRequest, OutputPlan,
        PackageIdentity, Platform, PolicyLayerProvenance, PolicyProvenance, PolicyTransitionProvenance,
        ProfileFragmentProvenance, RelationKind, RelationPlan, RepositorySnapshot, policy_composition_identity,
        profile_aggregate_fingerprint,
    };

    const SOURCE_LOCK_BYTES: &[u8] = b"test source lock bytes";

    let profiles = vec![ProfileFragmentProvenance {
        logical_name: "default".to_owned(),
        evaluation: test_evaluation("profile.d/default.glu", "1", &[]),
    }];
    let layers = vec![PolicyLayerProvenance {
        name: "foundation".to_owned(),
        transitions: vec![PolicyTransitionProvenance {
            operation: BuildPolicyOperation::Add,
            origin: "default.glu".to_owned(),
            evaluation: test_evaluation("default.glu", "2", &[]),
        }],
    }];
    let policy_inputs = policy_composition_identity("aerynos", &layers);
    let provenance = DerivationProvenance {
        recipe: test_evaluation("stone.glu", "3", SOURCE_LOCK_BYTES),
        profiles,
        policy: PolicyProvenance {
            name: "aerynos".to_owned(),
            root: test_evaluation("policy.glu", "4", &policy_inputs),
            layers,
        },
    };
    let platform = Platform {
        architecture: "x86_64".to_owned(),
        vendor: "unknown".to_owned(),
        operating_system: "linux".to_owned(),
        abi: "gnu".to_owned(),
    };
    let identity = |name: &str| LockedIdentity {
        name: name.to_owned(),
        fingerprint: format!("{name}-fingerprint"),
    };
    let mut build_lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: "request-fingerprint".to_owned(),
        repositories: vec![RepositorySnapshot {
            id: "test-repository".to_owned(),
            index_uri: "https://example.invalid/stone.index".to_owned(),
            snapshot: "test-repository-snapshot".to_owned(),
        }],
        requests: [
            "pkg-config",
            "python3",
            "llvm-objcopy",
            "llvm-strip",
            "objcopy",
            "strip",
        ]
        .into_iter()
        .map(|name| LockedRequest {
            request: format!("binary({name})"),
            package_id: "analyzer-tools-id".to_owned(),
            output: "out".to_owned(),
            origins: vec![InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.base".to_owned(),
                index: 0,
            }],
        })
        .collect(),
        packages: vec![LockedPackage {
            package_id: "analyzer-tools-id".to_owned(),
            name: "analyzer-tools".to_owned(),
            version: "1.0.0-1-1".to_owned(),
            architecture: "x86_64".to_owned(),
            repository: "test-repository".to_owned(),
            outputs: vec![LockedOutput { name: "out".to_owned() }],
            dependencies: Vec::new(),
        }],
        build_platform: platform.clone(),
        host_platform: platform.clone(),
        target_platform: platform,
        policy: LockedIdentity {
            name: provenance.policy.name.clone(),
            fingerprint: provenance.policy.root.sha256.clone(),
        },
        target: identity("x86_64"),
        profile: LockedIdentity {
            name: "profile".to_owned(),
            fingerprint: profile_aggregate_fingerprint(&provenance.profiles),
        },
        toolchain: identity("toolchain"),
        builder: identity("builder"),
    };
    build_lock.normalize();
    let mut plan = stone_recipe::derivation::DerivationPlan::new(
        PackageIdentity {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            source_release: 1,
            build_release: 1,
            homepage: "https://example.invalid".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            architecture: "x86_64".to_owned(),
        },
        build_lock,
        provenance,
    );
    plan.cast_version = "test-cast".to_owned();
    plan.cast_fingerprint = "sha256:test-cast-semantics".to_owned();
    plan.execution.executor = LockedIdentity {
        name: "test-executor".to_owned(),
        fingerprint: "test-executor-fingerprint".to_owned(),
    };
    plan.execution.credentials = ExecutionCredentials::IsolatedRoot;
    plan.source_lock_digest = plan.provenance.recipe.explicit_inputs_sha256.clone();
    plan.layout = BuilderLayout {
        hostname: "cast".to_owned(),
        guest_root: "/mason".to_owned(),
        artifacts_dir: "/mason/artefacts".to_owned(),
        build_dir: "/mason/build".to_owned(),
        source_dir: "/mason/sources".to_owned(),
        recipe_dir: "/mason/recipe".to_owned(),
        install_dir: "/mason/install".to_owned(),
        package_dir: "/mason/recipe/pkg".to_owned(),
        ccache_dir: "/mason/ccache".to_owned(),
        sccache_dir: "/mason/sccache".to_owned(),
        go_cache_dir: "/mason/gocache".to_owned(),
        go_mod_cache_dir: "/mason/gomodcache".to_owned(),
        cargo_cache_dir: "/mason/cargocache".to_owned(),
        zig_cache_dir: "/mason/zigcache".to_owned(),
    };
    plan.source_date_epoch = 1_700_000_000;
    plan.analysis.handlers = vec![
        AnalyzerKind::IgnoreBlocked,
        AnalyzerKind::Binary,
        AnalyzerKind::Elf,
        AnalyzerKind::PkgConfig,
        AnalyzerKind::Python,
        AnalyzerKind::CMake,
        AnalyzerKind::CompressMan,
        AnalyzerKind::IncludeAny,
    ];
    let analyzer_tool = |name: &str| ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: name.to_owned(),
        },
    };
    plan.analysis.tools.pkg_config = Some(analyzer_tool("pkg-config"));
    plan.analysis.tools.python = Some(analyzer_tool("python3"));
    plan.analysis.tools.strip = Some(analyzer_tool("llvm-strip"));
    plan.outputs = vec![OutputPlan {
        name: "out".to_owned(),
        package_name: "example".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    }];
    plan.validate().unwrap();
    plan
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("stone binary writer"))]
    StoneBinaryWriter { source: StoneWriteError },
    #[snafu(display("manifest"))]
    Manifest { source: manifest::Error },
    #[snafu(display("io"))]
    Io { source: io::Error },
    #[snafu(display("verified package input {}: {source}", path.display()))]
    VerifiedInput { path: PathBuf, source: collect::Error },
    #[snafu(display("failed to reserve {requested} units for {resource}: {detail}"))]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[snafu(display("size overflow while totaling {resource}"))]
    SizeOverflow { resource: &'static str },
}
