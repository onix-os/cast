//! Descriptor-anchored container view for all stateful system triggers.

use std::{
    ffi::CStr,
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

use container::Container;

use super::{
    Error, TRANSACTION_PSEUDO_FILESYSTEMS, TRANSACTION_ROOT_FILESYSTEM,
    anchored_locators::{beneath_installation_directory, exact_directory},
};
use crate::Installation;

const MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];
const MAX_INTERRUPTS: usize = 1_024;

pub(super) fn container(
    installation: &Installation,
    isolation_root: &crate::client::RetainedRootAbi,
    local_etc: &crate::client::transaction_root::RetainedLocalEtc,
    retained_usr: &std::fs::File,
    live_usr_path: &Path,
) -> Result<Container, Error> {
    // This is a tree-level capability boundary, not a claim that each handler
    // executable is pinned independently. The scratch root also does not
    // inherit ambient live mounts such as the variable-data, runtime, or boot
    // trees; any such
    // exposure requires a separate explicit retained bind.
    revalidate(installation, isolation_root, local_etc, retained_usr, live_usr_path)?;
    let isolation_path = isolation_root.path();
    let isolation = isolation_root.directory();
    for target in MOUNT_TARGETS {
        ensure_mount_target(isolation, target, isolation_path)?;
    }
    revalidate(installation, isolation_root, local_etc, retained_usr, live_usr_path)?;

    let root_locator = exact_directory(isolation_path, isolation)
        .map_err(|source| pin_error("container root", isolation_path, source))?;
    let etc_path = local_etc.path();
    let etc_locator = beneath_installation_directory(installation, etc_path, local_etc.directory())
        .map_err(|source| pin_error("installation /etc", etc_path, source))?;
    let usr_locator = retained_usr_locator(installation, retained_usr, live_usr_path)?;

    let container = Container::new_anchored(root_locator)
        .map_err(|source| pin_error("container root", isolation_path, source))?
        .networking(false)
        .root_filesystem(TRANSACTION_ROOT_FILESYSTEM)
        .pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS)
        .bind_rw_pinned(etc_locator, "/etc")
        .map_err(|source| pin_error("installation /etc", etc_path, source))?
        .bind_rw_pinned(usr_locator, "/usr")
        .map_err(|source| pin_error("installation /usr", live_usr_path, source))?;

    revalidate(installation, isolation_root, local_etc, retained_usr, live_usr_path)?;
    Ok(container.work_dir("/"))
}

pub(super) fn revalidate(
    installation: &Installation,
    isolation_root: &crate::client::RetainedRootAbi,
    local_etc: &crate::client::transaction_root::RetainedLocalEtc,
    retained_usr: &std::fs::File,
    live_usr_path: &Path,
) -> Result<(), Error> {
    isolation_root
        .revalidate()
        .map_err(|source| pin_error("container root", isolation_root.path(), io::Error::other(source)))?;
    local_etc
        .revalidate_mutable(installation)
        .map_err(|source| pin_error("installation /etc", local_etc.path(), io::Error::other(source)))?;
    drop(retained_usr_locator(installation, retained_usr, live_usr_path)?);
    isolation_root
        .revalidate()
        .map_err(|source| pin_error("container root", isolation_root.path(), io::Error::other(source)))?;
    local_etc
        .revalidate_mutable(installation)
        .map_err(|source| pin_error("installation /etc", local_etc.path(), io::Error::other(source)))?;
    drop(retained_usr_locator(installation, retained_usr, live_usr_path)?);
    Ok(())
}

fn retained_usr_locator(
    installation: &Installation,
    retained_usr: &std::fs::File,
    live_usr_path: &Path,
) -> Result<container::AnchoredLocator, Error> {
    let canonical = installation.root.join("usr");
    if live_usr_path != canonical {
        return Err(pin_error(
            "installation /usr",
            live_usr_path,
            io::Error::new(io::ErrorKind::InvalidInput, "system trigger /usr path is not canonical"),
        ));
    }
    beneath_installation_directory(installation, live_usr_path, retained_usr)
        .map_err(|source| pin_error("installation /usr", live_usr_path, source))
}

