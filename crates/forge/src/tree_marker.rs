// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Durable logical identity for one retained `/usr` tree.
//!
//! Runtime inode and mount identifiers are useful witnesses only inside their
//! boot and mount-namespace epoch.  The marker stored here follows the logical
//! tree through staging, exchange, archive, and quarantine.  Its creation API
//! is intentionally separate from the recovery reader: recovery can neither
//! mint a token nor promote, repair, or remove a temporary marker.

use std::{
    ffi::{CStr, OsStr},
    fs::{File, Permissions},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::OsStrExt as _,
            fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    linux_fs::{controlled_resolution, link_path_descriptor_noreplace, openat2_file, require_no_default_acl},
    transition_journal::TreeToken,
};

const MARKER_NAME: &CStr = c".cast-tree-id";
const TEMPORARY_NAME: &CStr = c".cast-tree-id.tmp";
const MARKER_MODE: u32 = 0o444;
const TEMPORARY_MODE: u32 = 0o600;

const MAGIC: &[u8; 8] = b"CASTTID\0";
const VERSION: u16 = 1;
const TOKEN_LENGTH: usize = TreeToken::TEXT_LENGTH;
const CHECKSUM_LENGTH: usize = 32;
const MAGIC_END: usize = MAGIC.len();
const VERSION_END: usize = MAGIC_END + size_of::<u16>();
const LENGTH_END: usize = VERSION_END + size_of::<u32>();
const CHECKSUM_END: usize = LENGTH_END + CHECKSUM_LENGTH;
const FRAME_LENGTH: usize = CHECKSUM_END + TOKEN_LENGTH;

/// A retained, readable `/usr` directory capability.
///
/// The pre-journal mutator must be called only while the installation and
/// journal locks prove there is no live journal or orphan transition row. The
/// type itself deliberately offers a separate recovery-only reader below.
#[derive(Debug)]
pub(crate) struct TreeMarkerStore {
    usr: File,
    path: PathBuf,
    witness: DirectoryWitness,
}

/// One decoded marker whose exact inode remains pinned.
#[derive(Debug)]
pub(crate) struct RetainedTreeMarker {
    token: TreeToken,
    file: File,
    path: PathBuf,
    witness: MarkerWitness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    identity: InodeIdentity,
    owner: u32,
    mode: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkerWitness {
    identity: InodeIdentity,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
}

#[derive(Debug)]
struct TemporaryMarker {
    file: File,
    witness: MarkerWitness,
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

#[derive(Debug, Error)]
pub(crate) enum TreeMarkerError {
    #[error("{operation} tree marker path `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "unsafe retained /usr directory `{}` (uid={owner}, mode={mode:04o})",
        path.display()
    )]
    UnsafeDirectory { path: PathBuf, owner: u32, mode: u32 },
    #[error(
        "unsafe {role} tree marker `{}` (uid={owner}, mode={mode:04o}, links={links}, length={length})",
        path.display()
    )]
    UnsafeMarker {
        role: &'static str,
        path: PathBuf,
        owner: u32,
        mode: u32,
        links: u64,
        length: u64,
    },
    #[error("required tree marker is missing at `{}`", path.display())]
    Missing { path: PathBuf },
    #[error("recovery refuses temporary tree marker evidence at `{}`", path.display())]
    TemporaryPresent { path: PathBuf },
    #[error("decode tree marker `{}`", path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: MarkerCodecError,
    },
    #[error(
        "tree marker token mismatch at `{}`: expected {expected}, found {actual}",
        path.display()
    )]
    TokenMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("tree marker inode changed while retained at `{}`", path.display())]
    MarkerChanged { path: PathBuf },
    #[error("temporary tree marker inode changed at `{}`", path.display())]
    TemporaryChanged { path: PathBuf },
}

