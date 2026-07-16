//! Descriptor-bound metadata decoration for a retained candidate `/usr`.
//!
//! Candidate metadata is never opened through the mutable staging pathname.
//! The state-transition or archived-repair guard supplies the exact retained
//! descriptor; this module retains every directory and input inode below it
//! with `openat2(2)` and publishes new regular files from anonymous
//! `O_TMPFILE` inodes. Existing output names are deliberately rejected rather
//! than replaced: Linux has no rename primitive which can condition
//! replacement on an already-retained destination inode, and leaving that
//! inode untouched is safer than path-based overwrite or cleanup.

use std::{
    ffi::CStr,
    fs::{File, Permissions},
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use thiserror::Error as ThisError;

use super::Error;
use crate::{
    SystemModel,
    linux_fs::{
        controlled_resolution, link_path_descriptor_noreplace, open_path_descriptor_readonly_until, openat2_file,
        require_no_access_acl, require_no_default_acl,
    },
    transition_identity::{ArchivedStateRepairIdentity, StatefulTreeIdentity},
};

mod private_directory;
mod retained_inode;

use retained_inode::{
    directory_witness, effective_user_id, file_type_name, metadata_io, published_witness, read_exact_at,
};

#[cfg(test)]
pub(super) fn arm_applied_private_directory_publication_error(after_parent_sync: impl FnOnce() + 'static) {
    private_directory::arm_applied_publication_error(after_parent_sync);
}

#[cfg(test)]
std::thread_local! {
    static CANDIDATE_USR_CLONE_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn arm_candidate_usr_clone_fault() {
    CANDIDATE_USR_CLONE_FAULT.with(|fault| {
        assert!(!fault.replace(true), "candidate /usr clone fault is already armed");
    });
}

#[cfg(test)]
pub(super) fn assert_candidate_usr_clone_fault_consumed() {
    CANDIDATE_USR_CLONE_FAULT.with(|fault| {
        assert!(!fault.get(), "candidate /usr clone fault was not consumed");
    });
}

const LIB_NAME: &CStr = c"lib";
const USR_NAME: &CStr = c"usr";
const OS_INFO_NAME: &CStr = c"os-info.json";
const OS_RELEASE_NAME: &CStr = c"os-release";
const SYSTEM_SNAPSHOT_NAME: &CStr = c"system-model.glu";
const TEMPORARY_FILE_MODE: u32 = 0o600;
const CANONICAL_FILE_MODE: u32 = 0o644;
const MAX_METADATA_BYTES: usize = 1024 * 1024;
// One-byte positive progress is still valid for a regular file. Admit that
// worst case for every bounded byte plus a finite interruption allowance.
const MAX_IO_ATTEMPTS: usize = MAX_METADATA_BYTES + 4_096;
const DESCRIPTOR_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) const GENERIC_OS_RELEASE: &str = r#"NAME="Unbranded OS"
VERSION="no-os-info.json"
ID="unbranded-os"
VERSION_CODENAME=no-os-info.json
VERSION_ID="no-os-info.json"
PRETTY_NAME="Unbranded OS no-os-info.json - I forgot to add os-info.json"
HOME_URL="https://github.com/AerynOS/os-info"
BUG_REPORT_URL="https://github.com/AerynOS/os-info/issues""#;

#[derive(Debug, ThisError)]
enum MetadataError {
    #[error("{operation} candidate metadata path `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "unsafe candidate metadata directory `{}` (type={kind}, uid={owner}, mode={mode:04o})",
        path.display()
    )]
    UnsafeDirectory {
        path: PathBuf,
        kind: &'static str,
        owner: u32,
        mode: u32,
    },
    #[error("candidate metadata directory changed while retained at `{}`", path.display())]
    DirectoryChanged { path: PathBuf },
    #[error(
        "unsafe candidate metadata input `{}` (type={kind}, uid={owner}, mode={mode:04o}, links={links}, length={length})",
        path.display()
    )]
    UnsafeInput {
        path: PathBuf,
        kind: &'static str,
        owner: u32,
        mode: u32,
        links: u64,
        length: u64,
    },
    #[error("candidate metadata input `{}` exceeds the {limit}-byte limit (got {actual})", path.display())]
    InputTooLarge { path: PathBuf, limit: usize, actual: u64 },
    #[error("generated candidate metadata `{name}` exceeds the {limit}-byte limit (got {actual})")]
    OutputTooLarge {
        name: &'static str,
        limit: usize,
        actual: usize,
    },
    #[error(
        "candidate metadata destination `{}` already exists as {kind} (uid={owner}, mode={mode:04o}, links={links}); refusing non-conditional replacement",
        path.display()
    )]
    DestinationExists {
        path: PathBuf,
        kind: &'static str,
        owner: u32,
        mode: u32,
        links: u64,
    },
    #[error("candidate metadata inode changed while retained at `{}`", path.display())]
    FileChanged { path: PathBuf },
    #[error("candidate metadata publication collided at `{}`", path.display())]
    PublicationCollision {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot reserve a private candidate metadata directory after {limit} attempts")]
    PrivateDirectoryExhausted { limit: usize },
    #[error("private candidate metadata directory `{}` unexpectedly contains `{entry}`", path.display())]
    PrivateDirectoryNotEmpty { path: PathBuf, entry: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Debug)]
