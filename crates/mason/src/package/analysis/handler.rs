// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use itertools::Itertools;
use nix::{
    errno::Errno,
    sys::{
        signal::{Signal, kill},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{Pid, getpid},
};
use std::{
    ffi::{CStr, CString, OsStr},
    fmt,
    fs::{File as StdFile, OpenOptions as StdOpenOptions, Permissions},
    io::{Read, Seek, SeekFrom, Write},
    mem::{MaybeUninit, size_of},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::mpsc::{self, RecvTimeoutError, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use stone::relation::{Dependency, Kind, Provider};
use thiserror::Error;

use crate::package::collect::{GeneratedArtifact, GeneratedTimes, PathInfo};

pub use self::elf::elf;
pub use self::python::python;
use super::{BoxError, BucketMut, Decision, Response};

mod elf;
mod execution;
mod input;
mod python;
mod sandbox;

pub(super) use self::{
    execution::{analyzer_command, checked_output_for},
    input::{ExternalAnalyzerInput, ExternalAnalyzerMutation, VerifiedAnalyzerInput},
};

const ANALYZER_LIMITS: execution::AnalyzerLimits = execution::AnalyzerLimits {
    // llvm-objcopy and llvm-strip can legitimately process very large debug
    // artefacts. Keep the ceiling finite without turning normal large-package
    // analysis into a race against an interactive-command timeout.
    wall_timeout: Duration::from_secs(5 * 60),
    stdout_bytes: 1024 * 1024,
    stderr_bytes: 1024 * 1024,
};

// These per-process ceilings are deliberately independent of the host's
// ambient limits. They complement the namespace-wide descendant cleanup;
// aggregate PID, memory, CPU, and scratch-space accounting belongs to the
// frozen executor boundary rather than being implied by rlimits here.
const ANALYZER_ADDRESS_SPACE_BYTES: nix::libc::rlim_t = 16 * 1024 * 1024 * 1024;
const ANALYZER_FILE_BYTES: nix::libc::rlim_t = 64 * 1024 * 1024 * 1024;
const ANALYZER_OPEN_FILES: nix::libc::rlim_t = 64;
const PKG_CONFIG_INPUT_BYTES: u64 = 4 * 1024 * 1024;
const SANDBOX_CLEANUP_ENTRY_LIMIT: usize = 65_536;
const SANDBOX_CLEANUP_DEPTH_LIMIT: usize = 256;
const ANALYZER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

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
    validate_pkg_config_module(provider_name)?;
    let provider = Provider::new(if emul32 { Kind::PkgConfig32 } else { Kind::PkgConfig }, provider_name)?;
    let logical_pcfiledir = info
        .target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| AnalyzerRelationError::MissingParent {
            path: info.target_path.clone(),
        })?;

    let program = &bucket
        .analysis
        .tools
        .pkg_config
        .as_ref()
        .expect("validated analysis plan requires pkg-config for the pkg-config handler")
        .path;
    let input = VerifiedAnalyzerInput::from_path_info(info, PKG_CONFIG_INPUT_BYTES)?;
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig")?;
    let operation = (|| {
        let mut command = analyzer_command(program);
        command
            .current_dir(sandbox.working_directory())
            .args(["--print-requires", "--print-requires-private", "--silence-errors"])
            // pkg-config otherwise recomputes this built-in from the random
            // sandbox path. Preserve the logical installed location so
            // `${pcfiledir}` stays deterministic and package-correct.
            .arg(format!("--define-variable=pcfiledir={}", logical_pcfiledir.display()))
            .arg(sandbox.path())
            .envs([
                ("LC_ALL", "C"),
                ("PKG_CONFIG_PATH", ""),
                (
                    "PKG_CONFIG_LIBDIR",
                    if emul32 {
                        "/usr/lib32/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
                    } else {
                        "/usr/lib/pkgconfig:/usr/share/pkgconfig"
                    },
                ),
                ("PKG_CONFIG_DISABLE_UNINSTALLED", "1"),
            ]);
        let output = checked_output_for(info, command)?;
        let stdout = String::from_utf8(output.stdout)?;
        parse_pkg_config_dependencies(info, &stdout)
    })();
    let dependency_names = sandbox.finish(info, operation)?;

    let mut dependencies = Vec::new();
    dependencies.try_reserve(dependency_names.len())?;
    for name in dependency_names {
        info.check_deadline()?;
        let kind = pkg_config_dependency_kind(info, logical_pcfiledir, &name, emul32)?;
        dependencies.push(Dependency::new(kind, name)?);
    }
    info.check_deadline()?;

    // Commit relation changes only after the authenticated input, child exit,
    // sandbox verification/cleanup, inventory queries, canonical relation
    // validation, and shared deadline all succeed.
    bucket.providers.insert(provider);
    bucket.dependencies.extend(dependencies);

    Ok(Decision::NextHandler.into())
}

fn pkg_config_dependency_kind(
    info: &PathInfo,
    logical_pcfiledir: &Path,
    name: &str,
    emul32: bool,
) -> Result<Kind, BoxError> {
    if !emul32 {
        return Ok(Kind::PkgConfig);
    }
    let sibling = logical_pcfiledir.join(format!("{name}.pc"));
    let canonical = Path::new("/usr/lib32/pkgconfig").join(format!("{name}.pc"));
    if info.inventory_contains_regular_target(&sibling)?
        || (sibling != canonical && info.inventory_contains_regular_target(&canonical)?)
        || frozen_root_contains_regular_target(&canonical)?
    {
        Ok(Kind::PkgConfig32)
    } else {
        Ok(Kind::PkgConfig)
    }
}

fn parse_pkg_config_dependencies(info: &PathInfo, stdout: &str) -> Result<Vec<String>, BoxError> {
    let mut dependencies = Vec::new();
    dependencies.try_reserve(stdout.lines().count())?;
    for line in stdout.lines() {
        info.check_deadline()?;
        let name = parse_pkg_config_dependency_line(line)?;
        // Route every analyzer-derived name through Stone's canonical
        // relation validator before assigning its architecture-specific kind.
        Dependency::new(Kind::PkgConfig, name)?;
        dependencies.push(name.to_owned());
    }
    info.check_deadline()?;
    Ok(dependencies)
}

fn parse_pkg_config_dependency_line(line: &str) -> Result<&str, AnalyzerRelationError> {
    let mut fields = line.split_whitespace();
    let Some(name) = fields.next() else {
        return Err(AnalyzerRelationError::InvalidPkgConfigDependencyLine { line: line.to_owned() });
    };
    validate_pkg_config_module(name)?;

    match (fields.next(), fields.next(), fields.next()) {
        (None, None, None) => Ok(name),
        (Some(operator), Some(version), None)
            if matches!(operator, "=" | "!=" | "<" | ">" | "<=" | ">=") && !version.is_empty() =>
        {
            Ok(name)
        }
        _ => Err(AnalyzerRelationError::InvalidPkgConfigDependencyLine { line: line.to_owned() }),
    }
}

fn frozen_root_contains_regular_target(target: &Path) -> Result<bool, BoxError> {
    let root = open_frozen_root_anchor(Path::new("/"))?;
    Ok(descriptor_root_contains_regular_target(&root, target)?)
}

fn open_frozen_root_anchor(root: &Path) -> Result<StdFile, FrozenRootLookupError> {
    StdOpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root)
        .map_err(|source| FrozenRootLookupError::OpenRoot {
            path: root.to_owned(),
            source,
        })
}

