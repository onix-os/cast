use std::{fmt, os::fd::BorrowedFd};

use xxhash_rust::xxh3::Xxh3;

use super::super::super::model::BootNamespaceRequest;
use super::{
    error::RetainedBootNamespaceAssessmentError,
    limits::LiveLedger,
    node::invalid_data,
    syscall::{descriptor_cloexec_flags_once, descriptor_seals_once, fstat_once, pread_once},
};

const HASH_BUFFER_BYTES: usize = 4 * 1024;
const REQUIRED_SEALS: i32 =
    nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;

/// One expected byte stream borrowed from an already-owned publication plan.
///
/// The representation is closed and non-`Clone`. A generated source borrows
/// bytes directly. A sealed source borrows one descriptor without exposing it
/// again, duplicating it, reopening it, or transferring ownership.
pub(crate) struct RetainedBootNamespaceExpectedSource<'source> {
    inner: ExpectedSourceKind<'source>,
}

enum ExpectedSourceKind<'source> {
    Generated(&'source [u8]),
    SealedDescriptor(BorrowedFd<'source>),
}

impl fmt::Debug for RetainedBootNamespaceExpectedSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.inner {
            ExpectedSourceKind::Generated(_) => "Generated",
            ExpectedSourceKind::SealedDescriptor(_) => "SealedDescriptor",
        };
        formatter
            .debug_struct("RetainedBootNamespaceExpectedSource")
            .field("kind", &kind)
            .finish()
    }
}

impl<'source> RetainedBootNamespaceExpectedSource<'source> {
    pub(crate) const fn generated(bytes: &'source [u8]) -> Self {
        Self {
            inner: ExpectedSourceKind::Generated(bytes),
        }
    }