struct RetainedDirectory {
    file: File,
    path: PathBuf,
    witness: DirectoryWitness,
}

#[derive(Debug)]
struct PreparedFile {
    file: File,
    identity: (u64, u64),
}

#[derive(Clone, Copy, Debug)]
enum MetadataContext {
    ArchivedRepair,
    Ephemeral,
    Stateful,
}

/// Exact external `/usr` inode retained beneath the already-authenticated
/// ephemeral materialization root. The diagnostic path is never reopened.
#[derive(Debug)]
pub(super) struct RetainedEphemeralUsr {
    directory: RetainedDirectory,
}

/// Retains the exact generated files and their parent directories until the
/// transition has crossed every trigger and publication boundary. A caller
/// must not treat successful decoration as a one-time pathname check: the
/// proof is deliberately revalidated while these descriptors remain live.
#[derive(Debug)]
pub(super) struct CandidateMetadataProof {
    context: MetadataContext,
    usr: File,
    usr_path: PathBuf,
    lib: RetainedDirectory,
    release: PreparedFile,
    release_bytes: Vec<u8>,
    snapshot: PreparedFile,
    snapshot_bytes: Vec<u8>,
}

pub(super) fn decorate_archived(
    identity: &ArchivedStateRepairIdentity,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    let (usr, usr_path) = identity.retained_candidate_usr();
    decorate_retained(MetadataContext::ArchivedRepair, usr, usr_path, snapshot)
        .map_err(|source| metadata_error(MetadataContext::ArchivedRepair, source))
}

pub(super) fn decorate_stateful(
    identity: &StatefulTreeIdentity,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    let (usr, usr_path) = identity.retained_candidate_usr();
    decorate_retained(MetadataContext::Stateful, usr, usr_path, snapshot)
        .map_err(|source| metadata_error(MetadataContext::Stateful, source))
}

pub(super) fn retain_ephemeral_usr(root: &File, root_path: &Path) -> Result<RetainedEphemeralUsr, Error> {
    let context = MetadataContext::Ephemeral;
    let directory = RetainedDirectory::retain_or_create(root, USR_NAME, root_path.join("usr"))
        .map_err(|source| metadata_error(context, source))?;
    directory
        .require_named(root, USR_NAME)
        .map_err(|source| metadata_error(context, source))?;
    Ok(RetainedEphemeralUsr { directory })
}

pub(super) fn decorate_ephemeral(
    usr: &RetainedEphemeralUsr,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    decorate_retained(MetadataContext::Ephemeral, usr.file(), usr.diagnostic_path(), snapshot)
        .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))
}

impl RetainedEphemeralUsr {
    pub(super) fn file(&self) -> &File {
        &self.directory.file
    }

    pub(super) fn diagnostic_path(&self) -> &Path {
        &self.directory.path
    }

