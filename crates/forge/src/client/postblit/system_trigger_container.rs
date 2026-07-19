//! Descriptor-anchored container view for stateful non-live-root system triggers.

use std::{
    ffi::CStr,
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

use container::Container;

use super::{
    Error,
    anchored_locators::{
        beneath_installation_directory, beneath_named_installation_directory, exact_directory,
        exact_named_installation_directory,
    },
};
use crate::Installation;

const MOUNT_TARGETS: [&CStr; 2] = [c"etc", c"usr"];
const MAX_INTERRUPTS: usize = 1_024;

pub(super) fn container(installation: &Installation) -> Result<Container, Error> {
    let isolation_path = installation.isolation_dir();
    let (isolation, initial_root_locator) = exact_named_installation_directory(installation, &isolation_path)
        .map_err(|source| pin_error("container root", &isolation_path, source))?;
    let etc_path = installation.root.join("etc");
    let (etc, initial_etc_locator) = beneath_named_installation_directory(installation, &etc_path)
        .map_err(|source| pin_error("installation /etc", &etc_path, source))?;
    let usr_path = installation.root.join("usr");
    let (usr, initial_usr_locator) = beneath_named_installation_directory(installation, &usr_path)
        .map_err(|source| pin_error("installation /usr", &usr_path, source))?;

    // Authenticate every external name before provisioning fixed mount points.
    // The retained descriptors below then prove that none changed during this
    // descriptor-relative mutation window.
    drop(initial_root_locator);
    drop(initial_etc_locator);
    drop(initial_usr_locator);
    for target in MOUNT_TARGETS {
        ensure_mount_target(&isolation, target, &isolation_path)?;
    }

    let root_locator = exact_directory(&isolation_path, &isolation)
        .map_err(|source| pin_error("container root", &isolation_path, source))?;
    let etc_locator = beneath_installation_directory(installation, &etc_path, &etc)
        .map_err(|source| pin_error("installation /etc", &etc_path, source))?;
    let usr_locator = beneath_installation_directory(installation, &usr_path, &usr)
        .map_err(|source| pin_error("installation /usr", &usr_path, source))?;

    let container = Container::new_anchored(root_locator)
        .map_err(|source| pin_error("container root", &isolation_path, source))?
        .networking(false)
        .bind_rw_pinned(etc_locator, "/etc")
        .map_err(|source| pin_error("installation /etc", &etc_path, source))?
        .bind_rw_pinned(usr_locator, "/usr")
        .map_err(|source| pin_error("installation /usr", &usr_path, source))?;

    Ok(container.work_dir("/"))
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
}

#[cfg(test)]
fn arm_before_activation(hook: impl FnOnce() + 'static) {
    BEFORE_ACTIVATION.with(|slot| {
        let previous = slot.borrow_mut().replace(Box::new(hook));
        assert!(previous.is_none(), "system trigger activation hook already armed");
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
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt as _, path::PathBuf};

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
    fn non_live_system_root_and_source_substitutions_fail_before_payload_mutation() {
        for substitution in [Substitution::IsolationRoot, Substitution::Etc, Substitution::Usr] {
            assert_substitution_fails_closed(substitution);
        }
    }

    fn assert_substitution_fails_closed(substitution: Substitution) {
        let temporary = tempfile::tempdir().unwrap();
        crate::test_support::prepare_private_installation_root(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        fs::create_dir(installation.root.join("etc")).unwrap();
        fs::create_dir(installation.root.join("usr")).unwrap();
        fs::write(installation.root.join("etc/retained-witness"), b"retained etc").unwrap();
        fs::write(installation.root.join("usr/retained-witness"), b"retained usr").unwrap();
        fs::write(installation.root.join("usr/payload-witness"), b"payload must not run").unwrap();
        drop(crate::client::create_root_links(&installation.isolation_dir()).unwrap());
        fs::write(
            installation.isolation_dir().join("retained-witness"),
            b"retained isolation",
        )
        .unwrap();

        let (target, detached) = substitution_paths(&installation, substitution);
        let hook_target = target.clone();
        let hook_detached = detached.clone();
        arm_before_activation(move || {
            fs::rename(&hook_target, &hook_detached).unwrap();
            fs::create_dir(&hook_target).unwrap();
            fs::set_permissions(&hook_target, std::fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(hook_target.join("replacement-witness"), b"foreign replacement").unwrap();
            if matches!(substitution, Substitution::IsolationRoot) {
                fs::create_dir(hook_target.join("etc")).unwrap();
                fs::create_dir(hook_target.join("usr")).unwrap();
            }
            if matches!(substitution, Substitution::Usr) {
                fs::write(hook_target.join("payload-witness"), b"foreign payload").unwrap();
            }
        });

        let matched = fnmatch::Match {
            path: "/usr/payload-witness".to_owned(),
            variables: BTreeMap::new(),
        };
        let trigger = Handler::Delete {
            delete: vec!["/usr/payload-witness".to_owned()],
        }
        .compiled(&matched);
        let result = TriggerRunner {
            scope: TriggerScope::System(&installation),
            trigger,
        }
        .execute();

        assert!(detached.is_dir(), "{substitution:?} substitution hook did not run");
        match substitution {
            Substitution::IsolationRoot => assert!(matches!(
                result,
                Err(Error::Container(container::Error::Failure { message }))
                    if message.contains("reopen anchored container root")
            )),
            Substitution::Etc | Substitution::Usr => assert!(matches!(
                result,
                Err(Error::Container(container::Error::Failure { message }))
                    if message.contains("reopen anchored bind source")
            )),
        }
        assert_eq!(
            fs::read(retained_payload_path(&installation, substitution, &detached)).unwrap(),
            b"payload must not run"
        );
        if matches!(substitution, Substitution::Usr) {
            assert_eq!(fs::read(target.join("payload-witness")).unwrap(), b"foreign payload");
        }
        assert_eq!(
            fs::read(target.join("replacement-witness")).unwrap(),
            b"foreign replacement"
        );
        let (retained_witness, expected) = match substitution {
            Substitution::IsolationRoot => (detached.join("retained-witness"), b"retained isolation".as_slice()),
            Substitution::Etc => (detached.join("retained-witness"), b"retained etc".as_slice()),
            Substitution::Usr => (detached.join("retained-witness"), b"retained usr".as_slice()),
        };
        assert_eq!(fs::read(retained_witness).unwrap(), expected);
    }

    fn substitution_paths(installation: &Installation, substitution: Substitution) -> (PathBuf, PathBuf) {
        match substitution {
            Substitution::IsolationRoot => (
                installation.isolation_dir(),
                installation.root_path("detached-system-isolation"),
            ),
            Substitution::Etc => (
                installation.root.join("etc"),
                installation.root.join("detached-system-etc"),
            ),
            Substitution::Usr => (
                installation.root.join("usr"),
                installation.root.join("detached-system-usr"),
            ),
        }
    }

    fn retained_payload_path(installation: &Installation, substitution: Substitution, detached: &Path) -> PathBuf {
        if matches!(substitution, Substitution::Usr) {
            detached.join("payload-witness")
        } else {
            installation.root.join("usr/payload-witness")
        }
    }
}
