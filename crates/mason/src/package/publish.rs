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

include!("publish/reuse.rs");

include!("publish/descriptor_verification.rs");

include!("publish/bundle_verification.rs");

include!("publish/temporary_bundle.rs");

include!("publish/filesystem_operations.rs");

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