    pub(super) fn revalidate_under(&self, root: &File) -> Result<(), Error> {
        self.directory
            .require_named(root, USR_NAME)
            .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))
    }

    /// Materialization may temporarily widen the retained directory mode and
    /// then apply the declarative final mode. Re-observe that same descriptor
    /// only after the blit has finished; never reacquire `usr` by pathname.
    pub(super) fn refresh_after_materialization(&mut self, root: &File) -> Result<(), Error> {
        self.directory.witness = directory_witness(&self.directory.file, &self.directory.path)
            .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))?;
        require_no_access_acl(&self.directory.file, &self.directory.path).map_err(|source| {
            metadata_error(
                MetadataContext::Ephemeral,
                metadata_io(
                    "reject access ACL on retained ephemeral /usr",
                    &self.directory.path,
                    source,
                ),
            )
        })?;
        require_no_default_acl(&self.directory.file, &self.directory.path).map_err(|source| {
            metadata_error(
                MetadataContext::Ephemeral,
                metadata_io(
                    "reject default ACL on retained ephemeral /usr",
                    &self.directory.path,
                    source,
                ),
            )
        })?;
        self.revalidate_under(root)
    }
}

fn decorate_retained(
    context: MetadataContext,
    usr: &File,
    usr_path: &Path,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, MetadataError> {
    let usr = clone_candidate_usr(usr, usr_path)?;
    let snapshot = bounded_output("system-model.glu", snapshot.encoded().as_bytes())?.to_vec();
    let lib = RetainedDirectory::retain_or_create(&usr, LIB_NAME, usr_path.join("lib"))?;
    let os_release = load_os_release(&lib)?;
    let os_release = bounded_output("os-release", os_release.as_bytes())?.to_vec();

    // Refuse every deterministic conflict before either canonical name is
    // published. A racing conflict after this point still cannot be replaced
    // because descriptor linking is no-replace.
    lib.require_named(&usr, LIB_NAME)?;
    lib.require_absent(OS_RELEASE_NAME)?;
    lib.require_absent(SYSTEM_SNAPSHOT_NAME)?;
    let prepared_release = PreparedFile::new(&lib, &os_release, lib.path.join("os-release"))?;
    let prepared_snapshot = PreparedFile::new(&lib, &snapshot, lib.path.join("system-model.glu"))?;

    publish(&usr, &lib, OS_RELEASE_NAME, &prepared_release, &os_release)?;
    lib.require_named(&usr, LIB_NAME)?;
    after_first_publication();
    publish(&usr, &lib, SYSTEM_SNAPSHOT_NAME, &prepared_snapshot, &snapshot)?;
    lib.require_named(&usr, LIB_NAME)?;
    lib.sync()?;
    usr.sync_all()
        .map_err(|source| metadata_io("sync candidate /usr after metadata decoration", usr_path, source))?;
    let proof = CandidateMetadataProof {
        context,
        usr,
        usr_path: usr_path.to_owned(),
        lib,
        release: prepared_release,
        release_bytes: os_release,
        snapshot: prepared_snapshot,
        snapshot_bytes: snapshot,
    };
    proof.revalidate_inner()?;
    Ok(proof)
}

fn clone_candidate_usr(usr: &File, usr_path: &Path) -> Result<File, MetadataError> {
    #[cfg(test)]
    if CANDIDATE_USR_CLONE_FAULT.with(|fault| fault.replace(false)) {
        return Err(metadata_io(
            "retain candidate /usr for metadata proof",
            usr_path,
            io::Error::other("injected candidate /usr clone failure"),
        ));
    }
    usr.try_clone()
        .map_err(|source| metadata_io("retain candidate /usr for metadata proof", usr_path, source))
}

impl CandidateMetadataProof {
    pub(super) fn revalidate(&self) -> Result<(), Error> {
        self.revalidate_inner()
            .map_err(|source| metadata_error(self.context, source))
    }

    pub(super) fn diagnostic_path(&self) -> &Path {
        &self.usr_path
    }

    fn revalidate_inner(&self) -> Result<(), MetadataError> {
        require_published_pair(
            &self.usr,
            &self.lib,
            &self.release,
            &self.release_bytes,
            &self.snapshot,
            &self.snapshot_bytes,
        )
    }
}

fn metadata_error(context: MetadataContext, source: MetadataError) -> Error {
    match context {
        MetadataContext::ArchivedRepair => Error::ArchivedStateRepair {
            source: Box::new(source),
        },
        MetadataContext::Ephemeral => Error::EphemeralCandidateMetadata {
            source: Box::new(source),
        },
        MetadataContext::Stateful => Error::StatefulCandidateMetadata {
            source: Box::new(source),
        },
    }
}

fn bounded_output<'a>(name: &'static str, bytes: &'a [u8]) -> Result<&'a [u8], MetadataError> {
    if bytes.len() <= MAX_METADATA_BYTES {
        Ok(bytes)
    } else {
        Err(MetadataError::OutputTooLarge {
            name,
            limit: MAX_METADATA_BYTES,
            actual: bytes.len(),
        })
    }
}

fn load_os_release(lib: &RetainedDirectory) -> Result<String, MetadataError> {
    let path = lib.path.join("os-info.json");
    let Some(bytes) = read_optional_input(lib, OS_INFO_NAME, &path)? else {
        return Ok(GENERIC_OS_RELEASE.to_owned());
    };
    let Ok(source) = std::str::from_utf8(&bytes) else {
        return Ok(GENERIC_OS_RELEASE.to_owned());
    };
    let Ok(info) = os_info::load_os_info(source) else {
        return Ok(GENERIC_OS_RELEASE.to_owned());
    };
    let release: os_info::OsRelease = (&info).into();
    Ok(release.to_string())
}

fn read_optional_input(
    directory: &RetainedDirectory,
    name: &CStr,
    path: &Path,
) -> Result<Option<Vec<u8>>, MetadataError> {
    directory.require_retained()?;
    let pinned = match openat2_file(
        directory.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(metadata_io("retain optional metadata input", path, source)),
    };
    let witness = input_witness(&pinned, path)?;
    let readable = open_path_descriptor_readonly_until(
        &pinned,
        Instant::now()
            .checked_add(DESCRIPTOR_READ_TIMEOUT)
            .unwrap_or_else(Instant::now),
    )
    .map_err(|source| metadata_io("open retained metadata input for reading", path, source))?;
    if input_witness(&readable, path)? != witness {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    let length = usize::try_from(witness.length).map_err(|_| MetadataError::InputTooLarge {
        path: path.to_owned(),
        limit: MAX_METADATA_BYTES,
        actual: witness.length,
    })?;
    let bytes = read_exact_at(&readable, length, path, "read retained metadata input")?;
    if input_witness(&pinned, path)? != witness || input_witness(&readable, path)? != witness {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    let named = openat2_file(
        directory.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| metadata_io("reopen retained metadata input name", path, source))?;
    if input_witness(&named, path)? != witness {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    directory.require_retained()?;
    Ok(Some(bytes))
}

fn input_witness(file: &File, path: &Path) -> Result<FileWitness, MetadataError> {
    let metadata = file
        .metadata()
        .map_err(|source| metadata_io("inspect retained metadata input", path, source))?;
    let witness = FileWitness::from_metadata(&metadata);
    if witness.length > MAX_METADATA_BYTES as u64 {
        return Err(MetadataError::InputTooLarge {
            path: path.to_owned(),
            limit: MAX_METADATA_BYTES,
            actual: witness.length,
        });
    }
    if !metadata.file_type().is_file()
        || witness.owner != effective_user_id()
        || witness.mode & 0o7000 != 0
        || witness.mode & 0o022 != 0
        || witness.mode & 0o400 == 0
    {
        return Err(MetadataError::UnsafeInput {
            path: path.to_owned(),
            kind: file_type_name(&metadata.file_type()),
            owner: witness.owner,
            mode: witness.mode,
            links: witness.links,
            length: witness.length,
        });
    }
    Ok(witness)
}

fn publish(
    parent: &File,
    directory: &RetainedDirectory,
    name: &CStr,
    prepared: &PreparedFile,
    expected: &[u8],
) -> Result<(), MetadataError> {
    let path = directory.path.join(name.to_string_lossy().as_ref());
    directory.require_named(parent, LIB_NAME)?;
    directory.require_retained()?;
    directory.require_absent(name)?;
    directory.require_named(parent, LIB_NAME)?;
    before_publication(name);
    let result = link_path_descriptor_noreplace(&prepared.file, &directory.file, name);
    if let Err(source) = result {
        match named_identity(directory, name, &path)? {
            Some(identity) if identity == prepared.identity => {}
            _ => return Err(MetadataError::PublicationCollision { path, source }),
        }
    }

    directory.require_named(parent, LIB_NAME)?;
    require_published(directory, name, &prepared.file, prepared.identity, expected, &path)?;
    prepared
        .file
        .sync_all()
        .map_err(|source| metadata_io("sync published candidate metadata", &path, source))?;
    directory.sync()?;
    directory.require_named(parent, LIB_NAME)?;
    require_published(directory, name, &prepared.file, prepared.identity, expected, &path)?;
    directory.require_named(parent, LIB_NAME)
}

fn require_published_pair(
    parent: &File,
    directory: &RetainedDirectory,
    release: &PreparedFile,
    expected_release: &[u8],
    snapshot: &PreparedFile,
    expected_snapshot: &[u8],
) -> Result<(), MetadataError> {
    let release_path = directory.path.join("os-release");
    let snapshot_path = directory.path.join("system-model.glu");

    // Keep both anonymous-file capabilities alive through the complete pair
    // publication. Verify the pair in both orders between exact `lib` name
    // proofs so removing or replacing the first output while the second is
    // being published cannot be mistaken for a successful decoration.
    directory.require_named(parent, LIB_NAME)?;
    require_published(
        directory,
        OS_RELEASE_NAME,
        &release.file,
        release.identity,
        expected_release,
        &release_path,
    )?;
    require_published(
        directory,
        SYSTEM_SNAPSHOT_NAME,
        &snapshot.file,
        snapshot.identity,
        expected_snapshot,
        &snapshot_path,
    )?;
    require_published(
        directory,
        SYSTEM_SNAPSHOT_NAME,
        &snapshot.file,
        snapshot.identity,
        expected_snapshot,
        &snapshot_path,
    )?;
    require_published(
        directory,
        OS_RELEASE_NAME,
        &release.file,
        release.identity,
        expected_release,
        &release_path,
    )?;
    directory.require_named(parent, LIB_NAME)
}

#[cfg(test)]
std::thread_local! {
    static AFTER_FIRST_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<(&'static str, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_after_first_publication(hook: impl FnOnce() + 'static) {
    AFTER_FIRST_PUBLICATION.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "metadata publication hook is already armed");
    });
}

#[cfg(test)]
pub(super) fn arm_before_publication(name: &'static str, hook: impl FnOnce() + 'static) {
    assert!(matches!(name, "os-release" | "system-model.glu"));
    BEFORE_PUBLICATION.with(|slot| {
        let previous = slot.borrow_mut().replace((name, Box::new(hook)));
        assert!(previous.is_none(), "metadata publication hook is already armed");
    });
}

fn before_publication(_name: &CStr) {
    #[cfg(test)]
    BEFORE_PUBLICATION.with(|slot| {
        let matches = slot
            .borrow()
            .as_ref()
            .is_some_and(|(expected, _)| _name.to_bytes() == expected.as_bytes());
        if matches {
            let (_, hook) = slot.borrow_mut().take().expect("matched metadata publication hook");
            hook();
        }
    });
}

fn after_first_publication() {
    #[cfg(test)]
    AFTER_FIRST_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn require_published(
    directory: &RetainedDirectory,
    name: &CStr,
    retained: &File,
    identity: (u64, u64),
    expected: &[u8],
    path: &Path,
) -> Result<(), MetadataError> {
    let retained_witness = published_witness(retained, path, expected.len())?;
    if (retained_witness.device, retained_witness.inode) != identity {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    let named = openat2_file(
        directory.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| metadata_io("reopen published candidate metadata", path, source))?;
    if published_witness(&named, path, expected.len())? != retained_witness {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    let readable = open_path_descriptor_readonly_until(
        &named,
        Instant::now()
            .checked_add(DESCRIPTOR_READ_TIMEOUT)
            .unwrap_or_else(Instant::now),
    )
    .map_err(|source| metadata_io("open published metadata through retained descriptor", path, source))?;
    if published_witness(&readable, path, expected.len())? != retained_witness
        || read_exact_at(&readable, expected.len(), path, "read back published metadata")? != expected
        || published_witness(retained, path, expected.len())? != retained_witness
    {
        return Err(MetadataError::FileChanged { path: path.to_owned() });
    }
    require_no_access_acl(retained, path)
        .map_err(|source| metadata_io("reject access ACL on published metadata", path, source))?;
    directory.require_retained()
}

fn named_identity(
    directory: &RetainedDirectory,
    name: &CStr,
    path: &Path,
) -> Result<Option<(u64, u64)>, MetadataError> {
    match openat2_file(
        directory.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|source| metadata_io("inspect metadata publication result", path, source))?;
            Ok(Some((metadata.dev(), metadata.ino())))
        }
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(metadata_io("reconcile metadata publication result", path, source)),
    }
}
