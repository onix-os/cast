// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Deterministic credential mapping for isolated build containers.
//!
//! A payload starts with namespace UID/GID zero and an empty supplementary
//! group list.  Linux requires `setgroups(2)` to remain enabled until the
//! child has dropped its inherited groups, so an unprivileged parent uses one
//! delegated subordinate GID solely to authorize that transition.  The
//! subordinate host ID is never exposed to the payload: it is always mapped
//! to the fixed namespace-only auxiliary GID below.

use std::{
    ffi::CString,
    io::{self, Read, Seek, SeekFrom},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs_err::{self as fs, os::unix::fs::OpenOptionsExt};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, SealFlag, fcntl},
    sys::memfd::{MemFdCreateFlag, memfd_create},
    unistd::{Pid, Uid, User},
};
use snafu::{ResultExt, Snafu, ensure};

/// A fixed, non-payload namespace GID used to keep `setgroups` enabled while
/// the child clears inherited supplementary groups.
const AUXILIARY_GID: u32 = u32::MAX - 1;
const NEWGIDMAP: &str = "/usr/bin/newgidmap";

// `/etc/subgid` is a small line-oriented policy file. These ceilings are far
// above a normal installation while keeping a substituted or corrupted file
// from becoming an unbounded allocation or parser walk.
const MAX_SUBGID_BYTES: usize = 1024 * 1024;
const MAX_SUBGID_LINES: usize = 65_536;
const MAX_SUBGID_LINE_BYTES: usize = 4096;

