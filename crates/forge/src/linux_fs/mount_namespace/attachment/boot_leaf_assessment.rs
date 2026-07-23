//! Descriptor-rooted, read-only classification of one receipt-identified boot leaf.
//!
//! The caller supplies scalar expectations, but only this module can construct
//! terminal evidence. Every lookup is one canonical component below a retained
//! boot-publication parent. Full parent-chain revalidation brackets the leaf
//! observation, its named-inode reconciliation, and a second terminal rebind.
//!
//! `Absent` is admitted only from `ENOENT` for that exact leaf. `Exact` requires
//! one stable regular, single-link, mode-0644 inode on the retained attachment
//! whose complete XXH3 and SHA-256 identities match. A stable regular,
//! single-link mode, length, or content mismatch is `Different`. Symlinks,
//! nonregular nodes, hard links, attachment crossings, identity changes, and
//! I/O failures are errors rather than mutation-enabling classifications.

use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    time::Instant,
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::Xxh3;

use super::{
    boot_file_publication::{AttachmentIdentity, RetainedBootFilePublicationTarget},
    boot_publication_parent::RetainedBootPublicationParent,
};
use crate::linux_fs::{
    controlled_resolution, descriptor_mount_id_until, is_retained_boot_file_private_component,
    openat2_file_until,
};

#[path = "boot_leaf_assessment/effect.rs"]
mod effect;
#[path = "boot_leaf_assessment/error.rs"]
mod error;
#[path = "boot_leaf_assessment/model.rs"]
mod model;
#[path = "boot_leaf_assessment/parent_walk.rs"]
mod parent_walk;

#[cfg(test)]
pub(crate) use effect::{
    FixtureRetainedBootLeafAssessmentHookGuard,
    arm_retained_boot_leaf_assessment_terminal_rebind_hook,
};
pub(crate) use error::RetainedBootLeafAssessmentError;
pub(crate) use model::{
    RetainedBootLeafAssessmentLimits, RetainedBootLeafAssessmentRequest,
    RetainedBootLeafAssessmentState, ValidatedRetainedBootLeafAssessment,
};