impl TreeMarkerStore {
    /// Reopen and authenticate one already-retained `/usr` capability.
    pub(crate) fn open(usr: &File, display_path: impl Into<PathBuf>) -> Result<Self, TreeMarkerError> {
        let path = display_path.into();
        let expected = directory_witness(usr, &path)?;
        let reopened = openat2_file(
            usr.as_raw_fd(),
            c".",
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            0,
            controlled_resolution(),
        )
        .map_err(|source| io_error("reopen retained /usr directory", &path, source))?;
        let actual = directory_witness(&reopened, &path)?;
        if actual != expected {
            return Err(TreeMarkerError::MarkerChanged { path });
        }
        require_no_default_acl(&reopened, &path)
            .map_err(|source| io_error("reject inheritable /usr ACL", &path, source))?;
        Ok(Self {
            usr: reopened,
            path,
            witness: expected,
        })
    }

    /// Create a marker or adopt a strictly valid marker while no journal
    /// exists, then return a retained proof of the canonical inode.
    ///
    /// Publication is built in an unnamed same-filesystem `O_TMPFILE` and
    /// linked directly from its retained descriptor. There is therefore no
    /// source pathname to substitute and no crash residue to clean. This is
    /// the only API in this module which can generate a token or publish a
    /// canonical name.
    pub(crate) fn adopt_or_create_before_journal(&self) -> Result<RetainedTreeMarker, TreeMarkerError> {
        self.validate_usr()?;
        self.usr
            .sync_all()
            .map_err(|source| io_error("sync /usr before tree marker preparation", &self.path, source))?;

        if let Some(existing) = self.load_canonical()? {
            self.reject_temporary()?;
            existing
                .file
                .sync_all()
                .map_err(|source| io_error("sync adopted tree marker", &existing.path, source))?;
            self.usr
                .sync_all()
                .map_err(|source| io_error("sync /usr after adopting tree marker", &self.path, source))?;
            existing.revalidate(self)?;
            return Ok(existing);
        }

        self.reject_temporary()?;
        let mut temporary = self.create_anonymous_temporary()?;
        let token = TreeToken::generate()
            .map_err(|source| io_error("generate kernel-random tree token", &self.marker_path(), source))?;
        self.write_complete_temporary(&mut temporary, &token)?;
        self.publish_temporary(temporary, &token)
    }

    /// Read only canonical recovery evidence.
    ///
    /// This path cannot generate, chmod, unlink, rename, repair, or promote a
    /// marker. Any temporary name is unresolved evidence and fails closed.
    pub(crate) fn read_for_recovery(&self) -> Result<RetainedTreeMarker, TreeMarkerError> {
        self.validate_usr()?;
        self.reject_temporary()?;
        let marker = self.load_canonical()?.ok_or_else(|| TreeMarkerError::Missing {
            path: self.marker_path(),
        })?;
        marker.revalidate(self)?;
        Ok(marker)
    }

    /// Read canonical recovery evidence and bind it to the journal token.
    pub(crate) fn read_expected_for_recovery(
        &self,
        expected: &TreeToken,
    ) -> Result<RetainedTreeMarker, TreeMarkerError> {
        let marker = self.read_for_recovery()?;
        require_expected_token(Some(expected), &marker.token, &marker.path)?;
        Ok(marker)
    }

    fn marker_path(&self) -> PathBuf {
        component_path(&self.path, MARKER_NAME)
    }

    fn temporary_path(&self) -> PathBuf {
        component_path(&self.path, TEMPORARY_NAME)
    }

