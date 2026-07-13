// SPDX-FileCopyrightText: 2025 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

// build.rs
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::Path,
};

use chrono::{DateTime, Utc};

#[path = "src/semantic_fingerprint.rs"]
mod semantic_fingerprint;

#[path = "src/native_build_context.rs"]
mod native_build_context;

#[path = "src/tool_identity.rs"]
mod tool_identity;

/// Returns value of given environment variable or error if missing.
///
/// This also outputs necessary ‘cargo:rerun-if-env-changed’ tag to make sure
/// build script is rerun if the environment variable changes.
fn env(key: &str) -> Result<OsString, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed={key}");
    std::env::var_os(key).ok_or_else(|| Box::from(format!("Missing `{key}` environmental variable")))
}

/// Calls program with given arguments and returns its standard output.  If
/// calling the program fails or it exits with non-zero exit status returns an
/// error.
fn command(prog: &str, args: &[&str], cwd: Option<std::path::PathBuf>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=PATH");
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    cmd.stderr(std::process::Stdio::inherit());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let out = cmd.output()?;
    if out.status.success() {
        let mut stdout = out.stdout;
        if let Some(b'\n') = stdout.last() {
            stdout.pop();
            if let Some(b'\r') = stdout.last() {
                stdout.pop();
            }
        }
        Ok(stdout)
    } else if let Some(code) = out.status.code() {
        Err(Box::from(format!("{prog}: terminated with {code}")))
    } else {
        Err(Box::from(format!("{prog}: killed by signal")))
    }
}

fn trim_trailing_newline(mut value: Vec<u8>) -> Vec<u8> {
    if value.last() == Some(&b'\n') {
        value.pop();
        if value.last() == Some(&b'\r') {
            value.pop();
        }
    }
    value
}

fn watch_executable(identity: &tool_identity::ExecutableIdentity) -> Result<(), Box<dyn std::error::Error>> {
    let path = identity.resolved_path();
    let path = path.to_str().ok_or_else(|| {
        Box::<dyn std::error::Error>::from(format!(
            "selected executable path is not UTF-8 and cannot be watched by Cargo: {}",
            path.display()
        ))
    })?;
    if path.contains(['\r', '\n']) {
        return Err(Box::from(format!(
            "selected executable path cannot be represented by a Cargo directive: {path:?}"
        )));
    }
    println!("cargo:rerun-if-changed={path}");
    Ok(())
}

fn identify_executable<F>(
    program: &OsStr,
    probe: F,
) -> Result<Option<tool_identity::ExecutableIdentity>, Box<dyn std::error::Error>>
where
    F: FnOnce(&Path) -> Result<Option<Vec<u8>>, std::io::Error>,
{
    println!("cargo:rerun-if-env-changed=PATH");
    let search_path = std::env::var_os("PATH");
    let working_directory = std::env::current_dir()?;
    let identity = tool_identity::identify(program, search_path.as_deref(), &working_directory, probe)?;
    if let Some(identity) = &identity {
        watch_executable(identity)?;
    }
    Ok(identity)
}

/// Return a content-strong identity of the compiler Cargo selected.
fn rustc_identity(rustc: &OsStr) -> Result<tool_identity::ExecutableIdentity, Box<dyn std::error::Error>> {
    identify_executable(rustc, |resolved| {
        let output = std::process::Command::new(resolved)
            .arg("-vV")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "{} -vV terminated with {}",
                resolved.display(),
                output.status
            )));
        }
        Ok(Some(trim_trailing_newline(output.stdout)))
    })?
    .ok_or_else(|| Box::from(format!("cannot resolve selected Rust compiler {rustc:?}")))
}

fn rustc_wrapper_identity(
    role: &str,
    selector: &OsStr,
) -> Result<tool_identity::ExecutableIdentity, Box<dyn std::error::Error>> {
    identify_executable(selector, |_| Ok(None))?
        .ok_or_else(|| Box::from(format!("cannot resolve selected {role} executable {selector:?}")))
}

