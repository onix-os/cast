//! Descriptor-rooted local `/etc` capability for transaction triggers.

use std::{
    ffi::{CStr, CString, OsStr},
    io,
    mem::size_of,
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
};

use crate::{Installation, linux_fs};

const MAX_PRIVATE_ETC_ATTEMPTS: usize = 256;
const MAX_RANDOM_INTERRUPTS: usize = 1_024;
const PRIVATE_ETC_RANDOM_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

/// One exact local `/etc` directory retained through transaction-trigger
/// container construction and activation.
///
/// The value is deliberately non-cloneable. Pathnames are diagnostic only;
/// bind authority comes from `directory` after an exact final-name sandwich.
#[derive(Debug)]
pub(super) struct RetainedLocalEtc {
    directory: std::fs::File,
    path: std::path::PathBuf,
    witness: DirectoryWitness,
}

impl RetainedLocalEtc {
    pub(super) fn directory(&self) -> &std::fs::File {
        &self.directory
    }

    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(super) fn revalidate(&self, installation: &Installation) -> Result<(), super::Error> {
        installation.revalidate_root_directory()?;
        let retained = local_etc_witness(&self.directory, &self.path, installation)?;
        if retained != self.witness {
            return Err(local_etc_changed(installation, "retained local /etc metadata changed"));
        }
        require_no_acl(
            &self.directory,
            &self.path,
            installation,
            "revalidate retained local /etc",
        )?;

        let named = open_local_etc(installation, "reopen retained local /etc final name")?;
        if local_etc_witness(&named, &self.path, installation)? != self.witness
            || local_etc_witness(&self.directory, &self.path, installation)? != self.witness
        {
            return Err(local_etc_changed(installation, "local /etc final name changed"));
        }
        require_no_acl(&named, &self.path, installation, "revalidate named local /etc")?;
        installation.revalidate_root_directory()?;
        Ok(())
    }
}

/// Provision the deterministic local `/etc` topology before acquiring
/// active-state authority and return the exact retained directory.
///
/// Missing `/etc` is created behind a kernel-random private name. A short-lived
/// child gives that inode its final `0755` mode at creation without changing
/// the parent process's umask. The exact empty private inode is then retained
/// and published once with `RENAME_NOREPLACE`; neither the private name nor the
/// canonical name is ever chmodded after a pathname reopen.
pub(super) fn prepare_local_etc(installation: &Installation) -> Result<RetainedLocalEtc, super::Error> {
    installation.revalidate_root_directory()?;
    match open_component(installation, c"etc") {
        Ok(_) => return require_local_etc(installation),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => {}
        Err(source) => return Err(local_etc_error("probe local /etc", installation, source)),
    }

    for _ in 0..MAX_PRIVATE_ETC_ATTEMPTS {
        let name = random_private_etc_name(installation)?;
        let path = installation.root.join(OsStr::from_bytes(name.to_bytes()));
        match create_private_component_exact(installation, &name, &path) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EEXIST) => continue,
            Err(source) => return Err(local_etc_error("create private local /etc", installation, source)),
        }

        after_private_local_etc_created(&path);
        let pinned = open_private_component(installation, &name, &path)?;
        require_private_residue(&pinned, &path)?;
        require_private_name(installation, &name, &path, &pinned)?;

        let readable = open_private_component_readable(installation, &name, &path)?;
        require_same_inode(&pinned, &readable, installation, "retain normalized private local /etc")?;
        require_empty_directory(&readable, &path, installation)?;
        require_no_acl(&readable, &path, installation, "reject private local /etc")?;
        readable
            .sync_all()
            .map_err(|source| local_etc_error("sync private local /etc", installation, source))?;

        let publication = linux_fs::renameat2_noreplace_once(
            installation.root_directory(),
            &name,
            installation.root_directory(),
            c"etc",
        );
        let canonical = require_local_etc(installation);
        match (publication, canonical) {
            (Ok(()), Ok(canonical)) | (Err(_), Ok(canonical))
                if same_inode(
                    canonical.directory(),
                    &pinned,
                    installation,
                    "reconcile local /etc publication",
                )? =>
            {
                require_component_absent(installation, &name, &path)?;
                installation.root_directory().sync_all().map_err(|source| {
                    local_etc_error(
                        "sync installation root after local /etc publication",
                        installation,
                        source,
                    )
                })?;
                canonical.revalidate(installation)?;
                return Ok(canonical);
            }
            (Err(source), _) => {
                return Err(local_etc_error(
                    "publish private local /etc without replacement",
                    installation,
                    source,
                ));
            }
            (Ok(()), _) => {
                return Err(local_etc_changed(
                    installation,
                    "published local /etc does not denote the retained private inode",
                ));
            }
        }
    }

    Err(local_etc_error(
        "reserve private local /etc",
        installation,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("all {MAX_PRIVATE_ETC_ATTEMPTS} private names were occupied"),
        ),
    ))
}