const NEWGIDMAP_LIMITS: HelperLimits = HelperLimits {
    wall_time: Duration::from_secs(5),
    termination_time: Duration::from_secs(2),
    stderr_bytes: 16 * 1024,
};
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy)]
struct TextLimits {
    bytes: usize,
    lines: usize,
    line_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
struct HelperLimits {
    wall_time: Duration,
    termination_time: Duration,
    stderr_bytes: usize,
}

pub(super) fn validated_caller_identity() -> Result<crate::credentials::IdentityCredentials, Error> {
    let caller = crate::credentials::read_current_identity().map_err(|failure| match failure {
        crate::credentials::ReadIdentityFailure::ReadGroupCredentials(source) => {
            Error::ReadCallerGroupCredentials { source }
        }
        crate::credentials::ReadIdentityFailure::ReadUserCredentials(source) => {
            Error::ReadCallerUserCredentials { source }
        }
    })?;
    ensure_uniform_caller_credentials(&caller)?;
    Ok(caller)
}

pub(super) fn idmap(pid: Pid, caller: &crate::credentials::IdentityCredentials) -> Result<(), Error> {
    let (uid, gid) = ensure_uniform_caller_credentials(caller)?;
    let current = validated_caller_identity()?;
    ensure_unchanged_caller_credentials(caller, &current)?;

    let proc_dir = Path::new("/proc").join(pid.as_raw().to_string());

    fs::write(proc_dir.join("uid_map"), mapping(0, uid)).context(WriteUidMapSnafu)?;

    let privileged_auxiliary_gid = if gid == AUXILIARY_GID {
        AUXILIARY_GID - 1
    } else {
        AUXILIARY_GID
    };
    let direct_map = gid_mapping(gid, privileged_auxiliary_gid);
    match fs::write(proc_dir.join("gid_map"), &direct_map) {
        Ok(()) => {}
        Err(source) if is_permission_denied(&source) => {
            let subordinate_gid = delegated_gid(uid, gid)?;
            map_gid_with_helper(pid, gid, subordinate_gid)?;
        }
        Err(source) => return Err(Error::WriteGidMap { source }),
    }

    verify_map(&proc_dir.join("uid_map"), &[(0, uid, 1)], "UID").context(VerifyUidMapSnafu)?;
    let gid_map = read_map(&proc_dir.join("gid_map")).context(VerifyGidMapSnafu)?;
    ensure!(
        gid_map.len() == 2
            && gid_map[0] == (0, gid, 1)
            && gid_map[1].0 == AUXILIARY_GID
            && gid_map[1].2 == 1
            && gid_map[1].1 != gid,
        UnexpectedGidMapSnafu { found: gid_map }
    );

    let setgroups = fs::read_to_string(proc_dir.join("setgroups")).context(ReadSetgroupsSnafu)?;
    ensure!(setgroups.trim() == "allow", SetgroupsDisabledSnafu);
    Ok(())
}

fn ensure_uniform_caller_credentials(
    credentials: &crate::credentials::IdentityCredentials,
) -> Result<(u32, u32), Error> {
    let Some(ids) = credentials.uniform_ids() else {
        return MixedCallerCredentialsSnafu {
            real_uid: credentials.real_uid,
            effective_uid: credentials.effective_uid,
            saved_uid: credentials.saved_uid,
            filesystem_uid: credentials.filesystem_uid,
            real_gid: credentials.real_gid,
            effective_gid: credentials.effective_gid,
            saved_gid: credentials.saved_gid,
            filesystem_gid: credentials.filesystem_gid,
        }
        .fail();
    };
    Ok(ids)
}

fn ensure_unchanged_caller_credentials(
    before_clone: &crate::credentials::IdentityCredentials,
    before_mapping: &crate::credentials::IdentityCredentials,
) -> Result<(), Error> {
    ensure!(
        before_clone == before_mapping,
        CallerCredentialsChangedSnafu {
            before_clone: before_clone.to_string(),
            before_mapping: before_mapping.to_string(),
        }
    );
    Ok(())
}

fn mapping(namespace_id: u32, host_id: u32) -> String {
    format!("{namespace_id} {host_id} 1\n")
}

fn gid_mapping(primary_gid: u32, auxiliary_host_gid: u32) -> String {
    format!(
        "{}{}",
        mapping(0, primary_gid),
        mapping(AUXILIARY_GID, auxiliary_host_gid)
    )
}

fn is_permission_denied(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(Errno::EPERM as i32)
}

fn delegated_gid(uid: u32, primary_gid: u32) -> Result<u32, Error> {
    let user = User::from_uid(Uid::from_raw(uid))
        .context(GetUserByUidSnafu)?
        .ok_or(Error::MissingUser { uid })?;
    let content = read_bounded_regular_text(
        Path::new("/etc/subgid"),
        TextLimits {
            bytes: MAX_SUBGID_BYTES,
            lines: MAX_SUBGID_LINES,
            line_bytes: MAX_SUBGID_LINE_BYTES,
        },
    )
    .context(ReadSubgidSnafu)?;
    select_delegated_gid(&content, uid, &user.name, primary_gid).ok_or(Error::MissingSubgid {
        uid,
        username: user.name,
    })
}

/// Open and read a line-oriented policy file without following a final
/// symlink, blocking on a special file, or allocating beyond its fixed limit.
/// Metadata is checked before any content is consumed and the extra-byte read
/// closes the usual size-check/read race for a concurrently growing file.
fn read_bounded_regular_text(path: &Path, limits: TextLimits) -> Result<String, io::Error> {
    let read_limit = limits
        .bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "text byte limit is too large"))?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if metadata.len() > limits.bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, exceeding the {}-byte limit",
                path.display(),
                metadata.len(),
                limits.bytes
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref().take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limits.bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} grew beyond the {}-byte limit while it was read",
                path.display(),
                limits.bytes
            ),
        ));
    }
    let after = file.metadata()?;
    let unchanged = metadata.dev() == after.dev()
        && metadata.ino() == after.ino()
        && metadata.mode() == after.mode()
        && metadata.nlink() == after.nlink()
        && metadata.uid() == after.uid()
        && metadata.gid() == after.gid()
        && metadata.len() == after.len()
        && metadata.mtime() == after.mtime()
        && metadata.mtime_nsec() == after.mtime_nsec()
        && metadata.ctime() == after.ctime()
        && metadata.ctime_nsec() == after.ctime_nsec();
    if !unchanged || bytes.len() as u64 != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} changed while it was read", path.display()),
        ));
    }

    validate_text_shape(path, &bytes, limits)?;
    String::from_utf8(bytes).map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not UTF-8: {source}", path.display()),
        )
    })
}