/// Probe a native build tool selected by Cargo or one of its native
/// dependencies.  `NotFound` remains distinguishable only so the AWS-LC
/// `cmake3`-then-`cmake` discovery rule can be reproduced; once a command is
/// selected, [`native_build_context::collect`] rejects a missing identity.
fn native_tool_identity(
    role: &str,
    command: &[OsString],
) -> Result<Option<tool_identity::CommandIdentity>, std::io::Error> {
    let Some((program, arguments)) = command.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "native tool command is empty",
        ));
    };
    if program.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "native tool program is empty",
        ));
    }

    let primary = identify_executable(program, |resolved| {
        let output = std::process::Command::new(resolved)
            .args(arguments)
            .arg("--version")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "native tool {command:?} --version terminated with {}",
                output.status
            )));
        }

        let stdout = trim_trailing_newline(output.stdout);
        let stderr = trim_trailing_newline(output.stderr);
        let mut version = Vec::with_capacity(stdout.len() + stderr.len() + 16);
        for value in [&stdout, &stderr] {
            let length = u64::try_from(value.len()).expect("native tool version output fits in u64");
            version.extend_from_slice(&length.to_be_bytes());
            version.extend_from_slice(value);
        }
        Ok(Some(version))
    })
    .map_err(|source| std::io::Error::other(source.to_string()))?;
    let Some(primary) = primary else {
        return Ok(None);
    };

    let mut identity = tool_identity::CommandIdentity::new(primary);
    let custom_wrapper = std::env::var_os("CC_KNOWN_WRAPPER_CUSTOM");
    if let Some(delegated_program) = native_build_context::delegated_compiler(role, command, custom_wrapper.as_deref())
    {
        let delegated = identify_executable(delegated_program, |_| Ok(None))
            .map_err(|source| std::io::Error::other(source.to_string()))?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cannot resolve delegated native compiler {delegated_program:?}"),
                )
            })?;
        identity.push_delegated(delegated);
    }
    Ok(Some(identity))
}

/// Collect build context which can change compiled behavior without changing
/// repository contents.  Absolute compiler paths are deliberately replaced by
/// stable Rust and native tool identities, while effective flags, selectors,
/// and dependency build-script controls retain their semantic values.
fn semantic_build_context(
    top_level: &Path,
) -> Result<Vec<semantic_fingerprint::ExplicitInput>, Box<dyn std::error::Error>> {
    const OPTIONAL_KEYS: &[&str] = &[
        "HOST",
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTC_BOOTSTRAP",
    ];

    let mut inputs = BTreeMap::<String, Vec<u8>>::new();
    let workspace = top_level.as_os_str().as_bytes();
    let rustc = env("RUSTC")?;
    let rustc_identity = rustc_identity(&rustc)?;
    inputs.insert(
        "toolchain.rustc.selector".to_owned(),
        tool_identity::normalize_workspace(rustc.as_os_str().as_bytes(), workspace),
    );
    inputs.insert("toolchain.rustc.identity".to_owned(), rustc_identity.encode(top_level));

    for (key, role, input_role) in [
        ("RUSTC_WRAPPER", "RUSTC_WRAPPER", "rustc-wrapper"),
        (
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "rustc-workspace-wrapper",
        ),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        let Some(selector) = std::env::var_os(key).filter(|value| !value.is_empty()) else {
            continue;
        };
        let identity = rustc_wrapper_identity(role, &selector)?;
        inputs.insert(
            format!("toolchain.{input_role}.selector"),
            tool_identity::normalize_workspace(selector.as_os_str().as_bytes(), workspace),
        );
        inputs.insert(format!("toolchain.{input_role}.identity"), identity.encode(top_level));
    }

    for key in OPTIONAL_KEYS {
        println!("cargo:rerun-if-env-changed={key}");
        if let Some(value) = std::env::var_os(key) {
            inputs.insert(format!("env.{key}"), value.into_vec());
        }
    }

    // Cargo's target cfgs, enabled package features, and profile overrides are
    // open-ended name sets.  Cargo already reruns build scripts when its own
    // feature/cfg calculation changes; the directives also cover explicit
    // environment overrides for variables present in this invocation.
    for (key, value) in std::env::vars_os() {
        let Some(key) = key.to_str() else {
            continue;
        };
        if key.starts_with("CARGO_CFG_") || key.starts_with("CARGO_FEATURE_") || key.starts_with("CARGO_PROFILE_") {
            println!("cargo:rerun-if-env-changed={key}");
            inputs.insert(format!("env.{key}"), value.into_vec());
        }
    }

    println!("cargo:rerun-if-env-changed=PATH");
    let native = native_build_context::collect(std::env::vars_os(), top_level, native_tool_identity)?;
    for key in native.watched_environment() {
        println!("cargo:rerun-if-env-changed={key}");
    }
    inputs.extend(native.inputs());

    Ok(inputs.into_iter().collect())
}

fn get_semantic_fingerprint(top_level: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let fingerprint = semantic_fingerprint::calculate(top_level, semantic_build_context(top_level)?)?;
    for path in fingerprint.watched_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=BUILDINFO_SEMANTIC_FINGERPRINT={}", fingerprint.value());
    Ok(())
}

