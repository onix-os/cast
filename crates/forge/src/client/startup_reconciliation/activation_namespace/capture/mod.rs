mod model;
mod reverse_exchange;
mod wrappers;

use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, IntoRawFd as _},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileExt as _, MetadataExt as _},
        },
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    time::{Duration, Instant},
};

use crate::{
    Installation,
    linux_fs::{
        controlled_resolution, openat2_file, require_no_access_acl, require_no_access_acl_until,
        require_no_default_acl, require_no_default_acl_until, require_no_xattrs_until,
    },
    transition_journal::{
        AbortDisposition, Operation, QuarantineName, RuntimeEpoch, RuntimeEvidenceError, RuntimeTreeIdentity,
        TransitionRecord, TreeToken,
    },
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

use model::*;
pub(super) use model::{NamespaceSnapshot, StateIdObservation, TreeLocation, UsrFingerprint, WrapperFingerprint};
#[allow(unused_imports)] // consumed when the reverse-effect executor is wired
pub(super) use reverse_exchange::{
    AppliedReverseExchangeReconciliation, DurableAppliedReverseExchangeReconciliation, DurableReverseExchangeNamespace,
    PendingReverseExchangeReconciliation, ProjectedReverseNamespace, RetainedReverseExchangeParents,
    ReverseExchangeCaptureError, ReverseExchangeDurabilityError, ReverseExchangeParentIdentity,
    ReverseExchangeReconciliation,
};
#[cfg(test)]
#[allow(unused_imports)] // exported for the later authority-level ambiguity contracts
pub(in crate::client) use reverse_exchange::{
    ReverseExchangeDurabilityEvent, ReverseExchangeDurabilityFaultPoint,
    arm_before_reverse_exchange_durable_revalidation_capture, arm_before_reverse_exchange_final_pre_capture,
    arm_before_reverse_exchange_installation_root_sync, arm_before_reverse_exchange_reconciliation_capture,
    arm_reverse_exchange_durability_fault, reset_reverse_exchange_durability_events,
    take_reverse_exchange_durability_events,
};
use wrappers::*;

const MAX_NAMESPACE_ENTRIES: usize = 1_024;
const MAX_WRAPPER_ENTRIES: usize = 32;
const MAX_NAME_BYTES: usize = 64 * 1_024;
const MAX_OPERATIONS: usize = 32 * 1_024;
const INVENTORY_DEADLINE: Duration = Duration::from_secs(10);
const ROOT_ABI_LINKS: [(&[u8], &[u8]); 5] = [
    (b"bin", b"usr/bin"),
    (b"sbin", b"usr/sbin"),
    (b"lib", b"usr/lib"),
    (b"lib32", b"usr/lib32"),
    (b"lib64", b"usr/lib"),
];
const ISOLATION_SCAFFOLD_DIRECTORIES: [&[u8]; 6] = [b"etc", b"usr", b"proc", b"tmp", b"sys", b"dev"];

pub(super) fn capture_snapshot(
    installation: &Installation,
    record: &TransitionRecord,
) -> Result<NamespaceSnapshot, CaptureError> {
    let mut budget = Budget::new()?;
    installation
        .revalidate_mutable_namespace()
        .map_err(CaptureError::Installation)?;
    let root_path = installation.root.clone();
    let root = open_directory(installation.root_directory(), c".", &root_path, &mut budget)?;
    let root_witness = controlled_directory_witness(&root, &root_path)?;
    let epoch_before = RuntimeEpoch::capture().map_err(CaptureError::RuntimeEpoch)?;
    let root_abi = inspect_root_abi(&root, &root_path, &mut budget)?;

    let live = inspect_usr(
        &root,
        c"usr",
        installation.root.join("usr"),
        TreeLocation::Live,
        &mut budget,
    )?
    .ok_or_else(|| CaptureError::RequiredTreeMissing {
        location: TreeLocation::Live,
    })?;
    let roots_path = installation.root_path("");
    let roots = open_directory(&root, c".cast/root", &roots_path, &mut budget)?;
    let roots_witness = controlled_directory_witness(&roots, &roots_path)?;
    let mut roots_entries = inspect_roots(&roots, &roots_path, record, &mut budget)?;
    let isolation = roots_entries
        .iter()
        .find(|entry| entry.fingerprint.name == b"isolation")
        .ok_or(CaptureError::FixedWrapperMissing { name: "isolation" })?;
    let isolation_abi = inspect_root_abi(&isolation.directory, &installation.isolation_dir(), &mut budget)?;

    let quarantine_path = installation.state_quarantine_dir();
    let quarantine = open_directory(&root, c".cast/quarantine", &quarantine_path, &mut budget)?;
    let quarantine_witness = controlled_directory_witness(&quarantine, &quarantine_path)?;
    let mut quarantine_entries = inspect_quarantine(&quarantine, &quarantine_path, record, &mut budget)?;

    authenticate_slot_links(record, &live, &mut roots_entries, &mut quarantine_entries)?;
    reject_duplicate_tree_tokens(&live, &roots_entries, &quarantine_entries)?;
    let epoch_after = RuntimeEpoch::capture().map_err(CaptureError::RuntimeEpoch)?;
    if epoch_before != epoch_after {
        return Err(CaptureError::RuntimeEpochChanged);
    }
    installation
        .revalidate_mutable_namespace()
        .map_err(CaptureError::Installation)?;

    let roots_fingerprint = roots_entries.iter().map(|entry| entry.fingerprint.clone()).collect();
    let quarantine_fingerprint = quarantine_entries
        .iter()
        .map(|entry| entry.fingerprint.clone())
        .collect();
    let fingerprint = NamespaceFingerprint {
        root: root_witness,
        roots: roots_witness,
        quarantine: quarantine_witness,
        epoch: epoch_after,
        live: live.fingerprint.clone(),
        root_abi: root_abi.fingerprint.clone(),
        isolation_abi: isolation_abi.fingerprint.clone(),
        roots_entries: roots_fingerprint,
        quarantine_entries: quarantine_fingerprint,
    };
    let snapshot = NamespaceSnapshot {
        root,
        root_path,
        roots,
        roots_path,
        quarantine,
        quarantine_path,
        live,
        root_abi,
        isolation_abi,
        roots_entries,
        quarantine_entries,
        fingerprint,
    };
    // Close the walk with the same descriptor/public-name proof used after
    // the second startup inventory.  A coherent set of individually safe
    // descriptors is not sufficient if a parent or wrapper name changed
    // while the bounded walk was in progress.
    snapshot.revalidate_retained()?;
    Ok(snapshot)
}

fn inspect_root_abi(directory: &File, path: &Path, budget: &mut Budget) -> Result<RetainedRootAbi, CaptureError> {
    let mut links = Vec::with_capacity(ROOT_ABI_LINKS.len());
    for (name, target) in ROOT_ABI_LINKS {
        let mut temporary = name.to_vec();
        temporary.extend_from_slice(b".next");
        if name_exists(
            directory,
            cstring(&temporary)?.as_c_str(),
            &path.join(os(&temporary)),
            budget,
        )? {
            return Err(CaptureError::RootAbiTemporary {
                path: path.join(os(&temporary)),
            });
        }
        let encoded = cstring(name)?;
        let link_path = path.join(os(name));
        let Some(file) = open_optional_path(directory, &encoded, &link_path, budget)? else {
            links.push(None);
            continue;
        };
        let witness = InodeWitness::read(&file, &link_path)?;
        if witness.kind() != nix::libc::S_IFLNK {
            return Err(CaptureError::RootAbiType { path: link_path });
        }
        let actual = read_link(&file, &link_path, budget)?;
        if actual != target {
            return Err(CaptureError::RootAbiTarget {
                path: link_path,
                expected: target.to_vec(),
                actual,
            });
        }
        links.push(Some(RetainedRootAbiLink {
            file,
            fingerprint: RootAbiLinkFingerprint {
                name: name.to_vec(),
                target: target.to_vec(),
                witness,
            },
        }));
    }
    let fingerprint = RootAbiFingerprint {
        links: links
            .iter()
            .map(|link| link.as_ref().map(|link| link.fingerprint.clone()))
            .collect(),
    };
    Ok(RetainedRootAbi { links, fingerprint })
}

fn classify_root_name(name: &[u8], record: &TransitionRecord) -> Result<RootEntryRole, CaptureError> {
    match name {
        b"staging" => return Ok(RootEntryRole::Staging),
        b"isolation" => return Ok(RootEntryRole::Isolation),
        _ => {}
    }
    if let Some(state) = parse_positive_decimal(name) {
        return Ok(RootEntryRole::State(state));
    }
    if let Some((state, token, index)) = parse_indexed_name(name, b".archived-candidate-slot-") {
        if token == record.candidate.tree_token.as_str() || token == record.previous.tree_token.as_str() {
            return Ok(RootEntryRole::ArchivedCandidateParking {
                state,
                token: token.to_owned(),
                index,
            });
        }
    }
    if let Some((state, token, index)) = parse_indexed_name(name, b".previous-slot-")
        && token == record.previous.tree_token.as_str()
    {
        return Ok(RootEntryRole::PreviousParking { state, index });
    }
    Err(CaptureError::UnexpectedRootName { name: name.to_vec() })
}

fn classify_quarantine_name(name: &[u8], record: &TransitionRecord) -> Result<QuarantineEntryRole, CaptureError> {
    if name == record.quarantine_name.as_str().as_bytes() {
        return Ok(QuarantineEntryRole::Transition);
    }
    if let Some((state, token, index)) = parse_indexed_name(name, b"replaced-active-reblit-wrapper-")
        && record.operation == Operation::ActiveReblit
        && token == record.previous.tree_token.as_str()
    {
        return Ok(QuarantineEntryRole::ActiveReblitWrapper { state, index });
    }
    let text = std::str::from_utf8(name).map_err(|_| CaptureError::UnexpectedQuarantineName { name: name.to_vec() })?;
    QuarantineName::parse(text.to_owned())
        .map_err(|_| CaptureError::UnexpectedQuarantineName { name: name.to_vec() })?;
    Ok(QuarantineEntryRole::Ambient)
}

fn parse_indexed_name<'a>(name: &'a [u8], prefix: &[u8]) -> Option<(i32, &'a str, usize)> {
    let tail = name.strip_prefix(prefix)?;
    let text = std::str::from_utf8(tail).ok()?;
    let mut parts = text.split('-');
    let state = parse_positive_decimal(parts.next()?.as_bytes())?;
    let token = parts.next()?;
    TreeToken::parse(token.to_owned()).ok()?;
    let index_text = parts.next()?;
    if parts.next().is_some() || (index_text.len() > 1 && index_text.starts_with('0')) {
        return None;
    }
    let index = index_text.parse().ok()?;
    (index < 256).then_some((state, token, index))
}