fn validate_text_shape(path: &Path, bytes: &[u8], limits: TextLimits) -> Result<(), io::Error> {
    let mut line_count = 0usize;
    let mut line_bytes = 0usize;
    for byte in bytes {
        if *byte == b'\n' {
            line_count = line_count.saturating_add(1);
            if line_bytes > limits.line_bytes {
                return Err(line_too_long(path, line_bytes, limits.line_bytes));
            }
            line_bytes = 0;
        } else {
            line_bytes = line_bytes.saturating_add(1);
            if line_bytes > limits.line_bytes {
                return Err(line_too_long(path, line_bytes, limits.line_bytes));
            }
        }
        if line_count > limits.lines {
            return Err(too_many_lines(path, line_count, limits.lines));
        }
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        line_count = line_count.saturating_add(1);
    }
    if line_count > limits.lines {
        return Err(too_many_lines(path, line_count, limits.lines));
    }
    Ok(())
}

fn line_too_long(path: &Path, observed: usize, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} contains a line of at least {observed} bytes, exceeding the {limit}-byte line limit",
            path.display()
        ),
    )
}

fn too_many_lines(path: &Path, observed: usize, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} contains at least {observed} lines, exceeding the {limit}-line limit",
            path.display()
        ),
    )
}

fn select_delegated_gid(content: &str, uid: u32, username: &str, primary_gid: u32) -> Option<u32> {
    content
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let owner = fields.next()?;
            let start = fields.next()?.parse::<u32>().ok()?;
            let count = fields.next()?.parse::<u32>().ok()?;
            if fields.next().is_some() || (owner != username && owner.parse::<u32>() != Ok(uid)) || count == 0 {
                return None;
            }
            let end = start.checked_add(count)?;
            (start..end).find(|candidate| *candidate != primary_gid)
        })
        .min()
}

fn map_gid_with_helper(pid: Pid, primary_gid: u32, subordinate_gid: u32) -> Result<(), Error> {
    let args = [
        pid.as_raw().to_string(),
        "0".to_owned(),
        primary_gid.to_string(),
        "1".to_owned(),
        AUXILIARY_GID.to_string(),
        subordinate_gid.to_string(),
        "1".to_owned(),
    ];
    let output = run_mapping_helper(Path::new(NEWGIDMAP), &args, NEWGIDMAP_LIMITS)?;
    ensure!(
        output.status.success(),
        NewgidmapFailedSnafu {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
    );
    Ok(())
}

#[derive(Debug)]
struct HelperOutput {
    status: ExitStatus,
    stderr: Vec<u8>,
}

fn run_mapping_helper<S>(program: &Path, args: &[S], limits: HelperLimits) -> Result<HelperOutput, Error>
where
    S: AsRef<std::ffi::OsStr>,
{
    let mut stderr = bounded_stderr_file(limits.stderr_bytes).context(PrepareNewgidmapStderrSnafu)?;
    let child_stderr = stderr.try_clone().context(PrepareNewgidmapStderrSnafu)?;
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(child_stderr.into_file()));
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            let core = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_CORE, &core) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().context(RunNewgidmapSnafu)?;
    // Linux PIDs are positive signed integers; `Child::id` uses `u32` only to
    // keep the cross-platform API uniform.
    let pid = i32::try_from(child.id()).expect("Linux child PID fits in i32");
    let started = Instant::now();
    let deadline = started.checked_add(limits.wall_time).unwrap_or(started);
    loop {
        let observed = match stderr.stream_position() {
            Ok(observed) => observed,
            Err(source) => {
                terminate_and_reap(&mut child, pid, limits.termination_time).context(TerminateNewgidmapSnafu)?;
                return Err(Error::ReadNewgidmapStderr { source });
            }
        };
        if observed > limits.stderr_bytes as u64 {
            terminate_and_reap(&mut child, pid, limits.termination_time).context(TerminateNewgidmapSnafu)?;
            return Err(Error::NewgidmapStderrTooLarge {
                observed,
                limit: limits.stderr_bytes,
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let observed = stderr.stream_position().context(ReadNewgidmapStderrSnafu)?;
                if observed > limits.stderr_bytes as u64 {
                    return Err(Error::NewgidmapStderrTooLarge {
                        observed,
                        limit: limits.stderr_bytes,
                    });
                }
                let stderr = read_helper_stderr(stderr, limits.stderr_bytes).context(ReadNewgidmapStderrSnafu)?;
                return Ok(HelperOutput { status, stderr });
            }
            Ok(None) => {}
            Err(source) => {
                terminate_and_reap(&mut child, pid, limits.termination_time).context(TerminateNewgidmapSnafu)?;
                return Err(Error::WaitNewgidmap { source });
            }
        }
        if Instant::now() >= deadline {
            terminate_and_reap(&mut child, pid, limits.termination_time).context(TerminateNewgidmapSnafu)?;
            return Err(Error::NewgidmapTimedOut {
                limit: limits.wall_time,
            });
        }
        thread::sleep(HELPER_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn bounded_stderr_file(limit: usize) -> Result<fs::File, io::Error> {
    let capacity = limit
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stderr byte limit is too large"))?;
    let name = CString::new("cast-newgidmap-stderr").expect("fixed memfd name has no NUL");
    let descriptor = memfd_create(&name, MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING)
        .map_err(io::Error::from)?;
    let file = fs::File::from_parts(descriptor.into(), "memfd:cast-newgidmap-stderr");
    file.set_len(capacity as u64)?;
    fcntl(
        file.as_raw_fd(),
        FcntlArg::F_ADD_SEALS(SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_SEAL),
    )
    .map_err(io::Error::from)?;
    Ok(file)
}

fn read_helper_stderr(mut stderr: fs::File, limit: usize) -> Result<Vec<u8>, io::Error> {
    let observed = stderr.stream_position()?;
    if observed > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("mapping helper wrote {observed} stderr bytes, exceeding the {limit}-byte limit"),
        ));
    }
    stderr.seek(SeekFrom::Start(0))?;
    let mut output = Vec::with_capacity(observed as usize);
    stderr.take(observed).read_to_end(&mut output)?;
    Ok(output)
}

