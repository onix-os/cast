// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Operations that happen post-blit (primarily, triggers within container)
//! Note that we support transaction scope and system scope triggers, invoked
//! before `/usr` is activated and after, respectively.
//!
//! Trigger intent is loaded from `/usr/share/cast/triggers/{tx.d,sys.d}/*.glu`
//! and do not yet support local triggers
mod process;

use std::{
    io,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{self as std_process, Stdio},
};

use crate::Installation;
use config::{DecodedGluon, GluonCodec, GluonCodecError};
use container::{
    Container, DevPolicy, ProcPolicy, PseudoFilesystemPolicy, RootFilesystemPolicy, SysPolicy, TmpPolicy, TmpfsLimits,
};
use gluon_config::{Evaluator, Source};
use itertools::Itertools;
use thiserror::Error;
use triggers::format::{CompiledHandler, Handler, Trigger};

use super::PendingFile;

/// Transaction triggers may inspect process state and use the conventional
/// null devices, but they do not need the host device or sysfs trees. Keep the
/// scratch directory private to each container invocation.
const TRANSACTION_TMPFS_SIZE_BYTES: u64 = 256 * 1024 * 1024;
const TRANSACTION_TMPFS_INODES: u64 = 65_536;
const TRANSACTION_TMPFS_LIMITS: TmpfsLimits =
    match TmpfsLimits::new(TRANSACTION_TMPFS_SIZE_BYTES, TRANSACTION_TMPFS_INODES) {
        Ok(limits) => limits,
        Err(_) => panic!("transaction tmpfs limits are non-zero"),
    };
const TRANSACTION_PSEUDO_FILESYSTEMS: PseudoFilesystemPolicy = PseudoFilesystemPolicy {
    proc: ProcPolicy::ReadOnly,
    tmp: TmpPolicy::Bounded(TRANSACTION_TMPFS_LIMITS),
    sys: SysPolicy::None,
    dev: DevPolicy::Minimal,
};
const TRANSACTION_ROOT_FILESYSTEM: RootFilesystemPolicy = RootFilesystemPolicy::ReadOnly;

/// Transaction trigger wrapper
/// These are loaded from `/usr/share/cast/triggers/tx.d/*.glu`
#[derive(Debug)]
struct TransactionTrigger(Trigger);

impl config::Config for TransactionTrigger {
    fn domain() -> String {
        "tx".into()
    }
}

/// System trigger wrapper
/// These triggers are loaded from `/usr/share/cast/triggers/sys.d/*.glu`
#[derive(Debug)]
struct SystemTrigger(Trigger);

impl config::Config for SystemTrigger {
    fn domain() -> String {
        "sys".into()
    }
}

struct TransactionTriggerCodec;

impl GluonCodec for TransactionTriggerCodec {
    type Config = TransactionTrigger;

    fn decode(&self, evaluator: &Evaluator, source: &Source) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        let evaluated = triggers::evaluate_gluon_with(evaluator, source).map_err(GluonCodecError::conversion)?;
        Ok(DecodedGluon {
            value: TransactionTrigger(evaluated.trigger),
            fingerprint: evaluated.fingerprint,
        })
    }

    fn encode(&self, _config: &Self::Config) -> Result<String, GluonCodecError> {
        Err(GluonCodecError::conversion(io::Error::new(
            io::ErrorKind::Unsupported,
            "packaged transaction triggers are read-only",
        )))
    }
}

struct SystemTriggerCodec;

impl GluonCodec for SystemTriggerCodec {
    type Config = SystemTrigger;

    fn decode(&self, evaluator: &Evaluator, source: &Source) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        let evaluated = triggers::evaluate_gluon_with(evaluator, source).map_err(GluonCodecError::conversion)?;
        Ok(DecodedGluon {
            value: SystemTrigger(evaluated.trigger),
            fingerprint: evaluated.fingerprint,
        })
    }

    fn encode(&self, _config: &Self::Config) -> Result<String, GluonCodecError> {
        Err(GluonCodecError::conversion(io::Error::new(
            io::ErrorKind::Unsupported,
            "packaged system triggers are read-only",
        )))
    }
}

/// The trigger scope determines the environment that the trigger runs in
#[derive(Clone, Copy, Debug)]
pub(super) enum TriggerScope<'a> {
    /// A transaction trigger, isolated to `/usr`
    Transaction(&'a Installation, &'a super::Scope),

    /// A system trigger with reduced sandboxing, capable of writes outside `/usr`
    System(&'a Installation, &'a super::Scope),
}