fn parse_slot_name(name: &[u8]) -> Option<(i32, String)> {
    let tail = name.strip_prefix(b".cast-state-slot-")?;
    let text = std::str::from_utf8(tail).ok()?;
    let split = text.find('-')?;
    let state = parse_positive_decimal(text[..split].as_bytes())?;
    let token = &text[split + 1..];
    TreeToken::parse(token.to_owned()).ok()?;
    Some((state, token.to_owned()))
}

fn parse_positive_decimal(bytes: &[u8]) -> Option<i32> {
    if bytes.is_empty() || (bytes.len() > 1 && bytes[0] == b'0') || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = std::str::from_utf8(bytes).ok()?.parse::<i32>().ok()?;
    (value > 0).then_some(value)
}

fn controlled_directory_witness(file: &File, path: &Path) -> Result<InodeWitness, CaptureError> {
    let witness = InodeWitness::read(file, path)?;
    if witness.kind() != nix::libc::S_IFDIR
        || witness.owner != effective_uid()
        || witness.mode & (0o7000 | 0o022) != 0
        || witness.mode & 0o700 != 0o700
    {
        return Err(CaptureError::UnsafeDirectory { path: path.to_owned() });
    }
    require_no_access_acl(file, path).map_err(|source| CaptureError::Io {
        operation: "reject access ACL on activation-namespace directory",
        path: path.to_owned(),
        source,
    })?;
    require_no_default_acl(file, path).map_err(|source| CaptureError::Io {
        operation: "reject default ACL on activation-namespace directory",
        path: path.to_owned(),
        source,
    })?;
    Ok(witness)
}