const READ_BUFFER_BYTES: usize = 4 * 1024;
const REQUIRED_MODE: u32 = 0o644;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LeafSnapshot {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PresentLeaf {
    state: RetainedBootLeafAssessmentState,
    snapshot: LeafSnapshot,
    mount_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservedLeaf {
    Absent,
    Present(PresentLeaf),
}

pub(super) struct AssessmentBinding {
    root: AttachmentIdentity,
    parent_components: Box<[Box<str>]>,
}

impl RetainedBootPublicationParent<'_, '_> {
    /// Assess one exact receipt-derived leaf without exposing the retained
    /// descriptor or granting any mutation authority.
    pub(crate) fn assess_boot_leaf_until(
        &self,
        request: RetainedBootLeafAssessmentRequest<'_>,
        limits: RetainedBootLeafAssessmentLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootLeafAssessment, RetainedBootLeafAssessmentError> {
        assess_from_retained_parent_until(
            self,
            request,
            limits,
            AssessmentBinding {
                root: self.publication_parent_identity(),
                parent_components: Vec::new().into_boxed_slice(),
            },
            deadline,
        )
    }
}

pub(super) fn assess_from_retained_parent_until(
    target: &impl RetainedBootFilePublicationTarget,
    request: RetainedBootLeafAssessmentRequest<'_>,
    limits: RetainedBootLeafAssessmentLimits,
    binding: AssessmentBinding,
    deadline: Instant,
) -> Result<ValidatedRetainedBootLeafAssessment, RetainedBootLeafAssessmentError> {
    let (canonical_name, canonical_leaf) = validate_request(request, limits, deadline)?;
    let expected_parent = target.publication_parent_identity();
    require_parent(target, "opening boot-leaf assessment", deadline)?;
    let parent = open_parent_alias(target.publication_parent(), expected_parent, deadline)?;
    require_parent_alias(&parent, expected_parent, "binding the opening parent alias", deadline)?;

    let observed = observe_leaf(&parent, &canonical_name, request, limits, expected_parent, deadline)?;

    require_parent(target, "sandwiching boot-leaf assessment", deadline)?;
    require_parent_alias(&parent, expected_parent, "sandwiching the retained parent alias", deadline)?;
    effect::before_terminal_rebind();
    require_observation_current(&parent, &canonical_name, observed, expected_parent, deadline)?;

    require_parent(target, "closing boot-leaf assessment", deadline)?;
    require_parent_alias(&parent, expected_parent, "closing the retained parent alias", deadline)?;
    require_observation_current(&parent, &canonical_name, observed, expected_parent, deadline)?;
    checkpoint(deadline)?;

    let (state, exact_file_identity) = match observed {
        ObservedLeaf::Absent => (RetainedBootLeafAssessmentState::Absent, None),
        ObservedLeaf::Present(found) => {
            let exact = (found.state == RetainedBootLeafAssessmentState::Exact).then_some((
                found.snapshot.device,
                found.snapshot.inode,
                found.mount_id,
            ));
            (found.state, exact)
        }
    };
    Ok(ValidatedRetainedBootLeafAssessment::new(
        state,
        (binding.root.device, binding.root.inode, binding.root.mount_id),
        binding.parent_components,
        Some((expected_parent.device, expected_parent.inode, expected_parent.mount_id)),
        canonical_leaf,
        request.expected_length(),
        request.expected_xxh3(),
        request.expected_sha256(),
        exact_file_identity,
    ))
}

fn validate_request(
    request: RetainedBootLeafAssessmentRequest<'_>,
    limits: RetainedBootLeafAssessmentLimits,
    deadline: Instant,
) -> Result<(CString, Box<str>), RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    let bytes = request.canonical_leaf().as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || matches!(bytes, b"." | b"..")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(RetainedBootLeafAssessmentError::InvalidCanonicalLeaf);
    }
    if is_retained_boot_file_private_component(request.canonical_leaf()) {
        return Err(RetainedBootLeafAssessmentError::ReservedPrivateLeaf);
    }
    if limits.max_read_bytes == 0 || limits.max_read_bytes > model::HARD_MAX_ASSESSMENT_BYTES {
        return Err(RetainedBootLeafAssessmentError::InvalidLimit { field: "read bytes" });
    }
    if limits.max_read_calls == 0 || limits.max_read_calls > model::HARD_MAX_ASSESSMENT_READ_CALLS {
        return Err(RetainedBootLeafAssessmentError::InvalidLimit { field: "read calls" });
    }
    if request.expected_length() > limits.max_read_bytes {
        return Err(RetainedBootLeafAssessmentError::LengthLimitExceeded {
            length: request.expected_length(),
            limit: limits.max_read_bytes,
        });
    }
    let required_calls = required_read_calls(request.expected_length())?;
    if required_calls > limits.max_read_calls {
        return Err(RetainedBootLeafAssessmentError::ReadCallLimitExceeded {
            required: required_calls,
            limit: limits.max_read_calls,
        });
    }
    let canonical_name = CString::new(bytes).map_err(|_| RetainedBootLeafAssessmentError::InvalidCanonicalLeaf)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|source| RetainedBootLeafAssessmentError::Allocation { source })?;
    owned.push_str(request.canonical_leaf());
    Ok((canonical_name, owned.into_boxed_str()))
}

fn required_read_calls(length: u64) -> Result<usize, RetainedBootLeafAssessmentError> {
    let data_calls = length
        .checked_add(READ_BUFFER_BYTES as u64 - 1)
        .ok_or(RetainedBootLeafAssessmentError::InvalidLimit { field: "read calls" })?
        / READ_BUFFER_BYTES as u64;
    usize::try_from(data_calls)
        .ok()
        .and_then(|calls| calls.checked_add(1))
        .ok_or(RetainedBootLeafAssessmentError::InvalidLimit { field: "read calls" })
}

