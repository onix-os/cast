//! Marker framing, inode witnesses, and low-level integrity checks.

use std::{
    ffi::{CStr, OsStr},
    fs::File,
    io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{FileExt as _, MetadataExt as _},
    },
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::{MARKER_MODE, TreeMarkerError};
use crate::transition_journal::TreeToken;

const MAGIC: &[u8; 8] = b"CASTTID\0";
const VERSION: u16 = 1;
pub(super) const TOKEN_LENGTH: usize = TreeToken::TEXT_LENGTH;
const CHECKSUM_LENGTH: usize = 32;
pub(super) const MAGIC_END: usize = MAGIC.len();
pub(super) const VERSION_END: usize = MAGIC_END + size_of::<u16>();
pub(super) const LENGTH_END: usize = VERSION_END + size_of::<u32>();
pub(super) const CHECKSUM_END: usize = LENGTH_END + CHECKSUM_LENGTH;
pub(crate) const FRAME_LENGTH: usize = CHECKSUM_END + TOKEN_LENGTH;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InodeIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DirectoryWitness {
    pub(super) identity: InodeIdentity,
    pub(super) owner: u32,
    pub(super) mode: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MarkerWitness {
    pub(super) identity: InodeIdentity,
    pub(super) owner: u32,
    pub(super) mode: u32,
    pub(super) links: u64,
    pub(super) length: u64,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum MarkerCodecError {
    #[error("tree marker has length {actual}, expected exactly {FRAME_LENGTH}")]
    InvalidLength { actual: usize },
    #[error("tree marker magic is not canonical")]
    InvalidMagic,
    #[error("tree marker version {0} is unsupported")]
    UnsupportedVersion(u16),
    #[error("tree marker payload length {0} is not canonical")]
    InvalidPayloadLength(u32),
    #[error("tree marker token is not one nonzero lowercase 128-bit value")]
    InvalidToken,
    #[error("tree marker checksum does not match its framed token")]
    ChecksumMismatch,
    #[error("tree marker frame is not in canonical byte form")]
    NonCanonical,
}

pub(super) fn encode_marker(token: &TreeToken) -> [u8; FRAME_LENGTH] {
    let mut frame = [0_u8; FRAME_LENGTH];
    frame[..MAGIC_END].copy_from_slice(MAGIC);
    frame[MAGIC_END..VERSION_END].copy_from_slice(&VERSION.to_be_bytes());
    frame[VERSION_END..LENGTH_END].copy_from_slice(&(TOKEN_LENGTH as u32).to_be_bytes());
    frame[CHECKSUM_END..].copy_from_slice(token.as_str().as_bytes());
    let checksum = marker_checksum(&frame[..LENGTH_END], &frame[CHECKSUM_END..]);
    frame[LENGTH_END..CHECKSUM_END].copy_from_slice(&checksum);
    frame
}

pub(super) fn decode_marker(frame: &[u8]) -> Result<TreeToken, MarkerCodecError> {
    if frame.len() != FRAME_LENGTH {
        return Err(MarkerCodecError::InvalidLength { actual: frame.len() });
    }
    if &frame[..MAGIC_END] != MAGIC {
        return Err(MarkerCodecError::InvalidMagic);
    }
    let version = u16::from_be_bytes(frame[MAGIC_END..VERSION_END].try_into().expect("fixed version range"));
    if version != VERSION {
        return Err(MarkerCodecError::UnsupportedVersion(version));
    }
    let length = u32::from_be_bytes(frame[VERSION_END..LENGTH_END].try_into().expect("fixed length range"));
    if length != TOKEN_LENGTH as u32 {
        return Err(MarkerCodecError::InvalidPayloadLength(length));
    }
    let expected = marker_checksum(&frame[..LENGTH_END], &frame[CHECKSUM_END..]);
    if frame[LENGTH_END..CHECKSUM_END] != expected {
        return Err(MarkerCodecError::ChecksumMismatch);
    }
    let token = std::str::from_utf8(&frame[CHECKSUM_END..])
        .ok()
        .and_then(|token| TreeToken::parse(token.to_owned()).ok())
        .ok_or(MarkerCodecError::InvalidToken)?;
    if encode_marker(&token) != frame {
        return Err(MarkerCodecError::NonCanonical);
    }
    Ok(token)
}

pub(super) fn marker_checksum(header: &[u8], token: &[u8]) -> [u8; CHECKSUM_LENGTH] {
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(token);
    digest.finalize().into()
}

pub(super) fn directory_witness(file: &File, path: &Path) -> Result<DirectoryWitness, TreeMarkerError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect retained /usr directory", path, source))?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_user_id()
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(TreeMarkerError::UnsafeDirectory {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }
    Ok(DirectoryWitness {
        identity: InodeIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        owner: metadata.uid(),
        mode,
    })
}

pub(super) fn canonical_witness(file: &File, path: &Path) -> Result<MarkerWitness, TreeMarkerError> {
    canonical_witness_with_links(file, path, 1)
}

pub(super) fn canonical_witness_with_links(
    file: &File,
    path: &Path,
    expected_links: u64,
) -> Result<MarkerWitness, TreeMarkerError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect canonical tree marker", path, source))?;
    let witness = MarkerWitness::from_metadata(&metadata);
    if metadata.file_type().is_file()
        && witness.owner == effective_user_id()
        && witness.mode == MARKER_MODE
        && witness.links == expected_links
        && witness.length == FRAME_LENGTH as u64
    {
        Ok(witness)
    } else {
        Err(unsafe_marker("canonical", path, witness))
    }
}

pub(super) fn anonymous_witness(
    file: &File,
    path: &Path,
    expected_mode: u32,
    expected_length: u64,
) -> Result<MarkerWitness, TreeMarkerError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect anonymous tree marker", path, source))?;
    let witness = MarkerWitness::from_metadata(&metadata);
    if metadata.file_type().is_file()
        && witness.owner == effective_user_id()
        && witness.links == 0
        && witness.mode == expected_mode
        && witness.length == expected_length
    {
        Ok(witness)
    } else {
        Err(unsafe_marker("anonymous", path, witness))
    }
}

