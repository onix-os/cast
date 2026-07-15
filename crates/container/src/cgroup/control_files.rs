//! Descriptor-relative cgroup v2 control-file access and authentication.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::{self, Read as _};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use nix::libc;

use super::{CgroupError, CgroupEvents, DescriptorIdentity, Result};

const CGROUP2_SUPER_MAGIC: libc::c_long = 0x6367_7270;
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];
pub(super) const CONTROL_READ_LIMIT_BYTES: usize = 64 * 1024;
const MAX_WRITE_EINTR_RETRIES: usize = 3;
pub(super) const ANCHORED_RESOLUTION: u64 =
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV;

pub(super) fn open_owned_writable_control(directory: &OwnedFd, name: &CStr, label: &Path) -> Result<OwnedFd> {
    let descriptor = open_control(directory, name, libc::O_WRONLY | libc::O_CLOEXEC, label)?;
    require_owned_private(&descriptor, &label.join(os_str(name)))?;
    Ok(descriptor)
}

pub(super) fn require_owned_private(descriptor: &OwnedFd, label: &Path) -> Result<()> {
    let stat = descriptor_stat(descriptor)
        .map_err(|source| descriptor_error("inspect delegated cgroup owner", label, source))?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let expected_uid = unsafe { libc::geteuid() };
    if stat.st_uid != expected_uid {
        return Err(CgroupError::DelegationOwnerMismatch {
            path: label.to_owned(),
            expected_uid,
            found_uid: stat.st_uid,
        });
    }
    let mode = stat.st_mode & 0o7777;
    if mode & (libc::S_IWGRP | libc::S_IWOTH) != 0 {
        return Err(CgroupError::DelegationSharedWritable {
            path: label.to_owned(),
            mode,
        });
    }
    Ok(())
}

pub(super) fn acquire_exclusive_delegation(directory: &OwnedFd, label: &Path) -> Result<()> {
    require_owned_private(directory, label)?;

    // SAFETY: directory remains live and LOCK_EX|LOCK_NB is a valid operation.
    if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::WouldBlock {
            Err(CgroupError::DelegationAlreadyOwned { path: label.to_owned() })
        } else {
            Err(descriptor_error(
                "lock delegated cgroup for exclusive supervision",
                label,
                source,
            ))
        }
    } else {
        Ok(())
    }
}

pub(super) fn require_controllers(controllers: &BTreeSet<String>, path: &Path) -> Result<()> {
    let missing = REQUIRED_CONTROLLERS
        .iter()
        .copied()
        .filter(|controller| !controllers.contains(*controller))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CgroupError::MissingControllers {
            path: path.to_owned(),
            missing: missing.join(", "),
        })
    }
}

fn missing_required_controllers(enabled: &BTreeSet<String>) -> Vec<&'static str> {
    REQUIRED_CONTROLLERS
        .iter()
        .copied()
        .filter(|controller| !enabled.contains(*controller))
        .collect()
}

