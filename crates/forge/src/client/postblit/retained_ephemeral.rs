//! Descriptor-pinned execution context for external ephemeral triggers.

use std::{
    ffi::CStr,
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

use container::{AnchoredLocator, Container};

use super::{
    Error, RetainedEphemeralPhase, TRANSACTION_PSEUDO_FILESYSTEMS, TRANSACTION_ROOT_FILESYSTEM,
    anchored_locators::{beneath_directory, exact_directory},
};
use crate::{
    Installation,
    client::{RetainedRootAbi, external_materialization::RetainedEphemeralTriggerView},
};

const TRANSACTION_MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];
const SYSTEM_MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];
const MAX_INTERRUPTS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateBindPolicy {
    etc_read_only: bool,
    usr_read_only: bool,
}

const fn candidate_bind_policy(phase: RetainedEphemeralPhase) -> CandidateBindPolicy {
    match phase {
        RetainedEphemeralPhase::Transaction => CandidateBindPolicy {
            etc_read_only: true,
            usr_read_only: false,
        },
        RetainedEphemeralPhase::System => CandidateBindPolicy {
            etc_read_only: false,
            usr_read_only: false,
        },
    }
}

pub(super) fn container(
    phase: RetainedEphemeralPhase,
    installation: &Installation,
    isolation_root: &RetainedRootAbi,
    view: RetainedEphemeralTriggerView<'_>,
) -> Result<Container, Error> {
    revalidate(installation, isolation_root, view)?;
    let isolation_path = isolation_root.path();
    let targets = match phase {
        RetainedEphemeralPhase::Transaction => &TRANSACTION_MOUNT_TARGETS[..],
        RetainedEphemeralPhase::System => &SYSTEM_MOUNT_TARGETS[..],
    };
    for target in targets {
        ensure_mount_target(isolation_root.directory(), target, isolation_path)?;
    }

    let (usr, usr_path) = view.usr();
    let (etc, etc_path) = view.etc();
    let (candidate_root, candidate_root_path) = view.root();
    let policy = candidate_bind_policy(phase);
    let root_locator = exact_directory(isolation_path, isolation_root.directory())
        .map_err(|source| pin_error("container root", isolation_path, source))?;
    let etc_locator = beneath_directory(candidate_root_path, candidate_root, Path::new("etc"), etc)
        .map_err(|source| pin_error("external candidate /etc", etc_path, source))?;
    let usr_locator = beneath_directory(candidate_root_path, candidate_root, Path::new("usr"), usr)
        .map_err(|source| pin_error("external candidate /usr", usr_path, source))?;
    let base = Container::new_anchored(root_locator)
        .map_err(|source| pin_error("container root", isolation_path, source))?
        .networking(false)
        .root_filesystem(TRANSACTION_ROOT_FILESYSTEM)
        .pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS);
    let base = bind_pinned(base, etc_locator, "/etc", policy.etc_read_only)
        .map_err(|source| pin_error("external candidate /etc", etc_path, source))?;
    let container = bind_pinned(base, usr_locator, "/usr", policy.usr_read_only)
        .map_err(|source| pin_error("external candidate /usr", usr_path, source))?;

    revalidate(installation, isolation_root, view)?;
    Ok(container.work_dir("/"))
}

pub(super) fn revalidate(
    installation: &Installation,
    isolation_root: &RetainedRootAbi,
    view: RetainedEphemeralTriggerView<'_>,
) -> Result<(), Error> {
    view.revalidate(installation).map_err(|source| {
        pin_error(
            "external candidate trigger view",
            view.root_path(),
            io::Error::other(source),
        )
    })?;
    isolation_root
        .revalidate()
        .map_err(|source| pin_error("container root", isolation_root.path(), io::Error::other(source)))?;
    view.revalidate(installation).map_err(|source| {
        pin_error(
            "external candidate trigger view",
            view.root_path(),
            io::Error::other(source),
        )
    })
}

