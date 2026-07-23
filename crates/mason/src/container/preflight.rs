use std::{
    ffi::CString,
    io,
    os::{fd::FromRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

#[cfg(feature = "delegated-fixture-test-support")]
use std::io::{Read as _, Write as _};

#[cfg(feature = "delegated-fixture-test-support")]
use ::container::RootFilesystemPolicy;
use ::container::{AnchoredLocator, Container};
#[cfg(feature = "delegated-fixture-test-support")]
use stone_recipe::derivation::FilesystemPolicy;

use super::Error;
#[cfg(feature = "delegated-fixture-test-support")]
use super::{discover_delegated_cgroup, frozen_cgroup_limits, frozen_loopback_policy, frozen_pseudo_filesystems};

#[cfg(feature = "delegated-fixture-test-support")]
const EXECUTION_PREFLIGHT_CGROUP_IDENTITY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Exercise the exact production clone3, credential, mount, and cgroup path
/// without first materializing a package closure.
#[cfg(feature = "delegated-fixture-test-support")]
pub(crate) fn preflight_delegated_execution_capability() -> Result<(), Error> {
    let root = tempfile::Builder::new()
        .prefix("cast-execution-capability-preflight-")
        .tempdir()
        .map_err(Error::CreateExecutionPreflightRoot)?;
    for relative in [
        "tmp",
        "dev",
        "preflight-root-source",
        "preflight-root-target",
        "preflight-pinned-target",
    ] {
        let path = root.path().join(relative);
        std::fs::create_dir(&path).map_err(|source| Error::PrepareExecutionPreflightRoot { path, source })?;
    }
    let anchor = open_execution_preflight_root(root.path()).map_err(|source| Error::OpenExecutionPreflightRoot {
        path: root.path().to_owned(),
        source,
    })?;
    let pinned = tempfile::Builder::new()
        .prefix("cast-execution-capability-preflight-bind-")
        .tempdir()
        .map_err(Error::CreateExecutionPreflightBindSource)?;
    let pinned_anchor =
        open_execution_preflight_root(pinned.path()).map_err(|source| Error::OpenExecutionPreflightBindSource {
            path: pinned.path().to_owned(),
            source,
        })?;
    let root_locator = AnchoredLocator::exact(root.path(), &anchor).map_err(Error::LocateExecutionPreflightRoot)?;
    let pinned_locator =
        AnchoredLocator::exact(pinned.path(), &pinned_anchor).map_err(Error::AnchorExecutionPreflightBindSource)?;
    let container = Container::new_anchored(root_locator)
        .map_err(Error::AnchorExecutionPreflightRoot)?
        .hostname("cast-execution-preflight")
        .networking(false)
        .ignore_host_sigint(true)
        .work_dir("/preflight-pinned-target")
        .pseudo_filesystems(frozen_pseudo_filesystems(FilesystemPolicy::default()))
        .loopback(frozen_loopback_policy())
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .bind_rw_from_root("/preflight-root-source", "/preflight-root-target")?
        .bind_rw_pinned(pinned_locator, "/preflight-pinned-target")?;
    let limits = frozen_cgroup_limits(1)?;
    let delegated = discover_delegated_cgroup()?;
    let leaf = delegated
        .create_leaf(EXECUTION_PREFLIGHT_CGROUP_IDENTITY, limits)
        .map_err(Error::CreateDerivationCgroup)?;

    container.run_in_cgroup::<io::Error>(leaf, || {
        verify_execution_device_contract()?;
        std::fs::write("/preflight-root-target/witness", b"root-relative")?;
        std::fs::write("workdir-witness", b"descriptor-pinned-workdir")
    })?;
    for (path, expected) in [
        (
            root.path().join("preflight-root-source/witness"),
            b"root-relative".as_slice(),
        ),
        (
            pinned.path().join("workdir-witness"),
            b"descriptor-pinned-workdir".as_slice(),
        ),
    ] {
        let found = std::fs::read(&path).map_err(|source| Error::VerifyExecutionPreflightPayload {
            path: path.clone(),
            source,
        })?;
        if found != expected {
            return Err(Error::UnexpectedExecutionPreflightPayload { path });
        }
    }
    Ok(())
}

#[cfg(feature = "delegated-fixture-test-support")]
fn verify_execution_device_contract() -> io::Result<()> {
    let mut found = std::fs::read_dir("/dev")?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    found.sort();
    let mut expected = ::container::MINIMAL_DEV_NODES
        .iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    expected.sort();
    if found != expected {
        return Err(io::Error::other(format!(
            "minimal /dev entries differ: expected {expected:?}, found {found:?}"
        )));
    }

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("/dev/cast-preflight-extra")
    {
        Err(source) if source.raw_os_error() == Some(nix::libc::EROFS) => {}
        Err(source) => return Err(source),
        Ok(_) => return Err(io::Error::other("minimal /dev accepted an undeclared entry")),
    }

    verify_null_open("write-only", false, false)?;
    verify_null_open("write-and-truncate", false, true)?;
    verify_null_open("write-and-create", true, false)?;
    let mut null = verify_null_open("python write-create-truncate", true, true)?;
    null.write_all(b"discarded")?;
    drop(null);

    let mut null = std::fs::File::open("/dev/null")?;
    let mut byte = [0_u8; 1];
    if null.read(&mut byte)? != 0 {
        return Err(io::Error::other("/dev/null did not return EOF"));
    }

    let mut zero = std::fs::File::open("/dev/zero")?;
    let mut zeros = [1_u8; 16];
    zero.read_exact(&mut zeros)?;
    if zeros != [0_u8; 16] {
        return Err(io::Error::other("/dev/zero returned non-zero bytes"));
    }

    let mut full = std::fs::OpenOptions::new().write(true).open("/dev/full")?;
    match full.write_all(&[1]) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOSPC) => Ok(()),
        Err(source) => Err(source),
        Ok(()) => Err(io::Error::other("/dev/full accepted a byte")),
    }
}