    pub(crate) const fn sealed_descriptor(descriptor: BorrowedFd<'source>) -> Self {
        Self {
            inner: ExpectedSourceKind::SealedDescriptor(descriptor),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoundExpectedSourceEvidence {
    Generated,
    SealedDescriptor(SealedDescriptorEvidence),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SealedDescriptorEvidence {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    size: u64,
    change_seconds: i64,
    change_nanoseconds: i64,
    descriptor_flags: i32,
    seals: i32,
}

pub(super) fn bind_expected_streams(
    requests: &[BootNamespaceRequest<'_>],
    expected: &[RetainedBootNamespaceExpectedSource<'_>],
    ledger: &mut LiveLedger,
) -> Result<Vec<BoundExpectedSourceEvidence>, RetainedBootNamespaceAssessmentError> {
    if expected.len() != requests.len() {
        return Err(RetainedBootNamespaceAssessmentError::ExpectedCountMismatch {
            expected: requests.len(),
            found: expected.len(),
        });
    }
    let mut bound = Vec::new();
    ledger.reserve(
        &mut bound,
        expected.len(),
        "allocating expected-source binding evidence",
    )?;
    for (request_index, (request, source)) in requests.iter().copied().zip(expected).enumerate() {
        let (length, digest, evidence) = match source.inner {
            ExpectedSourceKind::Generated(bytes) => {
                let length = u64::try_from(bytes.len()).map_err(|_| {
                    invalid_data(
                        "binding generated expected bytes",
                        "generated expected length does not fit u64",
                    )
                })?;
                if length != request.expected_length() {
                    return Err(RetainedBootNamespaceAssessmentError::ExpectedLengthMismatch {
                        request_index,
                        expected: request.expected_length(),
                        found: length,
                    });
                }
                let digest = hash_generated(bytes, ledger)?;
                (length, digest, BoundExpectedSourceEvidence::Generated)
            }
            ExpectedSourceKind::SealedDescriptor(descriptor) => {
                let (digest, evidence) = bind_sealed_descriptor(descriptor, request.expected_length(), ledger)?;
                (
                    evidence.size,
                    digest,
                    BoundExpectedSourceEvidence::SealedDescriptor(evidence),
                )
            }
        };
        if length != request.expected_length() {
            return Err(RetainedBootNamespaceAssessmentError::ExpectedLengthMismatch {
                request_index,
                expected: request.expected_length(),
                found: length,
            });
        }
        if digest != request.expected_digest() {
            return Err(RetainedBootNamespaceAssessmentError::ExpectedDigestMismatch { request_index });
        }
        bound.push(evidence);
    }
    ledger.checkpoint()?;
    Ok(bound)
}

pub(super) fn read_expected(
    source: &RetainedBootNamespaceExpectedSource<'_>,
    evidence: BoundExpectedSourceEvidence,
    expected_length: u64,
    offset: u64,
    output: &mut [u8],
    ledger: &mut LiveLedger,
) -> Result<usize, RetainedBootNamespaceAssessmentError> {
    ledger.checkpoint()?;
    match (&source.inner, evidence) {
        (ExpectedSourceKind::Generated(bytes), BoundExpectedSourceEvidence::Generated) => {
            let start =
                usize::try_from(offset).map_err(|_| RetainedBootNamespaceAssessmentError::ObserverProtocol {
                    reason: "classifier supplied an oversized generated expected-stream offset",
                })?;
            if start >= bytes.len() {
                return Ok(0);
            }
            let found = output.len().min(bytes.len() - start);
            output[..found].copy_from_slice(&bytes[start..start + found]);
            ledger.checkpoint()?;
            Ok(found)
        }
        (ExpectedSourceKind::SealedDescriptor(descriptor), BoundExpectedSourceEvidence::SealedDescriptor(_)) => {
            read_sealed_descriptor(*descriptor, expected_length, offset, output, ledger)
        }
        _ => Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "expected-source representation changed after prebinding",
        }),
    }
}

pub(super) fn terminally_revalidate_expected_streams(
    requests: &[BootNamespaceRequest<'_>],
    expected: &[RetainedBootNamespaceExpectedSource<'_>],
    bound: &[BoundExpectedSourceEvidence],
    ledger: &mut LiveLedger,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    if requests.len() != expected.len() || requests.len() != bound.len() {
        return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "terminal expected-source evidence count changed",
        });
    }
    for ((request, source), evidence) in requests.iter().copied().zip(expected).zip(bound.iter().copied()) {
        match (&source.inner, evidence) {
            (ExpectedSourceKind::Generated(_), BoundExpectedSourceEvidence::Generated) => {}
            (
                ExpectedSourceKind::SealedDescriptor(descriptor),
                BoundExpectedSourceEvidence::SealedDescriptor(opening),
            ) => {
                let closing = observe_sealed_descriptor(*descriptor, request.expected_length(), ledger)?;
                if closing != opening {
                    return Err(invalid_data(
                        "terminally revalidating one sealed expected source",
                        "sealed expected-source descriptor metadata drifted",
                    ));
                }
            }
            _ => {
                return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                    reason: "terminal expected-source representation changed",
                });
            }
        }
    }
    ledger.checkpoint()
}

fn hash_generated(bytes: &[u8], ledger: &mut LiveLedger) -> Result<u128, RetainedBootNamespaceAssessmentError> {
    let mut digest = Xxh3::new();
    for chunk in bytes.chunks(HASH_BUFFER_BYTES) {
        ledger.charge_expected_hash_chunk(chunk.len())?;
        digest.update(chunk);
    }
    ledger.checkpoint()?;
    Ok(digest.digest128())
}

fn bind_sealed_descriptor(
    descriptor: BorrowedFd<'_>,
    expected_length: u64,
    ledger: &mut LiveLedger,
) -> Result<(u128, SealedDescriptorEvidence), RetainedBootNamespaceAssessmentError> {
    let opening = observe_sealed_descriptor(descriptor, expected_length, ledger)?;
    let digest = hash_sealed_descriptor(descriptor, expected_length, ledger)?;
    let closing = observe_sealed_descriptor(descriptor, expected_length, ledger)?;
    if closing != opening {
        return Err(invalid_data(
            "binding one sealed expected source",
            "sealed expected-source descriptor metadata drifted around hashing",
        ));
    }
    Ok((digest, opening))
}

fn observe_sealed_descriptor(
    descriptor: BorrowedFd<'_>,
    expected_length: u64,
    ledger: &mut LiveLedger,
) -> Result<SealedDescriptorEvidence, RetainedBootNamespaceAssessmentError> {
    ledger.admit_observation_io_attempt("observing sealed expected-source metadata")?;
    let status = fstat_once(descriptor).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
        action: "observing sealed expected-source metadata",
        source,
    });
    ledger.complete_observation_io_attempt()?;
    let status = status?;