pub(super) fn controller_enable_request(enabled: &BTreeSet<String>) -> Option<String> {
    let missing = missing_required_controllers(enabled);
    (!missing.is_empty()).then(|| {
        missing
            .into_iter()
            .map(|controller| format!("+{controller}"))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn canonical_controller_set(controllers: &BTreeSet<String>) -> String {
    controllers.iter().map(String::as_str).collect::<Vec<_>>().join(" ")
}

fn require_exact_controller_set(found: &BTreeSet<String>, expected: &BTreeSet<String>, path: &Path) -> Result<()> {
    if found == expected {
        Ok(())
    } else {
        Err(CgroupError::ControlVerification {
            path: path.to_owned(),
            expected: canonical_controller_set(expected),
            found: canonical_controller_set(found),
        })
    }
}

/// Enable only the required controllers absent from the authenticated
/// pre-mutation set, then require an exact effective-set readback. Existing
/// delegated controllers are preserved, but an unexpected controller change
/// during the mutation fails closed rather than being silently accepted.
pub(super) fn enable_required_controllers_with(
    enabled: &BTreeSet<String>,
    path: &Path,
    write: &mut dyn FnMut(&[u8]) -> Result<()>,
    readback: &mut dyn FnMut() -> Result<BTreeSet<String>>,
) -> Result<()> {
    let mut expected = enabled.clone();
    expected.extend(missing_required_controllers(enabled).into_iter().map(str::to_owned));
    if let Some(request) = controller_enable_request(enabled) {
        write(request.as_bytes())?;
    }
    let found = readback()?;
    require_exact_controller_set(&found, &expected, path)
}

pub(super) fn enable_required_controllers(directory: &OwnedFd, label: &Path, enabled: &BTreeSet<String>) -> Result<()> {
    let path = label.join("cgroup.subtree_control");
    enable_required_controllers_with(
        enabled,
        &path,
        &mut |request| write_control(directory, c"cgroup.subtree_control", request, label),
        &mut || read_word_set(directory, c"cgroup.subtree_control", label),
    )
}

pub(super) fn require_empty_unfrozen_delegation(events: CgroupEvents, path: &Path) -> Result<()> {
    if events.frozen() {
        Err(CgroupError::DelegationFrozen { path: path.to_owned() })
    } else if events.populated() {
        Err(CgroupError::DelegationSubtreePopulated { path: path.to_owned() })
    } else {
        Ok(())
    }
}

pub(super) fn require_populated_unfrozen_delegation(events: CgroupEvents, path: &Path) -> Result<()> {
    if events.frozen() {
        Err(CgroupError::DelegationFrozen { path: path.to_owned() })
    } else if !events.populated() {
        Err(CgroupError::DelegationSubtreeUnpopulated { path: path.to_owned() })
    } else {
        Ok(())
    }
}

pub(super) fn read_descendant_counts(directory: &OwnedFd, label: &Path) -> Result<(u64, u64)> {
    let path = label.join("cgroup.stat");
    let bytes = read_control(directory, c"cgroup.stat", label)?;
    let values = parse_keyed_u64(&bytes, &path)?;
    let descendants = values
        .get("nr_descendants")
        .copied()
        .ok_or_else(|| malformed(&path, "missing required nr_descendants entry"))?;
    let dying_descendants = values
        .get("nr_dying_descendants")
        .copied()
        .ok_or_else(|| malformed(&path, "missing required nr_dying_descendants entry"))?;
    Ok((descendants, dying_descendants))
}

pub(super) fn read_events(directory: &OwnedFd, label: &Path) -> Result<CgroupEvents> {
    let path = label.join("cgroup.events");
    let bytes = read_control(directory, c"cgroup.events", label)?;
    parse_events(&bytes, &path)
}

pub(super) fn parse_events(bytes: &[u8], path: &Path) -> Result<CgroupEvents> {
    let values = parse_keyed_u64(bytes, path)?;
    let populated = required_binary_event(&values, "populated", path)?;
    let frozen = required_binary_event(&values, "frozen", path)?;
    Ok(CgroupEvents { populated, frozen })
}

fn required_binary_event(values: &BTreeMap<String, u64>, key: &'static str, path: &Path) -> Result<bool> {
    match values.get(key) {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        Some(value) => Err(malformed(path, format!("{key} must be 0 or 1, found {value}"))),
        None => Err(malformed(path, format!("missing required {key} entry"))),
    }
}

fn parse_keyed_u64(bytes: &[u8], path: &Path) -> Result<BTreeMap<String, u64>> {
    let text = ascii_control(bytes, path)?;
    let mut values = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let mut fields = line.split_ascii_whitespace();
        let Some(key) = fields.next() else {
            return Err(malformed(path, format!("line {} is empty", index + 1)));
        };
        let Some(value) = fields.next() else {
            return Err(malformed(path, format!("line {} has no value", index + 1)));
        };
        if fields.next().is_some() {
            return Err(malformed(path, format!("line {} has more than two fields", index + 1)));
        }
        if !key.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_') {
            return Err(malformed(path, format!("line {} has invalid key {key:?}", index + 1)));
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| malformed(path, format!("line {} has invalid counter {value:?}", index + 1)))?;
        if values.insert(key.to_owned(), value).is_some() {
            return Err(malformed(path, format!("duplicate key {key:?}")));
        }
    }
    Ok(values)
}

pub(super) fn read_word_set(directory: &OwnedFd, name: &CStr, label: &Path) -> Result<BTreeSet<String>> {
    let path = label.join(os_str(name));
    let bytes = read_control(directory, name, label)?;
    let text = ascii_control(&bytes, &path)?;
    let mut words = BTreeSet::new();
    for word in text.split_ascii_whitespace() {
        if !word.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_') {
            return Err(malformed(&path, format!("invalid controller name {word:?}")));
        }
        if !words.insert(word.to_owned()) {
            return Err(malformed(&path, format!("duplicate controller {word:?}")));
        }
    }
    Ok(words)
}