impl TriggerScope<'_> {
    // Determine the correct root directory
    fn root_dir(&self) -> PathBuf {
        match self {
            TriggerScope::Transaction(install, scope) => match scope {
                super::Scope::Stateful => install.staging_dir().clone(),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.clone(),
            },
            TriggerScope::System(install, scope) => match scope {
                super::Scope::Stateful => install.root.clone(),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.clone(),
            },
        }
    }

    /// Join "host" paths, outside the staging filesystem. Ensure no sandbox break for ephemeral
    fn host_path(&self, path: impl AsRef<Path>) -> PathBuf {
        match self {
            TriggerScope::Transaction(install, scope) => match scope {
                super::Scope::Stateful => install.root.join(path),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.join(path),
            },
            TriggerScope::System(install, scope) => match scope {
                super::Scope::Stateful => install.root.join(path),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.join(path),
            },
        }
    }

    /// Join guest paths, inside the staging filesystem. Ensure no sandbox break for ephemeral
    fn guest_path(&self, path: impl AsRef<Path>) -> PathBuf {
        match self {
            TriggerScope::Transaction(install, scope) => match scope {
                super::Scope::Stateful => install.staging_path(path),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.join(path),
            },
            TriggerScope::System(install, scope) => match scope {
                super::Scope::Stateful => install.root.join(path),
                super::Scope::Ephemeral { blit_root } | super::Scope::Frozen { blit_root } => blit_root.join(path),
            },
        }
    }
}

/// Condensed type for loaded triggers with scope and executor
#[derive(Debug)]
pub(super) struct TriggerRunner<'a> {
    scope: TriggerScope<'a>,
    trigger: CompiledHandler,
}

/// Load all triggers matching the given scope and staging filesystem
///
/// # Arguments
///
/// * `scope`  - Trigger execution scope
/// * `fstree` - Virtual filesystem tree populated with records of the staging filesystem
pub(super) fn triggers<'a>(
    scope: TriggerScope<'a>,
    fstree: &vfs::tree::Tree<PendingFile>,
) -> Result<Vec<TriggerRunner<'a>>, Error> {
    // Pre-calculate trigger root path once
    let trigger_root = {
        let mut path = PathBuf::with_capacity(50);
        path.push("usr");
        path.push("share");
        path.push("cast");
        path.push("triggers");
        path
    };

    let full_trigger_path = scope.root_dir().join(&trigger_root);

    // Load appropriate triggers from their locations and convert back to a vec of Trigger
    let triggers = match scope {
        TriggerScope::Transaction(..) => config::Manager::custom(&full_trigger_path)
            .load_gluon(&Evaluator::default(), &TransactionTriggerCodec)
            .map_err(|error| Error::Config(Box::new(error)))?
            .into_iter()
            .map(|l| l.value.0)
            .collect_vec(),
        TriggerScope::System(..) => config::Manager::custom(&full_trigger_path)
            .load_gluon(&Evaluator::default(), &SystemTriggerCodec)
            .map_err(|error| Error::Config(Box::new(error)))?
            .into_iter()
            .map(|l| l.value.0)
            .collect_vec(),
    };

    // Load trigger collection, process all the paths, convert to scoped TriggerRunner vec
    let mut collection = triggers::Collection::new(triggers.iter())?;
    collection.process_paths(fstree.iter().map(|m| m.to_string()));
    let computed_commands = collection
        .bake()?
        .into_iter()
        .map(|trigger| TriggerRunner { scope, trigger })
        .collect_vec();
    Ok(computed_commands)
}