fn ensure_mount_target(isolation: &std::fs::File, name: &CStr, root: &Path) -> Result<(), Error> {
    let path = root.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let mut interruptions = 0usize;
    loop {
        // SAFETY: the retained root and static single-component name remain
        // live; mkdirat neither follows nor replaces the final component.
        if unsafe { nix::libc::mkdirat(isolation.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted if interruptions < MAX_INTERRUPTS => {
                interruptions += 1;
                continue;
            }
            io::ErrorKind::AlreadyExists => break,
            _ => return Err(Error::PrepareRetainedEphemeralMountTarget { path, source }),
        }
    }

    crate::linux_fs::openat2_file(
        isolation.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        crate::linux_fs::controlled_resolution(),
    )
    .map(|_| ())
    .map_err(|source| Error::PrepareRetainedEphemeralMountTarget { path, source })
}

fn pin_error(role: &'static str, path: &Path, source: io::Error) -> Error {
    Error::PinRetainedEphemeralSource {
        role,
        path: path.to_owned(),
        source,
    }
}

fn bind_pinned(container: Container, source: AnchoredLocator, guest: &str, read_only: bool) -> io::Result<Container> {
    if read_only {
        container.bind_ro_pinned(source, guest)
    } else {
        container.bind_rw_pinned(source, guest)
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_ACTIVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_activation(hook: impl FnOnce() + 'static) {
    BEFORE_ACTIVATION.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "retained ephemeral activation hook already armed");
    });
}