pub(super) fn read_single_value(directory: &OwnedFd, name: &CStr, label: &Path) -> Result<String> {
    let path = label.join(os_str(name));
    let bytes = read_control(directory, name, label)?;
    let text = ascii_control(&bytes, &path)?;
    let mut fields = text.split_ascii_whitespace();
    let value = fields.next().ok_or_else(|| malformed(&path, "control is empty"))?;
    if fields.next().is_some() {
        return Err(malformed(&path, "control contains multiple values"));
    }
    Ok(value.to_owned())
}

pub(super) fn verify_control(directory: &OwnedFd, name: &CStr, expected: &str, label: &Path) -> Result<()> {
    let path = label.join(os_str(name));
    let bytes = read_control(directory, name, label)?;
    let text = ascii_control(&bytes, &path)?;
    let found = text.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    if found == expected {
        Ok(())
    } else {
        Err(CgroupError::ControlVerification {
            path,
            expected: expected.to_owned(),
            found,
        })
    }
}

pub(super) fn read_pid_list(directory: &OwnedFd, name: &CStr, label: &Path) -> Result<Vec<u32>> {
    let path = label.join(os_str(name));
    let bytes = read_control(directory, name, label)?;
    parse_pid_list(&bytes, &path)
}

pub(super) fn parse_pid_list(bytes: &[u8], path: &Path) -> Result<Vec<u32>> {
    let text = ascii_control(bytes, path)?;
    let mut pids = Vec::new();
    for field in text.split_ascii_whitespace() {
        let pid = field
            .parse::<u32>()
            .map_err(|_| malformed(path, format!("invalid PID {field:?}")))?;
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(malformed(path, format!("PID is outside the positive i32 range: {pid}")));
        }
        pids.push(pid);
    }
    Ok(pids)
}

fn ascii_control<'a>(bytes: &'a [u8], path: &Path) -> Result<&'a str> {
    if !bytes.is_ascii() {
        return Err(malformed(path, "control is not ASCII"));
    }
    std::str::from_utf8(bytes).map_err(|_| malformed(path, "control is not UTF-8"))
}