impl MarkerWitness {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            identity: InodeIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            owner: metadata.uid(),
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            length: metadata.len(),
        }
    }
}

pub(super) fn read_and_decode_with_links(
    file: &File,
    expected: MarkerWitness,
    path: &Path,
    expected_links: u64,
) -> Result<TreeToken, TreeMarkerError> {
    let bytes = read_exact_frame(file, path)?;
    if canonical_witness_with_links(file, path, expected_links)? != expected {
        return Err(TreeMarkerError::MarkerChanged { path: path.to_owned() });
    }
    decode_marker(&bytes).map_err(|source| TreeMarkerError::Decode {
        path: path.to_owned(),
        source,
    })
}

pub(super) fn marker_witness_with_links(mut witness: MarkerWitness, links: u64) -> MarkerWitness {
    witness.links = links;
    witness
}

pub(super) fn same_marker_inode(left: MarkerWitness, right: MarkerWitness) -> bool {
    left.identity == right.identity
        && left.owner == right.owner
        && left.mode == right.mode
        && left.length == right.length
}

pub(super) fn read_exact_frame(file: &File, path: &Path) -> Result<[u8; FRAME_LENGTH], TreeMarkerError> {
    let mut frame = [0_u8; FRAME_LENGTH];
    let mut offset = 0;
    while offset < frame.len() {
        match file.read_at(&mut frame[offset..], offset as u64) {
            Ok(0) => {
                return Err(io_error(
                    "read complete tree marker frame",
                    path,
                    io::Error::new(io::ErrorKind::UnexpectedEof, "tree marker ended before its fixed frame"),
                ));
            }
            Ok(read) => offset += read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(io_error("read tree marker frame", path, source)),
        }
    }
    let mut trailing = [0_u8; 1];
    loop {
        match file.read_at(&mut trailing, FRAME_LENGTH as u64) {
            Ok(0) => return Ok(frame),
            Ok(_) => {
                return Err(io_error(
                    "read bounded tree marker frame",
                    path,
                    io::Error::new(io::ErrorKind::InvalidData, "tree marker contains trailing bytes"),
                ));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(io_error("read tree marker frame bound", path, source)),
        }
    }
}

pub(super) fn write_all_at(file: &File, bytes: &[u8], path: &Path) -> Result<(), TreeMarkerError> {
    let mut written = 0;
    while written < bytes.len() {
        match file.write_at(&bytes[written..], written as u64) {
            Ok(0) => {
                return Err(io_error(
                    "write complete tree marker frame",
                    path,
                    io::Error::from_raw_os_error(nix::libc::EIO),
                ));
            }
            Ok(count) => written += count,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(io_error("write tree marker frame", path, source)),
        }
    }
    Ok(())
}

pub(super) fn require_expected_token(
    expected: Option<&TreeToken>,
    actual: &TreeToken,
    path: &Path,
) -> Result<(), TreeMarkerError> {
    if let Some(expected) = expected
        && expected != actual
    {
        return Err(TreeMarkerError::TokenMismatch {
            path: path.to_owned(),
            expected: expected.as_str().to_owned(),
            actual: actual.as_str().to_owned(),
        });
    }
    Ok(())
}

pub(super) fn unsafe_marker(role: &'static str, path: &Path, witness: MarkerWitness) -> TreeMarkerError {
    TreeMarkerError::UnsafeMarker {
        role,
        path: path.to_owned(),
        owner: witness.owner,
        mode: witness.mode,
        links: witness.links,
        length: witness.length,
    }
}

pub(super) fn io_error(operation: &'static str, path: &Path, source: io::Error) -> TreeMarkerError {
    TreeMarkerError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

pub(super) fn component_path(directory: &Path, name: &CStr) -> PathBuf {
    directory.join(OsStr::from_bytes(name.to_bytes()))
}

pub(super) fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}