fn open_parent_alias(
    retained_parent: &File,
    expected: AttachmentIdentity,
    deadline: Instant,
) -> Result<File, RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    let parent = openat2_file_until(
        retained_parent.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NOATIME
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| RetainedBootLeafAssessmentError::Filesystem {
        action: "opening a readable alias of the retained boot-leaf parent",
        source,
    })?;
    require_parent_alias(&parent, expected, "binding the readable retained parent", deadline)?;
    Ok(parent)
}

fn require_parent_alias(
    parent: &File,
    expected: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    let metadata = parent
        .metadata()
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    let mount_id = descriptor_mount_id_until(parent, deadline)
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || mount_id != expected.mount_id
    {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged { action });
    }
    Ok(())
}

fn observe_leaf(
    parent: &File,
    canonical_name: &CStr,
    request: RetainedBootLeafAssessmentRequest<'_>,
    limits: RetainedBootLeafAssessmentLimits,
    expected_parent: AttachmentIdentity,
    deadline: Instant,
) -> Result<ObservedLeaf, RetainedBootLeafAssessmentError> {
    let path = match open_leaf_path(parent, canonical_name, deadline) {
        Ok(path) => path,
        Err(OpenLeafError::Absent) => return Ok(ObservedLeaf::Absent),
        Err(OpenLeafError::Assessment(source)) => return Err(source),
    };
    let opening = require_regular_snapshot(&path, expected_parent, "opening the canonical boot leaf", deadline)?;
    let opening_mount_id = require_leaf_mount(&path, expected_parent, "binding the canonical boot leaf", deadline)?;

    if opening.mode != REQUIRED_MODE || opening.length != request.expected_length() {
        let closing = require_regular_snapshot(&path, expected_parent, "closing mismatched boot-leaf metadata", deadline)?;
        require_same_snapshot(opening, closing, "sandwiching mismatched boot-leaf metadata")?;
        return Ok(ObservedLeaf::Present(PresentLeaf {
            state: RetainedBootLeafAssessmentState::Different,
            snapshot: opening,
            mount_id: opening_mount_id,
        }));
    }

    let readable = open_readable_leaf(parent, canonical_name, deadline)?;
    let readable_opening = require_regular_snapshot(
        &readable,
        expected_parent,
        "binding the readable canonical boot leaf",
        deadline,
    )?;
    let readable_mount_id = require_leaf_mount(
        &readable,
        expected_parent,
        "binding the readable canonical boot-leaf attachment",
        deadline,
    )?;
    require_same_snapshot(opening, readable_opening, "rebinding the canonical boot leaf for reading")?;
    if readable_mount_id != opening_mount_id {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged {
            action: "rebinding the canonical boot-leaf mount",
        });
    }

    let (actual_xxh3, actual_sha256) = read_complete_identity(&readable, request, limits, deadline)?;
    let readable_closing = require_regular_snapshot(
        &readable,
        expected_parent,
        "closing the readable canonical boot leaf",
        deadline,
    )?;
    let path_closing = require_regular_snapshot(
        &path,
        expected_parent,
        "closing the retained canonical boot leaf",
        deadline,
    )?;
    require_same_snapshot(opening, readable_closing, "sandwiching canonical boot-leaf content")?;
    require_same_snapshot(opening, path_closing, "sandwiching the retained canonical boot leaf")?;

    let state = if actual_xxh3 == request.expected_xxh3() && actual_sha256 == request.expected_sha256() {
        RetainedBootLeafAssessmentState::Exact
    } else {
        RetainedBootLeafAssessmentState::Different
    };
    Ok(ObservedLeaf::Present(PresentLeaf {
        state,
        snapshot: opening,
        mount_id: opening_mount_id,
    }))
}

enum OpenLeafError {
    Absent,
    Assessment(RetainedBootLeafAssessmentError),
}

