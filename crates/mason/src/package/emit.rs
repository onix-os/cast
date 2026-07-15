// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::{
    ffi::{CStr, CString, OsStr},
    fs::Metadata,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{size_of, zeroed},
    num::NonZeroU64,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    time::Duration,
};

use forge::package::{Meta, is_reserved_usr_layout_target};
use fs_err::File;
use itertools::Itertools;
use nix::{errno::Errno, libc};
use regex::Regex;
use sha2::{Digest, Sha256};
use snafu::{ResultExt, Snafu};
use stone::{
    StoneHeaderV1FileType, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriteError,
    StoneWriter,
    relation::{Dependency, Provider},
};
use stone_recipe::derivation::{DerivationId, PackageIdentity};
use tui::{ProgressBar, ProgressStyle, Styled};

use self::{artifact_sink::ArtifactSink, manifest::Manifest};
use super::{ResolvedOutput, analysis, collect};
use crate::{Architecture, Paths};

mod artifact_directory;
mod artifact_sink;
mod artifact_verification;
mod manifest;

const RECIPE_FINGERPRINT_SOURCE_REF_PREFIX: &str = "gluon-evaluation-sha256:";
const DERIVATION_ID_SOURCE_REF_PREFIX: &str = "derivation-sha256:";
const EMISSION_STAGE_NAME: &[u8] = b".mason-emission";
const EMISSION_SCRATCH_NAME: &[u8] = b".content-scratch";
const MAX_EMITTED_ARTIFACTS: usize = 256;
const MAX_STONE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const ARTIFACT_DIGEST_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct ArtifactSpec {
    name: String,
    max_bytes: u64,
}

impl ArtifactSpec {
    fn stone(name: String) -> Self {
        Self {
            name,
            max_bytes: MAX_STONE_ARTIFACT_BYTES,
        }
    }