fn ensure_mount_target(isolation: &std::fs::File, name: &CStr, root: &Path) -> Result<(), Error> {
    let path = root.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let mut interruptions = 0usize;
    loop {
        // SAFETY: `isolation` and the static single-component C string remain
        // live for the call. mkdirat never follows the final component.
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
            _ => return Err(Error::PrepareSystemTriggerMountTarget { path, source }),
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
    .map_err(|source| Error::PrepareSystemTriggerMountTarget { path, source })
}

fn pin_error(role: &'static str, path: &Path, source: io::Error) -> Error {
    Error::PinSystemTriggerSource {
        role,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_ACTIVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_PAYLOAD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_activation(hook: impl FnOnce() + 'static) {
    BEFORE_ACTIVATION.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "system trigger activation hook already armed");
    });
}

#[cfg(test)]
fn arm_after_payload(hook: impl FnOnce() + 'static) {
    AFTER_PAYLOAD.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "system trigger post-payload hook already armed");
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

#[cfg(test)]
pub(super) fn after_payload() {
    AFTER_PAYLOAD.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_activation() {}

#[cfg(not(test))]
pub(super) fn after_payload() {}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io, os::unix::fs::PermissionsExt as _, path::PathBuf};

    use fs_err as fs;
    use triggers::format::Handler;

    use super::*;
    use crate::client::postblit::{TriggerRunner, TriggerScope};

    #[derive(Clone, Copy, Debug)]
    enum Substitution {
        IsolationRoot,
        Etc,
        Usr,
    }

    #[test]
    fn stateful_system_policy_is_read_only_and_exposes_only_bounded_kernel_views() {
        assert_eq!(MOUNT_TARGETS, [c"etc", c"usr", c"proc", c"tmp", c"dev"]);
        assert_eq!(TRANSACTION_ROOT_FILESYSTEM, container::RootFilesystemPolicy::ReadOnly);
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.proc, container::ProcPolicy::ReadOnly);
        assert!(matches!(
            TRANSACTION_PSEUDO_FILESYSTEMS.tmp,
            container::TmpPolicy::Bounded(limits)
                if limits.size_bytes() == 256 * 1024 * 1024 && limits.inodes() == 65_536
        ));
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.sys, container::SysPolicy::None);
        assert_eq!(TRANSACTION_PSEUDO_FILESYSTEMS.dev, container::DevPolicy::Minimal);
    }

    #[test]
    fn system_container_denies_root_tree_writes_and_exposes_only_declared_views() {
        let fixture = SystemContainerFixture::new();
        let isolation = fixture.container().unwrap();

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
                Ok(_) => return Err(io::Error::other("system trigger unexpectedly inherited sysfs")),
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

        if activation_completed(result, "stateful system policy") {
            assert_eq!(fs::read(fixture.installation.root.join("usr/system-write")).unwrap(), b"system usr");
            assert_eq!(fs::read(fixture.installation.root.join("etc/system-write")).unwrap(), b"system etc");
            assert!(!fixture.isolation_root.path().join("undeclared-root-write").exists());
            assert!(!fixture.isolation_root.path().join("tmp/system-write").exists());
        }
    }

    #[test]
    fn retained_system_capabilities_reject_preconstruction_substitutions() {
        for substitution in [Substitution::IsolationRoot, Substitution::Etc, Substitution::Usr] {
            let fixture = SystemContainerFixture::new();
            let (target, detached) = substitution_paths(&fixture, substitution);
            replace_public_target(&target, &detached, substitution);

            let error = match fixture.container() {
                Ok(_) => panic!("{substitution:?} replacement was accepted before construction"),
                Err(error) => error,
            };
            assert_pin_failure(error, substitution, &target);
            assert_eq!(fs::read(target.join("replacement-witness")).unwrap(), b"foreign replacement");
            assert!(detached.is_dir());
        }
    }

    #[test]
    fn anchored_activation_rejects_substitutions_before_payload_mutation() {
        for substitution in [Substitution::IsolationRoot, Substitution::Etc, Substitution::Usr] {
            assert_activation_substitution_fails_closed(substitution);
        }
    }

    #[test]
    fn post_payload_revalidation_rejects_all_public_identity_swaps() {
        for substitution in [Substitution::IsolationRoot, Substitution::Etc, Substitution::Usr] {
            assert_post_payload_substitution_is_detected(substitution);
        }
    }

    fn assert_activation_substitution_fails_closed(substitution: Substitution) {
        let fixture = SystemContainerFixture::new();
        fs::write(fixture.installation.root.join("usr/payload-witness"), b"payload must not run").unwrap();
        fs::write(
            fixture.installation.isolation_dir().join("retained-witness"),
            b"retained isolation",
        )
        .unwrap();

        let (target, detached) = substitution_paths(&fixture, substitution);
        let hook_target = target.clone();
        let hook_detached = detached.clone();
        arm_before_activation(move || {
            replace_public_target(&hook_target, &hook_detached, substitution);
        });

        let result = fixture.runner().execute();

        assert!(detached.is_dir(), "{substitution:?} substitution hook did not run");
        assert_activation_and_revalidation_failure(result, substitution, &target);
        assert_eq!(
            fs::read(retained_payload_path(&fixture, substitution, &detached)).unwrap(),
            b"payload must not run"
        );
        if matches!(substitution, Substitution::Usr) {
            assert_eq!(fs::read(target.join("payload-witness")).unwrap(), b"foreign payload");
        }
        assert_eq!(
            fs::read(target.join("replacement-witness")).unwrap(),
            b"foreign replacement"
        );
    }

    fn assert_post_payload_substitution_is_detected(substitution: Substitution) {
        let fixture = SystemContainerFixture::new();
        fs::write(fixture.installation.root.join("usr/payload-witness"), b"payload may run once").unwrap();
        let (target, detached) = substitution_paths(&fixture, substitution);
        let hook_target = target.clone();
        let hook_detached = detached.clone();
        arm_after_payload(move || replace_public_target(&hook_target, &hook_detached, substitution));

        let result = fixture.runner().execute();

        assert!(detached.is_dir(), "{substitution:?} post-payload substitution hook did not run");
        let payload_completed = assert_post_payload_revalidation_failure(result, substitution, &target);
        let retained_payload = retained_payload_path(&fixture, substitution, &detached);
        assert_eq!(retained_payload.exists(), !payload_completed);
        assert_eq!(fs::read(target.join("replacement-witness")).unwrap(), b"foreign replacement");
        if matches!(substitution, Substitution::Usr) {
            assert_eq!(fs::read(target.join("payload-witness")).unwrap(), b"foreign payload");
        }
    }

    struct SystemContainerFixture {
        _temporary: tempfile::TempDir,
        installation: Installation,
        isolation_root: crate::client::RetainedRootAbi,
        local_etc: crate::client::transaction_root::RetainedLocalEtc,
        retained_usr: std::fs::File,
        live_usr_path: PathBuf,
    }

    impl SystemContainerFixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            crate::test_support::prepare_private_installation_root(temporary.path());
            let installation = Installation::open(temporary.path(), None).unwrap();
            let local_etc = crate::client::transaction_root::prepare_local_etc(&installation).unwrap();
            create_safe_directory(&installation.root.join("usr"));
            fs::write(installation.root.join("etc/retained-witness"), b"retained etc").unwrap();
            fs::write(installation.root.join("usr/retained-witness"), b"retained usr").unwrap();
            let isolation_root = crate::client::create_root_links(&installation.isolation_dir()).unwrap();
            let live_usr_path = installation.root.join("usr");
            let retained_usr = std::fs::File::open(&live_usr_path).unwrap();
            Self {
                _temporary: temporary,
                installation,
                isolation_root,
                local_etc,
                retained_usr,
                live_usr_path,
            }
        }

        fn container(&self) -> Result<Container, Error> {
            container(
                &self.installation,
                &self.isolation_root,
                &self.local_etc,
                &self.retained_usr,
                &self.live_usr_path,
            )
        }

        fn runner(&self) -> TriggerRunner<'_> {
            let matched = fnmatch::Match {
                path: "/usr/payload-witness".to_owned(),
                variables: BTreeMap::new(),
            };
            TriggerRunner {
                scope: TriggerScope::System {
                    installation: &self.installation,
                    isolation_root: &self.isolation_root,
                    local_etc: &self.local_etc,
                    retained_usr: &self.retained_usr,
                    live_usr_path: &self.live_usr_path,
                },
                trigger: Handler::Delete {
                    delete: vec!["/usr/payload-witness".to_owned()],
                }
                .compiled(&matched),
            }
        }
    }

    fn substitution_paths(fixture: &SystemContainerFixture, substitution: Substitution) -> (PathBuf, PathBuf) {
        match substitution {
            Substitution::IsolationRoot => (
                fixture.installation.isolation_dir(),
                fixture.installation.root_path("detached-system-isolation"),
            ),
            Substitution::Etc => (
                fixture.installation.root.join("etc"),
                fixture.installation.root.join("detached-system-etc"),
            ),
            Substitution::Usr => (
                fixture.installation.root.join("usr"),
                fixture.installation.root.join("detached-system-usr"),
            ),
        }
    }

    fn replace_public_target(target: &Path, detached: &Path, substitution: Substitution) {
        fs::rename(target, detached).unwrap();
        create_safe_directory(target);
        fs::write(target.join("replacement-witness"), b"foreign replacement").unwrap();
        if matches!(substitution, Substitution::Usr) {
            fs::write(target.join("payload-witness"), b"foreign payload").unwrap();
        }
    }

    fn retained_payload_path(fixture: &SystemContainerFixture, substitution: Substitution, detached: &Path) -> PathBuf {
        if matches!(substitution, Substitution::Usr) {
            detached.join("payload-witness")
        } else {
            fixture.installation.root.join("usr/payload-witness")
        }
    }

    fn assert_activation_and_revalidation_failure(
        result: Result<(), Error>,
        substitution: Substitution,
        target: &Path,
    ) {
        match result {
            Err(Error::SystemTriggerOperationAndRevalidation {
                primary,
                revalidation,
            }) => {
                assert!(matches!(*primary, Error::Container(_)));
                assert_pin_failure(*revalidation, substitution, target);
            }
            other => panic!("{substitution:?} activation did not fail closed: {other:?}"),
        }
    }

    fn assert_post_payload_revalidation_failure(
        result: Result<(), Error>,
        substitution: Substitution,
        target: &Path,
    ) -> bool {
        match result {
            Err(error @ Error::PinSystemTriggerSource { .. }) => {
                assert_pin_failure(error, substitution, target);
                true
            }
            Err(Error::SystemTriggerOperationAndRevalidation {
                primary,
                revalidation,
            }) => {
                assert_pin_failure(*revalidation, substitution, target);
                match *primary {
                    Error::Container(source) if source.execution_capability_unavailable() => false,
                    primary => panic!("unexpected system payload failure before revalidation: {primary:?}"),
                }
            }
            other => panic!("{substitution:?} post-payload swap escaped revalidation: {other:?}"),
        }
    }

    fn assert_pin_failure(error: Error, substitution: Substitution, target: &Path) {
        let expected_role = match substitution {
            Substitution::IsolationRoot => "container root",
            Substitution::Etc => "installation /etc",
            Substitution::Usr => "installation /usr",
        };
        assert!(matches!(
            error,
            Error::PinSystemTriggerSource { role, path, .. }
                if role == expected_role && path == target
        ));
    }

    fn create_safe_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
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
}
