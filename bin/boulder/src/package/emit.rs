// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::{
    io::{self, Write},
    num::NonZeroU64,
    time::Duration,
};

use fs_err::{self as fs, File};
use itertools::Itertools;
use moss::package::Meta;
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
use super::{ResolvedOutput, analysis};
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
        format!(
            "{}-{}-{}-{}-{}.stone",
            self.name, self.identity.version, self.identity.source_release, self.build_release, self.architecture
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

    // Filter for all files -> dedupe by hash -> sort largest to smallest
    let files = package
        .analysis
        .paths
        .iter()
        // Filter by file
        .filter_map(|info| info.file_hash().map(|hash| (hash, info)))
        // Dedupe by hash
        .unique_by(|(hash, _)| *hash)
        // Sort largest to smallest
        .sorted_by(|(_, a), (_, b)| a.size.cmp(&b.size).reverse())
        .map(|(_, info)| info)
        .collect::<Vec<_>>();

    let total_file_size = files.iter().map(|info| info.size).sum();

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
        let layouts = package
            .analysis
            .paths
            .iter()
            .map(|p| p.layout.clone())
            .collect::<Vec<_>>();
        if !layouts.is_empty() {
            writer.add_payload(layouts.as_slice()).context(StoneBinaryWriterSnafu)?;
        }
    }

    // Only add content payload if we have some files
    if !files.is_empty() {
        // Unique plan-runtime-local scratch avoids collisions between
        // concurrent builds of the same output.
        let mut temp_content = NamedTempFile::new_in(&paths.artefacts().guest).context(IoSnafu)?;

        // Convert to content writer using pledged size = total size of all files
        let mut writer = writer
            .with_content(temp_content.as_file_mut(), Some(total_file_size), package.jobs)
            .context(StoneBinaryWriterSnafu)?;

        for info in files {
            let file = File::open(&info.path).context(IoSnafu)?;
            writer
                .add_content(&mut pb.wrap_read(&file))
                .context(StoneBinaryWriterSnafu)?;
        }

        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
    } else {
        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
    }

    pb.suspend(|| println!("{} {filename}", "Emitted".green()));
    pb.finish_and_clear();

    Ok(())
}

#[cfg(test)]
pub(crate) fn test_derivation_id() -> DerivationId {
    test_derivation_plan().derivation_id()
}

#[cfg(test)]
pub(crate) fn test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    use stone_recipe::build_policy::AnalyzerKind;
    use stone_recipe::derivation::{
        BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, LockedIdentity, OutputPlan, PackageIdentity, Platform,
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
    let build_lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: "request-fingerprint".to_owned(),
        repositories: Vec::new(),
        requests: Vec::new(),
        packages: Vec::new(),
        build_platform: platform.clone(),
        host_platform: platform.clone(),
        target_platform: platform,
        policy: identity("aerynos"),
        target: identity("x86_64"),
        profile: identity("profile"),
        toolchain: identity("toolchain"),
        builder: identity("builder"),
    };
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
    );
    plan.boulder_version = "test-boulder".to_owned();
    plan.boulder_fingerprint = "sha256:test-boulder-semantics".to_owned();
    plan.recipe_fingerprint = "recipe-fingerprint".to_owned();
    plan.source_lock_digest = "source-lock-digest".to_owned();
    plan.layout = BuilderLayout {
        hostname: "boulder".to_owned(),
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
}