    fn manifest(name: String) -> Self {
        Self {
            name,
            max_bytes: MAX_MANIFEST_ARTIFACT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    identity: Identity,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileWitness {
    identity: Identity,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn unchanged_by_rename_from(self, previous: Self) -> bool {
        self.identity == previous.identity
            && self.mode == previous.mode
            && self.links == previous.links
            && self.length == previous.length
            && self.modified_seconds == previous.modified_seconds
            && self.modified_nanoseconds == previous.modified_nanoseconds
    }
}

impl DirectoryStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("{operation} at {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{role} at {path:?} is not the expected {expected}")]
    UnexpectedKind {
        role: &'static str,
        path: PathBuf,
        expected: &'static str,
    },
    #[error("artifact name {name:?} is not one safe path component")]
    InvalidName { name: String },
    #[error("duplicate emitted artifact name {name:?}")]
    DuplicateName { name: String },
    #[error("artifact writer requested undeclared name {name:?}")]
    UnexpectedName { name: String },
    #[error("artifact {name:?} was prepared more than once")]
    AlreadyPrepared { name: String },
    #[error("artifact {name:?} was never prepared")]
    NotPrepared { name: String },
    #[error("artifact content scratch is unavailable")]
    ScratchUnavailable,
    #[error("owned artifact path changed identity or type: {path:?}")]
    OwnershipChanged { path: PathBuf },
    #[error("sealed artifact metadata changed: {path:?}")]
    ArtifactChanged { path: PathBuf },
    #[error("sealed artifact content digest changed: {path:?}")]
    DigestChanged { path: PathBuf },
    #[error("artifact directory changed during exact enumeration: {path:?}")]
    DirectoryChanged { path: PathBuf },
    #[error("{role} {path:?} has the wrong entries (expected {expected:?}, found {found:?})")]
    InventoryMismatch {
        role: &'static str,
        path: PathBuf,
        expected: Vec<Vec<u8>>,
        found: Vec<Vec<u8>>,
    },
    #[error("{role} {path:?} has mode {found:#06o}; expected {expected:#06o}")]
    ModeMismatch {
        role: &'static str,
        path: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("artifact {path:?} is {found} bytes; maximum is {maximum}")]
    ArtifactTooLarge { path: PathBuf, maximum: u64, found: u64 },
    #[error("{resource} exceeds finite limit {limit}")]
    ResourceLimit { resource: &'static str, limit: usize },
    #[error("failed to reserve {requested} units for {resource}: {detail}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[error("artifact rollback was incomplete: {failures:?}")]
    Cleanup { failures: Vec<String> },
    #[error("cannot safely remove an artifact path whose created inode could not be authenticated: {path:?}")]
    UnprovenCleanup { path: PathBuf },
    #[error("{primary}; rollback also failed: {cleanup}")]
    Rollback {
        primary: Box<ArtifactError>,
        cleanup: Box<ArtifactError>,
    },
}

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
    sealed: &collect::SealedTree,
) -> Result<(), Error> {
    verify_unique_layout_targets(packages)?;
    sealed.verify().map_err(|source| Error::VerifiedInventory { source })?;
    let mut manifest = Manifest::new(identity, recipe_fingerprint, build_deps, derivation_id);
    for package in packages {
        if package.definition.include_in_manifest {
            manifest.add_package(package);
        }
    }

    let binary_manifest_name = super::binary_manifest_filename(architecture);
    let json_manifest_name = super::jsonc_manifest_filename(architecture);
    if packages.len() > MAX_EMITTED_ARTIFACTS.saturating_sub(2) {
        return Err(Error::Artifact {
            source: ArtifactError::ResourceLimit {
                resource: "emitted package artifacts",
                limit: MAX_EMITTED_ARTIFACTS.saturating_sub(2),
            },
        });
    }
    let mut specs = Vec::new();
    specs
        .try_reserve(packages.len().saturating_add(2))
        .map_err(|source| Error::Allocation {
            resource: "expected artifact names",
            requested: packages.len().saturating_add(2),
            detail: source.to_string(),
        })?;
    specs.extend(packages.iter().map(|package| ArtifactSpec::stone(package.filename())));
    specs.push(ArtifactSpec::manifest(binary_manifest_name.clone()));
    specs.push(ArtifactSpec::manifest(json_manifest_name.clone()));
    let mut sink = ArtifactSink::new(&paths.artefacts().guest, specs).context(ArtifactSnafu)?;

    println!("Packaging");

    let emission = (|| {
        for package in packages {
            emit_package(&mut sink, package, recipe_fingerprint, derivation_id)?;
        }

        manifest
            .write_binary(sink.writer(&binary_manifest_name).context(ArtifactSnafu)?)
            .context(ManifestSnafu)?;
        manifest
            .write_json(sink.writer(&json_manifest_name).context(ArtifactSnafu)?)
            .context(ManifestSnafu)?;
        for package in packages {
            verify_paths(&package.analysis.paths)?;
        }
        sealed.verify().map_err(|source| Error::VerifiedInventory { source })?;
        Ok(())
    })();
    if let Err(primary) = emission {
        return match sink.abort() {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::ArtifactRollback {
                primary: Box::new(primary),
                cleanup,
            }),
        };
    }
    sink.commit().context(ArtifactSnafu)?;

    println!();

    Ok(())
}

