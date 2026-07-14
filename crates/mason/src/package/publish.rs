// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Descriptor-rooted publication of one complete derivation bundle.

// Publication constructs and transfers raw Linux descriptors directly. Using
// std::fs::File here avoids attaching potentially misleading path context to
// descriptor-relative operations; every boundary maps I/O errors to its own
// authenticated diagnostic path.
#![allow(clippy::disallowed_types)]

use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use nix::{errno::Errno, libc};
use sha2::{Digest, Sha256};
use stone_recipe::derivation::DerivationPlan;
use thiserror::Error;

use crate::{Paths, paths::ExecutionLock};

use super::{binary_manifest_filename, frozen_architecture, jsonc_manifest_filename, stone_artefact_filename};

pub(super) const PUBLISHED_ARTEFACT_MODE: u32 = 0o444;
pub(super) const PUBLISHED_BUNDLE_MODE: u32 = 0o555;

const MAX_EMITTED_ARTEFACTS: usize = 256;
const MAX_STONE_ARTEFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ARTEFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
// The per-file ceiling still permits very large debug/runtime splits. This
// independent ceiling prevents a plan with hundreds of maximum-sized outputs
// from turning publication into an effectively unbounded operation.
const MAX_BUNDLE_BYTES: u64 = 8 * 1024 * 1024 * 1024 * 1024;
const PUBLICATION_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
const MANIFEST_VERIFICATION_DEADLINE: Duration = Duration::from_secs(5 * 60);
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const TEMPORARY_ATTEMPTS: usize = 16;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Result of publishing one complete frozen derivation bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// A new complete bundle became visible with one atomic rename.
    Published,
    /// An exact, stable bundle was already present.
    Reused,
}

/// Optional host reference checked byte-for-byte against the binary manifest
/// as part of the same publication transaction.
#[derive(Debug, Clone, Copy, Default)]
pub enum ManifestVerification<'a> {
    /// Publish without a host reference.
    #[default]
    None,
    /// Require the emitted binary manifest to match this protected host file.
    ExactBinary(&'a Path),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PublishLimits {
    max_artefacts: usize,
    max_stone_bytes: u64,
    max_manifest_bytes: u64,
    max_bundle_bytes: u64,
    deadline: Duration,
    verification_deadline: Duration,
}

impl Default for PublishLimits {
    fn default() -> Self {
        Self {
            max_artefacts: MAX_EMITTED_ARTEFACTS,
            max_stone_bytes: MAX_STONE_ARTEFACT_BYTES,
            max_manifest_bytes: MAX_MANIFEST_ARTEFACT_BYTES,
            max_bundle_bytes: MAX_BUNDLE_BYTES,
            deadline: PUBLICATION_DEADLINE,
            verification_deadline: MANIFEST_VERIFICATION_DEADLINE,
        }
    }
}

#[cfg(test)]
impl PublishLimits {
    pub(super) fn with_file_and_bundle_bytes(max_file_bytes: u64, max_bundle_bytes: u64) -> Self {
        Self {
            max_stone_bytes: max_file_bytes,
            max_manifest_bytes: max_file_bytes,
            max_bundle_bytes,
            ..Self::default()
        }
    }

    pub(super) fn with_max_artefacts(max_artefacts: usize) -> Self {
        Self {
            max_artefacts,
            ..Self::default()
        }
    }

    pub(super) fn with_manifest_verification(maximum: u64, deadline: Duration) -> Self {
        Self {
            max_manifest_bytes: maximum,
            verification_deadline: deadline,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublishCheckpoint {
    SourcesPinned,
    BeforeRename,
    AfterRename,
    BeforeReuseConfirmation,
    AfterReuseDurabilitySync,
}

/// Publish all emitted artefacts as one read-only, derivation-owned bundle.
///
/// The typed `execution_lock` is held from workspace setup through this
/// operation. Emission has stopped every build/analyzer mutator before it
/// returns. Publication relies on that cooperative single-mutator boundary for
/// the narrow identity-check/unlink interval documented by
/// [`remove_owned_entry`]. Hostile same-UID actors which ignore the lock remain
/// out of scope because Linux has no conditional unlink-by-inode primitive.
pub fn publish_artefacts(
    paths: &Paths,
    plan: &DerivationPlan,
    execution_lock: &ExecutionLock,
    staged_anchor: &File,
    verification: ManifestVerification<'_>,
) -> Result<Publication, PublishError> {
    publish_with(
        paths,
        plan,
        execution_lock,
        Some(staged_anchor),
        verification,
        PublishLimits::default(),
        |_| Ok(()),
    )
}

#[cfg(test)]
pub(super) fn publish_artefacts_with<F>(
    paths: &Paths,
    plan: &DerivationPlan,
    execution_lock: &ExecutionLock,
    verification: ManifestVerification<'_>,
    limits: PublishLimits,
    hook: F,
) -> Result<Publication, PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    publish_with(paths, plan, execution_lock, None, verification, limits, hook)
}

fn publish_with<F>(
    paths: &Paths,
    plan: &DerivationPlan,
    execution_lock: &ExecutionLock,
    staged_anchor: Option<&File>,
    verification: ManifestVerification<'_>,
    limits: PublishLimits,
    mut hook: F,
) -> Result<Publication, PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    plan.validate().map_err(PublishError::InvalidFrozenPlan)?;
    paths.require_plan(plan).map_err(PublishError::InvalidFrozenPaths)?;
    paths
        .require_execution_lock(execution_lock, plan)
        .map_err(PublishError::InvalidExecutionLock)?;
    let specs = bundle_specs(plan, limits)?;
    let expected = expected_names(&specs)?;
    let deadline = Deadline::new(limits.deadline);
    let manifest_name = binary_manifest_filename(frozen_architecture(&plan.package.architecture)).into_bytes();
    validate_component(&manifest_name, "generated binary manifest")?;

    let staged_root = match staged_anchor {
        Some(anchor) => DirectoryHandle::open_pinned_root(&paths.artefacts().host, anchor, "staged")?,
        #[cfg(test)]
        None => DirectoryHandle::open_root(&paths.artefacts().host, "staged")?,
        #[cfg(not(test))]
        None => unreachable!("production artefact publication requires a retained descriptor"),
    };
    let mut staged = VerifiedBundle::open(staged_root, &specs, "staged", None, limits.max_bundle_bytes, &deadline)?;
    let (reference, expected_manifest_digest) = match verification {
        ManifestVerification::None => (None, None),
        ManifestVerification::ExactBinary(path) => {
            // The short verification budget covers only opening and streaming
            // the exact host comparison. The potentially multi-TiB bundle
            // copy remains governed by the independent publication deadline.
            let verification_deadline = Deadline::new(limits.verification_deadline);
            let mut reference = ReferenceManifest::open(path, limits.max_manifest_bytes, &verification_deadline)?;
            staged.reject_manifest_alias(&manifest_name, &reference)?;
            let digest = staged.compare_manifest(&manifest_name, &mut reference, &verification_deadline)?;
            (Some(reference), Some(digest))
        }
    };
    let output = DirectoryHandle::open_root(paths.output_dir(), "output")?;
    staged.root.require_path_identity("staged")?;
    output.require_path_identity("output")?;
    hook(PublishCheckpoint::SourcesPinned)?;

    let derivation_id = plan.derivation_id();
    let final_name = derivation_id.as_str().as_bytes();
    validate_component(final_name, "derivation bundle")?;
    if let Some(final_root) = output.open_child_directory(
        final_name,
        "published bundle",
        PUBLISHED_BUNDLE_MODE,
        Some(plan.source_date_epoch),
    )? {
        let mut published = VerifiedBundle::open(
            final_root,
            &specs,
            "published",
            Some(plan.source_date_epoch),
            limits.max_bundle_bytes,
            &deadline,
        )?;
        if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
            reference.require_digest(digest)?;
            published.verify_manifest_digest(
                &manifest_name,
                digest,
                &reference.path,
                &Deadline::new(limits.verification_deadline),
            )?;
            reference.require_digest(digest)?;
        }
        verify_reuse(
            &mut staged,
            &mut published,
            ReuseContext {
                expected: &expected,
                output: &output,
                final_name,
                source_date_epoch: plan.source_date_epoch,
                deadline: &deadline,
            },
            &mut hook,
        )?;
        if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
            reference.require_digest(digest)?;
            published.verify_manifest_digest(
                &manifest_name,
                digest,
                &reference.path,
                &Deadline::new(limits.verification_deadline),
            )?;
            staged.verify_manifest_digest(
                &manifest_name,
                digest,
                &reference.path,
                &Deadline::new(limits.verification_deadline),
            )?;
            reference.require_digest(digest)?;
        }
        return Ok(Publication::Reused);
    }

    let mut temporary = TemporaryBundle::create(&output, final_name, specs.len(), plan.source_date_epoch, &deadline)?;
    let preparation = (|| {
        for (index, source) in staged.entries.iter_mut().enumerate() {
            let digest = temporary.copy_from(index, source, &specs[index], &deadline)?;
            if let Some(expected) = source.digest
                && expected != digest
            {
                return Err(PublishError::ArtifactChanged {
                    path: source.path.clone(),
                });
            }
            source.digest = Some(digest);
        }
        temporary.seal(&expected, &deadline)?;
        verify_digest_round(&mut staged, &expected, &deadline)?;
        hook(PublishCheckpoint::BeforeRename)?;
        if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
            reference.require_digest(digest)?;
            temporary.verify_manifest_digest(
                &manifest_name,
                digest,
                &reference.path,
                &Deadline::new(limits.verification_deadline),
            )?;
            staged.verify_manifest_digest(
                &manifest_name,
                digest,
                &reference.path,
                &Deadline::new(limits.verification_deadline),
            )?;
            reference.require_digest(digest)?;
        }
        Ok(())
    })();
    if let Err(primary) = preparation {
        return Err(temporary.rollback_error(primary));
    }

    match temporary.install() {
        Ok(InstallOutcome::Installed) => {
            let publication = (|| {
                hook(PublishCheckpoint::AfterRename)?;
                temporary.verify_final(&expected, &deadline)?;
                if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
                    reference.require_digest(digest)?;
                    temporary.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    reference.require_digest(digest)?;
                }
                verify_digest_round(&mut staged, &expected, &deadline)?;
                output.sync("output after bundle rename")?;
                temporary.verify_final(&expected, &deadline)?;
                if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
                    reference.require_digest(digest)?;
                    temporary.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    staged.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    reference.require_digest(digest)?;
                }
                staged.root.require_path_identity("staged")?;
                output.require_path_identity("output")?;
                Ok(())
            })();
            if let Err(primary) = publication {
                return Err(temporary.rollback_error(primary));
            }
            temporary.commit();
            Ok(Publication::Published)
        }
        Ok(InstallOutcome::AlreadyExists) => {
            let verification = (|| {
                // This build already copied and authenticated one exact staged
                // byte set into `temporary`. Do not discard that witness when
                // another publisher wins the rename: otherwise a mutation of
                // staging followed by a matching competing bundle could make
                // us reuse bytes different from the ones this build prepared.
                verify_digest_round(&mut staged, &expected, &deadline)?;
                temporary.verify_entries(&expected, &deadline)?;
                if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
                    reference.require_digest(digest)?;
                    temporary.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    staged.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    reference.require_digest(digest)?;
                }
                let final_root = output
                    .open_child_directory(
                        final_name,
                        "published bundle",
                        PUBLISHED_BUNDLE_MODE,
                        Some(plan.source_date_epoch),
                    )?
                    .ok_or_else(|| PublishError::OwnershipChanged {
                        path: output.display(final_name),
                    })?;
                let mut published = VerifiedBundle::open(
                    final_root,
                    &specs,
                    "published",
                    Some(plan.source_date_epoch),
                    limits.max_bundle_bytes,
                    &deadline,
                )?;
                if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
                    reference.require_digest(digest)?;
                    published.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    reference.require_digest(digest)?;
                }
                verify_reuse(
                    &mut staged,
                    &mut published,
                    ReuseContext {
                        expected: &expected,
                        output: &output,
                        final_name,
                        source_date_epoch: plan.source_date_epoch,
                        deadline: &deadline,
                    },
                    &mut hook,
                )?;
                if let (Some(reference), Some(digest)) = (&reference, expected_manifest_digest) {
                    reference.require_digest(digest)?;
                    published.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    staged.verify_manifest_digest(
                        &manifest_name,
                        digest,
                        &reference.path,
                        &Deadline::new(limits.verification_deadline),
                    )?;
                    reference.require_digest(digest)?;
                }
                Ok(())
            })();
            if let Err(primary) = verification {
                return Err(temporary.rollback_error(primary));
            }
            temporary.abort().map_err(|cleanup| PublishError::Rollback {
                primary: Box::new(PublishError::ConcurrentPublication),
                cleanup: Box::new(cleanup),
            })?;
            Ok(Publication::Reused)
        }
        Err(primary) => Err(temporary.rollback_error(primary)),
    }
}