fn open_leaf_path(parent: &File, name: &CStr, deadline: Instant) -> Result<File, OpenLeafError> {
    checkpoint(deadline).map_err(OpenLeafError::Assessment)?;
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| match source.raw_os_error() {
        Some(nix::libc::ENOENT) => OpenLeafError::Absent,
        Some(nix::libc::ELOOP) => OpenLeafError::Assessment(RetainedBootLeafAssessmentError::UnsafeLeafType),
        _ => OpenLeafError::Assessment(RetainedBootLeafAssessmentError::Filesystem {
            action: "opening the canonical boot leaf below its retained parent",
            source,
        }),
    })
}

fn open_readable_leaf(
    parent: &File,
    name: &CStr,
    deadline: Instant,
) -> Result<File, RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NOATIME
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| match source.raw_os_error() {
        Some(nix::libc::ENOENT) => RetainedBootLeafAssessmentError::LeafIdentityChanged {
            action: "opening the readable canonical boot leaf",
        },
        Some(nix::libc::ELOOP) => RetainedBootLeafAssessmentError::UnsafeLeafType,
        _ => RetainedBootLeafAssessmentError::Filesystem {
            action: "opening the readable canonical boot leaf",
            source,
        },
    })
}

fn require_regular_snapshot(
    file: &File,
    expected_parent: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<LeafSnapshot, RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    let metadata = file
        .metadata()
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    if !metadata.file_type().is_file() {
        return Err(RetainedBootLeafAssessmentError::UnsafeLeafType);
    }
    if metadata.nlink() != 1 {
        return Err(RetainedBootLeafAssessmentError::UnsafeLinkCount {
            found: metadata.nlink(),
        });
    }
    if metadata.dev() != expected_parent.device || metadata.ino() == 0 {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged { action });
    }
    Ok(snapshot(&metadata))
}

fn require_leaf_mount(
    file: &File,
    expected_parent: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<u64, RetainedBootLeafAssessmentError> {
    let mount_id = descriptor_mount_id_until(file, deadline)
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    if mount_id != expected_parent.mount_id {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged { action });
    }
    Ok(mount_id)
}