fn verify_unique_layout_targets(packages: &[Package<'_>]) -> Result<(), Error> {
    let total = packages.iter().try_fold(0usize, |total, package| {
        total
            .checked_add(package.analysis.paths.len())
            .ok_or(Error::SizeOverflow {
                resource: "package layout targets",
            })
    })?;
    let mut targets = Vec::new();
    try_reserve(&mut targets, total, "package layout targets")?;
    for package in packages {
        for info in &package.analysis.paths {
            info.check_deadline().map_err(|source| Error::VerifiedInput {
                path: info.path.clone(),
                source,
            })?;
            let target = info.layout.file.target().trim_start_matches('/');
            if is_reserved_usr_layout_target(target) {
                return Err(Error::ReservedLayoutTarget {
                    target: format!("/usr/{target}"),
                    package: package.name.to_owned(),
                    path: info.path.clone(),
                });
            }
            targets.push((
                target,
                package.name,
                &info.path,
                matches!(info.layout.file, stone::StonePayloadLayoutFile::Directory(_)),
            ));
        }
    }
    targets.sort_unstable_by(|left, right| {
        left.0
            .cmp(right.0)
            .then_with(|| left.1.cmp(right.1))
            .then_with(|| left.2.cmp(right.2))
    });
    for pair in targets.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(Error::DuplicateLayoutTarget {
                target: format!("/{}", pair[0].0),
                first_package: pair[0].1.to_owned(),
                first_path: pair[0].2.to_owned(),
                second_package: pair[1].1.to_owned(),
                second_path: pair[1].2.to_owned(),
            });
        }
    }

    // Exact duplicates are rejected above, including duplicate directories.
    // A directory may be the ancestor of another layout target, but every
    // other inode kind would require the installer to materialize the same
    // target as both a terminal and a directory. Normalized `/usr` aliases can
    // otherwise make this conflict arise from distinct source paths.
    for descendant in &targets {
        let mut ancestor = descendant.0;
        while let Some(separator) = ancestor.rfind('/') {
            ancestor = &ancestor[..separator];
            if ancestor.is_empty() {
                break;
            }
            if let Ok(index) = targets.binary_search_by(|candidate| candidate.0.cmp(ancestor)) {
                let candidate = &targets[index];
                if !candidate.3 {
                    return Err(Error::AncestorLayoutTarget {
                        ancestor: format!("/{}", candidate.0),
                        ancestor_package: candidate.1.to_owned(),
                        ancestor_path: candidate.2.to_owned(),
                        descendant: format!("/{}", descendant.0),
                        descendant_package: descendant.1.to_owned(),
                        descendant_path: descendant.2.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn emit_package(
    sink: &mut ArtifactSink,
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

    let (out_file, temp_content) = sink.package_writers(&filename).context(ArtifactSnafu)?;

    // Create stone binary writer
    let mut writer = StoneWriter::new(&mut *out_file, StoneHeaderV1FileType::Binary).context(StoneBinaryWriterSnafu)?;

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
        layouts.sort_unstable_by(|left, right| left.file.target().cmp(right.file.target()));
        if !layouts.is_empty() {
            writer.add_payload(layouts.as_slice()).context(StoneBinaryWriterSnafu)?;
        }
    }

    // Only add content payload if we have some files
    if !hashed_files.is_empty() {
        // Convert to content writer using pledged size = total size of all files
        let mut writer = writer
            .with_content(temp_content, Some(total_file_size), package.jobs)
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
            finish_content_write(&info.path, write_result, verify_result)?;
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

    pb.suspend(|| println!("{} {filename}", "Prepared".green()));
    pb.finish_and_clear();

    Ok(())
}

fn finish_content_write(
    path: &Path,
    write_result: Result<(), StoneWriteError>,
    verify_result: Result<(), collect::Error>,
) -> Result<(), Error> {
    // Always finish descriptor verification, but do not replace a primary
    // Stone writer failure with the expected short-read consequence.
    write_result.context(StoneBinaryWriterSnafu)?;
    verify_result.map_err(|source| Error::VerifiedInput {
        path: path.to_owned(),
        source,
    })
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

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("artifact emission: {source}"))]
    Artifact { source: ArtifactError },
    #[snafu(display("{primary}; artifact rollback also failed: {cleanup}"))]
    ArtifactRollback {
        primary: Box<Error>,
        cleanup: ArtifactError,
    },
    #[snafu(display("stone binary writer"))]
    StoneBinaryWriter { source: StoneWriteError },
    #[snafu(display("manifest"))]
    Manifest { source: manifest::Error },
    #[snafu(display("io"))]
    Io { source: io::Error },
    #[snafu(display("verified package input {}: {source}", path.display()))]
    VerifiedInput { path: PathBuf, source: collect::Error },
    #[snafu(display("verified package inventory: {source}"))]
    VerifiedInventory { source: collect::Error },
    #[snafu(display("failed to reserve {requested} units for {resource}: {detail}"))]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[snafu(display("size overflow while totaling {resource}"))]
    SizeOverflow { resource: &'static str },
    #[snafu(display(
        "package layout target {target} from {package} ({}) is reserved for Cast system metadata",
        path.display()
    ))]
    ReservedLayoutTarget {
        target: String,
        package: String,
        path: PathBuf,
    },
    #[snafu(display(
        "duplicate package layout target {target}: {first_package} ({}) and {second_package} ({})",
        first_path.display(),
        second_path.display()
    ))]
    DuplicateLayoutTarget {
        target: String,
        first_package: String,
        first_path: PathBuf,
        second_package: String,
        second_path: PathBuf,
    },
    #[snafu(display(
        "non-directory package layout target {ancestor} from {ancestor_package} ({}) is an ancestor of {descendant} from {descendant_package} ({})",
        ancestor_path.display(),
        descendant_path.display()
    ))]
    AncestorLayoutTarget {
        ancestor: String,
        ancestor_package: String,
        ancestor_path: PathBuf,
        descendant: String,
        descendant_package: String,
        descendant_path: PathBuf,
    },
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod verification_tests;

#[cfg(test)]
pub(crate) use self::test_support::{set_test_compiler_cache, test_derivation_plan};