#[derive(Debug, Clone, Copy)]
struct ReuseContext<'a> {
    expected: &'a [Vec<u8>],
    output: &'a DirectoryHandle,
    final_name: &'a [u8],
    source_date_epoch: i64,
    deadline: &'a Deadline,
}

fn verify_reuse<F>(
    staged: &mut VerifiedBundle,
    published: &mut VerifiedBundle,
    context: ReuseContext<'_>,
    hook: &mut F,
) -> Result<(), PublishError>
where
    F: FnMut(PublishCheckpoint) -> Result<(), PublishError>,
{
    let ReuseContext {
        expected,
        output,
        final_name,
        source_date_epoch,
        deadline,
    } = context;
    let staged_first = digest_round(staged, expected, deadline)?;
    let published_first = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_first, &published_first)?;
    hook(PublishCheckpoint::BeforeReuseConfirmation)?;
    let staged_second = digest_round(staged, expected, deadline)?;
    let published_second = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_second, &published_second)?;
    if staged_first != staged_second || published_first != published_second {
        return Err(PublishError::ArtifactChanged {
            path: published.root.path.clone(),
        });
    }
    for entry in &published.entries {
        deadline.check("sync reused published artefact")?;
        entry.file.sync_all().map_err(|source| PublishError::SyncFile {
            path: entry.path.clone(),
            source,
        })?;
    }
    published.root.sync("reused published")?;
    output.sync("output after reuse confirmation")?;
    hook(PublishCheckpoint::AfterReuseDurabilitySync)?;
    let staged_durable = digest_round(staged, expected, deadline)?;
    let published_durable = digest_round(published, expected, deadline)?;
    compare_digests(staged, published, &staged_durable, &published_durable)?;
    if staged_second != staged_durable || published_second != published_durable {
        return Err(PublishError::ArtifactChanged {
            path: published.root.path.clone(),
        });
    }
    published.root.require_path_identity("published")?;
    output.require_named_directory(
        final_name,
        published.root.identity,
        PUBLISHED_BUNDLE_MODE,
        Some(source_date_epoch),
    )?;
    staged.root.require_path_identity("staged")?;
    output.require_path_identity("output")
}