impl TriggerRunner<'_> {
    pub fn handler(&self) -> &Handler {
        self.trigger.handler()
    }

    /// Execute a trigger, taking care to account for the transaction scope and client scope
    ///
    /// All transaction triggers are run via sandboxing ([`container::Container`]) to limit their
    /// system view, and limit write access.
    /// System triggers will execute without any sandboxing when Cast is used directly against the
    /// live root filesystem, and will force sandboxing when using a non-`/` root (such as using the
    /// `-D` argument with `cast install`)
    pub fn execute(&self) -> Result<(), Error> {
        match self.scope {
            TriggerScope::Transaction(install, _) => {
                // TODO: Add caching support via /var/
                let isolation = Container::new(install.isolation_dir())
                    .networking(false)
                    .root_filesystem(TRANSACTION_ROOT_FILESYSTEM)
                    .pseudo_filesystems(TRANSACTION_PSEUDO_FILESYSTEMS)
                    .bind_ro(self.scope.host_path("etc"), "/etc")
                    .bind_rw(self.scope.guest_path("usr"), "/usr")
                    .work_dir("/");

                Ok(isolation.run(|| execute_trigger_directly(&self.trigger))?)
            }
            TriggerScope::System(install, _) => {
                // OK, if the root == `/` then we can run directly, otherwise we need to containerise with RW.
                if install.root.to_string_lossy() == "/" {
                    Ok(execute_trigger_directly(&self.trigger)?)
                } else {
                    let isolation = Container::new(install.isolation_dir())
                        .networking(false)
                        .bind_rw(self.scope.host_path("etc"), "/etc")
                        .bind_rw(self.scope.guest_path("usr"), "/usr")
                        .work_dir("/");

                    Ok(isolation.run(|| execute_trigger_directly(&self.trigger))?)
                }
            }
        }
    }
}

/// Internal executor for triggers.
fn execute_trigger_directly(trigger: &CompiledHandler) -> Result<(), Error> {
    execute_handler_directly(trigger.handler())
}

fn execute_handler_directly(trigger: &Handler) -> Result<(), Error> {
    match trigger {
        Handler::Run { run, args } => {
            let mut command = trigger_command(run, args);
            let output = process::output(&mut command).map_err(|source| Error::TriggerExecution {
                command: run.clone(),
                args: args.clone(),
                source: Box::new(source),
            })?;
            if output.status.success() {
                return Ok(());
            }

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if let Some(code) = output.status.code() {
                return Err(Error::TriggerExited {
                    command: run.clone(),
                    args: args.clone(),
                    code,
                    stdout,
                    stderr,
                });
            }
            if let Some(signal) = output.status.signal() {
                return Err(Error::TriggerSignaled {
                    command: run.clone(),
                    args: args.clone(),
                    signal,
                    stdout,
                    stderr,
                });
            }

            return Err(Error::TriggerTerminated {
                command: run.clone(),
                args: args.clone(),
                stdout,
                stderr,
            });
        }
        Handler::Delete { delete } => {
            // Match the handler's documented `rm -- PATH...` semantics without
            // invoking an ambient executable. Validate the complete list before
            // mutating anything, unlink non-directory entries directly, and
            // never recurse or follow a symlink target.
            let paths = delete
                .iter()
                .map(|path| validate_delete_path(path))
                .collect::<Result<Vec<_>, _>>()?;
            for path in paths {
                fs_err::remove_file(path).map_err(|source| Error::DeletePath {
                    path: path.to_owned(),
                    source,
                })?;
            }
        }
    }

    Ok(())
}

fn validate_delete_path(path: &str) -> Result<&Path, Error> {
    let invalid = |reason| Error::InvalidDeletePath {
        path: PathBuf::from(path),
        reason,
    };

    if path.is_empty() {
        return Err(invalid("path is empty"));
    }
    if path.as_bytes().contains(&0) {
        return Err(invalid("path contains NUL"));
    }
    if !path.starts_with('/') {
        return Err(invalid("path is not absolute"));
    }
    if path == "/" {
        return Err(invalid("filesystem root cannot be deleted"));
    }
    if path[1..]
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid("path is not lexically normalized"));
    }

    Ok(Path::new(path))
}