/// Authenticate and retain the existing local `/etc` without changing it.
pub(super) fn require_local_etc(installation: &Installation) -> Result<RetainedLocalEtc, super::Error> {
    installation.revalidate_root_directory()?;
    let path = installation.root.join("etc");
    let directory = open_local_etc(installation, "open local /etc")?;
    let witness = local_etc_witness(&directory, &path, installation)?;
    require_no_acl(&directory, &path, installation, "reject local /etc")?;
    after_first_local_etc_witness();
    let named = open_local_etc(installation, "reopen local /etc final name")?;
    if local_etc_witness(&named, &path, installation)? != witness
        || local_etc_witness(&directory, &path, installation)? != witness
    {
        return Err(local_etc_changed(
            installation,
            "local /etc changed during descriptor-rooted proof",
        ));
    }
    require_no_acl(&named, &path, installation, "revalidate local /etc")?;
    installation.revalidate_root_directory()?;
    Ok(RetainedLocalEtc {
        directory,
        path,
        witness,
    })
}

fn open_local_etc(installation: &Installation, operation: &'static str) -> Result<std::fs::File, super::Error> {
    open_component(installation, c"etc").map_err(|source| local_etc_error(operation, installation, source))
}

/// Create one exact-mode directory without changing the multithreaded parent
/// process's umask.
///
/// The child inherits only the already-retained installation-root dirfd for
/// authority. After `fork(2)` it performs async-signal-safe libc operations
/// only, reports the `mkdirat(2)` errno through a pipe, and exits immediately.
/// Consequently the directory is born as `0755`; the parent never needs to
/// chmod an inode reopened after the creation syscall.
fn create_private_component_exact(installation: &Installation, name: &CStr, path: &std::path::Path) -> io::Result<()> {
    let mut report_pipe = [-1; 2];
    // SAFETY: report_pipe points to two writable integers.
    if unsafe { nix::libc::pipe2(report_pipe.as_mut_ptr(), nix::libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: the child branch below calls async-signal-safe libc operations
    // only and terminates with _exit without unwinding Rust state.
    let child = unsafe { nix::libc::fork() };
    if child == -1 {
        let source = io::Error::last_os_error();
        close_raw_fd(report_pipe[0]);
        close_raw_fd(report_pipe[1]);
        return Err(source);
    }
    if child == 0 {
        close_raw_fd(report_pipe[0]);
        // SAFETY: this umask belongs to the short-lived fork child only. The
        // inherited dirfd and CString remain valid until _exit.
        unsafe {
            nix::libc::umask(0);
            let result = nix::libc::mkdirat(installation.root_directory().as_raw_fd(), name.as_ptr(), 0o755);
            let errno = if result == 0 {
                0_i32
            } else {
                *nix::libc::__errno_location()
            };
            let report = errno.to_ne_bytes();
            let mut written = 0usize;
            while written < report.len() {
                let result = nix::libc::write(
                    report_pipe[1],
                    report[written..].as_ptr().cast(),
                    report.len() - written,
                );
                if result == -1 {
                    if *nix::libc::__errno_location() == nix::libc::EINTR {
                        continue;
                    }
                    nix::libc::_exit(126);
                }
                if result == 0 {
                    nix::libc::_exit(126);
                }
                written += result as usize;
            }
            nix::libc::close(report_pipe[1]);
            nix::libc::_exit(0);
        }
    }

    close_raw_fd(report_pipe[1]);
    let report = read_child_errno(report_pipe[0]);
    close_raw_fd(report_pipe[0]);
    let status = wait_for_creation_child(child, path);

    status?;
    match report? {
        0 => Ok(()),
        errno => Err(io::Error::from_raw_os_error(errno)),
    }
}

fn read_child_errno(fd: nix::libc::c_int) -> io::Result<i32> {
    let mut report = [0_u8; size_of::<i32>()];
    let mut read = 0usize;
    while read < report.len() {
        // SAFETY: the remaining report buffer is writable for the supplied
        // length and fd is the parent's pipe read end.
        let result = unsafe { nix::libc::read(fd, report[read..].as_mut_ptr().cast(), report.len() - read) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(source);
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "private local /etc creation child exited without an errno report",
            ));
        }
        read += result as usize;
    }
    Ok(i32::from_ne_bytes(report))
}