#[cfg(test)]
pub(super) fn before_activation() {
    BEFORE_ACTIVATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_activation() {}

#[cfg(test)]
mod tests {
    use std::{
        io,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        rc::Rc,
    };

    use fs_err as fs;

    use super::*;
    use crate::client::{
        AssetMaterialization, BlitExecution, candidate_metadata::RetainedEphemeralUsr,
        external_materialization::RetainedExternalMaterializationTarget,
    };

    #[test]
    fn retained_ephemeral_phase_policies_keep_transaction_etc_read_only() {
        assert_eq!(TRANSACTION_MOUNT_TARGETS, [c"etc", c"usr", c"proc", c"tmp", c"dev"]);
        assert_eq!(SYSTEM_MOUNT_TARGETS, [c"etc", c"usr", c"proc", c"tmp", c"dev"]);
        assert_eq!(TRANSACTION_ROOT_FILESYSTEM, container::RootFilesystemPolicy::ReadOnly);
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.proc, container::ProcPolicy::ReadOnly);
        assert!(matches!(
            TRANSACTION_PSEUDO_FILESYSTEMS.tmp,
            container::TmpPolicy::Bounded(limits)
                if limits.size_bytes() == 256 * 1024 * 1024 && limits.inodes() == 65_536
        ));
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.sys, container::SysPolicy::None);
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.dev, container::DevPolicy::Minimal);
        assert_eq!(
            candidate_bind_policy(RetainedEphemeralPhase::Transaction),
            CandidateBindPolicy {
                etc_read_only: true,
                usr_read_only: false,
            }
        );
        assert_eq!(
            candidate_bind_policy(RetainedEphemeralPhase::System),
            CandidateBindPolicy {
                etc_read_only: false,
                usr_read_only: false,
            }
        );
    }

    #[test]
    fn transaction_container_mounts_usr_read_write_and_etc_read_only() {
        let mut fixture = EphemeralContainerFixture::new();
        let isolation = fixture.container(RetainedEphemeralPhase::Transaction).unwrap();

        let result = isolation.run(|| {
            fs::write("/usr/transaction-write", b"transaction usr")?;
            match fs::write("/etc/transaction-write", b"forbidden") {
                Ok(()) => Err(io::Error::other("transaction /etc unexpectedly accepted a write")),
                Err(source)
                    if matches!(
                        source.raw_os_error(),
                        Some(nix::libc::EROFS | nix::libc::EACCES | nix::libc::EPERM)
                    ) =>
                {
                    Ok(())
                }
                Err(source) => Err(source),
            }
        });

        if activation_completed(result, "retained ephemeral transaction access") {
            assert_eq!(
                fs::read(fixture.root.join("usr/transaction-write")).unwrap(),
                b"transaction usr"
            );
            assert!(!fixture.root.join("etc/transaction-write").exists());
        }
    }

    #[test]
    fn system_container_uses_writable_candidate_binds_inside_a_read_only_minimal_root() {
        let mut fixture = EphemeralContainerFixture::new();
        let isolation = fixture.container(RetainedEphemeralPhase::System).unwrap();

        let result = isolation.run(|| {
            fs::write("/usr/system-write", b"system usr")?;
            fs::write("/etc/system-write", b"system etc")?;
            fs::write("/tmp/system-write", b"bounded tmp")?;

            let root_error = fs::write("/undeclared-root-write", b"forbidden")
                .expect_err("read-only scratch root accepted an undeclared write");
            require_write_denial(root_error, "scratch root")?;

            match fs::symlink_metadata("/sys") {
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(source),
                Ok(_) => return Err(io::Error::other("ephemeral system trigger unexpectedly inherited sysfs")),
            }

            let mut devices = fs::read_dir("/dev")?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<Result<Vec<_>, _>>()?;
            devices.sort();
            if devices != ["full", "null", "zero"] {
                return Err(io::Error::other(format!("unexpected minimal device view: {devices:?}")));
            }
            Ok::<(), io::Error>(())
        });

        if activation_completed(result, "retained ephemeral system access") {
            assert_eq!(fs::read(fixture.root.join("usr/system-write")).unwrap(), b"system usr");
            assert_eq!(fs::read(fixture.root.join("etc/system-write")).unwrap(), b"system etc");
            assert!(!fixture.isolation_root.path().join("undeclared-root-write").exists());
            assert!(!fixture.isolation_root.path().join("tmp/system-write").exists());
        }
    }

    #[test]
    fn public_root_usr_and_etc_substitution_fails_closed_before_payload() {
        let mut fixture = EphemeralContainerFixture::new();
        fs::write(fixture.root.join("usr/source-witness"), b"retained usr").unwrap();
        fs::write(fixture.root.join("etc/source-witness"), b"retained etc").unwrap();
        let (usr_identity, etc_identity) = fixture.retained_identities();
        let isolation = fixture.container(RetainedEphemeralPhase::System).unwrap();

        let root = fixture.root.clone();
        let detached_root = fixture.parent.join("detached-root");
        let hook_root = root.clone();
        let hook_detached_root = detached_root.clone();
        let hook_ran = Rc::new(std::cell::Cell::new(false));
        let hook_observation = Rc::clone(&hook_ran);
        arm_before_activation(move || {
            hook_observation.set(true);
            fs::rename(hook_root.join("usr"), hook_root.join("retained-usr")).unwrap();
            create_safe_directory(&hook_root.join("usr"));
            fs::write(hook_root.join("usr/source-witness"), b"foreign nested usr").unwrap();
            fs::rename(hook_root.join("etc"), hook_root.join("retained-etc")).unwrap();
            create_safe_directory(&hook_root.join("etc"));
            fs::write(hook_root.join("etc/source-witness"), b"foreign nested etc").unwrap();

            fs::rename(&hook_root, &hook_detached_root).unwrap();
            create_safe_directory(&hook_root);
            create_safe_directory(&hook_root.join("usr"));
            create_safe_directory(&hook_root.join("etc"));
            fs::write(hook_root.join("usr/source-witness"), b"foreign public usr").unwrap();
            fs::write(hook_root.join("etc/source-witness"), b"foreign public etc").unwrap();
        });
        before_activation();

        assert!(hook_ran.get(), "pre-activation substitution hook did not run");
        assert_eq!(directory_identity(&detached_root.join("retained-usr")), usr_identity);
        assert_eq!(directory_identity(&detached_root.join("retained-etc")), etc_identity);
        assert_eq!(
            fs::read(root.join("usr/source-witness")).unwrap(),
            b"foreign public usr"
        );
        assert_eq!(
            fs::read(root.join("etc/source-witness")).unwrap(),
            b"foreign public etc"
        );

        let result = isolation.run(|| {
            if fs::read("/usr/source-witness")? != b"retained usr" {
                return Err(io::Error::other("system /usr bind was redirected"));
            }
            if fs::read("/etc/source-witness")? != b"retained etc" {
                return Err(io::Error::other("system /etc bind was redirected"));
            }
            fs::write("/usr/pinned-write", b"pinned usr")?;
            fs::write("/etc/pinned-write", b"pinned etc")?;
            Ok::<(), io::Error>(())
        });

        assert!(result.is_err(), "substituted source locators reached the payload");
        assert!(!detached_root.join("retained-usr/pinned-write").exists());
        assert!(!detached_root.join("retained-etc/pinned-write").exists());
        assert!(!root.join("usr/pinned-write").exists());
        assert!(!root.join("etc/pinned-write").exists());
        assert_eq!(
            fs::read(detached_root.join("usr/source-witness")).unwrap(),
            b"foreign nested usr"
        );
        assert_eq!(
            fs::read(detached_root.join("etc/source-witness")).unwrap(),
            b"foreign nested etc"
        );
    }

    #[test]
    fn container_rejects_an_isolation_root_substitution() {
        let mut fixture = EphemeralContainerFixture::new();
        let isolation_path = fixture.installation.isolation_dir();
        let detached = fixture.installation.root_path("detached-ephemeral-isolation");
        fs::rename(&isolation_path, &detached).unwrap();
        create_safe_directory(&isolation_path);
        fs::write(isolation_path.join("foreign-root"), b"foreign").unwrap();

        let error = match fixture.container(RetainedEphemeralPhase::System) {
            Ok(_) => panic!("replacement isolation root was accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::PinRetainedEphemeralSource {
                role: "container root",
                path,
                ..
            } if path == isolation_path
        ));
        assert_eq!(fs::read(isolation_path.join("foreign-root")).unwrap(), b"foreign");
        assert_eq!(fs::read_link(detached.join("bin")).unwrap(), Path::new("usr/bin"));
    }

    struct EphemeralContainerFixture {
        _temporary: tempfile::TempDir,
        installation: Installation,
        parent: PathBuf,
        root: PathBuf,
        target: RetainedExternalMaterializationTarget,
        candidate_usr: RetainedEphemeralUsr,
        isolation_root: RetainedRootAbi,
    }

    impl EphemeralContainerFixture {
        fn new() -> Self {
            let temporary = crate::test_support::private_installation_tempdir();
            let installation_root = temporary.path().join("installation");
            create_safe_directory(&installation_root);
            let installation = Installation::open(&installation_root, None).unwrap();
            let parent = temporary.path().join("external");
            create_safe_directory(&parent);
            let root = parent.join("root");
            let mut target = RetainedExternalMaterializationTarget::prepare(&installation, &root).unwrap();
            let tree = crate::client::vfs(Vec::new()).unwrap();
            let candidate_usr = target
                .materialize(
                    &installation,
                    &tree,
                    AssetMaterialization::IndependentCopy,
                    BlitExecution::Sequential,
                )
                .unwrap();
            target.create_root_abi(&installation, &candidate_usr).unwrap();
            let isolation_root = crate::client::create_root_links(&installation.isolation_dir()).unwrap();
            target
                .prepare_trigger_view(&installation, &candidate_usr)
                .unwrap()
                .revalidate(&installation)
                .unwrap();

            Self {
                _temporary: temporary,
                installation,
                parent,
                root,
                target,
                candidate_usr,
                isolation_root,
            }
        }

        fn container(&mut self, phase: RetainedEphemeralPhase) -> Result<Container, Error> {
            let view = self
                .target
                .prepare_trigger_view(&self.installation, &self.candidate_usr)
                .unwrap();
            container(phase, &self.installation, &self.isolation_root, view)
        }

        fn retained_identities(&mut self) -> ((u64, u64), (u64, u64)) {
            let view = self
                .target
                .prepare_trigger_view(&self.installation, &self.candidate_usr)
                .unwrap();
            (descriptor_identity(view.usr().0), descriptor_identity(view.etc().0))
        }
    }

    fn create_safe_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn directory_identity(path: &Path) -> (u64, u64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }

    fn descriptor_identity(file: &std::fs::File) -> (u64, u64) {
        let metadata = file.metadata().unwrap();
        (metadata.dev(), metadata.ino())
    }

    fn activation_completed(result: Result<(), container::Error>, context: &str) -> bool {
        match result {
            Ok(()) => true,
            Err(error) if error.execution_capability_unavailable() => {
                eprintln!("SKIP {context}: host denied mandatory container capability: {error}");
                false
            }
            Err(error) => panic!("{context} activation failed: {error:?}"),
        }
    }

    fn require_write_denial(source: io::Error, path_role: &str) -> io::Result<()> {
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::EROFS | nix::libc::EACCES | nix::libc::EPERM)
        ) {
            Ok(())
        } else {
            Err(io::Error::other(format!("unexpected {path_role} write result: {source}")))
        }
    }
}