    fn validate_usr(&self) -> Result<(), TreeMarkerError> {
        let actual = directory_witness(&self.usr, &self.path)?;
        if actual != self.witness {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        require_no_default_acl(&self.usr, &self.path)
            .map_err(|source| io_error("reject inheritable /usr ACL", &self.path, source))
    }

    fn load_canonical(&self) -> Result<Option<RetainedTreeMarker>, TreeMarkerError> {
        let path = self.marker_path();
        let Some(probe) = self.open_optional(
            MARKER_NAME,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            "probe canonical tree marker",
        )?
        else {
            return Ok(None);
        };
        let witness = canonical_witness(&probe, &path)?;
        let file = self
            .open_optional(
                MARKER_NAME,
                nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                "open canonical tree marker",
            )?
            .ok_or_else(|| TreeMarkerError::MarkerChanged { path: path.clone() })?;
        if canonical_witness(&file, &path)? != witness {
            return Err(TreeMarkerError::MarkerChanged { path });
        }
        let token = read_and_decode(&file, witness, &path)?;
        Ok(Some(RetainedTreeMarker {
            token,
            file,
            path,
            witness,
        }))
    }

    fn create_anonymous_temporary(&self) -> Result<TemporaryMarker, TreeMarkerError> {
        let path = self.marker_path();
        let file = openat2_file(
            self.usr.as_raw_fd(),
            c".",
            nix::libc::O_TMPFILE
                | nix::libc::O_RDWR
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            TEMPORARY_MODE,
            controlled_resolution(),
        )
        .map_err(|source| io_error("create anonymous tree marker", &path, source))?;
        file.set_permissions(Permissions::from_mode(TEMPORARY_MODE))
            .map_err(|source| io_error("normalize anonymous tree marker mode", &path, source))?;
        let witness = anonymous_witness(&file, &path, TEMPORARY_MODE, 0)?;
        Ok(TemporaryMarker { file, witness })
    }

    fn write_complete_temporary(
        &self,
        temporary: &mut TemporaryMarker,
        token: &TreeToken,
    ) -> Result<(), TreeMarkerError> {
        let path = self.marker_path();
        temporary
            .file
            .set_permissions(Permissions::from_mode(TEMPORARY_MODE))
            .map_err(|source| io_error("make anonymous tree marker writable", &path, source))?;
        temporary
            .file
            .set_len(0)
            .map_err(|source| io_error("truncate anonymous tree marker", &path, source))?;
        let frame = encode_marker(token);
        write_all_at(&temporary.file, &frame, &path)?;
        temporary
            .file
            .sync_all()
            .map_err(|source| io_error("sync anonymous tree marker contents", &path, source))?;
        temporary
            .file
            .set_permissions(Permissions::from_mode(MARKER_MODE))
            .map_err(|source| io_error("seal anonymous tree marker mode", &path, source))?;
        temporary
            .file
            .sync_all()
            .map_err(|source| io_error("sync sealed anonymous tree marker", &path, source))?;

        let witness = anonymous_witness(&temporary.file, &path, MARKER_MODE, FRAME_LENGTH as u64)?;
        if witness.identity != temporary.witness.identity {
            return Err(TreeMarkerError::TemporaryChanged { path });
        }
        let bytes = read_exact_frame(&temporary.file, &path)?;
        if anonymous_witness(&temporary.file, &path, MARKER_MODE, FRAME_LENGTH as u64)? != witness {
            return Err(TreeMarkerError::TemporaryChanged { path });
        }
        let actual = decode_marker(&bytes).map_err(|source| TreeMarkerError::Decode {
            path: path.clone(),
            source,
        })?;
        require_expected_token(Some(token), &actual, &path)?;
        temporary.witness = witness;
        Ok(())
    }

    fn publish_temporary(
        &self,
        temporary: TemporaryMarker,
        token: &TreeToken,
    ) -> Result<RetainedTreeMarker, TreeMarkerError> {
        self.publish_temporary_with_hook(temporary, token, || {})
    }

    fn publish_temporary_with_hook(
        &self,
        temporary: TemporaryMarker,
        token: &TreeToken,
        before_link: impl FnOnce(),
    ) -> Result<RetainedTreeMarker, TreeMarkerError> {
        self.validate_usr()?;
        if self.load_canonical()?.is_some() {
            return self.adopt_racing_canonical();
        }
        self.reject_temporary()?;
        before_link();

        match link_path_descriptor_noreplace(&temporary.file, &self.usr, MARKER_NAME) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EEXIST) => {
                return self.adopt_racing_canonical();
            }
            Err(source) => return Err(io_error("publish canonical tree marker", &self.marker_path(), source)),
        }
        let linked = canonical_witness(&temporary.file, &self.marker_path())?;
        if linked.identity != temporary.witness.identity {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.marker_path(),
            });
        }
        temporary
            .file
            .sync_all()
            .map_err(|source| io_error("sync linked canonical tree marker", &self.marker_path(), source))?;
        self.usr
            .sync_all()
            .map_err(|source| io_error("sync /usr after tree marker publication", &self.path, source))?;

        let marker = self.load_canonical()?.ok_or_else(|| TreeMarkerError::MarkerChanged {
            path: self.marker_path(),
        })?;
        if marker.witness.identity != temporary.witness.identity {
            return Err(TreeMarkerError::MarkerChanged { path: marker.path });
        }
        require_expected_token(Some(token), &marker.token, &marker.path)?;
        marker
            .file
            .sync_all()
            .map_err(|source| io_error("sync reopened canonical tree marker", &marker.path, source))?;
        self.usr
            .sync_all()
            .map_err(|source| io_error("sync /usr after canonical tree marker verification", &self.path, source))?;
        self.reject_temporary()?;
        self.validate_usr()?;
        Ok(marker)
    }

    fn adopt_racing_canonical(&self) -> Result<RetainedTreeMarker, TreeMarkerError> {
        let marker = self.load_canonical()?.ok_or_else(|| TreeMarkerError::MarkerChanged {
            path: self.marker_path(),
        })?;
        self.reject_temporary()?;
        marker
            .file
            .sync_all()
            .map_err(|source| io_error("sync racing canonical tree marker", &marker.path, source))?;
        self.usr
            .sync_all()
            .map_err(|source| io_error("sync /usr after racing tree marker publication", &self.path, source))?;
        marker.revalidate(self)?;
        Ok(marker)
    }

    fn reject_temporary(&self) -> Result<(), TreeMarkerError> {
        if self
            .open_optional(
                TEMPORARY_NAME,
                nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                "probe recovery tree marker temporary",
            )?
            .is_some()
        {
            return Err(TreeMarkerError::TemporaryPresent {
                path: self.temporary_path(),
            });
        }
        Ok(())
    }

    fn open_optional(&self, name: &CStr, flags: i32, operation: &'static str) -> Result<Option<File>, TreeMarkerError> {
        let path = component_path(&self.path, name);
        match openat2_file(self.usr.as_raw_fd(), name, flags, 0, controlled_resolution()) {
            Ok(file) => Ok(Some(file)),
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
            Err(source) => Err(io_error(operation, &path, source)),
        }
    }
}