fn compare_digests(
    staged: &VerifiedBundle,
    published: &VerifiedBundle,
    staged_digests: &[[u8; 32]],
    published_digests: &[[u8; 32]],
) -> Result<(), PublishError> {
    for index in 0..staged_digests.len() {
        if staged.entries[index].witness.length != published.entries[index].witness.length
            || staged_digests[index] != published_digests[index]
        {
            return Err(PublishError::ContentMismatch {
                staged: staged.entries[index].path.clone(),
                published: published.entries[index].path.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Deadline {
    start: Instant,
    duration: Duration,
}

impl Deadline {
    fn new(duration: Duration) -> Self {
        Self {
            start: Instant::now(),
            duration,
        }
    }

    fn check(&self, operation: &'static str) -> Result<(), PublishError> {
        if self.start.elapsed() >= self.duration {
            Err(PublishError::Deadline {
                operation,
                limit: self.duration,
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
struct BundleSpec {
    name: Vec<u8>,
    maximum: u64,
}

fn bundle_specs(plan: &DerivationPlan, limits: PublishLimits) -> Result<Vec<BundleSpec>, PublishError> {
    let count = plan.outputs.len().checked_add(2).ok_or(PublishError::ResourceLimit {
        resource: "published artefact count",
        limit: limits.max_artefacts,
    })?;
    if count > limits.max_artefacts {
        return Err(PublishError::ResourceLimit {
            resource: "published artefact count",
            limit: limits.max_artefacts,
        });
    }
    let architecture = frozen_architecture(&plan.package.architecture);
    let mut specs = Vec::new();
    specs
        .try_reserve_exact(count)
        .map_err(|source| PublishError::Allocation {
            resource: "published artefact specifications",
            requested: count,
            detail: source.to_string(),
        })?;
    for output in &plan.outputs {
        specs.push(BundleSpec {
            name: copy_bytes(
                stone_artefact_filename(
                    &output.package_name,
                    &plan.package.version,
                    plan.package.source_release,
                    plan.package.build_release,
                    architecture,
                )
                .as_bytes(),
                "published Stone name",
            )?,
            maximum: limits.max_stone_bytes,
        });
    }
    for name in [
        binary_manifest_filename(architecture),
        jsonc_manifest_filename(architecture),
    ] {
        specs.push(BundleSpec {
            name: copy_bytes(name.as_bytes(), "published manifest name")?,
            maximum: limits.max_manifest_bytes,
        });
    }
    specs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    for spec in &specs {
        validate_component(&spec.name, "published artefact")?;
    }
    for pair in specs.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(PublishError::DuplicateName {
                name: OsString::from_vec(copy_bytes(&pair[0].name, "duplicate artefact name")?),
            });
        }
    }
    Ok(specs)
}

fn expected_names(specs: &[BundleSpec]) -> Result<Vec<Vec<u8>>, PublishError> {
    let mut names = Vec::new();
    names
        .try_reserve_exact(specs.len())
        .map_err(|source| PublishError::Allocation {
            resource: "expected published artefact names",
            requested: specs.len(),
            detail: source.to_string(),
        })?;
    for spec in specs {
        names.push(copy_bytes(&spec.name, "expected published artefact name")?);
    }
    Ok(names)
}

#[cfg(test)]
pub(super) fn expected_bundle_files(plan: &DerivationPlan) -> std::collections::BTreeSet<OsString> {
    bundle_specs(plan, PublishLimits::default())
        .expect("test plan has valid publication specifications")
        .into_iter()
        .map(|spec| OsString::from_vec(spec.name))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    user: u32,
    group: u32,
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            user: metadata.uid(),
            group: metadata.gid(),
        }
    }
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

#[derive(Debug)]
struct DirectoryHandle {
    path: PathBuf,
    file: File,
    identity: Identity,
}

impl DirectoryHandle {
    fn open_root(path: &Path, role: &'static str) -> Result<Self, PublishError> {
        Self::open_root_with_policy(path, role, false)
    }

    fn open_pinned_root(path: &Path, pinned: &File, role: &'static str) -> Result<Self, PublishError> {
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make pinned publication root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = pinned.try_clone().map_err(|source| PublishError::Io {
            operation: "duplicate pinned publication root",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect pinned publication root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        require_effective_owner(role, &path, &metadata)?;
        require_protected_root_mode(role, &path, &metadata)?;
        let root = Self {
            path,
            file,
            identity: Identity::from_metadata(&metadata),
        };
        root.require_path_identity(role)?;
        Ok(root)
    }

    fn open_reference_root(path: &Path) -> Result<Self, PublishError> {
        Self::open_root_with_policy(path, "expected manifest parent", true)
    }

    fn open_root_with_policy(path: &Path, role: &'static str, reference: bool) -> Result<Self, PublishError> {
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make publication root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened publication root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        if reference {
            require_reference_owner(&path, &metadata)?;
        } else {
            require_effective_owner(role, &path, &metadata)?;
        }
        require_protected_root_mode(role, &path, &metadata)?;
        Ok(Self {
            path,
            file,
            identity: Identity::from_metadata(&metadata),
        })
    }

    fn display(&self, name: &[u8]) -> PathBuf {
        self.path.join(OsStr::from_bytes(name))
    }

    fn metadata(&self, operation: &'static str) -> Result<Metadata, PublishError> {
        self.file.metadata().map_err(|source| PublishError::Io {
            operation,
            path: self.path.clone(),
            source,
        })
    }

    fn require_path_identity(&self, role: &'static str) -> Result<(), PublishError> {
        self.require_path_identity_with_policy(role, false)
    }

    fn require_reference_path_identity(&self) -> Result<(), PublishError> {
        self.require_path_identity_with_policy("expected manifest parent", true)
    }

    fn require_path_identity_with_policy(&self, role: &'static str, reference: bool) -> Result<(), PublishError> {
        let reopened = openat2_file(
            libc::AT_FDCWD,
            self.path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| PublishError::Io {
            operation: "reopen publication root",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| PublishError::Io {
            operation: "inspect reopened publication root",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(PublishError::OwnershipChanged {
                path: self.path.clone(),
            });
        }
        if reference {
            require_reference_owner(&self.path, &metadata)?;
        } else {
            require_effective_owner(role, &self.path, &metadata)?;
        }
        require_protected_root_mode(role, &self.path, &metadata)?;
        Ok(())
    }

    fn inspect(&self, name: &[u8], operation: &'static str) -> Result<Option<(Metadata, Identity)>, PublishError> {
        let path = self.display(name);
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
        ) {
            Ok(file) => {
                let metadata = file.metadata().map_err(|source| PublishError::Io {
                    operation,
                    path,
                    source,
                })?;
                let identity = Identity::from_metadata(&metadata);
                Ok(Some((metadata, identity)))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PublishError::Io {
                operation,
                path,
                source,
            }),
        }
    }

    fn open_child_directory(
        &self,
        name: &[u8],
        role: &'static str,
        expected_mode: u32,
        expected_mtime: Option<i64>,
    ) -> Result<Option<Self>, PublishError> {
        let path = self.display(name);
        let Some((before, identity)) = self.inspect(name, "inspect publication child")? else {
            return Ok(None);
        };
        if !before.file_type().is_dir() {
            return Err(PublishError::UnexpectedRoot { role, path });
        }
        require_effective_owner(role, &path, &before)?;
        require_mode(role, &path, &before, expected_mode)?;
        require_directory_timestamp(&path, &before, expected_mtime)?;
        let file = openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication child directory",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened publication child directory",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_effective_owner(role, &path, &metadata)?;
        require_mode(role, &path, &metadata, expected_mode)?;
        require_directory_timestamp(&path, &metadata, expected_mtime)?;
        Ok(Some(Self { path, file, identity }))
    }

    fn require_named_directory(
        &self,
        name: &[u8],
        identity: Identity,
        mode: u32,
        expected_mtime: Option<i64>,
    ) -> Result<(), PublishError> {
        let path = self.display(name);
        let Some((metadata, found)) = self.inspect(name, "authenticate named published bundle")? else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if !metadata.file_type().is_dir() || found != identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_effective_owner("published bundle", &path, &metadata)?;
        require_mode("published bundle", &path, &metadata, mode)?;
        require_directory_timestamp(&path, &metadata, expected_mtime)
    }

    fn require_inventory(
        &self,
        role: &'static str,
        expected: &[Vec<u8>],
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let maximum = expected.len().checked_add(1).ok_or(PublishError::ResourceLimit {
            resource: "publication directory entries",
            limit: expected.len(),
        })?;
        let before = DirectoryStamp::from_metadata(&self.metadata("inspect directory before inventory")?);
        let first = self.read_names(maximum, deadline)?;
        let between = DirectoryStamp::from_metadata(&self.metadata("inspect directory between inventories")?);
        if before != between {
            return Err(PublishError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        let second = self.read_names(maximum, deadline)?;
        let after = DirectoryStamp::from_metadata(&self.metadata("inspect directory after inventory")?);
        if between != after || first != second {
            return Err(PublishError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        if first != expected {
            return Err(PublishError::FrozenFileSetMismatch {
                role,
                path: self.path.clone(),
                expected: os_names(expected)?,
                found: os_names(&first)?,
            });
        }
        Ok(())
    }

    fn read_names(&self, maximum: usize, deadline: &Deadline) -> Result<Vec<Vec<u8>>, PublishError> {
        let cursor = openat2_file(
            self.file.as_raw_fd(),
            b".",
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open publication directory cursor",
            path: self.path.clone(),
            source,
        })?;
        let stream = DirectoryStream::from_file(cursor, &self.path)?;
        let mut names = Vec::new();
        names.try_reserve(maximum).map_err(|source| PublishError::Allocation {
            resource: "publication directory names",
            requested: maximum,
            detail: source.to_string(),
        })?;
        loop {
            deadline.check("enumerate publication directory")?;
            Errno::clear();
            // SAFETY: the live directory stream is exclusively used here.
            let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    break;
                }
                return Err(PublishError::Io {
                    operation: "enumerate publication directory",
                    path: self.path.clone(),
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: d_name is NUL-terminated and live until the next call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if names.len() == maximum {
                return Err(PublishError::ResourceLimit {
                    resource: "publication directory entries",
                    limit: maximum,
                });
            }
            names.push(copy_bytes(name, "publication directory entry name")?);
        }
        names.sort_unstable();
        Ok(names)
    }

    fn sync(&self, operation: &'static str) -> Result<(), PublishError> {
        self.file.sync_all().map_err(|source| PublishError::SyncDirectory {
            role: operation,
            path: self.path.clone(),
            source,
        })
    }
}

struct DirectoryStream(NonNull<libc::DIR>);

impl DirectoryStream {
    fn from_file(file: File, path: &Path) -> Result<Self, PublishError> {
        let descriptor = file.into_raw_fd();
        // SAFETY: descriptor is fresh and fdopendir consumes it on success.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and did not consume descriptor.
                unsafe { libc::close(descriptor) };
                Err(PublishError::Io {
                    operation: "open publication directory stream",
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the DIR pointer.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

#[derive(Debug)]
struct VerifiedEntry {
    name: Vec<u8>,
    path: PathBuf,
    file: File,
    witness: FileWitness,
    digest: Option<[u8; 32]>,
}

impl VerifiedEntry {
    fn open(
        root: &DirectoryHandle,
        spec: &BundleSpec,
        role: &'static str,
        expected_mtime: Option<i64>,
    ) -> Result<Self, PublishError> {
        let path = root.display(&spec.name);
        let Some((named_metadata, named_identity)) = root.inspect(&spec.name, "inspect verified bundle artefact")?
        else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if !named_metadata.file_type().is_file() {
            return Err(PublishError::UnexpectedEntry { role, path });
        }
        let file = openat2_file(
            root.file.as_raw_fd(),
            &spec.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open verified bundle artefact",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect verified bundle artefact",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != named_identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_regular(role, &path, &metadata, spec.maximum, expected_mtime)?;
        Ok(Self {
            name: copy_bytes(&spec.name, "verified artefact name")?,
            path,
            file,
            witness: FileWitness::from_metadata(&metadata),
            digest: None,
        })
    }

    fn require_named(&self, root: &DirectoryHandle, role: &'static str) -> Result<(), PublishError> {
        let Some((metadata, identity)) = root.inspect(&self.name, "reopen verified bundle artefact")? else {
            return Err(PublishError::OwnershipChanged {
                path: self.path.clone(),
            });
        };
        if identity != self.witness.identity || FileWitness::from_metadata(&metadata) != self.witness {
            return Err(PublishError::ArtifactChanged {
                path: self.path.clone(),
            });
        }
        let _ = role;
        Ok(())
    }

    fn digest(&mut self, deadline: &Deadline) -> Result<[u8; 32], PublishError> {
        hash_file(&mut self.file, &self.path, self.witness, deadline)
    }
}

#[derive(Debug)]
struct ReferenceManifest {
    parent: DirectoryHandle,
    name: Vec<u8>,
    path: PathBuf,
    file: File,
    witness: FileWitness,
    digest: Option<[u8; 32]>,
}

impl ReferenceManifest {
    fn open(path: &Path, maximum: u64, deadline: &Deadline) -> Result<Self, PublishError> {
        deadline.check("open expected binary manifest")?;
        let path = std::path::absolute(path).map_err(|source| PublishError::Io {
            operation: "make expected binary manifest path absolute",
            path: path.to_owned(),
            source,
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| PublishError::InvalidReferencePath { path: path.clone() })?
            .as_bytes();
        validate_component(name, "expected binary manifest")?;
        let name = copy_bytes(name, "expected binary manifest name")?;
        let parent_path = path
            .parent()
            .ok_or_else(|| PublishError::InvalidReferencePath { path: path.clone() })?;
        let parent = DirectoryHandle::open_reference_root(parent_path)?;
        let Some((named_metadata, named_identity)) = parent.inspect(&name, "inspect expected binary manifest")? else {
            return Err(PublishError::MissingReferenceManifest { path });
        };
        require_reference_regular(&path, &named_metadata, maximum)?;
        let file = openat2_file(
            parent.file.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation: "open expected binary manifest",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect opened expected binary manifest",
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != named_identity {
            return Err(PublishError::OwnershipChanged { path });
        }
        require_reference_regular(&path, &metadata, maximum)?;
        let reference = Self {
            parent,
            name,
            path,
            file,
            witness: FileWitness::from_metadata(&metadata),
            digest: None,
        };
        reference.require_stable()?;
        Ok(reference)
    }

    fn require_stable(&self) -> Result<(), PublishError> {
        let Some((metadata, identity)) = self
            .parent
            .inspect(&self.name, "authenticate expected binary manifest")?
        else {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        };
        if identity != self.witness.identity || FileWitness::from_metadata(&metadata) != self.witness {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        let descriptor = self.file.metadata().map_err(|source| PublishError::Io {
            operation: "inspect retained expected binary manifest",
            path: self.path.clone(),
            source,
        })?;
        if FileWitness::from_metadata(&descriptor) != self.witness {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        self.parent.require_reference_path_identity()
    }

    fn require_digest(&self, expected: [u8; 32]) -> Result<(), PublishError> {
        self.require_stable()?;
        if self.digest == Some(expected) {
            Ok(())
        } else {
            Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            })
        }
    }

    fn compare_file(
        &mut self,
        generated: &mut File,
        generated_path: &Path,
        generated_witness: FileWitness,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        self.require_stable()?;
        let digest = compare_manifest_files(
            generated,
            generated_path,
            generated_witness,
            &mut self.file,
            &self.path,
            self.witness,
            deadline,
        )?;
        self.require_stable()?;
        if let Some(expected) = self.digest
            && expected != digest
        {
            return Err(PublishError::ReferenceManifestChanged {
                path: self.path.clone(),
            });
        }
        self.digest = Some(digest);
        Ok(digest)
    }
}

fn require_reference_regular(path: &Path, metadata: &Metadata, maximum: u64) -> Result<(), PublishError> {
    if !metadata.file_type().is_file() {
        return Err(PublishError::UnexpectedEntry {
            role: "expected manifest",
            path: path.to_owned(),
        });
    }
    require_reference_owner(path, metadata)?;
    let mode = metadata.mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(PublishError::WritableReferenceManifest {
            path: path.to_owned(),
            found: mode,
        });
    }
    if metadata.len() > maximum {
        return Err(PublishError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    Ok(())
}

fn compare_manifest_files(
    generated: &mut File,
    generated_path: &Path,
    generated_witness: FileWitness,
    reference: &mut File,
    reference_path: &Path,
    reference_witness: FileWitness,
    deadline: &Deadline,
) -> Result<[u8; 32], PublishError> {
    require_file_witness(
        generated,
        generated_path,
        generated_witness,
        "generated manifest before comparison",
    )?;
    require_file_witness(
        reference,
        reference_path,
        reference_witness,
        "expected manifest before comparison",
    )?;
    if generated_witness.length != reference_witness.length {
        require_file_witness(
            generated,
            generated_path,
            generated_witness,
            "generated manifest after comparison",
        )?;
        require_file_witness(
            reference,
            reference_path,
            reference_witness,
            "expected manifest after comparison",
        )?;
        return Err(PublishError::ManifestVerificationMismatch {
            generated: generated_path.to_owned(),
            expected: reference_path.to_owned(),
        });
    }
    generated
        .seek(SeekFrom::Start(0))
        .map_err(|source| PublishError::Read {
            path: generated_path.to_owned(),
            source,
        })?;
    reference
        .seek(SeekFrom::Start(0))
        .map_err(|source| PublishError::Read {
            path: reference_path.to_owned(),
            source,
        })?;
    let mut generated_hash = Sha256::new();
    let mut reference_hash = Sha256::new();
    let mut generated_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut reference_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut remaining = generated_witness.length;
    let mut mismatch = false;
    while remaining > 0 {
        deadline.check("compare binary manifests")?;
        let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(COPY_BUFFER_BYTES);
        read_exact_manifest_chunk(generated, &mut generated_buffer[..amount], generated_path, deadline)?;
        read_exact_manifest_chunk(reference, &mut reference_buffer[..amount], reference_path, deadline)?;
        generated_hash.update(&generated_buffer[..amount]);
        reference_hash.update(&reference_buffer[..amount]);
        if generated_buffer[..amount] != reference_buffer[..amount] {
            mismatch = true;
            break;
        }
        remaining -= amount as u64;
    }
    if !mismatch {
        let mut generated_trailing = [0_u8; 1];
        let mut reference_trailing = [0_u8; 1];
        if generated
            .read(&mut generated_trailing)
            .map_err(|source| PublishError::Read {
                path: generated_path.to_owned(),
                source,
            })?
            != 0
        {
            return Err(PublishError::ArtifactChanged {
                path: generated_path.to_owned(),
            });
        }
        if reference
            .read(&mut reference_trailing)
            .map_err(|source| PublishError::Read {
                path: reference_path.to_owned(),
                source,
            })?
            != 0
        {
            return Err(PublishError::ReferenceManifestChanged {
                path: reference_path.to_owned(),
            });
        }
    }
    deadline.check("finish binary manifest comparison")?;
    require_file_witness(
        generated,
        generated_path,
        generated_witness,
        "generated manifest after comparison",
    )?;
    require_file_witness(
        reference,
        reference_path,
        reference_witness,
        "expected manifest after comparison",
    )?;
    let generated_digest: [u8; 32] = generated_hash.finalize().into();
    let reference_digest: [u8; 32] = reference_hash.finalize().into();
    if mismatch || generated_digest != reference_digest {
        return Err(PublishError::ManifestVerificationMismatch {
            generated: generated_path.to_owned(),
            expected: reference_path.to_owned(),
        });
    }
    Ok(generated_digest)
}

fn read_exact_manifest_chunk(
    file: &mut File,
    mut buffer: &mut [u8],
    path: &Path,
    deadline: &Deadline,
) -> Result<(), PublishError> {
    while !buffer.is_empty() {
        deadline.check("read binary manifest")?;
        let read = file.read(buffer).map_err(|source| PublishError::Read {
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            return Err(PublishError::ArtifactChanged { path: path.to_owned() });
        }
        buffer = &mut buffer[read..];
    }
    Ok(())
}

fn require_file_witness(
    file: &File,
    path: &Path,
    witness: FileWitness,
    operation: &'static str,
) -> Result<(), PublishError> {
    let metadata = file.metadata().map_err(|source| PublishError::Io {
        operation,
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&metadata) == witness {
        Ok(())
    } else {
        Err(PublishError::ArtifactChanged { path: path.to_owned() })
    }
}

#[derive(Debug)]
struct VerifiedBundle {
    root: DirectoryHandle,
    entries: Vec<VerifiedEntry>,
}

impl VerifiedBundle {
    fn open(
        root: DirectoryHandle,
        specs: &[BundleSpec],
        role: &'static str,
        expected_mtime: Option<i64>,
        max_bundle_bytes: u64,
        deadline: &Deadline,
    ) -> Result<Self, PublishError> {
        let expected = expected_names(specs)?;
        root.require_inventory(role, &expected, deadline)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(specs.len())
            .map_err(|source| PublishError::Allocation {
                resource: "verified bundle entries",
                requested: specs.len(),
                detail: source.to_string(),
            })?;
        let mut total = 0_u64;
        for spec in specs {
            deadline.check("open published bundle entries")?;
            let entry = VerifiedEntry::open(&root, spec, role, expected_mtime)?;
            total = total
                .checked_add(entry.witness.length)
                .ok_or(PublishError::BundleTooLarge {
                    maximum: max_bundle_bytes,
                    found: u64::MAX,
                })?;
            if total > max_bundle_bytes {
                return Err(PublishError::BundleTooLarge {
                    maximum: max_bundle_bytes,
                    found: total,
                });
            }
            entries.push(entry);
        }
        root.require_inventory(role, &expected, deadline)?;
        for entry in &entries {
            entry.require_named(&root, role)?;
        }
        root.require_path_identity(role)?;
        Ok(Self { root, entries })
    }

    fn reject_manifest_alias(&self, name: &[u8], reference: &ReferenceManifest) -> Result<(), PublishError> {
        let entry =
            self.entries
                .iter()
                .find(|entry| entry.name == name)
                .ok_or_else(|| PublishError::OwnershipChanged {
                    path: self.root.display(name),
                })?;
        if entry.witness.identity == reference.witness.identity {
            Err(PublishError::ReferenceAliasesStagedManifest {
                generated: entry.path.clone(),
                expected: reference.path.clone(),
            })
        } else {
            Ok(())
        }
    }

    fn compare_manifest(
        &mut self,
        name: &[u8],
        reference: &mut ReferenceManifest,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.root.display(name),
            })?;
        let root = &self.root;
        let entry = &mut self.entries[index];
        entry.require_named(root, "verified binary manifest")?;
        let digest = reference.compare_file(&mut entry.file, &entry.path, entry.witness, deadline)?;
        entry.require_named(root, "verified binary manifest")?;
        if let Some(expected) = entry.digest
            && expected != digest
        {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
        entry.digest = Some(digest);
        Ok(digest)
    }

    fn verify_manifest_digest(
        &mut self,
        name: &[u8],
        expected_digest: [u8; 32],
        expected_path: &Path,
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.root.display(name),
            })?;
        entry.require_named(&self.root, "verified binary manifest")?;
        let digest = entry.digest(deadline)?;
        entry.require_named(&self.root, "verified binary manifest")?;
        if let Some(previous) = entry.digest
            && previous != digest
        {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
        if digest != expected_digest {
            return Err(PublishError::ManifestVerificationMismatch {
                generated: entry.path.clone(),
                expected: expected_path.to_owned(),
            });
        }
        entry.digest = Some(digest);
        Ok(())
    }
}

fn digest_round(
    bundle: &mut VerifiedBundle,
    expected: &[Vec<u8>],
    deadline: &Deadline,
) -> Result<Vec<[u8; 32]>, PublishError> {
    bundle.root.require_inventory("verified", expected, deadline)?;
    let mut digests = Vec::new();
    digests
        .try_reserve_exact(bundle.entries.len())
        .map_err(|source| PublishError::Allocation {
            resource: "bundle digests",
            requested: bundle.entries.len(),
            detail: source.to_string(),
        })?;
    for entry in &mut bundle.entries {
        entry.require_named(&bundle.root, "verified")?;
        digests.push(entry.digest(deadline)?);
        entry.require_named(&bundle.root, "verified")?;
    }
    bundle.root.require_inventory("verified", expected, deadline)?;
    bundle.root.require_path_identity("verified")?;
    Ok(digests)
}

fn verify_digest_round(
    bundle: &mut VerifiedBundle,
    expected: &[Vec<u8>],
    deadline: &Deadline,
) -> Result<(), PublishError> {
    let digests = digest_round(bundle, expected, deadline)?;
    for (entry, digest) in bundle.entries.iter().zip(digests) {
        if entry.digest != Some(digest) {
            return Err(PublishError::ArtifactChanged {
                path: entry.path.clone(),
            });
        }
    }
    Ok(())
}

fn hash_file(
    file: &mut File,
    path: &Path,
    witness: FileWitness,
    deadline: &Deadline,
) -> Result<[u8; 32], PublishError> {
    let before = file.metadata().map_err(|source| PublishError::Io {
        operation: "inspect artefact before digest",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&before) != witness {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    file.seek(SeekFrom::Start(0)).map_err(|source| PublishError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut remaining = witness.length;
    while remaining > 0 {
        deadline.check("digest published artefact")?;
        let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = file.read(&mut buffer[..amount]).map_err(|source| PublishError::Read {
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            return Err(PublishError::ArtifactChanged { path: path.to_owned() });
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing).map_err(|source| PublishError::Read {
        path: path.to_owned(),
        source,
    })? != 0
    {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    let after = file.metadata().map_err(|source| PublishError::Io {
        operation: "inspect artefact after digest",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&after) != witness {
        return Err(PublishError::ArtifactChanged { path: path.to_owned() });
    }
    Ok(hasher.finalize().into())
}

#[derive(Debug)]
struct OwnedEntry {
    name: Vec<u8>,
    identity: Identity,
    witness: Option<FileWitness>,
    digest: Option<[u8; 32]>,
    file: Option<File>,
}

impl OwnedEntry {
    fn require_named(&self, directory: &DirectoryHandle, operation: &'static str) -> Result<(), PublishError> {
        let path = directory.display(&self.name);
        let witness = self
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        let Some((metadata, identity)) = directory.inspect(&self.name, operation)? else {
            return Err(PublishError::OwnershipChanged { path });
        };
        if identity != self.identity || FileWitness::from_metadata(&metadata) != witness {
            return Err(PublishError::ArtifactChanged { path });
        }
        Ok(())
    }

    fn open_readonly(&self, directory: &DirectoryHandle, operation: &'static str) -> Result<File, PublishError> {
        let path = directory.display(&self.name);
        let witness = self
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        self.require_named(directory, operation)?;
        let file = openat2_file(
            directory.file.as_raw_fd(),
            &self.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
        )
        .map_err(|source| PublishError::Io {
            operation,
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| PublishError::Io {
            operation,
            path: path.clone(),
            source,
        })?;
        if Identity::from_metadata(&metadata) != self.identity || FileWitness::from_metadata(&metadata) != witness {
            return Err(PublishError::ArtifactChanged { path });
        }
        self.require_named(directory, operation)?;
        Ok(file)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleLocation {
    Temporary,
    Published,
}

#[derive(Debug)]
struct TemporaryBundle<'a> {
    output: &'a DirectoryHandle,
    directory: DirectoryHandle,
    temporary_name: Vec<u8>,
    final_name: Vec<u8>,
    entries: Vec<OwnedEntry>,
    source_date_epoch: i64,
    location: BundleLocation,
    active: bool,
}

impl<'a> TemporaryBundle<'a> {
    fn create(
        output: &'a DirectoryHandle,
        final_name: &[u8],
        entry_capacity: usize,
        source_date_epoch: i64,
        deadline: &Deadline,
    ) -> Result<Self, PublishError> {
        output.require_path_identity("output")?;
        let final_name = copy_bytes(final_name, "final publication bundle name")?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_capacity)
            .map_err(|source| PublishError::Allocation {
                resource: "owned publication entries",
                requested: entry_capacity,
                detail: source.to_string(),
            })?;
        let mut last_collision = None;
        for _ in 0..TEMPORARY_ATTEMPTS {
            deadline.check("create private publication bundle")?;
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".mason-publish-{}-{}-{sequence}",
                std::process::id(),
                hex_prefix(&final_name)
            )
            .into_bytes();
            validate_component(&name, "temporary publication bundle")?;
            let path = output.display(&name);
            let c_name = c_name(&name, &path)?;
            // SAFETY: output and the NUL-terminated component remain live.
            if unsafe { libc::mkdirat(output.file.as_raw_fd(), c_name.as_ptr(), 0o700) } == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::AlreadyExists {
                    last_collision = Some(source);
                    continue;
                }
                return Err(PublishError::CreateTemporary {
                    output: output.path.clone(),
                    source,
                });
            }
            let pin = match openat2_file(
                output.file.as_raw_fd(),
                &name,
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0,
                descendant_resolution(),
            ) {
                Ok(file) => file,
                Err(source) => {
                    return Err(PublishError::Rollback {
                        primary: Box::new(PublishError::Io {
                            operation: "pin new publication directory",
                            path: path.clone(),
                            source,
                        }),
                        cleanup: Box::new(PublishError::UnprovenCleanup { path }),
                    });
                }
            };
            let metadata = pin.metadata().map_err(|source| PublishError::Rollback {
                primary: Box::new(PublishError::Io {
                    operation: "inspect new publication directory",
                    path: path.clone(),
                    source,
                }),
                cleanup: Box::new(PublishError::UnprovenCleanup { path: path.clone() }),
            })?;
            let identity = Identity::from_metadata(&metadata);
            if !metadata.file_type().is_dir() {
                return Err(with_cleanup(
                    PublishError::UnexpectedRoot {
                        role: "new temporary bundle",
                        path: path.clone(),
                    },
                    remove_owned_entry(output, &name, identity, true, "remove umask-rejected temporary bundle"),
                ));
            }
            if let Err(primary) = require_effective_owner("new temporary bundle", &path, &metadata) {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove foreign-owned temporary bundle"),
                ));
            }
            // mkdirat applies the ambient process umask. Never normalize by
            // name: the name could be replaced between authentication and
            // chmod. Instead, open the pinned inode as a usable directory,
            // authenticate that descriptor against the O_PATH pin, and chmod
            // only through the authenticated descriptor. A restrictive umask
            // which prevents that open fails closed and removes the empty
            // directory by its recorded identity.
            let file = match openat2_file(
                output.file.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
                descendant_resolution(),
            ) {
                Ok(file) => file,
                Err(source) => {
                    let primary = PublishError::Io {
                        operation: "open new publication directory",
                        path: path.clone(),
                        source,
                    };
                    return Err(with_cleanup(
                        primary,
                        remove_owned_entry(output, &name, identity, true, "remove unopened temporary bundle"),
                    ));
                }
            };
            let opened = file.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "inspect opened publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove uninspectable temporary bundle"),
                )
            })?;
            if !opened.file_type().is_dir() || Identity::from_metadata(&opened) != identity {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove replaced temporary bundle"),
                ));
            }
            if let Err(primary) = set_mode(&file, &path, 0o700, "new temporary bundle") {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove unnormalized temporary bundle"),
                ));
            }
            let normalized = file.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "inspect normalized publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove unverified temporary bundle"),
                )
            })?;
            if !normalized.file_type().is_dir()
                || Identity::from_metadata(&normalized) != identity
                || normalized.mode() & 0o7777 != 0o700
            {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove misnormalized temporary bundle"),
                ));
            }
            let pinned_after = pin.metadata().map_err(|source| {
                with_cleanup(
                    PublishError::Io {
                        operation: "reinspect pinned publication directory",
                        path: path.clone(),
                        source,
                    },
                    remove_owned_entry(output, &name, identity, true, "remove unverified temporary bundle"),
                )
            })?;
            if !pinned_after.file_type().is_dir()
                || Identity::from_metadata(&pinned_after) != identity
                || pinned_after.mode() & 0o7777 != 0o700
            {
                return Err(with_cleanup(
                    PublishError::OwnershipChanged { path: path.clone() },
                    remove_owned_entry(output, &name, identity, true, "remove misnormalized temporary bundle"),
                ));
            }
            let directory = DirectoryHandle { path, file, identity };
            if let Err(primary) = output.require_named_directory(&name, identity, 0o700, None) {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(output, &name, identity, true, "remove unauthenticated temporary bundle"),
                ));
            }
            return Ok(Self {
                output,
                directory,
                temporary_name: name,
                final_name,
                entries,
                source_date_epoch,
                location: BundleLocation::Temporary,
                active: true,
            });
        }
        Err(PublishError::CreateTemporary {
            output: output.path.clone(),
            source: last_collision.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)),
        })
    }

    fn copy_from(
        &mut self,
        index: usize,
        source: &mut VerifiedEntry,
        spec: &BundleSpec,
        deadline: &Deadline,
    ) -> Result<[u8; 32], PublishError> {
        deadline.check("copy published artefact")?;
        let path = self.directory.display(&spec.name);
        // Complete the only fallible owned-name allocation before O_EXCL
        // creates an inode. The entry vector was reserved before the bundle
        // directory was created, so the push immediately after fstat cannot
        // allocate and every created inode is tracked for rollback.
        let owned_name = copy_bytes(&spec.name, "owned publication entry name")?;
        let file = openat2_file(
            self.directory.file.as_raw_fd(),
            &spec.name,
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
            0o600,
            descendant_resolution(),
        )
        .map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        let metadata = file.metadata().map_err(|source_error| PublishError::Rollback {
            primary: Box::new(PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            }),
            cleanup: Box::new(PublishError::UnprovenCleanup { path: path.clone() }),
        })?;
        let identity = Identity::from_metadata(&metadata);
        self.entries.push(OwnedEntry {
            name: owned_name,
            identity,
            witness: None,
            digest: None,
            file: None,
        });
        if !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(PublishError::UnexpectedEntry {
                role: "temporary",
                path,
            });
        }
        require_effective_owner("temporary", &path, &metadata)?;
        // Normalize through the authenticated descriptor so the ambient umask
        // cannot silently weaken or strengthen the temporary file mode and no
        // name-based chmod can be redirected to a replacement inode.
        set_mode(&file, &path, 0o600, "new temporary artefact")?;
        let normalized = file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if !normalized.file_type().is_file()
            || normalized.nlink() != 1
            || Identity::from_metadata(&normalized) != identity
            || normalized.mode() & 0o7777 != 0o600
        {
            return Err(PublishError::UnexpectedEntry {
                role: "temporary",
                path,
            });
        }
        let mut target = file;
        let source_before = source.file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if FileWitness::from_metadata(&source_before) != source.witness {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        source
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|source_error| PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            })?;
        let mut hasher = Sha256::new();
        let mut remaining = source.witness.length;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        while remaining > 0 {
            deadline.check("copy published artefact")?;
            let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
            let read = source
                .file
                .read(&mut buffer[..amount])
                .map_err(|source_error| PublishError::Copy {
                    staged: source.path.clone(),
                    temporary: path.clone(),
                    source: source_error,
                })?;
            if read == 0 {
                return Err(PublishError::ArtifactChanged {
                    path: source.path.clone(),
                });
            }
            target
                .write_all(&buffer[..read])
                .map_err(|source_error| PublishError::Copy {
                    staged: source.path.clone(),
                    temporary: path.clone(),
                    source: source_error,
                })?;
            hasher.update(&buffer[..read]);
            remaining -= read as u64;
        }
        let mut trailing = [0_u8; 1];
        if source
            .file
            .read(&mut trailing)
            .map_err(|source_error| PublishError::Copy {
                staged: source.path.clone(),
                temporary: path.clone(),
                source: source_error,
            })?
            != 0
        {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        let source_after = source.file.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        if FileWitness::from_metadata(&source_after) != source.witness {
            return Err(PublishError::ArtifactChanged {
                path: source.path.clone(),
            });
        }
        let source_digest: [u8; 32] = hasher.finalize().into();
        target.flush().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        set_mode(&target, &path, PUBLISHED_ARTEFACT_MODE, "temporary artefact")?;
        set_timestamp(&target, &path, self.source_date_epoch)?;
        target.sync_all().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        let final_metadata = target.metadata().map_err(|source_error| PublishError::Copy {
            staged: source.path.clone(),
            temporary: path.clone(),
            source: source_error,
        })?;
        require_regular(
            "temporary",
            &path,
            &final_metadata,
            spec.maximum,
            Some(self.source_date_epoch),
        )?;
        if Identity::from_metadata(&final_metadata) != identity || final_metadata.len() != source.witness.length {
            return Err(PublishError::ArtifactChanged { path });
        }
        let witness = FileWitness::from_metadata(&final_metadata);
        // chmod does not revoke access held by an existing O_RDWR descriptor.
        // Close the construction descriptor before the bundle can be sealed,
        // then authenticate a fresh descriptor-relative O_RDONLY handle.
        drop(target);
        self.entries[index].witness = Some(witness);
        let mut readonly = self.entries[index].open_readonly(&self.directory, "open sealed temporary artefact")?;
        let target_digest = hash_file(&mut readonly, &path, witness, deadline)?;
        if target_digest != source_digest {
            return Err(PublishError::ContentMismatch {
                staged: source.path.clone(),
                published: path,
            });
        }
        self.entries[index].digest = Some(target_digest);
        Ok(source_digest)
    }

    fn seal(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        self.directory.require_inventory("temporary", expected, deadline)?;
        set_mode(
            &self.directory.file,
            &self.directory.path,
            PUBLISHED_BUNDLE_MODE,
            "temporary bundle",
        )?;
        set_timestamp(&self.directory.file, &self.directory.path, self.source_date_epoch)?;
        self.directory.sync("temporary bundle")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)?;
        self.verify_entries(expected, deadline)
    }

    fn verify_manifest_digest(
        &mut self,
        name: &[u8],
        expected_digest: [u8; 32],
        expected_path: &Path,
        deadline: &Deadline,
    ) -> Result<(), PublishError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| PublishError::OwnershipChanged {
                path: self.directory.display(name),
            })?;
        let directory = &self.directory;
        let entry = &mut self.entries[index];
        let path = directory.display(&entry.name);
        let witness = entry
            .witness
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        entry.require_named(directory, "authenticate copied binary manifest")?;
        if entry.file.is_none() {
            entry.file = Some(entry.open_readonly(directory, "retain copied binary manifest")?);
        }
        let file = entry
            .file
            .as_mut()
            .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
        require_file_witness(file, &path, witness, "authenticate retained copied binary manifest")?;
        let digest = hash_file(file, &path, witness, deadline)?;
        if entry.digest != Some(digest) {
            return Err(PublishError::ArtifactChanged { path });
        }
        if digest != expected_digest {
            return Err(PublishError::ManifestVerificationMismatch {
                generated: path,
                expected: expected_path.to_owned(),
            });
        }
        entry.require_named(directory, "reauthenticate copied binary manifest")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)
    }

    fn install(&mut self) -> Result<InstallOutcome, PublishError> {
        self.output.require_path_identity("output")?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)?;
        match rename_noreplace_at(
            self.output,
            &self.temporary_name,
            self.output,
            &self.final_name,
            "atomically install derivation bundle",
        ) {
            Ok(()) => {
                self.location = BundleLocation::Published;
                self.directory.path = self.output.display(&self.final_name);
                Ok(InstallOutcome::Installed)
            }
            Err(PublishError::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
                Ok(InstallOutcome::AlreadyExists)
            }
            Err(error) => Err(error),
        }
    }

    fn verify_final(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        if self.location != BundleLocation::Published {
            return Err(PublishError::OwnershipChanged {
                path: self.directory.path.clone(),
            });
        }
        self.output.require_named_directory(
            &self.final_name,
            self.directory.identity,
            PUBLISHED_BUNDLE_MODE,
            Some(self.source_date_epoch),
        )?;
        self.verify_entries(expected, deadline)?;
        self.output.require_path_identity("output")
    }

    fn verify_entries(&mut self, expected: &[Vec<u8>], deadline: &Deadline) -> Result<(), PublishError> {
        self.directory.require_inventory("published", expected, deadline)?;
        for entry in &mut self.entries {
            let path = self.directory.display(&entry.name);
            let witness = entry
                .witness
                .ok_or_else(|| PublishError::ArtifactChanged { path: path.clone() })?;
            let mut file = entry.open_readonly(&self.directory, "authenticate owned bundle entry")?;
            let digest = hash_file(&mut file, &path, witness, deadline)?;
            if entry.digest != Some(digest) {
                return Err(PublishError::ArtifactChanged { path });
            }
        }
        self.directory.require_inventory("published", expected, deadline)?;
        self.require_directory(PUBLISHED_BUNDLE_MODE)
    }

    fn require_directory(&self, mode: u32) -> Result<(), PublishError> {
        let name = match self.location {
            BundleLocation::Temporary => &self.temporary_name,
            BundleLocation::Published => &self.final_name,
        };
        let expected_mtime = (mode == PUBLISHED_BUNDLE_MODE).then_some(self.source_date_epoch);
        self.output
            .require_named_directory(name, self.directory.identity, mode, expected_mtime)
    }

    fn commit(&mut self) {
        self.active = false;
    }

    fn rollback_error(&mut self, primary: PublishError) -> PublishError {
        let published = self.location == BundleLocation::Published;
        match self.abort() {
            Ok(()) => primary,
            Err(cleanup) if published => PublishError::PublishedDurabilityUnknown {
                final_path: self.output.display(&self.final_name),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            },
            Err(cleanup) => PublishError::Rollback {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            },
        }
    }

    fn abort(&mut self) -> Result<(), PublishError> {
        if !self.active {
            return Ok(());
        }
        // Never retry implicitly after an ownership failure: a later retry
        // could observe a different foreign name and turn a fail-closed cleanup
        // into destructive path-based cleanup.
        self.active = false;
        let mut failures = Vec::new();
        if let Err(error) = set_mode(&self.directory.file, &self.directory.path, 0o700, "rollback bundle") {
            failures.push(error.to_string());
        }
        for entry in self.entries.iter().rev() {
            if let Err(error) = remove_owned_entry(
                &self.directory,
                &entry.name,
                entry.identity,
                false,
                "remove owned publication entry",
            ) {
                failures.push(error.to_string());
            }
        }
        if let Err(error) = self.directory.sync("rollback bundle") {
            failures.push(error.to_string());
        }
        let name = match self.location {
            BundleLocation::Temporary => &self.temporary_name,
            BundleLocation::Published => &self.final_name,
        };
        if let Err(error) = remove_owned_entry(
            self.output,
            name,
            self.directory.identity,
            true,
            "remove owned publication bundle",
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = self.output.sync("output after publication rollback") {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(PublishError::Cleanup { failures })
        }
    }
}

impl Drop for TemporaryBundle<'_> {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallOutcome {
    Installed,
    AlreadyExists,
}

fn require_regular(
    role: &'static str,
    path: &Path,
    metadata: &Metadata,
    maximum: u64,
    expected_mtime: Option<i64>,
) -> Result<(), PublishError> {
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(PublishError::UnexpectedEntry {
            role,
            path: path.to_owned(),
        });
    }
    require_effective_owner(role, path, metadata)?;
    require_mode(role, path, metadata, PUBLISHED_ARTEFACT_MODE)?;
    if metadata.len() > maximum {
        return Err(PublishError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    if let Some(expected) = expected_mtime
        && (metadata.mtime() != expected || metadata.mtime_nsec() != 0)
    {
        return Err(PublishError::TimestampMismatch {
            path: path.to_owned(),
            expected,
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
        });
    }
    Ok(())
}

fn require_effective_owner(role: &'static str, path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let expected = unsafe { libc::geteuid() };
    let found = metadata.uid();
    if found == expected {
        Ok(())
    } else {
        Err(PublishError::OwnerMismatch {
            role,
            path: path.to_owned(),
            expected,
            found,
        })
    }
}

pub(super) fn reference_owner_is_trusted(found: u32, effective: u32) -> bool {
    found == effective || found == 0
}

fn require_reference_owner(path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let effective = unsafe { libc::geteuid() };
    let found = metadata.uid();
    if reference_owner_is_trusted(found, effective) {
        Ok(())
    } else {
        Err(PublishError::ReferenceOwnerMismatch {
            path: path.to_owned(),
            effective,
            found,
        })
    }
}

fn require_protected_root_mode(role: &'static str, path: &Path, metadata: &Metadata) -> Result<(), PublishError> {
    let found = metadata.mode() & 0o7777;
    if found & 0o022 == 0 {
        Ok(())
    } else {
        Err(PublishError::WritableRoot {
            role,
            path: path.to_owned(),
            found,
        })
    }
}

fn require_mode(role: &'static str, path: &Path, metadata: &Metadata, expected: u32) -> Result<(), PublishError> {
    let found = metadata.mode() & 0o7777;
    if found == expected {
        Ok(())
    } else {
        Err(PublishError::ModeMismatch {
            role,
            path: path.to_owned(),
            expected,
            found,
        })
    }
}

fn require_directory_timestamp(
    path: &Path,
    metadata: &Metadata,
    expected_mtime: Option<i64>,
) -> Result<(), PublishError> {
    if let Some(expected) = expected_mtime
        && (metadata.mtime() != expected || metadata.mtime_nsec() != 0)
    {
        return Err(PublishError::TimestampMismatch {
            path: path.to_owned(),
            expected,
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
        });
    }
    Ok(())
}

fn with_cleanup(primary: PublishError, cleanup: Result<(), PublishError>) -> PublishError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => PublishError::Rollback {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}

fn set_mode(file: &File, path: &Path, mode: u32, role: &'static str) -> Result<(), PublishError> {
    // SAFETY: file is a live descriptor for an authenticated owned inode.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } == -1 {
        return Err(PublishError::NormalizeMode {
            role,
            path: path.to_owned(),
            mode,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn set_timestamp(file: &File, path: &Path, seconds: i64) -> Result<(), PublishError> {
    let seconds = libc::time_t::try_from(seconds).map_err(|_| PublishError::InvalidTimestamp { seconds })?;
    let times = [
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: 0,
        },
    ];
    // SAFETY: file and the two initialized timespec values remain live.
    if unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) } == -1 {
        return Err(PublishError::Io {
            operation: "normalize publication timestamp",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

// Linux has no unlinkat variant that accepts an expected inode. Publication
// runs beneath the derivation execution lock after every build/analyzer mutator
// has stopped; the emitted root is freshly recreated and the private child is
// mode 0700. Within that documented single-mutator boundary this check avoids
// stale/foreign deletion. Never use it in a concurrently same-UID-writable
// directory without kernel support for conditional unlink.
fn remove_owned_entry(
    directory: &DirectoryHandle,
    name: &[u8],
    identity: Identity,
    directory_entry: bool,
    operation: &'static str,
) -> Result<(), PublishError> {
    let path = directory.display(name);
    let Some((metadata, found)) = directory.inspect(name, operation)? else {
        return Err(PublishError::OwnershipChanged { path });
    };
    if found != identity || metadata.file_type().is_dir() != directory_entry {
        return Err(PublishError::OwnershipChanged { path });
    }
    require_effective_owner("owned publication cleanup", &path, &metadata)?;
    let name = c_name(name, &path)?;
    let flags = if directory_entry { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: descriptor/name remain live; unlinkat does not follow final links.
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), flags) } == -1 {
        return Err(PublishError::Io {
            operation,
            path,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn rename_noreplace_at(
    source_parent: &DirectoryHandle,
    source_name: &[u8],
    target_parent: &DirectoryHandle,
    target_name: &[u8],
    operation: &'static str,
) -> Result<(), PublishError> {
    let source_path = source_parent.display(source_name);
    let target_path = target_parent.display(target_name);
    let source_name = c_name(source_name, &source_path)?;
    let target_name = c_name(target_name, &target_path)?;
    // SAFETY: both pinned descriptors and both C strings remain live.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        Err(PublishError::Io {
            operation,
            path: target_path,
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn test_rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let parent = source
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?;
    if target.parent() != Some(parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "test paths need one parent",
        ));
    }
    let root = DirectoryHandle::open_root(parent, "test").map_err(io::Error::other)?;
    match rename_noreplace_at(
        &root,
        source.file_name().unwrap().as_bytes(),
        &root,
        target.file_name().unwrap().as_bytes(),
        "test rename",
    ) {
        Ok(()) => Ok(()),
        Err(PublishError::Io { source, .. }) => Err(source),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn descendant_resolution() -> u64 {
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, mode: u32, resolve: u64) -> io::Result<File> {
    let path = cstring_io(path)?;
    // SAFETY: zero initializes all current and future-compatible open_how fields.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: arguments remain live and successful syscall returns a fresh fd.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(result as RawFd) }))
}

fn validate_component(name: &[u8], role: &'static str) -> Result<(), PublishError> {
    if name.is_empty() || name.len() > 255 || matches!(name, b"." | b"..") || name.contains(&b'/') || name.contains(&0)
    {
        return Err(PublishError::InvalidName {
            role,
            name: OsString::from_vec(copy_bytes(name, "invalid publication name")?),
        });
    }
    Ok(())
}

fn c_name(name: &[u8], path: &Path) -> Result<CString, PublishError> {
    cstring_io(name).map_err(|source| PublishError::Io {
        operation: "encode publication component",
        path: path.to_owned(),
        source,
    })
}

fn cstring_io(bytes: &[u8]) -> io::Result<CString> {
    if bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"));
    }
    let requested = bytes
        .len()
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "path byte count overflow"))?;
    let mut terminated = Vec::new();
    terminated
        .try_reserve_exact(requested)
        .map_err(|source| io::Error::new(io::ErrorKind::OutOfMemory, source))?;
    terminated.extend_from_slice(bytes);
    terminated.push(0);
    CString::from_vec_with_nul(terminated).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, PublishError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| PublishError::Allocation {
            resource,
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn os_names(names: &[Vec<u8>]) -> Result<Vec<OsString>, PublishError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(names.len())
        .map_err(|source| PublishError::Allocation {
            resource: "publication error inventory",
            requested: names.len(),
            detail: source.to_string(),
        })?;
    for name in names {
        result.push(OsString::from_vec(copy_bytes(name, "publication error name")?));
    }
    Ok(result)
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(12).map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("frozen artefact paths are not bound to the published derivation")]
    InvalidFrozenPaths(#[source] io::Error),
    #[error("publication does not hold the execution lock for the frozen derivation")]
    InvalidExecutionLock(#[source] io::Error),
    #[error("invalid frozen derivation plan")]
    InvalidFrozenPlan(#[source] stone_recipe::derivation::DerivationValidationError),
    #[error("{role} artefact root {path:?} must be a real directory")]
    UnexpectedRoot { role: &'static str, path: PathBuf },
    #[error("{role} artefact entry {path:?} must be a single-link regular file")]
    UnexpectedEntry { role: &'static str, path: PathBuf },
    #[error("create private sibling bundle in {output:?}")]
    CreateTemporary {
        output: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("copy staged artefact {staged:?} to private bundle entry {temporary:?}")]
    Copy {
        staged: PathBuf,
        temporary: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{role} artefact bundle {path:?} does not match the frozen plan (expected {expected:?}, found {found:?})")]
    FrozenFileSetMismatch {
        role: &'static str,
        path: PathBuf,
        expected: Vec<OsString>,
        found: Vec<OsString>,
    },
    #[error("{role} path {path:?} has mode {found:#06o}; expected mode {expected:#06o}")]
    ModeMismatch {
        role: &'static str,
        path: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("{role} path {path:?} is owned by uid {found}; expected publisher euid {expected}")]
    OwnerMismatch {
        role: &'static str,
        path: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("expected manifest path {path:?} is owned by uid {found}; expected publisher euid {effective} or root")]
    ReferenceOwnerMismatch { path: PathBuf, effective: u32, found: u32 },
    #[error("{role} publication root {path:?} has group/other-writable mode {found:#06o}")]
    WritableRoot {
        role: &'static str,
        path: PathBuf,
        found: u32,
    },
    #[error("set {role} path {path:?} to mode {mode:#06o}")]
    NormalizeMode {
        role: &'static str,
        path: PathBuf,
        mode: u32,
        #[source]
        source: io::Error,
    },
    #[error("published artefact {published:?} does not match staged bytes from {staged:?}")]
    ContentMismatch { staged: PathBuf, published: PathBuf },
    #[error("read artefact file {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync reused published artefact {path:?}")]
    SyncFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync {role} bundle directory {path:?}")]
    SyncDirectory {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} at {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{role} name {name:?} is not one safe filesystem component")]
    InvalidName { role: &'static str, name: OsString },
    #[error("expected binary manifest path {path:?} has no safe parent/name split")]
    InvalidReferencePath { path: PathBuf },
    #[error("expected binary manifest does not exist: {path:?}")]
    MissingReferenceManifest { path: PathBuf },
    #[error("expected binary manifest {path:?} has group/other-writable mode {found:#06o}")]
    WritableReferenceManifest { path: PathBuf, found: u32 },
    #[error("expected binary manifest changed during publication: {path:?}")]
    ReferenceManifestChanged { path: PathBuf },
    #[error("expected binary manifest {expected:?} aliases staged manifest {generated:?}")]
    ReferenceAliasesStagedManifest { generated: PathBuf, expected: PathBuf },
    #[error("generated binary manifest {generated:?} does not exactly match {expected:?}")]
    ManifestVerificationMismatch { generated: PathBuf, expected: PathBuf },
    #[error("duplicate published artefact name {name:?}")]
    DuplicateName { name: OsString },
    #[error("{resource} exceeds the publication limit {limit}")]
    ResourceLimit { resource: &'static str, limit: usize },
    #[error("publication allocation failed for {resource} ({requested} items): {detail}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[error("artefact {path:?} exceeds {maximum} bytes (found {found})")]
    ArtifactTooLarge { path: PathBuf, maximum: u64, found: u64 },
    #[error("bundle exceeds {maximum} aggregate bytes (found {found})")]
    BundleTooLarge { maximum: u64, found: u64 },
    #[error("publication deadline expired while attempting to {operation} after {limit:?}")]
    Deadline { operation: &'static str, limit: Duration },
    #[error("publication directory changed during exact inventory: {path:?}")]
    DirectoryChanged { path: PathBuf },
    #[error("owned publication path changed identity or kind: {path:?}")]
    OwnershipChanged { path: PathBuf },
    #[error("sealed publication artefact changed: {path:?}")]
    ArtifactChanged { path: PathBuf },
    #[error("published path {path:?} has timestamp {seconds}.{nanoseconds}; expected {expected}.0")]
    TimestampMismatch {
        path: PathBuf,
        expected: i64,
        seconds: i64,
        nanoseconds: i64,
    },
    #[error("source_date_epoch {seconds} is not representable by this host")]
    InvalidTimestamp { seconds: i64 },
    #[error("a concurrent publisher installed the final derivation bundle")]
    ConcurrentPublication,
    #[error("publication failed and rollback also failed: primary={primary}; cleanup={cleanup}")]
    Rollback {
        primary: Box<PublishError>,
        cleanup: Box<PublishError>,
    },
    #[error(
        "published bundle {final_path:?} is complete but durability/rollback could not be proven: primary={primary}; cleanup={cleanup}"
    )]
    PublishedDurabilityUnknown {
        final_path: PathBuf,
        primary: Box<PublishError>,
        cleanup: Box<PublishError>,
    },
    #[error("publication cleanup ownership could not be proven for {path:?}")]
    UnprovenCleanup { path: PathBuf },
    #[error("publication cleanup failures: {failures:?}")]
    Cleanup { failures: Vec<String> },
}
