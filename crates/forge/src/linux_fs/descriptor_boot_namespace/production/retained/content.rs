use std::{fs::File, os::fd::AsFd as _};

use xxhash_rust::xxh3::Xxh3;

use super::super::super::{
    model::BootNamespaceRequest,
    observer::{BootNamespaceNodeIdentity, BootNamespaceObservationBoundary, BootNamespaceRegularWitness},
};
use super::{
    error::RetainedBootNamespaceAssessmentError,
    hook::{FixtureRetainedBootNamespaceProtocolEvent, RetainedBootNamespaceHook},
    limits::LiveLedger,
    node::{NodeObservation, NodeStat, invalid_data, observe_readable, observe_retained_path},
    syscall::pread_once,
};

const HASH_BUFFER_BYTES: usize = 4 * 1024;

pub(super) fn bind_expected_streams(
    requests: &[BootNamespaceRequest<'_>],
    expected: &[&[u8]],
    ledger: &mut LiveLedger,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    if expected.len() != requests.len() {
        return Err(RetainedBootNamespaceAssessmentError::ExpectedCountMismatch {
            expected: requests.len(),
            found: expected.len(),
        });
    }
    for (request_index, (request, bytes)) in requests.iter().copied().zip(expected).enumerate() {
        if u64::try_from(bytes.len()).ok() != Some(request.expected_length()) {
            return Err(RetainedBootNamespaceAssessmentError::ExpectedLengthMismatch {
                request_index,
                expected: request.expected_length(),
                found: bytes.len(),
            });
        }
        let mut digest = Xxh3::new();
        for chunk in bytes.chunks(HASH_BUFFER_BYTES) {
            ledger.charge_expected_hash_chunk(chunk.len())?;
            digest.update(chunk);
        }
        ledger.checkpoint()?;
        if digest.digest128() != request.expected_digest() {
            return Err(RetainedBootNamespaceAssessmentError::ExpectedDigestMismatch { request_index });
        }
    }
    ledger.checkpoint()
}

pub(super) fn capture_regular_witness<Hook: RetainedBootNamespaceHook>(
    retained: &File,
    reader: Option<&File>,
    identity: BootNamespaceNodeIdentity,
    expected_length: u64,
    boundary: BootNamespaceObservationBoundary,
    request_index: usize,
    ledger: &mut LiveLedger,
    hook: &mut Hook,
) -> Result<(BootNamespaceRegularWitness, NodeStat), RetainedBootNamespaceAssessmentError> {
    let opening = observe_retained_path(
        retained,
        ledger,
        "observing regular O_PATH metadata before content hashing",
    )?;
    require_regular(opening, identity)?;
    let digest = if opening.stat.size == expected_length {
        let reader = reader.ok_or_else(|| {
            invalid_data(
                "hashing one regular destination",
                "equal-length regular node has no revalidated readable description",
            )
        })?;
        observe_readable(
            reader,
            opening,
            false,
            ledger,
            "observing regular reader before content hashing",
        )?;
        let digest = hash_file(reader, opening.stat.size, ledger)?;
        observe_readable(
            reader,
            opening,
            false,
            ledger,
            "observing regular reader after content hashing",
        )?;
        emit(
            hook,
            FixtureRetainedBootNamespaceProtocolEvent::RegularHashComplete {
                boundary,
                request_index,
            },
        )?;
        digest
    } else {
        // Length mismatch is already a closed Different classification. Avoid
        // the otherwise expensive full-file hash while retaining metadata
        // sandwiches at both classifier boundaries.
        0
    };
    let closing = observe_retained_path(
        retained,
        ledger,
        "observing regular O_PATH metadata after content hashing",
    )?;
    if closing != opening {
        return Err(invalid_data(
            "closing one regular content hash",
            "regular descriptor changed around content hashing",
        ));
    }
    Ok((
        BootNamespaceRegularWitness {
            identity,
            length: opening.stat.size,
            digest,
            version: metadata_version(opening.stat),
        },
        opening.stat,
    ))
}

pub(super) fn pread_actual<Hook: RetainedBootNamespaceHook>(
    reader: &File,
    request_index: usize,
    offset: u64,
    output: &mut [u8],
    ledger: &mut LiveLedger,
    hook: &mut Hook,
) -> Result<usize, RetainedBootNamespaceAssessmentError> {
    ledger.charge_content_read(output.len(), "reading bounded actual destination bytes")?;
    ledger.admit_observation_io_attempt("reading bounded actual destination bytes")?;
    let read =
        pread_once(reader.as_fd(), offset, output).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
            action: "reading bounded actual destination bytes",
            source,
        });
    ledger.complete_observation_io_attempt()?;
    let read = read?;
    emit(
        hook,
        FixtureRetainedBootNamespaceProtocolEvent::ActualRead {
            request_index,
            offset,
            offered: output.len(),
        },
    )?;
    Ok(read)
}