fn safe_usr_witness(store: &TreeMarkerStore, path: &Path, budget: &mut Budget) -> Result<InodeWitness, CaptureError> {
    let file = store.retained_directory();
    budget.operation(path)?;
    let witness = InodeWitness::read(file, path)?;
    if witness.kind() != nix::libc::S_IFDIR
        || witness.owner != effective_uid()
        || witness.mode & (0o7000 | 0o022) != 0
        || witness.mode & 0o500 != 0o500
    {
        return Err(CaptureError::UnsafeDirectory { path: path.to_owned() });
    }
    budget.operation(path)?;
    require_no_access_acl_until(file, path, budget.deadline()).map_err(|source| CaptureError::Io {
        operation: "reject access ACL on retained /usr tree",
        path: path.to_owned(),
        source,
    })?;
    budget.operation(path)?;
    require_no_default_acl_until(file, path, budget.deadline()).map_err(|source| CaptureError::Io {
        operation: "reject default ACL on retained /usr tree",
        path: path.to_owned(),
        source,
    })?;
    budget.operation(path)?;
    require_no_xattrs_until(file, path, budget.deadline()).map_err(|source| CaptureError::Io {
        operation: "reject extended attributes on retained /usr tree",
        path: path.to_owned(),
        source,
    })?;
    budget.operation(path)?;
    if InodeWitness::read(file, path)? != witness {
        return Err(CaptureError::InodeChanged { path: path.to_owned() });
    }
    Ok(witness)
}