fn wait_for_creation_child(child: nix::libc::pid_t, path: &std::path::Path) -> io::Result<()> {
    let mut status = 0_i32;
    loop {
        // SAFETY: child is the exact pid returned by fork and status is live.
        let result = unsafe { nix::libc::waitpid(child, &mut status, 0) };
        if result == child {
            break;
        }
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(source);
        }
        return Err(io::Error::other(format!(
            "waitpid returned unexpected pid {result} for private directory {}",
            path.display()
        )));
    }

    if nix::libc::WIFEXITED(status) && nix::libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "private local /etc creation child failed with wait status {status} for {}",
            path.display()
        )))
    }
}

fn close_raw_fd(fd: nix::libc::c_int) {
    // SAFETY: callers pass one owned pipe end and never reuse it afterwards.
    unsafe {
        nix::libc::close(fd);
    }
}

fn open_component(installation: &Installation, name: &CStr) -> io::Result<std::fs::File> {
    linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        linux_fs::controlled_resolution(),
    )
}

fn open_private_component(
    installation: &Installation,
    name: &CStr,
    path: &std::path::Path,
) -> Result<std::fs::File, super::Error> {
    linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        linux_fs::controlled_resolution(),
    )
    .map_err(|source| local_etc_error_at("retain private local /etc", path, source))
}

fn open_private_component_readable(
    installation: &Installation,
    name: &CStr,
    path: &std::path::Path,
) -> Result<std::fs::File, super::Error> {
    open_component(installation, name).map_err(|source| local_etc_error_at("open private local /etc", path, source))
}

fn require_no_acl(
    directory: &std::fs::File,
    path: &std::path::Path,
    installation: &Installation,
    operation: &'static str,
) -> Result<(), super::Error> {
    linux_fs::require_no_access_acl(directory, path)
        .map_err(|source| local_etc_error(operation, installation, source))?;
    linux_fs::require_no_default_acl(directory, path).map_err(|source| local_etc_error(operation, installation, source))
}

fn local_etc_witness(
    directory: &std::fs::File,
    path: &std::path::Path,
    installation: &Installation,
) -> Result<DirectoryWitness, super::Error> {
    let metadata = directory
        .metadata()
        .map_err(|source| local_etc_error("revalidate local /etc metadata", installation, source))?;
    let witness = DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    };
    if metadata.file_type().is_dir() && witness.owner == super::effective_user_id() && witness.mode == 0o755 {
        Ok(witness)
    } else {
        Err(local_etc_error(
            "validate local /etc policy",
            installation,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "unsafe local /etc at {} (uid={}, mode={:04o})",
                    path.display(),
                    witness.owner,
                    witness.mode
                ),
            ),
        ))
    }
}

fn require_private_residue(directory: &std::fs::File, path: &std::path::Path) -> Result<(), super::Error> {
    let metadata = directory
        .metadata()
        .map_err(|source| local_etc_error_at("inspect private local /etc", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if metadata.file_type().is_dir() && metadata.uid() == super::effective_user_id() && mode == 0o755 {
        Ok(())
    } else {
        Err(local_etc_error_at(
            "validate private local /etc residue",
            path,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("unsafe private local /etc (uid={}, mode={mode:04o})", metadata.uid()),
            ),
        ))
    }
}

fn require_private_name(
    installation: &Installation,
    name: &CStr,
    path: &std::path::Path,
    expected: &std::fs::File,
) -> Result<(), super::Error> {
    let named = open_private_component(installation, name, path)?;
    require_same_inode(
        expected,
        &named,
        installation,
        "revalidate private local /etc final name",
    )
}

fn require_same_inode(
    expected: &std::fs::File,
    actual: &std::fs::File,
    installation: &Installation,
    operation: &'static str,
) -> Result<(), super::Error> {
    if same_inode(expected, actual, installation, operation)? {
        Ok(())
    } else {
        Err(local_etc_changed(installation, "local /etc directory inode changed"))
    }
}

fn same_inode(
    expected: &std::fs::File,
    actual: &std::fs::File,
    installation: &Installation,
    operation: &'static str,
) -> Result<bool, super::Error> {
    let expected = expected
        .metadata()
        .map_err(|source| local_etc_error(operation, installation, source))?;
    let actual = actual
        .metadata()
        .map_err(|source| local_etc_error(operation, installation, source))?;
    Ok((expected.dev(), expected.ino()) == (actual.dev(), actual.ino()))
}

fn require_component_absent(
    installation: &Installation,
    name: &CStr,
    path: &std::path::Path,
) -> Result<(), super::Error> {
    match linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        linux_fs::controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Err(source) => Err(local_etc_error_at(
            "prove private local /etc name absence",
            path,
            source,
        )),
        Ok(_) => Err(local_etc_error_at(
            "prove private local /etc name absence",
            path,
            io::Error::new(io::ErrorKind::AlreadyExists, "private local /etc name remains"),
        )),
    }
}