fn terminate_and_reap(child: &mut Child, process_group: i32, timeout: Duration) -> Result<(), io::Error> {
    let signal_result = unsafe { nix::libc::kill(-process_group, nix::libc::SIGKILL) };
    let signal_error = if signal_result == -1 {
        let source = io::Error::last_os_error();
        (source.raw_os_error() != Some(Errno::ESRCH as i32)).then_some(source)
    } else {
        None
    };
    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return signal_error.map_or(Ok(()), Err),
            Ok(None) => {}
            Err(source) => return Err(source),
        }
        if Instant::now() >= deadline {
            return Err(signal_error.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out reaping mapping helper process group {process_group}"),
                )
            }));
        }
        thread::sleep(HELPER_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn verify_map(path: &Path, expected: &[(u32, u32, u32)], kind: &'static str) -> Result<(), io::Error> {
    let found = read_map(path)?;
    if found == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "unexpected {kind} map {found:?}; expected {expected:?}"
        )))
    }
}

fn read_map(path: &Path) -> Result<Vec<(u32, u32, u32)>, io::Error> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            let fields = line
                .split_ascii_whitespace()
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(io::Error::other)?;
            if let [namespace_id, host_id, count] = fields.as_slice() {
                Ok((*namespace_id, *host_id, *count))
            } else {
                Err(io::Error::other(format!("invalid ID map line {line:?}")))
            }
        })
        .collect()
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display(
        "container mapping requires uniform real/effective/saved/filesystem credentials, found uid {real_uid}/{effective_uid}/{saved_uid}/{filesystem_uid} and gid {real_gid}/{effective_gid}/{saved_gid}/{filesystem_gid}"
    ))]
    MixedCallerCredentials {
        real_uid: u32,
        effective_uid: u32,
        saved_uid: u32,
        filesystem_uid: u32,
        real_gid: u32,
        effective_gid: u32,
        saved_gid: u32,
        filesystem_gid: u32,
    },
    #[snafu(display("read caller real, effective, and saved-set GIDs"))]
    ReadCallerGroupCredentials {
        source: crate::credentials::CredentialSyscallError,
    },
    #[snafu(display("read caller real, effective, and saved-set UIDs"))]
    ReadCallerUserCredentials {
        source: crate::credentials::CredentialSyscallError,
    },
    #[snafu(display(
        "caller credentials changed between the authenticated pre-clone snapshot ({before_clone}) and namespace mapping ({before_mapping})"
    ))]
    CallerCredentialsChanged {
        before_clone: String,
        before_mapping: String,
    },
    #[snafu(display("write namespace UID map"))]
    WriteUidMap { source: io::Error },
    #[snafu(display("write namespace GID map"))]
    WriteGidMap { source: io::Error },
    #[snafu(display("verify namespace UID map"))]
    VerifyUidMap { source: io::Error },
    #[snafu(display("read namespace GID map"))]
    VerifyGidMap { source: io::Error },
    #[snafu(display("unexpected namespace GID map {found:?}"))]
    UnexpectedGidMap { found: Vec<(u32, u32, u32)> },
    #[snafu(display("read namespace setgroups policy"))]
    ReadSetgroups { source: io::Error },
    #[snafu(display("namespace setgroups policy was disabled before inherited groups could be cleared"))]
    SetgroupsDisabled,
    #[snafu(display("look up caller UID"))]
    GetUserByUid { source: nix::Error },
    #[snafu(display("caller UID {uid} has no passwd entry"))]
    MissingUser { uid: u32 },
    #[snafu(display("read /etc/subgid"))]
    ReadSubgid { source: io::Error },
    #[snafu(display("caller {username} (UID {uid}) needs at least one delegated subordinate GID in /etc/subgid"))]
    MissingSubgid { uid: u32, username: String },
    #[snafu(display("run fixed mapping helper {NEWGIDMAP}"))]
    RunNewgidmap { source: io::Error },
    #[snafu(display("prepare bounded stderr for fixed mapping helper {NEWGIDMAP}"))]
    PrepareNewgidmapStderr { source: io::Error },
    #[snafu(display("read bounded stderr from fixed mapping helper {NEWGIDMAP}"))]
    ReadNewgidmapStderr { source: io::Error },
    #[snafu(display("wait for fixed mapping helper {NEWGIDMAP}"))]
    WaitNewgidmap { source: io::Error },
    #[snafu(display("terminate and reap fixed mapping helper {NEWGIDMAP}"))]
    TerminateNewgidmap { source: io::Error },
    #[snafu(display("fixed mapping helper {NEWGIDMAP} exceeded its {limit:?} wall-time limit"))]
    NewgidmapTimedOut { limit: Duration },
    #[snafu(display(
        "fixed mapping helper {NEWGIDMAP} wrote at least {observed} stderr bytes, exceeding its {limit}-byte limit"
    ))]
    NewgidmapStderrTooLarge { observed: u64, limit: usize },
    #[snafu(display("{NEWGIDMAP} failed with {status}: {stderr}"))]
    NewgidmapFailed { status: String, stderr: String },
}