    ledger.admit_observation_io_attempt("observing sealed expected-source descriptor flags")?;
    let descriptor_flags =
        descriptor_cloexec_flags_once(descriptor).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
            action: "observing sealed expected-source descriptor flags",
            source,
        });
    ledger.complete_observation_io_attempt()?;
    let descriptor_flags = descriptor_flags?;

    ledger.admit_observation_io_attempt("observing sealed expected-source seals")?;
    let seals = descriptor_seals_once(descriptor).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
        action: "observing sealed expected-source seals",
        source,
    });
    ledger.complete_observation_io_attempt()?;
    let seals = seals?;

    let mode = status.st_mode;
    let size = u64::try_from(status.st_size).map_err(|_| {
        invalid_data(
            "observing sealed expected-source metadata",
            "sealed expected-source length is negative or does not fit u64",
        )
    })?;
    if mode & nix::libc::S_IFMT != nix::libc::S_IFREG || mode & 0o7777 != 0o400 {
        return Err(invalid_data(
            "observing sealed expected-source metadata",
            "sealed expected source is not a mode-0400 regular file",
        ));
    }
    if size != expected_length {
        return Err(invalid_data(
            "observing sealed expected-source metadata",
            "sealed expected-source length does not match its request",
        ));
    }
    if descriptor_flags & nix::libc::FD_CLOEXEC == 0 {
        return Err(invalid_data(
            "observing sealed expected-source descriptor flags",
            "sealed expected-source descriptor lacks FD_CLOEXEC",
        ));
    }
    if seals & REQUIRED_SEALS != REQUIRED_SEALS {
        return Err(invalid_data(
            "observing sealed expected-source seals",
            "sealed expected source lacks immutable full seals",
        ));
    }
    Ok(SealedDescriptorEvidence {
        device: status.st_dev as u64,
        inode: status.st_ino as u64,
        mode,
        links: status.st_nlink as u64,
        uid: status.st_uid,
        gid: status.st_gid,
        size,
        change_seconds: status.st_ctime as i64,
        change_nanoseconds: status.st_ctime_nsec as i64,
        descriptor_flags,
        seals,
    })
}

fn hash_sealed_descriptor(
    descriptor: BorrowedFd<'_>,
    length: u64,
    ledger: &mut LiveLedger,
) -> Result<u128, RetainedBootNamespaceAssessmentError> {
    let mut digest = Xxh3::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut offset = 0u64;
    while offset < length {
        ledger.checkpoint()?;
        let offered = usize::try_from((length - offset).min(HASH_BUFFER_BYTES as u64))
            .expect("fixed expected-source hash buffer fits usize");
        let found = pread_sealed_descriptor(
            descriptor,
            offset,
            &mut buffer[..offered],
            ledger,
            "hashing sealed expected-source bytes",
        )?;
        if found == 0 {
            return Err(invalid_data(
                "hashing one sealed expected source",
                "sealed expected source stopped before its declared length",
            ));
        }
        ledger.charge_expected_hash_chunk(found)?;
        digest.update(&buffer[..found]);
        offset = offset.checked_add(found as u64).ok_or_else(|| {
            invalid_data(
                "hashing one sealed expected source",
                "sealed expected-source offset overflowed",
            )
        })?;
    }
    let mut probe = [0u8; 1];
    let found = pread_sealed_descriptor(
        descriptor,
        length,
        &mut probe,
        ledger,
        "probing sealed expected-source length",
    )?;
    if found != 0 {
        return Err(invalid_data(
            "probing one sealed expected source",
            "sealed expected source exceeds its declared length",
        ));
    }
    Ok(digest.digest128())
}

fn read_sealed_descriptor(
    descriptor: BorrowedFd<'_>,
    expected_length: u64,
    offset: u64,
    output: &mut [u8],
    ledger: &mut LiveLedger,
) -> Result<usize, RetainedBootNamespaceAssessmentError> {
    if output.is_empty() {
        ledger.checkpoint()?;
        return Ok(0);
    }
    if offset > expected_length {
        return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
            reason: "classifier supplied an expected-source offset beyond its declared length",
        });
    }
    let offered = if offset == expected_length {
        1
    } else {
        usize::try_from((expected_length - offset).min(output.len() as u64))
            .expect("offered expected-source read fits output length")
    };
    let found = pread_sealed_descriptor(
        descriptor,
        offset,
        &mut output[..offered],
        ledger,
        "reading sealed expected-source comparison bytes",
    )?;
    if offset == expected_length {
        if found == 0 {
            return Ok(0);
        }
        return Err(invalid_data(
            "reading one sealed expected source",
            "sealed expected source exceeds its declared length",
        ));
    }
    if found == 0 {
        return Err(invalid_data(
            "reading one sealed expected source",
            "sealed expected source stopped before its declared length",
        ));
    }
    Ok(found)
}

fn pread_sealed_descriptor(
    descriptor: BorrowedFd<'_>,
    offset: u64,
    output: &mut [u8],
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<usize, RetainedBootNamespaceAssessmentError> {
    ledger.charge_expected_source_read(output.len(), action)?;
    ledger.admit_observation_io_attempt(action)?;
    let found = pread_once(descriptor, offset, output)
        .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem { action, source });
    ledger.complete_observation_io_attempt()?;
    let found = found?;
    if found > output.len() {
        return Err(invalid_data(action, "pread returned more bytes than were offered"));
    }
    Ok(found)
}