fn descriptor_root_contains_regular_target(root: &StdFile, target: &Path) -> Result<bool, FrozenRootLookupError> {
    let relative = target
        .strip_prefix("/")
        .map_err(|_| FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        });
    }
    let relative_c =
        CString::new(relative.as_os_str().as_bytes()).map_err(|_| FrozenRootLookupError::InvalidTarget {
            path: target.to_owned(),
        })?;
    // SAFETY: an all-zero `open_how` is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from((nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW) as u32);
    how.resolve = nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: root and relative_c remain live for the syscall and `how`
    // describes a descriptor-only lookup beneath the held root.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            root.as_raw_fd(),
            relative_c.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        let source = std::io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(nix::libc::ENOENT) | Some(nix::libc::ENOTDIR) | Some(nix::libc::ELOOP)
        ) {
            return Ok(false);
        }
        return Err(FrozenRootLookupError::OpenTarget {
            path: target.to_owned(),
            source,
        });
    }
    let descriptor = i32::try_from(result).map_err(|_| FrozenRootLookupError::InvalidDescriptor {
        path: target.to_owned(),
        descriptor: result,
    })?;
    // SAFETY: successful openat2 returned a fresh descriptor owned here.
    let file = unsafe { StdFile::from_raw_fd(descriptor) };
    let metadata = file.metadata().map_err(|source| FrozenRootLookupError::InspectTarget {
        path: target.to_owned(),
        source,
    })?;
    Ok(metadata.file_type().is_file())
}