fn snapshot(metadata: &Metadata) -> LeafSnapshot {
    LeafSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn require_same_snapshot(
    expected: LeafSnapshot,
    found: LeafSnapshot,
    action: &'static str,
) -> Result<(), RetainedBootLeafAssessmentError> {
    if found == expected {
        Ok(())
    } else {
        Err(RetainedBootLeafAssessmentError::LeafIdentityChanged { action })
    }
}

fn read_complete_identity(
    file: &File,
    request: RetainedBootLeafAssessmentRequest<'_>,
    limits: RetainedBootLeafAssessmentLimits,
    deadline: Instant,
) -> Result<(u128, [u8; 32]), RetainedBootLeafAssessmentError> {
    let mut xxh3 = Xxh3::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    let mut offset = 0u64;
    let mut read_calls = 0usize;
    let mut read_bytes = 0u64;

    while offset < request.expected_length() {
        checkpoint(deadline)?;
        let offered = usize::try_from((request.expected_length() - offset).min(READ_BUFFER_BYTES as u64))
            .expect("fixed boot-leaf read buffer fits usize");
        read_calls = read_calls.checked_add(1).ok_or(RetainedBootLeafAssessmentError::InvalidLimit {
            field: "read calls",
        })?;
        if read_calls > limits.max_read_calls {
            return Err(RetainedBootLeafAssessmentError::ReadCallLimitExceeded {
                required: read_calls,
                limit: limits.max_read_calls,
            });
        }
        let found = pread_once(file, offset, &mut buffer[..offered]).map_err(|source| {
            RetainedBootLeafAssessmentError::Filesystem {
                action: "reading canonical boot-leaf content",
                source,
            }
        })?;
        if found == 0 || found > offered {
            return Err(RetainedBootLeafAssessmentError::LeafIdentityChanged {
                action: "reading the declared canonical boot-leaf length",
            });
        }
        read_bytes = read_bytes
            .checked_add(found as u64)
            .ok_or(RetainedBootLeafAssessmentError::InvalidLimit { field: "read bytes" })?;
        if read_bytes > limits.max_read_bytes {
            return Err(RetainedBootLeafAssessmentError::LengthLimitExceeded {
                length: read_bytes,
                limit: limits.max_read_bytes,
            });
        }
        xxh3.update(&buffer[..found]);
        sha256.update(&buffer[..found]);
        offset = offset
            .checked_add(found as u64)
            .ok_or(RetainedBootLeafAssessmentError::InvalidLimit { field: "read bytes" })?;
    }

    read_calls = read_calls.checked_add(1).ok_or(RetainedBootLeafAssessmentError::InvalidLimit {
        field: "read calls",
    })?;
    if read_calls > limits.max_read_calls {
        return Err(RetainedBootLeafAssessmentError::ReadCallLimitExceeded {
            required: read_calls,
            limit: limits.max_read_calls,
        });
    }
    let mut terminal = [0u8; 1];
    if pread_once(file, request.expected_length(), &mut terminal).map_err(|source| {
        RetainedBootLeafAssessmentError::Filesystem {
            action: "probing the terminal canonical boot-leaf length",
            source,
        }
    })? != 0
    {
        return Err(RetainedBootLeafAssessmentError::LeafIdentityChanged {
            action: "probing the terminal canonical boot-leaf length",
        });
    }
    checkpoint(deadline)?;
    Ok((xxh3.digest128(), sha256.finalize().into()))
}

fn require_observation_current(
    parent: &File,
    name: &CStr,
    expected: ObservedLeaf,
    expected_parent: AttachmentIdentity,
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    let rebound = match open_leaf_path(parent, name, deadline) {
        Ok(file) => Some(file),
        Err(OpenLeafError::Absent) => None,
        Err(OpenLeafError::Assessment(source)) => return Err(source),
    };
    match (expected, rebound) {
        (ObservedLeaf::Absent, None) => Ok(()),
        (ObservedLeaf::Absent, Some(_)) | (ObservedLeaf::Present(_), None) => {
            Err(RetainedBootLeafAssessmentError::LeafIdentityChanged {
                action: "rebinding the assessed canonical boot-leaf name",
            })
        }
        (ObservedLeaf::Present(expected), Some(file)) => {
            let found = require_regular_snapshot(
                &file,
                expected_parent,
                "rebinding the assessed canonical boot leaf",
                deadline,
            )?;
            let mount_id = require_leaf_mount(
                &file,
                expected_parent,
                "rebinding the assessed canonical boot-leaf mount",
                deadline,
            )?;
            require_same_snapshot(expected.snapshot, found, "rebinding the assessed canonical boot leaf")?;
            if mount_id != expected.mount_id {
                return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged {
                    action: "rebinding the assessed canonical boot-leaf mount",
                });
            }
            Ok(())
        }
    }
}

fn require_parent(
    target: &impl RetainedBootFilePublicationTarget,
    action: &'static str,
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    target
        .require_publication_parent_until(action, deadline)
        .map_err(|source| RetainedBootLeafAssessmentError::ParentRevalidation { action, source })
}

fn pread_once(file: &File, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
    let offset = nix::libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "boot-leaf read offset exceeds off_t"))?;
    // SAFETY: the output buffer and retained descriptor remain live for this
    // positional read. The kernel retains neither argument.
    let found = unsafe { nix::libc::pread(file.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), offset) };
    if found < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(found).map_err(|_| io::Error::other("pread returned an oversized byte count"))
    }
}

fn checkpoint(deadline: Instant) -> Result<(), RetainedBootLeafAssessmentError> {
    if Instant::now() > deadline {
        Err(RetainedBootLeafAssessmentError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
