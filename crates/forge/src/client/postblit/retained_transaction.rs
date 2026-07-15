//! Descriptor-pinned execution context for inactive-state transaction triggers.

use std::{
    ffi::CStr,
    io,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::Path,
};

use container::Container;

use super::{Error, TRANSACTION_PSEUDO_FILESYSTEMS, TRANSACTION_ROOT_FILESYSTEM};
use crate::Installation;

const MOUNT_TARGETS: [&CStr; 5] = [c"etc", c"usr", c"proc", c"tmp", c"dev"];
const MAX_INTERRUPTS: usize = 1_024;

/// Prepare an inactive repair trigger container without resolving any bind
/// source through a mutable pathname.
///
/// `Container` duplicates all three capabilities while this function runs.
/// A later fixed-staging or isolation-name substitution therefore cannot
/// redirect the writable `/usr` bind or either read-only execution source.
pub(super) fn container(
    installation: &Installation,
    isolation_root: &crate::client::RetainedRootAbi,
    local_etc: &crate::client::transaction_root::RetainedLocalEtc,
    candidate_usr: &std::fs::File,
    candidate_usr_path: &Path,
) -> Result<Container, Error> {
    let isolation_path = isolation_root.path();
    let isolation = isolation_root.directory();
    revalidate_isolation_root(isolation_root)?;
    for target in MOUNT_TARGETS {
        ensure_mount_target(isolation, target, isolation_path)?;
    }

    local_etc
        .revalidate(installation)
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "installation /etc",
            path: local_etc.path().to_owned(),
            source: io::Error::other(source),
        })?;

    let container = Container::new_anchored(isolation_path, isolation)
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "container root",
            path: isolation_path.to_owned(),
            source,
        })?
        .networking(false)
        .root_filesystem(TRANSACTION_ROOT_FILESYSTEM)
        .pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS)
        .bind_ro_pinned(local_etc.directory(), local_etc.path(), "/etc")
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "installation /etc",
            path: local_etc.path().to_owned(),
            source,
        })?
        .bind_rw_pinned(candidate_usr, candidate_usr_path, "/usr")
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "candidate /usr",
            path: candidate_usr_path.to_owned(),
            source,
        })?;

    // Sandwich construction between exact name proofs. Descriptor-pinned
    // activation would remain confined after a name race, but inactive repair
    // should fail closed rather than execute against an already-detached
    // scratch root or local configuration tree.
    revalidate_isolation_root(isolation_root)?;
    local_etc
        .revalidate(installation)
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "installation /etc",
            path: local_etc.path().to_owned(),
            source: io::Error::other(source),
        })?;

    Ok(container.work_dir("/"))
}

fn revalidate_isolation_root(isolation_root: &crate::client::RetainedRootAbi) -> Result<(), Error> {
    isolation_root
        .revalidate()
        .map_err(|source| Error::PinRetainedTransactionSource {
            role: "container root",
            path: isolation_root.path().to_owned(),
            source: io::Error::other(source),
        })
}