fn require_empty_directory(
    directory: &std::fs::File,
    path: &std::path::Path,
    _installation: &Installation,
) -> Result<(), super::Error> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(local_etc_error_at(
            "duplicate private local /etc for inventory",
            path,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: the fresh descriptor is a directory and remains uniquely owned.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed without consuming the descriptor.
        unsafe { nix::libc::close(duplicate) };
        return Err(local_etc_error_at("enumerate private local /etc", path, source));
    }
    let result = loop {
        // SAFETY: errno is thread-local and stream remains live.
        unsafe { *nix::libc::__errno_location() = 0 };
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(local_etc_error_at("enumerate private local /etc", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for a live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        break Err(local_etc_error_at(
            "validate empty private local /etc",
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private directory contains entry {name:?}"),
            ),
        ));
    };
    // SAFETY: stream came from fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(local_etc_error_at(
            "close private local /etc inventory",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

fn random_private_etc_name(installation: &Installation) -> Result<CString, super::Error> {
    let mut random = [0_u8; PRIVATE_ETC_RANDOM_BYTES];
    let mut filled = 0usize;
    let mut interruptions = 0usize;
    while filled < random.len() {
        // SAFETY: the remaining byte slice is writable and GRND_NONBLOCK keeps
        // this namespace preparation finite.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                nix::libc::GRND_NONBLOCK,
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interruptions < MAX_RANDOM_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(local_etc_error(
                "generate private local /etc name",
                installation,
                source,
            ));
        }
        let read = usize::try_from(result)
            .map_err(|_| local_etc_changed(installation, "getrandom returned an invalid length"))?;
        if read == 0 || read > random.len() - filled {
            return Err(local_etc_changed(installation, "getrandom returned a short result"));
        }
        filled += read;
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b".cast-local-etc-";
    let mut encoded = Vec::with_capacity(prefix.len() + random.len() * 2);
    encoded.extend_from_slice(prefix);
    for byte in random {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(encoded).map_err(|source| {
        local_etc_error(
            "encode private local /etc name",
            installation,
            io::Error::new(io::ErrorKind::InvalidData, source),
        )
    })
}

fn local_etc_changed(installation: &Installation, message: &'static str) -> super::Error {
    local_etc_error(
        "revalidate local /etc final name",
        installation,
        io::Error::other(message),
    )
}

fn local_etc_error(operation: &'static str, installation: &Installation, source: io::Error) -> super::Error {
    super::Error::LiveActiveStateProof {
        operation,
        path: installation.root.join("etc"),
        source,
    }
}

fn local_etc_error_at(operation: &'static str, path: &std::path::Path, source: io::Error) -> super::Error {
    super::Error::LiveActiveStateProof {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
thread_local! {
    static AFTER_PRIVATE_LOCAL_ETC_CREATED: std::cell::RefCell<Option<Box<dyn FnOnce(&std::path::Path)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_FIRST_LOCAL_ETC_WITNESS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_after_private_local_etc_created(hook: impl FnOnce(&std::path::Path) + 'static) {
    AFTER_PRIVATE_LOCAL_ETC_CREATED.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn arm_after_first_local_etc_witness(hook: impl FnOnce() + 'static) {
    AFTER_FIRST_LOCAL_ETC_WITNESS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_private_local_etc_created(path: &std::path::Path) {
    AFTER_PRIVATE_LOCAL_ETC_CREATED.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(path);
        }
    });
}

#[cfg(not(test))]
fn after_private_local_etc_created(_path: &std::path::Path) {}

#[cfg(test)]
fn after_first_local_etc_witness() {
    AFTER_FIRST_LOCAL_ETC_WITNESS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_first_local_etc_witness() {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt as _, symlink},
        process::{Command, Stdio},
    };

    use super::*;

    fn fixture() -> (tempfile::TempDir, Installation) {
        let temporary = tempfile::tempdir().unwrap();
        crate::test_support::prepare_private_installation_root(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        (temporary, installation)
    }

    fn assert_proof_failure<T>(result: Result<T, super::super::Error>) {
        let error = result.err().expect("local /etc proof unexpectedly succeeded");
        assert!(
            matches!(error, super::super::Error::LiveActiveStateProof { .. }),
            "{error:#?}"
        );
    }

    #[test]
    fn created_local_etc_is_normalized_and_authenticated() {
        const CHILD: &str = "CAST_PRIVATE_LOCAL_ETC_UMASK_CHILD";
        const TEST: &str = "client::transaction_root::tests::created_local_etc_is_normalized_and_authenticated";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--test-threads=1")
                .env(CHILD, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "restrictive-umask child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let (_temporary, installation) = fixture();
        let path = installation.root.join("etc");
        // SAFETY: this exact test runs alone in its dedicated child process.
        let previous = unsafe { nix::libc::umask(0o077) };
        let retained = prepare_local_etc(&installation).unwrap();
        // SAFETY: restore the child process umask before any further work.
        let retained_umask = unsafe { nix::libc::umask(previous) };
        assert_eq!(retained_umask, 0o077, "private creation changed the parent umask");
        retained.revalidate(&installation).unwrap();

        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o7777, 0o755);
    }

    #[test]
    fn private_name_substitution_is_rejected_without_chmodding_the_replacement() {
        let (_temporary, installation) = fixture();
        let displaced = installation.root.join("private-etc-displaced");
        let hook_displaced = displaced.clone();
        arm_after_private_local_etc_created(move |private| {
            fs::rename(private, &hook_displaced).unwrap();
            fs::create_dir(private).unwrap();
            fs::set_permissions(private, fs::Permissions::from_mode(0o700)).unwrap();
        });

        assert_proof_failure(prepare_local_etc(&installation));
        assert!(!installation.root.join("etc").exists());
        assert!(displaced.is_dir());
        assert_eq!(fs::metadata(&displaced).unwrap().permissions().mode() & 0o7777, 0o755);
        let replacement = fs::read_dir(&installation.root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".cast-local-etc-"))
            })
            .expect("substituted private name must remain present");
        assert_eq!(fs::metadata(replacement).unwrap().permissions().mode() & 0o7777, 0o700);
    }

    #[test]
    fn preexisting_group_writable_or_symlink_local_etc_is_preserved_and_rejected() {
        let (_temporary, installation) = fixture();
        let path = installation.root.join("etc");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o775)).unwrap();
        fs::write(path.join("sentinel"), b"local config").unwrap();

        assert_proof_failure(prepare_local_etc(&installation));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o775);
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"local config");

        let (_temporary, installation) = fixture();
        let path = installation.root.join("etc");
        let target = installation.root.join("etc-target");
        fs::create_dir(&target).unwrap();
        symlink(&target, &path).unwrap();

        assert_proof_failure(prepare_local_etc(&installation));
        assert_eq!(fs::read_link(path).unwrap(), target);
    }

    #[test]
    fn final_name_substitution_during_local_etc_proof_is_rejected() {
        let (_temporary, installation) = fixture();
        let path = installation.root.join("etc");
        let displaced = installation.root.join("etc-displaced");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(path.join("sentinel"), b"retained config").unwrap();

        let raced_path = path.clone();
        let raced_displaced = displaced.clone();
        arm_after_first_local_etc_witness(move || {
            fs::rename(&raced_path, &raced_displaced).unwrap();
            fs::create_dir(&raced_path).unwrap();
            fs::set_permissions(&raced_path, fs::Permissions::from_mode(0o755)).unwrap();
        });

        assert_proof_failure(require_local_etc(&installation));
        assert_eq!(fs::read(displaced.join("sentinel")).unwrap(), b"retained config");
        assert!(path.is_dir());
    }
}