#[cfg(feature = "delegated-fixture-test-support")]
fn verify_null_open(label: &str, create: bool, truncate: bool) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(create)
        .truncate(truncate)
        .open("/dev/null")
        .map_err(|source| io::Error::new(source.kind(), format!("minimal /dev/null {label} open failed: {source}")))
}

fn open_execution_preflight_root(path: &Path) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "execution preflight root contains NUL"))?;
    // SAFETY: path is NUL-terminated and these flags request a descriptor-only
    // directory capability without following the final component.
    let descriptor = unsafe {
        nix::libc::open(
            path.as_ptr(),
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful open returned one fresh owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

pub(crate) fn execution_namespace_capability_unavailable(error: &Error) -> bool {
    matches!(error, Error::Container(error) if error.execution_capability_unavailable())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd as _;

    use super::*;

    #[test]
    fn execution_preflight_root_is_an_opath_directory_capability() {
        let root = crate::private_tempdir();
        let anchor = open_execution_preflight_root(root.path()).unwrap();
        // SAFETY: F_GETFL only reads status flags from this live descriptor.
        let flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(flags & nix::libc::O_PATH, nix::libc::O_PATH);
        let locator = AnchoredLocator::exact(root.path(), &anchor).unwrap();
        Container::new_anchored(locator).unwrap();
    }

    #[test]
    fn execution_preflight_classifies_only_known_namespace_setup_denials() {
        for message in [
            "clear inherited supplementary groups: EPERM: Operation not permitted",
            "normalize payload real, effective, and saved-set GIDs: EACCES: Permission denied",
            "normalize payload real, effective, and saved-set UIDs: ENOSYS: Function not implemented",
            "mount /: EACCES: Permission denied",
            "pivot_root: ENOSYS: Function not implemented",
            "sethostname: EPERM: Operation not permitted",
            "unmount old root: EACCES: Permission denied",
        ] {
            assert!(execution_namespace_capability_unavailable(&Error::Container(
                ::container::Error::Failure {
                    message: message.to_owned(),
                }
            )));
        }

        assert!(execution_namespace_capability_unavailable(&Error::Container(
            ::container::Error::CloneIntoCgroup {
                source: io::Error::from_raw_os_error(nix::libc::EPERM),
            }
        )));
        assert!(!execution_namespace_capability_unavailable(&Error::Container(
            ::container::Error::CloneIntoCgroup {
                source: io::Error::from_raw_os_error(nix::libc::EAGAIN),
            }
        )));
        assert!(!execution_namespace_capability_unavailable(&Error::Container(
            ::container::Error::Failure {
                message: "run: EPERM: Operation not permitted".to_owned(),
            }
        )));
        assert!(!execution_namespace_capability_unavailable(&Error::Container(
            ::container::Error::Nix {
                source: nix::errno::Errno::EPERM,
            }
        )));
    }
}