fn validate_pkg_config_module(name: &str) -> Result<(), AnalyzerRelationError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        Ok(())
    } else {
        Err(AnalyzerRelationError::InvalidPkgConfigName { name: name.to_owned() })
    }
}

#[derive(Debug, Error)]
enum AnalyzerRelationError {
    #[error("analyzer input {path} has no logical parent directory")]
    MissingParent { path: PathBuf },
    #[error("pkg-config emitted invalid module name {name:?}")]
    InvalidPkgConfigName { name: String },
    #[error("pkg-config emitted malformed dependency line {line:?}")]
    InvalidPkgConfigDependencyLine { line: String },
}

#[derive(Debug, Error)]
enum FrozenRootLookupError {
    #[error("failed to open frozen root {path}: {source}")]
    OpenRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid descriptor-anchored frozen-root target {path}")]
    InvalidTarget { path: PathBuf },
    #[error("failed to open frozen-root target {path}: {source}")]
    OpenTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("frozen-root target {path} returned invalid descriptor {descriptor}")]
    InvalidDescriptor { path: PathBuf, descriptor: i64 },
    #[error("failed to inspect frozen-root target {path}: {source}")]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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

    let destination = info.target_path.strip_prefix("/")?.with_added_extension("zst");
    let target = Path::new("/").join(&destination);
    let newpath = info.path.with_added_extension("zst");
    let (accessed, modified) = info.file_times()?;
    let times = Some(GeneratedTimes { accessed, modified });

    let artifact = if info.is_symlink() {
        if info.inventory_contains_symlink_target(&target)? {
            None
        } else {
            let compressed_target = Path::new(info.symlink_target()?).with_added_extension("zst");
            let compressed_target = compressed_target
                .to_str()
                .ok_or_else(|| std::io::Error::other("compressed man symlink target is not UTF-8"))?
                .to_owned();
            Some(GeneratedArtifact::symlink(destination, compressed_target, times, false))
        }
    } else if info.is_file() {
        if info.inventory_contains_regular_target(&target)? {
            None
        } else {
            Some(GeneratedArtifact::regular(
                destination,
                compress_zstd(info)?,
                0o644,
                times,
                false,
            ))
        }
    } else {
        return Ok(Decision::NextHandler.into());
    };

    Ok(Response {
        decision: Decision::ReplaceFile { newpath },
        publications: artifact.into_iter().collect(),
    })
}

fn compress_zstd(info: &PathInfo) -> Result<Vec<u8>, BoxError> {
    let limit = usize::try_from(info.regular_file_byte_limit()?)
        .map_err(|_| std::io::Error::other("regular file byte limit does not fit in memory"))?;
    let mut input = info.open_verified()?;
    let mut output = BoundedBytes::new(limit);
    zstd::stream::copy_encode(&mut input, &mut output, 16)?;
    input.finish()?;
    Ok(output.into_inner())
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let total =
            self.bytes.len().checked_add(buffer.len()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "compressed output length overflow")
            })?;
        if total > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed output exceeds the collected regular-file limit",
            ));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|source| std::io::Error::other(format!("failed to reserve compressed output: {source}")))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(super) use self::execution::checked_output;

#[cfg(test)]
mod tests;