impl Error {
    /// Whether this failure proves that the host has not admitted the caller
    /// to the mandatory user-namespace mapping boundary.
    ///
    /// Keep this deliberately narrower than walking arbitrary error sources
    /// for `PermissionDenied`: verification, identity-integrity, and helper
    /// lifecycle failures remain hard errors even when an inner I/O operation
    /// happens to carry EPERM.
    pub(super) fn execution_capability_unavailable(&self) -> bool {
        match self {
            Self::MissingSubgid { .. } => true,
            Self::WriteUidMap { source }
            | Self::WriteGidMap { source }
            | Self::ReadSubgid { source }
            | Self::RunNewgidmap { source } => permission_denied(source),
            Self::MixedCallerCredentials { .. }
            | Self::ReadCallerGroupCredentials { .. }
            | Self::ReadCallerUserCredentials { .. }
            | Self::CallerCredentialsChanged { .. }
            | Self::VerifyUidMap { .. }
            | Self::VerifyGidMap { .. }
            | Self::UnexpectedGidMap { .. }
            | Self::ReadSetgroups { .. }
            | Self::SetgroupsDisabled
            | Self::GetUserByUid { .. }
            | Self::MissingUser { .. }
            | Self::PrepareNewgidmapStderr { .. }
            | Self::ReadNewgidmapStderr { .. }
            | Self::WaitNewgidmap { .. }
            | Self::TerminateNewgidmap { .. }
            | Self::NewgidmapTimedOut { .. }
            | Self::NewgidmapStderrTooLarge { .. }
            | Self::NewgidmapFailed { .. } => false,
        }
    }
}