/// Build the deliberately small, target-root-owned process context shared by
/// transaction and system triggers.
///
/// Trigger commands may resolve helper programs only from the target root's
/// standard system paths. Locale, timezone, home, temporary paths and the file
/// creation mask are fixed; no environment or open descriptor from the process
/// that launched Cast is a trigger input.
///
/// The contract is `PATH=/usr/sbin:/usr/bin:/sbin:/bin`, `HOME=/`,
/// `TMPDIR=/tmp`, `LANG=C`, `LC_ALL=C`, `TZ=UTC`, working directory `/`, umask
/// `0022`, null standard input, and captured standard output/error.
fn trigger_command(run: &str, args: &[String]) -> std_process::Command {
    const TRIGGER_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

    let mut command = std_process::Command::new(run);
    command
        .args(args)
        .current_dir("/")
        .env_clear()
        .env("HOME", "/")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", TRIGGER_PATH)
        .env("TMPDIR", "/tmp")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Container control descriptors are already close-on-exec, but system
    // triggers can run directly against `/`. Cover both paths, including file
    // descriptors inherited by Cast from its launcher.
    unsafe {
        command.pre_exec(|| {
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            nix::libc::umask(0o022);
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    command
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("load Gluon trigger configuration")]
    Config(#[source] Box<config::LoadGluonError>),

    #[error("container")]
    Container(#[from] container::Error),

    #[error("triggers")]
    Triggers(#[from] triggers::Error),

    #[error("trigger command `{command}` {args:?} failed: {source}")]
    TriggerExecution {
        command: String,
        args: Vec<String>,
        #[source]
        source: Box<process::Error>,
    },

    #[error("trigger command `{command}` {args:?} exited with status {code}; stdout: {stdout:?}; stderr: {stderr:?}")]
    TriggerExited {
        command: String,
        args: Vec<String>,
        code: i32,
        stdout: String,
        stderr: String,
    },

    #[error(
        "trigger command `{command}` {args:?} was terminated by signal {signal}; stdout: {stdout:?}; stderr: {stderr:?}"
    )]
    TriggerSignaled {
        command: String,
        args: Vec<String>,
        signal: i32,
        stdout: String,
        stderr: String,
    },

    #[error(
        "trigger command `{command}` {args:?} terminated without an exit code or signal; stdout: {stdout:?}; stderr: {stderr:?}"
    )]
    TriggerTerminated {
        command: String,
        args: Vec<String>,
        stdout: String,
        stderr: String,
    },

    #[error("invalid delete-trigger path `{}`: {reason}", path.display())]
    InvalidDeletePath { path: PathBuf, reason: &'static str },

    #[error("delete trigger could not unlink `{}`", path.display())]
    DeletePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("io")]
    IO(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        os::{fd::AsRawFd, unix::fs::symlink},
    };

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    use super::*;

    #[test]
    fn transaction_trigger_sandbox_is_read_only_with_minimal_kernel_views() {
        assert_eq!(TRANSACTION_ROOT_FILESYSTEM, RootFilesystemPolicy::ReadOnly);
        assert_eq!(TRANSACTION_TMPFS_LIMITS.size_bytes(), 256 * 1024 * 1024);
        assert_eq!(TRANSACTION_TMPFS_LIMITS.inodes(), 65_536);
        assert_eq!(
            TRANSACTION_PSEUDO_FILESYSTEMS,
            PseudoFilesystemPolicy {
                proc: ProcPolicy::ReadOnly,
                tmp: TmpPolicy::Bounded(TRANSACTION_TMPFS_LIMITS),
                sys: SysPolicy::None,
                dev: DevPolicy::Minimal,
            }
        );
    }

    #[test]
    fn packaged_transaction_triggers_load_from_gluon_fragments() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("tx.d/depmod.glu");
        fs_err::create_dir_all(path.parent().unwrap()).unwrap();
        fs_err::write(
            &path,
            r#"let cast = import! cast.trigger.v1
let base = cast.trigger "depmod" "Update kernel module dependencies"
{
    paths = [cast.path
        "/usr/lib/modules/(version:*)/kernel"
        ["depmod"]
        (cast.optional.set cast.path_kind.directory)],
    handlers = [cast.handler.named "depmod" (cast.handler.run
        "/sbin/depmod"
        ["-a", "$(version)"])],
    .. base
}
"#,
        )
        .unwrap();

        let loaded = config::Manager::custom(temporary.path())
            .load_gluon(&Evaluator::default(), &TransactionTriggerCodec)
            .unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].value.0.name, "depmod");
        let (pattern, _) = loaded[0].value.0.paths.iter().next().unwrap();
        let matched = pattern
            .match_path("/usr/lib/modules/6.12.1/kernel")
            .expect("kernel path must match");
        assert_eq!(matched.variables.get("version").map(String::as_str), Some("6.12.1"));
    }

    #[test]
    fn trigger_commands_have_only_the_fixed_target_environment() {
        let mut command = trigger_command("/usr/bin/env", &[]);
        let output = command.spawn().unwrap().wait_with_output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let environment = stdout
            .lines()
            .map(|line| line.split_once('=').unwrap())
            .collect::<BTreeMap<_, _>>();

        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
        assert!(output.status.success());
        assert_eq!(
            environment,
            BTreeMap::from([
                ("HOME", "/"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
                ("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
                ("TMPDIR", "/tmp"),
                ("TZ", "UTC"),
            ])
        );
    }

    #[test]
    fn trigger_commands_get_eof_on_stdin_and_no_inherited_extra_descriptors() {
        let inherited = tempfile::tempfile().unwrap();
        let inherited_fd = inherited.as_raw_fd();
        fcntl(inherited_fd, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();
        let script = format!(
            "test \"$(pwd)\" = / && test \"$(umask)\" = 0022 && \
             test ! -e /proc/self/fd/{inherited_fd} && ! read value"
        );

        let output = trigger_command("/bin/sh", &["-c".to_owned(), script])
            .spawn()
            .unwrap()
            .wait_with_output()
            .unwrap();

        assert!(
            output.status.success(),
            "trigger probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn nonzero_trigger_exit_is_a_hard_error_with_diagnostics() {
        let handler = Handler::Run {
            run: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf trigger-out; printf trigger-err >&2; exit 23".to_owned(),
            ],
        };

        let error = execute_handler_directly(&handler).unwrap_err();

        assert!(matches!(
            error,
            Error::TriggerExited {
                ref command,
                code: 23,
                ref stdout,
                ref stderr,
                ..
            } if command == "/bin/sh" && stdout == "trigger-out" && stderr == "trigger-err"
        ));
    }

    #[test]
    fn signaled_trigger_exit_is_a_hard_error() {
        let handler = Handler::Run {
            run: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), "kill -TERM $$".to_owned()],
        };

        let error = execute_handler_directly(&handler).unwrap_err();

        assert!(matches!(
            error,
            Error::TriggerSignaled {
                ref command,
                signal: nix::libc::SIGTERM,
                ..
            } if command == "/bin/sh"
        ));
    }

    #[test]
    fn delete_handler_unlinks_files_and_symlinks_without_following_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("generated-cache");
        let target = temporary.path().join("retained-target");
        let link = temporary.path().join("generated-link");
        fs_err::write(&file, b"delete me").unwrap();
        fs_err::write(&target, b"retain me").unwrap();
        symlink(&target, &link).unwrap();
        let handler = Handler::Delete {
            delete: vec![file.to_string_lossy().into_owned(), link.to_string_lossy().into_owned()],
        };

        execute_handler_directly(&handler).unwrap();

        assert!(!file.exists());
        assert!(!link.exists());
        assert_eq!(fs_err::read(target).unwrap(), b"retain me");
    }

    #[test]
    fn delete_handler_rejects_ambiguous_paths_before_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("must-remain");
        fs_err::write(&marker, b"untouched").unwrap();
        let ambiguous = temporary.path().join("subdir/../escape");
        let handler = Handler::Delete {
            delete: vec![
                marker.to_string_lossy().into_owned(),
                ambiguous.to_string_lossy().into_owned(),
            ],
        };

        let error = execute_handler_directly(&handler).unwrap_err();

        assert!(matches!(
            error,
            Error::InvalidDeletePath { ref path, reason }
                if path == &ambiguous && reason == "path is not lexically normalized"
        ));
        assert_eq!(fs_err::read(marker).unwrap(), b"untouched");
    }

    #[test]
    fn delete_handler_never_recurses_into_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("directory");
        let marker = directory.join("must-remain");
        fs_err::create_dir(&directory).unwrap();
        fs_err::write(&marker, b"untouched").unwrap();
        let handler = Handler::Delete {
            delete: vec![directory.to_string_lossy().into_owned()],
        };

        let error = execute_handler_directly(&handler).unwrap_err();

        assert!(matches!(
            error,
            Error::DeletePath { ref path, .. } if path == &directory
        ));
        assert_eq!(fs_err::read(marker).unwrap(), b"untouched");
    }

    #[test]
    fn delete_path_contract_requires_normalized_absolute_non_root_paths() {
        for path in [
            "",
            "relative",
            "/",
            "//tmp/file",
            "/tmp/./file",
            "/tmp/../file",
            "/tmp/file/",
        ] {
            assert!(matches!(
                validate_delete_path(path),
                Err(Error::InvalidDeletePath { .. })
            ));
        }

        assert_eq!(validate_delete_path("/tmp/file").unwrap(), Path::new("/tmp/file"));
    }
}