fn open_directory(parent: &File, name: &CStr, path: &Path, budget: &mut Budget) -> Result<File, CaptureError> {
    budget.operation(path)?;
    let pinned = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| CaptureError::Io {
        operation: "pin activation-namespace directory",
        path: path.to_owned(),
        source,
    })?;
    let expected = InodeWitness::read(&pinned, path)?;
    budget.operation(path)?;
    let readable = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| CaptureError::Io {
        operation: "open activation-namespace directory",
        path: path.to_owned(),
        source,
    })?;
    if InodeWitness::read(&readable, path)? != expected {
        return Err(CaptureError::InodeChanged { path: path.to_owned() });
    }
    Ok(readable)
}

fn open_optional_directory(
    parent: &File,
    name: &CStr,
    path: &Path,
    budget: &mut Budget,
) -> Result<Option<File>, CaptureError> {
    budget.operation(path)?;
    match openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(CaptureError::Io {
            operation: "open optional activation tree",
            path: path.to_owned(),
            source,
        }),
    }
}

fn open_file(parent: &File, name: &CStr, path: &Path, budget: &mut Budget) -> Result<File, CaptureError> {
    budget.operation(path)?;
    openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| CaptureError::Io {
        operation: "open activation-namespace file",
        path: path.to_owned(),
        source,
    })
}

fn open_optional_path(
    parent: &File,
    name: &CStr,
    path: &Path,
    budget: &mut Budget,
) -> Result<Option<File>, CaptureError> {
    budget.operation(path)?;
    match openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(CaptureError::Io {
            operation: "pin optional activation-namespace entry",
            path: path.to_owned(),
            source,
        }),
    }
}

fn name_exists(parent: &File, name: &CStr, path: &Path, budget: &mut Budget) -> Result<bool, CaptureError> {
    open_optional_path(parent, name, path, budget).map(|file| file.is_some())
}

fn directory_names(
    directory: &File,
    path: &Path,
    local_limit: usize,
    budget: &mut Budget,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    budget.operation(path)?;
    let cursor = openat2_file(
        directory.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| CaptureError::Io {
        operation: "open activation-namespace directory cursor",
        path: path.to_owned(),
        source,
    })?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe { nix::libc::close(descriptor) };
        return Err(CaptureError::Io {
            operation: "enumerate activation-namespace directory",
            path: path.to_owned(),
            source,
        });
    };
    let mut stream = DirectoryStream(Some(stream));
    let mut names = Vec::new();
    loop {
        budget.operation(path)?;
        // SAFETY: errno is thread-local and stream remains live.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream is live and exclusively borrowed.
        let entry = unsafe { nix::libc::readdir(stream.pointer().as_ptr()) };
        if entry.is_null() {
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            if errno == nix::libc::EINTR {
                continue;
            }
            return Err(CaptureError::Io {
                operation: "enumerate activation-namespace directory",
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: d_name is NUL terminated for this live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if names.len() >= local_limit {
            return Err(CaptureError::EntryLimit {
                path: path.to_owned(),
                limit: local_limit,
            });
        }
        budget.name(name.len(), path)?;
        names.push(name.to_vec());
    }
    stream.close().map_err(|source| CaptureError::Io {
        operation: "close activation-namespace directory cursor",
        path: path.to_owned(),
        source,
    })?;
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CaptureError::DuplicateDirectoryName { path: path.to_owned() });
    }
    Ok(names)
}