fn hash_file(
    reader: &File,
    length: u64,
    ledger: &mut LiveLedger,
) -> Result<u128, RetainedBootNamespaceAssessmentError> {
    let mut digest = Xxh3::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut offset = 0u64;
    while offset < length {
        ledger.checkpoint()?;
        let offered = usize::try_from((length - offset).min(HASH_BUFFER_BYTES as u64))
            .expect("fixed hash buffer length fits usize");
        let mut filled = 0usize;
        while filled < offered {
            let found_offset = offset
                .checked_add(filled as u64)
                .ok_or_else(|| invalid_data("hashing one regular destination", "regular content offset overflowed"))?;
            let output = &mut buffer[filled..offered];
            ledger.charge_content_read(output.len(), "hashing bounded regular destination bytes")?;
            ledger.admit_observation_io_attempt("hashing bounded regular destination bytes")?;
            let found = pread_once(reader.as_fd(), found_offset, output).map_err(|source| {
                RetainedBootNamespaceAssessmentError::Filesystem {
                    action: "hashing bounded regular destination bytes",
                    source,
                }
            });
            ledger.complete_observation_io_attempt()?;
            let found = found?;
            if found == 0 {
                return Err(invalid_data(
                    "hashing one regular destination",
                    "regular content stopped before its observed length",
                ));
            }
            filled += found;
        }
        digest.update(&buffer[..offered]);
        offset = offset
            .checked_add(offered as u64)
            .ok_or_else(|| invalid_data("hashing one regular destination", "regular content offset overflowed"))?;
    }
    let mut probe = [0u8; 1];
    ledger.charge_content_read(1, "probing hashed regular destination length")?;
    ledger.admit_observation_io_attempt("probing hashed regular destination length")?;
    let found = pread_once(reader.as_fd(), length, &mut probe).map_err(|source| {
        RetainedBootNamespaceAssessmentError::Filesystem {
            action: "probing hashed regular destination length",
            source,
        }
    });
    ledger.complete_observation_io_attempt()?;
    let found = found?;
    if found != 0 {
        return Err(invalid_data(
            "probing hashed regular destination length",
            "regular content exceeds its observed length",
        ));
    }
    Ok(digest.digest128())
}

fn require_regular(
    observed: NodeObservation,
    expected: BootNamespaceNodeIdentity,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    if observed.identity != expected || observed.kind != super::super::super::observer::BootNamespaceNodeKind::Regular {
        Err(invalid_data(
            "binding one retained regular destination",
            "retained regular identity or kind changed",
        ))
    } else {
        Ok(())
    }
}

fn metadata_version(stat: NodeStat) -> u64 {
    let mut digest = Xxh3::new();
    digest.update(&stat.device.to_ne_bytes());
    digest.update(&stat.inode.to_ne_bytes());
    digest.update(&stat.mode.to_ne_bytes());
    digest.update(&stat.links.to_ne_bytes());
    digest.update(&stat.uid.to_ne_bytes());
    digest.update(&stat.gid.to_ne_bytes());
    digest.update(&stat.special_device.to_ne_bytes());
    digest.update(&stat.size.to_ne_bytes());
    digest.update(&stat.block_size.to_ne_bytes());
    digest.update(&stat.blocks.to_ne_bytes());
    digest.update(&stat.access_seconds.to_ne_bytes());
    digest.update(&stat.access_nanoseconds.to_ne_bytes());
    digest.update(&stat.modify_seconds.to_ne_bytes());
    digest.update(&stat.modify_nanoseconds.to_ne_bytes());
    digest.update(&stat.change_seconds.to_ne_bytes());
    digest.update(&stat.change_nanoseconds.to_ne_bytes());
    digest.digest()
}

fn emit(
    hook: &mut impl RetainedBootNamespaceHook,
    event: FixtureRetainedBootNamespaceProtocolEvent,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    hook.emit(event)
        .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
            action: "running an injected retained-namespace protocol hook",
            source,
        })
}