impl RetainedTreeMarker {
    pub(crate) fn token(&self) -> &TreeToken {
        &self.token
    }

    /// Prove that both the retained descriptor and canonical name still denote
    /// the exact decoded marker. This is intended for every trigger boundary.
    pub(crate) fn revalidate(&self, store: &TreeMarkerStore) -> Result<(), TreeMarkerError> {
        store.validate_usr()?;
        store.reject_temporary()?;
        if canonical_witness(&self.file, &self.path)? != self.witness {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        let retained_token = read_and_decode(&self.file, self.witness, &self.path)?;
        require_expected_token(Some(&self.token), &retained_token, &self.path)?;
        let named = store.load_canonical()?.ok_or_else(|| TreeMarkerError::MarkerChanged {
            path: self.path.clone(),
        })?;
        if named.witness != self.witness || named.token != self.token {
            return Err(TreeMarkerError::MarkerChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

fn encode_marker(token: &TreeToken) -> [u8; FRAME_LENGTH] {
    let mut frame = [0_u8; FRAME_LENGTH];
    frame[..MAGIC_END].copy_from_slice(MAGIC);
    frame[MAGIC_END..VERSION_END].copy_from_slice(&VERSION.to_be_bytes());
    frame[VERSION_END..LENGTH_END].copy_from_slice(&(TOKEN_LENGTH as u32).to_be_bytes());
    frame[CHECKSUM_END..].copy_from_slice(token.as_str().as_bytes());
    let checksum = marker_checksum(&frame[..LENGTH_END], &frame[CHECKSUM_END..]);
    frame[LENGTH_END..CHECKSUM_END].copy_from_slice(&checksum);
    frame
}

fn decode_marker(frame: &[u8]) -> Result<TreeToken, MarkerCodecError> {
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

fn marker_checksum(header: &[u8], token: &[u8]) -> [u8; CHECKSUM_LENGTH] {
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(token);
    digest.finalize().into()
}

fn directory_witness(file: &File, path: &Path) -> Result<DirectoryWitness, TreeMarkerError> {
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

fn canonical_witness(file: &File, path: &Path) -> Result<MarkerWitness, TreeMarkerError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect canonical tree marker", path, source))?;
    let witness = MarkerWitness::from_metadata(&metadata);
    if metadata.file_type().is_file()
        && witness.owner == effective_user_id()
        && witness.mode == MARKER_MODE
        && witness.links == 1
        && witness.length == FRAME_LENGTH as u64
    {
        Ok(witness)
    } else {
        Err(unsafe_marker("canonical", path, witness))
    }
}

fn anonymous_witness(
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
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
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

fn read_and_decode(file: &File, expected: MarkerWitness, path: &Path) -> Result<TreeToken, TreeMarkerError> {
    let bytes = read_exact_frame(file, path)?;
    if canonical_witness(file, path)? != expected {
        return Err(TreeMarkerError::MarkerChanged { path: path.to_owned() });
    }
    decode_marker(&bytes).map_err(|source| TreeMarkerError::Decode {
        path: path.to_owned(),
        source,
    })
}

fn read_exact_frame(file: &File, path: &Path) -> Result<[u8; FRAME_LENGTH], TreeMarkerError> {
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

fn write_all_at(file: &File, bytes: &[u8], path: &Path) -> Result<(), TreeMarkerError> {
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

fn require_expected_token(
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

fn unsafe_marker(role: &'static str, path: &Path, witness: MarkerWitness) -> TreeMarkerError {
    TreeMarkerError::UnsafeMarker {
        role,
        path: path.to_owned(),
        owner: witness.owner,
        mode: witness.mode,
        links: witness.links,
        length: witness.length,
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> TreeMarkerError {
    TreeMarkerError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn component_path(directory: &Path, name: &CStr) -> PathBuf {
    directory.join(OsStr::from_bytes(name.to_bytes()))
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::{
            fs::{MetadataExt as _, PermissionsExt as _, symlink},
            net::UnixListener,
        },
        process::Command,
    };

    use super::*;

    fn token(digit: char) -> TreeToken {
        TreeToken::parse(digit.to_string().repeat(TreeToken::TEXT_LENGTH)).unwrap()
    }

    fn store(path: &Path) -> TreeMarkerStore {
        // tempfile follows the developer's umask and may create 0775 roots.
        // Production `/usr` is not normalized by the marker store, so make
        // the test capability explicitly owner-controlled before opening it.
        fs::set_permissions(path, Permissions::from_mode(0o700)).unwrap();
        let usr = File::open(path).unwrap();
        TreeMarkerStore::open(&usr, path).unwrap()
    }

    fn write_marker(path: &Path, token: &TreeToken, mode: u32) {
        fs::write(path, encode_marker(token)).unwrap();
        fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn marker_codec_has_one_exact_v1_golden_frame() {
        let encoded = encode_marker(&token('1'));
        assert_eq!(encoded.len(), 78);
        assert_eq!(
            hex::encode(encoded),
            "4341535454494400000100000020d93c1d589892bf2ae65c21c854059c5623f03e16d54db57320992c8c4d602ed33131313131313131313131313131313131313131313131313131313131313131"
        );
        assert_eq!(decode_marker(&encoded).unwrap(), token('1'));
    }

    #[test]
    fn marker_codec_rejects_noncanonical_or_corrupt_frames() {
        let canonical = encode_marker(&token('2'));
        for length in [0, FRAME_LENGTH - 1, FRAME_LENGTH + 1] {
            let mut candidate = canonical.to_vec();
            candidate.resize(length, 0);
            assert!(matches!(
                decode_marker(&candidate),
                Err(MarkerCodecError::InvalidLength { actual }) if actual == length
            ));
        }

        let mut invalid = canonical;
        invalid[0] ^= 1;
        assert_eq!(decode_marker(&invalid), Err(MarkerCodecError::InvalidMagic));
        let mut invalid = canonical;
        invalid[MAGIC_END..VERSION_END].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(decode_marker(&invalid), Err(MarkerCodecError::UnsupportedVersion(2)));
        let mut invalid = canonical;
        invalid[VERSION_END..LENGTH_END].copy_from_slice(&31_u32.to_be_bytes());
        assert_eq!(decode_marker(&invalid), Err(MarkerCodecError::InvalidPayloadLength(31)));
        let mut invalid = canonical;
        invalid[CHECKSUM_END] = b'3';
        assert_eq!(decode_marker(&invalid), Err(MarkerCodecError::ChecksumMismatch));

        for payload in ["0".repeat(TOKEN_LENGTH), "A".repeat(TOKEN_LENGTH)] {
            let mut invalid = canonical;
            invalid[CHECKSUM_END..].copy_from_slice(payload.as_bytes());
            let checksum = marker_checksum(&invalid[..LENGTH_END], &invalid[CHECKSUM_END..]);
            invalid[LENGTH_END..CHECKSUM_END].copy_from_slice(&checksum);
            assert_eq!(decode_marker(&invalid), Err(MarkerCodecError::InvalidToken));
        }
    }

    #[test]
    fn prejournal_creation_is_durable_immutable_and_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        let first = store.adopt_or_create_before_journal().unwrap();
        let metadata = fs::symlink_metadata(temporary.path().join(".cast-tree-id")).unwrap();
        assert_eq!(metadata.mode() & 0o7777, MARKER_MODE);
        assert_eq!(metadata.uid(), effective_user_id());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.len(), FRAME_LENGTH as u64);
        assert!(!temporary.path().join(".cast-tree-id.tmp").exists());

        let second = store.adopt_or_create_before_journal().unwrap();
        assert_eq!(second.token(), first.token());
        assert_eq!(second.witness.identity, first.witness.identity);
        first.revalidate(&store).unwrap();
        second.revalidate(&store).unwrap();
    }

    #[test]
    fn racing_canonical_is_durably_adopted_without_moving_foreign_names() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let mut anonymous = store.create_anonymous_temporary().unwrap();
        let attempted = token('3');
        let winner = token('4');
        store.write_complete_temporary(&mut anonymous, &attempted).unwrap();

        let marker = store
            .publish_temporary_with_hook(anonymous, &attempted, || {
                write_marker(&root.path().join(".cast-tree-id"), &winner, MARKER_MODE);
            })
            .unwrap();

        assert_eq!(marker.token(), &winner);
        assert_eq!(
            fs::read(root.path().join(".cast-tree-id")).unwrap(),
            encode_marker(&winner)
        );
        assert!(!root.path().join(".cast-tree-id.tmp").exists());
        let before = fs::symlink_metadata(root.path().join(".cast-tree-id")).unwrap();
        assert!(matches!(
            store.read_expected_for_recovery(&attempted),
            Err(TreeMarkerError::TokenMismatch { .. })
        ));
        let after = fs::symlink_metadata(root.path().join(".cast-tree-id")).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(
            fs::read(root.path().join(".cast-tree-id")).unwrap(),
            encode_marker(&winner)
        );
    }

    #[test]
    fn separate_usr_trees_receive_distinct_random_tokens() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first = store(first.path()).adopt_or_create_before_journal().unwrap();
        let second = store(second.path()).adopt_or_create_before_journal().unwrap();
        assert_ne!(first.token(), second.token());
    }

    #[test]
    fn named_temporary_evidence_blocks_prejournal_publication_unchanged() {
        for (contents, mode) in [
            (encode_marker(&token('3')).to_vec(), MARKER_MODE),
            (b"partial".to_vec(), 0o000),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(".cast-tree-id.tmp");
            fs::write(&path, &contents).unwrap();
            let retained = File::open(&path).unwrap();
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();

            assert!(matches!(
                store(root.path()).adopt_or_create_before_journal(),
                Err(TreeMarkerError::TemporaryPresent { .. })
            ));
            assert!(!root.path().join(".cast-tree-id").exists());
            let mut actual = vec![0_u8; contents.len()];
            retained.read_exact_at(&mut actual, 0).unwrap();
            assert_eq!(actual, contents);
            assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o7777, mode);
        }
    }

    #[test]
    fn recovery_never_promotes_or_repairs_temporary_evidence() {
        let missing = tempfile::tempdir().unwrap();
        let temporary_path = missing.path().join(".cast-tree-id.tmp");
        write_marker(&temporary_path, &token('5'), MARKER_MODE);
        let before = fs::read(&temporary_path).unwrap();
        let before_mode = fs::symlink_metadata(&temporary_path).unwrap().mode() & 0o7777;
        assert!(matches!(
            store(missing.path()).read_for_recovery(),
            Err(TreeMarkerError::TemporaryPresent { .. })
        ));
        assert!(!missing.path().join(".cast-tree-id").exists());
        assert_eq!(fs::read(&temporary_path).unwrap(), before);
        assert_eq!(
            fs::symlink_metadata(&temporary_path).unwrap().mode() & 0o7777,
            before_mode
        );

        let corrupt = tempfile::tempdir().unwrap();
        fs::write(corrupt.path().join(".cast-tree-id"), b"corrupt").unwrap();
        fs::set_permissions(
            corrupt.path().join(".cast-tree-id"),
            Permissions::from_mode(MARKER_MODE),
        )
        .unwrap();
        write_marker(&corrupt.path().join(".cast-tree-id.tmp"), &token('6'), MARKER_MODE);
        let canonical_before = fs::read(corrupt.path().join(".cast-tree-id")).unwrap();
        let temporary_before = fs::read(corrupt.path().join(".cast-tree-id.tmp")).unwrap();
        assert!(store(corrupt.path()).read_for_recovery().is_err());
        assert_eq!(
            fs::read(corrupt.path().join(".cast-tree-id")).unwrap(),
            canonical_before
        );
        assert_eq!(
            fs::read(corrupt.path().join(".cast-tree-id.tmp")).unwrap(),
            temporary_before
        );
    }

    #[test]
    fn recovery_missing_marker_is_read_only_and_never_mints() {
        let root = tempfile::tempdir().unwrap();
        let before = fs::read_dir(root.path()).unwrap().count();

        assert!(matches!(
            store(root.path()).read_for_recovery(),
            Err(TreeMarkerError::Missing { .. })
        ));

        assert_eq!(fs::read_dir(root.path()).unwrap().count(), before);
        assert!(!root.path().join(".cast-tree-id").exists());
        assert!(!root.path().join(".cast-tree-id.tmp").exists());
    }

    #[test]
    fn recovery_requires_the_exact_journal_token() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(temporary.path());
        let marker = store.adopt_or_create_before_journal().unwrap();
        let actual = marker.token().clone();
        let mismatch = if actual == token('7') { token('8') } else { token('7') };
        assert!(matches!(
            store.read_expected_for_recovery(&mismatch),
            Err(TreeMarkerError::TokenMismatch { expected, actual, .. })
                if expected == mismatch.as_str() && actual == marker.token().as_str()
        ));
        assert_eq!(store.read_expected_for_recovery(&actual).unwrap().token(), &actual);
    }

    #[test]
    fn canonical_marker_rejects_links_wrong_kinds_and_mutable_modes() {
        for attack in ["symlink", "hardlink", "directory", "fifo", "socket", "mode"] {
            let root = tempfile::tempdir().unwrap();
            let marker = root.path().join(".cast-tree-id");
            let external = root.path().join("external");
            let mut listener = None;
            match attack {
                "symlink" => {
                    fs::write(&external, b"external").unwrap();
                    symlink("external", &marker).unwrap();
                }
                "hardlink" => {
                    write_marker(&external, &token('9'), MARKER_MODE);
                    fs::hard_link(&external, &marker).unwrap();
                }
                "directory" => fs::create_dir(&marker).unwrap(),
                "fifo" => {
                    let encoded = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
                    // SAFETY: encoded is one live NUL-terminated pathname.
                    assert_eq!(unsafe { nix::libc::mkfifo(encoded.as_ptr(), 0o444) }, 0);
                }
                "socket" => listener = Some(UnixListener::bind(&marker).unwrap()),
                "mode" => write_marker(&marker, &token('9'), 0o644),
                _ => unreachable!(),
            }
            let external_before = fs::read(&external).ok();
            assert!(store(root.path()).read_for_recovery().is_err(), "accepted {attack}");
            assert_eq!(fs::read(&external).ok(), external_before, "modified {attack} target");
            if attack == "mode" {
                assert_eq!(fs::symlink_metadata(&marker).unwrap().mode() & 0o7777, 0o644);
            }
            drop(listener);
        }
    }

    #[test]
    fn hostile_temporary_entries_are_rejected_without_touching_targets() {
        for attack in ["symlink", "hardlink"] {
            let root = tempfile::tempdir().unwrap();
            let external = root.path().join("external");
            fs::write(&external, b"external bytes").unwrap();
            let path = root.path().join(".cast-tree-id.tmp");
            match attack {
                "symlink" => symlink("external", &path).unwrap(),
                "hardlink" => fs::hard_link(&external, &path).unwrap(),
                _ => unreachable!(),
            }
            assert!(store(root.path()).adopt_or_create_before_journal().is_err());
            assert_eq!(fs::read(&external).unwrap(), b"external bytes");
        }
    }

    #[test]
    fn retained_marker_detects_same_content_inode_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let marker = store.adopt_or_create_before_journal().unwrap();
        let old = root.path().join("old-marker");
        fs::rename(root.path().join(".cast-tree-id"), &old).unwrap();
        write_marker(&root.path().join(".cast-tree-id"), marker.token(), MARKER_MODE);

        assert!(matches!(
            marker.revalidate(&store),
            Err(TreeMarkerError::MarkerChanged { .. })
        ));
        assert_eq!(fs::read(&old).unwrap(), encode_marker(marker.token()));
    }

    #[test]
    fn marker_creation_normalizes_hostile_umasks() {
        const CHILD: &str = "CAST_TREE_MARKER_UMASK_CHILD";
        const ROOT: &str = "CAST_TREE_MARKER_UMASK_ROOT";
        const TEST: &str = "tree_marker::tests::marker_creation_normalizes_hostile_umasks";
        if let Some(mask) = std::env::var_os(CHILD) {
            let mask = u32::from_str_radix(mask.to_str().unwrap(), 8).unwrap();
            // SAFETY: the exact test runs alone in this throwaway process.
            unsafe { nix::libc::umask(mask) };
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let marker = store(&root).adopt_or_create_before_journal().unwrap();
            marker.revalidate(&store(&root)).unwrap();
            assert_eq!(
                fs::symlink_metadata(root.join(".cast-tree-id")).unwrap().mode() & 0o7777,
                MARKER_MODE
            );
            return;
        }

        for mask in ["0002", "0777"] {
            let root = tempfile::tempdir().unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD, mask)
                .env(ROOT, root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "tree-marker umask child {mask} failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn install_default_acl(path: &Path) -> io::Result<()> {
        const ACL: [u8; 28] = [
            0x02, 0x00, 0x00, 0x00, // version
            0x01, 0x00, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff, // user object
            0x04, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // group object
            0x20, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // other
        ];
        let directory = File::open(path)?;
        // SAFETY: the descriptor, static name, and canonical ACL bytes remain live.
        if unsafe {
            nix::libc::fsetxattr(
                directory.as_raw_fd(),
                c"system.posix_acl_default".as_ptr(),
                ACL.as_ptr().cast(),
                ACL.len(),
                0,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[test]
    fn marker_store_rejects_inheritable_default_acl() {
        let root = tempfile::tempdir().unwrap();
        match install_default_acl(root.path()) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
            Err(source) => panic!("install tree-marker test default ACL: {source}"),
        }
        let usr = File::open(root.path()).unwrap();
        assert!(TreeMarkerStore::open(&usr, root.path()).is_err());
        assert!(!root.path().join(".cast-tree-id").exists());
    }
}