fn permission_denied(source: &io::Error) -> bool {
    source.kind() == io::ErrorKind::PermissionDenied
        || matches!(source.raw_os_error(), Some(code) if code == nix::libc::EPERM || code == nix::libc::EACCES)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        os::{
            fd::{FromRawFd, OwnedFd},
            unix::fs::symlink,
        },
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use super::*;

    fn text_limits(bytes: usize, lines: usize, line_bytes: usize) -> TextLimits {
        TextLimits {
            bytes,
            lines,
            line_bytes,
        }
    }

    fn helper_limits(stderr_bytes: usize) -> HelperLimits {
        HelperLimits {
            wall_time: Duration::from_secs(2),
            termination_time: Duration::from_secs(1),
            stderr_bytes,
        }
    }

    fn uniform_caller(uid: u32, gid: u32) -> crate::credentials::IdentityCredentials {
        crate::credentials::IdentityCredentials {
            real_uid: uid,
            effective_uid: uid,
            saved_uid: uid,
            filesystem_uid: uid,
            real_gid: gid,
            effective_gid: gid,
            saved_gid: gid,
            filesystem_gid: gid,
        }
    }

    #[test]
    fn caller_mapping_identity_requires_every_uid_and_gid_slot_to_match() {
        let caller = uniform_caller(1000, 1001);
        assert_eq!(ensure_uniform_caller_credentials(&caller).unwrap(), (1000, 1001));

        let mutations: [fn(&mut crate::credentials::IdentityCredentials); 8] = [
            |credentials| credentials.real_uid = 2,
            |credentials| credentials.effective_uid = 2,
            |credentials| credentials.saved_uid = 2,
            |credentials| credentials.filesystem_uid = 2,
            |credentials| credentials.real_gid = 2,
            |credentials| credentials.effective_gid = 2,
            |credentials| credentials.saved_gid = 2,
            |credentials| credentials.filesystem_gid = 2,
        ];
        for mutate in mutations {
            let mut mixed = caller.clone();
            mutate(&mut mixed);
            assert!(matches!(
                ensure_uniform_caller_credentials(&mixed),
                Err(Error::MixedCallerCredentials { .. })
            ));
        }
    }

    #[test]
    fn caller_mapping_identity_must_not_change_after_the_pre_clone_snapshot() {
        let before_clone = uniform_caller(1000, 1001);
        assert!(ensure_unchanged_caller_credentials(&before_clone, &before_clone).is_ok());

        let mut before_mapping = before_clone.clone();
        before_mapping.real_gid = 1002;
        before_mapping.effective_gid = 1002;
        before_mapping.saved_gid = 1002;
        before_mapping.filesystem_gid = 1002;
        assert!(matches!(
            ensure_unchanged_caller_credentials(&before_clone, &before_mapping),
            Err(Error::CallerCredentialsChanged { .. })
        ));
    }

    fn denied_io() -> io::Error {
        io::Error::from_raw_os_error(nix::libc::EPERM)
    }

    #[test]
    fn execution_capability_classifier_accepts_only_idmap_admission_failures() {
        for failure in [
            Error::WriteUidMap { source: denied_io() },
            Error::WriteGidMap { source: denied_io() },
            Error::ReadSubgid { source: denied_io() },
            Error::RunNewgidmap { source: denied_io() },
            Error::MissingSubgid {
                uid: 1000,
                username: "builder".to_owned(),
            },
        ] {
            assert!(failure.execution_capability_unavailable(), "rejected {failure}");
        }

        assert!(
            !Error::WriteUidMap {
                source: io::Error::new(io::ErrorKind::InvalidData, "malformed map"),
            }
            .execution_capability_unavailable()
        );
    }

    #[test]
    fn execution_capability_classifier_keeps_integrity_and_lifecycle_failures_hard() {
        for failure in [
            Error::MixedCallerCredentials {
                real_uid: 1,
                effective_uid: 2,
                saved_uid: 3,
                filesystem_uid: 4,
                real_gid: 5,
                effective_gid: 6,
                saved_gid: 7,
                filesystem_gid: 8,
            },
            Error::ReadCallerGroupCredentials {
                source: crate::credentials::CredentialSyscallError::Kernel(Errno::EPERM),
            },
            Error::ReadCallerUserCredentials {
                source: crate::credentials::CredentialSyscallError::Kernel(Errno::EPERM),
            },
            Error::CallerCredentialsChanged {
                before_clone: "uid 1, gid 1".to_owned(),
                before_mapping: "uid 2, gid 2".to_owned(),
            },
            Error::VerifyUidMap { source: denied_io() },
            Error::ReadSetgroups { source: denied_io() },
            Error::ReadNewgidmapStderr { source: denied_io() },
            Error::WaitNewgidmap { source: denied_io() },
            Error::NewgidmapFailed {
                status: "1".to_owned(),
                stderr: "permission denied".to_owned(),
            },
        ] {
            assert!(!failure.execution_capability_unavailable(), "softened {failure}");
        }
    }

    #[test]
    fn mappings_have_fixed_namespace_identities() {
        assert_eq!(mapping(0, 1001), "0 1001 1\n");
        assert_eq!(
            gid_mapping(1002, 200000),
            format!("0 1002 1\n{AUXILIARY_GID} 200000 1\n")
        );
    }

    #[test]
    fn delegated_gid_selection_is_order_independent_and_uses_one_id() {
        let first = "builder:300000:10\n1001:200000:5\nother:100000:20\n";
        let second = "other:100000:20\n1001:200000:5\nbuilder:300000:10\n";
        assert_eq!(select_delegated_gid(first, 1001, "builder", 1002), Some(200000));
        assert_eq!(select_delegated_gid(second, 1001, "builder", 1002), Some(200000));
    }

    #[test]
    fn delegated_gid_selection_skips_primary_and_malformed_ranges() {
        let content = "builder:1002:2\nbuilder:not-a-gid:4\nbuilder:400000:0\ninvalid\n";
        assert_eq!(select_delegated_gid(content, 1001, "builder", 1002), Some(1003));
        assert_eq!(select_delegated_gid("other:1003:1\n", 1001, "builder", 1002), None);
    }

    #[test]
    fn map_parser_rejects_noncanonical_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let map = temporary.path().join("map");
        fs::write(&map, "0 1000\n").unwrap();
        assert!(read_map(&map).is_err());
        fs::write(&map, "0 1000 1\n").unwrap();
        assert_eq!(read_map(&map).unwrap(), vec![(0, 1000, 1)]);
    }

    #[test]
    fn bounded_text_accepts_exact_byte_line_and_line_length_limits() {
        let temporary = tempfile::tempdir().unwrap();
        let policy = temporary.path().join("subgid");
        let content = "aa\nbb\ncc";
        fs::write(&policy, content).unwrap();

        assert_eq!(
            read_bounded_regular_text(&policy, text_limits(content.len(), 3, 2)).unwrap(),
            content
        );
    }

    #[test]
    fn bounded_text_rejects_one_byte_over_each_limit() {
        let temporary = tempfile::tempdir().unwrap();
        let policy = temporary.path().join("subgid");
        let content = "aa\nbb\ncc";
        fs::write(&policy, content).unwrap();

        let bytes = read_bounded_regular_text(&policy, text_limits(content.len() - 1, 3, 2)).unwrap_err();
        assert_eq!(bytes.kind(), io::ErrorKind::InvalidData);
        assert!(bytes.to_string().contains("byte limit"));

        let lines = read_bounded_regular_text(&policy, text_limits(content.len(), 2, 2)).unwrap_err();
        assert_eq!(lines.kind(), io::ErrorKind::InvalidData);
        assert!(lines.to_string().contains("line limit"));

        let line_bytes = read_bounded_regular_text(&policy, text_limits(content.len(), 3, 1)).unwrap_err();
        assert_eq!(line_bytes.kind(), io::ErrorKind::InvalidData);
        assert!(line_bytes.to_string().contains("byte line limit"));
    }

    #[test]
    fn bounded_text_rejects_invalid_utf8_and_oversized_sparse_files() {
        let temporary = tempfile::tempdir().unwrap();
        let policy = temporary.path().join("subgid");
        fs::write(&policy, [0xff]).unwrap();
        let invalid = read_bounded_regular_text(&policy, text_limits(1, 1, 1)).unwrap_err();
        assert_eq!(invalid.kind(), io::ErrorKind::InvalidData);
        assert!(invalid.to_string().contains("not UTF-8"));

        let sparse = temporary.path().join("sparse-subgid");
        fs::File::create(&sparse).unwrap().set_len(9).unwrap();
        let oversized = read_bounded_regular_text(&sparse, text_limits(8, 1, 8)).unwrap_err();
        assert_eq!(oversized.kind(), io::ErrorKind::InvalidData);
        assert!(oversized.to_string().contains("9 bytes"));
    }

    #[test]
    fn bounded_text_rejects_symlinks_directories_and_fifos_without_blocking() {
        let temporary = tempfile::tempdir().unwrap();
        let policy = temporary.path().join("subgid");
        fs::write(&policy, "builder:100000:1\n").unwrap();
        let link = temporary.path().join("subgid-link");
        symlink(&policy, &link).unwrap();
        assert!(read_bounded_regular_text(&link, text_limits(64, 4, 32)).is_err());

        let directory = temporary.path().join("subgid-directory");
        fs::create_dir(&directory).unwrap();
        let directory_error = read_bounded_regular_text(&directory, text_limits(64, 4, 32)).unwrap_err();
        assert_eq!(directory_error.kind(), io::ErrorKind::InvalidData);

        let fifo = temporary.path().join("subgid-fifo");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let started = Instant::now();
        let fifo_error = read_bounded_regular_text(&fifo, text_limits(64, 4, 32)).unwrap_err();
        assert_eq!(fifo_error.kind(), io::ErrorKind::InvalidData);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn mapping_helper_accepts_exact_stderr_limit_and_rejects_one_more_byte() {
        let exact = run_mapping_helper(Path::new("/bin/sh"), &["-c", "printf 12345678 >&2"], helper_limits(8)).unwrap();
        assert!(exact.status.success());
        assert_eq!(exact.stderr, b"12345678");

        let error =
            run_mapping_helper(Path::new("/bin/sh"), &["-c", "printf 123456789 >&2"], helper_limits(8)).unwrap_err();
        assert!(matches!(
            error,
            Error::NewgidmapStderrTooLarge { observed: 9, limit: 8 }
        ));
    }

    #[test]
    fn mapping_helper_times_out_kills_its_process_group_and_reaps_the_child() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("pid");
        let args = [
            OsString::from("-c"),
            OsString::from("printf '%s' \"$$\" > \"$1\"; while :; do :; done"),
            OsString::from("mapping-helper-test"),
            pid_file.as_os_str().to_owned(),
        ];
        let limits = HelperLimits {
            wall_time: Duration::from_millis(100),
            termination_time: Duration::from_secs(1),
            stderr_bytes: 64,
        };
        let started = Instant::now();
        let error = run_mapping_helper(Path::new("/bin/sh"), &args, limits).unwrap_err();
        assert!(matches!(error, Error::NewgidmapTimedOut { limit } if limit == limits.wall_time));
        assert!(started.elapsed() < Duration::from_secs(2));

        let pid = fs::read_to_string(&pid_file).unwrap().parse::<i32>().unwrap();
        assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(Errno::ESRCH as i32));
    }

    #[test]
    fn mapping_helper_clears_environment_and_closes_unintended_descriptors() {
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let inherited_source = tempfile::tempfile().unwrap();
        let inherited_fd = unsafe { nix::libc::fcntl(inherited_source.as_raw_fd(), nix::libc::F_DUPFD, 200) };
        assert!(inherited_fd >= 200);
        let inherited = unsafe { OwnedFd::from_raw_fd(inherited_fd) };
        let inherited_path = format!("/proc/self/fd/{inherited_fd}");
        let args = [
            OsString::from("-c"),
            OsString::from(
                "test -z \"${CARGO_MANIFEST_DIR+x}\" && test \"$LANG\" = C && test \"$LC_ALL\" = C && test ! -e \"$1\"",
            ),
            OsString::from("mapping-helper-test"),
            OsString::from(inherited_path),
        ];
        let output = run_mapping_helper(Path::new("/bin/sh"), &args, helper_limits(64)).unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        drop(inherited);
    }
}
