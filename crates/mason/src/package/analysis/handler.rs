// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use filetime::FileTime;
use itertools::Itertools;
use nix::{
    errno::Errno,
    sys::{
        signal::{Signal, kill},
        wait::{WaitStatus, waitpid},
    },
    unistd::{Pid, getpid},
};
use std::{
    io::{BufReader, BufWriter, Read, Write},
    os::unix::fs::symlink,
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
};

use fs_err::{self as fs, File};
use stone::relation::{Dependency, Kind, Provider};
use thiserror::Error;

use crate::package::collect::PathInfo;

pub use self::elf::elf;
pub use self::python::python;
use super::{BoxError, BucketMut, Decision, Response};

mod elf;
mod python;

/// Construct an analyzer subprocess with no ambient environment or readable
/// standard input. Analyzer tools are part of frozen execution and must not
/// gain inputs from the process which launched Cast.
pub(super) fn analyzer_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear().stdin(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
}

/// Run one analyzer tool and reject all non-success statuses before consuming
/// any partial stdout. Silently accepting failed analysis would make package
/// relations depend on host/runtime failure state outside the frozen plan.
pub(super) fn checked_output(mut command: Command) -> Result<Output, BoxError> {
    let invocation = format!("{command:?}");
    let output = contained_output(&mut command, analyzer_containment())?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Box::new(AnalyzerCommandError {
            invocation,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyzerContainment {
    /// Frozen packaging runs as PID 1. Killing every other process in that
    /// namespace catches process-group changes, `setsid`, and double forks.
    PidNamespace,
    /// Unit tests do not own their PID namespace and use the command's private
    /// process group as a safe behavioral boundary.
    #[cfg(test)]
    ProcessGroup,
}

fn analyzer_containment() -> AnalyzerContainment {
    #[cfg(test)]
    if getpid().as_raw() != 1 {
        return AnalyzerContainment::ProcessGroup;
    }
    AnalyzerContainment::PidNamespace
}

fn contained_output(command: &mut Command, containment: AnalyzerContainment) -> Result<Output, BoxError> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let stdout = read_analyzer_pipe(child.stdout.take().expect("piped analyzer stdout"));
    let stderr = read_analyzer_pipe(child.stderr.take().expect("piped analyzer stderr"));

    // Drain both pipes concurrently so a verbose direct child cannot block on
    // a full pipe. Once that child exits, terminate every descendant before
    // waiting for EOF: a background pipe holder must never hang packaging.
    let status = child.wait();
    let cleanup = terminate_analyzer_descendants(containment, child_pid);
    if let Err(error) = cleanup {
        return Err(Box::new(error));
    }
    let stdout = join_analyzer_pipe(stdout)?;
    let stderr = join_analyzer_pipe(stderr)?;

    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn read_analyzer_pipe<R>(mut pipe: R) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_analyzer_pipe(handle: thread::JoinHandle<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>, BoxError> {
    handle
        .join()
        .map_err(|_| Box::<dyn std::error::Error + Send + Sync>::from("analyzer pipe reader panicked"))?
        .map_err(Into::into)
}

fn terminate_analyzer_descendants(containment: AnalyzerContainment, child_pid: Pid) -> std::io::Result<()> {
    let target = analyzer_descendant_signal_target(containment, child_pid);
    if matches!(containment, AnalyzerContainment::PidNamespace) && getpid().as_raw() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing namespace-wide analyzer cleanup outside PID 1",
        ));
    }
    match kill(target, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => return Err(error.into()),
    }

    if matches!(containment, AnalyzerContainment::PidNamespace) {
        loop {
            match waitpid(Pid::from_raw(-1), None) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {}
                Ok(
                    WaitStatus::Stopped(..)
                    | WaitStatus::PtraceEvent(..)
                    | WaitStatus::PtraceSyscall(..)
                    | WaitStatus::Continued(..)
                    | WaitStatus::StillAlive,
                ) => {}
                Err(Errno::EINTR) => {}
                Err(Errno::ECHILD) => break,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn analyzer_descendant_signal_target(containment: AnalyzerContainment, _child_pid: Pid) -> Pid {
    match containment {
        AnalyzerContainment::PidNamespace => Pid::from_raw(-1),
        #[cfg(test)]
        AnalyzerContainment::ProcessGroup => Pid::from_raw(-_child_pid.as_raw()),
    }
}

#[derive(Debug, Error)]
#[error("analyzer command {invocation} failed with {status}: {stderr}")]
struct AnalyzerCommandError {
    invocation: String,
    status: std::process::ExitStatus,
    stderr: String,
}

pub fn include_any(_bucket: &mut BucketMut<'_>, _info: &mut PathInfo) -> Result<Response, BoxError> {
    Ok(Decision::IncludeFile.into())
}

pub fn ignore_blocked(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    // non-/usr = bad
    if !info.target_path.starts_with("/usr") {
        return Ok(Decision::IgnoreFile {
            reason: "non /usr/ file".into(),
        }
        .into());
    }

    // libtool files break the world but very rarely a package will need them to function correctly
    if info.file_name().ends_with(".la")
        && (info.target_path.starts_with("/usr/lib") || info.target_path.starts_with("/usr/lib32"))
        && bucket.analysis.remove_libtool
    {
        return Ok(Decision::IgnoreFile {
            reason: "libtool file".into(),
        }
        .into());
    }

    Ok(Decision::NextHandler.into())
}

pub fn binary(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    if info.target_path.starts_with("/usr/bin") {
        let provider = Provider {
            kind: Kind::Binary,
            name: info.file_name().to_owned(),
        };
        bucket.providers.insert(provider);
    } else if info.target_path.starts_with("/usr/sbin") {
        let provider = Provider {
            kind: Kind::SystemBinary,
            name: info.file_name().to_owned(),
        };
        bucket.providers.insert(provider);
    }

    Ok(Decision::NextHandler.into())
}

pub fn pkg_config(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_name = info.file_name();

    if !info.has_component("pkgconfig") || !file_name.ends_with(".pc") {
        return Ok(Decision::NextHandler.into());
    }

    let provider_name = file_name.strip_suffix(".pc").expect("extension exists");
    let emul32 = info.has_component("lib32");

    let provider = Provider {
        kind: if emul32 { Kind::PkgConfig32 } else { Kind::PkgConfig },
        name: provider_name.to_owned(),
    };

    bucket.providers.insert(provider);

    let program = &bucket
        .analysis
        .tools
        .pkg_config
        .as_ref()
        .expect("validated analysis plan requires pkg-config for the pkg-config handler")
        .path;
    let mut command = analyzer_command(program);
    command
        .args(["--print-requires", "--print-requires-private", "--silence-errors"])
        .arg(&info.path)
        .envs([
            ("LC_ALL", "C"),
            (
                "PKG_CONFIG_PATH",
                if emul32 {
                    "/usr/lib32/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
                } else {
                    "/usr/lib/pkgconfig:/usr/share/pkgconfig"
                },
            ),
        ]);
    let output = checked_output(command)?;
    let stdout = String::from_utf8(output.stdout)?;
    let deps = stdout.lines().filter_map(|line| line.split_whitespace().next());

    for dep in deps {
        let emul32_path = PathBuf::from(format!("/usr/lib32/pkgconfig/{dep}.pc"));
        let local_path = info
            .path
            .parent()
            .map(|p| p.join(format!("{dep}.pc")))
            .unwrap_or_default();

        let kind = if emul32 && (local_path.exists() || emul32_path.exists()) {
            Kind::PkgConfig32
        } else {
            Kind::PkgConfig
        };

        bucket.dependencies.insert(Dependency {
            kind,
            name: dep.to_owned(),
        });
    }

    Ok(Decision::NextHandler.into())
}

pub fn cmake(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    let file_name = info.file_name();

    if (!file_name.ends_with("Config.cmake") && !file_name.ends_with("-config.cmake"))
        || file_name.ends_with("-Config.cmake")
    {
        return Ok(Decision::NextHandler.into());
    }

    let provider_name = file_name
        .strip_suffix("Config.cmake")
        .or_else(|| file_name.strip_suffix("-config.cmake"))
        .expect("extension exists");

    bucket.providers.insert(Provider {
        kind: Kind::CMake,
        name: provider_name.to_owned(),
    });

    Ok(Decision::NextHandler.into())
}

/// Ensure that man and info files are zst compressed for on-disk space savings.
pub fn compressman(bucket: &mut BucketMut<'_>, info: &mut PathInfo) -> Result<Response, BoxError> {
    /* if the compressman option is turned off, exit early */
    if !bucket.analysis.compress_man {
        return Ok(Decision::NextHandler.into());
    }

    let is_man_file = info.path.components().contains(&Component::Normal("man".as_ref()))
        && info.file_name().ends_with(|c| ('1'..'9').contains(&c));
    let is_info_file =
        info.path.components().contains(&Component::Normal("info".as_ref())) && info.file_name().ends_with(".info");

    /* we only care about compressing man and info files here */
    if !(is_man_file || is_info_file) {
        return Ok(Decision::NextHandler.into());
    }

    pub fn compress_file_zstd(path: &Path) -> Result<PathBuf, BoxError> {
        let output_path = path.with_added_extension(".zst");
        let mut reader = BufReader::new(File::open(path)?);
        let mut writer = BufWriter::new(File::create(&output_path)?);

        zstd::stream::copy_encode(&mut reader, &mut writer, 16)?;

        writer.flush()?;

        Ok(output_path)
    }

    let mut generated_path = PathBuf::new();

    let metadata = fs::metadata(&info.path)?;
    let atime = metadata.accessed()?;
    let mtime = metadata.modified()?;

    let uncompressed_file = fs::canonicalize(&info.path)?;
    /* we are deducing this in advance to have something against which to symlink */
    let compressed_zst_file = uncompressed_file.with_added_extension(".zst");

    /* If we have a man/info symlink then update the link to the compressed file */
    if info.path.is_symlink() {
        let new_zst_symlink = info.path.with_added_extension(".zst");

        /*
         * Depending on the order in which the files get analysed,
         * the new compressed file may not yet exist, so compress it _now_
         * in order that the correct metadata src info is returned to the binary writer.
         */
        if !fs::exists(&new_zst_symlink)? {
            compress_file_zstd(&uncompressed_file)?;
            let _ = bucket.paths.install().guest.join(&compressed_zst_file);
        }

        symlink(&compressed_zst_file, &new_zst_symlink)?;

        /* Restore the original {a,m}times for reproducibility */
        filetime::set_symlink_file_times(
            &new_zst_symlink,
            FileTime::from_system_time(atime),
            FileTime::from_system_time(mtime),
        )?;

        generated_path.push(bucket.paths.install().guest.join(new_zst_symlink));
        return Ok(Decision::ReplaceFile {
            newpath: generated_path,
        }
        .into());
    }

    /* We already know what the returned filename will be, so just ignore the return value */
    if !compressed_zst_file.try_exists()? {
        compress_file_zstd(&uncompressed_file)?;
    }

    /* Restore the original {a,m}times for reproducibility */
    filetime::set_file_handle_times(
        &File::open(&compressed_zst_file)?.into_file(),
        Some(FileTime::from_system_time(atime)),
        Some(FileTime::from_system_time(mtime)),
    )?;

    generated_path.push(bucket.paths.install().guest.join(compressed_zst_file));

    Ok(Decision::ReplaceFile {
        newpath: generated_path,
    }
    .into())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    use super::*;

    #[test]
    fn analyzer_commands_have_no_ambient_environment_stdin_or_descriptors() {
        let environment = checked_output(analyzer_command("/usr/bin/env")).unwrap();
        assert!(environment.stdout.is_empty());

        let inherited = tempfile::tempfile().unwrap();
        let inherited_fd = inherited.as_raw_fd();
        fcntl(inherited_fd, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();

        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", &format!("test ! -e /proc/self/fd/{inherited_fd} && ! read value")]);

        checked_output(command).unwrap();
    }

    #[test]
    fn analyzer_command_failure_is_rejected_even_with_partial_stdout() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf partial-output; printf analyzer-failed >&2; exit 9"]);

        let error = checked_output(command).unwrap_err().to_string();

        assert!(error.contains("exit status: 9"), "{error}");
        assert!(error.contains("analyzer-failed"), "{error}");
    }

    #[test]
    fn background_analyzer_pipe_holder_cannot_hang_packaging() {
        let mut command = analyzer_command("/bin/sh");
        command.args(["-c", "printf direct-output; (sleep 30) &"]);

        let started = Instant::now();
        let output = checked_output(command).unwrap();

        assert_eq!(output.stdout, b"direct-output");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn background_analyzer_cannot_mutate_after_direct_child_exit() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("delayed-write");
        let mut command = analyzer_command("/bin/sh");
        command
            .env("MARKER", &marker)
            .args(["-c", "(sleep 0.2; printf escaped > \"$MARKER\") &"]);

        checked_output(command).unwrap();
        thread::sleep(Duration::from_millis(500));

        assert!(!marker.exists());
    }

    #[test]
    fn production_analyzer_cleanup_targets_the_complete_pid_namespace() {
        assert_eq!(
            analyzer_descendant_signal_target(AnalyzerContainment::PidNamespace, Pid::from_raw(1234)),
            Pid::from_raw(-1)
        );
    }

    #[test]
    fn production_handlers_do_not_embed_analyzer_program_selection() {
        let production = |source: &'static str| source.split("#[cfg(test)]").next().unwrap();
        let sources = [
            production(include_str!("handler.rs")),
            production(include_str!("handler/python.rs")),
            production(include_str!("handler/elf.rs")),
        ];

        for source in sources {
            for forbidden in [
                "/usr/bin/pkg-config",
                "/usr/bin/python3",
                "/usr/bin/llvm-objcopy",
                "/usr/bin/llvm-strip",
                "/usr/bin/objcopy",
                "/usr/bin/strip",
                "AnalysisToolchain",
            ] {
                assert!(!source.contains(forbidden), "production analyzer embeds {forbidden}");
            }
        }
    }
}