fn read_link(file: &File, path: &Path, budget: &mut Budget) -> Result<Vec<u8>, CaptureError> {
    budget.operation(path)?;
    let mut bytes = vec![0_u8; nix::libc::PATH_MAX as usize + 1];
    // SAFETY: file is a pinned symlink and the output buffer is writable.
    let read = unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if read < 0 {
        return Err(CaptureError::Io {
            operation: "read retained root-ABI symlink",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| CaptureError::RootAbiTargetTooLong { path: path.to_owned() })?;
    if read == bytes.len() {
        return Err(CaptureError::RootAbiTargetTooLong { path: path.to_owned() });
    }
    bytes.truncate(read);
    Ok(bytes)
}

fn read_exact_at(file: &File, bytes: &mut [u8], path: &Path) -> Result<(), CaptureError> {
    let mut offset = 0;
    let mut attempts = 0;
    while offset < bytes.len() {
        attempts += 1;
        if attempts > 64 {
            return Err(CaptureError::Io {
                operation: "read bounded state ID",
                path: path.to_owned(),
                source: io::Error::other("state-ID read retry bound exceeded"),
            });
        }
        match file.read_at(&mut bytes[offset..], offset as u64) {
            Ok(0) => {
                return Err(CaptureError::Io {
                    operation: "read complete state ID",
                    path: path.to_owned(),
                    source: io::ErrorKind::UnexpectedEof.into(),
                });
            }
            Ok(read) => offset += read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(CaptureError::Io {
                    operation: "read state ID",
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    Ok(())
}

fn cstring(bytes: &[u8]) -> Result<CString, CaptureError> {
    CString::new(bytes).map_err(|_| CaptureError::NameContainsNul)
}

fn os(bytes: &[u8]) -> &std::ffi::OsStr {
    std::ffi::OsStr::from_bytes(bytes)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}

#[derive(Debug)]
struct Budget {
    entries: usize,
    name_bytes: usize,
    operations: usize,
    deadline: Instant,
}

impl Budget {
    fn new() -> Result<Self, CaptureError> {
        Ok(Self {
            entries: 0,
            name_bytes: 0,
            operations: 0,
            deadline: Instant::now()
                .checked_add(INVENTORY_DEADLINE)
                .ok_or(CaptureError::Deadline)?,
        })
    }

    fn operation(&mut self, path: &Path) -> Result<(), CaptureError> {
        if Instant::now() >= self.deadline {
            return Err(CaptureError::Deadline);
        }
        if self.operations >= MAX_OPERATIONS {
            return Err(CaptureError::OperationLimit {
                path: path.to_owned(),
                limit: MAX_OPERATIONS,
            });
        }
        self.operations += 1;
        Ok(())
    }

    fn deadline(&self) -> Instant {
        self.deadline
    }

    fn name(&mut self, bytes: usize, path: &Path) -> Result<(), CaptureError> {
        if self.entries >= MAX_NAMESPACE_ENTRIES {
            return Err(CaptureError::EntryLimit {
                path: path.to_owned(),
                limit: MAX_NAMESPACE_ENTRIES,
            });
        }
        self.entries += 1;
        self.name_bytes = self.name_bytes.checked_add(bytes).ok_or(CaptureError::NameByteLimit {
            path: path.to_owned(),
            limit: MAX_NAME_BYTES,
        })?;
        if self.name_bytes > MAX_NAME_BYTES {
            return Err(CaptureError::NameByteLimit {
                path: path.to_owned(),
                limit: MAX_NAME_BYTES,
            });
        }
        Ok(())
    }
}

struct DirectoryStream(Option<NonNull<nix::libc::DIR>>);

impl DirectoryStream {
    fn pointer(&self) -> NonNull<nix::libc::DIR> {
        self.0.expect("live directory stream")
    }

    fn close(&mut self) -> io::Result<()> {
        let stream = self.0.take().expect("live directory stream");
        // SAFETY: wrapper uniquely owns the stream.
        if unsafe { nix::libc::closedir(stream.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        if let Some(stream) = self.0.take() {
            // SAFETY: wrapper uniquely owns the stream.
            unsafe { nix::libc::closedir(stream.as_ptr()) };
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureError {
    #[error("revalidate mutable installation namespace during activation inventory")]
    Installation(#[source] crate::installation::Error),
    #[error("capture activation inventory runtime epoch")]
    RuntimeEpoch(#[source] RuntimeEvidenceError),
    #[error("runtime epoch changed during one activation inventory")]
    RuntimeEpochChanged,
    #[error("capture runtime identity for activation tree `{}`", path.display())]
    RuntimeTree {
        path: PathBuf,
        #[source]
        source: RuntimeEvidenceError,
    },
    #[error(transparent)]
    TreeMarker(#[from] TreeMarkerError),
    #[error("{operation} at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("activation inventory deadline expired")]
    Deadline,
    #[error("activation inventory exceeded {limit} operations at `{}`", path.display())]
    OperationLimit { path: PathBuf, limit: usize },
    #[error("activation inventory exceeded {limit} entries at `{}`", path.display())]
    EntryLimit { path: PathBuf, limit: usize },
    #[error("activation inventory exceeded {limit} raw name bytes at `{}`", path.display())]
    NameByteLimit { path: PathBuf, limit: usize },
    #[error("activation namespace contains a NUL name")]
    NameContainsNul,
    #[error("activation directory `{}` returned a duplicate raw name", path.display())]
    DuplicateDirectoryName { path: PathBuf },
    #[error("unsafe activation-namespace directory `{}`", path.display())]
    UnsafeDirectory { path: PathBuf },
    #[error("activation-namespace inode changed at `{}`", path.display())]
    InodeChanged { path: PathBuf },
    #[error("activation directory contents changed at `{}`", path.display())]
    DirectoryContentsChanged { path: PathBuf },
    #[error("fixed activation wrapper `{name}` is missing")]
    FixedWrapperMissing { name: &'static str },
    #[error("unexpected raw name in `.cast/root`: {name:?}")]
    UnexpectedRootName { name: Vec<u8> },
    #[error("unexpected raw name in `.cast/quarantine`: {name:?}")]
    UnexpectedQuarantineName { name: Vec<u8> },
    #[error("unexpected entry {name:?} in activation wrapper `{}`", wrapper.display())]
    UnexpectedWrapperEntry { wrapper: PathBuf, name: Vec<u8> },
    #[error("unexpected raw name in `.cast/root/isolation`: {name:?}")]
    UnexpectedIsolationEntry { name: Vec<u8> },
    #[error("required activation tree is missing from {location:?}")]
    RequiredTreeMissing { location: TreeLocation },
    #[error("invalid state-slot marker name at `{}`", path.display())]
    InvalidSlotName { path: PathBuf },
    #[error("unsafe state-slot marker inode at `{}`", path.display())]
    UnsafeSlotLink { path: PathBuf },
    #[error("duplicate state-slot links in `{}`", path.display())]
    DuplicateSlotLink { path: PathBuf },
    #[error("state-slot marker is in the wrong wrapper `{}`", path.display())]
    SlotWrongWrapper { path: PathBuf },
    #[error("parking wrapper unexpectedly contains `/usr`: `{}`", path.display())]
    ParkingWrapperContainsTree { path: PathBuf },
    #[error("state-slot token differs from its wrapper tree at `{}`", path.display())]
    SlotTokenMismatch { path: PathBuf },
    #[error("state wrapper `{}` does not carry canonical state ID {expected}", path.display())]
    StateWrapperMismatch { path: PathBuf, expected: i32 },
    #[error("tree token {token} requires exactly one authenticated slot link, found {actual}")]
    SlotAuthorizationCount { token: String, actual: usize },
    #[error("state-slot marker for tree token {token} has state {actual}, expected {expected:?}")]
    SlotWrongTransitionState {
        token: String,
        actual: i32,
        expected: Option<i32>,
    },
    #[error("orphan state-slot marker for tree token {token}")]
    OrphanSlotLink { token: String },
    #[error("tree token {token} occurs at {count} `/usr` locations")]
    DuplicateTreeToken { token: String, count: usize },
    #[error("temporary root-ABI name remains at `{}`", path.display())]
    RootAbiTemporary { path: PathBuf },
    #[error("root-ABI entry is not a symlink at `{}`", path.display())]
    RootAbiType { path: PathBuf },
    #[error("root-ABI target mismatch at `{}`", path.display())]
    RootAbiTarget {
        path: PathBuf,
        expected: Vec<u8>,
        actual: Vec<u8>,
    },
    #[error("root-ABI target exceeds the bounded read at `{}`", path.display())]
    RootAbiTargetTooLong { path: PathBuf },
}