/// Checks to see if we're building from a git source and if so attempts to gather information about the git status
fn get_git_info() -> Result<(), Box<dyn std::error::Error>> {
    // These are cfgs that can be set by this script. We need to declare them always to ensure that clippy is happy
    println!("cargo:rustc-check-cfg=cfg(BUILDINFO_IS_DIRTY)");
    println!("cargo:rustc-check-cfg=cfg(BUILDINFO_IS_GIT_BUILD)");

    let pkg_dir = std::path::PathBuf::from(env("CARGO_MANIFEST_DIR")?);
    let git_dir = command("git", &["rev-parse", "--git-dir"], Some(pkg_dir.clone()));
    let git_dir = match git_dir {
        Ok(git_dir) => {
            println!("cargo:rustc-cfg=BUILDINFO_IS_GIT_BUILD");

            std::path::PathBuf::from(OsString::from_vec(git_dir))
        }
        Err(msg) => {
            // We're not in a git repo, most likely we're building from a source archive
            println!("cargo:warning=unable to determine git version (not in git repository?)");
            println!("cargo:warning={msg}");

            // It's unlikely, but possible that someone could run git init. Might as well catch that.
            println!("cargo::rerun-if-changed={}/.git", pkg_dir.display());
            return Ok(());
        }
    };

    // Make Cargo rerun us if currently checked out commit or the state of the
    // working tree changes.  We try to accomplish that by looking at a few
    // crucial git state files.  This probably may result in some false
    // negatives but it’s best we’ve got.
    for subpath in ["HEAD", "logs/HEAD", "index"] {
        let path = git_dir.join(subpath).canonicalize()?;
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // Get the full git hash
    let args = &["rev-parse", "--output-object-format=sha1", "HEAD"];
    let out = command("git", args, None)?;
    match String::from_utf8_lossy(&out) {
        std::borrow::Cow::Borrowed(full_hash) => {
            println!("cargo:rustc-env=BUILDINFO_GIT_FULL_HASH={}", full_hash.trim());
        }
        std::borrow::Cow::Owned(full_hash) => return Err(Box::from(format!("git: Invalid output: {full_hash}"))),
    }

    // Get the short git hash
    let args = &["rev-parse", "--output-object-format=sha1", "--short", "HEAD"];
    let out = command("git", args, None)?;
    match String::from_utf8_lossy(&out) {
        std::borrow::Cow::Borrowed(short_hash) => {
            println!("cargo:rustc-env=BUILDINFO_GIT_SHORT_HASH={}", short_hash.trim());
        }
        std::borrow::Cow::Owned(short_hash) => return Err(Box::from(format!("git: Invalid output: {short_hash}"))),
    }

    // Get whether this is built from a dirty tree
    let args = &["status", "--porcelain"];
    let out = command("git", args, None)?;
    match String::from_utf8_lossy(&out) {
        std::borrow::Cow::Borrowed(output) => match output.trim().len() {
            0 => {}
            _ => println!("cargo:rustc-cfg=BUILDINFO_IS_DIRTY"),
        },
        std::borrow::Cow::Owned(output) => return Err(Box::from(format!("git: Invalid output: {output}"))),
    }

    // Get the commit summary
    let args = &["show", "--format=\"%s\"", "-s"];
    let out = command("git", args, None)?;
    match String::from_utf8_lossy(&out) {
        std::borrow::Cow::Borrowed(summary) => {
            println!("cargo:rustc-env=BUILDINFO_GIT_SUMMARY={}", summary.trim());
        }
        std::borrow::Cow::Owned(summary) => return Err(Box::from(format!("git: Invalid output: {summary}"))),
    }

    Ok(())
}

fn get_build_time() -> Result<(), Box<dyn std::error::Error>> {
    // Propagate SOURCE_DATE_EPOCH if set
    if let Ok(epoch_env) = env("SOURCE_DATE_EPOCH")
        && let Ok(seconds) = epoch_env.to_string_lossy().parse::<i64>()
        && let Some(time) = DateTime::from_timestamp(seconds, 0)
    {
        println!("cargo:rustc-env=BUILDINFO_BUILD_TIME={}", time.timestamp());
        return Ok(());
    }

    println!("cargo:rustc-env=BUILDINFO_BUILD_TIME={}", Utc::now().timestamp());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let package_dir = std::path::PathBuf::from(env("CARGO_MANIFEST_DIR")?);
    let top_level = package_dir.join("../..").canonicalize()?;

    get_semantic_fingerprint(&top_level)?;

    let version = env("CARGO_PKG_VERSION")?;
    println!("cargo:rustc-env=BUILDINFO_VERSION={}", version.to_string_lossy());

    get_build_time()?;

    get_git_info()?;

    Ok(())
}