fn malformed(path: &Path, reason: impl Into<String>) -> CgroupError {
    CgroupError::MalformedControl {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

pub(super) fn read_control(directory: &OwnedFd, name: &CStr, label: &Path) -> Result<Vec<u8>> {
    let path = label.join(os_str(name));
    let descriptor = open_control(
        directory,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        label,
    )?;
    let mut file = File::from(descriptor);
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| descriptor_error("read cgroup control", &path, source))?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > CONTROL_READ_LIMIT_BYTES {
            return Err(CgroupError::ControlTooLarge {
                path,
                limit: CONTROL_READ_LIMIT_BYTES,
            });
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

pub(super) fn write_control(directory: &OwnedFd, name: &CStr, value: &[u8], label: &Path) -> Result<()> {
    let path = label.join(os_str(name));
    let descriptor = open_control(
        directory,
        name,
        libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_TRUNC,
        label,
    )?;
    write_control_descriptor(&descriptor, &path, value)
}

fn write_control_descriptor(descriptor: &OwnedFd, path: &Path, value: &[u8]) -> Result<()> {
    write_exact_control_value(path, value, &mut |bytes| {
        // SAFETY: descriptor and bytes remain live for this single write.
        let written = unsafe { libc::write(descriptor.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if written == -1 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(written).map_err(|_| io::Error::other("write returned an invalid length"))
        }
    })
}

/// Write a control when it exists, accepting only an exact missing name.
///
/// A wrong-kind object, symlink, permission failure, unsupported resolution,
/// or any write failure is not absence and remains fatal.
pub(super) fn write_control_if_present(directory: &OwnedFd, name: &CStr, value: &[u8], label: &Path) -> Result<bool> {
    let path = label.join(os_str(name));
    let descriptor = match open_control_path(
        directory,
        name,
        libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_TRUNC,
    ) {
        Ok(descriptor) => descriptor,
        Err(source) if source.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
        Err(source) => return Err(descriptor_error("open cgroup control", &path, source)),
    };
    require_control_file(&descriptor, &path)?;
    write_control_descriptor(&descriptor, &path, value)?;
    Ok(true)
}

pub(super) fn write_exact_control_value(
    path: &Path,
    value: &[u8],
    write: &mut dyn FnMut(&[u8]) -> io::Result<usize>,
) -> Result<()> {
    let mut retries = 0;
    loop {
        let written = match write(value) {
            Ok(written) => written,
            Err(source) => {
                if source.kind() == io::ErrorKind::Interrupted && retries < MAX_WRITE_EINTR_RETRIES {
                    retries += 1;
                    continue;
                }
                return Err(descriptor_error("write cgroup control", path, source));
            }
        };
        if written != value.len() {
            return Err(CgroupError::ShortControlWrite {
                path: path.to_owned(),
                expected: value.len(),
                written,
            });
        }
        return Ok(());
    }
}

fn open_control(directory: &OwnedFd, name: &CStr, flags: i32, label: &Path) -> Result<OwnedFd> {
    let path = label.join(os_str(name));
    let descriptor = open_control_path(directory, name, flags)
        .map_err(|source| descriptor_error("open cgroup control", &path, source))?;
    require_control_file(&descriptor, &path)?;
    Ok(descriptor)
}

fn require_control_file(descriptor: &OwnedFd, path: &Path) -> Result<()> {
    let stat =
        descriptor_stat(descriptor).map_err(|source| descriptor_error("inspect cgroup control", path, source))?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(CgroupError::NotControlFile { path: path.to_owned() });
    }
    Ok(())
}

pub(super) fn open_control_path(directory: &OwnedFd, name: &CStr, flags: i32) -> io::Result<OwnedFd> {
    openat2(directory.as_raw_fd(), name, flags, ANCHORED_RESOLUTION)
}

pub(super) fn openat2(parent: RawFd, path: &CStr, flags: i32, resolve: u64) -> io::Result<OwnedFd> {
    // SAFETY: zero is a valid initial value for every public open_how field.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.resolve = resolve;
    // SAFETY: parent, path, and open_how remain live for the syscall and a
    // successful call returns one fresh descriptor.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

#[cfg(test)]
pub(super) fn duplicate_cloexec(descriptor: &impl AsRawFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC returns a fresh descriptor and does not retain a
    // borrow of the input descriptor.
    let duplicate = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn descriptor_stat(descriptor: &OwnedFd) -> io::Result<libc::stat> {
    // SAFETY: zero is valid output storage and descriptor remains live.
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor.as_raw_fd(), &mut stat) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(stat)
    }
}

pub(super) fn descriptor_identity(descriptor: &OwnedFd, label: &Path) -> Result<DescriptorIdentity> {
    let stat = descriptor_stat(descriptor).map_err(|source| descriptor_error("inspect cgroup leaf", label, source))?;
    Ok(DescriptorIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

pub(super) fn require_directory(descriptor: &OwnedFd, label: &Path) -> Result<()> {
    let stat =
        descriptor_stat(descriptor).map_err(|source| descriptor_error("inspect cgroup directory", label, source))?;
    if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
        Ok(())
    } else {
        Err(CgroupError::NotDirectory { path: label.to_owned() })
    }
}

pub(super) fn require_cgroup2(descriptor: &OwnedFd, label: &Path) -> Result<()> {
    // SAFETY: zero is valid output storage and descriptor remains live.
    let mut stat: libc::statfs = unsafe { zeroed() };
    if unsafe { libc::fstatfs(descriptor.as_raw_fd(), &mut stat) } == -1 {
        return Err(descriptor_error(
            "inspect cgroup filesystem",
            label,
            io::Error::last_os_error(),
        ));
    }
    if stat.f_type == CGROUP2_SUPER_MAGIC {
        Ok(())
    } else {
        Err(CgroupError::NotCgroupV2 {
            path: label.to_owned(),
            found: stat.f_type,
        })
    }
}

pub(super) fn path_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

pub(super) fn os_str(value: &CStr) -> &OsStr {
    OsStr::from_bytes(value.to_bytes())
}

pub(super) fn descriptor_error(operation: &'static str, path: &Path, source: io::Error) -> CgroupError {
    CgroupError::DescriptorOperation {
        operation,
        path: path.to_owned(),
        source,
    }
}