fn ensure_mount_target(isolation: &std::fs::File, name: &CStr, root: &Path) -> Result<(), Error> {
    let path = root.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let mut interruptions = 0usize;
    loop {
        // SAFETY: `isolation` and the static, single-component C string remain
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
            _ => {
                return Err(Error::PrepareRetainedTransactionMountTarget { path, source });
            }
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
    .map_err(|source| Error::PrepareRetainedTransactionMountTarget { path, source })
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
        assert!(previous.is_none(), "retained transaction activation hook already armed");
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
        collections::BTreeMap,
        os::{
            fd::AsRawFd as _,
            unix::fs::{MetadataExt as _, PermissionsExt as _},
        },
    };

    use triggers::format::Handler;

    use super::*;
    use crate::client::postblit::{RetainedTransactionKind, TriggerRunner, TriggerScope};

    #[test]
    fn container_rejects_an_isolation_root_replacement_after_abi_provisioning() {
        let temporary = tempfile::tempdir().unwrap();
        crate::test_support::prepare_private_installation_root(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        fs_err::create_dir(installation.root.join("etc")).unwrap();
        fs_err::set_permissions(installation.root.join("etc"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let local_etc = crate::client::transaction_root::require_local_etc(&installation).unwrap();

        let candidate_usr_path = installation.staging_dir().join("usr");
        fs_err::create_dir(&candidate_usr_path).unwrap();
        let candidate_usr = crate::linux_fs::openat2_file(
            installation.root_directory().as_raw_fd(),
            c".cast/root/staging/usr",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            crate::linux_fs::controlled_resolution(),
        )
        .unwrap();

        let isolation_path = installation.isolation_dir();
        let isolation_root = crate::client::create_root_links(&isolation_path).unwrap();
        let detached = installation.root_path("detached-isolation-root");
        fs_err::rename(&isolation_path, &detached).unwrap();
        fs_err::create_dir(&isolation_path).unwrap();
        fs_err::write(isolation_path.join("foreign-root"), b"foreign").unwrap();

        let error = match container(
            &installation,
            &isolation_root,
            &local_etc,
            &candidate_usr,
            &candidate_usr_path,
        ) {
            Ok(_) => panic!("replacement isolation root was accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::PinRetainedTransactionSource {
                role: "container root",
                path,
                ..
            } if path == isolation_path
        ));
        assert_eq!(fs_err::read(isolation_path.join("foreign-root")).unwrap(), b"foreign");
        assert_eq!(fs_err::read_link(detached.join("bin")).unwrap(), Path::new("usr/bin"));
    }

    #[test]
    fn writable_bind_ignores_fixed_staging_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        crate::test_support::prepare_private_installation_root(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        fs_err::create_dir(installation.root.join("etc")).unwrap();
        fs_err::set_permissions(installation.root.join("etc"), std::fs::Permissions::from_mode(0o755)).unwrap();
        fs_err::write(
            installation.root.join("etc/retained-etc-witness"),
            b"retained local etc",
        )
        .unwrap();
        let local_etc = crate::client::transaction_root::require_local_etc(&installation).unwrap();

        let staging = installation.staging_dir();
        let candidate_usr_path = staging.join("usr");
        fs_err::create_dir(&candidate_usr_path).unwrap();
        let candidate_witness = candidate_usr_path.join("candidate-witness");
        fs_err::write(&candidate_witness, b"retained candidate").unwrap();
        let isolation_root = crate::client::create_root_links(&installation.isolation_dir()).unwrap();

        let candidate_usr = crate::linux_fs::openat2_file(
            installation.root_directory().as_raw_fd(),
            c".cast/root/staging/usr",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            crate::linux_fs::controlled_resolution(),
        )
        .unwrap();
        let candidate_identity = candidate_usr.metadata().unwrap();

        let detached = installation.root_path("detached-trigger-candidate");
        let replacement_witness = staging.join("usr/candidate-witness");
        let hook_staging = staging.clone();
        let hook_detached = detached.clone();
        let etc = installation.root.join("etc");
        let detached_etc = installation.root.join("detached-local-etc");
        let hook_etc = etc.clone();
        let hook_detached_etc = detached_etc.clone();
        arm_before_activation(move || {
            fs_err::rename(&hook_staging, &hook_detached).unwrap();
            fs_err::create_dir(&hook_staging).unwrap();
            fs_err::create_dir(hook_staging.join("usr")).unwrap();
            fs_err::write(hook_staging.join("usr/candidate-witness"), b"foreign replacement").unwrap();
            fs_err::rename(&hook_etc, &hook_detached_etc).unwrap();
            fs_err::create_dir(&hook_etc).unwrap();
            fs_err::set_permissions(&hook_etc, std::fs::Permissions::from_mode(0o755)).unwrap();
            fs_err::write(hook_etc.join("replacement-etc-witness"), b"foreign local etc").unwrap();
        });

        let matched = fnmatch::Match {
            path: "/usr/candidate-witness".to_owned(),
            variables: BTreeMap::new(),
        };
        let trigger = Handler::Delete {
            delete: vec!["/usr/candidate-witness".to_owned()],
        }
        .compiled(&matched);
        let runner = TriggerRunner {
            scope: TriggerScope::RetainedTransaction {
                kind: RetainedTransactionKind::Stateful,
                installation: &installation,
                isolation_root: &isolation_root,
                local_etc: &local_etc,
                candidate_usr: &candidate_usr,
                candidate_usr_path: &candidate_usr_path,
            },
            trigger,
        };

        let result = runner.execute();
        assert!(
            detached.join("usr").is_dir(),
            "pre-activation substitution hook did not run: {result:?}"
        );
        assert_eq!(
            (candidate_identity.dev(), candidate_identity.ino()),
            {
                let detached_usr = fs_err::symlink_metadata(detached.join("usr")).unwrap();
                (detached_usr.dev(), detached_usr.ino())
            },
            "the descriptor-pinned candidate must be the wrapper detached by the hook"
        );
        assert_eq!(
            fs_err::read(&replacement_witness).unwrap(),
            b"foreign replacement",
            "the replacement fixed-staging tree must never become the writable bind"
        );
        assert_eq!(
            fs_err::read(detached_etc.join("retained-etc-witness")).unwrap(),
            b"retained local etc",
            "the descriptor-pinned local /etc must survive final-name substitution"
        );
        assert_eq!(
            fs_err::read(etc.join("replacement-etc-witness")).unwrap(),
            b"foreign local etc",
            "the replacement local /etc must remain a distinct inode"
        );

        match result {
            Ok(()) => assert!(
                !detached.join("usr/candidate-witness").exists(),
                "the delete handler must have mutated the descriptor-pinned candidate"
            ),
            Err(Error::Container(container::Error::CloneNamespaces {
                source: nix::errno::Errno::EPERM | nix::errno::Errno::EACCES | nix::errno::Errno::ENOSYS,
            })) => {
                eprintln!("SKIP retained transaction activation: host denied mandatory namespaces")
            }
            Err(Error::Container(container::Error::Failure { message }))
                if message.starts_with("legacy clone requires an authenticated single-task supervisor:") =>
            {
                // Forge's libtest process deliberately has a harness task and
                // a test task, while production legacy activation rejects
                // fork-after-threads. The assertions above still prove that
                // this post-pin/pre-activation hook cannot redirect the bind;
                // Container's own cfg(test) source-pin test covers the final
                // descriptor handoff without weakening the production audit.
                assert_eq!(
                    fs_err::read(detached.join("usr/candidate-witness")).unwrap(),
                    b"retained candidate"
                );
                eprintln!("SKIP retained transaction payload: {message}");
            }
            Err(error) => panic!("retained transaction activation failed: {error:?}"),
        }
    }
}
